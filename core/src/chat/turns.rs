//! Turns
//! =====
//! A message is logged as it comes and taken: by the running call, between
//! its tool calls, if it goes whole beside its view, else by the next. A
//! turn runs its calls till no message waits; each call first waits for
//! the summaries of the messages before its own (`settle`). A goal (/goal)
//! is checked as a turn would end, and its nudge keeps the turn going till
//! it is met.

use super::{Chat, Engine, Span};
use crate::ai::{self, Event as AiEvent};
use crate::memory::{self, IMAGES_ARE, Msg, Part};
use crate::wire::{Event, Live, TURN, live_push};
use crate::{Block, Kind, Served, config};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A turn's head, as a summary's is "Compaction:": one prompt serves both.
const HEAD: &str = "Turn: the user's new message:\n\n";

/// A goal's check: its own system prompt (Acorn's, which says a call is a
/// turn or a compaction, made the model summarize the chat instead of
/// answering), asked after the chat (the view, its latest messages whole)
/// of its goal; unmet, the user's nudge.
const GOAL: &str = "You check a goal. Below is a chat between a user and Acorn, an AI agent: its older messages as summary lines (id+n|text), then its latest messages whole, then the goal the user set with /goal. Answer yes if the chat shows the goal fully satisfied now, else no. Write that one word, nothing else.";
const GOAL_ASK: &str = "\nThe goal:\n\n";
const GOAL_NO: &str = "The goal isn't satisfied yet: keep working.";

impl Engine {
    /// A message from the person (its images first put in place), logged
    /// and taken.
    pub(super) async fn say(self: Arc<Self>, text: String, images: Vec<Vec<u8>>) {
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
        self.writing(async move || {
            let mut chat = me.lock();
            let msg = Msg { images: names, ..Msg::new(Kind::User, text) };
            let span = chat.push_msg(msg);
            chat.take(span);
        })
        .await;
    }

    /// Calls while messages wait.
    async fn turn(self: Arc<Self>) {
        let mut go = true;
        while go && !self.lock().wait.is_empty() {
            go = self.clone().call().await;
        }
        let mut chat = self.lock();
        if chat.halt.is_cancelled()
            && let Some(why) = chat.halted.take()
        {
            chat.note(why);
        }
        chat.call = None;
        chat.sent.clear();
        chat.wait.clear();
        chat.busy = false;
        chat.set_live(None, Vec::new());
        chat.spent();
        chat.after();
        chat.change();
    }

    /// A call of the turn, once the summaries before its messages are
    /// built; false: the turn ends.
    async fn call(self: Arc<Self>) -> bool {
        let halt = {
            let mut chat = self.lock();
            chat.halt = CancellationToken::new();
            chat.halted = None;
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
            if let Some(why) = self.settle(now, &halt).await {
                if !halt.is_cancelled() {
                    self.lock().failed(&why);
                }
                return false;
            }
        }
        let from = from.unwrap();
        let call = {
            let mut chat = self.lock();
            let msgs = std::mem::take(&mut chat.wait);
            chat.sent = msgs.clone();
            let says: Vec<crate::Say> = msgs.iter().filter(|m| m.end > from).map(|m| chat.mem.say(m.id.max(from), m.end)).collect();
            let text = says.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n\n");
            let imgs = says.into_iter().flat_map(|s| s.imgs).collect();
            chat.eye = chat.eye_from(from);
            chat.show();
            let mut blocks = chat.mem.view_blocks(from);
            blocks.push(chat.at_work());
            blocks.push(Block::new(format!("{HEAD}{text}"), true));
            let ask = ai::Ask { blocks, imgs, tools: Some(chat.kit(None)), meter: Some(chat.meter(Served::Chat)), ..chat.share() };
            chat.asked = true;
            chat.open = 0;
            let call = ai::ask(ask, chat.on_events(None));
            chat.call = Some(call.handle.clone());
            chat.change();
            call
        };
        self.ended(call, halt).await
    }

    /// The turn's call ends; false: the turn ends.
    async fn ended(self: Arc<Self>, call: ai::Call, halt: CancellationToken) -> bool {
        let end = call.end().await;
        let check = {
            let mut chat = self.lock();
            chat.call = None;
            chat.unanswered(None);
            chat.set_live(None, Vec::new());
            let left = chat.sent.len().saturating_sub(end.left.len());
            let back: Vec<Span> = chat.sent.split_off(left);
            chat.wait.splice(0..0, back);
            if !end.ok && !halt.is_cancelled() {
                chat.failed(&end.text);
            }
            if !end.note.is_empty() {
                chat.all(Event::Note(end.note.clone()));
            }
            match chat.mem.goal.clone() {
                Some(goal) if end.ok && !halt.is_cancelled() && chat.wait.is_empty() => Some((chat.check_goal(&goal), goal)),
                _ => None,
            }
        };
        match check {
            Some((call, goal)) => self.checked(call, goal, halt).await,
            None => !halt.is_cancelled(),
        }
    }

