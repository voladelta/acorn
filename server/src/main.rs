//! acorn-server
//! ============
//! Acorn's engines on this machine, its window in a browser. It opens
//! every chat (`hub`), keeps what they say as numbered events, and serves
//! the web window (web/dist: `cd web && bun run build`) and its API:
//!
//! - `GET /api/hello`: the assistants, and the last event's number;
//! - `GET /api/events?after=N`: the events after N, waiting up to 25 s
//!   for one (`lost`: some were dropped, too old);
//! - `POST /api/cmd?chat=ID`: a `Command` (JSON) to a chat, once it is open
//!   (404: no such chat);
//! - `GET /api/image?chat=ID&name=F`: an image or other file of a chat's
//!   (a video, a sound, a document), in ranges if asked.
//!
//! The agent runs commands on this machine, so every request needs the
//! server's token: opening the link it prints sets it as a cookie. The
//! token is kept in $ACORN_DIR/server-token, unless `ACORN_TOKEN` is set.
//! It listens on 127.0.0.1:8484 (`--addr` or $ACORN_ADDR to change), or on a
//! Unix socket (`--unix PATH`),
//! which `acorn-host` uses: a path only its sandbox and the host can reach,
//! where a port could be taken by anyone's agent while this one restarts.

use acorn_core::chat::hub;
use acorn_core::wire::{Command, Event};
use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Query, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tower::ServiceExt as _;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate as _};
use tower_http::services::{ServeDir, ServeFile};

/// Events kept for polls that come late: at most so many, and bytes.
const KEEP: usize = 20_000;
const KEEP_BYTES: usize = 256 << 20;
/// A poll's events of more bytes than this are joined off the async workers.
const BIG_BODY: usize = 1 << 20;
/// The most bytes of a command: a message with its files (50 MB each at
/// most, in base64).
const CMD_BYTES: usize = 160 << 20;
/// How long a poll waits for an event.
const WAIT: Duration = Duration::from_secs(25);
const COOKIE: &str = "acorn_token";

/// The events said, numbered from 1: `[chat, event]` each, as JSON.
#[derive(Default)]
struct Log {
  /// The number of the first kept.
  first: u64,
  events: VecDeque<Arc<str>>,
  bytes: usize,
}

impl Log {
  fn last(&self) -> u64 {
    self.first + self.events.len() as u64 - 1
  }

  fn push(&mut self, json: String) -> u64 {
    self.push_capped(json, KEEP, KEEP_BYTES)
  }

  fn push_capped(&mut self, json: String, max_events: usize, max_bytes: usize) -> u64 {
    self.bytes += json.len();
    self.events.push_back(json.into());
    while self.events.len() > max_events || self.bytes > max_bytes {
      let Some(old) = self.events.pop_front() else { break };
      self.bytes -= old.len();
      self.first += 1;
    }
    self.last()
  }
}

/// The server: its events, told as they come; which run of it this is
/// (`epoch`: its numbers start anew each run, so a window polling with
/// another run's knows it missed all since); its token, and whether its
/// cookie is for HTTPS only (`--secure`).
#[derive(Clone)]
struct Server {
  epoch: u64,
  log: Arc<Mutex<Log>>,
  tick: watch::Receiver<u64>,
  said: Arc<watch::Sender<u64>>,
  token: Arc<String>,
  secure: bool,
}

impl Server {
  fn tell(&self, chat: &str, ev: &Event) {
    let Ok(json) = serde_json::to_string(&(chat, ev)) else {
      return;
    };
    let seq = self.log.lock().unwrap().push(json);
    self.said.send_replace(seq);
  }
}

fn main() {
  tokio::runtime::Runtime::new().expect("runtime").block_on(serve());
}

/// What the command line asks for, the environment where it is silent.
struct Args {
  addr: String,
  unix: Option<PathBuf>,
  web: PathBuf,
  secure: bool,
}

fn args() -> Args {
  let mut a = Args {
    addr: std::env::var("ACORN_ADDR").unwrap_or_else(|_| "127.0.0.1:8484".into()),
    unix: None,
    web: std::env::var("ACORN_WEB").map_or_else(|_| "web/dist".into(), PathBuf::from),
    secure: false,
  };
  let mut args = std::env::args().skip(1);
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--addr" => a.addr = args.next().expect("--addr HOST:PORT"),
      "--web" => a.web = args.next().expect("--web DIR").into(),
      "--unix" => a.unix = Some(args.next().expect("--unix PATH").into()),
      "--secure" => a.secure = true,
      _ => {
        eprintln!("usage: acorn-server [--addr HOST:PORT | --unix PATH] [--web DIR] [--secure]");
        std::process::exit(2);
      }
    }
  }
  a
}

