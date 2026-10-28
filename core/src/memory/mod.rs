//! Memory
//! ======
//! The chat, kept forever, and the view of it every call sees. Sizes are
//! bytes of UTF-8. A message is logged in pages (`clip::CAP` characters,
//! PICS images).
//!
//! Over the messages grows a binary tree: node (0,i) is message i (or the
//! line it was logged with) in at most NODE bytes, node (l,i) merges
//! (l-1,2i) and (l-1,2i+1). A source that fits is its node; else a
//! compaction writes it (`compactor`). Nodes of imported messages are a
//! side of their own (side 1; the chat's turns don't wait on them), held
//! as the user says (`hold`: a pause).
//!
//! A view lists nodes covering the chat to its end, oldest first; each
//! message it reaches appends its node. Past `high` bytes, a batch merges
//! the adjacent siblings whose parent is built and most due (oldest since
//! its last message, to the view's end, in halves: detail fades with age)
//! down to half, so a view changes only at its end between batches. Two,
//! each on its own: the chat's (turns'), to the last message, and the
//! compactions' (`seen`), to the first not built of the sides that build.
//! A call gets one in BLOCK-line blocks, the last whole one marked for the
//! cache (`ai`).
//!
//! Files: chat.db (SQLite, WAL) holds the messages (`msgs`, log ''), each
//! agent's log (`msgs`, log its name), the nodes by slot (`nodes`) and the
//! views, the chat's goal and work (`state`, written over). Pages
//! `more` after the last that isn't are not in the chat. Memory holds only
//! the views' nodes and those built above them; any other row is read as
//! needed. Images are in docs/images/ by their hash.
//!
//! A purge erases a message for good (a secret pasted in): at once, its
//! text and images and every node built from it, so no call sees it again;
//! its node and those above build anew without it. Other nodes built while
//! its summary was in the compactions' view may still echo it. Its old
//! bytes stay in chat.db's pages till a wipe rewrites the file (`erase`),
//! owed in a marker file (WIPE) till done, and run by the chat once idle,
//! as it holds up every write while it runs. Copies the disk itself keeps
//! (APFS snapshots, the SSD) are beyond Acorn's reach.

mod images;
mod purge;
mod view;

pub use images::{IMAGES_ARE, image_add, image_for_model, image_kind};
pub use purge::{erase, wipe};

use crate::clip;
use crate::{Img, Kind, Served, config, disk, names};
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

// Constants
// =========

pub const NODE: usize = 512;
pub const JOBS: usize = 64;
/// The compactions' view merges past this (16-32 KB).
const SEEN: usize = 32_000;
const BLOCK: usize = 4;

/// A request's images: 20 at full size, each IMAGE bytes at most; PIXELS,
/// the most decoded: a larger image goes as is.
pub const PICS: usize = 20;
const FIT: f64 = 1568.0;
const IMAGE: usize = 1 << 20;
const PIXELS: u64 = 1 << 28;

pub const WAIT: &str = "(not summarized yet: zoom it)";
/// A purged message's text.
pub const PURGED: &str = "(purged)";
const IMAGES: &str = "docs/images";
/// The chat's log in `msgs`; an agent's is its name.
const CHAT: &str = "";
/// A wipe owed: this file in the chat's dir.
const WIPE: &str = "chat.db.wipe";

const SCHEMA: &str = "pragma journal_mode = wal; pragma synchronous = normal; pragma secure_delete = on;
create table if not exists msgs (log text not null, i integer not null, kind text not null, text text not null, date text not null,
    images text not null, imported integer not null, line text, more integer not null, size integer not null, primary key (log, i)) without rowid;
create table if not exists nodes (slot integer primary key, text text not null);
create table if not exists agents (name text primary key) without rowid;
create table if not exists state (id integer primary key check (id = 1), json text not null);";

/// A compaction's task, before its <input>: one message to compress
/// (level 0), or two lines to merge. The ruler, not a sample line: the
/// compactor copied a sample's content.
fn leaf_task(id: u64) -> String {
  format!(
    "Compaction: compress message {id} into one line of at most {NODE} bytes\n(about 70 words), the length of this ruler:\n{}\n<input>\n",
    "-".repeat(NODE)
  )
}

