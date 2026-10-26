//! Model
//! =====
//! What the window holds and draws of the chat, with no view of its own:
//! `hear` takes the engine's events, `act` the person's acts, and both
//! answer what to send back. The main chat shows what its agent sees: the
//! view's parts as blocks ("nx"), from the turn on each message whole (1x)
//! and its thoughts, then what streams or what the turn waits on. An
//! agent's page is its log, kept live. The AI notice opens the chat and
//! comes again before a message in another 3-hour window.

use crate::call::{self, Call};
use crate::text;
use acorn_core::import::Found;
use acorn_core::pick::Pick;
use acorn_core::wire::{Command, Entry, Event, Live, Node, Progress, Seg, State, TURN, Usage, Wait, live_push};
use acorn_core::{Kind, Spend};
use std::collections::HashMap;

/// The disclosure the chat shows (EU AI Act art. 50, California SB 243,
/// New York GBL art. 47).
pub const AI: &str = "Acorn is an AI, not a person, and can make mistakes.";

/// The AI notice's period (ms).
const NOTICE: i64 = 3 * 60 * 60 * 1000;

const WAIT: &str = "compressing…";
const SUMS: &str = "compressing earlier messages…";
const FOLD: &str = "The whole chat, folded into the fewest blocks.";

/// An import's minutes a micro-USD of its time (the compactor, measured on
/// a real import: 990 s for 162,900).
const PACE: f64 = 1e-4;

/// An agent's page: its log (None while it loads), what streams.
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub log: Option<Vec<Entry>>,
    pub live: Vec<Live>,
}

/// The chat as the window knows it. fold: the tops shown (None: not
/// folded); nodes and whole: the summaries and messages known; open: the
/// blocks opened (true) or shut by hand; minds: the thoughts after each
/// message; pages: the agents' asked for; page: the one shown (None:
/// Acorn's).
#[derive(Clone, Debug)]
pub struct Model {
    pub state: Option<State>,
    pub parts: Vec<Seg>,
    pub turn: u64,
    pub fold: Option<Vec<Seg>>,
    pub nodes: HashMap<Seg, Node>,
    pub whole: HashMap<u64, Entry>,
    pub open: HashMap<Seg, bool>,
    pub live: Vec<Live>,
    pub minds: HashMap<u64, Vec<String>>,
    pub pages: HashMap<String, Page>,
    pub page: Option<String>,
}

impl Default for Model {
    fn default() -> Model {
        Model {
            state: None,
            parts: Vec::new(),
            turn: u64::MAX,
            fold: None,
            nodes: HashMap::new(),
            whole: HashMap::new(),
            open: HashMap::new(),
            live: Vec::new(),
            minds: HashMap::new(),
            pages: HashMap::new(),
            page: None,
        }
    }
}

/// What a page draws, top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// A block: nx, its summary (call: a tool call's, word for word).
    Sum { seg: Seg, text: String, call: Option<Call> },
    /// A block open: its bar, its halves.
    Box { seg: Seg, text: String, rows: Vec<Row> },
    /// A message whole: who said it (named on its frame), its text (a
    /// report's, its replies), its call if a tool call.
    Msg { kind: Kind, who: String, text: String, call: Option<Call>, images: Vec<String> },
    /// What a turn used, under its last message.
    Use(Spend),
    /// A line about the page.
    Cap(String),
}

/// A person's act.
pub enum Act {
    /// Enter: a message, or, empty, interrupt.
    Send { text: String, images: Vec<Vec<u8>> },
    /// A click on a block.
    Open(Seg),
    /// A click on an open block's bar.
    Shut(Seg),
    /// Fold all, unfold all.
    Fold(bool),
    /// A tab, a name, the logo (None: Acorn).
    Goto(Option<String>),
}

/// What an act or an event leads to: commands to the engine, notes to
/// show.
#[derive(Default)]
pub struct Then {
    pub cmds: Vec<Command>,
    pub notes: Vec<String>,
    pub usage: Option<Vec<Usage>>,
    pub found: Option<Found>,
}

