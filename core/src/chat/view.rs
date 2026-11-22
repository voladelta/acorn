//! View
//! ====
//! What the app is told of the chat: the parts the agent saw (`Eye`), each
//! message after them, and a running import's messages as the fewest
//! blocks of the tree that hold them, so no view ever gets the raw
//! history; a block opened gets its halves. Each message and summary is
//! told before a part that shows it, and not again while a view shows it;
//! a summary not built yet, once built. A window may drop what it no
//! longer shows (web/src/model.ts `prune`), so a part new to the view, a
//! block's halves and the overview's tops are told again each time.

use super::{Chat, Engine, Eye, Phase};
use crate::memory::{self, Part, local_time};
use crate::wire::{Entry, Event, Node, Seg, State, Wait};
use base64::Engine as _;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

impl Chat {
  pub(super) fn state(&mut self) -> State {
    let wait = match &self.phase {
      Phase::Calling { asked: true, .. } => Wait::Model,
      Phase::Checking(_) => Wait::Goal,
      Phase::Settling if !self.wait.is_empty() && self.mem.first(0, 0) < self.from() => Wait::Summaries,
      Phase::Idle | Phase::Settling | Phase::Calling { .. } => Wait::None,
    };
    State {
      busy: self.busy(),
      wait,
      agents: self.working(),
      goal: self.mem.goal.clone(),
      importing: self.importing(),
      today: self.today(),
    }
  }

  pub(super) fn tell_state(&mut self) {
    let state = self.state();
    self.all(Event::State(state));
  }

  /// What the agent saw before message `from`.
  pub(super) fn eye_from(&self, from: u64) -> Eye {
    let (whole, boundary) = memory::prefix(&self.mem.view.parts, from);
    let parts = whole
      .iter()
      .copied()
      .chain(boundary)
      .map(|p| (p.span().0, p.n()))
      .collect();
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
    let near: Vec<Seg> = self
      .eye
      .parts
      .iter()
      .copied()
      .filter(|&(id, n)| id < hi && id + n > lo)
      .collect();
    let cover = (
      near.iter().map(|s| s.0).fold(lo, u64::min),
      near.iter().map(|s| s.0 + s.1).fold(hi, u64::max),
    );
    let mut parts: Vec<Seg> = self
      .eye
      .parts
      .iter()
      .copied()
      .filter(|&(id, n)| id + n <= cover.0 || id >= cover.1)
      .collect();
    parts.extend(memory::tiles(cover.0, cover.1.min(count)).map(|p| (p.span().0, p.n())));
    parts.extend((from..cover.0).map(|k| (k, 1)));
    parts.extend((from.max(cover.1)..count).map(|k| (k, 1)));
    parts.sort_by_key(|s| s.0);
    let was = std::mem::replace(&mut self.shown, parts.iter().copied().collect());
    let new: Vec<Seg> = parts.iter().copied().filter(|p| !was.contains(p)).collect();
    self.retell(&new);
    self.teach(&parts);
    self.all(Event::View { parts, turn: from });
  }

