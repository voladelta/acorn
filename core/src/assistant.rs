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

use crate::config;
use crate::effort::Effort;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const FILE: &str = "assistants.json";

/// Held while assistants.json is read and written: each assistant's
/// engine may make one.
static FILING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

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
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub archived: bool,
}

impl Assistant {
  pub fn acorn() -> Assistant {
    Assistant {
      id: config::get().chat.clone(),
      name: "Acorn".into(),
      about: "General assistant".into(),
      instructions: String::new(),
      welcome: crate::chat::HELLO.into(),
      effort: config::get().pick,
      archived: false,
    }
  }

  pub fn max() -> Assistant {
    Assistant {
      id: MAX.into(),
      name: "Max".into(),
      about: "Thoughtful expert".into(),
      instructions: MAX_WAY.into(),
      welcome: MAX_HELLO.into(),
      effort: Effort::Max,
      archived: false,
    }
  }

  pub fn is_acorn(&self) -> bool {
    self.id == config::get().chat
  }

  /// Whether it is the one named (any case).
  pub fn named(&self, name: &str) -> bool {
    self.name.eq_ignore_ascii_case(name)
  }

  /// Its about, if it has one.
  fn brief(&self) -> Option<&str> {
    Some(self.about.trim()).filter(|a| !a.is_empty())
  }

  /// Where its tools work: Acorn's, the config's; another's, a dir of
  /// its own in it (no agent's name: those are capitalized words).
  pub fn work(&self) -> PathBuf {
    let work = &config::get().work;
    if self.is_acorn() {
      work.clone()
    } else {
      work.join("assistants").join(&self.id)
    }
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

  /// What its system prompt says of it, after Acorn's: nothing for
  /// Acorn; else its name, who it is, and its instructions.
  pub fn personality(&self) -> String {
    if self.is_acorn() {
      return String::new();
    }
    let about = self.brief().map_or_else(String::new, |a| format!(": {a}"));
    let more = match self.instructions.trim() {
      "" => String::new(),
      more => format!("\n\n{more}"),
    };
    format!(
      "\n\n# This assistant\n\nIn this chat the user calls you {}{about}.{more}",
      self.name
    )
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

/// Every assistant: Acorn, Max, then those made, but the archived.
pub fn all() -> Vec<Assistant> {
  everyone().into_iter().filter(|a| !a.archived).collect()
}

/// The assistant named (any case), but the archived.
pub fn find(name: &str) -> Option<Assistant> {
  all().into_iter().find(|a| a.named(name))
}

/// The assistants archived, oldest first.
pub fn archived() -> Vec<Assistant> {
  saved().into_iter().filter(|a| a.archived).collect()
}

/// Every assistant, the archived too.
fn everyone() -> Vec<Assistant> {
  let mut all = vec![Assistant::acorn()];
  if config::get().chat != MAX {
    all.push(Assistant::max());
  }
  all.extend(saved());
  all
}

fn saved() -> Vec<Assistant> {
  let file = config::get().dir.join(FILE);
  let Ok(text) = std::fs::read_to_string(&file) else {
    return Vec::new();
  };
  serde_json::from_str(&text).unwrap_or_else(|err| {
    eprintln!("acorn: {FILE}: {err}");
    Vec::new()
  })
}

/// An assistant made, by name (any case), archived; never Acorn, Max,
/// or the one asking (`me`, its id).
pub fn archive(name: &str, me: &str) -> Result<Assistant, String> {
  mark(name, true, |a| {
    if a.is_acorn() || a.id == MAX {
      return Err(format!("{} stays: only assistants made can be archived.", a.name));
    }
    if a.id == me {
      return Err("That's you: another assistant can archive you.".into());
    }
    Ok(())
  })
}

/// An assistant archived, by name (any case), back.
pub fn restore(name: &str) -> Result<Assistant, String> {
  mark(name, false, |_| Ok(()))
}

/// The one named (any case) set archived or not, if `may`.
fn mark(name: &str, archived: bool, may: impl Fn(&Assistant) -> Result<(), String>) -> Result<Assistant, String> {
  let _filing = FILING.lock();
  let name = name.trim();
  let Some(it) = everyone().into_iter().find(|a| a.named(name)) else {
    return Err(format!(
      "No assistant {name}; list_assistants names them, the archived too."
    ));
  };
  may(&it)?;
  if it.archived == archived {
    return Err(format!(
      "{} is {}.",
      it.name,
      if archived { "archived already" } else { "not archived" }
    ));
  }
  let mut mine = saved();
  mine
    .iter_mut()
    .filter(|a| a.id == it.id)
    .for_each(|a| a.archived = archived);
  write(&mine)?;
  Ok(Assistant { archived, ..it })
}

fn write(mine: &[Assistant]) -> Result<(), String> {
  let dir = &config::get().dir;
  let text = serde_json::to_string_pretty(mine).map_err(|e| e.to_string())?;
  std::fs::create_dir_all(dir)
    .and_then(|_| std::fs::write(dir.join(FILE), text))
    .map_err(|e| format!("{FILE}: {e}"))
}

/// A new assistant, saved: its id the name's letters and digits, made
/// unique among the assistants and the chats on disk (a chat it didn't
/// start is never taken over).
pub fn add(name: &str, about: &str, instructions: &str, welcome: &str, effort: Effort) -> Result<Assistant, String> {
  let _filing = FILING.lock();
  let name = name.trim();
  if name.is_empty() {
    return Err("An assistant needs a name.".into());
  }
  let about = about.trim();
  if about.is_empty() {
    return Err("An assistant needs an about: who it is, in a few words.".into());
  }
  let all = everyone();
  if let Some(same) = all.iter().find(|a| a.named(name)) {
    let archived = if same.archived {
      " (archived: restore_assistant brings them back)"
    } else {
      ""
    };
    return Err(format!("There is already an assistant named {name}{archived}."));
  }
  let slug: String = name
    .chars()
    .filter(char::is_ascii_alphanumeric)
    .map(|c| c.to_ascii_lowercase())
    .collect();
  let base = if slug.is_empty() { "assistant".to_string() } else { slug };
  let taken = |id: &str| all.iter().any(|a| a.id == id) || config::get().chats.join(id).exists();
  let id = std::iter::once(base.clone())
    .chain((2..).map(|n| format!("{base}-{n}")))
    .find(|id| !taken(id))
    .unwrap_or(base);
  let made = Assistant {
    id,
    name: name.into(),
    about: about.into(),
    instructions: instructions.trim().into(),
    welcome: welcome.trim().into(),
    effort,
    archived: false,
  };
  let mut mine = saved();
  mine.push(made.clone());
  write(&mine)?;
  Ok(made)
}
