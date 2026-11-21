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
//! compactions' (`seen`), grown to the first missing leaf of the sides
//! that build. If that frontier retreats, calls read only its prefix.
//! A call gets one in BLOCK-line blocks, the last whole one marked for the
//! cache (`ai`).
//!
//! Files: chat.db (SQLite, WAL) holds the messages (`msgs`, log ''), each
//! agent's log (`msgs`, log its name) and effort (`agents`), the nodes by
//! slot (`nodes`), the views, the chat's goal and what ran (`state`,
//! written over), a full-text index of the messages' texts (`search`,
//! FTS5), kept by triggers on `msgs`, so no write can leave it behind, and
//! what each zoom and search found (`looks`: how many hits, when).
//! Pages `more` after the last that isn't are not in the chat. Memory
//! holds only the views' nodes and those built above them; any other row
//! is read as needed. Images are in docs/images/ by their hash.
//!
//! A purge erases a message (a secret pasted in): at once, its text and
//! images and every node built from it, so no call sees it again; its node
//! and those above build anew without it. Other nodes built while its
//! summary was in the compactions' view may still echo it. Its text and
//! images are held GRACE (`held`), so an unpurge can bring them back; then
//! a wipe deletes them and rewrites the file (`erase`), as old bytes stay
//! in chat.db's pages till then. The wipe is owed in a marker file (WIPE)
//! till done, and run by the chat once idle and nothing held is in its
//! GRACE, as it holds up every write while it runs. Copies the disk itself
//! keeps (APFS snapshots, the SSD) are beyond Acorn's reach.

mod images;
mod import;
mod purge;
mod view;

pub use images::{IMAGES_ARE, image_add, image_for_model, image_kind, is_image, media_add, media_kind};
pub use purge::{GRACE, erase, wipe};

use crate::clip;
use crate::effort::Effort;
use crate::{Img, Kind, Served, config, disk, names};
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
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
pub const CHAT: &str = "";
/// A wipe owed: this file in the chat's dir.
const WIPE: &str = "chat.db.wipe";

const SCHEMA: &str = "pragma journal_mode = wal; pragma synchronous = normal; pragma secure_delete = on;
create table if not exists msgs (log text not null, i integer not null, kind text not null, text text not null, date text not null,
    images text not null, imported integer not null, line text, more integer not null, size integer not null, primary key (log, i)) without rowid;
create table if not exists nodes (slot integer primary key, text text not null);
create table if not exists agents (name text primary key, effort text) without rowid;
create table if not exists state (id integer primary key check (id = 1), json text not null);
create table if not exists import_batch (id integer primary key check (id = 1), a integer not null, b integer not null,
    said integer not null, spent real not null, paused integer not null);
create table if not exists looks (at integer not null, tool text not null, hits integer not null);
create virtual table if not exists search using fts5 (log unindexed, i unindexed, text);
create trigger if not exists search_add after insert on msgs begin
    insert into search (log, i, text) values (new.log, new.i, new.text); end;
create trigger if not exists search_drop after delete on msgs begin
    delete from search where log = old.log and i = old.i; end;
create trigger if not exists search_change after update of text on msgs begin
    delete from search where log = old.log and i = old.i;
    insert into search (log, i, text) values (new.log, new.i, new.text); end;";
/// The most hits a search gives.
pub const HITS: usize = 50;

/// A compaction's task, before its <input>: one message to compress
/// (level 0), or two lines to merge. The ruler, not a sample line: the
/// compactor copied a sample's content.
fn leaf_task(id: u64) -> String {
  format!(
    "Compaction: compress message {id} into one line of at most {AIM} bytes\n(about 60 words), the length of this ruler:\n{}\n<input>\n",
    "-".repeat(AIM)
  )
}

