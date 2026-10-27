//! Import
//! ======
//! A person's past Claude and ChatGPT chats (APPS), read into their chat:
//! those on this Mac (Claude Code's and Codex's logs) and those on the
//! websites, from the zip each mails on request (`export`). A session (a
//! log; a site's chat) is read twice: for its messages' heads (the count
//! and the estimate), then the heads picked (`Want`): a note, then its
//! messages (pages), oldest first; never thinking. Its summaries run on
//! the compactor. A session imported before (`key`) brings only its new
//! messages.
//!
//! The estimate is the run's plan (`guess`): each node whose source is
//! over NODE takes a call, priced as measured on the compactor (PRICE). It
//! is shown, never a cap.

use crate::Kind;
use crate::clip;
use crate::memory::{NODE, local_time};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A summary's bytes (the compactor writes 460-510).
const LINE: usize = 480;

/// What a compaction call costs on the compactor, in micro-USD: base,
/// `byte` a doubling of its source past NODE, `depth` a summary under it
/// (its own too); fit on 5,617 compaction calls of real imports.
const PRICE: (f64, f64, f64) = (187.0, 735.0, 628.0);

/// The apps a session is of: each its program on this Mac and its website.
pub const APPS: [&str; 2] = ["Claude", "ChatGPT"];

/// Where each app's website gives its export, a zip it emails.
pub const EXPORTS: [(&str, &str); 2] = [("Claude", "Settings › Privacy › Export data"), ("ChatGPT", "Settings › Data controls › Export data")];

/// Sessions all imported before.
pub const OLD: &str = "Nothing new: these chats are in Acorn's memory already.";
const NOT_EXPORT: &str = "Not a Claude or ChatGPT export.";

/// A message (a page) as imported: its date ISO, whether the next goes on
/// with its text.
#[derive(Clone, Debug)]
pub struct Msg {
    pub kind: Kind,
    pub text: String,
    pub date: String,
    pub more: bool,
}

/// A message as the count and the estimate read it: its size, its place in
/// its session.
#[derive(Clone, Debug)]
pub struct Head {
    kind: Kind,
    date: String,
    size: usize,
    n: usize,
}

/// Where a session's messages are: a log's file (Claude Code's or Codex's,
/// told by its first line), or a site's chat, read.
#[derive(Clone, Debug)]
enum Source {
    Log(PathBuf),
    Msgs(Arc<Vec<Msg>>),
}

/// A session: its app, where it was (a log: "session in <cwd>"; a site's
/// chat: 'chat "<title>"'), its first message's date, its messages' heads.
#[derive(Clone, Debug)]
pub struct Session {
    pub app: String,
    pub place: String,
    pub start: String,
    source: Source,
    heads: Vec<Head>,
}

/// The sessions imported before, by `key`: their last message's date.
pub type Done = HashMap<String, String>;

/// What an import takes: the sessions read but those left out (off: their
/// ids), the messages from a day on ("": all), tool calls or not.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Want {
    pub off: Vec<String>,
    pub since: String,
    pub tools: bool,
}

/// An import: the sessions as wanted but for off, those picked, their
/// messages, the first new message's day ("": none), their sizes, its cost
/// in USD.
pub struct Plan {
    pub sessions: Vec<Session>,
    pub picked: Vec<Session>,
    pub msgs: usize,
    pub first: String,
    pub sizes: Vec<usize>,
    pub usd: f64,
}

/// A session as listed to pick: its id, app, cells (its day, its messages
/// at a glance, where), whether picked.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    pub app: String,
    pub cells: [String; 3],
    pub picked: bool,
}

/// What a look for sessions found, as an import would take it.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub rows: Vec<Row>,
    pub msgs: usize,
    pub first: String,
    pub usd: f64,
    pub want: Want,
}

// Finding
// =======

/// Where Claude Code and Codex keep their sessions.
pub fn homes() -> Vec<PathBuf> {
    let home = crate::config::home();
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from);
    let codex = std::env::var_os("CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from);
    vec![claude.join("projects"), codex.join("sessions"), codex.join("archived_sessions")]
}

