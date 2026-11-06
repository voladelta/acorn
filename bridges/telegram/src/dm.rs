//! The DM
//! ======
//! The owner's DM with the bot, as Telegram App's chat: what they write
//! there is said in it (a photo with it, a quote of what they answered),
//! and what the chat says goes back. All the assistant says is sent; what
//! the owner wrote to it in the app, quoted, and the replies to that
//! without a buzz, since they are at their desk; another assistant's
//! answer ("[Max] ...") under its name; a reminder, loud. "typing" shows
//! while a turn runs.

use super::{Tg, VIA};
use acorn_core::Kind;
use acorn_core::chat::{APP, TIMER};
use acorn_core::wire::{Entry, Event};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// How often "typing" is said again: Telegram shows it 5 s.
const TYPING: Duration = Duration::from_secs(4);
/// The most characters of an answered message quoted.
const QUOTE: usize = 200;

/// The owner's message, said in the chat.
pub async fn inbound(tg: &Arc<Tg>, msg: &Value, edited: bool) {
  let mut text = msg["text"]
    .as_str()
    .or(msg["caption"].as_str())
    .unwrap_or_default()
    .to_string();
  let mut images = Vec::new();
  let photo = msg["photo"]
    .as_array()
    .and_then(|sizes| sizes.last())
    .map(|p| &p["file_id"]);
  let picture = msg["document"]["mime_type"]
    .as_str()
    .is_some_and(|m| m.starts_with("image/"))
    .then(|| &msg["document"]["file_id"]);
  if let Some(file) = photo.or(picture).and_then(|f| f.as_str()) {
    match tg.bot.download(file, &tg.stop).await {
      Ok(bytes) => images.push(bytes),
      Err(why) => text = format!("{text}\n\n[a picture that didn't come through: {why}]"),
    }
  } else if text.is_empty()
    && let Some(held) = super::rooms::what(msg)
  {
    text = format!("{held} (sent on Telegram; you get its words and pictures only)");
  }
  if text.trim().is_empty() && images.is_empty() {
    return;
  }
  if let Some(re) = msg.get("reply_to_message") {
    let said = super::rooms::what(re).unwrap_or_default();
    let quote: String = said.chars().take(QUOTE).collect();
    let quote = quote.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
    text = format!("{quote}\n\n{text}");
  }
  if edited {
    text = format!("(edited) {text}");
  }
  tg.home.say(text, images);
}

/// What the owner's DM gets of a chat's entry: its text, and whether it
/// goes without a buzz. `loud`: whether the turn was begun on Telegram
/// or by a timer, kept from entry to entry.
pub fn mirrored(entry: &Entry, loud: &mut bool, assistant: impl Fn(&str) -> bool) -> Option<(String, bool)> {
  let via = entry.via.as_deref();
  let text = entry.text.trim();
  match entry.kind {
    Kind::User => match via {
      Some(VIA) => {
        *loud = true;
        None
      }
      Some(APP) => {
        *loud = false;
        let quote = text.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
        Some((format!("You, in Acorn:\n{quote}"), true))
      }
      _ => None,
    },
    Kind::Work if via == Some(TIMER) => {
      *loud = true;
      None
    }
    Kind::Work => {
      let (head, said) = text.split_once('\n')?;
      let name = head.strip_prefix('[')?.strip_suffix(']')?;
      assistant(name).then(|| (format!("**{name}:**\n{}", said.trim()), !*loud))
    }
    Kind::Acorn if via == Some(TIMER) => Some((text.to_string(), false)),
    Kind::Acorn if !text.is_empty() => Some((text.to_string(), !*loud)),
    Kind::Note if !text.is_empty() => Some((format!("_{text}_"), !*loud)),
    _ => None,
  }
}

/// The chat's events, mirrored to the owner's DM as they come.
pub async fn mirror(tg: Arc<Tg>, mut events: UnboundedReceiver<Event>) {
  let busy = Arc::new(AtomicBool::new(false));
  tokio::spawn(typing(tg.clone(), busy.clone()));
  let mut loud = true;
  let assistant = |name: &str| acorn_core::assistant::find(name).is_some();
  while let Some(ev) = events.recv().await {
    match ev {
      Event::State(state) => busy.store(state.busy, Ordering::Relaxed),
      Event::Entry { agent: None, entry, .. } => {
        let (Some((text, silent)), Some(owner)) = (mirrored(&entry, &mut loud, assistant), tg.owner()) else {
          continue;
        };
        if let Err(why) = tg.send(owner, &text, silent).await {
          eprintln!("acorn: telegram: {why}");
        }
      }
      _ => {}
    }
  }
}

/// "typing" in the owner's DM while a turn runs.
async fn typing(tg: Arc<Tg>, busy: Arc<AtomicBool>) {
  loop {
    tokio::select! {
        _ = tokio::time::sleep(TYPING) => {}
        _ = tg.stop.cancelled() => return,
    }
    if busy.load(Ordering::Relaxed)
      && let Some(owner) = tg.owner()
    {
      let _ = tg
        .bot
        .call(
          "sendChatAction",
          &json!({"chat_id": owner, "action": "typing"}),
          &tg.stop,
        )
        .await;
    }
  }
}