fn merge_task(a: &str, b: &str, id: u64, end: u64) -> String {
  format!(
    "Compaction: merge lines {a} and {b}, adjacent, into one line of at most\n{AIM} bytes (about 60 words), the length of this ruler:\n{}\n<chat> may hold their messages, {id} to {end}, in more detail: take details\nof them from there too.\n<input>\n",
    "-".repeat(AIM)
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

  /// The node of a key.
  pub fn at(slot: u64) -> Part {
    let l = (slot + 1).trailing_zeros();
    Part::new(l, (slot + 1) >> (l + 1))
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

/// The unchanged lines before `end`, and the children needed if it cuts
/// through a line. Only the latter can be below the in-memory view.
pub(crate) fn prefix(parts: &[Part], end: u64) -> (&[Part], impl Iterator<Item = Part>) {
  let cut = parts.partition_point(|p| p.span().1 < end);
  let start = parts.get(cut).map_or(end, |p| p.span().0);
  (&parts[..cut], tiles(start, end))
}

#[derive(Serialize, Deserialize, Default)]
struct Kept {
  parts: Vec<(u32, u64)>,
  #[serde(default)]
  batch: bool,
}

impl Kept {
  fn end(&self) -> u64 {
    self.parts.last().map_or(0, |&(l, i)| Part::new(l, i).span().1 + 1)
  }
}

/// What ran: the chat's turn, the agents at work.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WorkAs")]
pub struct Work {
  pub turn: bool,
  pub agents: Vec<String>,
}

/// Work as saved: now, or before, as names, the turn among them as
/// "your turn" (or "Acorn's turn", before that).
#[derive(Deserialize)]
#[serde(untagged)]
enum WorkAs {
  Names(Vec<String>),
  Work {
    #[serde(default)]
    turn: bool,
    #[serde(default)]
    agents: Vec<String>,
  },
}

impl From<WorkAs> for Work {
  fn from(w: WorkAs) -> Work {
    match w {
      WorkAs::Work { turn, agents } => Work { turn, agents },
      WorkAs::Names(names) => {
        let (turns, agents): (Vec<String>, Vec<String>) =
          names.into_iter().partition(|n| n == "your turn" || n == "Acorn's turn");
        Work {
          turn: !turns.is_empty(),
          agents,
        }
      }
    }
  }
}

/// The state row: the chat's view, the compactions', the goal and work.
#[derive(Serialize, Deserialize, Default)]
struct Saved {
  view: Kept,
  seen: Kept,
  #[serde(default)]
  goal: Option<String>,
  #[serde(default)]
  work: Work,
}

/// The chat's memory. db: chat.db, shared with `chat::usage`; count: the
/// pages logged whole; open: those after, not whole yet; goal: the
/// user's, till met or dropped; work: what ran, as last saved; nodes: the
/// texts of the views' nodes built and of those above them, by slot;
/// agents: their logs' lengths; ready: the nodes not built whose halves
/// are, by side and level, in order; busy: nodes building, by slot, with
/// their side; sides: the side by the last message of nodes not built;
/// paused: the import's side is held by request; made: nodes built since
/// last drained; fitted: the views were fit since last drained; age: the
/// chat's purges so far (a node a call began before one is not stored);
/// purged, wiped: the purges so far, those a wipe has erased from the file
/// (owed: purged > wiped); wipe: where the wipe of those owed is.
pub struct Memory {
  pub dir: PathBuf,
  pub db: Connection,
  pub count: u64,
  pub open: u64,
  pub goal: Option<String>,
  pub work: Work,
  nodes: HashMap<u64, String>,
  agents: HashMap<String, u64>,
  pub view: View,
  pub seen: View,
  ready: [Vec<BTreeSet<u64>>; 2],
  pub busy: HashMap<u64, usize>,
  sides: HashMap<u64, usize>,
  pub paused: bool,
  pub done: bool,
  pub made: Vec<Part>,
  pub fitted: bool,
  pub age: u64,
  pub purged: u64,
  pub wiped: u64,
  pub wipe: Wipe,
}

/// A wipe of chat.db's purged bytes: none running, one running, or one
/// that failed (none runs again till Acorn starts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wipe {
  Idle,
  Running,
  Stuck,
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
    purge::tables(&db)?;
    let efforts: bool = db.query_row(
      "select exists (select 1 from pragma_table_info('agents') where name = 'effort')",
      [],
      |r| r.get(0),
    )?;
    if !efforts {
      db.execute("alter table agents add column effort text", [])?;
    }
    db.execute(
      "insert into search (log, i, text) select log, i, text from msgs where not exists (select 1 from search)",
      [],
    )?;
    let saved = saved(&db);
    let mut mem = Memory {
      dir: dir.to_path_buf(),
      db,
      count: 0,
      open: 0,
      goal: None,
      work: Work::default(),
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
      paused: false,
      done: false,
      made: Vec::new(),
      fitted: false,
      age: 0,
      purged: u64::from(dir.join(WIPE).exists()),
      wiped: 0,
      wipe: Wipe::Idle,
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
    mem.migrate_import()?;
    mem.view = mem.opened(mem.view.high, mem.count, saved.as_ref().map(|s| &s.view));
    let seen_end = saved.as_ref().map_or(mem.count, |s| s.seen.end().min(mem.count));
    mem.seen = mem.opened(SEEN, seen_end, saved.as_ref().map(|s| &s.seen));
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

  /// A view up to `end` as kept (none: the highest nodes built), caching
  /// every built node past `end` so later `reach` and `merge` see it.
  fn opened(&mut self, high: usize, end: u64, kept: Option<&Kept>) -> View {
    let keys: Option<HashSet<u64>> = kept.map(|k| k.parts.iter().map(|&(l, i)| Part::new(l, i).slot()).collect());
    let parts: Vec<Part> = roots(self.count)
      .into_iter()
      .flat_map(|p| self.down(p, keys.as_ref()))
      .filter(|p| p.span().1 < end)
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
    self.publish(ids);
    Ok(roots)
  }

  fn publish(&mut self, ids: u64) {
    let from = self.count;
    self.count += ids;
    for i in from..self.count {
      self.ready(Part::leaf(i), true);
    }
    self.fit(true);
  }

  /// The first message, from `from` on, from which those to the end go
  /// whole in a call: the view's HIGH bytes, PICS images (the last
  /// always).
  pub fn whole(&self, from: u64) -> u64 {
    self.whole_in(from, config::get().view)
  }

  /// As `whole`, in `high` bytes.
  pub fn whole_in(&self, from: u64, high: usize) -> u64 {
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
    q.query_row([part.slot()], |r| {
      let mut text: String = r.get(0)?;
      // Older compactors could keep an overlong line after their last try.
      text.truncate(cut(&text).len());
      Ok(text)
    })
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
      .and_then(|q| q.iter().nth(k))
      .copied()
      .unwrap_or(self.count)
  }

  /// A node ready (on), or no more (built).
  fn ready(&mut self, part: Part, on: bool) {
    let side = self.side(part);
    let levels = &mut self.ready[side];
    if levels.len() <= part.l as usize {
      levels.resize(part.l as usize + 1, BTreeSet::new());
    }
    let queue = &mut levels[part.l as usize];
    if on {
      queue.insert(part.i);
    } else {
      queue.remove(&part.i);
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
      if side == 1 && self.paused {
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
        q.execute(params![part.slot(), cut(text)])?;
      }
    }
    tx.commit()?;
    for (part, text) in parts {
      let text = cut(text);
      self.ready(*part, false);
      let old = self.nodes.insert(part.slot(), text.to_owned()).map_or(0, |s| s.len());
      self.sides.remove(&part.span().1);
      if part.l == 0 {
        for view in [&mut self.view, &mut self.seen] {
          if part.i < view.end() && view.parts[view.cover(part.i)] == *part {
            view.size = view.size + text.len() - old;
          }
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

  /// A new agent, at its effort (none: the run's).
  pub fn agent_new(&mut self, effort: Option<Effort>) -> Result<String> {
    loop {
      let name = names::random();
      let effort = effort.map(Effort::as_str);
      if self
        .db
        .execute("insert or ignore into agents values (?, ?)", params![name, effort])?
        == 1
      {
        self.agents.insert(name.clone(), 0);
        return Ok(name);
      }
    }
  }

  /// A zoom or search, logged with what it found (`looks`).
  pub fn look(&self, tool: &str, hits: usize) {
    let row = params![crate::now_ms(), tool, hits as i64];
    if let Err(err) = self.db.execute("insert into looks values (?, ?, ?)", row) {
      eprintln!("memory: looks: {err}");
    }
  }

  /// An agent's effort; None: the run's (or no such agent).
  pub fn agent_effort(&self, name: &str) -> Option<Effort> {
    let effort: Option<String> = self
      .db
      .query_row("select effort from agents where name = ?", [name], |r| r.get(0))
      .optional()
      .ok()??;
    Effort::parse(&effort?)
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
      Some(log) => shown(&log, usize::MAX),
      None => format!("No agent {name}."),
    }
  }

  /// An agent's log as its own calls read it: within half the view's HIGH
  /// bytes (`shown`), as it goes after the view the agent started from.
  pub fn agent_reads(&self, name: &str) -> String {
    shown(&self.agent(name).unwrap_or_default(), config::get().view / 2)
  }
}

// Helpers
// =======

/// An agent's log, read within `most` bytes: past them, its oldest
/// entries over NODE bytes are cut to their first NODE (zoom shows them
/// whole), but the first two (what it is, its task). They are cut by half
/// of `most` at a time, so what a call caches of the log holds till the
/// next half. Entries of NODE bytes or under always stay.
fn shown(log: &[Root], most: usize) -> String {
  let step = (most / 2).max(1);
  let size: usize = log.iter().map(|r| r.text.len()).sum();
  let mut over = size.saturating_sub(most).div_ceil(step) * step;
  let entry = |(k, r): (usize, &Root)| {
    if over == 0 || k < 2 || r.text.len() <= NODE {
      return format!("{}|{}", r.i, r.show());
    }
    over = over.saturating_sub(r.text.len() - NODE);
    format!(
      "{}|{}: {}\n[cut to fit: the first {NODE} of its {} bytes]",
      r.i,
      r.kind.as_str(),
      cut(&r.text),
      r.text.len()
    )
  };
  log.iter().enumerate().map(entry).collect::<Vec<_>>().join("\n\n")
}

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
  tiles(0, count).collect()
}

/// The fewest aligned tree nodes covering [start, end), in order.
pub(crate) fn tiles(mut start: u64, end: u64) -> impl Iterator<Item = Part> {
  std::iter::from_fn(move || {
    if start >= end {
      return None;
    }
    let level = start.trailing_zeros().min((end - start).ilog2());
    let part = Part::new(level, start >> level);
    start += part.n();
    Some(part)
  })
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
  clip(text, NODE)
}

/// A text's first `most` bytes, whole characters.
fn clip(text: &str, most: usize) -> &str {
  let mut end = text.len().min(most);
  while !text.is_char_boundary(end) {
    end -= 1;
  }
  &text[..end]
}

/// What a line written for NODE bytes comes out under them at: NODE less
/// what a model miscounts.
pub(crate) const AIM: usize = NODE * 7 / 8;

/// What a line over NODE bytes is answered, and the bytes it asks for
/// now: of the `asked` it was written for, fewer by as much as it ran
/// over (a model told its line's size alone cut too little, try after
/// try), by half at most (one far over, held to its measure, cut more
/// than it had to).
pub fn over(line: &str, asked: usize) -> (String, usize) {
  let most = (asked * AIM / line.len()).max(asked / 2);
  let said = format!(
    "Too long: your last line for this <input> was {} bytes, and the limit is\ntighter than it reads to you. Write the whole line again within {most} bytes,\nthe length of this ruler, cutting the least valuable items:\n{}\nYour last line, up to where it passes this ruler:\n{}| ← LIMIT",
    line.len(),
    "-".repeat(most),
    clip(line, most)
  );
  (said, most)
}

pub fn line(part: Part, text: &str) -> String {
  format!("{}|{}", part.name(), flat(text))
}

#[cfg(test)]
mod tests {
  use super::{Memory, Msg, Part, Work};
  use crate::Kind;

  #[test]
  fn compaction_tasks_leave_room_for_miscounting() {
    for task in [Memory::task(Part::leaf(3)), Memory::task(Part::new(1, 0))] {
      assert!(task.contains(&format!("{} bytes", super::AIM)));
      let ruler = task.lines().find(|line| line.starts_with('-')).unwrap();
      assert_eq!(ruler, "-".repeat(super::AIM));
      assert!(ruler.len() < super::NODE);
    }
  }

  #[test]
  fn work_reads_as_saved_before() {
    let read = |json: &str| serde_json::from_str::<Work>(json).unwrap();
    let typed = Work {
      turn: true,
      agents: vec!["Ann".into()],
    };
    assert_eq!(read(&serde_json::to_string(&typed).unwrap()), typed);
    assert_eq!(read(r#"["your turn", "Ann"]"#), typed);
    assert_eq!(read(r#"["Acorn's turn", "Ann"]"#), typed);
    assert!(!read(r#"["Ann"]"#).turn);
    assert_eq!(read("[]"), Work::default());
  }

  /// A line too long is asked for fewer bytes by as much as it ran over,
  /// by half at most, each try from the last.
  #[test]
  fn a_line_too_long_is_asked_for_less() {
    let (said, most) = super::over(&"é".repeat(320), 512);
    assert_eq!(most, 358, "640 written for 512: 448 * 512 / 640");
    assert!(said.contains("was 640 bytes") && said.contains("within 358 bytes"));
    assert!(said.contains(&format!("\n{}\n", "-".repeat(358))), "its ruler");
    assert!(
      said.ends_with(&format!("\n{}| ← LIMIT", "é".repeat(179))),
      "cut at a character"
    );
    assert_eq!(
      super::over(&"x".repeat(520), most).1,
      308,
      "over still: 448 * 358 / 520"
    );
    assert_eq!(super::over(&"x".repeat(2000), 512).1, 256, "far over: half");
  }

  /// An agent's log reads whole while it fits; past that its oldest long
  /// entries are cut, but its first two, by half the bound at a time, so
  /// what was read stays as it was till the next half.
  #[test]
  fn an_agents_log_is_read_within_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    let name = mem.agent_new(None).unwrap();
    let push = |mem: &mut Memory, kind, size: usize| mem.agent_push(&name, kind, &"x".repeat(size)).unwrap();
    push(&mut mem, Kind::User, 3000); // its task
    push(&mut mem, Kind::Acorn, 100);
    for _ in 0..6 {
      push(&mut mem, Kind::Tool, 10_000);
      push(&mut mem, Kind::Acorn, 100);
    }
    let read = |mem: &Memory, most| super::shown(&mem.agent(&name).unwrap(), most);
    let marks = |read: &str| -> Vec<bool> { read.split("\n\n").map(|e| e.contains("[cut to fit:")).collect() };
    let whole = mem.agent_show(&name);
    assert_eq!(read(&mem, 80_000), whole, "63700 bytes fit");
    // 23700 over 40000: two halves (40000) to cut, so the five oldest results
    let cut = read(&mem, 40_000);
    assert!(cut.len() <= 40_000, "{}", cut.len());
    let (t, f) = (true, false);
    assert_eq!(marks(&cut), [f, f, t, f, t, f, t, f, t, f, t, f, f, f]);
    let entries: Vec<&str> = cut.split("\n\n").collect();
    assert!(entries[2].ends_with("[cut to fit: the first 512 of its 10000 bytes]"));
    assert!(entries[0].len() > 3000, "its task is whole");
    // more logged, what was read stays the same (a call's cache holds)
    push(&mut mem, Kind::Tool, 1000);
    push(&mut mem, Kind::Tool, 10_000);
    let more = read(&mem, 40_000);
    assert!(more.len() <= 40_000 && more.starts_with(&cut), "the log only grew");
    // till the next half is passed: the newest result still whole
    push(&mut mem, Kind::Tool, 10_000);
    let next = read(&mem, 40_000);
    assert!(next.len() <= 40_000, "{}", next.len());
    assert_eq!(marks(&next), [f, f, t, f, t, f, t, f, t, f, t, f, t, f, t, t, f]);
  }

  #[test]
  fn store_keeps_view_sizes_exact_on_overwrite_and_merged_parts() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    mem
      .push(vec![Msg::new(Kind::User, "first"), Msg::new(Kind::Acorn, "second")])
      .unwrap();
    mem
      .store(&[(Part::new(0, 0), "alpha".into()), (Part::new(0, 1), "beta".into())])
      .unwrap();
    mem.fit(false);
    let exact = |mem: &Memory, parts: &[Part]| parts.iter().map(|&p| mem.size(p)).sum::<usize>();
    assert_eq!(mem.view.size, exact(&mem, &mem.view.parts));
    assert_eq!(mem.seen.size, exact(&mem, &mem.seen.parts));
    // Overwriting an existing leaf updates view.size by the delta, not by adding again.
    mem.store(&[(Part::new(0, 0), "longer-alpha".into())]).unwrap();
    assert_eq!(mem.view.size, exact(&mem, &mem.view.parts));
    assert_eq!(mem.seen.size, exact(&mem, &mem.seen.parts));
  }
}
