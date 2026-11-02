//! acorn-server
//! ============
//! Acorn's engines on this machine, its window in a browser. It opens
//! every chat (`hub`), keeps what they say as numbered events, and serves
//! the web window (web/dist, `web/build.sh`) and its API:
//!
//! - `GET /api/hello`: the assistants, and the last event's number;
//! - `GET /api/events?after=N`: the events after N, waiting up to 25 s
//!   for one (`lost`: some were dropped, too old);
//! - `POST /api/cmd?chat=ID`: a `Command` (JSON) to a chat;
//! - `GET /api/image?chat=ID&name=F`: an image of a chat's.
//!
//! The agent runs commands on this machine, so every request needs the
//! server's token: opening the link it prints sets it as a cookie. The
//! token is kept in $ACORN_DIR/server-token. It listens on 127.0.0.1:8484
//! (`--addr` or $ACORN_ADDR to change).

use acorn_core::chat::hub;
use acorn_core::wire::{Command, Event};
use axum::body::Bytes;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

/// Events kept for polls that come late: at most so many, and bytes.
const KEEP: usize = 20_000;
const KEEP_BYTES: usize = 256 << 20;
/// How long a poll waits for an event.
const WAIT: Duration = Duration::from_secs(25);
const COOKIE: &str = "acorn_token";

/// The events said, numbered from 1: `[chat, event]` each, as JSON.
#[derive(Default)]
struct Log {
  /// The number of the first kept.
  first: u64,
  events: VecDeque<String>,
  bytes: usize,
}

impl Log {
  fn last(&self) -> u64 {
    self.first + self.events.len() as u64 - 1
  }

  fn push(&mut self, json: String) -> u64 {
    self.bytes += json.len();
    self.events.push_back(json);
    while self.events.len() > KEEP || self.bytes > KEEP_BYTES {
      let Some(old) = self.events.pop_front() else { break };
      self.bytes -= old.len();
      self.first += 1;
    }
    self.last()
  }
}

#[derive(Clone)]
struct Server {
  log: Arc<Mutex<Log>>,
  tick: watch::Receiver<u64>,
  said: Arc<watch::Sender<u64>>,
  token: Arc<String>,
}

impl Server {
  fn tell(&self, chat: &str, ev: &Event) {
    let Ok(json) = serde_json::to_string(&(chat, ev)) else { return };
    let seq = self.log.lock().unwrap().push(json);
    self.said.send_replace(seq);
  }
}

fn main() {
  dotenv();
  tokio::runtime::Runtime::new().expect("runtime").block_on(serve());
}

async fn serve() {
  let mut args = std::env::args().skip(1);
  let mut addr = std::env::var("ACORN_ADDR").unwrap_or_else(|_| "127.0.0.1:8484".into());
  let mut web = std::env::var("ACORN_WEB").map(PathBuf::from).unwrap_or_else(|_| "web/dist".into());
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--addr" => addr = args.next().expect("--addr HOST:PORT"),
      "--web" => web = args.next().expect("--web DIR").into(),
      _ => {
        eprintln!("usage: acorn-server [--addr HOST:PORT] [--web DIR]");
        std::process::exit(2);
      }
    }
  }
  if !web.join("index.html").exists() {
    eprintln!("acorn-server: no web window in {} (run web/build.sh)", web.display());
    std::process::exit(1);
  }

  let (said, tick) = watch::channel(0);
  let server = Server {
    log: Arc::new(Mutex::new(Log {
      first: 1,
      ..Log::default()
    })),
    tick,
    said: Arc::new(said),
    token: Arc::new(token()),
  };

  let (tx, mut heard) = tokio::sync::mpsc::unbounded_channel();
  hub::listen(tx);
  let teller = server.clone();
  tokio::spawn(async move {
    while let Some((chat, ev)) = heard.recv().await {
      teller.tell(&chat, &ev);
    }
  });
  tokio::spawn(async { hub::open_all() });

  let app = Router::new()
    .route("/api/hello", get(hello))
    .route("/api/events", get(events))
    .route("/api/cmd", post(cmd))
    .route("/api/image", get(image))
    .fallback_service(ServeDir::new(&web))
    .layer(CompressionLayer::new())
    .layer(middleware::from_fn_with_state(server.clone(), guard))
    .with_state(server.clone());

  let listener = tokio::net::TcpListener::bind(&addr)
    .await
    .unwrap_or_else(|err| panic!("acorn-server: can't listen on {addr}: {err}"));
  let host = match addr.split_once(':') {
    Some(("0.0.0.0", port)) => format!("localhost:{port}"),
    _ => addr.clone(),
  };
  println!("Acorn: http://{host}/?token={}", server.token);
  axum::serve(listener, app)
    .with_graceful_shutdown(async {
      let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .expect("acorn-server");
  acorn_core::machine::end();
}

/// `.env` in the working dir, if there: each KEY=value not already set
/// joins the environment (DEEPSEEK_API_KEY; see .env.example).
fn dotenv() {
  let Ok(text) = std::fs::read_to_string(".env") else { return };
  for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
    if let Some((k, v)) = line.split_once('=') {
      let (k, v) = (k.trim(), v.trim().trim_matches('"'));
      if std::env::var_os(k).is_none() {
        // SAFETY: at the start, before any other thread runs.
        unsafe { std::env::set_var(k, v) };
      }
    }
  }
}

