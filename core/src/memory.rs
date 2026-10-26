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
//! Files: main/ (messages) and tree/ (nodes), JSON lines in a file per
//! day; main.index and tree.index give each line's place by slot (16
//! bytes: its offset as an f64, its day, its length); view.json keeps the
//! views, the chat's pick, goal and work, written over. Pages `more` after
//! the last that isn't are not in the chat. Memory holds only the views'
//! nodes and those built above them; any other line is read by its index.
//! Images are in docs/images/ by their hash; an agent's log in
//! docs/agent/<Name>.jsonl. All of it is UniiChat's format.

use crate::clip;
use crate::{Img, Kind, Served, config, disk, names};
use anyhow::{Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

// Constants
// =========

pub const NODE: usize = 512;
pub const JOBS: usize = 64;
/// The compactions' view merges past this (16-32 KB).
const SEEN: usize = 32_000;
const BLOCK: usize = 4;
const SLOT: u64 = 16;
/// The files kept open to read: the two indexes and the days read lately.
const FDS: usize = 8;

/// A request's images: 20 at full size, each IMAGE bytes at most; PIXELS,
/// the most decoded: a larger image goes as is.
pub const PICS: usize = 20;
const FIT: f64 = 1568.0;
const IMAGE: usize = 1 << 20;
const PIXELS: u64 = 1 << 28;

pub const WAIT: &str = "(not summarized yet: zoom it)";
const IMAGES: &str = "docs/images";
const AGENTS: &str = "docs/agent";

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

    /// Its place in tree.index: nodes in order, each parent between its
    /// halves.
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

#[derive(Serialize, Deserialize)]
struct TreeRec {
    l: u32,
    i: u64,
    text: String,
    size: usize,
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

/// A log: of messages (main) or of nodes (tree).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Main,
    Tree,
}

impl Stream {
    fn name(self) -> &'static str {
        match self {
            Stream::Main => "main",
            Stream::Tree => "tree",
        }
    }
}

/// How far a log is indexed: its last file's day (since 1970), its size.
type End = (i64, u64);

#[derive(Serialize, Deserialize, Default)]
struct Kept {
    parts: Vec<(u32, u64)>,
    #[serde(default)]
    batch: bool,
}

/// view.json: the chat's view, the compactions' (none: the chat's).
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    #[serde(flatten)]
    view: Kept,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen: Option<Kept>,
    #[serde(default)]
    pick: serde_json::Value,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    work: Vec<String>,
    #[serde(default)]
    main: End,
    #[serde(default)]
    tree: End,
}

/// The chat's memory. count: the pages logged whole; open: those after,
/// not whole yet; pick: the chat's, as last set (as saved: one not offered
/// is told once); goal: the user's (/goal), till met or halted; work: what
/// ran, as last saved; nodes: the texts of the views' nodes built and of
/// those above them, by slot; agents: their logs' lengths; ready: the
/// nodes not built whose halves are, by side and level, in order; busy:
/// nodes building, by slot, with their side; sides: the side by the last
/// message of nodes not built; hold: why each side is held (None: it
/// builds); made: nodes built since last drained; fitted: the views were
/// fit since last drained.
pub struct Memory {
    pub dir: PathBuf,
    fds: Vec<(PathBuf, File)>,
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
    ends: [End; 2],
    pub busy: HashMap<u64, usize>,
    sides: HashMap<u64, usize>,
    pub hold: [Option<String>; 2],
    pub done: bool,
    pub made: Vec<Part>,
    pub fitted: bool,
}

// Memory
// ======

