//! Acorn's engine
//! ==============
//! One chat that never ends per assistant (`chat`, `assistant`), kept forever in a memory tree
//! (`memory`, built in the background by `compactor`), answered by an
//! agent on DeepSeek (`ai`) with tools on this Mac (`tools`, `machine`). The app (the `acorn` crate) only shows it: it sends
//! `Command`s and hears `Event`s (`wire`).
//!
//! A chat is kept in its chat.db (`memory`); what the user asked to keep
//! for good, facts every assistant (or one) sees, in facts.db (`facts`).

#[cfg(feature = "engine")]
pub mod ai;
pub mod assistant;
#[cfg(feature = "engine")]
pub mod bridge;
#[cfg(feature = "engine")]
pub mod chat;
#[cfg(feature = "engine")]
pub mod clip;
#[cfg(feature = "engine")]
pub mod compactor;
#[cfg(feature = "engine")]
pub mod config;
#[cfg(feature = "engine")]
pub mod decide;
#[cfg(feature = "engine")]
pub mod disk;
pub mod effort;
#[cfg(all(test, feature = "engine"))]
mod evals;
#[cfg(feature = "engine")]
pub mod extend;
#[cfg(feature = "engine")]
pub mod facts;
#[cfg(feature = "engine")]
pub mod files;
#[cfg(feature = "engine")]
pub mod import;
#[cfg(feature = "engine")]
pub mod luau;
#[cfg(feature = "engine")]
pub mod machine;
#[cfg(feature = "engine")]
pub mod memory;
pub mod names;
#[cfg(feature = "engine")]
pub mod plugins;
#[cfg(feature = "engine")]
pub mod price;
#[cfg(feature = "engine")]
pub mod skills;
#[cfg(feature = "engine")]
pub mod timers;
#[cfg(feature = "engine")]
pub mod tools;
#[cfg(feature = "engine")]
pub mod web;
pub mod wire;

use serde::{Deserialize, Serialize};

/// A message's kind: the user's, an agent's report ("[Name]\n..."), a
/// thought (shown, never logged), Acorn's reply (logged as "unii", its
/// name before, so the tree reads alike all along), a tool call, its
/// result, a note (from before the chat, or why work ended).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
  User,
  Work,
  Mind,
  #[serde(rename = "unii")]
  Acorn,
  Tool,
  Echo,
  Note,
}

impl Kind {
  pub fn as_str(self) -> &'static str {
    match self {
      Kind::User => "user",
      Kind::Work => "work",
      Kind::Mind => "mind",
      Kind::Acorn => "unii",
      Kind::Tool => "tool",
      Kind::Echo => "echo",
      Kind::Note => "note",
    }
  }
}

/// An image for a model: its mime type, its bytes in base64.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Img {
  #[serde(rename = "type")]
  pub mime: String,
  pub data: String,
}

/// A message for the agent: its text, its images, and whether it cuts in
/// on a call at work (the words of whoever the call works for).
#[derive(Clone, Debug, Default)]
pub struct Say {
  pub text: String,
  pub imgs: Vec<Img>,
  pub cut: bool,
}

/// A text block of a call's message; `mark`: a cache entry ends after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
  pub text: String,
  pub mark: bool,
}

impl Block {
  pub fn new(text: impl Into<String>, mark: bool) -> Block {
    Block {
      text: text.into(),
      mark,
    }
  }
}

/// What a call serves: the chat's turn, a summary (the chat's, an
/// import's), an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Served {
  Chat,
  Compactor,
  Import,
  Agent,
}

impl Served {
  pub fn as_str(self) -> &'static str {
    match self {
      Served::Chat => "chat",
      Served::Compactor => "compactor",
      Served::Import => "import",
      Served::Agent => "agent",
    }
  }
}

/// What requests used, summed: how many, tokens in (new, and read from
/// DeepSeek's cache) and out, USD, when the last ended (ms).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spend {
  pub n: u64,
  pub input: u64,
  pub read: u64,
  pub output: u64,
  pub usd: f64,
  pub at: i64,
}

impl std::ops::Add for Spend {
  type Output = Spend;
  fn add(self, b: Spend) -> Spend {
    Spend {
      n: self.n + b.n,
      input: self.input + b.input,
      read: self.read + b.read,
      output: self.output + b.output,
      usd: self.usd + b.usd,
      at: self.at.max(b.at),
    }
  }
}

/// Milliseconds since 1970.
#[cfg(feature = "engine")]
pub fn now_ms() -> i64 {
  chrono::Utc::now().timestamp_millis()
}