/// The sessions of Claude Code's and Codex's dirs (one missing has none),
/// and of the exports given.
pub fn find(exports: &[PathBuf]) -> Result<Vec<Session>, String> {
    let mut found = Vec::new();
    for dir in homes() {
        let mut files = Vec::new();
        walk(&dir, &mut files);
        files.sort();
        found.extend(files.into_iter().filter(|f| is_log(f)).filter_map(|f| session(Source::Log(f), "", "")));
    }
    for file in exports {
        if !file.exists() {
            return Err(format!("no file {}", file.display()));
        }
        found.extend(export(file)?);
    }
    Ok(found)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        if path.is_dir() { walk(&path, out) } else { out.push(path) }
    }
}

/// A path the import reads: a session's file.
fn is_log(path: &Path) -> bool {
    let p = path.to_string_lossy();
    let session = ["projects/", "sessions/", "archived_sessions/"].iter().any(|d| p.contains(d));
    session && p.ends_with(".jsonl") && !p.contains("subagents/")
}

/// A session read for its heads (a log's app and where are told by its
/// lines); None: no messages.
fn session(source: Source, app: &str, place: &str) -> Option<Session> {
    let mut s = Session { app: app.into(), place: place.into(), start: String::new(), source, heads: Vec::new() };
    let mut heads = Vec::new();
    s.read(|m| heads.push(Head { kind: m.kind, date: m.date.clone(), size: size(m.kind, &m.text), n: heads.len() }));
    s.heads = heads;
    s.start = s.heads.first()?.date.clone();
    Some(s)
}

impl Session {
    /// Its messages, its file read a line at a time.
    fn read(&mut self, mut each: impl FnMut(&Msg)) {
        match self.source.clone() {
            Source::Msgs(msgs) => msgs.iter().for_each(each),
            Source::Log(file) => {
                let Ok(f) = std::fs::File::open(&file) else { return };
                let mut date = String::new();
                for line in std::io::BufReader::new(f).split(b'\n').map_while(Result::ok) {
                    // a log is another app's file: one odd line never fails
                    // its session, nor the import of all the others
                    let Ok(j) = serde_json::from_slice::<Value>(&line) else { continue };
                    let msgs = self.line(&j, &date);
                    if let Some(m) = msgs.last() {
                        date.clone_from(&m.date);
                    }
                    msgs.iter().for_each(&mut each);
                }
            }
        }
    }

    /// A log's line: a Codex log (first line session_meta) is a ChatGPT
    /// session, a Claude Code log a Claude one.
    fn line(&mut self, j: &Value, last: &str) -> Vec<Msg> {
        if self.app.is_empty() {
            self.app = if j["type"] == "session_meta" { "ChatGPT" } else { "Claude" }.into();
        }
        if self.app == "ChatGPT" { self.codex(j, last) } else { self.claude(j, last) }
    }

    /// A session: the app, where, when.
    fn name(&self) -> String {
        format!("{} {}, from {}", self.app, self.place, local_time(&self.start).get(..16).unwrap_or_default())
    }

    /// As it opens: its note.
    fn note(&self) -> Msg {
        Msg { kind: Kind::Note, text: format!("{} (imported)", self.name()), date: self.start.clone(), more: false }
    }

    /// Its name in Done (its note's) and in Want's off.
    pub fn id(&self) -> String {
        let note = self.note();
        key(&note.text, &note.date)
    }

    /// As listed to pick, in columns: its day, its messages at a glance,
    /// where (a folder short, a chat's title).
    pub fn cells(&self) -> [String; 3] {
        let day = chrono::DateTime::parse_from_rfc3339(&self.start).map(|d| d.format("%b %d, %Y").to_string()).unwrap_or_default();
        let place = match self.place.strip_prefix("session in ") {
            Some(dir) => path_short(dir),
            None => {
                let title = self.place.strip_prefix("chat \"").and_then(|t| t.strip_suffix('"')).unwrap_or(&self.place);
                if title.is_empty() { "untitled".into() } else { title.into() }
            }
        };
        [day, count_short(self.heads.len()), place]
    }

    /// Its note, then the messages of its heads kept, read again.
    pub fn body(&self, each: &mut impl FnMut(Msg)) {
        each(self.note());
        let kept: HashSet<usize> = self.heads.iter().map(|h| h.n).collect();
        let mut n = 0;
        let mut me = self.clone();
        me.read(|m| {
            if kept.contains(&n) {
                each(m.clone());
            }
            n += 1;
        });
    }
}