    /// A goal check's answer, as the turn's call's is: unmet (any answer
    /// but yes), the user's nudge goes to the turn as their message; met,
    /// the goal ends. A check that fails, a stop, a message or a /halt
    /// meanwhile end it with no nudge, so the turn ends; false: it ends.
    async fn checked(self: Arc<Self>, call: ai::Call, goal: String, halt: CancellationToken) -> bool {
        let handle = call.handle.clone();
        self.lock().check = Some(handle.clone());
        let mut done = call.done;
        let end = tokio::select! {
            end = &mut done => end,
            _ = halt.cancelled() => {
                handle.stop();
                done.await
            }
        }
        .unwrap_or_else(|err| ai::End::failed(err.to_string()));
        let nudge = {
            let mut chat = self.lock();
            let on = !halt.is_cancelled() && chat.mem.goal.as_deref() == Some(goal.as_str()) && chat.wait.is_empty();
            chat.check = None;
            if on && !end.ok {
                chat.note(format!("No goal check: {}. The goal stays; /halt drops it.", end.text.trim_end_matches('.')));
            } else if on && says_yes(&end.text) {
                chat.mem.goal = None;
                chat.save();
                chat.note("Goal met.");
            }
            on && end.ok && !says_yes(&end.text)
        };
        if nudge {
            let me = self.clone();
            self.writing(async move || {
                let mut chat = me.lock();
                let span = chat.push(None, Kind::User, GOAL_NO);
                chat.take(span);
            })
            .await;
        }
        !halt.is_cancelled()
    }
}

