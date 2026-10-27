//! Hub
//! ===
//! The chats open, an engine each: every assistant's with the user (named
//! by the assistant's id), and every thread, an assistant's with another
//! who asked it something (`<answers>/<asks>`, kept in the answerer's dir
//! under `with/`). Each opens once, as it is first needed, or at the
//! start if on disk, in a slot of its own: one opening holds up no other
//! chat, and a chat still opening is not there yet (`get`). Their events
//! go to one listener, the app, named by chat.

use super::Engine;
use crate::assistant::{self, Assistant};
use crate::config;
use crate::wire::Event;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, OnceLock};
use tokio::sync::mpsc::UnboundedSender;

/// Where every chat's events go, each named by its chat.
pub type Heard = UnboundedSender<(String, Event)>;

static HEARD: OnceLock<Heard> = OnceLock::new();

/// A chat's slot: its engine, once open.
type Slot = Arc<Mutex<Option<Arc<Engine>>>>;

static OPEN: LazyLock<Mutex<BTreeMap<String, Slot>>> = LazyLock::new(Default::default);

/// Where the chats' events go from now on: set once (false: it was).
pub fn listen(heard: Heard) -> bool {
    HEARD.set(heard).is_ok()
}

/// Every assistant's chat, and every thread on disk, opened (on the
/// runtime): the work a quit cut in any of them goes on. Each one's
/// failure is told as its note.
pub fn open_all() {
    let all = assistant::all();
    for me in &all {
        let _ = chat(me);
        let with = std::fs::read_dir(config::get().chats.join(&me.id).join(WITH));
        let asks = with.into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok());
        for asks in asks.filter_map(|id| all.iter().find(|a| a.id == id)) {
            let _ = thread(me, asks);
        }
    }
}

/// An assistant's chat with the user.
pub fn chat(me: &Assistant) -> Result<Arc<Engine>, String> {
    open(me.id.clone(), me, None)
}

/// The thread where `asks` asks `answers`.
pub fn thread(answers: &Assistant, asks: &Assistant) -> Result<Arc<Engine>, String> {
    open(format!("{}/{}", answers.id, asks.id), answers, Some(asks))
}

/// A chat open (None: not yet, or opening), never waiting on one.
pub fn get(id: &str) -> Option<Arc<Engine>> {
    let slot = OPEN.lock().get(id)?.clone();
    slot.try_lock()?.clone()
}

/// Every chat open.
pub fn all() -> Vec<Arc<Engine>> {
    let slots: Vec<Slot> = OPEN.lock().values().cloned().collect();
    slots.iter().filter_map(|s| s.try_lock()?.clone()).collect()
}

/// Where a chat lives: an assistant's, `chats`/<id>; a thread, in its
/// answerer's, `with`/<asker>.
pub fn dir(id: &str) -> PathBuf {
    let chats = &config::get().chats;
    match id.split_once('/') {
        Some((answers, asks)) => chats.join(answers).join(WITH).join(asks),
        None => chats.join(id),
    }
}

/// The assistant a chat is with (a thread's: its answerer).
pub fn whose(id: &str) -> &str {
    id.split_once('/').map_or(id, |(answers, _)| answers)
}

const WITH: &str = "with";

fn open(id: String, me: &Assistant, with: Option<&Assistant>) -> Result<Arc<Engine>, String> {
    let Some(heard) = HEARD.get() else { return Err("no one listens to the chats".into()) };
    let slot = OPEN.lock().entry(id.clone()).or_default().clone();
    let mut slot = slot.lock();
    if let Some(engine) = &*slot {
        return Ok(engine.clone());
    }
    match Engine::start(id.clone(), me.clone(), with.cloned(), heard.clone()) {
        Ok(engine) => {
            *slot = Some(engine.clone());
            Ok(engine)
        }
        Err(err) => {
            let why = format!("{} can't open its chat: {err:#}", me.name);
            let _ = heard.send((id, Event::Note(why.clone())));
            Err(why)
        }
    }
}