/// A session's name in Done: its note's date and where (its text less the
/// app's name, whatever it was, and the time, whose zone was whoever
/// imported it).
pub fn key(text: &str, date: &str) -> String {
    let from = [" session in ", " chat \""].iter().filter_map(|m| text.find(m)).min().unwrap_or(0);
    let text = &text[from..];
    let text = match text.rfind(", from ") {
        Some(k) if !text[k + 7..].contains(',') => &text[..k],
        _ => text,
    };
    format!("{date} {text}")
}

/// Whether a message of a session imported up to `last` (none: never) is
/// new: dated after it.
pub fn is_new(date: &str, last: Option<&String>) -> bool {
    last.is_none_or(|l| date > l.as_str())
}

/// A folder as read at a glance: home as ~, and past three folders the
/// first and the last two, the middle as … ("~/code/…/app/src").
pub fn path_short(path: &str) -> String {
    let mut p = path.to_string();
    for root in ["/Users/", "/home/"] {
        if let Some(rest) = path.strip_prefix(root) {
            let user_end = rest.find('/').unwrap_or(rest.len());
            p = format!("~{}", &rest[user_end..]);
        }
    }
    let parts: Vec<&str> = p.split('/').collect();
    let head = if parts[0].is_empty() { 2 } else { 1 };
    if parts.len() > head + 3 {
        let mut out = parts[..head + 1].to_vec();
        out.push("…");
        out.extend(&parts[parts.len() - 2..]);
        out.join("/")
    } else {
        p
    }
}

/// A count in at most 4 characters: 999, 12k, 3m.
fn count_short(n: usize) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{}k", (n as f64 / 1e3).round()),
        _ => format!("{}m", (n as f64 / 1e6).round()),
    }
}

/// A message's bytes as summarized.
pub fn size(kind: Kind, text: &str) -> usize {
    kind.as_str().len() + 2 + text.len()
}

/// "1 message", "2,048 new messages" (what: "new ").
pub fn count(n: usize, what: &str) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (k, c) in digits.chars().enumerate() {
        if k > 0 && (digits.len() - k).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{grouped} {what}{}", if n == 1 { "message" } else { "messages" })
}

// Planning
// ========

/// The sessions oldest first, with the heads kept only.
fn pick(sessions: &[Session], want: &Want, done: &Done) -> Vec<Session> {
    let mut old = sessions.to_vec();
    old.sort_by(|a, b| a.start.cmp(&b.start));
    old.into_iter()
        .filter_map(|mut s| {
            let last = done.get(&s.id());
            s.heads.retain(|m| (want.tools || matches!(m.kind, Kind::User | Kind::Acorn)) && m.date.as_str() >= want.since.as_str() && is_new(&m.date, last));
            (!s.heads.is_empty()).then_some(s)
        })
        .collect()
}

/// The sessions found, as an import into a chat of `count` messages.
pub fn plan(sessions: &[Session], want: &Want, done: &Done, count: u64) -> Plan {
    let all = pick(sessions, &Want { since: String::new(), ..want.clone() }, done);
    let shown = pick(&all, want, &Done::new());
    let picked: Vec<Session> = shown.iter().filter(|s| !want.off.contains(&s.id())).cloned().collect();
    let first = all.iter().map(|s| s.heads[0].date.clone()).min().unwrap_or_default();
    let sizes: Vec<usize> = picked.iter().flat_map(|s| std::iter::once(size(Kind::Note, &s.note().text)).chain(s.heads.iter().map(|m| m.size))).collect();
    Plan { msgs: sizes.len() - picked.len(), first: first.get(..10).unwrap_or_default().into(), usd: usd(count, &sizes), sessions: shown, picked, sizes }
}

impl Plan {
    pub fn found(&self, want: &Want) -> Found {
        let rows = self
            .sessions
            .iter()
            .map(|s| Row { id: s.id(), app: s.app.clone(), cells: s.cells(), picked: !want.off.contains(&s.id()) })
            .collect();
        Found { rows, msgs: self.msgs, first: self.first.clone(), usd: self.usd, want: want.clone() }
    }
}

/// A node as the estimate sees it: its source's bytes, and the summaries
/// under it (0: its source is messages as they are).
#[derive(Clone, Copy)]
struct Node {
    bytes: usize,
    depth: usize,
}

