//! Turns
//! =====
//! A message is logged as it comes and taken: by the running call if it
//! goes whole beside its view, else by the next. The user's cuts in on the
//! call at work (`ai::agent`); an agent's report or a note waits for its
//! step to end. A
//! turn runs its calls till no message waits; each call first waits for
//! the summaries of the messages before its own (`settle`). A goal (the
//! `goal` tool) is checked as a turn would end, and its nudge keeps the
//! turn going till it is met, unless the user cut in meanwhile: then the
//! assistant answers them and waits. A thread's turn ends with its reply
//! (every call's, or why there was none) sent to its asker; one that
//! asked for an import begins it.

use super::{Chat, Engine, Phase, Span};
use crate::ai::{self, Event as AiEvent};
use crate::memory::{self, IMAGES_ARE, Msg, Part};
use crate::wire::{Event, Live, live_push};
use crate::{Block, Kind, Served, config};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Where a message came from, as told (`wire::Entry`).
pub const APP: &str = "app";
pub const TIMER: &str = "timer";

/// A turn's head, as a summary's is "Compaction:": one prompt serves both.
const HEAD: &str = "Turn: the user's new message:\n\n";

/// A goal's check: its own system prompt (Acorn's, which says a call is a
/// turn or a compaction, made the model summarize the chat instead of
/// answering), asked after the chat (the view, its latest messages whole)
/// of its goal; unmet, the user's nudge.
const GOAL: &str = "You check a goal. Below is a chat between a user and an AI assistant: its older messages as summary lines (id+n|text), then its latest messages whole, then the goal the assistant set itself from the user's words. Answer yes if the chat shows the goal fully satisfied now, by evidence in it (tool results, files, outputs), not by the assistant's word alone; else no. Write that one word, nothing else.";
const GOAL_ASK: &str = "\nThe goal:\n\n";
const GOAL_NO: &str = "The goal isn't satisfied yet: keep working. Treat the current workspace, tool results and files as authoritative; inspect them instead of assuming earlier narration is still current. Make concrete progress and verify the result. Before saying the goal is done, gather evidence that the whole of it is achieved.";

impl Engine {
  /// A message from the person (its images first put in place), logged
  /// and taken, told as come from `via` (none: the app).
  pub(super) async fn say(self: Arc<Self>, text: String, images: Vec<Vec<u8>>, via: Option<String>) {
    let dir = self.lock().mem.dir.clone();
    let mut names = Vec::new();
    for bytes in images {
      let dir = dir.clone();
      match tokio::task::spawn_blocking(move || memory::image_add(&dir, &bytes)).await {
        Ok(Ok(name)) => names.push(name),
        _ => return self.tell(Event::Note(IMAGES_ARE.into())),
      }
    }
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

  /// A call of the turn, once the summaries before its messages are
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
      chat.sent.clone_from(&msgs);
      let says: Vec<crate::Say> = msgs
        .iter()
        .filter(|m| m.end > from)
        .map(|m| chat.mem.say(m.id.max(from), m.end))
        .collect();
      let text = says.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n\n");
      let imgs = says.into_iter().flat_map(|s| s.imgs).collect();
      chat.eye = chat.eye_from(from);
      chat.show();
      let mut blocks = chat.mem.view_blocks(from);
      blocks.push(chat.at_work());
      blocks.extend(threads);
      blocks.push(Block::new(format!("{HEAD}{text}"), true));
      let ask = ai::Ask {
        blocks,
        imgs,
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
      let left = chat.sent.len().saturating_sub(end.left.len());
      let back: Vec<Span> = chat.sent.split_off(left);
      chat.wait.splice(0..0, back);
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
      let check = match chat.mem.goal.clone() {
        Some(goal) if end.ok && !halt.is_cancelled() && chat.wait.is_empty() && !chat.cut_in => {
          let check = chat.check_goal(&goal);
          chat.phase = Phase::Checking(check.handle.clone());
          Some((check, goal))
        }
        _ => None,
      };
      chat.change();
      check
    };
    if let Some((check, goal)) = check {
      self.checked(check, goal, halt).await;
    }
  }

