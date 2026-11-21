//! Turns
//! =====
//! A message is logged as it comes and taken: by the running call if it
//! goes whole beside its view, else by the next. The user's cuts in on the
//! call at work (`ai::agent`); an agent's report or a note waits for its
//! step to end. A
//! turn runs its calls till no message waits; each call takes the latest
//! messages whole, waiting only for summaries before them (`settle`). A goal
//! (`goal`) is checked as a turn would end, and its nudge keeps the
//! turn going till it is met, unless the user cut in meanwhile: then the
//! assistant answers them and waits. A thread's turn ends with its reply
//! (every call's, or why there was none) sent to its asker; one that
//! asked for an import begins it.

use super::{Chat, Engine, Phase, Span, veil};
use crate::ai::{self, Event as AiEvent};
use crate::memory::{self, Msg};
use crate::wire::{Event, Live, live_push};
use crate::{Block, Kind, Served, config};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Where a message came from, as told (`wire::Entry`).
pub const APP: &str = "app";
pub const TIMER: &str = "timer";

/// A turn's head, as a summary's is "Compaction:": one prompt serves both.
const HEAD: &str = "Turn: the user's new message:\n\n";

pub(super) fn latest(mem: &memory::Memory, from: u64, end: u64) -> crate::Say {
  let mut say = crate::Say::default();
  for id in from..end {
    if let Some(root) = mem.message(id) {
      if say.text.is_empty() {
        say
          .text
          .push_str("The chat's latest messages, whole, not summarized yet:\n\n");
      }
      say.text.push_str(&format!("{id}+0|{}\n", root.show()));
      say.imgs.extend(mem.images(id));
    }
  }
  say
}

impl Engine {
  /// A message from the person (its images and other media first put in
  /// place, each but an image named in its text by its path, since models
  /// see images alone), logged and taken, told as come from `via` (none:
  /// the app).
  pub(super) async fn say(self: Arc<Self>, mut text: String, images: Vec<Vec<u8>>, via: Option<String>) {
    let (dir, at) = {
      let chat = self.lock();
      (chat.mem.dir.clone(), chat.mem.images_dir())
    };
    let mut names = Vec::new();
    for bytes in images {
      let dir = dir.clone();
      match tokio::task::spawn_blocking(move || memory::media_add(&dir, &bytes)).await {
        Ok(Ok(name)) => {
          if !memory::is_image(&name) {
            text.push_str(&format!("\n[attached: {}]", at.join(&name).display()));
          }
          names.push(name)
        }
        _ => return self.tell(Event::Note("That file couldn't be kept.".into())),
      }
    }
    let text = text.trim_start().to_string();
    let me = self.clone();
    self
      .writing(async move || {
        let mut chat = me.lock();
        let msg = Msg {
          images: names,
          ..Msg::new(Kind::User, text)
        };
        let span = chat.push_via(msg, Some(via.as_deref().unwrap_or(APP)));
        chat.take(span);
      })
      .await;
  }

  /// A reminder come due (`timers`): the assistant's own message, said
  /// as it is, no call made.
  pub fn remind(self: &Arc<Self>, text: String) {
    let me = self.clone();
    tokio::spawn(async move {
      me.writing(async || {
        me.lock().push_via(Msg::new(Kind::Acorn, text), Some(TIMER));
      })
      .await
    });
  }

  /// A note in the chat, from outside it (a bridge's: paired, refused).
  pub fn notice(self: &Arc<Self>, text: String) {
    let me = self.clone();
    tokio::spawn(async move {
      me.writing(async || {
        me.lock().push(None, Kind::Note, &text);
      })
      .await
    });
  }

  /// A task come due (`timers`): a message from outside the chat, as an
  /// agent's report is, that starts a turn, never cutting in.
  pub fn wake(self: &Arc<Self>, text: String) {
    let me = self.clone();
    tokio::spawn(async move {
      me.writing(async || {
        let mut chat = me.lock();
        let span = chat.push_via(Msg::new(Kind::Work, text), Some(TIMER));
        chat.take(span);
        chat.change();
      })
      .await
    });
  }