/// By level, what each node of messages of these sizes appended to a chat
/// of `a` costs (micro-USD): the import's nodes (their last message its),
/// each its cost; a leaf's source is its message, a merge's its two
/// children's lines (a chat's own older one, a summary as deep as it).
pub fn guess(a: u64, sizes: &[usize]) -> Vec<Vec<u64>> {
    let mut usd = Vec::new();
    let mut nodes: Vec<Node> = sizes.iter().map(|&bytes| Node { bytes, depth: 0 }).collect();
    let (mut l, mut i) = (0usize, a as i64);
    while !nodes.is_empty() {
        usd.push(nodes.iter().map(|&n| node_usd(n)).collect());
        let line = |k: i64| if k < 0 { Node { bytes: LINE, depth: l } } else { node_line(nodes[k as usize]) };
        let from = i.div_euclid(2);
        let to = (i + nodes.len() as i64).div_euclid(2);
        nodes = (0..to - from).map(|j| node_merge(line(2 * (from + j) - i), line(2 * (from + j) + 1 - i))).collect();
        l += 1;
        i = i.div_euclid(2);
    }
    usd
}

/// The guess's total (USD).
pub fn usd(a: u64, sizes: &[usize]) -> f64 {
    guess(a, sizes).iter().flatten().sum::<u64>() as f64 / 1e6
}

/// "$3.10"
pub fn guess_usd(usd: f64) -> String {
    format!("${:.2}", usd.max(0.01))
}

/// A node's cost: none if its source fits, else a call's (PRICE).
fn node_usd(node: Node) -> u64 {
    if node.bytes <= NODE {
        return 0;
    }
    let (base, byte, depth) = PRICE;
    (base + byte * (node.bytes as f64 / NODE as f64).log2() + depth * (node.depth + 1) as f64).round() as u64
}

/// A node's line: its source if it fits, else a summary one deeper.
fn node_line(node: Node) -> Node {
    if node.bytes > NODE { Node { bytes: LINE, depth: node.depth + 1 } } else { Node { bytes: node.bytes, depth: 0 } }
}

fn node_merge(a: Node, b: Node) -> Node {
    Node { bytes: a.bytes + 1 + b.bytes, depth: a.depth.max(b.depth) }
}

// Export
// ======

/// A site's export, its zip (conversations.json in it) or that file alone,
/// as sessions, one a chat: ChatGPT's (a tree, `mapping`) or Claude's
/// (`chat_messages`); a file not a list of those is no export.
fn export(file: &Path) -> Result<Vec<Session>, String> {
    let mut data = std::fs::read(file).map_err(|e| e.to_string())?;
    if data.starts_with(b"PK") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
        let name = zip.file_names().find(|n| *n == "conversations.json" || n.ends_with("/conversations.json")).map(str::to_string);
        let Some(name) = name else { return Err("No conversations.json in the zip.".into()) };
        let mut json = Vec::new();
        zip.by_name(&name).map_err(|e| e.to_string())?.read_to_end(&mut json).map_err(|e| e.to_string())?;
        data = json;
    }
    let chats: Vec<Value> = serde_json::from_slice(&data).map_err(|_| NOT_EXPORT.to_string())?;
    if !chats.iter().all(|c| !c["mapping"].is_null() || c["chat_messages"].is_array()) {
        return Err(NOT_EXPORT.into());
    }
    Ok(chats
        .iter()
        .filter_map(|chat| {
            let (app, msgs) = if chat["mapping"].is_null() { ("Claude", chat_claude(chat)) } else { ("ChatGPT", chat_openai(chat)) };
            let title = chat["title"].as_str().or(chat["name"].as_str()).unwrap_or_default();
            session(Source::Msgs(Arc::new(msgs)), app, &format!("chat \"{title}\""))
        })
        .collect())
}