fn merge_task(a: &str, b: &str, id: u64, end: u64) -> String {
  format!(
    "Compaction: merge lines {a} and {b}, adjacent, into one line of at most\n{NODE} bytes (about 70 words), the length of this ruler:\n{}\n<chat> may hold their messages, {id} to {end}, in more detail: take details\nof them from there too.\n<input>\n",
    "-".repeat(NODE)
  )
}

// Types
// =====

/// Node (l,i): the 2^l messages from i·2^l on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Part {
  pub l: u32,
  pub i: u64,
}

impl Part {
  pub const fn new(l: u32, i: u64) -> Part {
    Part { l, i }
  }

  pub fn leaf(i: u64) -> Part {
    Part { l: 0, i }
  }

  pub fn n(self) -> u64 {
    1 << self.l
  }

  /// Its key: nodes in order, each parent between its halves.
  pub fn slot(self) -> u64 {
    (2 * self.i + 1) * self.n() - 1
  }

  /// Its first and last message.
  pub fn span(self) -> (u64, u64) {
    let a = self.i * self.n();
    (a, a + self.n() - 1)
  }

  /// The messages it waits on: a leaf its own, a merge all of its.
  pub fn ctx(self) -> u64 {
    if self.l == 0 { self.i } else { (self.i + 1) * self.n() }
  }

  pub fn halves(self) -> Option<[Part; 2]> {
    (self.l > 0).then(|| [Part::new(self.l - 1, 2 * self.i), Part::new(self.l - 1, 2 * self.i + 1)])
  }

  pub fn up(self) -> Part {
    Part::new(self.l + 1, self.i >> 1)
  }

  /// Its zoom target, "id+n".
  pub fn name(self) -> String {
    format!("{}+{}", self.span().0, self.n())
  }

  /// The line of the n messages from id (n a power of two dividing id).
  pub fn of(id: u64, n: u64) -> Option<Part> {
    (n.is_power_of_two() && id.is_multiple_of(n)).then(|| Part::new(n.trailing_zeros(), id / n))
  }
}

/// A node and its text.
pub type Built = (Part, String);

/// A message (a page) as logged: its kind, text, date (ISO), images,
/// whether imported, its node (if written with it), whether it is logged
/// as one with the next, its index and size as models read it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Root {
  pub kind: Kind,
  pub text: String,
  pub date: String,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub images: Vec<String>,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub imported: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub line: Option<String>,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub more: bool,
  pub i: u64,
  pub size: usize,
}

impl Root {
  /// As models read it, tagged by its author: an imported chat's AI (its
  /// say and calls) is "ai", not Acorn.
  pub fn show(&self) -> String {
    let who = if self.imported && matches!(self.kind, Kind::Acorn | Kind::Tool) {
      "ai"
    } else {
      self.kind.as_str()
    };
    let pics: String = self
      .images
      .iter()
      .map(|name| format!("\n[image docs/images/{name}]"))
      .collect();
    format!("{who}: {}{pics}", self.text)
  }

  /// Its date and time here, as "2026-10-26 14:03:05".
  pub fn when(&self) -> String {
    local_time(&self.date)
  }
}

/// A message to log: a Root but its index and size; no date: now.
#[derive(Clone, Debug)]
pub struct Msg {
  pub kind: Kind,
  pub text: String,
  pub date: Option<String>,
  pub images: Vec<String>,
  pub imported: bool,
  pub line: Option<String>,
  pub more: bool,
}

impl Msg {
  pub fn new(kind: Kind, text: impl Into<String>) -> Msg {
    Msg {
      kind,
      text: text.into(),
      date: None,
      images: Vec::new(),
      imported: false,
      line: None,
      more: false,
    }
  }
}

/// A view: its lines, the bytes of those built, a batch under way, the
/// bytes past which it merges to half.
#[derive(Clone, Debug, Default)]
pub struct View {
  pub parts: Vec<Part>,
  pub size: usize,
  pub batch: bool,
  pub high: usize,
}

impl View {
  /// The message after its last line.
  pub fn end(&self) -> u64 {
    self.parts.last().map_or(0, |p| p.span().1 + 1)
  }

  /// The index of its line holding message id (past its end: its length).
  pub fn cover(&self, id: u64) -> usize {
    self.parts.partition_point(|p| p.span().1 < id)
  }
}