  /// Calls while messages wait; then a thread's reply goes to its asker,
  /// and an import asked for begins.
  async fn turn(self: Arc<Self>) {
    let (reply, then) = loop {
      {
        // whether it ends, and its end, under one lock: a message taken
        // meanwhile either waits here or begins a turn of its own
        let mut chat = self.lock();
        if chat.halt.is_cancelled() || chat.wait.is_empty() {
          break chat.end_turn();
        }
      }
      self.clone().call().await;
    };
    if let Some((text, asks)) = reply {
      let name = self.lock().me.name.clone();
      match super::hub::chat(&asks) {
        Ok(asker) => asker.lock().heard(&format!("[{name}]\n{text}")),
        Err(why) => eprintln!("acorn: {name}'s reply to {}: {why}", asks.name),
      }
    }
    if let Some(want) = then {
      self.import(want).await;
    }
  }

  /// A call of the turn, once the summaries before its whole tail are
  /// built.
  async fn call(self: Arc<Self>) {
    let halt = {
      let mut chat = self.lock();
      chat.cut_in = false;
      chat.change();
      chat.halt.clone()
    };
    let mut from = None;
    loop {
      let now = self.lock().from();
      if from == Some(now) {
        break;
      }
      from = Some(now);
      if !self.settle(now, &halt).await {
        return;
      }
    }
    let from = from.unwrap();
    let threads = self.threads();
    let call = {
      let mut chat = self.lock();
      let msgs = std::mem::take(&mut chat.wait);
      let says: Vec<crate::Say> = msgs
        .iter()
        .filter(|m| m.end > from)
        .map(|m| chat.mem.say(m.id.max(from), m.end))
        .collect();
      let text = says.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n\n");
      let mut latest = latest(&chat.mem, from, msgs.first().map_or(chat.mem.count, |m| m.id));
      latest.imgs.extend(says.into_iter().flat_map(|s| s.imgs));
      chat.eye = chat.eye_from(from);
      chat.show();
      let mut blocks = chat.mem.view_blocks(from);
      blocks.push(chat.at_work());
      blocks.extend(threads);
      if !latest.text.is_empty() {
        blocks.push(Block::new(latest.text, true));
      }
      blocks.push(Block::new(format!("{HEAD}{text}"), true));
      let ask = ai::Ask {
        blocks,
        imgs: latest.imgs,
        tools: Some(chat.kit(None)),
        meter: Some(chat.meter(Served::Chat)),
        ..chat.share()
      };
      let call = ai::ask(ask, chat.on_events(None));
      chat.phase = Phase::Calling {
        call: call.handle.clone(),
        asked: true,
        open: 0,
      };
      chat.change();
      call
    };
    self.ended(call, halt).await;
  }

  /// The turn's call ends: its goal, if it has one and the call went
  /// whole, is checked.
  async fn ended(self: Arc<Self>, call: ai::Call, halt: CancellationToken) {
    let end = call.end().await;
    let check = {
      let mut chat = self.lock();
      chat.phase = Phase::Settling;
      chat.unanswered(None);
      chat.set_live(None, Vec::new());
      let taken = chat.sent.len().saturating_sub(end.left.len());
      let sent = std::mem::take(&mut chat.sent);
      chat.wait.splice(0..0, sent.into_iter().skip(taken));
      if !end.ok && !halt.is_cancelled() {
        chat.failed(&end.text);
      }
      if !halt.is_cancelled() {
        chat.answer(&if end.ok {
          end.text.clone()
        } else {
          format!("(no answer: {})", end.text)
        });
      }
      if !end.note.is_empty() {
        chat.all(Event::Note(end.note.clone()));
      }
      let check = if end.ok && !halt.is_cancelled() && chat.wait.is_empty() && !chat.cut_in {
        chat.check_goal()
      } else {
        None
      };
      chat.change();
      check
    };
    if let Some((check, goal)) = check {
      self.checked(check, goal, halt).await;
    }
  }
}