  /// Parts a window may have dropped: `teach` tells them again.
  fn retell(&mut self, parts: &[Seg]) {
    for part in parts {
      self.seen.remove(part);
    }
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
        whole.push((
          id,
          Entry {
            kind: m.kind,
            text: m.text,
            date: m.date,
            images: m.images,
            spend: self.turn_use(id),
            via: None,
          },
        ));
      }
      nodes.extend(self.node((id, n)));
    }
    if !whole.is_empty() || !nodes.is_empty() {
      self.all(Event::Know { whole, nodes });
    }
  }

  /// A summary, if built.
  fn node(&self, (id, n): Seg) -> Option<(Seg, Node)> {
    let text = self.mem.node(Part::of(id, n)?)?;
    let to = self.mem.message(id + n - 1)?.date;
    Some(((id, n), Node { text, to }))
  }

  /// A summary built, told if the app was shown its block.
  pub(super) fn made(&mut self, part: Part) {
    let seg = (part.span().0, part.n());
    if self.seen.contains(&seg)
      && let Some(node) = self.node(seg)
    {
      self.all(Event::Know {
        whole: vec![],
        nodes: vec![node],
      });
    }
  }

  /// A part opened into its halves.
  pub(super) fn zoom(&mut self, (id, n): Seg) {
    if n > 1 && self.mem.part(id, n).is_some() {
      let parts = vec![(id, n / 2), (id + n / 2, n / 2)];
      self.retell(&parts);
      self.teach(&parts);
      self.all(Event::Zoom { seg: (id, n), parts });
    }
  }

  /// The tree's roots: the chat in the fewest blocks.
  pub(super) fn root(&mut self) {
    let parts: Vec<Seg> = memory::roots(self.mem.count)
      .into_iter()
      .map(|p| (p.span().0, p.n()))
      .collect();
    self.retell(&parts);
    self.teach(&parts);
    self.all(Event::Root { parts });
  }
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
  /// messages, the summaries, the agents' logs, then their attachments, each
  /// in base64.
  pub(super) async fn export(self: &Arc<Self>) -> Result<PathBuf, String> {
    let file = crate::config::home()
      .join("Downloads")
      .join(format!("acorn-memory-{}.json", stamp()));
    let me = self.clone();
    let wrote = tokio::task::spawn_blocking(move || me.dump(&file).map(|()| file)).await;
    wrote
      .map_err(|err| err.to_string())?
      .map_err(|err| format!("The memory was not saved: {err}."))
  }

  fn dump(&self, file: &std::path::Path) -> anyhow::Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(file)?);
    let (id, count) = {
      let chat = self.lock();
      (chat.id.clone(), chat.mem.count)
    };
    let mut images = std::collections::BTreeSet::new();
    write!(out, "{{\"chat\": {},\n\"messages\": [", serde_json::to_string(&id)?)?;
    for from in (0..count).step_by(PIECE as usize) {
      let chat = self.lock();
      for i in from..(from + PIECE).min(count) {
        let Some(m) = chat.mem.message(i) else { continue };
        let rec = serde_json::json!({"id": i, "kind": m.kind, "text": m.text, "date": m.date, "images": m.images, "more": m.more});
        images.extend(m.images);
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
            write!(
              out,
              "{sep}{}",
              serde_json::json!({"id": part.span().0, "n": n, "text": text})
            )?;
            sep = ",\n";
          }
        }
      }
    }
    write!(out, "],\n\"agents\": {{")?;
    sep = "\n";
    let agents = self.lock().mem.agent_names();
    for name in &agents {
      let Some(log) = self.lock().mem.agent(name) else {
        continue;
      };
      let entries: Vec<_> = log
        .iter()
        .map(|r| serde_json::json!({"id": r.i, "kind": r.kind, "text": r.text, "date": r.date}))
        .collect();
      write!(
        out,
        "{sep}{}: {}",
        serde_json::to_string(name)?,
        serde_json::to_string(&entries)?
      )?;
      sep = ",\n";
    }
    write!(out, "}},\n\"images\": {{")?;
    sep = "\n";
    for name in images {
      let Some(path) = self.lock().mem.image_file(&name) else {
        continue;
      };
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
    let made = tokio::task::spawn_blocking(move || me.page(&file).map(|()| file)).await;
    let file = made
      .map_err(|err| err.to_string())?
      .map_err(|err| format!("No tree page: {err}."))?;
    let _ = tokio::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" })
      .arg(&file)
      .status()
      .await;
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
      if a == b {
        first
      } else {
        format!("{first} – {}", date(chat, b))
      }
    };
    let node = |chat: &mut Chat, part: Part, text: &str| {
      let (a, b) = part.span();
      item(
        &[
          format!("LVL{} #{a}-{b}", part.l),
          when(chat, a, b),
          format!("{} B", text.len()),
        ],
        text,
      )
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
        let head = [
          format!("ROOT #{i}"),
          when(&mut chat, i, i),
          r.kind.as_str().to_string(),
          format!("{} B", r.size),
        ];
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Kind, memory::Msg};

  #[tokio::test]
  async fn export_contains_only_referenced_attachments_even_during_undo_grace() {
    let dir = tempfile::tempdir().unwrap();
    let engine = crate::chat::tests::open(dir.path());
    let public = memory::media_add(dir.path(), b"shared").unwrap();
    let private = memory::media_add(dir.path(), b"held").unwrap();
    let _orphan = memory::media_add(dir.path(), b"no message references this").unwrap();
    engine
      .lock()
      .mem
      .push(vec![
        Msg {
          images: vec![public.clone(), private.clone()],
          ..Msg::new(Kind::User, "first")
        },
        Msg {
          images: vec![public.clone()],
          ..Msg::new(Kind::User, "second")
        },
      ])
      .unwrap();
    engine.lock().purge(None, 0, false);
    let exported = || {
      let file = dir.path().join("export.json");
      engine.dump(&file).unwrap();
      let json: serde_json::Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
      json["images"].as_object().unwrap().clone()
    };
    let images = exported();
    assert_eq!(images.len(), 1);
    assert_eq!(images[&public], "c2hhcmVk");
    engine.lock().purge(None, 0, true);
    assert_eq!(exported()[&private], "aGVsZA==");
  }
}
