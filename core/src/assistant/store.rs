//! Where the assistants are kept (assistants.json) and what of each the
//! config decides: Acorn's id, effort and work dir.

use super::{Assistant, MAX};
use crate::config;
use crate::effort::Effort;
use crate::model::Model;
use std::path::PathBuf;

const FILE: &str = "assistants.json";

/// Held while assistants.json is read and written: each assistant's
/// engine may make one.
static FILING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

impl Assistant {
  pub fn acorn() -> Assistant {
    Assistant {
      id: config::get().chat.clone(),
      name: "Acorn".into(),
      about: "General assistant".into(),
      instructions: String::new(),
      welcome: crate::chat::HELLO.into(),
      effort: config::get().pick,
      model: Model::Neon,
      archived: false,
    }
  }

  pub fn is_acorn(&self) -> bool {
    self.id == config::get().chat
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

/// Every assistant, the archived too: Acorn, Max, the bridges' (as
/// Telegram App), then those made.
pub(crate) fn everyone() -> Vec<Assistant> {
  let mut all = vec![Assistant::acorn()];
  if config::get().chat != MAX {
    all.push(Assistant::max());
  }
  for theirs in crate::bridge::all().iter().map(|b| b.assistant()) {
    if !all.iter().any(|a| a.id == theirs.id) {
      all.push(theirs);
    }
  }
  let taken: Vec<String> = all.iter().map(|a| a.id.clone()).collect();
  all.extend(saved().into_iter().filter(|m| !taken.contains(&m.id)));
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
    if a.is_acorn() || a.id == MAX || crate::bridge::of(&a.id).is_some() {
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
    .and_then(|()| std::fs::write(dir.join("assistants.json.tmp"), text))
    .and_then(|()| std::fs::rename(dir.join("assistants.json.tmp"), dir.join(FILE)))
    .map_err(|e| format!("{FILE}: {e}"))
}

/// A new assistant, saved: its id the name's letters and digits, made
/// unique among the assistants and the chats on disk (a chat it didn't
/// start is never taken over).
pub fn add(
  name: &str,
  about: &str,
  instructions: &str,
  welcome: &str,
  effort: Effort,
  model: Model,
) -> Result<Assistant, String> {
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
    model,
    archived: false,
  };
  let mut mine = saved();
  mine.push(made.clone());
  write(&mine)?;
  Ok(made)
}