impl Chat {
  /// A message logged: to the running call if whole beside its view (the
  /// user's cutting in), else to the next.
  pub(super) fn take(&mut self, msg: Span) {
    if self.wait.is_empty()
      && let Phase::Calling { call, .. } = &self.phase
      && self.mem.whole(msg.id) == msg.id
    {
      let cut = self.mem.message(msg.id).is_some_and(|m| m.kind == Kind::User);
      if call.send(crate::Say {
        cut,
        ..self.mem.say(msg.id, msg.end)
      }) {
        self.sent.push(msg);
        self.cut_in |= cut;
        return;
      }
    }
    self.wait.push(msg);
    match &self.phase {
      Phase::Idle => {
        self.phase = Phase::Settling;
        tokio::spawn(self.engine().turn());
      }
      Phase::Checking(check) => check.stop(),
      Phase::Settling | Phase::Calling { .. } => {}
    }
  }

  /// A turn runs.
  pub(super) fn busy(&self) -> bool {
    !matches!(self.phase, Phase::Idle)
  }

  /// The turn ended: its state cleared and told; what it leaves, a
  /// thread's reply to its asker and an import asked for.
  fn end_turn(
    &mut self,
  ) -> (
    Option<(String, crate::assistant::Assistant)>,
    Option<crate::import::Want>,
  ) {
    self.phase = Phase::Idle;
    self.sent.clear();
    self.wait.clear();
    self.set_live(None, Vec::new());
    self.spent();
    self.after();
    self.change();
    let reply = self.reply.take().filter(|_| !self.mem.done).zip(self.with.clone());
    (reply, self.then.take())
  }

  /// Where a call's messages go whole from (a message meanwhile may move
  /// it).
  pub(super) fn from(&self) -> u64 {
    let first = self.wait.first().map_or(self.mem.count, |m| m.id);
    self.mem.whole(first.min(self.mem.first(0, 0)))
  }

  /// A thread's call's answer, joined to the turn's reply (none outside
  /// a thread).
  fn answer(&mut self, text: &str) {
    if self.with.is_none() || text.trim().is_empty() {
      return;
    }
    let reply = self.reply.get_or_insert_default();
    if !reply.is_empty() {
      reply.push_str("\n\n");
    }
    reply.push_str(text.trim());
  }

  /// A failed turn's why, as a note.
  pub(super) fn failed(&mut self, why: &str) {
    self.note(format!(
      "No reply: {}. Send a message to try again.",
      why.trim_end_matches('.')
    ));
  }

  /// A note in the chat, why work ended as it did.
  pub(super) fn note(&mut self, text: impl Into<String>) {
    let text = text.into();
    self.hold(move |chat| {
      chat.push(None, Kind::Note, &text);
    });
  }

  /// To the chat (in pages), or an agent's log; each told to the app.
  pub(super) fn push(&mut self, agent: Option<&str>, kind: Kind, text: &str) -> Span {
    let text = &self.redact(text);
    match agent {
      Some(name) => {
        let id = self.mem.agent_push(name, kind, text).unwrap_or_else(|err| {
          eprintln!("agent {name}: {err}");
          0
        });
        let entry = crate::wire::Entry {
          kind,
          text: text.into(),
          date: memory::iso_now(),
          images: vec![],
          spend: None,
          via: None,
        };
        self.all(Event::Entry {
          agent: Some(name.into()),
          id: Some(id),
          entry,
        });
        Span { id, end: id + 1 }
      }
      None => self.push_msg(Msg::new(kind, text)),
    }
  }

  pub(super) fn push_msg(&mut self, msg: Msg) -> Span {
    self.push_via(msg, None)
  }

