//! Link
//! ====
//! How the window reaches the assistants' engines. On a Mac the engines
//! run in the same process, on the runtime's threads (`hub`); in a
//! browser they run in `acorn-server`, and the window reaches them over
//! HTTP: a command is a POST, the events come by long polling. Either
//! way the window hears `(chat, Event)`s on one channel and sends
//! `Command`s to a chat by its id.

use acorn_core::assistant::Assistant;
use acorn_core::wire::{Command, Event};
use tokio::sync::mpsc::UnboundedReceiver;

pub use imp::*;

/// What every chat says, named by chat.
pub type Heard = UnboundedReceiver<(String, Event)>;

/// Said (as a note to no chat) when events were lost: the window shows
/// its chat anew.
pub const LOST: &str = "\u{0}lost";

/// The assistant a chat is with (a thread's: its answerer).
pub fn whose(chat: &str) -> &str {
  chat.split_once('/').map_or(chat, |(answers, _)| answers)
}

/// An assistant to show before the list arrives, or when it has none.
pub fn nobody(id: &str) -> Assistant {
  Assistant {
    id: id.to_string(),
    name: "Acorn".into(),
    about: String::new(),
    instructions: String::new(),
    welcome: String::new(),
    effort: acorn_core::effort::Effort::Low,
    archived: false,
  }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
  use super::*;
  use acorn_core::chat::hub;
  use std::sync::LazyLock;

  /// The engines' threads: their calls, tools and files never hold the
  /// window's.
  static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
      .enable_all()
      .thread_name("acorn")
      .build()
      .expect("the engine's runtime")
  });

  /// The assistants known at the start.
  pub fn assistants() -> Vec<Assistant> {
    acorn_core::assistant::all()
  }

  /// Every chat opened, the first shown told all, and their events from
  /// now on (None: someone already hears them).
  pub fn listen(first: String) -> Option<Heard> {
    let (tx, heard) = tokio::sync::mpsc::unbounded_channel();
    if !hub::listen(tx) {
      return None;
    }
    RUNTIME.spawn(async move {
      hub::open_all();
      if let Some(engine) = hub::get(&first) {
        engine.attach();
      }
    });
    Some(heard)
  }

  /// A command to a chat's engine, if it is open.
  pub fn send(chat: &str, cmd: Command) {
    if let Some(engine) = hub::get(chat) {
      let _in = RUNTIME.enter();
      engine.command(cmd);
    }
  }

  /// Whether a chat is still opening (its engine not there yet).
  pub fn opening(chat: &str) -> bool {
    hub::get(chat).is_none()
  }

  /// Where a chat's image is shown from.
  pub fn image(chat: &str, name: &str) -> String {
    format!("file://{}", hub::dir(chat).join("docs/images").join(name).display())
  }
}

#[cfg(target_arch = "wasm32")]
mod imp {
  use super::*;
  use nsh_core::platform::HttpRequest;
  use nsh_core::prelude::{fetch, spawn, timer};
  use serde::Deserialize;
  use std::time::Duration;

  /// None at the start: the server lists them (`listen`).
  pub fn assistants() -> Vec<Assistant> {
    Vec::new()
  }

  /// What `/api/hello` answers: the assistants, and the last event's
  /// number (polling goes on from it).
  #[derive(Deserialize)]
  struct Hello {
    assistants: Vec<Assistant>,
    seq: u64,
  }

  /// What `/api/events` answers: the events after the one asked from, the
  /// last's number, and whether some were lost (the poll came too late:
  /// the chat shown is told anew).
  #[derive(Deserialize)]
  struct Batch {
    seq: u64,
    lost: bool,
    events: Vec<(String, Event)>,
  }

  /// The server's assistants (each heard as `Event::Assistant`, the first
  /// the one talked to), then every chat's events, polled. `first` is
  /// unknown here: the window shows the first assistant listed.
  pub fn listen(_first: String) -> Option<Heard> {
    let (tx, heard) = tokio::sync::mpsc::unbounded_channel();
    spawn(async move {
      let mut seq = loop {
        match get::<Hello>("/api/hello".into()).await {
          Ok(hello) => {
            for a in hello.assistants {
              let _ = tx.send((a.id.clone(), Event::Assistant(a)));
            }
            break hello.seq;
          }
          Err(err) => {
            let _ = tx.send((String::new(), Event::Note(format!("Can't reach the server: {err}"))));
            timer(Duration::from_secs(3)).await;
          }
        }
      };
      loop {
        match get::<Batch>(format!("/api/events?after={seq}")).await {
          Ok(batch) => {
            seq = batch.seq;
            if batch.lost {
              let _ = tx.send((String::new(), Event::Note(LOST.into())));
            }
            for ev in batch.events {
              if tx.send(ev).is_err() {
                return;
              }
            }
          }
          Err(_) => timer(Duration::from_secs(2)).await,
        }
      }
    });
    Some(heard)
  }

  async fn get<T: for<'de> Deserialize<'de>>(url: String) -> Result<T, String> {
    let res = fetch(HttpRequest::get(url)).await?;
    if !res.ok() {
      return Err(format!("{} {}", res.status(), String::from_utf8_lossy(&res.body)));
    }
    serde_json::from_slice(&res.body).map_err(|e| e.to_string())
  }

  /// A command to a chat's engine, on the server.
  pub fn send(chat: &str, cmd: Command) {
    let Ok(body) = serde_json::to_vec(&cmd) else { return };
    let url = format!("/api/cmd?chat={}", escaped(chat));
    spawn(async move {
      let req = HttpRequest::post(url, body).header("content-type", "application/json");
      let _ = fetch(req).await;
    });
  }

  /// The server tells a chat still opening itself (a note).
  pub fn opening(_chat: &str) -> bool {
    false
  }

  /// Where a chat's image is shown from: the server.
  pub fn image(chat: &str, name: &str) -> String {
    format!("/api/image?chat={}&name={}", escaped(chat), escaped(name))
  }

  /// A query value, %XX-escaped.
  fn escaped(s: &str) -> String {
    s.bytes()
      .map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
      })
      .collect()
  }
}
