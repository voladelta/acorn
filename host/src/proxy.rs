//! The host's front: POST `/join/<code>` signs a device in; every other
//! request needs a session, and goes to that person's server, and only there,
//! with that server's token in place of the client's cookies. Bodies stream
//! both ways with no limit or timeout of the host's own (`/api/cmd` takes up
//! to 160 MB, `/api/events` waits 25 s).

use crate::runner::{self, Plan, Runner};
use crate::store::{self, Store};
use axum::body::Body;
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SESSION: &str = "acorn_session";
/// How long a request waits for its server to come up.
const UP_WAIT: Duration = Duration::from_secs(10);

pub struct Host {
  pub store: Store,
  pub plan: Plan,
  pub secure: bool,
  pub runners: Mutex<HashMap<String, Arc<Runner>>>,
}

impl Host {
  /// The person's runner, started if it isn't.
  pub fn runner(&self, id: &str) -> Arc<Runner> {
    let mut runners = self.runners.lock().unwrap();
    runners
      .entry(id.to_string())
      .or_insert_with(|| Runner::start(&self.plan, id))
      .clone()
  }
}

pub fn app(host: Arc<Host>) -> axum::Router {
  axum::Router::new()
    .route("/join/{code}", axum::routing::get(preview).post(join))
    .fallback(forward)
    .with_state(host)
}

const GONE: &str = "This link was used or has expired. Ask for a new one.\n";

/// The page a join link opens: a button, so a link preview fetching it
/// doesn't burn the code.
async fn preview(State(host): State<Arc<Host>>, UrlPath(code): UrlPath<String>) -> Response {
  let live = tokio::task::spawn_blocking(move || host.store.live(&code)).await;
  match live {
    Ok(Ok(true)) => (
      StatusCode::OK,
      [
        (header::CACHE_CONTROL, "no-store"),
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
      ],
      JOIN_PAGE,
    )
      .into_response(),
    Ok(Ok(false)) => gone(),
    Ok(Err(e)) => {
      eprintln!("acorn-host: join: {e}");
      (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.\n").into_response()
    }
    Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
  }
}

const JOIN_PAGE: &str = r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Open Acorn</title><style>body{margin:0;min-height:100vh;display:grid;place-items:center;font:16px system-ui,sans-serif}button{font:inherit;padding:.75em 2em;border-radius:.5em;border:1px solid;cursor:pointer}</style><form method="post"><button>Open Acorn</button></form>"#;

fn gone() -> Response {
  (StatusCode::GONE, [(header::CACHE_CONTROL, "no-store")], GONE).into_response()
}

async fn join(State(host): State<Arc<Host>>, UrlPath(code): UrlPath<String>, headers: HeaderMap) -> Response {
  // only the join page's own button: another site can't sign a browser in to
  // an account of its choosing (browsers send Sec-Fetch-Site; tools don't)
  if headers
    .get("sec-fetch-site")
    .is_some_and(|v| v != "same-origin" && v != "none")
  {
    return (StatusCode::FORBIDDEN, "Open the link itself.\n").into_response();
  }
  let joined = {
    let host = host.clone();
    tokio::task::spawn_blocking(move || host.store.join(&code)).await
  };
  match joined {
    Ok(Ok(Some((user, session)))) => {
      host.runner(&user);
      let secure = if host.secure { "; Secure" } else { "" };
      let cookie = format!("{SESSION}={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000{secure}");
      let mut res = (StatusCode::SEE_OTHER, [(header::LOCATION, "/")]).into_response();
      res.headers_mut().insert(header::SET_COOKIE, cookie.parse().unwrap());
      res
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
      res
    }
    Ok(Ok(None)) => gone(),
    Ok(Err(e)) => {
      eprintln!("acorn-host: join: {e}");
      (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.\n").into_response()
    }
    Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
  }
}

/// The session cookie's value, if the request has one.
fn session(req: &Request) -> Option<String> {
  req
    .headers()
    .get_all(header::COOKIE)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(';'))
    .find_map(|c| c.trim().strip_prefix(&format!("{SESSION}=")).map(str::to_string))
}

async fn forward(State(host): State<Arc<Host>>, mut req: Request) -> Response {
  let denied = || (StatusCode::UNAUTHORIZED, "Open your invite link.\n").into_response();
  let Some(value) = session(&req) else {
    return denied();
  };
  let book = match host.store.read() {
    Ok(book) => book,
    Err(e) => {
      eprintln!("acorn-host: {e}");
      return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
  };
  let Some(user) = store::who(&book, &value) else {
    return denied();
  };
  let runner = host.runner(&user);

  // the server's token, and none of the client's cookies
  let headers = req.headers_mut();
  headers.remove(header::COOKIE);
  for hop in [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "upgrade",
    "proxy-authorization",
  ] {
    headers.remove(hop);
  }
  headers.insert(header::HOST, HeaderValue::from_static("localhost"));
  let started = Instant::now();
  let sock = runner.sock();
  let stream = loop {
    // the token is read at each start of the server
    let token = runner.token();
    if !token.is_empty()
      && runner::is_sock(&sock)
      && let Ok(s) = tokio::net::UnixStream::connect(&sock).await
    {
      req
        .headers_mut()
        .insert(header::COOKIE, format!("acorn_token={token}").parse().unwrap());
      break s;
    }
    if started.elapsed() >= UP_WAIT {
      return (
        StatusCode::SERVICE_UNAVAILABLE,
        "Your Acorn is starting. Try again in a moment.\n",
      )
        .into_response();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  };
  let (mut send, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
    Ok(pair) => pair,
    Err(e) => return bad_gateway(e),
  };
  tokio::spawn(async move {
    let _ = conn.await;
  });
  // only the path goes on: the server is on a socket
  let path = req.uri().path_and_query().map_or("/", |p| p.as_str()).to_string();
  *req.uri_mut() = path.parse().unwrap_or_default();
  match send.send_request(req).await {
    Ok(res) => {
      let (mut parts, body) = res.into_parts();
      // the server's own cookie stays between it and the host
      let kept: Vec<HeaderValue> = parts
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter(|v| !v.to_str().is_ok_and(|v| v.trim_start().starts_with("acorn_token=")))
        .cloned()
        .collect();
      parts.headers.remove(header::SET_COOKIE);
      for v in kept {
        parts.headers.append(header::SET_COOKIE, v);
      }
      Response::from_parts(parts, Body::new(body))
    }
    Err(e) => bad_gateway(e),
  }
}

fn bad_gateway(e: impl std::fmt::Display) -> Response {
  eprintln!("acorn-host: forwarding: {e}");
  (StatusCode::BAD_GATEWAY, "Your Acorn didn't answer.\n").into_response()
}