  /// To the chat, told as come from `via` (`wire::Entry`).
  pub(super) fn push_via(&mut self, mut msg: Msg, via: Option<&str>) -> Span {
    msg.text = self.redact(&msg.text);
    let roots = match self.mem.push(vec![msg]) {
      Ok(roots) => roots,
      Err(err) => {
        eprintln!("acorn: a message was not logged: {err}");
        return Span {
          id: self.mem.count,
          end: self.mem.count,
        };
      }
    };
    for r in &roots {
      let entry = crate::wire::Entry {
        kind: r.kind,
        text: r.text.clone(),
        date: r.date.clone(),
        images: r.images.clone(),
        spend: None,
        via: via.map(str::to_string),
      };
      self.all(Event::Entry {
        agent: None,
        id: Some(r.i),
        entry,
      });
    }
    self.pump();
    Span {
      id: roots[0].i,
      end: self.mem.count,
    }
  }

  /// A call's events, told as they come to the chat or an agent.
  pub(super) fn on_events(&self, agent: Option<String>) -> ai::On {
    let this = self.this.clone();
    Arc::new(move |ev| {
      if let Some(engine) = this.upgrade() {
        engine.lock().on(agent.as_deref(), ev);
      }
    })
  }

  /// A message the call took was logged as it came; a tool call waits
  /// for its result, logged with it.
  fn on(&mut self, agent: Option<&str>, ev: AiEvent) {
    if agent.is_none() {
      self.asks(&ev);
    }
    match ev {
      AiEvent::Delta { kind, mut text, fresh } => {
        if !self.secrets.is_empty() || !self.tail_of(agent).is_empty() {
          let mut tail = std::mem::take(self.tail_of(agent));
          let last = self.live_of(agent).last().map(|l| l.kind);
          if let Some(last) = last.filter(|&last| (fresh || last != kind) && !tail.is_empty()) {
            // what was kept back of the part before grew no further
            let text = self.redact(&std::mem::take(&mut tail));
            live_push(self.live_of(agent), last, &text, false);
            self.all(Event::Delta {
              agent: agent.map(str::to_string),
              kind: last,
              text,
              fresh: false,
            });
          }
          text = veil(&self.secrets, &mut tail, &text);
          *self.tail_of(agent) = tail;
        }
        live_push(self.live_of(agent), kind, &text, fresh);
        self.all(Event::Delta {
          agent: agent.map(str::to_string),
          kind,
          text,
          fresh,
        });
        return;
      }
      AiEvent::Entry {
        kind: Kind::Tool,
        text,
        of: Some(of),
      } => {
        let calls = self.calls_of(agent);
        calls.retain(|(id, _)| *id != of);
        calls.push((of, text));
      }
      AiEvent::Entry {
        kind: Kind::Mind, text, ..
      } => {
        let entry = crate::wire::Entry {
          kind: Kind::Mind,
          text: self.redact(&text),
          date: memory::iso_now(),
          images: vec![],
          spend: None,
          via: None,
        };
        self.all(Event::Entry {
          agent: agent.map(str::to_string),
          id: None,
          entry,
        });
      }
      AiEvent::Entry { kind: Kind::User, .. } => {}
      AiEvent::Entry { kind, text, of } => {
        let calls = self.calls_of(agent);
        let call = of.and_then(|of| calls.iter().position(|(id, _)| *id == of).map(|k| calls.remove(k).1));
        let agent = agent.map(str::to_string);
        self.hold(move |chat| {
          if let Some(call) = call {
            chat.push(agent.as_deref(), Kind::Tool, &call);
          }
          chat.push(agent.as_deref(), kind, &text);
        });
      }
    }
    let waiting = self.waiting(agent);
    self.set_live(agent, waiting);
  }

  pub(super) fn live_of(&mut self, agent: Option<&str>) -> &mut Vec<Live> {
    match agent.and_then(|name| self.agents.get_mut(name)) {
      Some(a) => &mut a.live,
      None => &mut self.live,
    }
  }

  fn tail_of(&mut self, agent: Option<&str>) -> &mut String {
    match agent.and_then(|name| self.agents.get_mut(name)) {
      Some(a) => &mut a.tail,
      None => &mut self.tail,
    }
  }