impl Memory {
    pub fn load(dir: &Path) -> Result<Memory> {
        fs::create_dir_all(dir)?;
        let saved = saved(dir);
        let mut mem = Memory {
            dir: dir.to_path_buf(),
            fds: Vec::new(),
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
            ends: [(0, 0), (0, 0)],
            busy: HashMap::new(),
            sides: HashMap::new(),
            hold: [None, Some("paused".into())],
            done: false,
            made: Vec::new(),
            fitted: false,
        };
        if let Some(s) = &saved {
            mem.pick = s.pick.clone();
            mem.goal = s.goal.clone();
            mem.work = s.work.clone();
        }
        mem.ends[0] = mem.index(Stream::Main, saved.as_ref().map_or((0, 0), |s| s.main))?;
        mem.ends[1] = mem.index(Stream::Tree, saved.as_ref().map_or((0, 0), |s| s.tree))?;
        mem.count = fs::metadata(dir.join("main.index"))?.len() / SLOT;
        while mem.count > 0 && mem.message(mem.count - 1).is_some_and(|m| m.more) {
            mem.count -= 1;
        }
        let kept = saved.as_ref().map(|s| &s.view);
        let seen = saved.as_ref().map(|s| s.seen.as_ref().unwrap_or(&s.view));
        mem.view = mem.opened(mem.view.high, kept);
        mem.seen = mem.opened(SEEN, seen);
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

    /// view.json, written whole, then moved in place; flushed soon.
    pub fn save(&self) -> Result<()> {
        let kept = |v: &View| Kept { parts: v.parts.iter().map(|p| (p.l, p.i)).collect(), batch: v.batch };
        let saved = Saved {
            view: kept(&self.view),
            seen: Some(kept(&self.seen)),
            pick: self.pick.clone(),
            goal: self.goal.clone(),
            work: self.work.clone(),
            main: self.ends[0],
            tree: self.ends[1],
        };
        let file = self.dir.join("view.json");
        let new = self.dir.join("view.json.new");
        fs::write(&new, serde_json::to_vec(&saved)?)?;
        fs::rename(&new, &file)?;
        disk::wrote(&file);
        Ok(())
    }

    // Logs
    // ----

    /// Indexes the log's lines past `from` (none, or no index: all, in an
    /// index made anew); answers how far.
    fn index(&mut self, stream: Stream, from: End) -> Result<End> {
        let file = self.dir.join(format!("{}.index", stream.name()));
        let mut end = if file.exists() { from } else { (0, 0) };
        let logs = self.dir.join(stream.name());
        fs::create_dir_all(&logs)?;
        let fd = OpenOptions::new().read(true).write(true).create(true).truncate(end.0 == 0).open(&file)?;
        let mut days: Vec<i64> = fs::read_dir(&logs)?
            .filter_map(|e| e.ok()?.file_name().to_str().and_then(file_day))
            .filter(|&day| day >= end.0)
            .collect();
        days.sort_unstable();
        for day in days {
            let at = if day == end.0 { end.1 } else { 0 };
            end = (day, self.scan(stream, day, at, &fd)?);
        }
        Ok(end)
    }

    /// Indexes, into fd, the lines of a day's file from byte `at`; answers
    /// the file's end. A line a crash cut short is skipped.
    fn scan(&mut self, stream: Stream, day: i64, at: u64, fd: &File) -> Result<u64> {
        use std::io::{Read, Seek, SeekFrom};
        let file = self.path(stream, day);
        let mut data = Vec::new();
        let mut f = File::open(&file)?;
        f.seek(SeekFrom::Start(at))?;
        f.read_to_end(&mut data)?;
        let mut k = 0;
        while let Some(nl) = data[k..].iter().position(|&b| b == b'\n') {
            let line = &data[k..k + nl];
            if let Some(rec) = parse(line, &file) {
                place(fd, stream, &rec, day, at + k as u64, nl as u64)?;
            }
            k += nl + 1;
        }
        if k < data.len() {
            eprintln!("skipped the last line of {}: cut short", file.display());
            OpenOptions::new().append(true).open(&file)?.write_all(b"\n")?;
            return Ok(at + data.len() as u64 + 1);
        }
        Ok(at + k as u64)
    }

    /// The record at a slot of a log; None: none.
    fn read<T: for<'a> Deserialize<'a>>(&mut self, stream: Stream, slot: u64) -> Option<T> {
        let index = self.dir.join(format!("{}.index", stream.name()));
        let mut rec = [0u8; SLOT as usize];
        let got = self.fd(&index)?.read_at(&mut rec, slot * SLOT).ok()?;
        let day = u32::from_le_bytes(rec[8..12].try_into().unwrap());
        if got < SLOT as usize || day == 0 {
            return None;
        }
        let at = f64::from_le_bytes(rec[0..8].try_into().unwrap()) as u64;
        let len = u32::from_le_bytes(rec[12..16].try_into().unwrap()) as usize;
        let mut line = vec![0u8; len];
        let path = self.path(stream, day as i64);
        self.fd(&path)?.read_exact_at(&mut line, at).ok()?;
        serde_json::from_slice(&line).ok()
    }

    /// A file of the chat to read, kept open (the FDS read last).
    fn fd(&mut self, path: &Path) -> Option<&File> {
        match self.fds.iter().position(|(p, _)| p == path) {
            Some(k) => {
                let fd = self.fds.remove(k);
                self.fds.push(fd);
            }
            None => {
                let fd = File::open(path).ok()?;
                if self.fds.len() >= FDS {
                    self.fds.remove(0);
                }
                self.fds.push((path.to_path_buf(), fd));
            }
        }
        self.fds.last().map(|(_, f)| f)
    }

    fn path(&self, stream: Stream, day: i64) -> PathBuf {
        let date = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap() + chrono::Days::new(day as u64);
        self.dir.join(stream.name()).join(format!("{}.jsonl", date.format("%Y-%m-%d")))
    }

    /// Records appended to the file of the local date, then indexed.
    fn log<T: Serialize>(&mut self, stream: Stream, recs: &[T], slots: impl Fn(&T) -> u64) -> Result<()> {
        let day = (Local::now().date_naive() - NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).num_days();
        let lines: Vec<String> = recs.iter().map(serde_json::to_string).collect::<Result<_, _>>()?;
        let file = self.path(stream, day);
        let mut f = OpenOptions::new().append(true).create(true).open(&file)?;
        let mut at = f.metadata()?.len();
        f.write_all(lines.iter().map(|l| format!("{l}\n")).collect::<String>().as_bytes())?;
        disk::wrote(&file);
        let index = OpenOptions::new().write(true).open(self.dir.join(format!("{}.index", stream.name())))?;
        for (rec, line) in recs.iter().zip(&lines) {
            write_slot(&index, slots(rec), day, at, line.len() as u64)?;
            at += line.len() as u64 + 1;
        }
        self.ends[stream as usize] = (day, at);
        Ok(())
    }

    // Messages
    // --------

    /// Message i, from its file; None: none.
    pub fn message(&mut self, i: u64) -> Option<Root> {
        if i < self.count { self.read(Stream::Main, i) } else { None }
    }

    /// The pages logged; those `more` stay open, past the count, till one
    /// that isn't. The caller pumps the compactor.
    pub fn push(&mut self, msgs: Vec<Msg>) -> Result<Vec<Root>> {
        let base = self.count + self.open;
        let roots: Vec<Root> = msgs.into_iter().flat_map(pages).enumerate().map(|(k, m)| root(base + k as u64, m)).collect();
        let done = roots.iter().rposition(|r| !r.more).map_or(0, |k| k + 1);
        let ids = if done == 0 { 0 } else { self.open + done as u64 };
        self.log(Stream::Main, &roots, |r| r.i)?;
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
    pub fn whole(&mut self, from: u64) -> u64 {
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
    pub fn say(&mut self, a: u64, b: u64) -> crate::Say {
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
    pub fn images(&mut self, id: u64) -> Vec<Img> {
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

    /// Node (l,i)'s text, kept or from its file; None: not built.
    pub fn node(&mut self, part: Part) -> Option<String> {
        if let Some(text) = self.nodes.get(&part.slot()) {
            return Some(text.clone());
        }
        self.read::<TreeRec>(Stream::Tree, part.slot()).map(|r| r.text)
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
    pub fn source(&mut self, part: Part) -> Option<String> {
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
        let recs: Vec<TreeRec> = parts.iter().map(|(p, text)| TreeRec { l: p.l, i: p.i, text: text.clone(), size: text.len() }).collect();
        self.log(Stream::Tree, &recs, |r| Part::new(r.l, r.i).slot())?;
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
            eprintln!("memory: view.json: {err}");
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

    pub fn date(&mut self, id: u64) -> String {
        match self.message(id) {
            Some(root) => root.when(),
            None => format!("No message {id}."),
        }
    }

    /// Where a page of the chat starts: the last message starting a turn
    /// (the user's or a report) the view holds whole; none: the first, or
    /// the end.
    pub fn turn(&mut self) -> u64 {
        let from = self.view.parts.iter().rposition(|p| p.l > 0).map_or(0, |k| k + 1);
        let whole: Vec<Part> = self.view.parts[from..].to_vec();
        let user = whole.iter().rev().find(|p| self.message(p.i).is_some_and(|m| matches!(m.kind, Kind::User | Kind::Work)));
        user.or(whole.first()).map_or(self.count, |p| p.i)
    }

    /// Level l's nodes built, in order.
    pub fn level(&mut self, l: u32) -> Vec<(Part, String)> {
        let n = 1u64 << l;
        (0..self.count / n).filter_map(|i| self.node(Part::new(l, i)).map(|t| (Part::new(l, i), t))).collect()
    }

    /// The tree's levels: as many as the count's bits.
    pub fn levels(&self) -> u32 {
        u64::BITS - self.count.leading_zeros()
    }

    // Agents
    // ------

    fn agent_file(&self, name: &str) -> PathBuf {
        self.dir.join(AGENTS).join(format!("{name}.jsonl"))
    }

    /// An agent's log, read whole; a name is letters and hyphens, never a
    /// path.
    pub fn agent(&self, name: &str) -> Option<Vec<Root>> {
        let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic() || c == '-');
        let file = self.agent_file(name);
        if !ok || !file.exists() {
            return None;
        }
        let data = fs::read(&file).ok()?;
        if !data.is_empty() && !data.ends_with(b"\n") {
            let _ = OpenOptions::new().append(true).open(&file).and_then(|mut f| f.write_all(b"\n"));
        }
        Some(data.split(|&b| b == b'\n').filter_map(|line| parse(line, &file)).collect())
    }

    pub fn agent_new(&mut self) -> Result<String> {
        let dir = self.dir.join(AGENTS);
        fs::create_dir_all(&dir)?;
        loop {
            let name = names::random();
            match OpenOptions::new().write(true).create_new(true).open(self.agent_file(&name)) {
                Ok(_) => {
                    self.agents.insert(name.clone(), 0);
                    return Ok(name);
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.into()),
            }
        }
    }

    /// A message to an agent's log: its length read once, then counted.
    pub fn agent_push(&mut self, name: &str, kind: Kind, text: &str) -> Result<u64> {
        let n = match self.agents.get(name) {
            Some(&n) => n,
            None => self.agent(name).map_or(0, |log| log.len() as u64),
        };
        let root = root(n, Msg::new(kind, text));
        let file = self.agent_file(name);
        let mut f = OpenOptions::new().append(true).create(true).open(&file).with_context(|| format!("agent {name}"))?;
        f.write_all(format!("{}\n", serde_json::to_string(&root)?).as_bytes())?;
        disk::wrote(&file);
        self.agents.insert(name.to_string(), n + 1);
        Ok(n)
    }

    pub fn agent_show(&self, name: &str) -> String {
        match self.agent(name) {
            Some(log) => log.iter().map(|r| format!("{}|{}", r.i, r.show())).collect::<Vec<_>>().join("\n\n"),
            None => format!("No agent {name}."),
        }
    }

    /// Closes the files it reads.
    pub fn close(&mut self) {
        self.fds.clear();
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

/// view.json; None: none, or unreadable.
fn saved(dir: &Path) -> Option<Saved> {
    serde_json::from_slice(&fs::read(dir.join("view.json")).ok()?).ok()
}

/// Whether a chat was cut at work (its view.json names what ran).
pub fn was_working(dir: &Path) -> bool {
    saved(dir).is_some_and(|s| !s.work.is_empty())
}

/// A day file's day since 1970, by its name.
fn file_day(name: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(name.get(..10)?, "%Y-%m-%d").ok()?;
    Some((date - NaiveDate::from_ymd_opt(1970, 1, 1)?).num_days())
}

/// A JSON line; None: not JSON.
fn parse<T: for<'a> Deserialize<'a>>(line: &[u8], file: &Path) -> Option<T> {
    match serde_json::from_slice(line) {
        Ok(rec) => Some(rec),
        Err(_) => {
            if !line.is_empty() {
                eprintln!("skipped a line of {}: not a record", file.display());
            }
            None
        }
    }
}

/// A record's line in its log's index; one with no index (an act's record
/// UniiChat logged) has none.
fn place(fd: &File, stream: Stream, rec: &serde_json::Value, day: i64, at: u64, len: u64) -> Result<()> {
    let Some(i) = rec.get("i").and_then(|i| i.as_u64()) else { return Ok(()) };
    let slot = match stream {
        Stream::Main => i,
        Stream::Tree => Part::new(rec.get("l").and_then(|l| l.as_u64()).unwrap_or(0) as u32, i).slot(),
    };
    write_slot(fd, slot, day, at, len)
}

fn write_slot(fd: &File, slot: u64, day: i64, at: u64, len: u64) -> Result<()> {
    let mut buf = [0u8; SLOT as usize];
    buf[0..8].copy_from_slice(&(at as f64).to_le_bytes());
    buf[8..12].copy_from_slice(&(day as u32).to_le_bytes());
    buf[12..16].copy_from_slice(&(len as u32).to_le_bytes());
    fd.write_all_at(&buf, slot * SLOT)?;
    Ok(())
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
