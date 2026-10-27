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
//! views, the chat's pick, goal and work (`state`, written over). Pages
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

use crate::clip;
use crate::{Img, Kind, Served, config, disk, names};
use anyhow::{Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::{BinaryHeap, HashMap, HashSet};
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
        let who = if self.imported && matches!(self.kind, Kind::Acorn | Kind::Tool) { "ai" } else { self.kind.as_str() };
        let pics: String = self.images.iter().map(|name| format!("\n[image docs/images/{name}]")).collect();
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
        Msg { kind, text: text.into(), date: None, images: Vec::new(), imported: false, line: None, more: false }
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

/// The state row: the chat's view, the compactions', the pick, goal and
/// work.
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    view: Kept,
    seen: Kept,
    #[serde(default)]
    pick: serde_json::Value,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    work: Vec<String>,
}

/// The chat's memory. db: chat.db, shared with `chat::usage`; count: the
/// pages logged whole; open: those after, not whole yet; pick: the chat's,
/// as last set (as saved: one not offered is told once); goal: the user's
/// (/goal), till met or halted; work: what ran, as last saved; nodes: the
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
    pub pick: serde_json::Value,
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
            pick: serde_json::Value::Null,
            goal: None,
            work: Vec::new(),
            nodes: HashMap::new(),
            agents: HashMap::new(),
            view: View { high: config::get().view, ..View::default() },
            seen: View { high: SEEN, ..View::default() },
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
            mem.pick = s.pick.clone();
            mem.goal = s.goal.clone();
            mem.work = s.work.clone();
        }
        mem.count = mem.db.query_row("select coalesce(max(i) + 1, 0) from msgs where log = ''", [], |r| r.get(0))?;
        while mem.count > 0 && mem.message(mem.count - 1).is_some_and(|m| m.more) {
            mem.count -= 1;
        }
        mem.db.execute("delete from msgs where log = '' and i >= ?", [mem.count])?;
        mem.view = mem.opened(mem.view.high, saved.as_ref().map(|s| &s.view));
        mem.seen = mem.opened(SEEN, saved.as_ref().map(|s| &s.seen));
        for part in roots(mem.count).into_iter().flat_map(|p| mem.todo(p)).collect::<Vec<_>>() {
            mem.ready(part, true);
        }
        mem.fit(false);
        mem.save()?;
        Ok(mem)
    }

    /// A view as kept (none: the highest nodes built).
    fn opened(&mut self, high: usize, kept: Option<&Kept>) -> View {
        let keys: Option<HashSet<u64>> = kept.map(|k| k.parts.iter().map(|&(l, i)| Part::new(l, i).slot()).collect());
        let parts: Vec<Part> = roots(self.count).into_iter().flat_map(|p| self.down(p, keys.as_ref())).collect();
        let size = parts.iter().map(|&p| self.size(p)).sum();
        View { parts, size, batch: kept.is_some_and(|k| k.batch), high }
    }

    /// The state row, written over.
    pub fn save(&self) -> Result<()> {
        let kept = |v: &View| Kept { parts: v.parts.iter().map(|p| (p.l, p.i)).collect(), batch: v.batch };
        let saved = Saved { view: kept(&self.view), seen: kept(&self.seen), pick: self.pick.clone(), goal: self.goal.clone(), work: self.work.clone() };
        self.db.execute("insert or replace into state values (1, ?)", [serde_json::to_string(&saved)?])?;
        Ok(())
    }

    // Logs
    // ----

    /// Message i of a log (CHAT, or an agent's); None: none.
    fn read(&self, log: &str, i: u64) -> Option<Root> {
        let mut q = self.db.prepare_cached("select * from msgs where log = ? and i = ?").ok()?;
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
                q.execute(params![log, r.i, r.kind.as_str(), r.text, r.date, images, r.imported, r.line, r.more, r.size])?;
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
        let roots: Vec<Root> = msgs.into_iter().flat_map(pages).enumerate().map(|(k, m)| root(base + k as u64, m)).collect();
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

    // Images
    // ------

    pub fn images_dir(&self) -> PathBuf {
        self.dir.join(IMAGES)
    }

    /// An image file of the chat by its name; None: no such.
    pub fn image_file(&self, name: &str) -> Option<PathBuf> {
        let (hash, ext) = name.split_once('.')?;
        let ok = hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) && !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_lowercase());
        let file = self.images_dir().join(name);
        (ok && file.exists()).then_some(file)
    }

    /// What models get of message id's images: their copies, if any.
    pub fn images(&self, id: u64) -> Vec<Img> {
        let names = self.message(id).map(|m| m.images).unwrap_or_default();
        let dir = self.images_dir();
        names
            .iter()
            .filter_map(|name| {
                let fit = dir.join(format!("{name}.jpg"));
                let (file, mime) = if fit.exists() {
                    (fit, "image/jpeg".to_string())
                } else {
                    (dir.join(name), format!("image/{}", name.rsplit('.').next().unwrap_or("png")))
                };
                let data = base64::engine::general_purpose::STANDARD.encode(fs::read(file).ok()?);
                Some(Img { mime, data })
            })
            .collect()
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
        q.query_row([part.slot()], |r| r.get(0)).optional().unwrap_or_else(|err| {
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
        self.ready[side].first().and_then(|q| q.get(k)).copied().unwrap_or(self.count)
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
        if self.side(part) == 1 { Served::Import } else { Served::Compactor }
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
                next.extend(queue.iter().map(|&i| Part::new(l as u32, i)).take_while(|p| p.ctx() <= end));
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

    // Views
    // -----

    /// A node of the views or above them: its text, or WAIT if not built.
    pub fn text(&self, part: Part) -> &str {
        self.nodes.get(&part.slot()).map_or(WAIT, String::as_str)
    }

    /// A line's bytes as calls get it: none until built.
    fn size(&self, part: Part) -> usize {
        if self.built(part) { self.text(part).len() } else { 0 }
    }

    /// Each view to its end; a batch starts as a view passes `high`, or goes
    /// on at a message (`next`) if a missing parent stopped it; between
    /// messages, only a whole batch, to half or, as parents not built keep
    /// it over (an import's lag JOBS calls a level), by `high`: each change
    /// makes every call after write the view anew. A node merged away no
    /// view holds, nor one under it, is let go.
    pub fn fit(&mut self, next: bool) {
        let mut gone = Vec::new();
        let mut changed = false;
        let sides: Vec<u64> = [0, 1].into_iter().filter(|&s| self.hold[s].is_none()).map(|s| self.first(0, s)).collect();
        let count = self.count;
        let seen_end = sides.into_iter().fold(count, u64::min);
        let mut views = [std::mem::take(&mut self.view), std::mem::take(&mut self.seen)];
        self.reach(&mut views[0], count);
        self.reach(&mut views[1], seen_end);
        for view in &mut views {
            let (parts, batch) = (view.parts.len(), view.batch);
            if view.size > view.high || (next && view.batch) {
                let merged = self.merge(view);
                if next || merged.size <= (view.high / 2).max(view.size.saturating_sub(view.high)) {
                    view.parts = merged.parts;
                    view.size = merged.size;
                    gone.extend(merged.gone);
                }
                view.batch = view.size > view.high / 2;
            }
            changed |= view.parts.len() != parts || view.batch != batch;
        }
        [self.view, self.seen] = views;
        for part in gone {
            if !self.held(part) {
                self.nodes.remove(&part.slot());
            }
        }
        if changed && let Err(err) = self.save() {
            eprintln!("memory: state: {err}");
        }
        self.fitted = true;
    }

    /// A view to message `end`: its lines from it dropped (an import going
    /// on again), the messages to it appended.
    fn reach(&self, view: &mut View, end: u64) {
        let cut = view.cover(end);
        for part in view.parts.split_off(cut) {
            view.size -= self.size(part);
        }
        for i in view.end()..end {
            view.parts.push(Part::leaf(i));
            view.size += self.size(Part::leaf(i));
        }
    }

    /// Whether a view holds `part` or a node under it, or will (not there).
    fn held(&self, part: Part) -> bool {
        [&self.view, &self.seen].into_iter().any(|view| {
            let k = view.cover(part.span().0);
            k == view.parts.len() || view.parts[k].l <= part.l
        })
    }

    /// A view's most due pairs merged down to half, as their parents allow:
    /// a pair whose parent is built, most due first ((T + 1) / 2^l - i, T the
    /// view's end: its age from its last message, in halves), the oldest of
    /// equals; a heap of pairs over the view as a linked list, so a view of
    /// thousands merges in milliseconds.
    fn merge(&self, from: &View) -> Merged {
        let t = from.end() as f64;
        let low = from.high / 2;
        let mut view: Vec<Option<Part>> = from.parts.iter().copied().map(Some).collect();
        let len = view.len();
        let mut next: Vec<usize> = (1..=len).collect();
        let mut prev: Vec<isize> = (0..len as isize).map(|k| k - 1).collect();
        let mut heap = BinaryHeap::new();
        let mut gone = Vec::new();
        let offer = |heap: &mut BinaryHeap<Due>, view: &[Option<Part>], next: &[usize], k: isize| {
            if k < 0 {
                return;
            }
            let k = k as usize;
            let (Some(a), Some(Some(b))) = (view[k], view.get(next[k]).copied()) else { return };
            if a.i % 2 != 0 || b.l != a.l || b.i != a.i + 1 || !self.built(a.up()) {
                return;
            }
            let d = (t + 1.0) / a.n() as f64 - a.i as f64;
            heap.push(Due { d, k, a, b });
        };
        let mut size = from.size;
        for k in 0..len {
            offer(&mut heap, &view, &next, k as isize);
        }
        while size > low {
            let Some(Due { k, a, b, .. }) = heap.pop() else { break };
            let j = next[k];
            if view[k] != Some(a) || view.get(j).copied().flatten() != Some(b) {
                continue;
            }
            let up = a.up();
            size = size + self.size(up) - self.size(a) - self.size(b);
            view[k] = Some(up);
            view[j] = None;
            next[k] = next[j];
            if next[j] < len {
                prev[next[j]] = k as isize;
            }
            gone.extend([a, b]);
            offer(&mut heap, &view, &next, prev[k]);
            offer(&mut heap, &view, &next, k as isize);
        }
        Merged { parts: view.into_iter().flatten().collect(), size, gone }
    }

    /// The views' lines built before message `end`, in blocks (the last cut
    /// short), one marked: the last whole one, else the opening one. The
    /// mark ends the prefix single flight keys (`ai::cache`); DeepSeek
    /// caches prefixes itself.
    pub fn blocks(&self, end: u64, parts: &[Part]) -> Vec<crate::Block> {
        let lines: Vec<String> = parts
            .iter()
            .filter(|p| p.span().1 < end && self.built(**p))
            .map(|&p| format!("{}\n", line(p, self.text(p))))
            .collect();
        let whole = lines.len() / BLOCK;
        let mut blocks = vec![crate::Block::new("<chat>\n", whole == 0)];
        for k in 0..whole {
            blocks.push(crate::Block::new(lines[k * BLOCK..(k + 1) * BLOCK].concat(), k == whole - 1));
        }
        blocks.push(crate::Block::new(lines[whole * BLOCK..].concat() + "</chat>\n", false));
        blocks
    }

    /// The chat's view before message `end`, as a call's blocks.
    pub fn view_blocks(&self, end: u64) -> Vec<crate::Block> {
        self.blocks(end, &self.view.parts)
    }

    /// The node of the n messages from id, if in the chat.
    pub fn part(&self, id: u64, n: u64) -> Option<Part> {
        Part::of(id, n).filter(|_| id + n <= self.count)
    }

    /// A message (n = 1) whole, a long one a page from `at`, its images on
    /// the first; else the two lines under id+n.
    pub fn zoom(&mut self, id: u64, n: u64, at: usize) -> String {
        let Some(part) = self.part(id, n) else {
            let roots: Vec<String> = roots(self.count).into_iter().map(Part::name).collect();
            return format!("No line {id}+{n}: n is a power of 2, id a multiple of n. The tree's roots: {}.", roots.join(", "));
        };
        match part.halves() {
            None => {
                let mut root = self.message(id).expect("a message of the chat");
                root.text = clip::page(&root.text, at);
                if at > 0 {
                    root.images.clear();
                }
                format!("{id}+0|{}", root.show())
            }
            Some(halves) => halves
                .into_iter()
                .map(|h| {
                    let text = self.node(h).unwrap_or_else(|| WAIT.into());
                    line(h, &text)
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    pub fn date(&self, id: u64) -> String {
        match self.message(id) {
            Some(root) => root.when(),
            None => format!("No message {id}."),
        }
    }

    /// Where a page of the chat starts: the last message starting a turn
    /// (the user's or a report) the view holds whole; none: the first, or
    /// the end.
    pub fn turn(&self) -> u64 {
        let from = self.view.parts.iter().rposition(|p| p.l > 0).map_or(0, |k| k + 1);
        let whole: Vec<Part> = self.view.parts[from..].to_vec();
        let user = whole.iter().rev().find(|p| self.message(p.i).is_some_and(|m| matches!(m.kind, Kind::User | Kind::Work)));
        user.or(whole.first()).map_or(self.count, |p| p.i)
    }

    /// Level l's nodes built, in order.
    pub fn level(&self, l: u32) -> Vec<(Part, String)> {
        let n = 1u64 << l;
        (0..self.count / n).filter_map(|i| self.node(Part::new(l, i)).map(|t| (Part::new(l, i), t))).collect()
    }

    /// The tree's levels: as many as the count's bits.
    pub fn levels(&self) -> u32 {
        u64::BITS - self.count.leading_zeros()
    }

    // Agents
    // ------

    /// An agent's log, read whole; None: no such agent.
    pub fn agent(&self, name: &str) -> Option<Vec<Root>> {
        let known: bool = self.db.query_row("select exists (select 1 from agents where name = ?)", [name], |r| r.get(0)).ok()?;
        if !known {
            return None;
        }
        let mut q = self.db.prepare_cached("select * from msgs where log = ? order by i").ok()?;
        let rows = q.query_map([name], from_row).ok()?;
        Some(rows.filter_map(Result::ok).collect())
    }

    /// The agents' names, in order.
    pub fn agent_names(&self) -> Vec<String> {
        let names = self.db.prepare("select name from agents order by name").and_then(|mut q| q.query_map([], |r| r.get(0))?.collect());
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
            None => self.db.query_row("select count(*) from msgs where log = ?", [name], |r| r.get(0))?,
        };
        self.log(name, &[root(n, Msg::new(kind, text))])?;
        self.agents.insert(name.to_string(), n + 1);
        Ok(n)
    }

    pub fn agent_show(&self, name: &str) -> String {
        match self.agent(name) {
            Some(log) => log.iter().map(|r| format!("{}|{}", r.i, r.show())).collect::<Vec<_>>().join("\n\n"),
            None => format!("No agent {name}."),
        }
    }

    // Purges
    // ------

    /// Erases a message for good: the chat's message id (each of its
    /// pages), or message id of an agent's log (`agent`). Its text becomes
    /// PURGED, its line and images go, and, in the chat, every node built
    /// from it: those in a view open into their halves, and it and those
    /// above build anew (a call building any node now is not stored: the
    /// caller stops them). A wipe of chat.db is then owed. Answers what it
    /// erased.
    pub fn purge(&mut self, agent: Option<&str>, id: u64) -> Result<String> {
        let log = agent.unwrap_or(CHAT);
        let (mut a, mut b) = (id, id);
        match agent {
            Some(name) if self.read(name, id).is_none() => return Ok(format!("No message {id} in {name}'s chat.")),
            Some(_) => {}
            None if id >= self.count => return Ok(format!("No message {id}.")),
            None => {
                while a > 0 && self.message(a - 1).is_some_and(|m| m.more) {
                    a -= 1;
                }
                while b + 1 < self.count && self.message(b).is_some_and(|m| m.more) {
                    b += 1;
                }
            }
        }
        let mut gone = HashSet::new();
        if agent.is_none() {
            for i in a..=b {
                let mut part = Part::leaf(i);
                while part.span().1 < self.count {
                    if self.node(part).is_some() {
                        gone.insert(part.slot());
                    }
                    part = part.up();
                }
            }
        }
        let wipe = self.dir.join(WIPE);
        fs::write(&wipe, "")?;
        disk::wrote(&wipe);
        let mut images = Vec::new();
        let tx = self.db.transaction()?;
        for i in a..=b {
            let Some(mut root) = tx.query_row("select * from msgs where log = ? and i = ?", params![log, i], from_row).optional()? else { continue };
            images.append(&mut root.images);
            root.text = PURGED.into();
            root.line = None;
            root.size = root.show().len();
            tx.execute("update msgs set text = ?, images = '[]', line = null, size = ? where log = ? and i = ?", params![root.text, root.size, log, i])?;
        }
        for &slot in &gone {
            tx.execute("delete from nodes where slot = ?", [slot])?;
        }
        tx.commit()?;
        let mut said = match agent {
            Some(name) => format!("Purged message {id} of {name}'s chat."),
            None => {
                self.unbuild(a, b, &gone);
                let pages = if a == b { String::new() } else { format!(" (its pages {a} to {b})") };
                format!("Purged message {id}{pages} and the {} summaries made from it; they are being written anew without it.", gone.len())
            }
        };
        for name in images {
            let used = self.db.query_row("select exists (select 1 from msgs where images like '%' || ? || '%')", [&name], |r| r.get::<_, bool>(0));
            match used {
                Ok(true) => {}
                Ok(false) => {
                    let dir = self.images_dir();
                    for file in [dir.join(&name), dir.join(format!("{name}.jpg"))] {
                        let _ = fs::remove_file(file);
                    }
                }
                Err(err) => said += &format!(" Its image {name} was kept: {err}."),
            }
        }
        self.purged += 1;
        said += if self.stuck { " Its old bytes are wiped from Acorn's files when Acorn starts again." } else { " Its old bytes are wiped from Acorn's files once the chat is idle." };
        Ok(said)
    }

    /// A wipe done of the purges up to `upto`; none owed after, its marker
    /// goes.
    pub fn wiped(&mut self, upto: u64) {
        self.wiped = upto;
        if self.wiped == self.purged {
            let _ = fs::remove_file(self.dir.join(WIPE));
        }
    }

    /// The nodes `gone` (by slot) let go, the views' open into the halves
    /// left, the leaves a..=b and those above them to build anew.
    fn unbuild(&mut self, a: u64, b: u64, gone: &HashSet<u64>) {
        self.age += 1;
        self.nodes.retain(|slot, _| !gone.contains(slot));
        for i in a..=b {
            let mut part = Part::leaf(i).up();
            while part.span().1 < self.count {
                self.ready(part, false);
                part = part.up();
            }
            self.ready(Part::leaf(i), true);
        }
        for k in 0..2 {
            let view = if k == 0 { &self.view } else { &self.seen };
            let parts: Vec<Part> = view.parts.clone().into_iter().flat_map(|p| self.open_up(p, gone)).collect();
            let size = parts.iter().map(|&p| self.size(p)).sum();
            let view = if k == 0 { &mut self.view } else { &mut self.seen };
            (view.parts, view.size) = (parts, size);
        }
        self.fit(false);
        if let Err(err) = self.save() {
            eprintln!("memory: state: {err}");
        }
    }

    /// A view's line, or, if gone, the lines under it left, kept.
    fn open_up(&mut self, part: Part, gone: &HashSet<u64>) -> Vec<Part> {
        match part.halves() {
            Some(halves) if gone.contains(&part.slot()) => halves.into_iter().flat_map(|h| self.open_up(h, gone)).collect(),
            _ => {
                if let Some(text) = self.node(part) {
                    self.nodes.insert(part.slot(), text);
                }
                vec![part]
            }
        }
    }
}

struct Merged {
    parts: Vec<Part>,
    size: usize,
    gone: Vec<Part>,
}

/// A pair of a view's lines to merge: how due, where.
struct Due {
    d: f64,
    k: usize,
    a: Part,
    b: Part,
}

impl PartialEq for Due {
    fn eq(&self, o: &Due) -> bool {
        self.cmp(o).is_eq()
    }
}
impl Eq for Due {}
impl PartialOrd for Due {
    fn partial_cmp(&self, o: &Due) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Due {
    /// The most due first, then the oldest.
    fn cmp(&self, o: &Due) -> std::cmp::Ordering {
        self.d.total_cmp(&o.d).then(o.k.cmp(&self.k))
    }
}

// Helpers
// =======

/// The state row; None: none, or unreadable.
fn saved(db: &Connection) -> Option<Saved> {
    let json: String = db.query_row("select json from state where id = 1", [], |r| r.get(0)).optional().ok()??;
    serde_json::from_str(&json).ok()
}

/// Deleted rows erased from a db's file: rewritten whole (secure_delete
/// zeroes only cells and pages freed, not old bytes left in a page still
/// used), its WAL truncated. It takes as long as the db is big (2 s for
/// 450 MB) and holds up every write meanwhile.
pub fn erase(db: &Connection) -> Result<()> {
    db.execute_batch("vacuum")?;
    let busy: i64 = db.query_row("pragma wal_checkpoint(truncate)", [], |r| r.get(0))?;
    anyhow::ensure!(busy == 0, "its WAL is in use");
    Ok(())
}

/// The chat in `dir` wiped (`erase`), on a connection of its own: blocking.
pub fn wipe(dir: &Path) -> Result<()> {
    let db = Connection::open(dir.join("chat.db"))?;
    db.busy_timeout(std::time::Duration::from_secs(60))?;
    erase(&db)
}

/// A message from its row in `msgs`.
fn from_row(r: &Row) -> rusqlite::Result<Root> {
    let bad = |k, e: serde_json::Error| rusqlite::Error::FromSqlConversionFailure(k, rusqlite::types::Type::Text, e.into());
    let kind = serde_json::from_value(serde_json::Value::String(r.get(2)?)).map_err(|e| bad(2, e))?;
    let images = serde_json::from_str(&r.get::<_, String>(5)?).map_err(|e| bad(5, e))?;
    Ok(Root { i: r.get(1)?, kind, text: r.get(3)?, date: r.get(4)?, images, imported: r.get(6)?, line: r.get(7)?, more: r.get(8)?, size: r.get(9)? })
}

/// Now, as logged: "2026-10-26T12:03:05.123Z".
pub fn iso_now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// An ISO date here, as "2026-10-26 14:03:05" ("" if not one).
pub fn local_time(iso: &str) -> String {
    DateTime::parse_from_rfc3339(iso).map(|d| d.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
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
            let text = (k + texts.len()).checked_sub(n).and_then(|t| texts.get(t)).cloned().unwrap_or_default();
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

/// An image's kind by its first bytes; None: none of png, jpeg, gif, webp.
pub fn image_kind(head: &[u8]) -> Option<&'static str> {
    let at = |k: usize, mark: &[u8]| head.get(k..k + mark.len()) == Some(mark);
    if at(0, b"\x89PNG") {
        Some("image/png")
    } else if at(0, b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if at(0, b"GIF8") {
        Some("image/gif")
    } else if at(8, b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

pub const IMAGES_ARE: &str = "Images are png, jpeg, gif or webp.";

/// An image's bytes put in a chat's dir by their hash, its copy for models
/// beside it (scaled to FIT, as a JPEG); its name. Blocking: run off the
/// chat's lock.
pub fn image_add(dir: &Path, bytes: &[u8]) -> Result<String> {
    use sha2::Digest;
    let kind = image_kind(bytes).context(IMAGES_ARE)?;
    let img = image::load_from_memory(bytes).context(IMAGES_ARE)?;
    let (w, h) = (img.width(), img.height());
    let hash = sha2::Sha256::digest(bytes);
    let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let name = format!("{hex}.{}", &kind["image/".len()..]);
    let k = (FIT / w.max(h) as f64).min(1.0);
    let fit = (k < 1.0 || bytes.len() > IMAGE) && (w as u64) * (h as u64) <= PIXELS;
    let images = dir.join(IMAGES);
    fs::create_dir_all(&images)?;
    if fit {
        let small = img.resize_exact((w as f64 * k).round() as u32, (h as f64 * k).round() as u32, image::imageops::FilterType::Lanczos3);
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&small.to_rgb8())?;
        let file = images.join(format!("{name}.jpg"));
        fs::write(&file, out)?;
        disk::wrote(&file);
    }
    let file = images.join(&name);
    fs::write(&file, bytes)?;
    disk::wrote(&file);
    Ok(name)
}