impl Model {
    pub fn hear(&mut self, ev: Event) -> Then {
        let mut then = Then::default();
        match ev {
            Event::Know { whole, nodes } => {
                self.whole.extend(whole);
                self.nodes.extend(nodes);
            }
            Event::View { parts, turn } => {
                self.parts = parts;
                self.turn = turn;
                if self.fold.is_some() {
                    then.cmds.push(Command::Root);
                }
            }
            Event::Zoom { .. } => {}
            Event::Root { parts } => {
                if self.fold.is_some() {
                    self.fold = Some(parts);
                }
            }
            Event::Entry { agent: Some(name), entry, .. } => {
                if let Some(log) = self.pages.get_mut(&name).and_then(|p| p.log.as_mut()) {
                    log.push(entry);
                }
            }
            Event::Entry { agent: None, id: Some(id), entry } => {
                self.whole.insert(id, entry);
                self.parts.push((id, 1));
            }
            Event::Entry { agent: None, id: None, entry } => {
                if entry.kind == Kind::Mind
                    && let Some(&(id, n)) = self.parts.last()
                {
                    self.minds.entry(id + n - 1).or_default().push(entry.text);
                }
            }
            Event::Delta { agent, kind, text, fresh } => {
                if let Some(live) = self.live_of(agent.as_deref()) {
                    live_push(live, kind, &text, fresh);
                }
            }
            Event::Live { agent, live } => {
                if let Some(now) = self.live_of(agent.as_deref()) {
                    *now = live;
                }
            }
            Event::State(state) => self.state = Some(state),
            Event::Agent { name, log } => match log {
                Some(log) => {
                    if let Some(page) = self.pages.get_mut(&name) {
                        page.log = Some(log);
                    }
                }
                None => {
                    self.pages.remove(&name);
                    if self.page.as_deref() == Some(&name) {
                        self.page = None;
                    }
                    then.notes.push(format!("No agent {name}."));
                }
            },
            Event::Note(text) => then.notes.push(text),
            Event::Use { id, spend } => {
                if let Some(entry) = self.whole.get_mut(&id) {
                    entry.spend = Some(spend);
                }
            }
            Event::Usage(days) => then.usage = Some(days),
            Event::Sessions(found) => then.found = Some(found),
            Event::Assistant(_) => {}
        }
        then
    }

