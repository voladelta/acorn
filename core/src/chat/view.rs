//! View
//! ====
//! What the app is told of the chat: the parts the agent saw (`Eye`), each
//! message after them, and a running import's messages as the fewest
//! blocks of the tree that hold them, so no view ever gets the raw
//! history; a block opened gets its halves. Each message and summary is
//! told once, before a part that shows it; a summary not built yet, once
//! built.

use super::{Chat, Engine, Eye};
use crate::memory::{self, Part, local_time};
use crate::wire::{Entry, Event, Node, Seg, State, Wait};
use base64::Engine as _;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

impl Chat {
    pub(super) fn state(&mut self) -> State {
        let wait = match self.call.is_some() {
            true if self.asked => Wait::Model,
            true => Wait::None,
            false if self.busy && !self.wait.is_empty() && self.mem.first(0, 0) < self.from() => Wait::Summaries,
            false => Wait::None,
        };
        State { busy: self.busy, wait, agents: self.working(), goal: self.mem.goal.clone(), importing: self.importing(), today: self.today() }
    }

    pub(super) fn tell_state(&mut self) {
        let state = self.state();
        self.all(Event::State(state));
    }

    /// What the agent saw before message `from`.
    pub(super) fn eye_from(&self, from: u64) -> Eye {
        let parts = self.mem.view.parts.iter().filter(|p| p.span().1 < from).map(|p| (p.span().0, p.n())).collect();
        Eye { parts, from }
    }

    /// The chat as the agent saw it, then each message after, a running
    /// import's as blocks of the tree; they start where the tree's root
    /// holding the import's first message starts, so the oldest blocks are
    /// the largest, as in the tree.
    pub(super) fn show(&mut self) {
        let (from, count) = (self.eye.from, self.mem.count);
        let (a, hi) = self.batch.as_ref().map_or((0, 0), |b| (b.a, b.b));
        let roots = memory::roots(count);
        let lo = roots.iter().find(|p| p.span().1 >= a).map_or(a, |p| p.span().0);
        let near: Vec<Seg> = self.eye.parts.iter().copied().filter(|&(id, n)| id < hi && id + n > lo).collect();
        let cover = (near.iter().map(|s| s.0).fold(lo, u64::min), near.iter().map(|s| s.0 + s.1).fold(hi, u64::max));
        let mut parts: Vec<Seg> = self.eye.parts.iter().copied().filter(|&(id, n)| id + n <= cover.0 || id >= cover.1).collect();
        parts.extend(roots.iter().flat_map(|&p| blocks(p, cover)));
        parts.extend((from..cover.0).map(|k| (k, 1)));
        parts.extend((from.max(cover.1)..count).map(|k| (k, 1)));
        parts.sort_by_key(|s| s.0);
        self.teach(&parts);
        self.all(Event::View { parts, turn: from });
    }

    /// What the app lacks of parts, told before the parts themselves.
    fn teach(&mut self, parts: &[Seg]) {
        let (mut whole, mut nodes) = (Vec::new(), Vec::new());
        for &(id, n) in parts {
            if !self.seen.insert((id, n)) {
                continue;
            }
            if n == 1
                && let Some(m) = self.mem.message(id)
            {
                whole.push((id, Entry { kind: m.kind, text: m.text, date: m.date, images: m.images, spend: self.turn_use(id) }));
            }
            nodes.extend(self.node((id, n)));
        }
        if !whole.is_empty() || !nodes.is_empty() {
            self.all(Event::Know { whole, nodes });
        }
    }

    /// A summary, if built.
    fn node(&mut self, (id, n): Seg) -> Option<(Seg, Node)> {
        let text = self.mem.node(Part::of(id, n)?)?;
        let to = self.mem.message(id + n - 1)?.date;
        Some(((id, n), Node { text, to }))
    }

    /// A summary built: counted, if the import's; told, if the app was
    /// shown its block.
    pub(super) fn made(&mut self, part: Part) {
        if let Some(batch) = &mut self.batch {
            batch.built(part.l as usize, part.i as i64 - (batch.a >> part.l) as i64);
        }
        let seg = (part.span().0, part.n());
        if self.seen.contains(&seg)
            && let Some(node) = self.node(seg)
        {
            self.all(Event::Know { whole: vec![], nodes: vec![node] });
        }
    }

