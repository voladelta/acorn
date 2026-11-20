//! Link
//! ====
//! How the window reaches the assistants' engines. On the desktop (macOS,
//! Linux) the engines run in the same process, on the runtime's threads
//! (`hub`); in a browser they run in `acorn-server`, and the window
//! reaches them over HTTP: a command is a POST, the events come by long
//! polling. Either way the window is told everything on one channel
//! (`Told`), and sends `Command`s to a chat by its id: each chat's in
//! order, each once its chat is open; one that doesn't get there comes
//! back (`Told::Unsent`).
//!
//! In a browser the server is reached, or not since a request failed
//! (why), till one gets through; events lost (too old, or another run of
//! the server's) are told after the assistants anew, as what was lost of
//! them a roster replaces.

use acorn_core::assistant::Assistant;
use acorn_core::wire::{Command, Event};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub use imp::*;

/// What the window is told.
pub enum Told {
  /// The assistants, all: in a browser, at first and anew once events
  /// were lost.
  #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code, reason = "told in a browser alone"))]
  Roster(Vec<Assistant>),
  /// A chat's event.
  Heard(String, Event),
  /// Events were lost: the chat shown is to be told anew.
  #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code, reason = "told in a browser alone"))]
  Lost,
  /// Why the server can't be reached; None: it is again.
  #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code, reason = "told in a browser alone"))]
  Reach(Option<String>),
  /// A command that may not have got to its chat.
  Unsent(String, Command),
}