#[derive(Serialize, Deserialize, Default)]
struct Kept {
  parts: Vec<(u32, u64)>,
  #[serde(default)]
  batch: bool,
}

/// The state row: the chat's view, the compactions', the goal and work.
#[derive(Serialize, Deserialize, Default)]
struct Saved {
  view: Kept,
  seen: Kept,
  #[serde(default)]
  goal: Option<String>,
  #[serde(default)]
  work: Vec<String>,
}

/// The chat's memory. db: chat.db, shared with `chat::usage`; count: the
/// pages logged whole; open: those after, not whole yet; goal: the
/// user's, till met or dropped; work: what ran, as last saved; nodes: the
/// texts of the views' nodes built and of those above them, by slot;
/// agents: their logs' lengths; ready: the nodes not built whose halves
/// are, by side and level, in order; busy: nodes building, by slot, with
/// their side; sides: the side by the last message of nodes not built;
/// hold: why each side is held (None: it builds); made: nodes built since
/// last drained; fitted: the views were fit since last drained; age: the
/// chat's purges so far (a node a call began before one is not stored);
/// purged, wiped: the purges so far, those a wipe has erased from the file
/// (owed: purged > wiped); wiping: a wipe runs, or failed (it runs again
/// when Acorn starts: `stuck`).
pub struct Memory {
  pub dir: PathBuf,
  pub db: Connection,
  pub count: u64,
  pub open: u64,
  pub goal: Option<String>,
  pub work: Vec<String>,
  nodes: HashMap<u64, String>,
  agents: HashMap<String, u64>,
  pub view: View,
  pub seen: View,
  ready: [Vec<Vec<u64>>; 2],
  pub busy: HashMap<u64, usize>,
  sides: HashMap<u64, usize>,
  pub hold: [Option<String>; 2],
  pub done: bool,
  pub made: Vec<Part>,
  pub fitted: bool,
  pub age: u64,
  pub purged: u64,
  pub wiped: u64,
  pub wiping: bool,
  pub stuck: bool,
}

// Memory
// ======

impl Memory {
  pub fn load(dir: &Path) -> Result<Memory> {
    fs::create_dir_all(dir)?;
    let db = Connection::open(dir.join("chat.db"))?;
    // only a wipe holds it up (no other process opens it): wait it out
    db.busy_timeout(std::time::Duration::from_secs(600))?;
    db.execute_batch(SCHEMA)?;
    let saved = saved(&db);
    let mut mem = Memory {
      dir: dir.to_path_buf(),
      db,
      count: 0,
      open: 0,
      goal: None,
      work: Vec::new(),
      nodes: HashMap::new(),
      agents: HashMap::new(),
      view: View {
        high: config::get().view,
        ..View::default()
      },
      seen: View {
        high: SEEN,
        ..View::default()
      },
      ready: [Vec::new(), Vec::new()],
      busy: HashMap::new(),
      sides: HashMap::new(),
      hold: [None, Some("paused".into())],
      done: false,
      made: Vec::new(),
      fitted: false,
      age: 0,
      purged: u64::from(dir.join(WIPE).exists()),
      wiped: 0,
      wiping: false,
      stuck: false,
    };
    if let Some(s) = &saved {
      mem.goal.clone_from(&s.goal);
      mem.work.clone_from(&s.work);
    }
    mem.count = mem
      .db
      .query_row("select coalesce(max(i) + 1, 0) from msgs where log = ''", [], |r| {
        r.get(0)
      })?;
    while mem.count > 0 && mem.message(mem.count - 1).is_some_and(|m| m.more) {
      mem.count -= 1;
    }
    mem
      .db
      .execute("delete from msgs where log = '' and i >= ?", [mem.count])?;
    mem.view = mem.opened(mem.view.high, saved.as_ref().map(|s| &s.view));
    mem.seen = mem.opened(SEEN, saved.as_ref().map(|s| &s.seen));
    for part in roots(mem.count)
      .into_iter()
      .flat_map(|p| mem.todo(p))
      .collect::<Vec<_>>()
    {
      mem.ready(part, true);
    }
    mem.fit(false);
    mem.save()?;
    Ok(mem)
  }