async fn serve() {
  let Args {
    addr,
    unix,
    web,
    secure,
  } = args();
  if !web.join("index.html").exists() {
    eprintln!(
      "acorn-server: no web window in {} (cd web && bun install && bun run build)",
      web.display()
    );
    std::process::exit(1);
  }

  let (said, tick) = watch::channel(0);
  let server = Server {
    epoch: std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |d| d.as_nanos() as u64),
    log: Arc::new(Mutex::new(Log {
      first: 1,
      ..Log::default()
    })),
    tick,
    said: Arc::new(said),
    token: Arc::new(token()),
    secure,
  };

  acorn_telegram::add();
  let (tx, mut heard) = tokio::sync::mpsc::unbounded_channel();
  hub::listen(tx);
  let teller = server.clone();
  tokio::spawn(async move {
    while let Some((chat, ev)) = heard.recv().await {
      teller.tell(&chat, &ev);
    }
  });
  tokio::task::spawn_blocking(hub::open_all);

  let app = app(&server, &web);
  if let Some(path) = unix {
    // a socket left by a run that was killed
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)
      .unwrap_or_else(|err| panic!("acorn-server: can't listen on {}: {err}", path.display()));
    println!("Acorn: on {}", path.display());
    axum::serve(listener, app)
      .with_graceful_shutdown(stopped())
      .await
      .expect("acorn-server");
  } else {
    let listener = tokio::net::TcpListener::bind(&addr)
      .await
      .unwrap_or_else(|err| panic!("acorn-server: can't listen on {addr}: {err}"));
    let host = match addr.split_once(':') {
      Some(("0.0.0.0", port)) => format!("localhost:{port}"),
      _ => addr.clone(),
    };
    println!("Acorn: http://{host}/?token={}", server.token);
    axum::serve(listener, app)
      .with_graceful_shutdown(stopped())
      .await
      .expect("acorn-server");
  }
  acorn_core::machine::end();
}

/// The API and the web window, behind the token.
fn app(server: &Server, web: &Path) -> Router {
  Router::new()
    .route("/api/hello", get(hello))
    .route("/api/events", get(events))
    .route("/api/cmd", post(cmd).layer(DefaultBodyLimit::max(CMD_BYTES)))
    .route("/api/image", get(image))
    .fallback_service(ServeDir::new(web))
    .layer(
      CompressionLayer::new().compress_when(
        // a video or a sound is compressed already, and asked for in ranges
        DefaultPredicate::new()
          .and(NotForContentType::const_new("video/"))
          .and(NotForContentType::const_new("audio/")),
      ),
    )
    .layer(middleware::from_fn_with_state(server.clone(), guard))
    .with_state(server.clone())
}

/// Ends on Ctrl-C or a TERM. A service stops with a TERM: end as Ctrl-C
/// does, so `end` kills the agents' jobs (a browser ignoring TERM would
/// hold the stop up).
async fn stopped() {
  let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("a TERM handler");
  tokio::select! {
    _ = tokio::signal::ctrl_c() => {}
    _ = term.recv() => {}
  }
}

/// The server's token: `ACORN_TOKEN` wins; otherwise keep it in $ACORN_DIR.
fn token() -> String {
  if let Ok(t) = std::env::var(acorn_core::config::TOKEN_VAR)
    && t.trim().len() >= 32
  {
    return t.trim().to_string();
  }
  let file = acorn_core::config::get().dir.join("server-token");
  if let Ok(t) = std::fs::read_to_string(&file)
    && t.trim().len() >= 32
  {
    return t.trim().to_string();
  }
  let mut bytes = [0u8; 24];
  use std::io::Read;
  std::fs::File::open("/dev/urandom")
    .and_then(|mut f| f.read_exact(&mut bytes))
    .expect("random bytes");
  let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
  let _ = std::fs::create_dir_all(&acorn_core::config::get().dir);
  let _ = std::fs::OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .mode(0o600)
    .open(&file)
    .and_then(|mut f| {
      f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
      f.write_all(t.as_bytes())
    });
  t
}