/// /^\W*yes\b/i
fn says_yes(text: &str) -> bool {
    let rest = text.trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
    rest.get(..3).is_some_and(|w| w.eq_ignore_ascii_case("yes")) && !rest[3..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

impl Chat {
    /// A message logged: to the running call if whole beside its view, else
    /// to the next.
    pub(super) fn take(&mut self, msg: Span) {
        if let Some(call) = self.call.clone()
            && self.mem.whole(msg.id) == msg.id
        {
            self.sent.push(msg);
            call.send(self.mem.say(msg.id, msg.end));
            return;
        }
        self.wait.push(msg);
        if !self.busy {
            self.busy = true;
            let engine = self.engine();
            tokio::spawn(engine.turn());
        }
    }

    /// Where a call's messages go whole from (a message meanwhile may move
    /// it).
    pub(super) fn from(&mut self) -> u64 {
        let first = self.wait.first().map_or(self.mem.count, |m| m.id);
        self.mem.whole(first)
    }

    /// The user's goal (/goal), asked of the chat's model, without tools, on
    /// the chat as it is now.
    fn check_goal(&mut self, goal: &str) -> ai::Call {
        let at = self.mem.whole(0);
        let tail: String = (at..self.mem.count)
            .filter_map(|i| self.mem.message(i))
            .map(|m| format!("{}\n", memory::line(Part::leaf(m.i), &format!("{}: {}", m.kind.as_str(), m.text))))
            .collect();
        let mut blocks = self.mem.view_blocks(at);
        blocks.push(Block::new(format!("{tail}{GOAL_ASK}{goal}"), true));
        let ask = ai::Ask { system: GOAL.into(), blocks, meter: Some(self.meter(Served::Chat)), ..self.share() };
        ai::ask(ask, Arc::new(|_| {}))
    }

    /// A failed turn's why, as a note.
    pub(super) fn failed(&mut self, why: &str) {
        self.note(format!("No reply: {}. Send a message to try again.", why.trim_end_matches('.')));
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
        match agent {
            Some(name) => {
                let id = self.mem.agent_push(name, kind, text).unwrap_or_else(|err| {
                    eprintln!("agent {name}: {err}");
                    0
                });
                let entry = crate::wire::Entry { kind, text: text.into(), date: memory::iso_now(), images: vec![], spend: None };
                self.all(Event::Entry { agent: Some(name.into()), id: Some(id), entry });
                Span { id, end: id + 1 }
            }
            None => self.push_msg(Msg::new(kind, text)),
        }
    }

    pub(super) fn push_msg(&mut self, msg: Msg) -> Span {
        let roots = match self.mem.push(vec![msg]) {
            Ok(roots) => roots,
            Err(err) => {
                eprintln!("acorn: a message was not logged: {err}");
                return Span { id: self.mem.count, end: self.mem.count };
            }
        };
        for r in &roots {
            let entry = crate::wire::Entry { kind: r.kind, text: r.text.clone(), date: r.date.clone(), images: r.images.clone(), spend: None };
            self.all(Event::Entry { agent: None, id: Some(r.i), entry });
        }
        self.pump();
        Span { id: roots[0].i, end: self.mem.count }
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
                self.all(Event::Delta { agent: agent.map(str::to_string), kind, text, fresh });
                return;
            }
            AiEvent::Entry { kind: Kind::Tool, text, of: Some(of) } => {
                let calls = self.calls_of(agent);
                calls.retain(|(id, _)| *id != of);
                calls.push((of, text));
            }
            AiEvent::Entry { kind: Kind::Mind, text, .. } => {
                let entry = crate::wire::Entry { kind: Kind::Mind, text, date: memory::iso_now(), images: vec![], spend: None };
                self.all(Event::Entry { agent: agent.map(str::to_string), id: None, entry });
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
        self.calls_of(agent).iter().map(|(_, text)| Live { kind: Kind::Tool, text: text.clone() }).collect()
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
        let kind = match ev {
            AiEvent::Entry { kind, .. } => Some(*kind),
            AiEvent::Delta { .. } => None,
        };
        self.open = match (ev, kind) {
            (AiEvent::Delta { .. }, _) => 0,
            (_, Some(Kind::Tool)) => self.open + 1,
            (_, Some(Kind::Echo)) => self.open.saturating_sub(1),
            _ => self.open,
        };
        let asked = matches!(kind, Some(Kind::Echo | Kind::User)) && self.open == 0;
        if asked != self.asked {
            self.asked = asked;
            self.change();
        }
    }

    /// What streams for the chat (agent: None) or an agent, set and told.
    pub(super) fn set_live(&mut self, agent: Option<&str>, live: Vec<Live>) {
        *self.live_of(agent) = live.clone();
        self.all(Event::Live { agent: agent.map(str::to_string), live });
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
    fn after(&mut self) {
        let after = config::get().after.clone();
        if after.is_empty() {
            return;
        }
        let engine = self.engine();
        tokio::spawn(async move {
            let _one = engine.after.lock().await;
            let out = tokio::process::Command::new("/bin/sh").args(["-c", &after]).stdout(std::process::Stdio::null()).output().await;
            match out {
                Ok(out) if !out.status.success() => engine.tell(Event::Note(format!("after: {}", String::from_utf8_lossy(&out.stderr).trim()))),
                Err(err) => eprintln!("after: {err}"),
                _ => {}
            }
        });
    }

    /// Acorn's turn only, with the spawns and tells it waits on; agents go
    /// on. why: the note the turn's end logs, after the reply the stop cut.
    pub(super) fn interrupt(&mut self, why: Option<String>) {
        if !self.halt.is_cancelled() {
            self.halted = why;
        }
        self.halt.cancel();
        if let Some(call) = &self.call {
            call.stop();
        }
    }

    /// All work, agents too; its why noted at the turn's end, or at once if
    /// no turn is left to end.
    pub(super) fn stop(&mut self, why: Option<String>) {
        let turn = self.busy && !self.halt.is_cancelled();
        self.interrupt(if turn { why.clone() } else { None });
        self.stop_agents(None);
        if !turn && let Some(why) = why {
            self.note(why);
        }
    }

    /// The person stops all work, or what they name.
    pub(super) fn stop_by_user(&mut self, name: Option<String>) {
        let what: Vec<String> = self.running().into_iter().filter(|w| name.as_ref().is_none_or(|n| n == w)).collect();
        if what.is_empty() {
            return;
        }
        let why = format!("Stopped by the user: {}.", what.join(", "));
        let turn = what.iter().any(|w| w == TURN);
        if turn {
            self.interrupt(Some(why.clone()));
        }
        for w in &what {
            self.stop_agents(Some(w));
        }
        if !turn {
            self.note(why);
        }
    }

    pub(super) fn set_goal(&mut self, goal: Option<String>) {
        if goal == self.mem.goal {
            return;
        }
        let halted = goal.is_none();
        self.mem.goal = goal;
        self.save();
        if halted {
            self.note("Goal halted by the user.");
        }
    }

    pub(super) fn set_pick(&mut self, pick: crate::pick::Pick) {
        self.mem.pick = serde_json::to_value(pick).unwrap_or_default();
        self.save();
        self.change();
    }

    pub(super) fn save(&self) {
        if let Err(err) = self.mem.save() {
            eprintln!("acorn: state: {err}");
        }
    }

    /// What runs: Acorn's turn, the agents at work.
    pub(super) fn running(&self) -> Vec<String> {
        let turn = self.busy.then(|| TURN.to_string());
        turn.into_iter().chain(self.working()).collect()
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