/// Everything the window is told.
pub type Heard = UnboundedReceiver<Told>;

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
  use std::collections::HashMap;
  use std::sync::{LazyLock, Mutex, OnceLock};

  /// The engines' threads: their calls, tools and files never hold the
  /// window's.
  static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
      .enable_all()
      .thread_name("acorn")
      .build()
      .expect("the engine's runtime")
  });

  /// Where the window is told.
  static TOLD: OnceLock<UnboundedSender<Told>> = OnceLock::new();

  /// Each chat's commands, waiting their turn.
  static QUEUES: LazyLock<Mutex<HashMap<String, UnboundedSender<Command>>>> = LazyLock::new(Default::default);

  /// The bridges this app has (`acorn_core::bridge`), added once,
  /// before any assistant is listed or chat opened.
  fn bridges() {
    static ADDED: std::sync::Once = std::sync::Once::new();
    ADDED.call_once(|| {
      acorn_telegram::add();
    });
  }

  /// The assistants known at the start.
  pub fn assistants() -> Vec<Assistant> {
    bridges();
    acorn_core::assistant::all()
  }

  /// Every chat opened, and all told from now on (None: someone already
  /// hears them).
  pub fn listen() -> Option<Heard> {
    bridges();
    let (tx, mut chats) = unbounded_channel();
    if !hub::listen(tx) {
      return None;
    }
    let (told, heard) = unbounded_channel();
    let _ = TOLD.set(told.clone());
    RUNTIME.spawn(async move {
      while let Some((chat, ev)) = chats.recv().await {
        if told.send(Told::Heard(chat, ev)).is_err() {
          return;
        }
      }
    });
    RUNTIME.spawn_blocking(hub::open_all);
    Some(heard)
  }

  /// A command to a chat's engine, after those sent it before, once it
  /// is open.
  pub fn send(chat: &str, cmd: Command) {
    let mut queues = QUEUES.lock().unwrap();
    let queue = queues
      .entry(chat.to_string())
      .or_insert_with(|| commands(chat.to_string()));
    let _ = queue.send(cmd);
  }

  /// A chat's queue: each command to its engine, waited for while it
  /// opens; with no engine (archived, or a thread never opened), back.
  fn commands(chat: String) -> UnboundedSender<Command> {
    let (tx, mut rx) = unbounded_channel::<Command>();
    RUNTIME.spawn(async move {
      while let Some(cmd) = rx.recv().await {
        let engine = match hub::get(&chat) {
          Some(engine) => Some(engine),
          None => {
            let id = chat.clone();
            tokio::task::spawn_blocking(move || hub::ready(&id))
              .await
              .ok()
              .flatten()
          }
        };
        match engine {
          Some(engine) => engine.command(cmd),
          None => {
            if let Some(told) = TOLD.get() {
              let _ = told.send(Told::Unsent(chat.clone(), cmd));
            }
          }
        }
      }
    });
    tx
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
  use std::cell::RefCell;
  use std::time::Duration;

  /// How long after a failed request the next.
  const RETRY: Duration = Duration::from_secs(2);

  thread_local! {
    /// Where the window is told.
    static TOLD: RefCell<Option<UnboundedSender<Told>>> = const { RefCell::new(None) };
    /// Why the server can't be reached (None: it can).
    static DOWN: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The commands, waiting their turn, each with its chat.
    static QUEUE: RefCell<Option<UnboundedSender<(String, Command)>>> = const { RefCell::new(None) };
  }

  /// None at the start: the server lists them (`listen`).
  pub fn assistants() -> Vec<Assistant> {
    Vec::new()
  }

  /// What `/api/hello` answers: the assistants, the server's run, and the
  /// last event's number (polling goes on from it).
  #[derive(Deserialize)]
  struct Hello {
    assistants: Vec<Assistant>,
    epoch: u64,
    seq: u64,
  }

  /// What `/api/events` answers: the server's run, the events after the
  /// one asked from, the last's number, and whether some were lost (the
  /// poll came too late, or the server started again).
  #[derive(Deserialize)]
  struct Batch {
    epoch: u64,
    seq: u64,
    lost: bool,
    events: Vec<(String, Event)>,
  }

  fn tell(told: Told) {
    TOLD.with(|t| {
      if let Some(t) = &*t.borrow() {
        let _ = t.send(told);
      }
    });
  }

  /// The server reached (None), or why not: told as it changes.
  fn reach(why: Option<String>) {
    let changed = DOWN.with(|d| d.replace(why.clone()) != why);
    if changed {
      tell(Told::Reach(why));
    }
  }

  /// The server's assistants, then every chat's events, polled. Lost
  /// events are polled on from where the batch ends, after the assistants
  /// anew: those told twice are told the same.
  pub fn listen() -> Option<Heard> {
    let (told, heard) = unbounded_channel();
    TOLD.with(|t| *t.borrow_mut() = Some(told));
    let (queue, commands) = unbounded_channel();
    QUEUE.with(|q| *q.borrow_mut() = Some(queue));
    spawn(posts(commands));
    spawn(async move {
      let mut at: Option<(u64, u64)> = None;
      let mut lost = false;
      loop {
        let got = match at {
          Some((epoch, seq)) if !lost => get::<Batch>(format!("/api/events?epoch={epoch}&after={seq}"))
            .await
            .map(|batch| {
              at = Some((batch.epoch, batch.seq));
              reach(None);
              for (chat, ev) in batch.events {
                tell(Told::Heard(chat, ev));
              }
              lost = batch.lost;
            }),
          _ => get::<Hello>("/api/hello".into()).await.map(|hello| {
            at.get_or_insert((hello.epoch, hello.seq));
            reach(None);
            tell(Told::Roster(hello.assistants));
            if std::mem::take(&mut lost) {
              tell(Told::Lost);
            }
          }),
        };
        if let Err(why) = got {
          reach(Some(why));
          timer(RETRY).await;
        }
      }
    });
    Some(heard)
  }

  async fn get<T: for<'de> Deserialize<'de>>(url: String) -> Result<T, String> {
    let res = fetch(HttpRequest::get(url)).await?;
    if !res.ok() {
      return Err(
        format!("{} {}", res.status(), String::from_utf8_lossy(&res.body))
          .trim()
          .to_string(),
      );
    }
    serde_json::from_slice(&res.body).map_err(|e| e.to_string())
  }

  /// A command to a chat's engine, on the server, after those sent before
  /// it.
  pub fn send(chat: &str, cmd: Command) {
    let unsent = QUEUE.with(|q| match &*q.borrow() {
      Some(queue) => queue.send((chat.to_string(), cmd)).err().map(|e| e.0),
      None => Some((chat.to_string(), cmd)),
    });
    if let Some((chat, cmd)) = unsent {
      tell(Told::Unsent(chat, cmd));
    }
  }

  /// The commands posted, one at a time; one that fails, back.
  async fn posts(mut commands: UnboundedReceiver<(String, Command)>) {
    while let Some((chat, cmd)) = commands.recv().await {
      let (got, down) = post(&chat, &cmd).await;
      reach(down);
      if !got {
        tell(Told::Unsent(chat, cmd));
      }
    }
  }

  /// Whether a command got there, and why the server can't be reached:
  /// answered, it is; failing on its side (5xx), not.
  async fn post(chat: &str, cmd: &Command) -> (bool, Option<String>) {
    let Ok(body) = serde_json::to_vec(cmd) else {
      return (false, None);
    };
    let url = format!("/api/cmd?chat={}", escaped(chat));
    let req = HttpRequest::post(url, body).header("content-type", "application/json");
    match fetch(req).await {
      Ok(res) if res.ok() => (true, None),
      Ok(res) => {
        let why = format!("{} {}", res.status(), String::from_utf8_lossy(&res.body));
        (false, (res.status() >= 500).then(|| why.trim().to_string()))
      }
      Err(why) => (false, Some(why)),
    }
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
