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
use crate::memory::{self, Memory, Msg, Part, Wipe, Work};
use crate::wire::{Command, Event, Live, Seg};
use crate::{Kind, Served, Spend};
use anyhow::Result;
use parking_lot::{Mutex, MutexGuard};
use std::collections::HashSet;
use std::sync::{Arc, Weak};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub use agents::Agent;
pub(crate) use imports::Batch;
pub use turns::{APP, TIMER};
pub use usage::over_budget;

/// The shortest text a secret is purged by: shorter, it would erase too much.
const SECRET: usize = 8;

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

/// Where the chat's turn is. A turn runs from its first message taken
/// till no message waits: it settles (waits for the summaries before its
/// messages), calls, then settles again.
pub(super) enum Phase {
  /// No turn runs.
  Idle,
  /// Between calls: the summaries before its messages awaited, or its
  /// next step decided.
  Settling,
  /// Its call runs: whether its model is asked (handed all it waits for,
  /// its answer not begun), its tool calls without results.
  Calling { call: Handle, asked: bool, open: usize },
}

/// The chat. phase: where its turn is; halt: what stops the turn (a quit);
/// sent, wait: messages given and waiting; live: what streams; calls: tool
/// calls awaiting their results, in order; spend: what the running turn's
/// requests used; held: steps held while a post logs (None: none); seen:
/// the parts the app was told of; shown: the last view's parts;
/// compactions: the summaries' calls;
/// found: the sessions a look found to import, and the import the turn
/// asked for (`then`), begun as it ends; reply: what a thread's turn
/// answers its asker.
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
  phase: Phase,
  sent: Vec<Span>,
  wait: Vec<Span>,
  halt: CancellationToken,
  live: Vec<Live>,
  /// The end of what streams kept back, not shown yet: it may begin a
  /// secret (`veil`).
  tail: String,
  calls: Vec<(String, String)>,
  eye: Eye,
  batch: Option<Batch>,
  held: Option<Vec<Held>>,
  seen: HashSet<Seg>,
  shown: HashSet<Seg>,
  compactions: Vec<(Handle, Part, Served)>,
  found: Vec<import::Session>,
  then: Option<import::Want>,
  reply: Option<String>,
  /// When a wipe waiting on a held purge is woken (ms; 0: none).
  wake: i64,
  /// Secrets purged by their text, in the order purged (one purged again
  /// is listed again): written `[purged]` in whatever is logged (`push`)
  /// or streams (`veil`) after, and in what a call whose prompt was built
  /// before sends (`Kit::purged`). Kept in memory only, never on disk.
  secrets: Vec<String>,
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

/// What may be shown of a stream as `text` comes: each secret whole in it
/// `[purged]` as `ai::veil` writes it, and its end kept back in `tail`
/// while a secret may yet grow there (one can come in pieces): from where
/// a longer one may begin, and from the start of any whole one that
/// reaches past that, since secrets that overlap are purged as one.
fn veil(secrets: &[String], tail: &mut String, text: &str) -> String {
  tail.push_str(text);
  let most = secrets.iter().map(String::len).max().unwrap_or(0);
  let mut keep = (tail.len().saturating_sub(most)..tail.len())
    .filter(|&k| tail.is_char_boundary(k))
    .find(|&k| {
      let end = &tail[k..];
      secrets.iter().any(|s| s.len() > end.len() && s.starts_with(end))
    })
    .unwrap_or(tail.len());
  for (at, till) in ai::held(tail, secrets).into_iter().rev() {
    if at < keep && keep < till {
      keep = at;
    }
  }
  let mut out: String = tail.drain(..keep).collect();
  ai::veil(&mut out, secrets);
  out
}

