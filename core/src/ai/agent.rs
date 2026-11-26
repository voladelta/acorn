//! Agent
//! =====
//! Gemini in Acorn's own loop. Each step sends every item and streams
//! one output; tool calls run in order, and the call ends at a step with
//! no tools and nothing sent. A message that cuts in (`Say::cut`: the
//! user's, or whoever the call works for) stops the step in flight, as a
//! person spoken to stops and listens: what it streamed is logged "[cut
//! off]", its reply so far goes back as said, a tool call it cut answers
//! what it did and that it was cut, one it never ran that it wasn't, and
//! the call goes on with the message. Any other message follows the step's
//! results; a call that ends first leaves every message it heard (`left`). A request goes again, TRIES times
//! at most, after its retry-after or a doubling wait, for a passing reason
//! (busy: no connection, a retry status, a stream error, cut short or
//! silent for IDLE), dropping what it streamed (`pace` keeps a busy
//! model's room). A call ends answered (`Fail`: the model's answer failed,
//! a refusal or a stop reason, or the request itself was refused as
//! malformed or too long: its input's doing, asked again it fails alike)
//! or not: `Down`, the service's (a refusal of the key or the account, as
//! when its credit is gone, or busy past TRIES), or the daily budget's,
//! spent before a request (`chat::over_budget`: no request goes, and its
//! why is told, so a patient call waits it out as an outage); `Lost`, ours
//! (a stop). A refusal's why is told, as the user may mend it (a key, the
//! credit); a busy service's goes to the log, the user reading only that
//! it is busy. A request past the model's context goes again with less
//! room for its output, down to LEAST; past that, the call's older tool
//! results are dropped (`shed`), a half at a time. A text purged since
//! a call's prompt was built (`Toolbox::purged`) is `[purged]` in all the
//! call sends after: that prompt and its items. A stop ends the call at
//! once, its request cut. What each request used goes to its meter; one
//! cut short (a stop's, a message's, a stream's end) tells no usage, so
//! what it sent and read is counted (`used`).

use super::gemini::{self, Got, Halt, IDLE, Item, MAX_OUTPUT_TOKENS, Step, URL};
use super::{Ask, End, Event, On, Out, Spec, Stop, Used, cache, nap, pace};
use crate::wire::{Live, live_push};
use crate::{Kind, Say, clip, config, price};
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const TRIES: u32 = 8;

/// What a call the service never answered says.
const BUSY: &str = "the AI service is busy or unavailable right now";

/// The least room for its output a request is sent with: with less left
/// of the model's context, the call sheds.
const LEAST: u64 = 32_768;

/// What a tool result dropped to fit the model's context reads.
const SHED: &str = "[dropped: this call outgrew the model's context; run it again if still needed]";

/// What a tool call a message cut in on answers: cut while it ran (after
/// what it did), or never run.
const CUT: &str = "[cut off: a message came in while it ran]";
const UNRUN: &str = "not run: a message came in first";

pub static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
  reqwest::Client::builder()
    .user_agent("Acorn/0.1")
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .expect("an HTTP client")
});

/// One try of a call: its steps till it ends; the messages it heard but
/// never took, left.
pub async fn run(ask: &Ask, on: &On, rx: &mut mpsc::UnboundedReceiver<Say>, cancel: &CancellationToken) -> End {
  let mut end = End::default();
  let mut heard = Heard {
    rx,
    open: true,
    says: Vec::new(),
  };
  if let Err(why) = turns(ask, on, &mut heard, cancel, &mut end).await
    && end.text.is_empty()
  {
    end.text = why;
  }
  end.left.append(&mut heard.says);
  if cancel.is_cancelled() {
    end.ok = false;
    end.text = "stopped".into();
  }
  end
}

async fn turns(
  ask: &Ask,
  on: &On,
  heard: &mut Heard<'_>,
  cancel: &CancellationToken,
  end: &mut End,
) -> Result<(), String> {
  let key = &config::get().key;
  if key.is_empty() {
    return Err(format!("no API key: set {}", config::KEY_VAR));
  }
  let tools: Vec<Spec> = ask.tools.as_ref().map(|t| t.specs()).unwrap_or_default();
  let mut items = vec![Item::User {
    parts: parts(ask.blocks.iter().map(|b| &b.text)),
    imgs: ask.imgs.clone(),
  }];
  let mut room = MAX_OUTPUT_TOKENS;
  while !cancel.is_cancelled() {
    let cut = cancel.child_token();
    let mut body = gemini::body(ask, &items, &tools, room);
    // one purged as the call ran: its prompt, built before, holds it
    let secrets = ask.tools.as_ref().map(|t| t.purged()).unwrap_or_default();
    gemini::veil(&mut body, &secrets);
    let step = heard.till_cut(step(ask, on, key, body, &cut), &cut).await;
    if step.halt == Halt::Fail && context_limit(&step.why) && fit(&mut room, &mut items, on) {
      continue;
    }
    for e in &step.entries {
      tell(on, e.kind, &e.text, e.of.clone());
    }
    if cancel.is_cancelled() {
      return Ok(());
    }
    let mut backs = Vec::new();
    // cut before it gave a whole output (one that raced the cut runs
    // no tools: each answers it wasn't run)
    if cut.is_cancelled() && step.halt == Halt::Lost {
      let said = said(&step.entries);
      if !said.is_empty() {
        items.push(Item::Said(said.join("\n\n")));
      }
    } else if let Some(stop) = match step.halt {
      Halt::Down | Halt::Capped => Some(Stop::Down),
      Halt::Lost => Some(Stop::Lost),
      Halt::Fail => Some(Stop::Fail),
      _ => None,
    } {
      if end.text.is_empty() {
        end.text.clone_from(&step.why);
      }
      end.stop = Some(stop);
      return Ok(());
    } else {
      items.push(Item::Turn(step.out));
      for u in step.uses {
        let out = used_tool(ask, &tools, u.name.clone(), u.input, heard, &cut).await;
        if cancel.is_cancelled() {
          return Ok(());
        }
        tell(on, Kind::Echo, &out.text, Some(u.id.clone()));
        backs.push((u.id, u.name, out));
      }
    }
    let says = heard.all();
    if backs.is_empty() && says.is_empty() {
      end.ok = true;
      end.text = said(&step.entries).join("\n\n");
      return Ok(());
    }
    for say in &says {
      tell(on, Kind::User, &say.text, None);
    }
    if !backs.is_empty() {
      items.push(Item::Back(backs));
    }
    if !says.is_empty() {
      let parts = parts(says.iter().map(|s| &s.text));
      let imgs = says.into_iter().flat_map(|s| s.imgs).collect();
      items.push(Item::User { parts, imgs });
    }
  }
  Ok(())
}

