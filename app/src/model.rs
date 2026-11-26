//! Model
//! =====
//! What the window holds and draws of the chat, with no view of its own:
//! `hear` takes the engine's events, `act` the person's acts, and both
//! answer what to send back. The main chat shows what its agent sees: the
//! view's parts as blocks ("nx"), from the turn on each message whole (1x)
//! and its thoughts, then what streams or what the turn waits on. An
//! agent's page is its log, kept live. A thread opens with its caption.
//! The only act that changes anything is a message; the rest look.

use crate::call::{self, Call};
use crate::text;
use acorn_core::wire::{Command, Entry, Event, Live, Node, Progress, Seg, State, Wait, live_push};
use acorn_core::{Kind, Spend};
use std::collections::HashMap;

const WAIT: &str = "summarizing…";
const SUMS: &str = "summarizing earlier messages…";
const FOLD: &str = "Overview: the whole chat in the fewest blocks. Esc goes back.";
const FOLDING: &str = "finding the fewest blocks…";

/// An import's minutes a micro-USD of its time (the compactor, measured on
/// a real import: 990 s for 162,900).
const PACE: f64 = 1e-4;

/// An agent's page: its log (None while it loads), what streams.
#[derive(Clone, Debug, Default)]
pub struct Page {
  pub log: Option<Vec<Entry>>,
  pub live: Vec<Live>,
}

/// The overview: asked for, or its tops.
#[derive(Clone, Debug, PartialEq)]
pub enum Fold {
  Asked,
  Tops(Vec<Seg>),
}

/// The chat as the window knows it. head: what it opens with (a thread's
/// caption; None: nothing); fold: the overview (None: not shown); nodes
/// and whole: the
/// summaries and messages known; open: the blocks opened (true) or shut
/// by hand; minds: the thoughts after each message; pages: the agents'
/// asked for; page: the one shown (None: the chat).
#[derive(Clone, Debug)]
pub struct Model {
  pub head: Option<String>,
  pub state: Option<State>,
  pub parts: Vec<Seg>,
  pub turn: u64,
  pub fold: Option<Fold>,
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
      head: None,
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
  /// A block: of n messages, its summary (call: a tool call's, word for
  /// word).
  Sum { seg: Seg, text: String, call: Option<Call> },
  /// A block open: its bar, its halves.
  Box { seg: Seg, text: String, rows: Vec<Row> },
  /// A message whole: who said it (named on its frame), its text (a
  /// report's, its replies), its call if a tool call.
  Msg {
    kind: Kind,
    who: String,
    text: String,
    call: Option<Call>,
    images: Vec<String>,
  },
  /// What a turn used, under its last message.
  Use(Spend),
  /// A line about the page.
  Cap(String),
}

/// A person's act: a message, or a look.
pub enum Act {
  /// Enter: a message (it cuts in on work).
  Send { text: String, images: Vec<Vec<u8>> },
  /// A click on a block.
  Open(Seg),
  /// A click on an open block's bar: collapsed.
  Shut(Seg),
  /// The overview, or back from it.
  Fold(bool),
  /// A tab, a name, the logo (None: Acorn).
  Goto(Option<String>),
  /// Back to the chat: off the agent's page, out of the overview.
  Home,
}

