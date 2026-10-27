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
//! own work dir; Acorn's are the config's. One made may be deleted as asked
//! (`delete_assistant`): its chat, its threads and its work dir go to the
//! Trash, whence the user may bring them back.

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

/// What Max's chat opens with.
const MAX_HELLO: &str = "Hello! This is Max.

I'm the one you come to when it matters: the hard call, the second opinion, the plan you can't afford to get wrong.

I take my time. I'll weigh the alternatives, check my own reasoning, and tell you how sure I am and what would change my mind. **The conclusion comes first.**

The other assistants come to me too, when they're stuck.

Bring me something hard.";

const MAX_HELLO_LINE: &str = "unii: hello: Max, the thoughtful expert for when it matters; weighs the alternatives, checks its reasoning, says how sure it is; the conclusion first; the other assistants ask it too.";

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
}

impl Assistant {
    pub fn acorn() -> Assistant {
        Assistant { id: config::get().chat.clone(), name: "Acorn".into(), about: "General assistant".into(), instructions: String::new(), welcome: crate::chat::HELLO.into(), effort: config::get().pick }
    }

    pub fn max() -> Assistant {
        Assistant { id: MAX.into(), name: "Max".into(), about: "Thoughtful expert".into(), instructions: MAX_WAY.into(), welcome: MAX_HELLO.into(), effort: Effort::Max }
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

    /// What its chat opens with, and the line its memory keeps of it:
    /// its welcome (one made without one, a plain hello).
    pub fn hello(&self) -> (String, String) {
        if self.is_acorn() {
            return (self.welcome.clone(), crate::chat::HELLO_LINE.into());
        }
        if self.id == MAX {
            return (self.welcome.clone(), MAX_HELLO_LINE.into());
        }
        let name = &self.name;
        let text = match self.welcome.trim() {
            "" => format!("Hello! This is {name}.\n\nOne chat between us, forever, and a memory that grows. Tell me what you need."),
            welcome => welcome.to_string(),
        };
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect();
        (text, format!("unii: hello: {name}, {}: {flat}", self.about.trim()))
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

/// An assistant made, by name (any case), deleted from assistants.json;
/// never Acorn, Max, or the one asking (`me`, its id).
pub fn remove(name: &str, me: &str) -> Result<Assistant, String> {
    let _filing = FILING.lock().unwrap_or_else(|e| e.into_inner());
    let name = name.trim();
    let Some(gone) = all().into_iter().find(|a| a.name.eq_ignore_ascii_case(name)) else {
        return Err(format!("No assistant {name}; list_assistants names them."));
    };
    if gone.is_acorn() || gone.id == MAX {
        return Err(format!("{} stays: only assistants made can be deleted.", gone.name));
    }
    if gone.id == me {
        return Err("That's you: another assistant can delete you.".into());
    }
    let mine: Vec<Assistant> = saved().into_iter().filter(|a| a.id != gone.id).collect();
    write(&mine)?;
    Ok(gone)
}

/// A deleted assistant's files, moved to the Trash in one folder: its
/// chat (with the threads where others asked it), its threads where it
/// asked others, its work dir. The folder.
pub fn trash(gone: &Assistant) -> Result<PathBuf, String> {
    let chats = &config::get().chats;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M.%S");
    let to = config::home().join(".Trash").join(format!("Acorn {} {stamp}", gone.name));
    let mut moves = vec![(chats.join(&gone.id), to.join("chat")), (gone.work(), to.join("work"))];
    for other in std::fs::read_dir(chats).into_iter().flatten().flatten() {
        let thread = other.path().join("with").join(&gone.id);
        moves.push((thread, to.join("asked").join(other.file_name())));
    }
    for (from, to) in moves.into_iter().filter(|(from, _)| from.exists()) {
        let moved = to.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::rename(&from, &to));
        moved.map_err(|e| format!("{} couldn't go to the Trash: {e}", from.display()))?;
    }
    Ok(to)
}

fn write(mine: &[Assistant]) -> Result<(), String> {
    let dir = &config::get().dir;
    let text = serde_json::to_string_pretty(mine).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).and_then(|_| std::fs::write(dir.join(FILE), text)).map_err(|e| format!("{FILE}: {e}"))
}

/// A new assistant, saved: its id the name's letters and digits, made
/// unique among the assistants and the chats on disk (a chat it didn't
/// start is never taken over).
pub fn add(name: &str, about: &str, instructions: &str, welcome: &str, effort: Effort) -> Result<Assistant, String> {
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
    let made = Assistant { id, name: name.into(), about: about.into(), instructions: instructions.trim().into(), welcome: welcome.trim().into(), effort };
    let mut mine = saved();
    mine.push(made.clone());
    write(&mine)?;
    Ok(made)
}