/// A message done, told.
fn tell(on: &On, kind: Kind, text: &str, of: Option<String>) {
  on(Event::Entry {
    kind,
    text: text.to_string(),
    of,
  });
}

/// The texts a user's item is made of: those that say something.
fn parts<'a>(texts: impl Iterator<Item = &'a String>) -> Vec<String> {
  texts.filter(|t| !t.is_empty()).cloned().collect()
}

/// What a step's output said: its replies.
fn said(entries: &[gemini::Entry]) -> Vec<&str> {
  entries
    .iter()
    .filter(|e| e.kind == Kind::Acorn)
    .map(|e| e.text.as_str())
    .collect()
}

fn context_limit(why: &str) -> bool {
  let why = why.to_ascii_lowercase();
  [
    "input token",
    "context length",
    "context window",
    "input context",
    "http 413",
  ]
  .iter()
  .any(|part| why.contains(part))
}

/// Each retry leaves less room for output or drops older tool results.
fn fit(room: &mut u64, items: &mut [Item], on: &On) -> bool {
  if *room > LEAST {
    *room = (*room / 2).max(LEAST);
    return true;
  }
  if shed(items) {
    *room = MAX_OUTPUT_TOKENS;
    tell(
      on,
      Kind::Mind,
      "This call outgrew the model's context: its older tool results were dropped.",
      None,
    );
    return true;
  }
  false
}

/// A tool call of a step's output, answered: run, unless a message cut in
/// first; one cut as it ran says so after what it did; a failure reads
/// as an error.
async fn used_tool(
  ask: &Ask,
  tools: &[Spec],
  name: String,
  input: Value,
  heard: &mut Heard<'_>,
  cut: &CancellationToken,
) -> Out {
  let out = if cut.is_cancelled() {
    Out::fail(UNRUN)
  } else {
    let out = heard.till_cut(tool(ask, tools, name, input, cut), cut).await;
    if cut.is_cancelled() {
      Out {
        text: format!("{}\n{CUT}", out.text),
        ..out
      }
    } else {
      out
    }
  };
  if out.fail {
    Out {
      text: format!("Error: {}", out.text),
      ..out
    }
  } else {
    out
  }
}

/// The older half (by size) of the tool results a call still holds,
/// dropped; whether any was.
fn shed(items: &mut [Item]) -> bool {
  let size = |out: &Out| out.text.len() + out.imgs.iter().map(|i| i.data.len()).sum::<usize>();
  let held = |out: &&mut Out| out.text != SHED || !out.imgs.is_empty();
  let mut outs: Vec<&mut Out> = items
    .iter_mut()
    .filter_map(|item| match item {
      Item::Back(backs) => Some(backs.iter_mut().map(|(_, _, out)| out)),
      _ => None,
    })
    .flatten()
    .filter(held)
    .collect();
  let half = outs.iter().map(|out| size(out)).sum::<usize>() / 2;
  let mut dropped = 0;
  for out in &mut outs {
    if dropped > half {
      break;
    }
    dropped += size(out).max(1);
    out.text = SHED.into();
    out.imgs.clear();
  }
  dropped > 0
}

/// The messages sent to a call as it runs: those heard so far wait in
/// `says` for the step's end.
struct Heard<'a> {
  rx: &'a mut mpsc::UnboundedReceiver<Say>,
  open: bool,
  says: Vec<Say>,
}

impl Heard<'_> {
  /// `work` run till it ends, hearing messages meanwhile; one that cuts
  /// in cuts the step (`cut`), and `work` then ends what it cut short.
  async fn till_cut<T>(&mut self, work: impl Future<Output = T>, cut: &CancellationToken) -> T {
    tokio::pin!(work);
    loop {
      tokio::select! {
          biased;
          done = &mut work => return done,
          say = self.rx.recv(), if self.open && !cut.is_cancelled() => match say {
              Some(say) => {
                  if say.cut {
                      cut.cancel();
                  }
                  self.says.push(say);
              }
              None => self.open = false,
          },
      }
    }
  }

  /// Every message heard, and those sent since.
  fn all(&mut self) -> Vec<Say> {
    while let Ok(say) = self.rx.try_recv() {
      self.says.push(say);
    }
    std::mem::take(&mut self.says)
  }
}