/// A ChatGPT chat's messages: its thread shown, current_node up to the
/// root (an edit or a retry leaves its old branch); a person's say,
/// ChatGPT's, what it sent a tool (recipient) as a call, a tool's output as
/// an echo; never thoughts, nor a hidden one.
fn chat_openai(chat: &Value) -> Vec<Msg> {
    let mut nodes = Vec::new();
    let mut id = chat["current_node"].as_str().map(str::to_string);
    let mut seen = HashSet::new();
    while let Some(k) = id.filter(|k| seen.insert(k.clone())) {
        nodes.push(chat["mapping"][&k]["message"].clone());
        id = chat["mapping"][&k]["parent"].as_str().map(str::to_string);
    }
    nodes.reverse();
    let mut date = date_of(&chat["create_time"], "");
    let mut out = Vec::new();
    for m in nodes {
        let kind = match m["author"]["role"].as_str() {
            Some("user") => Some(Kind::User),
            Some("assistant") => Some(if m["recipient"] == "all" { Kind::Acorn } else { Kind::Tool }),
            Some("tool") => Some(Kind::Echo),
            _ => None,
        };
        let ctype = m["content"]["content_type"].as_str().unwrap_or("undefined");
        let hidden = m["metadata"]["is_visually_hidden_from_conversation"] == true;
        date = date_of(&m["create_time"], &date);
        let Some(kind) = kind.filter(|_| !hidden && ctype != "thoughts" && ctype != "reasoning_recap") else { continue };
        let parts = if m["content"]["parts"].is_array() { m["content"]["parts"].clone() } else { Value::Array(vec![m["content"]["text"].clone()]) };
        let head = if kind == Kind::Tool { format!("{} ", m["recipient"].as_str().unwrap_or("undefined")) } else { String::new() };
        out.extend(msgs_of(kind, &format!("{head}{}", parts_text(&parts)), &date));
    }
    out
}

/// A Claude website chat's messages, in order: a person's say (its texts
/// and its files' names), Claude's, their tool calls and results; never
/// thinking.
fn chat_claude(chat: &Value) -> Vec<Msg> {
    let mut date = date_of(&chat["created_at"], "");
    let mut out = Vec::new();
    for m in chat["chat_messages"].as_array().into_iter().flatten() {
        date = date_of(&m["created_at"], &date);
        let kind = if m["sender"] == "human" { Kind::User } else { Kind::Acorn };
        let mut blocks = match m["content"].as_array() {
            Some(c) => c.clone(),
            None => vec![serde_json::json!({"type": "text", "text": m["text"].as_str().unwrap_or_default()})],
        };
        for f in m["attachments"].as_array().into_iter().chain(m["files"].as_array()).flatten() {
            blocks.push(serde_json::json!({"type": "text", "text": format!("[file {}]", f["file_name"].as_str().unwrap_or("undefined"))}));
        }
        out.extend(blocks_msgs(kind, &blocks, &date));
    }
    out
}

// Claude Code
// ===========

impl Session {
    /// A line's say (its texts and images) is one message; then its tools;
    /// a line without a timestamp is of the last message's date.
    fn claude(&mut self, j: &Value, last: &str) -> Vec<Msg> {
        let date = date_of(&j["timestamp"], last);
        let t = j["type"].as_str().unwrap_or_default();
        let own = (t == "user" || t == "assistant") && j["isMeta"] != true && j["isSidechain"] != true && j["isCompactSummary"] != true;
        let content = &j["message"]["content"];
        let blocks = match content {
            _ if !own => vec![],
            Value::String(s) => vec![serde_json::json!({"type": "text", "text": s})],
            Value::Array(a) => a.clone(),
            _ => vec![],
        };
        if self.place.is_empty()
            && let Some(cwd) = j["cwd"].as_str().filter(|c| !c.is_empty())
        {
            self.place = format!("session in {cwd}");
        }
        if t == "attachment" && j["attachment"]["type"] == "queued_command" {
            return msgs_of(Kind::User, j["attachment"]["prompt"].as_str().unwrap_or_default(), &date);
        }
        blocks_msgs(if t == "user" { Kind::User } else { Kind::Acorn }, &blocks, &date)
    }

    /// An agent's rollout (source subagent, internal) is no person's chat.
    fn codex(&mut self, j: &Value, last: &str) -> Vec<Msg> {
        let p = &j["payload"];
        let date = date_of(&j["timestamp"], last);
        if j["type"] == "session_meta" {
            let agent = !p["source"]["subagent"].is_null() || !p["source"]["internal"].is_null();
            self.place = if agent { String::new() } else { format!("session in {}", p["cwd"].as_str().unwrap_or("undefined")) };
        }
        if j["type"] != "response_item" || self.place.is_empty() {
            return vec![];
        }
        let str = |v: &Value| v.as_str().unwrap_or("undefined").to_string();
        match p["type"].as_str() {
            Some("message") => {
                let kind = match p["role"].as_str() {
                    Some("user") => Kind::User,
                    Some("assistant") => Kind::Acorn,
                    _ => return vec![],
                };
                let parts: Vec<Value> = p["content"].as_array().into_iter().flatten().filter(|c| !injected(c["text"].as_str().unwrap_or_default())).cloned().collect();
                msgs_of(kind, &parts_text(&Value::Array(parts)), &date)
            }
            Some("function_call") => msgs_of(Kind::Tool, &format!("{} {}", str(&p["name"]), str(&p["arguments"])), &date),
            Some("custom_tool_call") => msgs_of(Kind::Tool, &format!("{} {}", str(&p["name"]), serde_json::json!({"input": p["input"]})), &date),
            Some("local_shell_call") => msgs_of(Kind::Tool, &format!("shell {}", serde_json::json!({"command": p["action"]["command"]})), &date),
            Some("function_call_output" | "custom_tool_call_output") => msgs_of(Kind::Echo, &parts_text(&p["output"]), &date),
            _ => vec![],
        }
    }
}

