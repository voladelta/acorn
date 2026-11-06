//! The DM
//! ======
//! The owner's DM with the bot, as Telegram App's chat: what they write
//! there is said in it (a photo with it, a quote of what they answered),
//! and what the chat says goes back. All the assistant says is sent; what
//! the owner wrote to it in the app, quoted, and the replies to that
//! without a buzz, since they are at their desk; another assistant's
//! answer ("[Max] ...") under its name; a reminder, loud. A turn the
//! owner began by calling on the bot in a group answers there, as a reply
//! to them; its notes still come to the DM. "typing" shows where the turn
//! answers while it runs.

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
  tg.home.say(text, images, VIA.into());
}

/// Where a turn answers, kept from entry to entry: loud, whether it was
/// begun on Telegram or by a timer; room, the group and message it began
/// by (none: the DM).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Route {
  pub loud: bool,
  pub room: Option<(i64, i64)>,
}

/// Where an entry goes: the DM, or a group as a reply to a message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum To {
  Dm,
  Room(i64, i64),
}

/// How a mention in a group is said to come (`Home::say`).
pub fn room_via(chat: i64, msg: i64) -> String {
  format!("{VIA}:{chat}:{msg}")
}

/// The group and message a `room_via` names.
fn room_of(via: &str) -> Option<(i64, i64)> {
  let (chat, msg) = via.strip_prefix(VIA)?.strip_prefix(':')?.split_once(':')?;
  Some((chat.parse().ok()?, msg.parse().ok()?))
}

/// What Telegram gets of a chat's entry: its text, whether it goes
/// without a buzz, and where.
pub fn mirrored(entry: &Entry, route: &mut Route, assistant: impl Fn(&str) -> bool) -> Option<(String, bool, To)> {
  let via = entry.via.as_deref();
  let text = entry.text.trim();
  let to = route.room.map_or(To::Dm, |(chat, msg)| To::Room(chat, msg));
  let quiet = !route.loud;
  match entry.kind {
    Kind::User => {
      match via {
        Some(VIA) => *route = Route { loud: true, room: None },
        Some(APP) => {
          *route = Route {
            loud: false,
            room: None,
          };
          let quote = text.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
          return Some((format!("You, in Acorn:\n{quote}"), true, To::Dm));
        }
        Some(v) if room_of(v).is_some() => {
          *route = Route {
            loud: true,
            room: room_of(v),
          }
        }
        _ => {}
      }
      None
    }
    Kind::Work if via == Some(TIMER) => {
      *route = Route { loud: true, room: None };
      None
    }
    Kind::Work => {
      let (head, said) = text.split_once('\n')?;
      let name = head.strip_prefix('[')?.strip_suffix(']')?;
      assistant(name).then(|| (format!("**{name}:**\n{}", said.trim()), quiet, to))
    }
    Kind::Acorn if via == Some(TIMER) => Some((text.to_string(), false, To::Dm)),
    Kind::Acorn if !text.is_empty() => Some((text.to_string(), quiet, to)),
    Kind::Note if !text.is_empty() => Some((format!("_{text}_"), quiet, To::Dm)),
    _ => None,
  }
}

/// The chat's events, mirrored to Telegram as they come.
pub async fn mirror(tg: Arc<Tg>, mut events: UnboundedReceiver<Event>) {
  let busy = Arc::new(AtomicBool::new(false));
  let route = Arc::new(parking_lot::Mutex::new(Route { loud: true, room: None }));
  tokio::spawn(typing(tg.clone(), busy.clone(), route.clone()));
  let assistant = |name: &str| acorn_core::assistant::find(name).is_some();
  while let Some(ev) = events.recv().await {
    match ev {
      Event::State(state) => busy.store(state.busy, Ordering::Relaxed),
      Event::Entry { agent: None, entry, .. } => {
        let said = mirrored(&entry, &mut route.lock(), assistant);
        let (Some((text, silent, to)), Some(owner)) = (said, tg.owner()) else {
          continue;
        };
        let sent = match to {
          To::Dm => tg.send(owner, &text, silent).await,
          To::Room(chat, msg) => tg.send_to(chat, &text, silent, Some(msg)).await,
        };
        if let Err(why) = sent {
          eprintln!("acorn: telegram: {why}");
        }
      }
      _ => {}
    }
  }
}

/// "typing" where the turn answers, while it runs.
async fn typing(tg: Arc<Tg>, busy: Arc<AtomicBool>, route: Arc<parking_lot::Mutex<Route>>) {
  loop {
    tokio::select! {
        _ = tokio::time::sleep(TYPING) => {}
        _ = tg.stop.cancelled() => return,
    }
    let at = route.lock().room.map(|(chat, _)| chat).or(tg.owner());
    if busy.load(Ordering::Relaxed)
      && let Some(chat) = at
    {
      let _ = tg
        .bot
        .call(
          "sendChatAction",
          &json!({"chat_id": chat, "action": "typing"}),
          &tg.stop,
        )
        .await;
    }
  }
}