/// A request, asked again while the service is busy. Asking again, it
/// holds its room (`pace`): its model's, or its import's, which one request
/// holds at a time while the rest wait on it.
async fn step(ask: &Ask, on: &On, key: &str, body: Value, cancel: &CancellationToken) -> Step {
  let model = ask.model.name();
  let own = ask
    .yields
    .as_ref()
    .map_or_else(|| model.to_string(), |import| format!("{model} {import}"));
  let mut asking = None;
  let mut k = 1;
  loop {
    if cancel.is_cancelled() {
      break fail("stopped", &[], Halt::Lost);
    }
    if let Some(why) = crate::chat::over_budget() {
      break fail(&why, &[], Halt::Capped);
    }
    let waits: Vec<&str> = match (&ask.yields, asking.is_some()) {
      (None, _) => vec![],
      (Some(_), true) => vec![model],
      (Some(_), false) => vec![model, &own],
    };
    pace::yielded(&waits, cancel).await;
    let Some(Sent { after, step }) = request(ask, on, key, &body, cancel).await else {
      break fail("stopped", &[], Halt::Lost);
    };
    if step.halt != Halt::Busy || k >= TRIES {
      break if step.halt == Halt::Busy {
        eprintln!("{model}: {}", clip::head(&step.why, 300));
        Step {
          halt: Halt::Down,
          why: BUSY.into(),
          ..step
        }
      } else {
        step
      };
    }
    k += 1;
    if asking.is_none() && own != model && pace::held(&own) {
      continue;
    }
    if asking.is_none() {
      asking = Some(pace::hold(model, &own, &step.why));
    }
    let wait = after
      .filter(|&s| s.is_finite() && s > 0.0)
      .unwrap_or_else(|| 2f64.powi(k as i32 - 1))
      .min(300.0);
    tell(
      on,
      Kind::Mind,
      &format!("The AI service is busy: asking again in {wait} s."),
      None,
    );
    let jitter = rand::random::<f64>() * 0.5;
    nap(Duration::from_secs_f64(wait + jitter), cancel).await;
  }
}

/// A request sent: the wait its response asked for (retry-after, s), its
/// step.
struct Sent {
  after: Option<f64>,
  step: Step,
}

/// One request, in single flight (`cache`) and at its model's pace; None:
/// stopped before it was sent.
async fn request(ask: &Ask, on: &On, key: &str, body: &Value, cancel: &CancellationToken) -> Option<Sent> {
  #[cfg(test)]
  if std::env::var_os("ACORN_TEST_CAPPED_REQUEST").is_some() {
    tests::DISPATCHED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    return Some(Sent {
      after: None,
      step: fail("unexpected dispatch", &[], Halt::Fail),
    });
  }
  let mut flight = tokio::select! {
      flight = cache::wait(ask) => flight,
      () = cancel.cancelled() => return None,
  };
  let seat = pace::fly(ask.model.name(), ask.yields.is_some(), cancel).await?;
  let (status, sent) = send(ask, on, key, body, &mut flight, cancel).await;
  seat.land(status == 429 || status == 529);
  Some(sent)
}

/// A request sent: its HTTP status (0: none came), and what it gave. A stop cuts it here, not by dropping it, so that what it used
/// till then is still metered.
async fn send(
  ask: &Ask,
  on: &On,
  key: &str,
  body: &Value,
  flight: &mut cache::Flight,
  cancel: &CancellationToken,
) -> (u16, Sent) {
  let json = body.to_string();
  let seen = Mutex::new(Vec::new());
  let mut got = Got::default();
  let mut read = Read::default();
  let mut status = 0;
  let post = HTTP
    .post(url())
    .header("x-goog-api-key", key)
    .header("content-type", "application/json")
    .body(json.clone())
    .send();
  let res = tokio::select! {
      res = tokio::time::timeout(IDLE, post) => Some(res),
      () = cancel.cancelled() => None,
  };
  let (after, step, ok) = match res {
    None => (None, fail("stopped", &[], Halt::Lost), false),
    Some(Err(_)) => (None, silent(&[]), false),
    Some(Ok(Err(err))) => (None, fail(&err.to_string(), &[], Halt::Busy), false),
    Some(Ok(Ok(res))) => {
      status = res.status().as_u16();
      let after = header(&res, "retry-after").and_then(|v| retry_after(&v));
      if res.status().is_success() {
        let step = stream(res, on, flight, &seen, cancel, &mut got, &mut read).await;
        (after, step, true)
      } else {
        (after, refused(res, ask.model.name(), key, cancel).await, false)
      }
    }
  };
  flight.end();
  if ok && let Some(meter) = &ask.meter {
    let usage = used(read.usage, read.bytes, json.len(), flight.cached);
    let now = chrono::Utc::now();
    meter(Used {
      effort: ask.effort,
      usage,
      micros: price::cost(ask.model, usage, now),
      at: now.timestamp_millis(),
    });
  }
  (status, Sent { after, step })
}

/// A header of a response, if it is text.
fn header(res: &reqwest::Response, name: &str) -> Option<String> {
  res
    .headers()
    .get(name)
    .and_then(|v| v.to_str().ok())
    .map(str::to_string)
}

/// A response that refused the request, as the step it ends: busy where
/// the service says to ask again (else by its status), failed where the
/// request is at fault, else down.
async fn refused(res: reqwest::Response, model: &str, key: &str, cancel: &CancellationToken) -> Step {
  let status = res.status().as_u16();
  let id = header(&res, "x-request-id")
    .or_else(|| header(&res, "x-goog-request-id"))
    .map_or(String::new(), |id| format!(" [{id}]"));
  // its why may stall as a stream does: a stop cuts it too
  let told = tokio::select! {
      told = tokio::time::timeout(IDLE, res.text()) => told,
      () = cancel.cancelled() => return fail("stopped", &[], Halt::Lost),
  };
  let body = told.ok().and_then(Result::ok).unwrap_or_default();
  let error = serde_json::from_str::<Value>(&body).unwrap_or_default();
  let key_error = error["error"]["details"]
    .as_array()
    .into_iter()
    .flatten()
    .any(|d| d["reason"].as_str().is_some_and(|r| r.starts_with("API_KEY_")));
  let halt = match status {
    408 | 409 | 429 | 500..=599 => Halt::Busy,
    400 | 413 | 422 if !key_error => Halt::Fail,
    _ => Halt::Down,
  };
  let why = error["error"]["message"]
    .as_str()
    .unwrap_or_else(|| clip::head(&body, 2000))
    .replace(key, super::PURGED);
  if halt != Halt::Busy {
    eprintln!("{model}: HTTP {status}{id}: {}", clip::head(&why, 300));
  }
  let why = match halt {
    Halt::Busy => format!("HTTP {status}{id}: {why}"),
    _ => format!("the AI service refused the request (HTTP {status}): {why}"),
  };
  fail(&why, &[], halt)
}

