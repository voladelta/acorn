//! Web
//! ===
//! The tools that read the web: web_search and web_fetch (TOOLS), through
//! DeepSeek's Search and Fetch APIs, on the same key as its model. Busy
//! (408, 429, 5xx) is tried again after a wait; any other error answers at
//! once. A stop ends the wait and the request. A plugin's http (`luau`) goes
//! through the same `request`; its errors never name the url, which may hold
//! a key.

use crate::ai::agent::HTTP;
use crate::ai::{Out, nap};
use crate::clip;
use crate::config;
use reqwest::Response;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;
use tokio_util::sync::CancellationToken;

pub const SEARCH: &str = "https://api.deepseek.com/v1/tools/search";
pub const FETCH: &str = "https://api.deepseek.com/v1/tools/fetch";

/// Tries of a busy request, the first wait, doubled after each.
const TRIES: u32 = 4;
const WAIT: Duration = Duration::from_secs(2);
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// The most bytes of a response read into memory (`save` has no such limit).
const TEXT_BYTES: usize = 50 << 20;

/// The next part file's number.
static PARTS: AtomicU64 = AtomicU64::new(1);

pub async fn run(name: &str, input: &Value, cancel: CancellationToken) -> Out {
  let str = |k: &str| input[k].as_str().unwrap_or_default().trim().to_string();
  match name {
    "web_search" => {
      let query = str("query");
      if query.is_empty() {
        return Out::fail("Give a query.");
      }
      let mut body = json!({"query": query, "count": input["count"].as_u64().unwrap_or(10).clamp(1, 50)});
      let question: String = str("question").chars().take(128).collect();
      if !question.is_empty() {
        body["ori_question"] = json!(question);
      }
      match post(SEARCH, &body, &cancel).await {
        Ok(got) => Out::ok(results(&got)),
        Err(why) => Out::fail(why),
      }
    }
    "web_fetch" => {
      let url = str("url");
      if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Out::fail("Give an http or https url.");
      }
      match post(FETCH, &json!({"url": url}), &cancel).await {
        Ok(got) => {
          let title = got["title"].as_str().unwrap_or_default();
          let page = format!(
            "# {}\n{url}\n\n{}",
            if title.is_empty() { "(no title)" } else { title },
            got["content"].as_str().unwrap_or_default()
          );
          Out::ok(clip::page(&page, input["at"].as_u64().unwrap_or(0) as usize))
        }
        Err(why) => Out::fail(why),
      }
    }
    _ => Out::fail(format!("No tool {name}.")),
  }
}

