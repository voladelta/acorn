//! Chat
//! ====
//! The agent: one chat that never ends (`memory`), an assistant's with
//! the user, or its thread with another assistant who asked it something
//! (`hub`), and the app that shows it, by `wire`'s commands and events.
//! A thread is a chat like any: the asker is its user, and each reply
//! goes back to the asker's chat as a message "[Name]".
//!
//! One turn at a time. A message is logged as it arrives; the running call
//! takes it if it goes whole beside its view (the user's cutting in on its
//! step), else the next call does, at its assistant's effort. A quit or crash cuts all: a start
//! resumes what ran, told so.
//!
//! The chat's state is one lock (`Engine::lock`), as one thread would hold
//! it: every change is made whole between two awaits. What a change sets
//! off (a summary built, the views fit) runs as the lock is let go
//! (`Guard`), before anyone else takes it.

mod agents;
pub mod hub;
mod imports;
mod turns;
mod usage;
mod view;

use crate::ai::{self, Handle, Used};
use crate::assistant::Assistant;
use crate::compactor;
use crate::import;
use crate::memory::{self, Memory, Msg, Part};
use crate::wire::{Command, Event, Live, Seg, TURN};
use crate::{Kind, Served, Spend};
use anyhow::Result;
use parking_lot::{Mutex, MutexGuard};
use std::collections::HashSet;
use std::sync::{Arc, Weak};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub use agents::Agent;
pub(crate) use imports::Batch;
pub use usage::over_budget;

/// The first message: a new chat opens with it, its node written with it,
/// so no compactor call summarizes it.
pub const HELLO: &str = "Hello! This is Acorn.

One chat between us, and it never ends.

Every message stays. Old days fold into short notes, recent ones stay sharp, and when the details matter I dig them back up. Squirrels forget where they buried half their acorns. I don't.

Three ways to start:

- **Bring your past.** Say \"import my chats\" and I'll move your old Claude and ChatGPT history in.
- **Hand me a job.** Plan the trip, untangle the spreadsheet, write the email you've been dodging. Big jobs I split between helpers who work side by side.
- **Tell me what's true.** \"I'm vegetarian.\" \"Mom's birthday is May 3.\" I keep facts like these for good.

For a hard call, Max is in the sidebar.

Every oak was once a nut that held its ground. So: day one. What's on your mind?";

pub const HELLO_LINE: &str = "unii: hello: Acorn, one endless chat; every message kept, old days folded into notes, unfolded when needed; to start: import past Claude and ChatGPT chats, hand it a job (helpers in parallel), tell it facts to keep; Max in the sidebar for hard calls.";

/// A message logged: its pages [id, end).
#[derive(Clone, Copy, Debug)]
pub struct Span {
  pub id: u64,
  pub end: u64,
}

/// What the agent saw at its last call: the view before message `from`,
/// then every message from it on.
#[derive(Clone, Debug, Default)]
pub struct Eye {
  pub parts: Vec<Seg>,
  pub from: u64,
}

/// A step that logs to the chat, held while an import logs (`hold`).
type Held = Box<dyn FnOnce(&mut Chat) + Send>;

/// The engine: its chat's id, the chat, its lock, and where its events
/// go.
pub struct Engine {
  id: String,
  chat: Mutex<Chat>,
  events: hub::Heard,
  /// Told each time the views are fit (`settle` waits on it).
  fitted: watch::Sender<u64>,
  /// The posts logging, one after another (`writing`).
  writes: tokio::sync::Mutex<()>,
  /// The after-turn commands, one after another.
  after: tokio::sync::Mutex<()>,
}

/// The chat. busy: a turn runs; call: the turn's call (its model asked:
/// `asked`; tool calls without results: `open`); check: the turn's goal
/// check; sent, wait: messages given and waiting; halt: what stops the
/// turn (and why: `halted`); live: what streams; calls: tool calls
/// awaiting their results, in order; spend: what the running turn's
/// requests used; held: steps held while a post logs (None: none);
/// seen: the parts the app was told of; compactions: the summaries'
/// calls; found: the sessions a look found to import, and the import
/// the turn asked for (`then`), begun as it ends; reply: what a thread's
/// turn answers its asker; cut_in: the user cut in on the turn's call
/// (its goal waits for them).
pub struct Chat {
  this: Weak<Engine>,
  pub id: String,
  /// Whose chat it is: its effort, its prompt, its work dir.
  pub me: Assistant,
  /// In a thread, who asks: its user.
  pub with: Option<Assistant>,
  spend: Spend,
  pub mem: Memory,
  agents: std::collections::BTreeMap<String, Agent>,
  busy: bool,
  call: Option<Handle>,
  check: Option<Handle>,
  asked: bool,
  open: usize,
  sent: Vec<Span>,
  wait: Vec<Span>,
  halt: CancellationToken,
  halted: Option<String>,
  live: Vec<Live>,
  calls: Vec<(String, String)>,
  eye: Eye,
  batch: Option<Batch>,
  held: Option<Vec<Held>>,
  seen: HashSet<Seg>,
  compactions: Vec<(Handle, Part, Served)>,
  found: Vec<import::Session>,
  then: Option<import::Want>,
  reply: Option<String>,
  cut_in: bool,
}