  /// A view as kept (none: the highest nodes built).
  fn opened(&mut self, high: usize, kept: Option<&Kept>) -> View {
    let keys: Option<HashSet<u64>> = kept.map(|k| k.parts.iter().map(|&(l, i)| Part::new(l, i).slot()).collect());
    let parts: Vec<Part> = roots(self.count)
      .into_iter()
      .flat_map(|p| self.down(p, keys.as_ref()))
      .collect();
    let size = parts.iter().map(|&p| self.size(p)).sum();
    View {
      parts,
      size,
      batch: kept.is_some_and(|k| k.batch),
      high,
    }
  }

  /// The state row, written over.
  pub fn save(&self) -> Result<()> {
    let kept = |v: &View| Kept {
      parts: v.parts.iter().map(|p| (p.l, p.i)).collect(),
      batch: v.batch,
    };
    let saved = Saved {
      view: kept(&self.view),
      seen: kept(&self.seen),
      goal: self.goal.clone(),
      work: self.work.clone(),
    };
    self.db.execute(
      "insert or replace into state values (1, ?)",
      [serde_json::to_string(&saved)?],
    )?;
    Ok(())
  }

  // Logs
  // ----

  /// Message i of a log (CHAT, or an agent's); None: none.
  fn read(&self, log: &str, i: u64) -> Option<Root> {
    let mut q = self
      .db
      .prepare_cached("select * from msgs where log = ? and i = ?")
      .ok()?;
    q.query_row(params![log, i], from_row).optional().unwrap_or_else(|err| {
      eprintln!("memory: message {i}: {err}");
      None
    })
  }

  /// Messages appended to a log, in one transaction.
  fn log(&mut self, log: &str, roots: &[Root]) -> Result<()> {
    let tx = self.db.transaction()?;
    {
      let mut q = tx.prepare_cached("insert into msgs values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")?;
      for r in roots {
        let images = serde_json::to_string(&r.images)?;
        q.execute(params![
          log,
          r.i,
          r.kind.as_str(),
          r.text,
          r.date,
          images,
          r.imported,
          r.line,
          r.more,
          r.size
        ])?;
      }
    }
    tx.commit()?;
    Ok(())
  }

  // Messages
  // --------

  /// Message i; None: none.
  pub fn message(&self, i: u64) -> Option<Root> {
    if i < self.count { self.read(CHAT, i) } else { None }
  }

  /// The pages logged; those `more` stay open, past the count, till one
  /// that isn't. The caller pumps the compactor.
  pub fn push(&mut self, msgs: Vec<Msg>) -> Result<Vec<Root>> {
    let base = self.count + self.open;
    let roots: Vec<Root> = msgs
      .into_iter()
      .flat_map(pages)
      .enumerate()
      .map(|(k, m)| root(base + k as u64, m))
      .collect();
    let done = roots.iter().rposition(|r| !r.more).map_or(0, |k| k + 1);
    let ids = if done == 0 { 0 } else { self.open + done as u64 };
    self.log(CHAT, &roots)?;
    self.open = self.open + roots.len() as u64 - ids;
    let from = self.count;
    self.count += ids;
    for i in from..self.count {
      self.ready(Part::leaf(i), true);
    }
    self.fit(true);
    Ok(roots)
  }

  /// The first message, from `from` on, from which those to the end go
  /// whole in a call: the view's HIGH bytes, PICS images (the last
  /// always).
  pub fn whole(&self, from: u64) -> u64 {
    let high = config::get().view;
    let (mut size, mut pics) = (0, 0);
    let mut k = self.count;
    while k > from {
      let Some(root) = self.message(k - 1) else { break };
      size += root.size;
      pics += root.images.len();
      if k < self.count && (size > high || pics > PICS) {
        return k;
      }
      k -= 1;
    }
    k
  }

  /// The pages [a, b) of a message as a model gets them: one text, their
  /// images.
  pub fn say(&self, a: u64, b: u64) -> crate::Say {
    let mut say = crate::Say::default();
    for id in a..b {
      if let Some(root) = self.message(id) {
        say.text.push_str(&root.text);
      }
      say.imgs.extend(self.images(id));
    }
    say
  }

  // Tree
  // ----

  pub fn built(&self, part: Part) -> bool {
    self.nodes.contains_key(&part.slot())
  }