/// A search's results as text, one block each.
pub fn results(got: &Value) -> String {
  let all = got["results"].as_array().cloned().unwrap_or_default();
  if all.is_empty() {
    return "No results.".into();
  }
  let str = |r: &Value, k: &str| r[k].as_str().unwrap_or_default().trim().to_string();
  all
    .iter()
    .enumerate()
    .map(|(i, r)| {
      let date = str(r, "date");
      let date = date.get(..10).unwrap_or(&date);
      let site = [str(r, "site_name"), date.to_string()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
      let site = if site.is_empty() {
        String::new()
      } else {
        format!(" ({site})")
      };
      let text = Some(str(r, "summary"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| str(r, "snippet"));
      format!("{}. {}{site}\n{}\n{text}", i + 1, str(r, "title"), str(r, "url"))
    })
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// A request to one of the APIs; its JSON, or why it failed.
async fn post(url: &str, body: &Value, cancel: &CancellationToken) -> Result<Value, String> {
  let key = &config::get().key;
  if key.is_empty() {
    return Err(format!("no API key: set {}", config::KEY_VAR));
  }
  let req = Req {
    method: "POST".into(),
    url: url.into(),
    query: Vec::new(),
    headers: vec![
      ("authorization".into(), format!("Bearer {key}")),
      ("content-type".into(), "application/json".into()),
    ],
    body: Some(body.to_string().into_bytes()),
    timeout: TIMEOUT,
  };
  let bytes = read(request(&req, cancel).await?, cancel).await?;
  serde_json::from_slice(&bytes).map_err(|err| err.to_string())
}

/// A request to send.
pub struct Req {
  pub method: String,
  pub url: String,
  pub query: Vec<(String, String)>,
  pub headers: Vec<(String, String)>,
  pub body: Option<Vec<u8>>,
  pub timeout: Duration,
}

/// A request sent, tried again while busy: its response, or why it failed
/// (any status but a 2xx is a failure).
pub async fn request(req: &Req, cancel: &CancellationToken) -> Result<Response, String> {
  let method = reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|_| format!("no method {}", req.method))?;
  let mut wait = WAIT;
  for n in 1..=TRIES {
    let mut sent = HTTP
      .request(method.clone(), &req.url)
      .query(&req.query)
      .timeout(req.timeout);
    for (k, v) in &req.headers {
      sent = sent.header(k, v);
    }
    if let Some(body) = &req.body {
      sent = sent.body(body.clone());
    }
    let res = tokio::select! {
        res = sent.send() => res,
        _ = cancel.cancelled() => return Err("stopped".into()),
    };
    let why = match res {
      Ok(res) if res.status().is_success() => return Ok(res),
      Ok(res) => {
        let code = res.status();
        let why = format!("{code}: {}", reason(&res.text().await.unwrap_or_default()));
        if !(code.as_u16() == 408 || code.as_u16() == 429 || code.is_server_error()) {
          return Err(why);
        }
        why
      }
      Err(err) if err.is_timeout() || err.is_connect() => err.without_url().to_string(),
      Err(err) => return Err(err.without_url().to_string()),
    };
    if n == TRIES {
      return Err(why);
    }
    nap(wait, cancel).await;
    if cancel.is_cancelled() {
      return Err("stopped".into());
    }
    wait *= 2;
  }
  unreachable!()
}

/// What an API's error body says: its message, or else its start.
fn reason(body: &str) -> String {
  let got: Value = serde_json::from_str(body).unwrap_or_default();
  let said = [&got["error"]["message"], &got["error"], &got["message"]]
    .iter()
    .find_map(|v| v.as_str())
    .map(str::to_string);
  said.unwrap_or_else(|| match clip::head(body.trim(), 200) {
    "" => "failed".into(),
    start => start.to_string(),
  })
}

/// A response's next piece, unless stopped.
async fn piece(res: &mut Response, cancel: &CancellationToken) -> Result<Option<Vec<u8>>, String> {
  tokio::select! {
      got = res.chunk() => got
        .map(|c| c.map(|c| c.to_vec()))
        .map_err(|err| err.without_url().to_string()),
      _ = cancel.cancelled() => Err("stopped".into()),
  }
}

/// A response's body, up to TEXT_BYTES.
pub async fn read(mut res: Response, cancel: &CancellationToken) -> Result<Vec<u8>, String> {
  let mut all = Vec::new();
  while let Some(got) = piece(&mut res, cancel).await? {
    all.extend_from_slice(&got);
    if all.len() > TEXT_BYTES {
      return Err(format!("response over {} MB: save it to a file", TEXT_BYTES >> 20));
    }
  }
  Ok(all)
}

/// A part file, gone with this unless it was linked into place.
struct Part<'a>(&'a Path);

impl Drop for Part<'_> {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(self.0);
  }
}