  /// A goal check's answer, as the turn's call's is: unmet (any answer
  /// but yes), the user's nudge goes to the turn as their message; met,
  /// the goal ends. A check that fails, a stop, a message (which stops
  /// the check: its answer would be moot) or the goal dropped meanwhile
  /// end it with no nudge.
  async fn checked(self: Arc<Self>, call: ai::Call, goal: String, halt: CancellationToken) {
    let end = call.end().await;
    let nudge = {
      let mut chat = self.lock();
      chat.phase = Phase::Settling;
      let on = !halt.is_cancelled() && chat.mem.goal.as_deref() == Some(goal.as_str()) && chat.wait.is_empty();
      if on && !end.ok {
        chat.note(format!(
          "No goal check: {}. The goal stays till it is dropped.",
          end.text.trim_end_matches('.')
        ));
      } else if on && says_yes(&end.text) {
        chat.mem.goal = None;
        chat.save();
        chat.note("Goal met.");
      }
      chat.change();
      on && end.ok && !says_yes(&end.text)
    };
    if nudge {
      let me = self.clone();
      self
        .writing(async move || {
          let mut chat = me.lock();
          let span = chat.push(None, Kind::User, GOAL_NO);
          chat.take(span);
        })
        .await;
    }
  }
}

/// /^\W*yes\b/i
fn says_yes(text: &str) -> bool {
  let rest = text.trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
  rest.get(..3).is_some_and(|w| w.eq_ignore_ascii_case("yes"))
    && !rest[3..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

impl Chat {
  /// A message logged: to the running call if whole beside its view (the
  /// user's cutting in), else to the next.
  pub(super) fn take(&mut self, msg: Span) {
    if let Phase::Calling { call, .. } = &self.phase
      && self.mem.whole(msg.id) == msg.id
    {
      let call = call.clone();
      self.sent.push(msg);
      let cut = self.mem.message(msg.id).is_some_and(|m| m.kind == Kind::User);
      self.cut_in |= cut;
      call.send(crate::Say {
        cut,
        ..self.mem.say(msg.id, msg.end)
      });
      return;
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
    self.mem.whole(first)
  }

  /// The goal (the `goal` tool), asked of the chat's model, without
  /// tools, on the chat as it is now.
  fn check_goal(&self, goal: &str) -> ai::Call {
    let at = self.mem.whole(0);
    let tail: String = (at..self.mem.count)
      .filter_map(|i| self.mem.message(i))
      .map(|m| {
        format!(
          "{}\n",
          memory::line(Part::leaf(m.i), &format!("{}: {}", m.kind.as_str(), m.text))
        )
      })
      .collect();
    let mut blocks = self.mem.view_blocks(at);
    blocks.push(Block::new(format!("{tail}{GOAL_ASK}{goal}"), true));
    let ask = ai::Ask {
      system: GOAL.into(),
      blocks,
      meter: Some(self.meter(Served::Chat)),
      ..self.share()
    };
    ai::ask(ask, Arc::new(|_| {}))
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
      AiEvent::Delta { kind, text, fresh } => {
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
          text,
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

  fn live_of(&mut self, agent: Option<&str>) -> &mut Vec<Live> {
    match agent.and_then(|name| self.agents.get_mut(name)) {
      Some(a) => &mut a.live,
      None => &mut self.live,
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

  /// The goal set, or dropped (None), and told.
  pub(super) fn set_goal(&mut self, goal: Option<String>) {
    if goal != self.mem.goal {
      self.mem.goal = goal;
      self.save();
      self.tell_state();
    }
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
  #[test]
  fn yes_is_a_word() {
    assert!(super::says_yes("Yes."));
    assert!(super::says_yes("**yes**"));
    assert!(!super::says_yes("yesterday"));
    assert!(!super::says_yes("no"));
  }
}