  /// Node (l,i)'s text, kept or from chat.db; None: not built.
  pub fn node(&self, part: Part) -> Option<String> {
    if let Some(text) = self.nodes.get(&part.slot()) {
      return Some(text.clone());
    }
    let mut q = self.db.prepare_cached("select text from nodes where slot = ?").ok()?;
    q.query_row([part.slot()], |r| r.get(0))
      .optional()
      .unwrap_or_else(|err| {
        eprintln!("memory: node {}/{}: {err}", part.l, part.i);
        None
      })
  }

  /// The view under `part`, down from it, keeping the built nodes met:
  /// the nodes of `keys` built (none: any built), or messages.
  fn down(&mut self, part: Part, keys: Option<&HashSet<u64>>) -> Vec<Part> {
    let text = self.node(part);
    let built = text.is_some();
    if let Some(text) = text {
      self.nodes.insert(part.slot(), text);
    }
    match part.halves() {
      Some(halves) if !(built && keys.is_none_or(|k| k.contains(&part.slot()))) => {
        halves.into_iter().flat_map(|h| self.down(h, keys)).collect()
      }
      _ => vec![part],
    }
  }

  /// What a node is made of: its message (its line, if logged with one),
  /// or its halves' lines; None: not all built.
  pub fn source(&self, part: Part) -> Option<String> {
    match part.halves() {
      None => self.message(part.i).map(|r| r.line.clone().unwrap_or_else(|| r.show())),
      Some([a, b]) => Some(format!("{}\n{}", flat(&self.node(a)?), flat(&self.node(b)?))),
    }
  }

  /// The message of the (k+1)th node not built of a side (0: the chat's,
  /// 1: an import's) at level 0, else the count.
  pub fn first(&self, k: usize, side: usize) -> u64 {
    self.ready[side]
      .first()
      .and_then(|q| q.get(k))
      .copied()
      .unwrap_or(self.count)
  }

  /// A node ready (on), or no more (built).
  fn ready(&mut self, part: Part, on: bool) {
    let side = self.side(part);
    let levels = &mut self.ready[side];
    if levels.len() <= part.l as usize {
      levels.resize(part.l as usize + 1, Vec::new());
    }
    let queue = &mut levels[part.l as usize];
    let at = queue.partition_point(|&i| i < part.i);
    let here = queue.get(at) == Some(&part.i);
    match (on, here) {
      (true, false) => queue.insert(at, part.i),
      (false, true) => {
        queue.remove(at);
      }
      _ => {}
    }
  }

  /// A node's side: 1 if its last message is imported, else 0.
  pub fn side(&mut self, part: Part) -> usize {
    let last = part.span().1;
    if let Some(&side) = self.sides.get(&last) {
      return side;
    }
    let side = usize::from(self.message(last).is_some_and(|m| m.imported));
    self.sides.insert(last, side);
    side
  }

  /// What node (l,i)'s summary serves: an import, or the chat.
  pub fn served(&mut self, part: Part) -> Served {
    if self.side(part) == 1 {
      Served::Import
    } else {
      Served::Compactor
    }
  }

  /// Of each side not held, the nodes ready up to its JOBS-th leaf not
  /// built, in the order the messages came (`ctx`; at a tie, a merge
  /// first), while JOBS run: those whose source fits (built at once, by
  /// the caller), and those that take a call (marked busy; the caller
  /// starts them).
  pub fn starts(&mut self) -> (Vec<Built>, Vec<Built>) {
    let (mut free, mut jobs) = (Vec::new(), Vec::new());
    for side in [0, 1] {
      if self.hold[side].is_some() {
        continue;
      }
      let end = self.first(JOBS - 1, side);
      let mut next: Vec<Part> = Vec::new();
      for (l, queue) in self.ready[side].iter().enumerate() {
        next.extend(
          queue
            .iter()
            .map(|&i| Part::new(l as u32, i))
            .take_while(|p| p.ctx() <= end),
        );
      }
      next.sort_by(|a, b| a.ctx().cmp(&b.ctx()).then(b.l.cmp(&a.l)));
      for part in next {
        if self.done || self.running(side) >= JOBS {
          break;
        }
        if self.busy.contains_key(&part.slot()) {
          continue;
        }
        match self.source(part) {
          Some(text) if text.len() <= NODE => free.push((part, text)),
          Some(text) => {
            self.busy.insert(part.slot(), side);
            jobs.push((part, text));
          }
          None => {}
        }
      }
    }
    (free, jobs)
  }