/// A response's body written to a new file `to`, in place only when whole;
/// its bytes. A file already there is left, and is a failure.
pub async fn save(mut res: Response, to: &Path, cancel: &CancellationToken) -> Result<u64, String> {
  let failed = |err: std::io::Error| format!("save {}: {err}", to.display());
  if let Some(dir) = to.parent() {
    std::fs::create_dir_all(dir).map_err(failed)?;
  }
  let name = to.file_name().unwrap_or_default().to_string_lossy();
  let part = to.with_file_name(format!(
    ".{name}.{}.{}.part",
    std::process::id(),
    PARTS.fetch_add(1, Ordering::Relaxed)
  ));
  let _gone = Part(&part);
  let mut file = tokio::fs::File::create(&part).await.map_err(failed)?;
  let mut bytes = 0;
  while let Some(got) = piece(&mut res, cancel).await? {
    file.write_all(&got).await.map_err(failed)?;
    bytes += got.len() as u64;
  }
  file.flush().await.map_err(failed)?;
  drop(file);
  std::fs::hard_link(&part, to).map_err(failed)?;
  Ok(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn results_read_as_numbered_blocks() {
    let got = json!({"results": [
        {"title": "V4", "url": "https://a.com/v4", "snippet": "short", "summary": "long", "site_name": "A", "date": "2026-06-01T00:00:00+00:00"},
        {"title": "B", "url": "https://b.com", "snippet": "only a snippet", "summary": "", "site_name": null, "date": null},
    ]});
    assert_eq!(
      results(&got),
      "1. V4 (A, 2026-06-01)\nhttps://a.com/v4\nlong\n\n2. B\nhttps://b.com\nonly a snippet"
    );
    assert_eq!(results(&json!({"results": []})), "No results.");
  }

  /// A server answering each connection with the next of `replies` (status,
  /// body); its address. Each request it saw comes back on the channel.
  async fn server(replies: Vec<(u16, &'static str)>) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    use tokio::io::AsyncReadExt as _;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
      for (code, body) in replies {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut got = vec![0; 8192];
        let n = sock.read(&mut got).await.unwrap();
        let _ = tx.send(String::from_utf8_lossy(&got[..n]).into_owned());
        let head = format!(
          "HTTP/1.1 {code} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
          body.len()
        );
        sock.write_all(head.as_bytes()).await.unwrap();
      }
    });
    (at, rx)
  }

  fn get(url: &str) -> Req {
    Req {
      method: "GET".into(),
      url: url.into(),
      query: Vec::new(),
      headers: Vec::new(),
      body: None,
      timeout: TIMEOUT,
    }
  }

  #[tokio::test]
  async fn a_busy_request_is_tried_again_and_a_refused_one_is_not() {
    let (at, mut seen) = server(vec![(503, "busy"), (200, "done")]).await;
    let cancel = CancellationToken::new();
    let res = request(&get(&at), &cancel).await.unwrap();
    assert_eq!(read(res, &cancel).await.unwrap(), b"done");
    assert!(seen.recv().await.is_some() && seen.recv().await.is_some());
    let (at, _seen) = server(vec![(401, r#"{"error":"bad key"}"#), (200, "never")]).await;
    assert_eq!(
      request(&get(&at), &cancel).await.unwrap_err(),
      "401 Unauthorized: bad key"
    );
  }

  #[tokio::test]
  async fn a_failure_never_names_the_url() {
    let why = request(&get("notaurl://x?key=SECRET"), &CancellationToken::new()).await;
    assert!(!why.unwrap_err().contains("SECRET"));
  }

  #[tokio::test]
  async fn a_stop_ends_the_wait_between_tries() {
    let (at, _seen) = server(vec![(503, "busy")]).await;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(200)).await;
      stop.cancel();
    });
    assert_eq!(request(&get(&at), &cancel).await.unwrap_err(), "stopped");
  }

  #[tokio::test]
  async fn a_save_makes_a_new_file_and_leaves_no_part() {
    let (at, _seen) = server(vec![(200, "whole"), (200, "again")]).await;
    let dir = tempfile::tempdir().unwrap();
    let to = dir.path().join("new/out.bin");
    let cancel = CancellationToken::new();
    let res = request(&get(&at), &cancel).await.unwrap();
    assert_eq!(save(res, &to, &cancel).await.unwrap(), 5);
    assert_eq!(std::fs::read(&to).unwrap(), b"whole");
    let res = request(&get(&at), &cancel).await.unwrap();
    assert!(save(res, &to, &cancel).await.is_err(), "a file there stays");
    assert_eq!(std::fs::read(&to).unwrap(), b"whole");
    assert_eq!(std::fs::read_dir(to.parent().unwrap()).unwrap().count(), 1);
  }

  /// Searches and fetches for real; run with `cargo test -- --ignored`
  /// and DEEPSEEK_API_KEY.
  #[tokio::test]
  #[ignore]
  async fn search_and_fetch_read_the_web() {
    let found = run(
      "web_search",
      &json!({"query": "rust-lang site:github.com", "count": 3}),
      CancellationToken::new(),
    )
    .await;
    assert!(
      !found.fail && found.text.starts_with("1. ") && found.text.contains("github.com"),
      "{}",
      found.text
    );
    let page = run(
      "web_fetch",
      &json!({"url": "https://example.com"}),
      CancellationToken::new(),
    )
    .await;
    assert!(!page.fail && page.text.contains("Example Domain"), "{}", page.text);
    let bad = run(
      "web_fetch",
      &json!({"url": "ftp://example.com"}),
      CancellationToken::new(),
    )
    .await;
    assert!(bad.fail);
  }
}