  fn calls_of(&mut self, agent: Option<&str>) -> &mut Vec<(String, String)> {
    match agent.and_then(|name| self.agents.get_mut(name)) {
      Some(a) => &mut a.calls,
      None => &mut self.calls,
    }
  }

  /// What streams once an entry came: the tool calls awaiting their
  /// results, as they will be logged.
  fn waiting(&mut self, agent: Option<&str>) -> Vec<Live> {
    self
      .calls_of(agent)
      .iter()
      .map(|(_, text)| Live {
        kind: Kind::Tool,
        text: text.clone(),
      })
      .collect()
  }

  /// A call's tool calls left without a result (it was stopped), logged
  /// as they are, once it ends.
  pub(super) fn unanswered(&mut self, agent: Option<&str>) {
    let calls = std::mem::take(self.calls_of(agent));
    let name = agent.map(str::to_string);
    self.hold(move |chat| {
      for (_, call) in calls {
        chat.push(name.as_deref(), Kind::Tool, &call);
      }
    });
    self.set_live(agent, Vec::new());
  }

  /// The model is asked again once handed all it waits for, until its
  /// answer starts.
  fn asks(&mut self, ev: &AiEvent) {
    let Phase::Calling { asked, open, .. } = &mut self.phase else {
      return;
    };
    let kind = match ev {
      AiEvent::Entry { kind, .. } => Some(*kind),
      AiEvent::Delta { .. } => None,
    };
    *open = match (ev, kind) {
      (AiEvent::Delta { .. }, _) => 0,
      (_, Some(Kind::Tool)) => *open + 1,
      (_, Some(Kind::Echo)) => open.saturating_sub(1),
      _ => *open,
    };
    let now = matches!(kind, Some(Kind::Echo | Kind::User)) && *open == 0;
    if std::mem::replace(asked, now) != now {
      self.change();
    }
  }

  /// What streams for the chat (agent: None) or an agent, set and told.
  pub(super) fn set_live(&mut self, agent: Option<&str>, live: Vec<Live>) {
    let live = self.redacted(live);
    self.tail_of(agent).clear();
    self.live_of(agent).clone_from(&live);
    self.all(Event::Live {
      agent: agent.map(str::to_string),
      live,
    });
  }

  /// An agent's report, once it ends: a turn of its own, unless stopped.
  pub(super) fn report(&mut self, agent: &str, text: String, stopped: bool) {
    self.unanswered(Some(agent));
    self.set_live(Some(agent), Vec::new());
    self.hold(move |chat| {
      let msg = chat.push(None, Kind::Work, &text);
      if !stopped {
        chat.take(msg);
      }
      chat.change();
    });
  }

  /// The config's `after`, a shell command, after each turn.
  fn after(&self) {
    let after = config::get().after.clone();
    if after.is_empty() {
      return;
    }
    let engine = self.engine();
    tokio::spawn(async move {
      let _one = engine.after.lock().await;
      let out = tokio::process::Command::new("/bin/sh")
        .args(["-c", &after])
        .stdout(std::process::Stdio::null())
        .output()
        .await;
      match out {
        Ok(out) if !out.status.success() => engine.tell(Event::Note(format!(
          "after: {}",
          String::from_utf8_lossy(&out.stderr).trim()
        ))),
        Err(err) => eprintln!("after: {err}"),
        _ => {}
      }
    });
  }

  /// All work stopped for good (a quit, or the assistant archived): the
  /// turn, with the waits on summaries its delegations and tells make,
  /// its goal's check, its agents.
  pub(super) fn stop(&mut self) {
    self.halt.cancel();
    if let Phase::Calling { call, .. } | Phase::Checking(call) = &self.phase {
      call.stop();
    }
    self.stop_agents(None);
  }

  /// A message from outside the chat (another assistant's reply), as an
  /// agent's report is: logged, and taken, never cutting in.
  pub(super) fn heard(&mut self, text: &str) {
    let text = text.to_string();
    self.hold(move |chat| {
      let msg = chat.push(None, Kind::Work, &text);
      chat.take(msg);
      chat.change();
    });
  }

