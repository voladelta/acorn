//! acorn-server
//! ============
//! Acorn's engines on this machine, its window in a browser. It opens
//! every chat (`hub`), keeps what they say as numbered events, and serves
//! the web window (web/dist: `cd web && npm run build`) and its API:
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
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
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

/// The server: its events, told as they come; which run of it this is
/// (`epoch`: its numbers start anew each run, so a window polling with
/// another run's knows it missed all since); its token.
#[derive(Clone)]
struct Server {
  epoch: u64,
  log: Arc<Mutex<Log>>,
  tick: watch::Receiver<u64>,
  said: Arc<watch::Sender<u64>>,
  token: Arc<String>,
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

async fn serve() {
  let mut args = std::env::args().skip(1);
  let mut addr = std::env::var("ACORN_ADDR").unwrap_or_else(|_| "127.0.0.1:8484".into());
  let mut unix: Option<PathBuf> = None;
  let mut web = std::env::var("ACORN_WEB")
    .map(PathBuf::from)
    .unwrap_or_else(|_| "web/dist".into());
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--addr" => addr = args.next().expect("--addr HOST:PORT"),
      "--web" => web = args.next().expect("--web DIR").into(),
      "--unix" => unix = Some(args.next().expect("--unix PATH").into()),
      _ => {
        eprintln!("usage: acorn-server [--addr HOST:PORT | --unix PATH] [--web DIR]");
        std::process::exit(2);
      }
    }
  }
  if !web.join("index.html").exists() {
    eprintln!(
      "acorn-server: no web window in {} (cd web && npm ci && npm run build)",
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
  tokio::spawn(async { hub::open_all() });

  let app = Router::new()
    .route("/api/hello", get(hello))
    .route("/api/events", get(events))
    .route("/api/cmd", post(cmd).layer(DefaultBodyLimit::max(CMD_BYTES)))
    .route("/api/image", get(image))
    .fallback_service(ServeDir::new(&web))
    .layer(
      CompressionLayer::new().compress_when(
        // a video or a sound is compressed already, and asked for in ranges
        DefaultPredicate::new()
          .and(NotForContentType::const_new("video/"))
          .and(NotForContentType::const_new("audio/")),
      ),
    )
    .layer(middleware::from_fn_with_state(server.clone(), guard))
    .with_state(server.clone());

  // A service stops with a TERM: end as Ctrl-C does, so `end` kills the
  // agents' jobs (a browser ignoring TERM would hold the stop up).
  let stop = async {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("a TERM handler");
    tokio::select! {
      _ = tokio::signal::ctrl_c() => {}
      _ = term.recv() => {}
    }
  };
  if let Some(path) = unix {
    // a socket left by a run that was killed
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)
      .unwrap_or_else(|err| panic!("acorn-server: can't listen on {}: {err}", path.display()));
    println!("Acorn: on {}", path.display());
    axum::serve(listener, app)
      .with_graceful_shutdown(stop)
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
      .with_graceful_shutdown(stop)
      .await
      .expect("acorn-server");
  }
  acorn_core::machine::end();
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
    .and_then(|mut f| f.write_all(t.as_bytes()));
  t
}

/// Every request shows the token: a link's `?token=` sets the cookie and
/// leads on without it; else the cookie must hold it.
async fn guard(State(s): State<Server>, req: Request, next: Next) -> Response {
  let query = req.uri().query().unwrap_or("");
  if let Some(t) = query.split('&').find_map(|kv| kv.strip_prefix("token=")) {
    if same(t, &s.token) {
      let cookie = format!(
        "{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000",
        s.token
      );
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
  let log = s.log.lock().unwrap();
  let last = log.last();
  let lost = !ours || after.saturating_add(1) < log.first;
  let from = if ours {
    after.saturating_add(1).max(log.first)
  } else {
    last.saturating_add(1)
  };
  let skip = from.saturating_sub(log.first) as usize;
  let events: Vec<&str> = log.events.iter().skip(skip).map(String::as_str).collect();
  let body = format!(
    "{{\"epoch\":{},\"seq\":{},\"lost\":{lost},\"events\":[{}]}}",
    s.epoch,
    last,
    events.join(",")
  );
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