/// A request the API said nothing to for IDLE: busy.
fn silent(seen: &[Live]) -> Step {
  fail(
    &format!("no word from the API in {} s", IDLE.as_secs()),
    seen,
    Halt::Busy,
  )
}

/// What a request's stream read: the bytes of its output, and whether it
/// ended (its last event read, its usage all told).
#[derive(Default)]
struct Read {
  bytes: u64,
  usage: Option<price::Usage>,
}

/// Where requests go: Gemini's API (a test's: its own).
fn url() -> String {
  #[cfg(test)]
  if let Ok(api) = tests::API.try_with(String::clone) {
    return api;
  }
  URL.to_string()
}

/// The stream's events, each told as it comes, as the step it gives.
async fn stream(
  res: reqwest::Response,
  on: &On,
  flight: &mut cache::Flight,
  seen: &Mutex<Vec<Live>>,
  cancel: &CancellationToken,
  got: &mut Got,
  read: &mut Read,
) -> Step {
  let mut bytes = res.bytes_stream();
  let mut rest: Vec<u8> = Vec::new();
  loop {
    let chunk = tokio::select! {
        biased;
        () = cancel.cancelled() => return fail("stopped", &seen.lock(), Halt::Lost),
        chunk = tokio::time::timeout(IDLE, bytes.next()) => chunk,
    };
    let chunk = match chunk {
      Err(_) => return silent(&seen.lock()),
      Ok(None) => return fail("the stream ended early", &seen.lock(), Halt::Busy),
      Ok(Some(Err(err))) => return fail(&err.to_string(), &seen.lock(), Halt::Busy),
      Ok(Some(Ok(chunk))) => chunk,
    };
    rest.extend_from_slice(&chunk);
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
      let line: Vec<u8> = rest.drain(..=nl).collect();
      let line = String::from_utf8_lossy(&line);
      let Some(data) = line.trim().strip_prefix("data:").map(str::trim) else {
        continue;
      };
      if data == "[DONE]" || data.is_empty() {
        continue;
      }
      let ev: Value = match serde_json::from_str(data) {
        Ok(ev) => ev,
        Err(err) => return fail(&err.to_string(), &seen.lock(), Halt::Busy),
      };
      if Got::begun(&ev) {
        flight.begun();
      }
      let mut tell = |kind: Kind, text: &str, fresh: bool| {
        read.bytes += text.len() as u64;
        live_push(&mut seen.lock(), kind, text, fresh);
        on(Event::Delta {
          kind,
          text: text.to_string(),
          fresh,
        });
      };
      if got.read(&ev, &mut tell) {
        read.usage = got.usage;
        let step = std::mem::take(got).step();
        return match step.halt {
          Halt::Fail | Halt::Busy => fail(&step.why, &seen.lock(), step.halt),
          _ => step,
        };
      }
    }
  }
}

/// A request's usage: the API's, told as its stream ends; else (cut
/// short, none told) an estimate, a token each BYTES of what it sent (the
/// `cached` first of them read from the cache, the rest written) and of
/// the output it read.
fn used(usage: Option<price::Usage>, read: u64, sent: usize, cached: usize) -> price::Usage {
  let tokens = |bytes: usize| (bytes as u64).div_ceil(BYTES);
  let cached = cached.min(sent);
  usage.unwrap_or_else(|| price::Usage {
    input: tokens(sent - cached),
    write: 0,
    read: tokens(cached),
    output: read.div_ceil(BYTES),
  })
}

/// The approximate bytes per token when a stopped stream reports no usage.
const BYTES: u64 = 3;

/// A retry-after: seconds, or an HTTP date.
fn retry_after(v: &str) -> Option<f64> {
  v.trim().parse::<f64>().ok().or_else(|| {
    let at = chrono::DateTime::parse_from_rfc2822(v).ok()?;
    Some(((at.timestamp_millis() - crate::now_ms()) as f64 / 1000.0).max(0.0))
  })
}

/// A step that failed: what streamed, marked where it was cut (a stop's,
/// or a message's that cut in, bare; a tool call cut so, never made, is
/// dropped).
fn fail(why: &str, seen: &[Live], halt: Halt) -> Step {
  let shown = if halt == Halt::Busy { BUSY } else { why };
  let mark = match halt {
    Halt::Lost => "\n[cut off]".to_string(),
    _ => format!("\n[cut off: {}]", clip::head(shown, 300)),
  };
  let entries = seen
    .iter()
    .filter(|l| !l.text.trim().is_empty() && !(halt == Halt::Lost && l.kind == Kind::Tool))
    .map(|l| gemini::Entry {
      kind: l.kind,
      text: format!("{}{mark}", l.text.trim()),
      of: None,
    })
    .collect();
  Step {
    out: vec![],
    uses: vec![],
    entries,
    halt,
    why: why.to_string(),
  }
}