  /// The nodes under `part` not built whose sources are: only nodes not
  /// built and their halves are visited.
  fn todo(&self, part: Part) -> Vec<Part> {
    if self.built(part) {
      return Vec::new();
    }
    match part.halves() {
      None => vec![part],
      Some(halves) => {
        let here = halves.iter().all(|&h| self.built(h));
        let mut out = if here { vec![part] } else { Vec::new() };
        out.extend(halves.into_iter().flat_map(|h| self.todo(h)));
        out
      }
    }
  }

  /// How many nodes of a side build.
  pub fn running(&self, side: usize) -> usize {
    self.busy.values().filter(|&&s| s == side).count()
  }

  /// Nodes built: logged in one write, kept.
  pub fn store(&mut self, parts: &[(Part, String)]) -> Result<()> {
    let tx = self.db.transaction()?;
    {
      let mut q = tx.prepare_cached("insert or replace into nodes values (?, ?)")?;
      for (part, text) in parts {
        q.execute(params![part.slot(), text])?;
      }
    }
    tx.commit()?;
    for (part, text) in parts {
      self.ready(*part, false);
      self.nodes.insert(part.slot(), text.clone());
      self.sides.remove(&part.span().1);
      for view in [&mut self.view, &mut self.seen] {
        if part.l == 0 && part.i < view.end() {
          view.size += text.len();
        }
      }
    }
    for (part, _) in parts {
      let sibling = Part::new(part.l, part.i ^ 1);
      if self.built(sibling) && !self.built(part.up()) {
        self.ready(part.up(), true);
      }
    }
    self.made.extend(parts.iter().map(|(p, _)| *p));
    Ok(())
  }

  /// Node (l,i)'s task, but its input.
  pub fn task(part: Part) -> String {
    let (a, z) = part.span();
    if part.l == 0 {
      return leaf_task(a);
    }
    let n = part.n() / 2;
    merge_task(&format!("{a}+{n}"), &format!("{}+{n}", a + n), a, z)
  }

  // Agents
  // ------

  /// An agent's log, read whole; None: no such agent.
  pub fn agent(&self, name: &str) -> Option<Vec<Root>> {
    let known: bool = self
      .db
      .query_row("select exists (select 1 from agents where name = ?)", [name], |r| {
        r.get(0)
      })
      .ok()?;
    if !known {
      return None;
    }
    let mut q = self
      .db
      .prepare_cached("select * from msgs where log = ? order by i")
      .ok()?;
    let rows = q.query_map([name], from_row).ok()?;
    Some(rows.filter_map(Result::ok).collect())
  }

  /// The agents' names, in order.
  pub fn agent_names(&self) -> Vec<String> {
    let names = self
      .db
      .prepare("select name from agents order by name")
      .and_then(|mut q| q.query_map([], |r| r.get(0))?.collect());
    names.unwrap_or_default()
  }

  pub fn agent_new(&mut self) -> Result<String> {
    loop {
      let name = names::random();
      if self.db.execute("insert or ignore into agents values (?)", [&name])? == 1 {
        self.agents.insert(name.clone(), 0);
        return Ok(name);
      }
    }
  }

  /// A message to an agent's log: its length read once, then counted.
  pub fn agent_push(&mut self, name: &str, kind: Kind, text: &str) -> Result<u64> {
    let n = match self.agents.get(name) {
      Some(&n) => n,
      None => self
        .db
        .query_row("select count(*) from msgs where log = ?", [name], |r| r.get(0))?,
    };
    self.log(name, &[root(n, Msg::new(kind, text))])?;
    self.agents.insert(name.to_string(), n + 1);
    Ok(n)
  }

  pub fn agent_show(&self, name: &str) -> String {
    match self.agent(name) {
      Some(log) => log
        .iter()
        .map(|r| format!("{}|{}", r.i, r.show()))
        .collect::<Vec<_>>()
        .join("\n\n"),
      None => format!("No agent {name}."),
    }
  }
}

