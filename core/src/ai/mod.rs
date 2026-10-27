//! AI
//! ==
//! One way to call a model: a system prompt and a user message in; deltas
//! (live pieces) and an entry per finished message out (`Event`). A call's
//! `Handle` sends the agent a message at its next chance or stops it; its
//! end holds those never taken (`End::left`). DeepSeek runs in our own
//! loop (`agent`). A patient call (a summary's) asks again, AGAIN after
//! each try nothing answered (the service down or refusing it, as when the
//! API key's credit is gone), till one is answered or we stop it: an outage
//! only delays it, and no summary is made of a failure that isn't its
//! input's (`compactor`). A call that yields (an import's summary, naming
//! its import) sends nothing while one that doesn't waits on its busy
//! model, nor while another of its import's does (`pace`).

pub mod agent;
pub mod cache;
pub mod deepseek;
pub mod pace;

use crate::effort::Effort;
use crate::price::Usage;
use crate::{Block, Img, Kind, Say};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How long a patient call waits to ask again: short enough that summaries
/// go on within a minute of an outage's end, long enough not to flood the
/// service meanwhile.
const AGAIN: Duration = Duration::from_secs(60);

/// A request done: what it ran on, its tokens as the API told them (or,
/// cut short, what it read), its cost (micro-USD) and when it ended (ms).
#[derive(Clone, Copy, Debug)]
pub struct Used {
    pub effort: Effort,
    pub usage: Usage,
    pub micros: u64,
    pub at: i64,
}

/// Told each request a call makes, as it ends.
pub type Meter = Arc<dyn Fn(Used) + Send + Sync>;

/// Told a call's deltas and entries.
pub type On = Arc<dyn Fn(Event) + Send + Sync>;

/// A tool as the model sees it: its name, what it does, its input's JSON
/// Schema.
#[derive(Clone, Debug)]
pub struct Spec {
    pub name: &'static str,
    pub info: String,
    pub schema: Value,
}

/// A tool's answer: its text, whether it failed, its images.
#[derive(Clone, Debug, Default)]
pub struct Out {
    pub text: String,
    pub fail: bool,
    pub imgs: Vec<Img>,
}

impl Out {
    pub fn ok(text: impl Into<String>) -> Out {
        Out { text: text.into(), ..Out::default() }
    }

    pub fn fail(text: impl Into<String>) -> Out {
        Out { text: text.into(), fail: true, ..Out::default() }
    }
}

/// What a call may use: its tools, and running one (a stop cancels it).
pub trait Toolbox: Send + Sync {
    fn specs(&self) -> Vec<Spec>;
    fn call(&self, name: String, input: Value, cancel: CancellationToken) -> BoxFuture<'static, Out>;
}

#[derive(Clone)]
pub struct Ask {
    pub effort: Effort,
    pub system: String,
    pub blocks: Vec<Block>,
    pub imgs: Vec<Img>,
    pub tools: Option<Arc<dyn Toolbox>>,
    pub meter: Option<Meter>,
    pub patient: bool,
    pub yields: Option<String>,
    pub user: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// A live piece of a message; `fresh`: it starts one (a tool call).
    Delta { kind: Kind, text: String, fresh: bool },
    /// A message done; `of`: the tool call it is or answers.
    Entry { kind: Kind, text: String, of: Option<String> },
}

/// Who ended a call that failed: the model (`Fail`: it answered, so the
/// failure is its input's doing; asked again, it fails alike), we (`Lost`:
/// a stop), the service (`Down`), or none (our setup).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Fail,
    Lost,
    Down,
}

#[derive(Clone, Debug, Default)]
pub struct End {
    pub ok: bool,
    pub text: String,
    pub left: Vec<Say>,
    pub note: String,
    pub stop: Option<Stop>,
}

impl End {
    /// A call's end, before it ends: failed, saying `text`.
    pub fn failed(text: impl Into<String>) -> End {
        End { text: text.into(), ..End::default() }
    }
}

/// How to reach a running call: send it a message, or stop it.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Say>,
    cancel: CancellationToken,
}

impl Handle {
    pub fn send(&self, say: Say) {
        let _ = self.tx.send(say);
    }

    pub fn stop(&self) {
        self.cancel.cancel();
    }

    pub fn stopped(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Whether two handles reach one call.
    pub fn same(&self, o: &Handle) -> bool {
        self.tx.same_channel(&o.tx)
    }
}

pub struct Call {
    pub handle: Handle,
    pub done: tokio::task::JoinHandle<End>,
}

impl Call {
    /// Its end (a call that panicked failed).
    pub async fn end(self) -> End {
        self.done.await.unwrap_or_else(|err| End::failed(format!("the call failed: {err}")))
    }
}

/// A call: its tries, one after another (a patient one's).
pub fn ask(ask: Ask, on: On) -> Call {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let handle = Handle { tx, cancel: cancel.clone() };
    let done = tokio::spawn(async move {
        loop {
            let mut end = agent::run(&ask, &on, &mut rx, &cancel).await;
            let wait = ask.patient && !end.ok && !matches!(end.stop, Some(Stop::Fail | Stop::Lost));
            if wait {
                nap(AGAIN, &cancel).await;
            }
            if cancel.is_cancelled() || !wait {
                rx.close();
                while let Ok(say) = rx.try_recv() {
                    end.left.push(say);
                }
                return end;
            }
        }
    });
    Call { handle, done }
}

/// A while, or till cancelled.
pub async fn nap(time: Duration, cancel: &CancellationToken) {
    tokio::select! {
        _ = tokio::time::sleep(time) => {}
        _ = cancel.cancelled() => {}
    }
}