/// The cookie a good token sets.
fn cookie(token: &str, secure: bool) -> String {
  let secure = if secure { "; Secure" } else { "" };
  format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000{secure}")
}

/// Every request shows the token: a link's `?token=` sets the cookie and
/// leads on without it; else the cookie must hold it, and only a read
/// may come from another origin's page.
async fn guard(State(s): State<Server>, req: Request, next: Next) -> Response {
  let query = req.uri().query().unwrap_or("");
  if let Some(t) = query.split('&').find_map(|kv| kv.strip_prefix("token=")) {
    if same(t, &s.token) {
      // behind HTTPS (`--secure`, or a proxy that says so) the cookie is
      // never sent in the clear
      let https = req
        .headers()
        .get("x-forwarded-proto")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"https"));
      let cookie = cookie(&s.token, s.secure || https);
      let path = format!("/{}", req.uri().path().trim_start_matches('/'));
      let mut res = Redirect::to(&path).into_response();
      res.headers_mut().insert(header::SET_COOKIE, cookie.parse().unwrap());
      return res;
    }
    return (StatusCode::UNAUTHORIZED, "Wrong token.").into_response();
  }
  let has = req
    .headers()
    .get_all(header::COOKIE)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(';'))
    .filter_map(|c| c.trim().strip_prefix(&format!("{COOKIE}=")))
    .any(|t| same(t, &s.token));
  if !has {
    return (StatusCode::UNAUTHORIZED, "Open the link acorn-server printed.").into_response();
  }
  // the cookie goes with a request from any port of this host, and from a
  // sibling subdomain: what changes anything comes from Acorn's own page
  let safe = matches!(*req.method(), Method::GET | Method::HEAD);
  let own = req
    .headers()
    .get("sec-fetch-site")
    .is_none_or(|v| v == "same-origin" || v == "none");
  if !safe && !own {
    return (StatusCode::FORBIDDEN, "Not from Acorn's own page.").into_response();
  }
  next.run(req).await
}