    /// A part opened into its halves.
    pub(super) fn zoom(&mut self, (id, n): Seg) {
        if n > 1 && self.mem.part(id, n).is_some() {
            let parts = vec![(id, n / 2), (id + n / 2, n / 2)];
            self.teach(&parts);
            self.all(Event::Zoom { seg: (id, n), parts });
        }
    }

    /// The tree's roots: the chat in the fewest blocks.
    pub(super) fn root(&mut self) {
        let parts: Vec<Seg> = memory::roots(self.mem.count).into_iter().map(|p| (p.span().0, p.n())).collect();
        self.teach(&parts);
        self.all(Event::Root { parts });
    }
}

/// The fewest blocks under a tree part that tile messages [a, b).
fn blocks(part: Part, (a, b): Seg) -> Vec<Seg> {
    let (first, last) = part.span();
    if last < a || first >= b {
        return vec![];
    }
    if first >= a && last < b {
        return vec![(first, part.n())];
    }
    part.halves().into_iter().flatten().flat_map(|h| blocks(h, (a, b))).collect()
}

/// How many messages a lock is held for, at most, as the memory is written
/// out: the chat goes on between.
const PIECE: u64 = 512;

/// Now, as a file's name tells it: the date and time here.
fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string()
}

impl Engine {
    /// The memory as JSON, in the user's Downloads: the chat's id, the
    /// messages, the summaries, the agents' logs, then every image, each
    /// in base64.
    pub(super) async fn export(self: &Arc<Self>) -> Result<PathBuf, String> {
        let file = crate::config::home().join("Downloads").join(format!("acorn-memory-{}.json", stamp()));
        let me = self.clone();
        let wrote = tokio::task::spawn_blocking(move || me.dump(&file).map(|_| file)).await;
        wrote.map_err(|err| err.to_string())?.map_err(|err| format!("The memory was not saved: {err}."))
    }