    pub fn act(&mut self, act: Act) -> Then {
        let mut then = Then::default();
        match act {
            Act::Send { text, images } => {
                // a `/word` is no command (the window has its own), but
                // /goal (a message, sent as typed, set as the goal) and
                // /halt
                let text = text.trim().to_string();
                let word = text.strip_prefix('/').map(|w| w.split(char::is_whitespace).next().unwrap_or_default()).filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphabetic()));
                if self.page.is_some() {
                    return then;
                }
                if text.is_empty() && images.is_empty() {
                    if self.busy() {
                        then.cmds.push(Command::Stop(Some(TURN.into())));
                    }
                    return then;
                }
                match word {
                    Some("halt") => then.cmds.push(Command::Goal(None)),
                    Some("goal") => {
                        let goal = text["/goal".len()..].trim().to_string();
                        if goal.is_empty() {
                            then.notes.push("/goal needs one: /goal <message>.".into());
                        } else {
                            self.fold = None;
                            then.cmds.push(Command::Goal(Some(goal.clone())));
                            then.cmds.push(Command::Say { text: goal, images });
                        }
                    }
                    Some(w) => then.notes.push(format!("/{w} is no command; nothing was sent. The ☰ menu has the commands.")),
                    None => {
                        self.fold = None;
                        then.cmds.push(Command::Say { text, images });
                    }
                }
            }
            Act::Open(seg) => {
                self.open.insert(seg, true);
                if seg.1 > 1 {
                    then.cmds.push(Command::Zoom(seg));
                }
            }
            Act::Shut(seg) => {
                self.open.insert(seg, false);
            }
            Act::Fold(on) => {
                self.open.clear();
                self.fold = on.then(Vec::new);
                if on {
                    then.cmds.push(Command::Root);
                }
            }
            Act::Goto(None) => self.page = None,
            Act::Goto(Some(name)) => {
                if !self.pages.contains_key(&name) {
                    self.pages.insert(name.clone(), Page::default());
                    then.cmds.push(Command::Agent(name.clone()));
                }
                self.page = Some(name);
            }
        }
        then
    }

    fn live_of(&mut self, agent: Option<&str>) -> Option<&mut Vec<Live>> {
        match agent {
            None => Some(&mut self.live),
            Some(name) => self.pages.get_mut(name).map(|p| &mut p.live),
        }
    }

    pub fn busy(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.busy)
    }

    pub fn pick(&self) -> Option<Pick> {
        self.state.as_ref().map(|s| s.pick)
    }

    pub fn agents(&self) -> &[String] {
        self.state.as_ref().map_or(&[], |s| &s.agents)
    }

    /// The chat, each agent at work, and the page shown while its agent is
    /// done (so it can be left).
    pub fn tabs(&self) -> Vec<Option<String>> {
        let mut names: Vec<String> = self.agents().to_vec();
        if let Some(at) = &self.page
            && !names.contains(at)
        {
            names.push(at.clone());
        }
        std::iter::once(None).chain(names.into_iter().map(Some)).collect()
    }

    /// Whether messages go from the box: only on Acorn's page.
    pub fn typing(&self) -> bool {
        self.page.is_none()
    }

    // Rows
    // ----

    /// The page shown, as items (each a part's rows, or an agent's entry).
    pub fn body(&self) -> Vec<Vec<Row>> {
        if let Some(name) = &self.page {
            let page = self.pages.get(name);
            let at = if self.agents().contains(name) { "at work" } else { "done" };
            let head = vec![Row::Cap(format!("{name} · {at}"))];
            let log = page.and_then(|p| p.log.as_ref()).into_iter().flatten().map(|e| entry_rows(e.kind, &e.text, &e.images));
            return std::iter::once(head).chain(log).filter(|rows| !rows.is_empty()).collect();
        }
        if let Some(tops) = &self.fold {
            let rows = tops.iter().map(|&(id, n)| self.part_rows(id, n, u64::MAX, false));
            return std::iter::once(vec![Row::Cap(FOLD.into())]).chain(rows).collect();
        }
        let mut items = vec![vec![Row::Cap(AI.into())]];
        for (k, &part) in self.parts.iter().enumerate() {
            let mut rows = self.part_rows(part.0, part.1, self.turn, true);
            if self.due(part, k.checked_sub(1).map(|k| self.parts[k])) {
                rows.insert(0, Row::Cap(AI.into()));
            }
            items.push(rows);
        }
        items
    }

    /// What streams on the page shown, and what the turn waits on.
    pub fn tail(&self) -> Vec<Row> {
        if self.page.is_none() && self.fold.is_some() {
            return Vec::new();
        }
        let live = match &self.page {
            Some(name) => self.pages.get(name).map(|p| p.live.as_slice()).unwrap_or_default(),
            None => &self.live,
        };
        let rows: Vec<Row> = live.iter().flat_map(|l| entry_rows(l.kind, &l.text, &[])).collect();
        let load: Vec<Row> = match (&self.page, self.state.as_ref().and_then(|s| s.importing.as_ref())) {
            (None, Some(p)) => vec![Row::Cap(progress_show(p))],
            _ => vec![],
        };
        let wait = self.state.as_ref().map_or(Wait::None, |s| s.wait.clone());
        if self.page.is_some() || !rows.is_empty() || wait == Wait::None {
            return rows.into_iter().chain(load).collect();
        }
        let cap = match wait {
            Wait::Summaries => SUMS.to_string(),
            _ => format!("waiting for {}…", acorn_core::ai::deepseek::MODEL),
        };
        std::iter::once(Row::Cap(cap)).chain(load).collect()
    }

    /// Whether the AI notice comes before part, after the part `before`: a
    /// message from the turn on, in another window than that part's.
    fn due(&self, part: Seg, before: Option<Seg>) -> bool {
        let Some(last) = before.and_then(|b| self.window(b)) else { return false };
        part.1 == 1 && part.0 >= self.turn && self.window(part).is_some_and(|w| w != last)
    }

    /// The window (NOTICE) of a part's last date (None: not known).
    fn window(&self, (id, n): Seg) -> Option<i64> {
        let date = if n == 1 { self.whole.get(&id).map(|e| e.date.as_str()) } else { self.nodes.get(&(id, n)).map(|n| n.to.as_str()) }?;
        let ms = chrono::DateTime::parse_from_rfc3339(date).ok()?.timestamp_millis();
        Some(ms.div_euclid(NOTICE))
    }

    /// t: the turn; talk: whether talk opens by itself (not when folded).
    /// Open: the person's choice if made, else a message of the user or
    /// Acorn (tool calls, reports and the rest stay folded).
    fn part_rows(&self, id: u64, n: u64, t: u64, talk: bool) -> Vec<Row> {
        let entry = self.whole.get(&id);
        if let Some(entry) = entry.filter(|_| n == 1 && id >= t) {
            let mut rows = entry_rows(entry.kind, &entry.text, &entry.images);
            rows.extend(entry.spend.filter(|s| s.n > 0).map(Row::Use));
            for mind in self.minds.get(&id).into_iter().flatten() {
                rows.extend(entry_rows(Kind::Mind, mind, &[]));
            }
            return rows;
        }
        let talks = talk && n == 1 && entry.is_some_and(|e| matches!(e.kind, Kind::User | Kind::Acorn));
        if !self.open.get(&(id, n)).copied().unwrap_or(talks) {
            return vec![self.sum_row(id, n)];
        }
        let text = if n == 1 { "fold".to_string() } else { format!("▾ {n}x · fold") };
        let rows = if n > 1 {
            let half = n / 2;
            let mut rows = self.part_rows(id, half, t, talk);
            rows.extend(self.part_rows(id + half, half, t, talk));
            rows
        } else {
            let mut rows = entry.map(|e| entry_rows(e.kind, &e.text, &e.images)).unwrap_or_default();
            rows.extend(entry.and_then(|e| e.spend).filter(|s| s.n > 0).map(Row::Use));
            rows
        };
        vec![Row::Box { seg: (id, n), text, rows }]
    }

    /// A block; one whose summary isn't known yet says WAIT. A line of one
    /// message that is a tool call word for word shows as the call.
    fn sum_row(&self, id: u64, n: u64) -> Row {
        let text = self.nodes.get(&(id, n)).map_or(WAIT, |node| node.text.as_str());
        let call = (n == 1).then(|| text.strip_prefix("tool: ").map(call::parse)).flatten().filter(|c| !c.name.is_empty());
        Row::Sum { seg: (id, n), text: text::flat(text), call }
    }
}