/// The chat, held: as it is let go, what its changes set off runs.
pub struct Guard<'a>(MutexGuard<'a, Chat>);

impl std::ops::Deref for Guard<'_> {
  type Target = Chat;
  fn deref(&self) -> &Chat {
    &self.0
  }
}

impl std::ops::DerefMut for Guard<'_> {
  fn deref_mut(&mut self) -> &mut Chat {
    &mut self.0
  }
}

impl Drop for Guard<'_> {
  fn drop(&mut self) {
    self.0.settle();
  }
}

impl Engine {
  /// A chat opened (`hub`), its events to `events`.
  fn start(id: String, me: Assistant, with: Option<Assistant>, events: hub::Heard) -> Result<Arc<Engine>> {
    let dir = hub::dir(&id);
    std::fs::create_dir_all(&dir)?;
    let mem = Memory::load(&dir)?;
    usage::tables(&mem.db)?;
    let work = mem.work.clone();
    let engine = Arc::new_cyclic(|this| Engine {
      id: id.clone(),
      chat: Mutex::new(Chat {
        this: this.clone(),
        id,
        me,
        with,
        spend: Spend::default(),
        mem,
        agents: Default::default(),
        busy: false,
        call: None,
        check: None,
        asked: false,
        open: 0,
        sent: Vec::new(),
        wait: Vec::new(),
        halt: CancellationToken::new(),
        halted: None,
        live: Vec::new(),
        calls: Vec::new(),
        eye: Eye::default(),
        batch: None,
        held: None,
        seen: HashSet::new(),
        compactions: Vec::new(),
        found: Vec::new(),
        then: None,
        reply: None,
        cut_in: false,
      }),
      events,
      fitted: watch::channel(0).0,
      writes: tokio::sync::Mutex::new(()),
      after: tokio::sync::Mutex::new(()),
    });
    {
      let mut chat = engine.lock();
      if chat.mem.count == 0 && chat.with.is_none() {
        let (hello, line) = chat.me.hello();
        let hello = Msg {
          line: Some(line),
          ..Msg::new(Kind::Acorn, &hello)
        };
        chat.mem.push(vec![hello])?;
      }
      let turn = chat.mem.turn();
      chat.eye = chat.eye_from(turn);
      chat.batch = chat.resumed();
      chat.pump();
      chat.progress();
    }
    let resumed = engine.clone();
    tokio::spawn(async move { resumed.resume(work).await });
    Ok(engine)
  }

  /// The chat, held.
  pub fn lock(&self) -> Guard<'_> {
    Guard(self.chat.lock())
  }

  fn tell(&self, ev: Event) {
    let _ = self.events.send((self.id.clone(), ev));
  }

  /// The app (again) shows the chat: told it as it is now.
  pub fn attach(&self) {
    let mut chat = self.lock();
    chat.seen.clear();
    chat.tell_state();
    chat.show();
    chat.all(Event::Live {
      agent: None,
      live: chat.live.clone(),
    });
  }

  /// A person's act, or a read.
  pub fn command(self: &Arc<Self>, cmd: Command) {
    let me = self.clone();
    match cmd {
      Command::Say { text, images } => {
        tokio::spawn(async move { me.say(text, images).await });
      }
      Command::Attach => self.attach(),
      Command::Zoom(seg) => self.lock().zoom(seg),
      Command::Root => self.lock().root(),
      Command::Agent(name) => self.lock().agent_page(&name),
    }
  }

  /// As Acorn quits: all work stops, and what it ran is left to resume.
  pub fn quit(&self) {
    self.close();
    crate::machine::end();
  }

  /// Its work stopped: its turn, its agents, its summaries.
  pub fn close(&self) {
    let mut chat = self.lock();
    chat.stop(None);
    chat.mem.done = true;
    for (call, _, _) in &chat.compactions {
      call.stop();
    }
  }

  /// Once the chat's messages before `end` are built (not an import's):
  /// None; else why they won't be ("Stopped.": halt), so no turn or agent
  /// waits on summaries no call is building.
  pub async fn settle(&self, end: u64, halt: &CancellationToken) -> Option<String> {
    let mut fitted = self.fitted.subscribe();
    loop {
      {
        let chat = self.lock();
        let built = chat.mem.first(0, 0) >= end;
        let why = if halt.is_cancelled() {
          Some("Stopped.".to_string())
        } else if built {
          None
        } else {
          chat.mem.hold[0].clone()
        };
        if built || why.is_some() {
          return why;
        }
      }
      tokio::select! {
          _ = fitted.changed() => {}
          _ = halt.cancelled() => {}
      }
    }
  }

  /// A post's logging, after the chat's earlier ones, holding its other
  /// logging (`hold`) till it ends, so a long post's pages stay together.
  async fn writing<T>(&self, write: impl AsyncFnOnce() -> T) -> T {
    let _turn = self.writes.lock().await;
    self.lock().held = Some(Vec::new());
    let out = write().await;
    let mut chat = self.lock();
    let held = chat.held.take().unwrap_or_default();
    chat.mem.open = 0;
    for step in held {
      step(&mut chat);
    }
    out
  }

  /// The work a quit cut goes on, told so: each agent, and the turn by a
  /// note.
  async fn resume(self: Arc<Self>, work: Vec<String>) {
    if work.is_empty() {
      return;
    }
    let text = format!("Acorn quit, cutting {}; nothing was lost: go on.", work.join(", "));
    let msg = self.lock().push(None, Kind::Note, &text);
    for w in work {
      // "Acorn's turn": as a chat saved it before
      if w == TURN || w == "Acorn's turn" {
        self.lock().take(msg);
      } else {
        let text = text.clone();
        self.tell_agent(&w, &text).await;
      }
    }
  }
}

