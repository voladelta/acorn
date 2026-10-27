//! Assistants
//! ==========
//! Who the person chats with, as people: Acorn, the general assistant (the
//! config's chat, at its `pick`); Max, the thoughtful expert (at max
//! effort); then those an assistant created as asked (`create_assistant`),
//! each a name, an about (who it is, one line the app shows), its
//! instructions (its system prompt) and an effort, kept in
//! $ACORN_DIR/assistants.json, oldest first. How hard one thinks is part
//! of who it is, never shown. Each has its own chat (`chats`/<id>) and its
//! own work dir; Acorn's are the config's.

use crate::config;
use crate::effort::Effort;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const FILE: &str = "assistants.json";

/// Held while assistants.json is read and written: each assistant's
/// engine may make one.
static FILING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Max's chat.
pub const MAX: &str = "max";

/// How Max works: the expert one goes to when it matters.
const MAX_WAY: &str = "You are the one the user, and the other assistants, come to when it matters. Think it through before you answer: weigh the alternatives, check your reasoning, say how sure you are and what would change your mind. Answer plainly, the conclusion first.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assistant {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub about: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    pub effort: Effort,
}

impl Assistant {
    pub fn acorn() -> Assistant {
        Assistant { id: config::get().chat.clone(), name: "Acorn".into(), about: "General assistant".into(), instructions: String::new(), effort: config::get().pick }
    }

    pub fn max() -> Assistant {
        Assistant { id: MAX.into(), name: "Max".into(), about: "Thoughtful expert".into(), instructions: MAX_WAY.into(), effort: Effort::Max }
    }

    pub fn is_acorn(&self) -> bool {
        self.id == config::get().chat
    }

    /// Where its tools work: Acorn's, the config's; another's, a dir of
    /// its own in it (no agent's name: those are capitalized words).
    pub fn work(&self) -> PathBuf {
        let work = &config::get().work;
        if self.is_acorn() { work.clone() } else { work.join("assistants").join(&self.id) }
    }

    /// What its system prompt says of it, after Acorn's: nothing for
    /// Acorn; else its name, who it is, and its instructions.
    pub fn personality(&self) -> String {
        if self.is_acorn() {
            return String::new();
        }
        let about = if self.about.trim().is_empty() { String::new() } else { format!(": {}", self.about.trim()) };
        let more = if self.instructions.trim().is_empty() { String::new() } else { format!("\n\n{}", self.instructions.trim()) };
        format!("\n\n# This assistant\n\nIn this chat the user calls you {}{about}.{more}", self.name)
    }

    /// What a thread's prompt says of it as the one asking there: another
    /// of the user's assistants, not the user.
    pub fn asking(&self) -> String {
        let about = if self.about.trim().is_empty() { String::new() } else { format!(" ({})", self.about.trim()) };
        let name = &self.name;
        format!(
            "\n\n# This thread\n\nThis chat is not with the user: it is your thread with {name}{about}, another of the user's assistants, who asks you things for the user's sake. Its user messages are {name}'s, and each reply of yours goes back to {name}, not to the user. Answer as yourself, from your part and what you know, plainly and to the point. The facts below are the user's."
        )
    }
}

/// Every assistant: Acorn, Max, then those made.
pub fn all() -> Vec<Assistant> {
    let mut all = vec![Assistant::acorn()];
    if config::get().chat != MAX {
        all.push(Assistant::max());
    }
    all.extend(saved());
    all
}

fn saved() -> Vec<Assistant> {
    let file = config::get().dir.join(FILE);
    let Ok(text) = std::fs::read_to_string(&file) else { return Vec::new() };
    serde_json::from_str(&text).unwrap_or_else(|err| {
        eprintln!("acorn: {FILE}: {err}");
        Vec::new()
    })
}

/// A new assistant, saved: its id the name's letters and digits, made
/// unique among the assistants and the chats on disk (a chat it didn't
/// start is never taken over).
pub fn add(name: &str, about: &str, instructions: &str, effort: Effort) -> Result<Assistant, String> {
    let _filing = FILING.lock().unwrap_or_else(|e| e.into_inner());
    let name = name.trim();
    if name.is_empty() {
        return Err("An assistant needs a name.".into());
    }
    let about = about.trim();
    if about.is_empty() {
        return Err("An assistant needs an about: who it is, in a few words.".into());
    }
    let all = all();
    if all.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
        return Err(format!("There is already an assistant named {name}."));
    }
    let slug: String = name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect();
    let base = if slug.is_empty() { "assistant".to_string() } else { slug };
    let taken = |id: &str| all.iter().any(|a| a.id == id) || config::get().chats.join(id).exists();
    let id = std::iter::once(base.clone()).chain((2..).map(|n| format!("{base}-{n}"))).find(|id| !taken(id)).unwrap_or(base);
    let made = Assistant { id, name: name.into(), about: about.into(), instructions: instructions.trim().into(), effort };
    let mut mine = saved();
    mine.push(made.clone());
    let dir = &config::get().dir;
    let text = serde_json::to_string_pretty(&mine).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).and_then(|_| std::fs::write(dir.join(FILE), text)).map_err(|e| format!("{FILE}: {e}"))?;
    Ok(made)
}