  pub(super) fn save(&self) {
    if let Err(err) = self.mem.save() {
      eprintln!("acorn: state: {err}");
    }
  }

  /// What runs: the turn, the agents at work.
  pub(super) fn running(&self) -> memory::Work {
    memory::Work {
      turn: self.busy(),
      agents: self.working(),
    }
  }

  /// What runs changed: saved, and told.
  pub(super) fn change(&mut self) {
    let work = self.running();
    if work != self.mem.work {
      self.mem.work = work;
      self.save();
    }
    self.tell_state();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn handoff_keeps_a_message_sent_after_the_call_finished() {
    let dir = tempfile::tempdir().unwrap();
    let engine = crate::chat::tests::open(dir.path());
    let (call, halt) = {
      let mut chat = engine.lock();
      let call = ai::ask(chat.share(), Arc::new(|_| {}));
      chat.phase = Phase::Calling {
        call: call.handle.clone(),
        asked: false,
        open: 0,
      };
      let msg = chat.push(None, Kind::User, "already in the inbox");
      chat.take(msg);
      // Close the real inbox without dispatching a request. The first
      // message comes back in End::left, the second arrives after close.
      call.handle.stop();
      (call, chat.halt.clone())
    };
    while !call.done.is_finished() {
      tokio::task::yield_now().await;
    }
    engine.clone().say("one more thing".into(), vec![], None).await;
    engine.clone().ended(call, halt).await;
    let chat = engine.lock();
    let queued: Vec<_> = chat.wait.iter().map(|s| chat.mem.say(s.id, s.end).text).collect();
    assert_eq!(queued, ["already in the inbox", "one more thing"]);
  }

  #[tokio::test]
  async fn unbuilt_tail_goes_whole_before_the_waiting_messages() {
    let dir = tempfile::tempdir().unwrap();
    let engine = crate::chat::tests::open(dir.path());
    let mut chat = engine.lock();
    assert_eq!(chat.from(), 0);
    assert!(latest(&chat.mem, 0, 0).text.is_empty());
    chat
      .mem
      .push(vec![
        Msg::new(Kind::Acorn, "previous reply"),
        Msg::new(Kind::User, "next"),
      ])
      .unwrap();
    chat.wait.push(Span { id: 1, end: 2 });
    assert_eq!(chat.from(), 0);
    let tail = latest(&chat.mem, chat.from(), 1);
    assert!(
      tail
        .text
        .contains(&format!("0+0|{}", chat.mem.message(0).unwrap().show()))
    );
    assert!(!tail.text.contains("1+0|"));
    assert!(tail.imgs.is_empty());
    assert!(latest(&chat.mem, 2, 3).text.is_empty());
  }

  #[tokio::test]
  async fn an_oversize_tail_still_needs_summaries() {
    let dir = tempfile::tempdir().unwrap();
    let engine = crate::chat::tests::open(dir.path());
    let mut chat = engine.lock();
    chat
      .mem
      .push(vec![
        Msg::new(Kind::Acorn, "previous reply"),
        Msg::new(Kind::User, "x".repeat(config::get().view + 1)),
      ])
      .unwrap();
    let end = chat.mem.count;
    let call = ai::ask(chat.share(), Arc::new(|_| {}));
    chat.phase = Phase::Calling {
      call: call.handle.clone(),
      asked: false,
      open: 0,
    };
    chat.take(Span { id: 1, end });
    assert_eq!(chat.mem.first(0, 0), 0);
    assert!(chat.from() > 0);
    assert!(chat.from() < end);
    // A later, smaller message must follow the deferred pages.
    chat.mem.push(vec![Msg::new(Kind::User, "one more thing")]).unwrap();
    chat.take(Span { id: end, end: end + 1 });
    assert_eq!(chat.wait.iter().map(|s| s.id).collect::<Vec<_>>(), [1, end]);
    call.handle.stop();
  }
}