impl Chat {
  fn engine(&self) -> Arc<Engine> {
    self.this.upgrade().expect("the engine outlives its chat")
  }

  /// What every call of the chat starts with, a summary's too: its
  /// system prompt (its assistant's personality after Acorn's, a
  /// thread's asker, then the facts it sees), its assistant's effort.
  pub fn share(&self) -> ai::Ask {
    let asks = self.with.as_ref().map(|a| a.asking()).unwrap_or_default();
    let head = crate::tools::master(&self.me.work(), self.with.is_some())
      + &self.me.personality()
      + &asks
      + &crate::facts::prompt(&self.me.id);
    ai::Ask {
      effort: self.me.effort,
      system: crate::tools::system(&head),
      blocks: Vec::new(),
      imgs: Vec::new(),
      tools: None,
      meter: None,
      patient: false,
      yields: None,
      user: Some(self.id.clone()),
    }
  }

  /// A message purged (`Memory::purge`): a chat's, the summaries' calls
  /// cut first (none stores a line its words went into), then the app
  /// shown the chat anew; or an agent's.
  pub(super) fn purge(&mut self, agent: Option<&str>, id: u64) -> String {
    if agent.is_none() {
      for (call, _, _) in &self.compactions {
        call.stop();
      }
    }
    let said = self
      .mem
      .purge(agent, id)
      .unwrap_or_else(|err| format!("The purge failed: {err}"));
    if agent.is_none() {
      self.seen.clear();
      self.show();
      self.pump();
    }
    said
  }

  /// An event to the app.
  fn all(&self, ev: Event) {
    if let Some(engine) = self.this.upgrade() {
      engine.tell(ev);
    }
  }

  /// What a held change set off: each summary built told (an import
  /// counts its own), and, as the views were fit, its waiters woken and
  /// an import's progress told.
  fn settle(&mut self) {
    for part in std::mem::take(&mut self.mem.made) {
      self.made(part);
    }
    if std::mem::take(&mut self.mem.fitted) {
      if let Some(engine) = self.this.upgrade() {
        engine.fitted.send_modify(|n| *n += 1);
      }
      self.progress();
    }
    self.wipe();
  }

  /// A wipe owed (`Memory::purge`) started once the chat is idle (no
  /// turn, agent or summary at work), so its rewrite of chat.db holds up
  /// no write, on a thread of its own. One that fails is left owed, till
  /// Acorn starts again.
  fn wipe(&mut self) {
    let mem = &self.mem;
    if mem.wiping || mem.wiped == mem.purged || self.busy || !mem.busy.is_empty() || !self.working().is_empty() {
      return;
    }
    self.mem.wiping = true;
    let (this, dir, upto) = (self.this.clone(), self.mem.dir.clone(), self.mem.purged);
    std::thread::spawn(move || {
      let done = memory::wipe(&dir);
      let Some(engine) = this.upgrade() else { return };
      let mut chat = engine.lock();
      match done {
        Ok(()) => {
          chat.mem.wiping = false;
          chat.mem.wiped(upto);
          chat.all(Event::Note("Purge finished: erased from Acorn's files.".into()));
        }
        Err(err) => {
          chat.mem.stuck = true;
          eprintln!("acorn: wipe: {err}");
          chat.all(Event::Note(format!(
            "A purged message's old bytes are still in chat.db ({err}); Acorn wipes them when it starts again."
          )));
        }
      }
    });
  }

  /// A step that logs to the chat: now, or once the post logging ends.
  fn hold(&mut self, step: impl FnOnce(&mut Chat) + Send + 'static) {
    match &mut self.held {
      Some(held) => held.push(Box::new(step)),
      None => step(self),
    }
  }

  /// The compactor's next nodes started (`compactor`).
  pub fn pump(&mut self) {
    compactor::pump(self);
  }

  /// The meter of a kind of call.
  pub fn meter(&self, kind: Served) -> ai::Meter {
    let this = self.this.clone();
    std::sync::Arc::new(move |used: Used| {
      if let Some(engine) = this.upgrade() {
        engine.lock().used(kind, used);
      }
    })
  }

  pub(crate) fn compactions(&mut self) -> &mut Vec<(Handle, Part, Served)> {
    &mut self.compactions
  }

  pub(crate) fn this(&self) -> Weak<Engine> {
    self.this.clone()
  }
}
