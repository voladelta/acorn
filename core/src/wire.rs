//! Wire
//! ====
//! What the engine and the app say to each other. The app sends a
//! `Command` (a person's act, or a read of the chat); the engine answers
//! with `Event`s, the chat as the agent sees it: the view's parts as
//! blocks, from the turn on each message whole, then what streams. Each
//! message and summary is told (`Event::Know`) before any part that shows
//! it, and again once a part shows it anew (`chat::view`).

use crate::{Kind, Spend};
use serde::{Deserialize, Serialize};

/// The one model every call runs on (DeepSeek Flash; `ai::deepseek`).
pub const MODEL: &str = "deepseek-flash";

/// The n messages from id on (n a power of two).
pub type Seg = (u64, u64);

/// A message logged: its kind, text, date (ISO), images, and, the last of
/// a turn, what the turn's requests used; as it is told, where it came
/// from (`via`: "app", "telegram", "timer"; none for the engine's own),
/// never kept.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
  pub kind: Kind,
  pub text: String,
  pub date: String,
  pub images: Vec<String>,
  pub spend: Option<Spend>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub via: Option<String>,
}

/// A summary: its text, its last message's date.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
  pub text: String,
  pub to: String,
}

/// A piece of a message streaming.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Live {
  pub kind: Kind,
  pub text: String,
}

/// A piece joins the message streaming, if of its kind, unless it starts
/// one (fresh: a tool call, as two calls never join).
pub fn live_push(live: &mut Vec<Live>, kind: Kind, text: &str, fresh: bool) {
  match live.last_mut() {
    Some(top) if top.kind == kind && !fresh => top.text.push_str(text),
    _ => live.push(Live {
      kind,
      text: text.to_string(),
    }),
  }
}

/// An import's progress: its time done and in all (micro-USD of calls),
/// USD spent and left, why paused, its messages, its nodes not built by
/// level.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
  pub done: f64,
  pub total: f64,
  pub spent: f64,
  pub left: f64,
  pub why: Option<String>,
  pub msgs: u64,
  pub open: Vec<u64>,
}

/// What a turn waits on while nothing streams.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Wait {
  None,
  /// The summaries of the messages before its own.
  Summaries,
  /// The model's first piece (a request out, no tool running).
  Model,
}

/// The chat's state: a turn runs; what it waits on; the agents at work;
/// how each pinned message begins, oldest first; an import's progress; what every call used today.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
  pub busy: bool,
  pub wait: Wait,
  pub agents: Vec<String>,
  pub pins: Vec<String>,
  pub importing: Option<Progress>,
  pub today: Option<Spend>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
  /// Messages and summaries a view will show.
  Know {
    whole: Vec<(u64, Entry)>,
    nodes: Vec<(Seg, Node)>,
  },
  /// The view's parts, and where the turn starts (each message from it
  /// on whole).
  View {
    parts: Vec<Seg>,
    turn: u64,
  },
  /// The halves of a part opened.
  Zoom {
    seg: Seg,
    parts: Vec<Seg>,
  },
  /// The tree's roots: the chat, folded.
  Root {
    parts: Vec<Seg>,
  },
  /// A message logged: Acorn's (`id`), an agent's, or a thought (none).
  Entry {
    agent: Option<String>,
    id: Option<u64>,
    entry: Entry,
  },
  Delta {
    agent: Option<String>,
    kind: Kind,
    text: String,
    fresh: bool,
  },
  /// What streams, told anew.
  Live {
    agent: Option<String>,
    live: Vec<Live>,
  },
  State(State),
  /// An agent's log, asked for (None: no such agent).
  Agent {
    name: String,
    log: Option<Vec<Entry>>,
  },
  Note(String),
  /// What a turn used, on its last message.
  Use {
    id: u64,
    spend: Spend,
  },
  /// An assistant made (`create_assistant`), to list and start.
  Assistant(crate::assistant::Assistant),
  /// An assistant archived (`archive_assistant`), by id, to drop.
  Gone(String),
}

/// What the app sends: the person's words, and looks. Every other act is
/// said, and an assistant does it by a tool (`tools`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
  /// A message; one sent while the assistant works cuts in. via: where
  /// it was written (none: the app).
  Say {
    text: String,
    #[serde(with = "base64s")]
    images: Vec<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    via: Option<String>,
  },
  /// The app (again) shows this chat: tell it all anew.
  Attach,
  /// Open a part into its halves.
  Zoom(Seg),
  /// The tree's roots.
  Root,
  /// An agent's log.
  Agent(String),
}

/// Images on the wire as base64, not arrays of numbers.
mod base64s {
  use base64::Engine;
  use base64::engine::general_purpose::STANDARD;
  use serde::{Deserialize, Deserializer, Serialize, Serializer};

  pub fn serialize<S: Serializer>(images: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
    images
      .iter()
      .map(|i| STANDARD.encode(i))
      .collect::<Vec<_>>()
      .serialize(s)
  }

  pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
    Vec::<String>::deserialize(d)?
      .iter()
      .map(|i| STANDARD.decode(i).map_err(serde::de::Error::custom))
      .collect()
  }
}