/// A message's rows: none if it shows nothing.
pub fn entry_rows(kind: Kind, text: &str, images: &[String]) -> Vec<Row> {
    let call = (kind == Kind::Tool).then(|| call::parse(text));
    let report = (kind == Kind::Work).then(|| call::report_read(text)).flatten();
    let who = match (&report, kind) {
        (Some((name, _)), _) => name.clone(),
        (None, Kind::Work) => "Report".into(),
        (None, Kind::Note) => "Note".into(),
        _ => String::new(),
    };
    let text = report.map_or_else(|| text.to_string(), |(_, t)| t);
    if call.is_none() && text.trim().is_empty() && images.is_empty() {
        return vec![];
    }
    vec![Row::Msg { kind, who, text, call, images: images.to_vec() }]
}

/// An import's progress.
pub fn progress_show(p: &Progress) -> String {
    let merges: u64 = p.open.iter().sum();
    let phase = if p.open.first().is_some_and(|&n| n > 0) {
        format!("Summarizing messages: {} of {}", text::grouped(p.msgs - p.open[0]), text::grouped(p.msgs))
    } else {
        let minutes = ((p.total - p.done) * PACE).ceil();
        format!("Merging the top of the tree: {merges} {} left · about {minutes} min", if merges == 1 { "merge" } else { "merges" })
    };
    if p.why.is_some() { format!("{phase} · paused") } else { phase }
}

