//! Agent
//! =====
//! DeepSeek in Acorn's own loop. Each step sends every item and streams
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
//! results are dropped (`shed`), a half at a time. A stop ends the call at
//! once, its request cut. What each request used goes to its meter; one
//! cut short (a stop's, a message's, a stream's end) tells no usage, so
//! what it sent and read is counted (`used`).

use super::deepseek::{Got, Halt, IDLE, Item, MAX_OUTPUT_TOKENS, MODEL, Step, URL};
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
const LEAST: u64 = 32_000;

/// What a tool result dropped to fit the model's context reads.
const SHED: &str = "[dropped: this call outgrew the model's context; run it again if still needed]";

/// What a tool call a message cut in on answers: cut while it ran (after
/// what it did), or never run.
const CUT: &str = "[cut off: a message came in while it ran]";
const UNRUN: &str = "not run: a message came in first";

pub static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
  reqwest::Client::builder()
    .user_agent("Acorn/0.1")
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
  let parts = ask
    .blocks
    .iter()
    .filter(|b| !b.text.is_empty())
    .map(|b| b.text.clone())
    .collect();
  let mut items = vec![Item::User {
    parts,
    imgs: ask.imgs.clone(),
  }];
  let mut room = MAX_OUTPUT_TOKENS;
  while !cancel.is_cancelled() {
    let cut = cancel.child_token();
    let body = super::deepseek::body(ask, &items, &tools, room);
    let step = heard.till_cut(step(ask, on, key, body, &cut), &cut).await;
    if step.halt == Halt::Fail
      && let Some(left) = super::deepseek::left(&step.why)
    {
      // each pass sends less: less room, or fewer results
      if left >= LEAST && left < room {
        room = left;
        continue;
      }
      if shed(&mut items) {
        room = MAX_OUTPUT_TOKENS;
        on(Event::Entry {
          kind: Kind::Mind,
          text: "This call outgrew the model's context: its older tool results were dropped.".into(),
          of: None,
        });
        continue;
      }
    }
    for e in &step.entries {
      on(Event::Entry {
        kind: e.kind,
        text: e.text.clone(),
        of: e.of.clone(),
      });
    }
    if cancel.is_cancelled() {
      return Ok(());
    }
    let mut backs = Vec::new();
    // cut before it gave a whole output (one that raced the cut runs
    // no tools: each answers it wasn't run)
    if cut.is_cancelled() && step.halt == Halt::Lost {
      let said: Vec<&str> = step
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Acorn)
        .map(|e| e.text.as_str())
        .collect();
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
        end.text = step.why.clone();
      }
      end.stop = Some(stop);
      return Ok(());
    } else {
      items.push(Item::Turn(step.out));
      for u in step.uses {
        let out = if cut.is_cancelled() {
          Out::fail(UNRUN)
        } else {
          let out = heard.till_cut(tool(ask, &tools, u.name, u.input, &cut), &cut).await;
          if cut.is_cancelled() {
            Out {
              text: format!("{}\n{CUT}", out.text),
              ..out
            }
          } else {
            out
          }
        };
        if cancel.is_cancelled() {
          return Ok(());
        }
        let out = if out.fail {
          Out {
            text: format!("Error: {}", out.text),
            ..out
          }
        } else {
          out
        };
        on(Event::Entry {
          kind: Kind::Echo,
          text: out.text.clone(),
          of: Some(u.id.clone()),
        });
        backs.push((u.id, out));
      }
    }
    let says = heard.all();
    if backs.is_empty() && says.is_empty() {
      end.ok = true;
      end.text = step
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Acorn)
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
      return Ok(());
    }
    for say in &says {
      on(Event::Entry {
        kind: Kind::User,
        text: say.text.clone(),
        of: None,
      });
    }
    if !backs.is_empty() {
      items.push(Item::Back(backs));
    }
    if !says.is_empty() {
      let parts = says
        .iter()
        .filter(|s| !s.text.is_empty())
        .map(|s| s.text.clone())
        .collect();
      let imgs = says.into_iter().flat_map(|s| s.imgs).collect();
      items.push(Item::User { parts, imgs });
    }
  }
  Ok(())
}