/// Equal, in time that doesn't tell where they differ.
fn same(a: &str, b: &str) -> bool {
  a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

async fn hello(State(s): State<Server>) -> Response {
  let seq = s.log.lock().unwrap().last();
  Json(serde_json::json!({ "assistants": acorn_core::assistant::all(), "epoch": s.epoch, "seq": seq })).into_response()
}

/// The events after `after`, once there are some (or WAIT passes); a
/// poll of another run's (its `epoch`) is told at once that it lost them
/// all, and polls on from now.
async fn events(State(s): State<Server>, Query(q): Query<HashMap<String, String>>) -> Response {
  let after: u64 = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
  let ours = q.get("epoch").and_then(|e| e.parse().ok()) == Some(s.epoch);
  if ours {
    let mut tick = s.tick.clone();
    let _ = tokio::time::timeout(WAIT, tick.wait_for(|&last| last > after)).await;
  }
  // the lock is held to share the events, not to copy them
  let (last, lost, events) = {
    let log = s.log.lock().unwrap();
    let last = log.last();
    let lost = !ours || after.saturating_add(1) < log.first;
    let from = if ours {
      after.saturating_add(1).max(log.first)
    } else {
      last.saturating_add(1)
    };
    let skip = from.saturating_sub(log.first) as usize;
    let events: Vec<Arc<str>> = log.events.iter().skip(skip).cloned().collect();
    (last, lost, events)
  };
  let (epoch, bytes) = (s.epoch, events.iter().map(|e| e.len()).sum::<usize>());
  let join = move || {
    format!(
      "{{\"epoch\":{epoch},\"seq\":{last},\"lost\":{lost},\"events\":[{}]}}",
      events.join(",")
    )
  };
  let body = if bytes > BIG_BODY {
    match tokio::task::spawn_blocking(join).await {
      Ok(body) => body,
      Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
  } else {
    join()
  };
  ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn cmd(Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
  let Some(chat) = q.get("chat").cloned() else {
    return (StatusCode::BAD_REQUEST, "no chat").into_response();
  };
  let cmd: Command = match serde_json::from_slice(&body) {
    Ok(cmd) => cmd,
    Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
  };
  // a chat still opening is waited for: its commands go once it is open
  let id = chat.clone();
  match tokio::task::spawn_blocking(move || hub::ready(&id))
    .await
    .ok()
    .flatten()
  {
    Some(engine) => {
      engine.command(cmd);
      StatusCode::NO_CONTENT.into_response()
    }
    None => (StatusCode::NOT_FOUND, format!("No chat {chat}.")).into_response(),
  }
}

async fn image(Query(q): Query<HashMap<String, String>>, req: Request) -> Response {
  let (Some(chat), Some(name)) = (q.get("chat"), q.get("name")) else {
    return StatusCode::BAD_REQUEST.into_response();
  };
  let plain = |s: &str| !s.is_empty() && !s.starts_with('.') && !s.contains(['\\', '\0']);
  if !plain(name) || name.contains('/') || !chat.split('/').all(plain) || chat.split('/').count() > 2 {
    return StatusCode::BAD_REQUEST.into_response();
  }
  let path = hub::dir(chat).join("docs/images").join(name);
  // by its kind, and in ranges, so a video plays from any point
  match ServeFile::new(&path).oneshot(req).await {
    Ok(res) => res.into_response(),
    Err(never) => match never {},
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::body::{Body, to_bytes};

  fn test_server(epoch: u64, secure: bool) -> Server {
    let (said, tick) = watch::channel(0);
    Server {
      epoch,
      log: Arc::new(Mutex::new(Log {
        first: 1,
        ..Log::default()
      })),
      tick,
      said: Arc::new(said),
      token: Arc::new("0123456789abcdef0123456789abcdef".into()),
      secure,
    }
  }

  fn test_app(server: Server) -> Router {
    Router::new()
      .route("/api/events", get(events))
      .route("/api/image", get(image))
      .route("/ok", get(|| async { "ok" }).post(|| async { "ok" }))
      .layer(middleware::from_fn_with_state(server.clone(), guard))
      .with_state(server)
  }

  async fn json_body(res: Response) -> serde_json::Value {
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
  }

  #[test]
  fn log_evicts_by_count_and_bytes() {
    let mut log = Log {
      first: 1,
      ..Log::default()
    };
    assert_eq!(log.last(), 0);
    assert_eq!(log.push_capped("aaaa".into(), 2, 100), 1);
    assert_eq!(log.push_capped("bbbb".into(), 2, 100), 2);
    assert_eq!(log.push_capped("cccc".into(), 2, 100), 3);
    assert_eq!((log.first, log.last(), log.events.len(), log.bytes), (2, 3, 2, 8));

    // byte cap evicts older entries until within budget
    assert_eq!(log.push_capped("dddddd".into(), 10, 8), 4);
    assert_eq!((log.first, log.last(), log.events.len(), log.bytes), (4, 4, 1, 6));
  }

  #[tokio::test]
  async fn guard_enforces_token_cookie_and_secure_flag() {
    let app = test_app(test_server(1, false));

    // no token or cookie
    let res = app
      .clone()
      .oneshot(Request::builder().uri("/ok").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // wrong query token
    let res = app
      .clone()
      .oneshot(Request::builder().uri("/ok?token=wrong").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // valid query token sets HttpOnly cookie and redirects without token
    let res = app
      .clone()
      .oneshot(
        Request::builder()
          .uri("/ok?token=0123456789abcdef0123456789abcdef")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers()[header::LOCATION], "/ok");
    let set_cookie = res.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
    assert!(!set_cookie.contains("; Secure"));

    // x-forwarded-proto: https adds Secure
    let res = app
      .clone()
      .oneshot(
        Request::builder()
          .uri("/ok?token=0123456789abcdef0123456789abcdef")
          .header("x-forwarded-proto", "HTTPS")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert!(res.headers()[header::SET_COOKIE].to_str().unwrap().contains("; Secure"));

    // --secure flag also adds Secure without proxy header
    let secure_app = test_app(test_server(1, true));
    let res = secure_app
      .oneshot(
        Request::builder()
          .uri("/ok?token=0123456789abcdef0123456789abcdef")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert!(res.headers()[header::SET_COOKIE].to_str().unwrap().contains("; Secure"));

    // valid cookie passes (even alongside other cookies), wrong cookie rejected
    let res = app
      .clone()
      .oneshot(
        Request::builder()
          .uri("/ok")
          .header(header::COOKIE, "other=1; acorn_token=0123456789abcdef0123456789abcdef")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
      .oneshot(
        Request::builder()
          .uri("/ok")
          .header(header::COOKIE, "acorn_token=bad")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
  }

  /// The cookie alone sends no command from another origin's page: a
  /// browser sends it from any port of the host.
  #[tokio::test]
  async fn guard_takes_changes_from_its_own_page_only() {
    let app = test_app(test_server(1, false));
    let ask = |method: Method, site: Option<&'static str>| {
      let mut req = Request::builder()
        .method(method)
        .uri("/ok")
        .header(header::COOKIE, "acorn_token=0123456789abcdef0123456789abcdef");
      if let Some(site) = site {
        req = req.header("sec-fetch-site", site);
      }
      app.clone().oneshot(req.body(Body::empty()).unwrap())
    };
    for site in ["same-site", "cross-site"] {
      let res = ask(Method::POST, Some(site)).await.unwrap();
      assert_eq!(res.status(), StatusCode::FORBIDDEN, "{site}");
      // a link from another page still opens it
      let res = ask(Method::GET, Some(site)).await.unwrap();
      assert_eq!(res.status(), StatusCode::OK, "{site}");
    }
    // its own page, and a client that is no browser (the app, the host's tests)
    for site in [Some("same-origin"), Some("none"), None] {
      let res = ask(Method::POST, site).await.unwrap();
      assert_eq!(res.status(), StatusCode::OK, "{site:?}");
    }
  }

  #[tokio::test]
  async fn events_reports_epoch_mismatch_and_eviction_loss() {
    let server = test_server(42, false);
    {
      let mut log = server.log.lock().unwrap();
      let seq = log.push_capped(r#"["main",1]"#.into(), 2, 1024);
      server.said.send_replace(seq);
      let seq = log.push_capped(r#"["main",2]"#.into(), 2, 1024);
      server.said.send_replace(seq);
    }
    let app = test_app(server.clone());
    let cookie_hdr = "acorn_token=0123456789abcdef0123456789abcdef";

    // matching epoch returns buffered events with lost=false
    let res = app
      .clone()
      .oneshot(
        Request::builder()
          .uri("/api/events?epoch=42&after=0")
          .header(header::COOKIE, cookie_hdr)
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["epoch"], 42);
    assert_eq!(body["seq"], 2);
    assert_eq!(body["lost"], false);
    assert_eq!(body["events"].as_array().unwrap().len(), 2);

    // wrong epoch returns immediately with lost=true and empty events
    let res = app
      .clone()
      .oneshot(
        Request::builder()
          .uri("/api/events?epoch=99&after=2")
          .header(header::COOKIE, cookie_hdr)
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    let body = json_body(res).await;
    assert_eq!(body["epoch"], 42);
    assert_eq!(body["seq"], 2);
    assert_eq!(body["lost"], true);
    assert_eq!(body["events"].as_array().unwrap().len(), 0);

    // evicting past `after` signals lost=true and returns only kept events
    {
      let mut log = server.log.lock().unwrap();
      let seq = log.push_capped(r#"["main",3]"#.into(), 2, 1024);
      server.said.send_replace(seq);
    }
    let res = app
      .oneshot(
        Request::builder()
          .uri("/api/events?epoch=42&after=0")
          .header(header::COOKIE, cookie_hdr)
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    let body = json_body(res).await;
    assert_eq!(body["seq"], 3);
    assert_eq!(body["lost"], true);
    assert_eq!(body["events"], serde_json::json!([["main", 2], ["main", 3]]));
  }

  #[tokio::test]
  async fn image_rejects_traversal_and_malformed_params() {
    let app = test_app(test_server(1, false));
    let cookie_hdr = "acorn_token=0123456789abcdef0123456789abcdef";

    for uri in [
      "/api/image",
      "/api/image?chat=main",
      "/api/image?name=a.png",
      "/api/image?chat=main&name=../secret.png",
      "/api/image?chat=main&name=.hidden",
      "/api/image?chat=main&name=sub/a.png",
      "/api/image?chat=main&name=a%5Cb.png",
      "/api/image?chat=..&name=a.png",
      "/api/image?chat=.hidden&name=a.png",
      "/api/image?chat=a/b/c&name=a.png",
      "/api/image?chat=&name=a.png",
    ] {
      let res = app
        .clone()
        .oneshot(
          Request::builder()
            .uri(uri)
            .header(header::COOKIE, cookie_hdr)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
  }
}