/// A tool call, run.
async fn tool(ask: &Ask, tools: &[Spec], name: String, input: Value, cancel: &CancellationToken) -> Out {
  let Some(toolbox) = ask.tools.as_ref().filter(|_| tools.iter().any(|t| t.name == name)) else {
    return Out::fail(format!("No tool {name}."));
  };
  if !input.is_object() {
    return Out::fail("The input is not a JSON object.");
  }
  let out = toolbox.call(name, input, cancel.child_token()).await;
  if out.text.is_empty() {
    Out {
      text: "(no output)".into(),
      ..out
    }
  } else {
    out
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};

  pub(super) static DISPATCHED: AtomicUsize = AtomicUsize::new(0);

  tokio::task_local! {
    /// The API a test's requests go to (`api`).
    pub(super) static API: String;
  }

  const OVER: &str = "The input context exceeds the model's context window.";

  /// A local API, by its URL: each request's JSON goes to `answer`, whose
  /// status and body it answers with, a 200's as an event stream left
  /// open.
  async fn api(answer: impl Fn(Value) -> (u16, String) + Send + Sync + 'static) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let answer = std::sync::Arc::new(answer);
    tokio::spawn(async move {
      while let Ok((mut sock, _)) = listener.accept().await {
        let answer = answer.clone();
        tokio::spawn(async move {
          let (mut req, mut buf) = (Vec::new(), [0u8; 8192]);
          let json = loop {
            let n = sock.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
              return;
            }
            req.extend_from_slice(&buf[..n]);
            let Some(at) = req.windows(4).position(|w| w == b"\r\n\r\n") else {
              continue;
            };
            let head = String::from_utf8_lossy(&req[..at]).to_lowercase();
            let len = head.lines().find_map(|l| l.strip_prefix("content-length:"));
            let len: usize = len.map_or(0, |v| v.trim().parse().unwrap());
            if req.len() >= at + 4 + len {
              break serde_json::from_slice(&req[at + 4..]).unwrap();
            }
          };
          let (status, body) = answer(json);
          let head = if status == 200 {
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n".to_string()
          } else {
            format!(
              "HTTP/1.1 {status} No\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
              body.len()
            )
          };
          let _ = sock.write_all((head + &body).as_bytes()).await;
          if status == 200 {
            let _ = sock.read(&mut buf).await; // open till the client goes
          }
        });
      }
    });
    url
  }

  fn refusal(why: &str) -> String {
    serde_json::json!({"error": {"message": why}}).to_string()
  }

  fn events(events: &[Value]) -> String {
    events.iter().map(|ev| format!("data: {ev}\n\n")).collect()
  }

  fn answer(text: &str) -> String {
    use serde_json::json;
    events(&[
      json!({"event_type": "step.start", "index": 0, "step": {"type": "model_output"}}),
      json!({"event_type": "step.delta", "index": 0, "delta": {"type": "text", "text": text}}),
      json!({"event_type": "step.stop", "index": 0}),
      json!({"event_type": "interaction.completed", "interaction": {"status": "completed",
        "usage": {"total_input_tokens": 9, "total_cached_tokens": 5, "total_tool_use_tokens": 2,
          "total_output_tokens": 2, "total_thought_tokens": 7}}}),
    ])
  }

  fn ask(meter: Option<crate::ai::Meter>) -> Ask {
    Ask {
      model: crate::model::Model::Neon,
      effort: crate::effort::Effort::None,
      system: "test".into(),
      blocks: vec![crate::Block::new("hello", false)],
      imgs: vec![],
      tools: None,
      meter,
      patient: false,
      yields: None,
      user: None,
    }
  }

  /// One request of `ask` sent to the API at `url`.
  async fn sent(ask: &Ask, on: &On, url: String, cancel: &CancellationToken) -> (u16, Step) {
    let body = gemini::body(ask, &[], &[], MAX_OUTPUT_TOKENS);
    let mut flight = cache::wait(ask).await;
    let (status, sent) = API
      .scope(url, send(ask, on, "acorn-test-key", &body, &mut flight, cancel))
      .await;
    (status, sent.step)
  }

  /// A request a stop cuts as it streams ends at once, what it said kept,
  /// and is metered by what it sent and read (the API told no usage).
  #[tokio::test]
  async fn a_request_cut_is_metered() {
    let url = api(|_| {
      let stream = events(&[
        serde_json::json!({"event_type": "step.start", "index": 0, "step": {"type": "model_output"}}),
        serde_json::json!({"event_type": "step.delta", "index": 0, "delta": {"type": "text", "text": "Moving the files"}}),
      ]);
      (200, stream)
    })
    .await;
    let uses = std::sync::Arc::new(Mutex::new(Vec::new()));
    let meter: crate::ai::Meter = {
      let uses = uses.clone();
      std::sync::Arc::new(move |used: Used| uses.lock().push(used))
    };
    let ask = ask(Some(meter));
    let cancel = CancellationToken::new();
    let on: On = {
      let cancel = cancel.clone();
      std::sync::Arc::new(move |ev| {
        if matches!(ev, Event::Delta { .. }) {
          cancel.cancel();
        }
      })
    };
    let sending = sent(&ask, &on, url, &cancel);
    let (status, step) = tokio::time::timeout(Duration::from_secs(10), sending).await.unwrap();
    assert_eq!((status, step.halt), (200, Halt::Lost));
    assert_eq!(step.entries[0].text, "Moving the files\n[cut off]");
    let uses = uses.lock();
    assert_eq!(uses.len(), 1, "a request cut short is metered once");
    let json = gemini::body(&ask, &[], &[], MAX_OUTPUT_TOKENS).to_string();
    let usage = price::Usage {
      input: (json.len() as u64).div_ceil(BYTES),
      write: 0,
      read: 0,
      output: 6, // "Moving the files": 16 bytes
    };
    assert_eq!(uses[0].usage, usage);
    assert!(uses[0].micros > 0);
  }

  /// A refusal whose why never comes whole is cut by a stop as a stream
  /// is: the request ends at once, its seat and its flight let go.
  #[tokio::test]
  async fn a_refusal_stalled_is_cut() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
      let (mut sock, _) = listener.accept().await.unwrap();
      let mut buf = [0u8; 8192];
      let _ = sock.read(&mut buf).await;
      let head = "HTTP/1.1 400 No\r\ncontent-type: application/json\r\ncontent-length: 64\r\n\r\n{\"error\"";
      let _ = sock.write_all(head.as_bytes()).await;
      while sock.read(&mut buf).await.is_ok_and(|n| n > 0) {} // open till the client goes
    });
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(300)).await;
      stop.cancel();
    });
    let on: On = std::sync::Arc::new(|_| {});
    let ask = ask(None);
    let sending = sent(&ask, &on, url, &cancel);
    let (status, step) = tokio::time::timeout(Duration::from_secs(10), sending).await.unwrap();
    assert_eq!((status, step.halt), (400, Halt::Lost));
  }

  /// Cut short, a request's prompt counts as read where a call cached it.
  #[test]
  fn a_cut_request_reads_what_was_cached() {
    let usage = used(None, 300, 9000, 6000);
    let told = price::Usage {
      input: 1000,
      write: 0,
      read: 2000,
      output: 100,
    };
    assert_eq!(usage, told);
    assert_eq!(used(Some(told), 300, 9, 6), told, "the API's own usage wins");
  }

  /// A refusal tells its why, and whose doing it is: the account's
  /// (`Down`: a patient call waits it out), the request's (`Fail`: it
  /// would fail again); a busy service's stays in the log. None is
  /// metered.
  #[tokio::test]
  async fn a_refusal_tells_why() {
    let metered: crate::ai::Meter = std::sync::Arc::new(|_| panic!("a refusal uses nothing"));
    let ask = ask(Some(metered));
    let on: On = std::sync::Arc::new(|_| {});
    let cancel = CancellationToken::new();
    for (status, why, halt, told) in [
      (
        401,
        "The API key acorn-test-key is invalid",
        Halt::Down,
        "the AI service refused the request (HTTP 401): The API key [purged] is invalid",
      ),
      (
        402,
        "Insufficient Balance",
        Halt::Down,
        "the AI service refused the request (HTTP 402): Insufficient Balance",
      ),
      (
        400,
        "Content Exists Risk",
        Halt::Fail,
        "the AI service refused the request (HTTP 400): Content Exists Risk",
      ),
      (503, "Server overloaded", Halt::Busy, "HTTP 503: Server overloaded"),
    ] {
      let url = api(move |_| (status, refusal(why))).await;
      let (got, step) = sent(&ask, &on, url, &cancel).await;
      assert_eq!((got, step.halt, step.why.as_str()), (status, halt, told));
    }
    let url = api(|_| (400, refusal(OVER))).await;
    let (_, step) = sent(&ask, &on, url, &cancel).await;
    assert_eq!(step.halt, Halt::Fail);
    assert!(context_limit(&step.why));
    assert!(!context_limit("Content Exists Risk"));
  }

  /// A call past the model's context is sent again with the room its
  /// input leaves; one whose input leaves too little, and holds nothing
  /// to drop, fails telling why.
  #[tokio::test]
  async fn a_call_past_the_context_makes_room() {
    let test = "ai::agent::tests::a_call_past_the_context_makes_room";
    if std::env::var_os("ACORN_TEST_CONTEXT").is_none() {
      let dir = tempfile::tempdir().unwrap();
      let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("ACORN_TEST_CONTEXT", "1")
        .env("ACORN_DIR", dir.path())
        .env("ACORN_CHATS", dir.path().join("chats"))
        .env(config::KEY_VAR, "unused-test-key")
        .output()
        .unwrap();
      assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
      );
      return;
    }
    let rooms = std::sync::Arc::new(Mutex::new(Vec::new()));
    let url = {
      let rooms = rooms.clone();
      api(move |req| {
        let room = req["generation_config"]["max_output_tokens"].as_u64().unwrap();
        rooms.lock().push(room);
        if room == MAX_OUTPUT_TOKENS {
          return (400, refusal(OVER));
        }
        (200, answer("made it"))
      })
      .await
    };
    let ask = ask(None);
    let on: On = std::sync::Arc::new(|_| {});
    let (_tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let end = API.scope(url, run(&ask, &on, &mut rx, &cancel)).await;
    assert!(end.ok, "{}", end.text);
    assert_eq!(end.text, "made it");
    assert_eq!(*rooms.lock(), [MAX_OUTPUT_TOKENS, LEAST]);

    let url = api(move |_| (400, refusal(OVER))).await;
    let (_tx, mut rx) = mpsc::unbounded_channel();
    let end = API.scope(url, run(&ask, &on, &mut rx, &cancel)).await;
    assert_eq!((end.ok, end.stop), (false, Some(Stop::Fail)));
    assert!(end.text.contains(OVER), "{}", end.text);
  }

  /// A tool that purges `SECRET` when called: from then on the call's
  /// toolbox names it purged.
  struct Purger(std::sync::atomic::AtomicBool);

  const SECRET: &str = "tok\"en-12345";

  impl crate::ai::Toolbox for Purger {
    fn specs(&self) -> Vec<Spec> {
      vec![Spec {
        name: "purge",
        info: "Purges a text.".into(),
        schema: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
      }]
    }

    fn call(&self, _: String, input: Value, _: CancellationToken) -> futures_util::future::BoxFuture<'static, Out> {
      self.0.store(true, std::sync::atomic::Ordering::SeqCst);
      let text = input["text"].as_str().unwrap_or_default().to_string();
      Box::pin(async move { Out::ok(format!("Purged {text}")) })
    }

    fn purged(&self) -> Vec<String> {
      if self.0.load(std::sync::atomic::Ordering::SeqCst) {
        vec![SECRET.into()]
      } else {
        vec![]
      }
    }
  }

  /// A text purged as a call runs is in nothing the call sends after: its
  /// system prompt and view, built before, the tool call that named it
  /// (there escaped, as JSON in a string), that call's result.
  #[tokio::test]
  async fn a_call_sends_no_text_purged_as_it_ran() {
    let test = "ai::agent::tests::a_call_sends_no_text_purged_as_it_ran";
    if std::env::var_os("ACORN_TEST_PURGED").is_none() {
      let dir = tempfile::tempdir().unwrap();
      let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("ACORN_TEST_PURGED", "1")
        .env("ACORN_DIR", dir.path())
        .env("ACORN_CHATS", dir.path().join("chats"))
        .env(config::KEY_VAR, "unused-test-key")
        .output()
        .unwrap();
      assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
      );
      return;
    }
    let reqs = std::sync::Arc::new(Mutex::new(Vec::<Value>::new()));
    let url = {
      let reqs = reqs.clone();
      api(move |req| {
        let mut reqs = reqs.lock();
        reqs.push(req);
        if reqs.len() != 1 {
          return (200, answer("gone"));
        }
        use serde_json::json;
        let args = json!({"text": SECRET}).to_string();
        (200, events(&[
          json!({"event_type": "step.start", "index": 0, "step": {"type": "thought"}}),
          json!({"event_type": "step.delta", "index": 0, "delta": {"type": "thought_summary", "content": {
            "type": "text", "text": "I should purge the named key."}}}),
          json!({"event_type": "step.delta", "index": 0, "delta": {"type": "thought_signature", "signature": "opaque"}}),
          json!({"event_type": "step.stop", "index": 0}),
          json!({"event_type": "step.start", "index": 1, "step": {
            "type": "function_call", "id": "c1", "name": "purge", "arguments": {}}}),
          json!({"event_type": "step.delta", "index": 1, "delta": {"type": "arguments_delta", "arguments": &args[..8]}}),
          json!({"event_type": "step.delta", "index": 1, "delta": {"type": "arguments_delta", "arguments": &args[8..]}}),
          json!({"event_type": "step.stop", "index": 1}),
          json!({"event_type": "interaction.completed", "interaction": {"status": "requires_action"}}),
        ]))
      })
      .await
    };
    let ask = Ask {
      system: format!("Facts: the key is {SECRET}"),
      blocks: vec![
        crate::Block::new(format!("my key is {SECRET}, keep it"), true),
        crate::Block::new("no, purge it", false),
      ],
      tools: Some(std::sync::Arc::new(Purger(false.into()))),
      ..ask(None)
    };
    let on: On = std::sync::Arc::new(|_| {});
    let (_tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let end = API.scope(url, run(&ask, &on, &mut rx, &cancel)).await;
    assert!(end.ok, "{}", end.text);
    let reqs = reqs.lock();
    assert_eq!(reqs.len(), 2);
    // however it is escaped, a secret sent holds this
    let tail = "en-12345";
    assert!(
      reqs[0].to_string().contains(tail),
      "it was in the prompt before the purge"
    );
    assert!(!reqs[1].to_string().contains(tail), "{}", reqs[1]);
    assert_eq!(reqs[1]["system_instruction"], "Facts: the key is [purged]");
    let input = reqs[1]["input"].as_array().unwrap();
    assert_eq!(input[0]["content"][0]["text"], "my key is [purged], keep it");
    assert_eq!(input[0]["content"][1]["text"], "no, purge it");
    assert_eq!(input[1]["signature"], "opaque");
    assert_eq!(input[2]["arguments"]["text"], "[purged]");
    assert_eq!(input[3]["call_id"], "c1");
    assert_eq!(input[3]["name"], "purge");
    assert_eq!(input[3]["result"][0]["text"], "Purged [purged]");
  }

  /// Secrets that overlap are veiled as one: none of the longer shows
  /// around the shorter, whichever was purged first.
  #[test]
  fn secrets_that_overlap_are_veiled_whole() {
    let veiled = |text: &str, secrets: &[&str]| {
      let secrets: Vec<String> = secrets.iter().map(std::string::ToString::to_string).collect();
      let mut text = text.to_string();
      crate::ai::veil(&mut text, &secrets);
      text
    };
    let (short, long) = ("hunter2pass", "sk-hunter2pass-extra");
    assert_eq!(veiled("key sk-hunter2pass-extra.", &[short, long]), "key [purged].");
    assert_eq!(veiled("key sk-hunter2pass-extra.", &[long, short]), "key [purged].");
    assert_eq!(veiled("abcdefghijkl", &["abcdefgh", "efghijkl"]), "[purged]");
    // each apart is its own, one named twice is one
    assert_eq!(
      veiled("a hunter2pass, hunter2passé", &[short, short]),
      "a [purged], [purged]é"
    );
    assert_eq!(
      veiled("abcdefghefghijkl", &["abcdefgh", "efghijkl"]),
      "[purged][purged]"
    );
    assert_eq!(veiled("nothing held", &[short, long]), "nothing held");
  }

  #[test]
  fn a_request_veiled_keeps_its_shape() {
    let mut body = serde_json::json!({
      "system_instruction": "Facts: function_call",
      "input": [
        {"type": "user_input", "content": [
          {"type": "text", "text": "purge function_call"},
          {"type": "image", "mime_type": "image/png", "data": "function_call"},
        ]},
        {"type": "thought", "signature": "function_call"},
        {"type": "function_call", "id": "c1-function_call", "name": "purge",
         "arguments": {"name": "function_call", "data": "function_call"}},
      ],
    });
    gemini::veil(&mut body, &["function_call".into()]);
    assert_eq!(
      body,
      serde_json::json!({
        "system_instruction": "Facts: [purged]",
        "input": [
          {"type": "user_input", "content": [
            {"type": "text", "text": "purge [purged]"},
            {"type": "image", "mime_type": "image/png", "data": "function_call"},
          ]},
          {"type": "thought", "signature": "function_call"},
          {"type": "function_call", "id": "c1-function_call", "name": "purge",
           "arguments": {"name": "[purged]", "data": "[purged]"}},
        ],
      })
    );
  }

  /// Shedding drops the older half of a call's tool results, by size, and
  /// then the rest; nothing left to drop, it tells so.
  #[test]
  fn a_call_sheds_its_older_results() {
    let back = |id: &str, size: usize| (id.to_string(), "test".into(), Out::ok("x".repeat(size)));
    let mut items = vec![
      Item::User {
        parts: vec!["task".into()],
        imgs: vec![],
      },
      Item::Back(vec![back("a", 4000), back("b", 100)]),
      Item::Back(vec![back("c", 3000)]),
    ];
    let texts = |items: &[Item]| -> Vec<bool> {
      let outs = items.iter().filter_map(|item| match item {
        Item::Back(backs) => Some(backs.iter().map(|(_, _, out)| out.text == SHED)),
        _ => None,
      });
      outs.flatten().collect()
    };
    assert!(shed(&mut items));
    assert_eq!(texts(&items), [true, false, false], "a alone is over half");
    assert!(shed(&mut items));
    assert_eq!(texts(&items), [true, true, true], "b, then c: over half of the rest");
    assert!(!shed(&mut items));
  }

  #[tokio::test]
  async fn a_request_past_the_budget_waits_until_cancelled() {
    let test = "ai::agent::tests::a_request_past_the_budget_waits_until_cancelled";
    if std::env::var_os("ACORN_TEST_CAPPED_REQUEST").is_none() {
      let dir = tempfile::tempdir().unwrap();
      let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("ACORN_TEST_CAPPED_REQUEST", "1")
        .env("ACORN_DIR", dir.path())
        .env("ACORN_CHATS", dir.path().join("chats"))
        .env("ACORN_BUDGET", "0.01")
        .env(config::KEY_VAR, "unused-test-key")
        .output()
        .unwrap();
      assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
      );
      return;
    }
    let (tx, _events) = mpsc::unbounded_channel();
    assert!(crate::chat::hub::listen(tx));
    let engine = crate::chat::hub::chat(&crate::assistant::all()[0]).unwrap();
    engine
      .lock()
      .mem
      .db
      .execute(
        "insert into uses values (?, 'chat', 0, 0, 0, 20000)",
        [chrono::Local::now().timestamp_millis()],
      )
      .unwrap();
    let why = crate::chat::over_budget().expect("the fixture spent more than its daily cap");
    let ask = Ask {
      model: crate::model::Model::Neon,
      effort: crate::effort::Effort::None,
      system: "test".into(),
      blocks: vec![],
      imgs: vec![],
      tools: None,
      meter: None,
      patient: true,
      yields: None,
      user: None,
    };
    let on: On = std::sync::Arc::new(|_| panic!("a capped call emits no model output"));
    let (_tx, mut rx) = mpsc::unbounded_channel();
    let capped = run(&ask, &on, &mut rx, &CancellationToken::new()).await;
    assert!(!capped.ok);
    assert_eq!(capped.stop, Some(Stop::Down));
    assert_eq!(capped.text, why);
    let call = super::super::ask(ask, on);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!call.done.is_finished(), "a patient over-budget call must wait");
    assert_eq!(DISPATCHED.load(Ordering::SeqCst), 0);
    call.handle.stop();
    let end = tokio::time::timeout(Duration::from_secs(1), call.end()).await.unwrap();
    assert!(!end.ok);
    assert_eq!(end.text, why);
    assert_eq!(DISPATCHED.load(Ordering::SeqCst), 0);
    engine.close();
  }

  fn say(text: &str, cut: bool) -> Say {
    Say {
      text: text.into(),
      imgs: vec![],
      cut,
    }
  }

  /// A message that cuts in cuts the step at once; one that doesn't
  /// waits for the step's end; both are heard, in order.
  #[tokio::test]
  async fn a_message_cuts_in_and_others_wait() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut heard = Heard {
      rx: &mut rx,
      open: true,
      says: Vec::new(),
    };
    let cut = CancellationToken::new();
    tx.send(say("[Scout] report", false)).unwrap();
    tx.send(say("stop, wrong folder", true)).unwrap();
    let step = heard
      .till_cut(
        async {
          cut.cancelled().await;
          "cut short"
        },
        &cut,
      )
      .await;
    assert_eq!(step, "cut short");
    tx.send(say("and one more", false)).unwrap();
    let texts: Vec<String> = heard.all().into_iter().map(|s| s.text).collect();
    assert_eq!(texts, ["[Scout] report", "stop, wrong folder", "and one more"]);

    let calm = CancellationToken::new();
    tx.send(say("[Scout] report", false)).unwrap();
    let step = heard
      .till_cut(
        async {
          tokio::task::yield_now().await;
          "done"
        },
        &calm,
      )
      .await;
    assert_eq!(step, "done");
    assert!(!calm.is_cancelled(), "a report never cuts in");
    drop(tx);
    let step = heard
      .till_cut(
        async {
          tokio::task::yield_now().await;
          "done"
        },
        &calm,
      )
      .await;
    assert_eq!(step, "done", "a closed channel ends no step");
  }

  /// A step a stop or a message cut is marked bare, its text kept, a
  /// tool call it cut mid-stream dropped.
  #[test]
  fn a_cut_step_keeps_what_it_said() {
    let seen = vec![
      Live {
        kind: Kind::Acorn,
        text: "Moving the files to ".into(),
      },
      Live {
        kind: Kind::Tool,
        text: "bash {\"comm".into(),
      },
    ];
    let step = fail("stopped", &seen, Halt::Lost);
    assert_eq!(step.entries.len(), 1);
    assert_eq!(step.entries[0].text, "Moving the files to\n[cut off]");
  }
}