impl Engine {
  /// A chat opened (`hub`), its events to `events`.
  fn start(
    dir: &std::path::Path,
    id: String,
    me: Assistant,
    with: Option<Assistant>,
    events: hub::Heard,
  ) -> Result<Arc<Engine>> {
    let mem = Memory::load(dir)?;
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
        phase: Phase::Idle,
        sent: Vec::new(),
        wait: Vec::new(),
        halt: CancellationToken::new(),
        live: Vec::new(),
        tail: String::new(),
        calls: Vec::new(),
        eye: Eye::default(),
        batch: None,
        held: None,
        seen: HashSet::new(),
        shown: HashSet::new(),
        compactions: Vec::new(),
        found: Vec::new(),
        then: None,
        reply: None,
        wake: 0,
        secrets: Vec::new(),
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
      chat.batch = chat.resumed()?;
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
    hub::tapped(&self.id, &ev);
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
      Command::Say { text, images, via } => {
        tokio::spawn(async move { me.say(text, images, via).await });
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
    chat.stop();
    chat.mem.done = true;
    for (call, _, _) in &chat.compactions {
      call.stop();
    }
  }

  /// Once the chat's messages before `end` are built (not an import's):
  /// true; false if `halt` stops the wait first. The chat's own summaries
  /// are never held, so nothing else keeps them from being built.
  pub async fn settle(&self, end: u64, halt: &CancellationToken) -> bool {
    let mut fitted = self.fitted.subscribe();
    loop {
      if halt.is_cancelled() {
        return false;
      }
      if self.lock().mem.first(0, 0) >= end {
        return true;
      }
      tokio::select! {
          _ = fitted.changed() => {}
          () = halt.cancelled() => {}
      }
    }
  }

  /// A post's logging, after the chat's earlier ones, holding its other
  /// logging (`hold`) till it ends, so a long post's pages stay together.
  async fn writing<T>(&self, write: impl AsyncFnOnce() -> T) -> T {
    /// The steps held, run as the logging ends: dropped too, so a post
    /// cut short (its future dropped, or a panic, which runs none) leaves
    /// no later step waiting on it.
    struct Release<'a>(&'a Engine);
    impl Drop for Release<'_> {
      fn drop(&mut self) {
        let mut chat = self.0.lock();
        let held = chat.held.take().unwrap_or_default();
        chat.mem.open = 0;
        if std::thread::panicking() {
          return;
        }
        for step in held {
          step(&mut chat);
        }
      }
    }
    let _turn = self.writes.lock().await;
    self.lock().held = Some(Vec::new());
    let _release = Release(self);
    write().await
  }

  /// The work a quit cut goes on, told so: each agent, and the turn by a
  /// note.
  async fn resume(self: Arc<Self>, work: Work) {
    if work == Work::default() {
      return;
    }
    let cut: Vec<&str> = work
      .turn
      .then_some("your turn")
      .into_iter()
      .chain(work.agents.iter().map(String::as_str))
      .collect();
    let text = format!("Acorn quit, cutting {}; nothing was lost: go on.", cut.join(", "));
    let msg = self.lock().push(None, Kind::Note, &text);
    if work.turn {
      self.lock().take(msg);
    }
    for name in &work.agents {
      self.tell_agent(name, &text).await;
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
    let asks = self
      .with
      .as_ref()
      .map(super::assistant::Assistant::asking)
      .unwrap_or_default();
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

  /// A message purged (`Memory::purge`), or a purge taken back
  /// (`Memory::unpurge`, `back`): a chat's, the summaries' calls cut first
  /// (none stores a line built on the text as it was), then the app shown
  /// the chat anew, and what is pinned if the purge unpinned; or an agent's.
  pub(super) fn purge(&mut self, agent: Option<&str>, id: u64, back: bool) -> String {
    if agent.is_none() {
      for (call, _, _) in &self.compactions {
        call.stop();
      }
    }
    let pins = self.mem.pins.len();
    let said = if back {
      self.mem.unpurge(agent, id)
    } else {
      self.mem.purge(agent, id)
    };
    let said = said.unwrap_or_else(|err| format!("The {} failed: {err}", if back { "unpurge" } else { "purge" }));
    if agent.is_none() {
      self.seen.clear();
      self.show();
      self.pump();
    }
    if self.mem.pins.len() != pins {
      self.tell_state();
    }
    said
  }

  /// A message pinned (`Memory::pin`; no id: the user's new one that a
  /// call was given, not one still waiting), or (`back`) unpinned: saved,
  /// and the app told.
  pub(super) fn pin(&mut self, id: Option<u64>, back: bool) -> Result<String, String> {
    let said = match (back, id) {
      (false, id) => self.mem.pin(id, self.wait.first().map_or(u64::MAX, |m| m.id)),
      (true, Some(id)) => self.mem.unpin(id),
      (true, None) => Err("Give id: the pinned message's.".into()),
    };
    if said.is_ok() {
      self.save();
      self.tell_state();
    }
    said
  }

  /// Every message holding `text` purged, the chat's and the agents', for
  /// good (not to be taken back, as none held before is: `kept_from`),
  /// and from now on `text` written `[purged]` in whatever is logged: a
  /// search's results, this very call, a reply. Answers what it erased.
  pub(super) fn purge_text(&mut self, text: &str) -> String {
    let text = text.trim();
    if text.chars().count() < SECRET {
      return format!("Give text of {SECRET} characters at least: the secret itself.");
    }
    if !self.secrets.iter().any(|s| s == text) {
      self.secrets.push(text.to_string());
    }
    for (call, _, _) in &self.compactions {
      call.stop();
    }
    let (mut erased, mut last) = (Vec::new(), None);
    let pins = self.mem.pins.len();
    while let Some(at) = self.mem.holding(text) {
      if last.as_ref() == Some(&at) {
        break; // not erased: never again
      }
      match self.mem.purge(at.0.as_deref(), at.1) {
        Ok(_) => erased.push(match &at.0 {
          Some(name) => format!("{name}'s {}", at.1),
          None => at.1.to_string(),
        }),
        Err(err) => return format!("The purge failed: {err}"),
      }
      last = Some(at);
    }
    // held for undo, it would come back: never, on disk too, and wiped
    if let Err(err) = self.mem.kept_from(text) {
      return format!("The purge failed: {err}");
    }
    // a summary may have copied it from its context; a fact may keep it
    let lines = match self.mem.sweep(text) {
      Ok(n) => n,
      Err(err) => return format!("The purge failed: {err}"),
    };
    let facts = match crate::facts::purge(&self.me.id, text) {
      Ok(n) => n,
      Err(err) => return format!("The purge failed: {err}"),
    };
    // what streams now may hold it too
    let streams: Vec<Option<String>> = std::iter::once(None)
      .chain(self.agents.keys().cloned().map(Some))
      .collect();
    for agent in streams {
      let was = self.live_of(agent.as_deref()).clone();
      let live = self.redacted(was.clone());
      if live != was {
        self.live_of(agent.as_deref()).clone_from(&live);
        self.all(Event::Live { agent, live });
      }
    }
    self.seen.clear();
    self.show();
    self.pump();
    if self.mem.pins.len() != pins {
      self.tell_state();
    }
    let mut what = match erased.len() {
      0 => "No message held it".to_string(),
      _ => format!("Purged the messages holding it ({})", erased.join(", ")),
    };
    if lines > 0 {
      what += &format!("; {lines} summaries that held it or were made from one are being written anew");
    }
    if facts > 0 {
      what += &format!("; forgot the {facts} facts holding it");
    }
    format!(
      "{what}; from now on it is written [purged] wherever a message would hold it, this call too. Purged as text, they can't be taken back (unpurge); their old bytes are wiped from Acorn's files once the chat is idle."
    )
  }

  /// A text as logged: each secret purged by its text, `[purged]`.
  fn redact(&self, text: &str) -> String {
    let mut text = text.to_string();
    ai::veil(&mut text, &self.secrets);
    text
  }

  /// What streams, as shown: each secret in it `[purged]`.
  fn redacted(&self, mut live: Vec<Live>) -> Vec<Live> {
    if !self.secrets.is_empty() {
      for l in &mut live {
        l.text = self.redact(&l.text);
      }
    }
    live
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
    let made = std::mem::take(&mut self.mem.made);
    self.count_built(&made);
    for part in made {
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
  /// no write, and no purge is held in its GRACE (till then, woken as the
  /// last one's ends), on a thread of its own. One that fails is left
  /// owed, till Acorn starts again.
  fn wipe(&mut self) {
    let mem = &self.mem;
    if mem.wipe != Wipe::Idle
      || mem.wiped == mem.purged
      || self.busy()
      || !mem.busy.is_empty()
      || !self.working().is_empty()
    {
      return;
    }
    let now = crate::now_ms();
    if let Some(till) = self.mem.held_till().filter(|&t| t > now) {
      // Only on the runtime: the wipe's own thread lets the chat go too,
      // and the next drop on a runtime's thread sets the timer then.
      if self.wake != till
        && let Ok(rt) = tokio::runtime::Handle::try_current()
      {
        self.wake = till;
        let this = self.this.clone();
        rt.spawn(async move {
          tokio::time::sleep(std::time::Duration::from_millis((till - now) as u64 + 1)).await;
          if let Some(engine) = this.upgrade() {
            drop(engine.lock());
          }
        });
      }
      return;
    }
    self.mem.wipe = Wipe::Running;
    let (this, dir, upto) = (self.this.clone(), self.mem.dir.clone(), self.mem.purged);
    std::thread::spawn(move || {
      let done = memory::wipe(&dir, now - memory::GRACE);
      let Some(engine) = this.upgrade() else { return };
      let mut chat = engine.lock();
      match done {
        Ok(()) => {
          chat.mem.wipe = Wipe::Idle;
          chat.mem.wiped(upto);
          chat.all(Event::Note("Purge finished: erased from Acorn's files.".into()));
        }
        Err(err) => {
          chat.mem.wipe = Wipe::Stuck;
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
    self.meter_at(kind, None)
  }

  pub(crate) fn meter_at(&self, kind: Served, part: Option<Part>) -> ai::Meter {
    let this = self.this.clone();
    std::sync::Arc::new(move |used: Used| {
      if let Some(engine) = this.upgrade() {
        engine.lock().used(kind, part, used);
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

#[cfg(test)]
mod tests {
  use super::veil;

  pub(super) fn open(dir: &std::path::Path) -> std::sync::Arc<super::Engine> {
    let me = super::Assistant::max();
    let (events, _) = tokio::sync::mpsc::unbounded_channel();
    super::Engine::start(dir, "test".into(), me.clone(), Some(me), events).unwrap()
  }

  /// A stream's pieces as shown, and what is still kept back.
  fn shown(secrets: &[&str], pieces: &[&str]) -> (String, String) {
    let secrets: Vec<String> = secrets.iter().map(std::string::ToString::to_string).collect();
    let mut tail = String::new();
    let out = pieces.iter().map(|p| veil(&secrets, &mut tail, p)).collect();
    (out, tail)
  }

  #[test]
  fn a_secret_streaming_in_pieces_is_never_shown() {
    let key = ["sk-12345678"];
    let purged = ("The key is [purged], kept.".to_string(), String::new());
    assert_eq!(shown(&key, &["The key is sk-12345678, kept."]), purged);
    assert_eq!(shown(&key, &["The key is sk-", "1234", "5678, kept."]), purged);
    let each: Vec<String> = "The key is sk-12345678, kept.".chars().map(String::from).collect();
    let each: Vec<&str> = each.iter().map(String::as_str).collect();
    assert_eq!(shown(&key, &each), purged);
    // nothing of it shows while it comes
    let (out, tail) = shown(&key, &["is sk-1234"]);
    assert_eq!((out.as_str(), tail.as_str()), ("is ", "sk-1234"));
  }

  #[test]
  fn what_only_began_like_a_secret_is_shown_once_it_differs() {
    let key = ["sk-12345678"];
    assert_eq!(shown(&key, &["ask-", "12 more"]).0, "ask-12 more");
    assert_eq!(
      shown(&key, &["sk-1sk-1234", "5678"]),
      ("sk-1[purged]".to_string(), String::new())
    );
    // a secret that ends as it begins, whole: kept back whole, not half,
    // till what follows shows it is not two that overlap
    assert_eq!(
      shown(&["abcd-abcd"], &["abcd-abcd"]),
      (String::new(), "abcd-abcd".to_string())
    );
    assert_eq!(
      shown(&["abcd-abcd"], &["abcd-abcd", ", kept."]),
      ("[purged], kept.".to_string(), String::new())
    );
    assert_eq!(
      shown(&["abcd-abcd"], &["abcd-abcd", "-abcd."]),
      ("[purged].".to_string(), String::new())
    );
    assert_eq!(
      shown(&["12345678", "péché-péché"], &["x péché-p", "éché 1234", "5678é"]),
      ("x [purged] [purged]é".to_string(), String::new())
    );
  }

  /// Each stream as shown, in one piece, in two cut at each byte, and
  /// letter by letter: what `ai::veil` makes of it whole.
  fn shown_as_whole(secrets: &[&str], text: &str, purged: &str) {
    let mut whole = text.to_string();
    crate::ai::veil(
      &mut whole,
      &secrets.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
    );
    assert_eq!(whole, purged);
    let each: Vec<String> = text.chars().map(String::from).collect();
    let each: Vec<&str> = each.iter().map(String::as_str).collect();
    let cuts = (0..=text.len()).filter(|&k| text.is_char_boundary(k));
    let pieces = cuts.map(|k| vec![&text[..k], &text[k..]]).chain([each]);
    for pieces in pieces {
      assert_eq!(
        shown(secrets, &pieces),
        (purged.to_string(), String::new()),
        "{pieces:?}"
      );
    }
  }

  #[test]
  fn secrets_that_overlap_are_purged_as_one_as_they_stream() {
    // one purged, then a longer one holding it
    shown_as_whole(
      &["sk-abc12345", "sk-abc12345678"],
      "key sk-abc12345678 end",
      "key [purged] end",
    );
    // one ending as the next begins
    shown_as_whole(&["AAAABBBB", "BBBBCCCC"], "x AAAABBBBCCCC y", "x [purged] y");
    shown_as_whole(
      &["hunter2-", "XXhunter2-hunter2-long"],
      "XXhunter2-hunter2-long.",
      "[purged].",
    );
    shown_as_whole(&["ab", "ba"], "x abababa y", "x [purged] y");
    // two that only touch are two
    shown_as_whole(&["abc", "def"], "abcdef.", "[purged][purged].");
    shown_as_whole(&["péché", "ché-pé"], "un péché-péché.", "un [purged].");
    // one overlapping itself
    shown_as_whole(&["abcd-abcd"], "abcd-abcd-abcd.", "[purged].");
    shown_as_whole(&["ééé"], "éééé, éé.", "[purged], éé.");
  }
  #[tokio::test]
  async fn a_message_holding_a_secret_stays_purged() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(dir.path());
    let mut chat = engine.lock();
    let msg = chat.push(None, crate::Kind::User, "my code is my-project-code-1234");
    // purged by id first: no message shows it when its text is purged
    let early = chat.push(None, crate::Kind::User, "again: my-project-code-1234");
    chat.purge(None, early.id, false);
    let said = chat.purge_text("my-project-code-1234");
    assert!(said.contains("can't be taken back"), "{said}");
    let said = chat.purge(None, early.id, true);
    assert!(said.contains("stays purged"), "{said}");
    assert_eq!(chat.mem.message(early.id).unwrap().text, crate::memory::PURGED);
    chat.purge_text("my-project-code-1234");
    assert_eq!(chat.secrets.len(), 1);
    let said = chat.purge(None, msg.id, true);
    assert!(said.contains("stays purged"), "{said}");
    assert_eq!(chat.mem.message(msg.id).unwrap().text, crate::memory::PURGED);
    let other = chat.push(None, crate::Kind::User, "my aside");
    chat.purge(None, other.id, false);
    let said = chat.purge(None, other.id, true);
    assert!(said.starts_with("Unpurged"), "{said}");
    assert_eq!(chat.mem.message(other.id).unwrap().text, "my aside");
  }
}