// Helpers
// =======

/// The state row; None: none, or unreadable.
fn saved(db: &Connection) -> Option<Saved> {
  let json: String = db
    .query_row("select json from state where id = 1", [], |r| r.get(0))
    .optional()
    .ok()??;
  serde_json::from_str(&json).ok()
}

/// A message from its row in `msgs`.
fn from_row(r: &Row) -> rusqlite::Result<Root> {
  let bad =
    |k, e: serde_json::Error| rusqlite::Error::FromSqlConversionFailure(k, rusqlite::types::Type::Text, e.into());
  let kind = serde_json::from_value(serde_json::Value::String(r.get(2)?)).map_err(|e| bad(2, e))?;
  let images = serde_json::from_str(&r.get::<_, String>(5)?).map_err(|e| bad(5, e))?;
  Ok(Root {
    i: r.get(1)?,
    kind,
    text: r.get(3)?,
    date: r.get(4)?,
    images,
    imported: r.get(6)?,
    line: r.get(7)?,
    more: r.get(8)?,
    size: r.get(9)?,
  })
}

/// Now, as logged: "2026-10-26T12:03:05.123Z".
pub fn iso_now() -> String {
  Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// An ISO date here, as "2026-10-26 14:03:05" ("" if not one).
pub fn local_time(iso: &str) -> String {
  DateTime::parse_from_rfc3339(iso)
    .map(|d| d.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string())
    .unwrap_or_default()
}

fn root(i: u64, msg: Msg) -> Root {
  let mut root = Root {
    kind: msg.kind,
    text: msg.text,
    date: msg.date.unwrap_or_else(iso_now),
    images: msg.images,
    imported: msg.imported,
    line: msg.line,
    more: msg.more,
    i,
    size: 0,
  };
  root.size = root.show().len();
  root
}

/// A message in pages: its text and images on the last pages (a call takes
/// them whole), its line on the first.
fn pages(msg: Msg) -> Vec<Msg> {
  let texts = clip::pages(&msg.text);
  let n = texts.len().max(msg.images.len().div_ceil(PICS));
  (0..n)
    .map(|k| {
      let end = msg.images.len().saturating_sub((n - 1 - k) * PICS);
      let text = (k + texts.len())
        .checked_sub(n)
        .and_then(|t| texts.get(t))
        .cloned()
        .unwrap_or_default();
      Msg {
        kind: msg.kind,
        text,
        date: msg.date.clone(),
        images: msg.images[end.saturating_sub(PICS)..end].to_vec(),
        imported: msg.imported,
        line: if k == 0 { msg.line.clone() } else { None },
        more: k < n - 1 || msg.more,
      }
    })
    .collect()
}

/// The tree's roots: a node per power of two in the count, largest first.
pub fn roots(count: u64) -> Vec<Part> {
  let mut out = Vec::new();
  let mut id = 0;
  for l in (0..=40).rev() {
    if id + (1u64 << l) <= count {
      out.push(Part::new(l, id >> l));
      id += 1 << l;
    }
  }
  out
}

/// A text on one line: each run of whitespace holding a newline, a space.
pub fn flat(text: &str) -> String {
  if !text.contains('\n') {
    return text.to_string();
  }
  let mut out = String::with_capacity(text.len());
  let mut run = String::new();
  for c in text.chars() {
    if c.is_whitespace() {
      run.push(c);
      continue;
    }
    out.push_str(if run.contains('\n') { " " } else { &run });
    run.clear();
    out.push(c);
  }
  out.push_str(if run.contains('\n') { " " } else { &run });
  out
}

/// A text's first NODE bytes, whole characters.
pub fn cut(text: &str) -> &str {
  let mut end = text.len().min(NODE);
  while !text.is_char_boundary(end) {
    end -= 1;
  }
  &text[..end]
}

pub fn over(line: &str) -> String {
  format!(
    "Too long: your last line for this <input> was {} bytes,\nover the {NODE}-byte limit. Write the whole line again, cutting just\nenough of the least valuable items to fit before this cut:\n{}| ← LIMIT",
    line.len(),
    cut(line)
  )
}

pub fn line(part: Part, text: &str) -> String {
  format!("{}|{}", part.name(), flat(text))
}