/// A say's blocks (Claude's shape: texts, images, tool calls and their
/// results) as a message, its texts and images; then its calls, each its
/// name and input, and their results.
fn blocks_msgs(kind: Kind, blocks: &[Value], date: &str) -> Vec<Msg> {
    let say: Vec<Value> = blocks.iter().filter(|b| b["type"] == "image" || (b["type"] == "text" && !skipped(b["text"].as_str().unwrap_or_default()))).cloned().collect();
    let mut out = msgs_of(kind, &parts_text(&Value::Array(say)), date);
    for b in blocks {
        match b["type"].as_str() {
            Some("tool_use") => out.extend(msgs_of(Kind::Tool, &format!("{} {}", b["name"].as_str().unwrap_or("undefined"), b["input"]), date)),
            Some("tool_result") => out.extend(msgs_of(Kind::Echo, &parts_text(&b["content"]), date)),
            _ => {}
        }
    }
    out
}

/// What Codex injects as a person's: its own tags, AGENTS.md.
fn injected(text: &str) -> bool {
    if text.starts_with("# AGENTS.md") {
        return true;
    }
    let Some(rest) = text.strip_prefix('<') else { return false };
    let name = rest.bytes().take_while(|b| b.is_ascii_lowercase() || *b == b'_').count();
    name > 0 && matches!(rest.as_bytes().get(name), Some(b' ' | b'>'))
}

/// What Claude Code logs as a person's that no person said.
fn skipped(text: &str) -> bool {
    ["<command-", "<local-command", "[Request interrupted"].iter().any(|p| text.starts_with(p))
}

/// A time as ISO: a log's or Claude's site's string, ChatGPT's seconds;
/// none or bad: `last`.
fn date_of(at: &Value, last: &str) -> String {
    let ms = match at {
        Value::Number(n) => n.as_f64().map(|s| (s * 1000.0) as i64),
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis()),
        _ => None,
    };
    ms.and_then(chrono::DateTime::from_timestamp_millis).map_or_else(|| last.to_string(), |d| d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// A message in its pages.
fn msgs_of(kind: Kind, text: &str, date: &str) -> Vec<Msg> {
    if text.trim().is_empty() {
        return vec![];
    }
    let pages = clip::pages(text);
    let n = pages.len();
    pages.into_iter().enumerate().map(|(k, text)| Msg { kind, text, date: date.into(), more: k < n - 1 }).collect()
}

/// Content as text: a string, or parts (texts, images).
fn parts_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|c| match c.as_str().or_else(|| c["text"].as_str()) {
                Some(text) => text.to_string(),
                None => {
                    let t = c["type"].as_str().or_else(|| c["content_type"].as_str()).unwrap_or("undefined");
                    if t.contains("image") { "[image]".into() } else { String::new() }
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_drop_the_app_and_time() {
        let k = key("Claude session in /x/y, from 2026-01-02 03:04 (imported)", "2026-01-02T03:04:00.000Z");
        assert_eq!(k, "2026-01-02T03:04:00.000Z  session in /x/y");
        assert_eq!(path_short("/Users/me/code/a/b/app/src"), "~/code/…/app/src");
        assert_eq!(path_short("/Users/me/code"), "~/code");
        assert_eq!(count(2048, "new "), "2,048 new messages");
        assert!(injected("<environment_context>") && !injected("<3 you"));
    }

    #[test]
    fn guesses_price_the_tree() {
        let small = guess(0, &[100, 100, 100, 100]);
        assert!(small.iter().flatten().all(|&c| c == 0));
        let big = guess(3, &[5000; 8]);
        assert!(big[0].iter().all(|&c| c > 0));
        assert!(usd(3, &[5000; 8]) > 0.0);
    }
}
