//! The Bot API
//! ===========
//! Telegram's HTTP API, as a bot calls it: a method and its JSON, the
//! result or why not. Busy (429, which says how long to wait, 5xx, or no
//! connection) is tried again; a conflict (another poller, or a webhook
//! set) and a refusal are not. The token is in every url, so no error
//! names one.

use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Tries of a busy call, the first wait, doubled after each, the most.
const TRIES: u32 = 5;
const WAIT: Duration = Duration::from_secs(2);
const MOST_WAIT: Duration = Duration::from_secs(300);
/// What a call waits for beyond the long poll it asks for.
const SLACK: Duration = Duration::from_secs(20);
/// The biggest file a bot may download, and send (a photo: PHOTO_BYTES).
pub const FILE_BYTES: u64 = 20 << 20;
pub const SEND_BYTES: u64 = 50 << 20;
pub const PHOTO_BYTES: u64 = 10 << 20;
/// What an upload waits for.
const UPLOAD: Duration = Duration::from_secs(300);

#[derive(Debug, PartialEq)]
pub enum Fail {
  /// 409: another poller of this bot, or a webhook set.
  Conflict(String),
  /// Refused as asked: a bad token, a chat gone, a text that won't parse.
  Refused(String),
  /// Still busy, or no answer.
  Down(String),
  Stopped,
}

impl std::fmt::Display for Fail {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    match self {
      Fail::Conflict(why) | Fail::Refused(why) | Fail::Down(why) => f.write_str(why),
      Fail::Stopped => f.write_str("stopped"),
    }
  }
}

pub struct Bot {
  base: String,
  files: String,
  client: reqwest::Client,
}

impl Bot {
  pub fn new(token: &str) -> Bot {
    Bot::at(
      &format!("https://api.telegram.org/bot{token}"),
      &format!("https://api.telegram.org/file/bot{token}"),
    )
  }

  /// A bot at other urls (a test's server).
  pub fn at(base: &str, files: &str) -> Bot {
    Bot {
      base: base.into(),
      files: files.into(),
      client: reqwest::Client::builder()
        .user_agent("Acorn/0.1")
        .build()
        .expect("an HTTP client"),
    }
  }

  /// A method called: its result. A long poll (`timeout` in the body) is
  /// waited for that long and more.
  pub async fn call(&self, method: &str, body: &Value, cancel: &CancellationToken) -> Result<Value, Fail> {
    let poll = Duration::from_secs(body["timeout"].as_u64().unwrap_or(0));
    self
      .ask(method, poll + SLACK, cancel, || {
        self.client.post(format!("{}/{method}", self.base)).json(body)
      })
      .await
  }

  /// A method called with a file: `fields` (a JSON object) as form
  /// fields, the file's bytes as `field`, named `name`.
  pub async fn upload(
    &self,
    method: &str,
    fields: &Value,
    field: &str,
    name: &str,
    bytes: &[u8],
    cancel: &CancellationToken,
  ) -> Result<Value, Fail> {
    let form = || {
      let mut form = reqwest::multipart::Form::new();
      for (k, v) in fields.as_object().into_iter().flatten() {
        let v = v.as_str().map_or_else(|| v.to_string(), str::to_string);
        form = form.text(k.clone(), v);
      }
      let part = reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(name.to_string());
      form.part(field.to_string(), part)
    };
    self
      .ask(method, UPLOAD, cancel, || {
        self.client.post(format!("{}/{method}", self.base)).multipart(form())
      })
      .await
  }

  /// A request, sent anew while busy (`TRIES`): its result.
  async fn ask(
    &self,
    method: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    request: impl Fn() -> reqwest::RequestBuilder,
  ) -> Result<Value, Fail> {
    let mut wait = WAIT;
    for n in 1..=TRIES {
      let sent = request().timeout(timeout).send();
      let res = tokio::select! {
          res = sent => res,
          _ = cancel.cancelled() => return Err(Fail::Stopped),
      };
      let why = match res {
        Ok(res) => {
          let code = res.status().as_u16();
          let got: Value = res.json().await.unwrap_or_default();
          if got["ok"].as_bool() == Some(true) {
            return Ok(got["result"].clone());
          }
          let said = got["description"].as_str().unwrap_or("failed").to_string();
          let why = format!("{method}: {code}: {said}");
          match code {
            409 => return Err(Fail::Conflict(why)),
            429 => {
              if let Some(after) = got["parameters"]["retry_after"].as_u64() {
                wait = Duration::from_secs(after).min(MOST_WAIT);
              }
              why
            }
            500.. => why,
            _ => return Err(Fail::Refused(why)),
          }
        }
        Err(err) => format!("{method}: {}", err.without_url()),
      };
      if n == TRIES {
        return Err(Fail::Down(why));
      }
      tokio::select! {
          _ = tokio::time::sleep(wait) => {}
          _ = cancel.cancelled() => return Err(Fail::Stopped),
      }
      wait = (wait * 2).min(MOST_WAIT);
    }
    unreachable!()
  }

  /// A file of a message's, by its id: its bytes, if no bigger than a bot
  /// may take.
  pub async fn download(&self, file: &str, cancel: &CancellationToken) -> Result<Vec<u8>, Fail> {
    let got = self.call("getFile", &json!({"file_id": file}), cancel).await?;
    if got["file_size"].as_u64().is_some_and(|s| s > FILE_BYTES) {
      return Err(Fail::Refused("over 20 MB, the most a bot may download".into()));
    }
    let path = got["file_path"]
      .as_str()
      .ok_or_else(|| Fail::Refused("no file path".into()))?;
    let res = self
      .client
      .get(format!("{}/{path}", self.files))
      .timeout(Duration::from_secs(120))
      .send()
      .await
      .and_then(|r| r.error_for_status())
      .map_err(|e| Fail::Down(format!("download: {}", e.without_url())))?;
    let bytes = res
      .bytes()
      .await
      .map_err(|e| Fail::Down(format!("download: {}", e.without_url())))?;
    Ok(bytes.to_vec())
  }
}