/// The older half (by size) of the tool results a call still holds,
/// dropped; whether any was.
fn shed(items: &mut [Item]) -> bool {
  let size = |out: &Out| out.text.len() + out.imgs.iter().map(|i| i.data.len()).sum::<usize>();
  let held = |out: &&mut Out| out.text != SHED || !out.imgs.is_empty();
  let mut outs: Vec<&mut Out> = items
    .iter_mut()
    .filter_map(|item| match item {
      Item::Back(backs) => Some(backs.iter_mut().map(|(_, out)| out)),
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
  let own = ask
    .yields
    .as_ref()
    .map_or_else(|| MODEL.to_string(), |import| format!("{MODEL} {import}"));
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
      (Some(_), true) => vec![MODEL],
      (Some(_), false) => vec![MODEL, &own],
    };
    pace::yielded(&waits, cancel).await;
    let Some(Sent { after, step }) = request(ask, on, key, &body, cancel).await else {
      break fail("stopped", &[], Halt::Lost);
    };
    if step.halt != Halt::Busy || k >= TRIES {
      break if step.halt == Halt::Busy {
        eprintln!("{MODEL}: {}", clip::head(&step.why, 300));
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
    if asking.is_none() && own != MODEL && pace::held(&own) {
      continue;
    }
    if asking.is_none() {
      asking = Some(pace::hold(MODEL, &own, &step.why));
    }
    let wait = after
      .filter(|&s| s.is_finite() && s > 0.0)
      .unwrap_or_else(|| 2f64.powi(k as i32 - 1))
      .min(300.0);
    on(Event::Entry {
      kind: Kind::Mind,
      text: format!("The AI service is busy: asking again in {wait} s."),
      of: None,
    });
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
      _ = cancel.cancelled() => return None,
  };
  let seat = pace::fly(MODEL, ask.yields.is_some(), cancel).await?;
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
    .bearer_auth(key)
    .header("content-type", "application/json")
    .body(json.clone())
    .send();
  let res = tokio::select! {
      res = tokio::time::timeout(IDLE, post) => Some(res),
      _ = cancel.cancelled() => None,
  };
  let (after, step, ok) = match res {
    None => (None, fail("stopped", &[], Halt::Lost), false),
    Some(Err(_)) => (
      None,
      fail(
        &format!("no word from the API in {} s", IDLE.as_secs()),
        &[],
        Halt::Busy,
      ),
      false,
    ),
    Some(Ok(Err(err))) => (None, fail(&err.to_string(), &[], Halt::Busy), false),
    Some(Ok(Ok(res))) => {
      status = res.status().as_u16();
      let header = |name: &str| {
        res
          .headers()
          .get(name)
          .and_then(|v| v.to_str().ok())
          .map(str::to_string)
      };
      let after = header("retry-after").and_then(|v| retry_after(&v));
      if res.status().is_success() {
        let step = stream(res, on, flight, &seen, cancel, &mut got, &mut read).await;
        (after, step, true)
      } else {
        let id = header("x-request-id")
          .or_else(|| header("x-deepseek-request-id"))
          .map_or(String::new(), |id| format!(" [{id}]"));
        let should = header("x-should-retry");
        let busy = [408, 409, 429].contains(&status) || status >= 500;
        let passing = should.as_deref() == Some("true") || (should.as_deref() != Some("false") && busy);
        // its why may stall as a stream does: a stop cuts it too
        let told = tokio::select! {
            told = tokio::time::timeout(IDLE, res.text()) => Some(told),
            _ = cancel.cancelled() => None,
        };
        let halt = match status {
          _ if passing => Halt::Busy,
          400 | 413 | 422 => Halt::Fail,
          _ => Halt::Down,
        };
        match told {
          None => (None, fail("stopped", &[], Halt::Lost), false),
          Some(told) => {
            let why = reason(&told.ok().and_then(Result::ok).unwrap_or_default());
            if halt != Halt::Busy {
              eprintln!("{MODEL}: HTTP {status}{id}: {}", clip::head(&why, 300));
            }
            let why = match halt {
              Halt::Busy => format!("HTTP {status}{id}: {why}"),
              _ => format!("the AI service refused the request (HTTP {status}): {why}"),
            };
            (after, fail(&why, &[], halt), false)
          }
        }
      }
    }
  };
  flight.end();
  if ok && let Some(meter) = &ask.meter {
    let usage = used(got.usage, &read, json.len(), flight.cached);
    let now = chrono::Utc::now();
    meter(Used {
      effort: ask.effort,
      usage,
      micros: price::cost(usage, now),
      at: now.timestamp_millis(),
    });
  }
  (status, Sent { after, step })
}

/// What a request's stream read: the bytes of its output, and whether it
/// ended (its last event read, its usage all told).
#[derive(Default)]
struct Read {
  bytes: u64,
  done: bool,
}

/// Where requests go: DeepSeek's API (a test's: its own).
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
        _ = cancel.cancelled() => return fail("stopped", &seen.lock(), Halt::Lost),
        chunk = tokio::time::timeout(IDLE, bytes.next()) => chunk,
    };
    let chunk = match chunk {
      Err(_) => {
        return fail(
          &format!("no word from the API in {} s", IDLE.as_secs()),
          &seen.lock(),
          Halt::Busy,
        );
      }
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
        read.done = true;
        let step = got.step();
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
fn used(usage: price::Usage, read: &Read, sent: usize, cached: usize) -> price::Usage {
  if read.done {
    return usage;
  }
  let tokens = |bytes: usize| (bytes as u64).div_ceil(BYTES);
  let cached = cached.min(sent);
  price::Usage {
    input: 0,
    write: tokens(sent - cached),
    read: tokens(cached),
    output: tokens(read.bytes as usize),
  }
}

/// The bytes of text a token is counted for where the API tells no usage:
/// by DeepSeek's own rule of thumb, a token is three English characters or
/// more (other scripts take more bytes a token).
const BYTES: u64 = 3;

/// A failed request's why: the API's message, else the start of its body.
fn reason(body: &str) -> String {
  serde_json::from_str::<Value>(body)
    .ok()
    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
    .unwrap_or_else(|| clip::head(body, 2000).to_string())
}

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
    .map(|l| super::deepseek::Entry {
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

  const OVER: &str = "This model's maximum context length is 1048576 tokens. However, you requested 1100000 tokens (716000 in the messages, 384000 in the completion). Please reduce the length of the messages or completion.";

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

  fn ask(meter: Option<crate::ai::Meter>) -> Ask {
    Ask {
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
    let body = super::super::deepseek::body(ask, &[], &[], MAX_OUTPUT_TOKENS);
    let mut flight = cache::wait(ask).await;
    let (status, sent) = API.scope(url, send(ask, on, "key", &body, &mut flight, cancel)).await;
    (status, sent.step)
  }

  /// A request a stop cuts as it streams ends at once, what it said kept,
  /// and is metered by what it sent and read (the API told no usage).
  #[tokio::test]
  async fn a_request_cut_is_metered() {
    let url = api(|_| {
      let stream = events(&[
        serde_json::json!({"type": "response.output_item.added", "item": {"type": "message"}}),
        serde_json::json!({"type": "response.output_text.delta", "delta": "Moving the files"}),
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
    let json = super::super::deepseek::body(&ask, &[], &[], MAX_OUTPUT_TOKENS).to_string();
    let usage = price::Usage {
      input: 0,
      write: (json.len() as u64).div_ceil(BYTES),
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
    let read = Read {
      bytes: 300,
      done: false,
    };
    let usage = used(price::Usage::default(), &read, 9000, 6000);
    let told = price::Usage {
      input: 0,
      write: 1000,
      read: 2000,
      output: 100,
    };
    assert_eq!(usage, told);
    let done = Read { bytes: 300, done: true };
    assert_eq!(used(told, &done, 9, 6), told, "the API's own usage wins");
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
        "Authentication Fails, Your api key is invalid",
        Halt::Down,
        "the AI service refused the request (HTTP 401): Authentication Fails, Your api key is invalid",
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
    assert_eq!(super::super::deepseek::left(&step.why), Some(1_048_576 - 716_000));
    assert_eq!(super::super::deepseek::left("Content Exists Risk"), None);
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
        let room = req["max_output_tokens"].as_u64().unwrap();
        rooms.lock().push(room);
        if room == MAX_OUTPUT_TOKENS {
          return (400, refusal(OVER));
        }
        let done = serde_json::json!({"type": "response.completed", "response": {
          "status": "completed",
          "output": [{"type": "message", "content": [{"type": "output_text", "text": "made it"}]}],
          "usage": {"input_tokens": 716_000, "output_tokens": 2},
        }});
        (200, events(&[done]))
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
    assert_eq!(*rooms.lock(), [MAX_OUTPUT_TOKENS, 1_048_576 - 716_000]);

    let tight = OVER.replace("716000 in the messages", "1040000 in the messages");
    let url = api(move |_| (400, refusal(&tight))).await;
    let (_tx, mut rx) = mpsc::unbounded_channel();
    let end = API.scope(url, run(&ask, &on, &mut rx, &cancel)).await;
    assert_eq!((end.ok, end.stop), (false, Some(Stop::Fail)));
    assert!(
      end.text.contains("maximum context length is 1048576 tokens"),
      "{}",
      end.text
    );
  }

  /// Shedding drops the older half of a call's tool results, by size, and
  /// then the rest; nothing left to drop, it tells so.
  #[test]
  fn a_call_sheds_its_older_results() {
    let back = |id: &str, size: usize| (id.to_string(), Out::ok("x".repeat(size)));
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
        Item::Back(backs) => Some(backs.iter().map(|(_, out)| out.text == SHED)),
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
