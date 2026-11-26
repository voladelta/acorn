//! Assistants
//! ==========
//! Who the person chats with, as people: Acorn, the general assistant (the
//! config's chat, at its `pick`); Max, the thoughtful expert (at max
//! effort); then those an assistant created as asked (`create_assistant`),
//! each a name, an about (who it is, one line the app shows), its
//! instructions (its system prompt), its welcome (its chat's first
//! message) and an effort, kept in
//! $ACORN_DIR/assistants.json, oldest first. How hard one thinks is part
//! of who it is, never shown. Each has its own chat (`chats`/<id>) and its
//! own work dir; Acorn's are the config's. One made may be archived as
//! asked (`archive_assistant`): off the sidebar and out of reach, its chats
//! closed, every file kept as it was, till it is restored
//! (`restore_assistant`).

use crate::effort::Effort;
use crate::model::Model;
use serde::{Deserialize, Serialize};

#[cfg(feature = "engine")]
mod store;
#[cfg(feature = "engine")]
pub use store::*;

/// Max's chat.
pub const MAX: &str = "max";

/// How Max works: the expert one goes to when it matters.
const MAX_WAY: &str = "You are the one the user, and the other assistants, come to when it matters. Think it through before you answer: weigh the alternatives, check your reasoning, say how sure you are and what would change your mind. Answer plainly, the conclusion first.";

/// What Max's chat opens with.
const MAX_HELLO: &str = "Hello! This is Max.

I'm the one you come to when it matters: the hard call, the second opinion, the plan you can't afford to get wrong.

I take my time. I'll weigh the alternatives, check my own reasoning, and tell you how sure I am and what would change my mind. **The conclusion comes first.**

The other assistants come to me too, when they're stuck.

Bring me something hard.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assistant {
  pub id: String,
  pub name: String,
  #[serde(default)]
  pub about: String,
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub instructions: String,
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub welcome: String,
  pub effort: Effort,
  #[serde(default)]
  pub model: Model,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub archived: bool,
}

impl Assistant {
  pub fn max() -> Assistant {
    Assistant {
      id: MAX.into(),
      name: "Max".into(),
      about: "Thoughtful expert".into(),
      instructions: MAX_WAY.into(),
      welcome: MAX_HELLO.into(),
      effort: Effort::Max,
      model: Model::Carbon,
      archived: false,
    }
  }

  /// Whether it is the one named (any case).
  pub fn named(&self, name: &str) -> bool {
    self.name.eq_ignore_ascii_case(name)
  }

  /// Its about, if it has one.
  pub(crate) fn brief(&self) -> Option<&str> {
    Some(self.about.trim()).filter(|a| !a.is_empty())
  }

  /// What its chat opens with, and the line its memory keeps of it:
  /// its welcome (one made without one, a plain hello), and that on one
  /// line, cut short, after its name and about.
  pub fn hello(&self) -> (String, String) {
    let name = &self.name;
    let text = match self.welcome.trim() {
      "" => format!(
        "Hello! This is {name}.\n\nOne chat between us, forever, and a memory that grows. Tell me what you need."
      ),
      welcome => welcome.to_string(),
    };
    let flat: String = text
      .split_whitespace()
      .collect::<Vec<_>>()
      .join(" ")
      .chars()
      .take(240)
      .collect();
    (text, format!("unii: hello: {name}, {}: {flat}", self.about.trim()))
  }

  /// What a thread's prompt says of it as the one asking there: another
  /// of the user's assistants, not the user.
  pub fn asking(&self) -> String {
    let about = self.brief().map_or_else(String::new, |a| format!(" ({a})"));
    let name = &self.name;
    format!(
      "\n\n# This thread\n\nThis chat is not with the user: it is your thread with {name}{about}, another of the user's assistants, who asks you things for the user's sake. Its user messages are {name}'s, and each reply of yours goes back to {name}, not to the user. Answer as yourself, from your part and what you know, plainly and to the point. The facts below are the user's."
    )
  }
}