/// What an act or an event leads to: commands to the engine, notes to
/// show.
#[derive(Default)]
pub struct Then {
  pub cmds: Vec<Command>,
  pub notes: Vec<String>,
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
          self.fold = Some(Fold::Tops(parts));
        }
      }
      Event::Entry {
        agent: Some(name),
        entry,
        ..
      } => {
        if let Some(log) = self.pages.get_mut(&name).and_then(|p| p.log.as_mut()) {
          log.push(entry);
        }
      }
      Event::Entry {
        agent: None,
        id: Some(id),
        entry,
      } => {
        self.whole.insert(id, entry);
        self.parts.push((id, 1));
      }
      Event::Entry {
        agent: None,
        id: None,
        entry,
      } => {
        if entry.kind == Kind::Mind
          && let Some(&(id, n)) = self.parts.last()
        {
          self.minds.entry(id + n - 1).or_default().push(entry.text);
        }
      }
      Event::Delta {
        agent,
        kind,
        text,
        fresh,
      } => {
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
      Event::Assistant(_) | Event::Gone(_) => {}
    }
    then
  }

  pub fn act(&mut self, act: Act) -> Then {
    let mut then = Then::default();
    match act {
      Act::Send { text, images } => {
        let text = text.trim().to_string();
        if !text.is_empty() || !images.is_empty() {
          self.page = None;
          self.fold = None;
          then.cmds.push(Command::Say {
            text,
            images,
            via: None,
          });
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
        // the overview is the chat's: shown, it shows on the chat's tab
        if on {
          self.page = None;
        }
        self.open.clear();
        self.fold = on.then_some(Fold::Asked);
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
      Act::Home => {
        self.page = None;
        if self.fold.take().is_some() {
          self.open.clear();
        }
      }
    }
    then
  }

  /// All the chat shown asks its engine anew, as events were lost or asks
  /// failed: the view, the overview, the blocks opened, the agent's page
  /// shown (the rest are dropped, asked for once shown). What is known
  /// stays till the replies tell it anew.
  pub fn resync(&mut self) -> Then {
    let mut then = Then::default();
    then.cmds.push(Command::Attach);
    if self.fold.is_some() {
      then.cmds.push(Command::Root);
    }
    let mut opened: Vec<Seg> = self
      .open
      .iter()
      .filter(|&(seg, &on)| on && seg.1 > 1)
      .map(|(&seg, _)| seg)
      .collect();
    opened.sort_unstable();
    then.cmds.extend(opened.into_iter().map(Command::Zoom));
    let page = self.page.clone();
    self.pages.retain(|name, _| Some(name) == page.as_ref());
    if let Some(name) = page.filter(|name| self.pages.contains_key(name)) {
      then.cmds.push(Command::Agent(name));
    }
    then
  }

  /// What is looked at: the agent's page, or the chat; whether the overview.
  pub fn looking(&self) -> (Option<String>, bool) {
    (self.page.clone(), self.fold.is_some())
  }

  fn live_of(&mut self, agent: Option<&str>) -> Option<&mut Vec<Live>> {
    match agent {
      None => Some(&mut self.live),
      Some(name) => self.pages.get_mut(name).map(|p| &mut p.live),
    }
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

  // Rows
  // ----

  /// A chat's model, empty, opening with `head`.
  pub fn headed(head: String) -> Model {
    Model {
      head: Some(head),
      ..Model::default()
    }
  }

  /// The page shown, as items (each a part's rows, or an agent's entry).
  pub fn body(&self) -> Vec<Vec<Row>> {
    if let Some(name) = &self.page {
      let page = self.pages.get(name);
      let at = if self.agents().contains(name) {
        "at work"
      } else {
        "done"
      };
      let head = vec![Row::Cap(format!("{name} · {at}"))];
      let log = page
        .and_then(|p| p.log.as_ref())
        .into_iter()
        .flatten()
        .map(|e| entry_rows(e.kind, &e.text, &e.images));
      return std::iter::once(head)
        .chain(log)
        .filter(|rows| !rows.is_empty())
        .collect();
    }
    if let Some(fold) = &self.fold {
      let cap = std::iter::once(vec![Row::Cap(FOLD.into())]);
      return match fold {
        Fold::Asked => cap.chain(std::iter::once(vec![Row::Cap(FOLDING.into())])).collect(),
        Fold::Tops(tops) => cap
          .chain(tops.iter().map(|&(id, n)| self.part_rows(id, n, u64::MAX, false)))
          .collect(),
      };
    }
    let head = self.head.iter().map(|h| vec![Row::Cap(h.clone())]);
    head
      .chain(self.parts.iter().map(|&(id, n)| self.part_rows(id, n, self.turn, true)))
      .collect()
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
      Wait::Model | Wait::None => {
        let model = self.state.as_ref().map(|s| s.model).unwrap_or_default();
        format!("waiting for {}…", model.name())
      }
    };
    std::iter::once(Row::Cap(cap)).chain(load).collect()
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
    let text = if n == 1 {
      "collapse".to_string()
    } else {
      format!("▾ {n} messages · collapse")
    };
    let rows = if n > 1 {
      let half = n / 2;
      let mut rows = self.part_rows(id, half, t, talk);
      rows.extend(self.part_rows(id + half, half, t, talk));
      rows
    } else {
      let mut rows = entry
        .map(|e| entry_rows(e.kind, &e.text, &e.images))
        .unwrap_or_default();
      rows.extend(entry.and_then(|e| e.spend).filter(|s| s.n > 0).map(Row::Use));
      rows
    };
    vec![Row::Box {
      seg: (id, n),
      text,
      rows,
    }]
  }

  /// A block; one whose summary isn't known yet says WAIT. A line of one
  /// message that is a tool call word for word shows as the call.
  fn sum_row(&self, id: u64, n: u64) -> Row {
    let text = self.nodes.get(&(id, n)).map_or(WAIT, |node| node.text.as_str());
    let call = (n == 1)
      .then(|| text.strip_prefix("tool: ").map(call::parse))
      .flatten()
      .filter(|c| !c.name.is_empty());
    Row::Sum {
      seg: (id, n),
      text: text::flat(text),
      call,
    }
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
  vec![Row::Msg {
    kind,
    who,
    text,
    call,
    images: images.to_vec(),
  }]
}

/// An import's progress.
pub fn progress_show(p: &Progress) -> String {
  let merges: u64 = p.open.iter().sum();
  let phase = if p.open.first().is_some_and(|&n| n > 0) {
    format!(
      "Summarizing messages: {} of {}",
      text::grouped(p.msgs - p.open[0]),
      text::grouped(p.msgs)
    )
  } else {
    let minutes = ((p.total - p.done) * PACE).ceil();
    format!(
      "Merging the top of the tree: {merges} {} left · about {minutes} min",
      if merges == 1 { "merge" } else { "merges" }
    )
  };
  if p.why.is_some() {
    format!("{phase} · paused")
  } else {
    phase
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn entry(kind: Kind, text: &str) -> Entry {
    Entry {
      kind,
      text: text.into(),
      date: "2026-10-26T10:00:00.000Z".into(),
      images: vec![],
      spend: None,
      via: None,
    }
  }

  #[test]
  fn the_chat_shows_blocks_then_the_turn_whole() {
    let mut m = Model::default();
    m.hear(Event::Know {
      whole: vec![
        (0, entry(Kind::User, "hi")),
        (1, entry(Kind::Tool, "bash {\"command\":\"ls\"}")),
      ],
      nodes: vec![],
    });
    m.hear(Event::View {
      parts: vec![(0, 1), (1, 1)],
      turn: 1,
    });
    let body = m.body();
    assert!(
      matches!(&body[0][0], Row::Box { .. }),
      "a user's message before the turn opens"
    );
    assert!(matches!(&body[1][0], Row::Msg { call: Some(c), .. } if c.name == "bash"));
  }

  /// Words are the one act: sent at work they go as any (the engine cuts
  /// in), from an agent's page or the overview they bring the chat back,
  /// and nothing at all sends nothing.
  #[test]
  fn a_message_is_the_only_act() {
    let mut m = Model::default();
    m.hear(Event::State(State {
      busy: true,
      wait: Wait::None,
      model: acorn_core::model::Model::Neon,
      agents: vec!["FireAnt".into()],
      pins: vec!["the spec".into()],
      importing: None,
      today: None,
    }));
    assert!(
      m.act(Act::Send {
        text: " ".into(),
        images: vec![]
      })
      .cmds
      .is_empty(),
      "an empty Enter stops nothing"
    );
    m.act(Act::Goto(Some("FireAnt".into())));
    m.act(Act::Fold(true));
    assert!(matches!(
      m.act(Act::Send {
        text: "stop".into(),
        images: vec![]
      })
      .cmds[..],
      [Command::Say { .. }]
    ));
    assert_eq!(
      (m.page.as_deref(), m.fold.is_some()),
      (None, false),
      "back to the chat, where the reply comes"
    );
    assert!(
      matches!(
        m.act(Act::Send {
          text: "/goal ship it".into(),
          images: vec![]
        })
        .cmds[..],
        [Command::Say { .. }]
      ),
      "no slash words: words"
    );
  }

  /// The overview says it is asked for till its tops come, a late one
  /// unasked stays unshown, and Home leaves a page and the overview at
  /// once (at home, nothing changes).
  #[test]
  fn the_overview_waits_and_home_leaves_it() {
    let mut m = Model::default();
    m.act(Act::Goto(Some("FireAnt".into())));
    let on = m.act(Act::Fold(true));
    assert!(matches!(on.cmds[..], [Command::Root]));
    assert_eq!((m.page.as_deref(), m.fold.clone()), (None, Some(Fold::Asked)));
    assert_eq!(m.body()[1], vec![Row::Cap(FOLDING.into())], "asked for, not empty");
    m.hear(Event::Root { parts: vec![(0, 2)] });
    assert_eq!(m.fold, Some(Fold::Tops(vec![(0, 2)])));
    let mut late = Model::default();
    late.hear(Event::Root { parts: vec![(0, 2)] });
    assert_eq!(late.fold, None);
    m.act(Act::Open((0, 2)));
    m.act(Act::Goto(Some("FireAnt".into())));
    m.act(Act::Home);
    assert_eq!((m.looking(), m.open.len()), ((None, false), 0));
    assert!(m.act(Act::Home).cmds.is_empty());
  }

  /// Resynced, a chat asks for all it shows; the pages not shown go.
  #[test]
  fn a_chat_resynced_asks_for_all_it_shows() {
    let mut m = Model::default();
    m.act(Act::Open((4, 1)));
    m.act(Act::Open((8, 4)));
    m.act(Act::Open((0, 4)));
    m.act(Act::Shut((12, 2)));
    m.act(Act::Goto(Some("RedAnt".into())));
    m.hear(Event::Agent {
      name: "RedAnt".into(),
      log: Some(vec![entry(Kind::Work, "hi")]),
    });
    m.act(Act::Goto(Some("BlueOwl".into())));
    let cmds: Vec<String> = m.resync().cmds.iter().map(|c| format!("{c:?}")).collect();
    assert_eq!(cmds, ["Attach", "Zoom((0, 4))", "Zoom((8, 4))", "Agent(\"BlueOwl\")"]);
    assert_eq!(m.pages.keys().collect::<Vec<_>>(), ["BlueOwl"]);
    m.act(Act::Fold(true));
    assert!(
      matches!(m.resync().cmds[..], [Command::Attach, Command::Root]),
      "folding shut the blocks"
    );
  }
}