/// The server's token: kept in $ACORN_DIR, made the first time.
fn token() -> String {
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
  if std::fs::write(&file, &t).is_ok() {
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
  }
  t
}

/// Every request shows the token: a link's `?token=` sets the cookie and
/// leads on without it; else the cookie must hold it.
async fn guard(State(s): State<Server>, req: Request, next: Next) -> Response {
  let query = req.uri().query().unwrap_or("");
  if let Some(t) = query.split('&').find_map(|kv| kv.strip_prefix("token=")) {
    if same(t, &s.token) {
      let cookie = format!("{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000", s.token);
      let mut res = Redirect::to(req.uri().path()).into_response();
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
  next.run(req).await
}

/// Equal, in time that doesn't tell where they differ.
fn same(a: &str, b: &str) -> bool {
  a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

async fn hello(State(s): State<Server>) -> Response {
  let seq = s.log.lock().unwrap().last();
  Json(serde_json::json!({ "assistants": acorn_core::assistant::all(), "seq": seq })).into_response()
}

async fn events(State(s): State<Server>, Query(q): Query<HashMap<String, String>>) -> Response {
  let after: u64 = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
  let mut tick = s.tick.clone();
  let _ = tokio::time::timeout(WAIT, tick.wait_for(|&last| last > after)).await;
  let log = s.log.lock().unwrap();
  let last = log.last();
  let lost = after + 1 < log.first;
  let from = (after + 1).max(log.first);
  let skip = (from - log.first) as usize;
  let events: Vec<&str> = log.events.iter().skip(skip).map(String::as_str).collect();
  let body = format!(
    "{{\"seq\":{},\"lost\":{lost},\"events\":[{}]}}",
    last,
    events.join(",")
  );
  ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn cmd(State(s): State<Server>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
  let Some(chat) = q.get("chat") else {
    return (StatusCode::BAD_REQUEST, "no chat").into_response();
  };
  let cmd: Command = match serde_json::from_slice(&body) {
    Ok(cmd) => cmd,
    Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
  };
  match hub::get(chat) {
    Some(engine) => {
      engine.command(cmd);
      StatusCode::NO_CONTENT.into_response()
    }
    None => {
      let name = acorn_core::assistant::all()
        .into_iter()
        .find(|a| a.id == hub::whose(chat))
        .map_or_else(|| chat.clone(), |a| a.name);
      s.tell(chat, &Event::Note(format!("{name}'s chat is still opening: send again in a moment.")));
      StatusCode::ACCEPTED.into_response()
    }
  }
}


async fn image(Query(q): Query<HashMap<String, String>>, _: HeaderMap) -> Response {
  let (Some(chat), Some(name)) = (q.get("chat"), q.get("name")) else {
    return StatusCode::BAD_REQUEST.into_response();
  };
  let plain = |s: &str| !s.is_empty() && !s.starts_with('.') && !s.contains(['\\', '\0']);
  if !plain(name) || name.contains('/') || !chat.split('/').all(plain) || chat.split('/').count() > 2 {
    return StatusCode::BAD_REQUEST.into_response();
  }
  let path = hub::dir(chat).join("docs/images").join(name);
  let Ok(bytes) = tokio::fs::read(&path).await else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "webp" => "image/webp",
    _ => "application/octet-stream",
  };
  ([(header::CONTENT_TYPE, mime)], bytes).into_response()
}