/// The days' use (newest first): a table a day, then a total; kinds as a
/// person counts them.
pub fn usage_md(days: &[Usage]) -> String {
    if days.is_empty() {
        return "No usage yet.".into();
    }
    let kind = |k: &str| match k {
        "chat" => "replies",
        "compactor" => "summaries",
        "import" => "import summaries",
        "agent" => "agents",
        "computer" => "computer use",
        k => k,
    }
    .to_string();
    let cells = |name: String, u: &Spend| {
        format!("| {name} | {} | {} | {} | {} | {} |", u.n, text::tokens_show(u.input + u.read), text::tokens_show(u.read), text::tokens_show(u.output), text::usd_show(u.usd))
    };
    let mut lines = vec!["| | Calls | In | Cached | Out | Cost |".to_string(), "|---|--:|--:|--:|--:|--:|".to_string()];
    let mut seen: Vec<&str> = Vec::new();
    for d in days {
        if !seen.contains(&d.day.as_str()) {
            seen.push(&d.day);
        }
    }
    for day in seen {
        let of: Vec<&Usage> = days.iter().filter(|d| d.day == day).collect();
        let sum = of.iter().fold(Spend::default(), |a, d| a + d.spend);
        lines.push(cells(format!("**{day}**"), &sum));
        lines.extend(of.iter().map(|d| cells(format!("  {}", kind(&d.kind)), &d.spend)));
    }
    let all = days.iter().fold(Spend::default(), |a, d| a + d.spend);
    lines.push(cells("**All**".into(), &all));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use acorn_core::pick::Effort;

    fn entry(kind: Kind, text: &str) -> Entry {
        Entry { kind, text: text.into(), date: "2026-10-26T10:00:00.000Z".into(), images: vec![], spend: None }
    }

    #[test]
    fn the_chat_shows_blocks_then_the_turn_whole() {
        let mut m = Model::default();
        m.hear(Event::Know { whole: vec![(0, entry(Kind::User, "hi")), (1, entry(Kind::Tool, "shell {\"command\":\"ls\"}"))], nodes: vec![] });
        m.hear(Event::View { parts: vec![(0, 1), (1, 1)], turn: 1 });
        let body = m.body();
        assert_eq!(body[0], vec![Row::Cap(AI.into())]);
        assert!(matches!(&body[1][0], Row::Box { .. }), "a user's message before the turn opens");
        assert!(matches!(&body[2][0], Row::Msg { call: Some(c), .. } if c.name == "shell"));
    }

    #[test]
    fn acts_say_what_to_send() {
        let mut m = Model::default();
        m.hear(Event::State(State { busy: true, wait: Wait::None, agents: vec![], pick: Pick { effort: Effort::High }, importing: None, today: None }));
        assert!(matches!(m.act(Act::Send { text: " ".into(), images: vec![] }).cmds[..], [Command::Stop(_)]));
        assert!(matches!(m.act(Act::Send { text: "/goal ship it".into(), images: vec![] }).cmds[..], [Command::Goal(Some(_)), Command::Say { .. }]));
        assert_eq!(m.act(Act::Send { text: "/nope".into(), images: vec![] }).notes.len(), 1);
    }
}