    fn dump(&self, file: &std::path::Path) -> anyhow::Result<()> {
        let mut out = std::io::BufWriter::new(std::fs::File::create(file)?);
        let (id, count, dir) = {
            let chat = self.lock();
            (chat.id.clone(), chat.mem.count, chat.mem.dir.clone())
        };
        write!(out, "{{\"chat\": {},\n\"messages\": [", serde_json::to_string(&id)?)?;
        for from in (0..count).step_by(PIECE as usize) {
            let chat = self.lock();
            for i in from..(from + PIECE).min(count) {
                let Some(m) = chat.mem.message(i) else { continue };
                let rec = serde_json::json!({"id": i, "kind": m.kind, "text": m.text, "date": m.date, "images": m.images, "more": m.more});
                write!(out, "{}{rec}", if i == 0 { "\n" } else { ",\n" })?;
            }
        }
        write!(out, "],\n\"summaries\": [")?;
        let mut sep = "\n";
        let levels = self.lock().mem.levels();
        for l in 0..levels {
            let n = 1u64 << l;
            for from in (0..count / n).step_by(PIECE as usize) {
                let chat = self.lock();
                for i in from..(from + PIECE).min(count / n) {
                    let part = Part::new(l, i);
                    if let Some(text) = chat.mem.node(part) {
                        write!(out, "{sep}{}", serde_json::json!({"id": part.span().0, "n": n, "text": text}))?;
                        sep = ",\n";
                    }
                }
            }
        }
        write!(out, "],\n\"agents\": {{")?;
        let ls = |sub: &str| -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(dir.join(sub)).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
            names.sort();
            names
        };
        sep = "\n";
        let agents = self.lock().mem.agent_names();
        for name in &agents {
            let Some(log) = self.lock().mem.agent(name) else { continue };
            let entries: Vec<_> = log.iter().map(|r| serde_json::json!({"id": r.i, "kind": r.kind, "text": r.text, "date": r.date})).collect();
            write!(out, "{sep}{}: {}", serde_json::to_string(name)?, serde_json::to_string(&entries)?)?;
            sep = ",\n";
        }
        write!(out, "}},\n\"images\": {{")?;
        sep = "\n";
        for name in ls("docs/images") {
            let Some(path) = self.lock().mem.image_file(&name) else { continue };
            let data = base64::engine::general_purpose::STANDARD.encode(std::fs::read(path)?);
            write!(out, "{sep}{}: \"{data}\"", serde_json::to_string(&name)?)?;
            sep = ",\n";
        }
        writeln!(out, "}}}}")?;
        out.flush()?;
        Ok(())
    }

    /// The whole tree as a page in the temp dir: a section for the view,
    /// the messages and each level; opened in the browser.
    pub(super) async fn tree(self: &Arc<Self>) -> Result<PathBuf, String> {
        let file = std::env::temp_dir().join(format!("acorn-tree-{}.html", stamp()));
        let me = self.clone();
        let made = tokio::task::spawn_blocking(move || me.page(&file).map(|_| file)).await;
        let file = made.map_err(|err| err.to_string())?.map_err(|err| format!("No tree page: {err}."))?;
        let _ = tokio::process::Command::new("open").arg(&file).status().await;
        Ok(file)
    }

    fn page(&self, file: &std::path::Path) -> anyhow::Result<()> {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;");
        let item = |head: &[String], text: &str| format!("<h4>{}</h4><pre>{}</pre>", esc(&head.join(" · ")), esc(text));
        let mut out = std::io::BufWriter::new(std::fs::File::create(file)?);
        let (count, levels, view) = {
            let chat = self.lock();
            (chat.mem.count, chat.mem.levels(), chat.mem.view.parts.clone())
        };
        let when = |chat: &mut Chat, a: u64, b: u64| {
            let date = |chat: &mut Chat, id| chat.mem.message(id).map(|m| local_time(&m.date)).unwrap_or_default();
            let first = date(chat, a);
            if a == b { first } else { format!("{first} – {}", date(chat, b)) }
        };
        let node = |chat: &mut Chat, part: Part, text: &str| {
            let (a, b) = part.span();
            item(&[format!("LVL{} #{a}-{b}", part.l), when(chat, a, b), format!("{} B", text.len())], text)
        };
        let menu: String = ["VIEW".to_string(), "ROOT".to_string()]
            .into_iter()
            .chain((0..levels).map(|l| format!("LVL{l}")))
            .map(|name| format!("<a href=\"#{name}\">{name}</a>"))
            .collect();
        write!(
            out,
            "<!doctype html><meta charset=\"utf-8\"><title>Acorn memory</title>
<style>
body {{ font: 13px/1.4 ui-monospace, monospace; margin: 2em; }} nav a {{ margin-right: 1em; }}
section {{ display: none; }} section:target {{ display: block; }}
h4 {{ margin: .8em 0 .2em; color: #888; font-weight: normal; }} pre {{ margin: 0; white-space: pre-wrap; font: inherit; }}
</style>
<script>location.hash = location.hash || \"VIEW\";</script>
<nav>{menu}</nav>
<section id=\"VIEW\">"
        )?;
        for part in view {
            let mut chat = self.lock();
            let text = chat.mem.text(part).to_string();
            write!(out, "{}", node(&mut chat, part, &text))?;
        }
        write!(out, "</section>\n<section id=\"ROOT\">")?;
        for from in (0..count).step_by(PIECE as usize) {
            let mut chat = self.lock();
            for i in from..(from + PIECE).min(count) {
                let Some(r) = chat.mem.message(i) else { continue };
                let head = [format!("ROOT #{i}"), when(&mut chat, i, i), r.kind.as_str().to_string(), format!("{} B", r.size)];
                write!(out, "{}", item(&head, &r.text))?;
            }
        }
        for l in 0..levels {
            write!(out, "</section>\n<section id=\"LVL{l}\">")?;
            let nodes = self.lock().mem.level(l);
            for (part, text) in nodes {
                let mut chat = self.lock();
                write!(out, "{}", node(&mut chat, part, &text))?;
            }
        }
        writeln!(out, "</section>")?;
        out.flush()?;
        Ok(())
    }
}
