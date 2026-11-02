//! Views
//! =====
//! The chat's view and the compactions', fit to their bytes by merging
//! the most due pairs, and what calls and zooms read of them.

use super::*;
use std::collections::BinaryHeap;

impl Memory {
  /// A node of the views or above them: its text, or WAIT if not built.
  pub fn text(&self, part: Part) -> &str {
    self.nodes.get(&part.slot()).map_or(WAIT, String::as_str)
  }

  /// A line's bytes as calls get it: none until built.
  pub(super) fn size(&self, part: Part) -> usize {
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
    let sides: Vec<u64> = [0, 1]
      .into_iter()
      .filter(|&s| self.hold[s].is_none())
      .map(|s| self.first(0, s))
      .collect();
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
      let (Some(a), Some(Some(b))) = (view[k], view.get(next[k]).copied()) else {
        return;
      };
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
    Merged {
      parts: view.into_iter().flatten().collect(),
      size,
      gone,
    }
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
      blocks.push(crate::Block::new(
        lines[k * BLOCK..(k + 1) * BLOCK].concat(),
        k == whole - 1,
      ));
    }
    blocks.push(crate::Block::new(lines[whole * BLOCK..].concat() + "</chat>\n", false));
    blocks
  }

  /// The view's last `n` lines, oldest first; a message not summarized
  /// yet as its kind and its text's first NODE bytes, so the latest show
  /// at once. Only those lines are read.
  pub fn view_lines(&self, n: usize) -> Vec<String> {
    let shown = |p: Part| match (self.built(p), p.n(), self.message(p.i)) {
      (true, ..) => line(p, self.text(p)),
      (false, 1, Some(m)) => line(p, cut(&format!("{}: {}", m.kind.as_str(), flat(&m.text)))),
      _ => line(p, WAIT),
    };
    let parts = &self.view.parts;
    parts[parts.len().saturating_sub(n)..]
      .iter()
      .map(|&p| shown(p))
      .collect()
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
      self.look("zoom", 0);
      let roots: Vec<String> = roots(self.count).into_iter().map(Part::name).collect();
      return format!(
        "No line {id}+{n}: n is a power of 2, id a multiple of n. The tree's roots: {}.",
        roots.join(", ")
      );
    };
    self.look("zoom", 1);
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

  /// Up to n messages of a log (CHAT, or an agent's) whose text matches
  /// an FTS5 query, newest first, each "id|kind: snippet" around its hit;
  /// a query FTS5 can't read, why. Logged with its hits (`look`).
  pub fn search(&self, log: &str, query: &str, n: usize) -> String {
    if query.trim().is_empty() {
      return "Give a query: a word or name the message holds.".into();
    }
    let end = if log == CHAT { self.count } else { u64::MAX >> 1 };
    let sql =
      "select s.i, m.kind, snippet(search, 2, '', '', '…', 24) from search s join msgs m on m.log = s.log and m.i = s.i
        where search match ?1 and s.log = ?2 and s.i < ?3 order by s.i desc limit ?4";
    let hits = self.db.prepare_cached(sql).and_then(|mut q| {
      q.query_map(params![query, log, end, n.clamp(1, HITS)], |r| {
        Ok(format!(
          "{}|{}: {}",
          r.get::<_, u64>(0)?,
          r.get::<_, String>(1)?,
          flat(&r.get::<_, String>(2)?)
        ))
      })?
      .collect::<rusqlite::Result<Vec<String>>>()
    });
    self.look("search", hits.as_ref().map_or(0, Vec::len));
    match hits {
      Ok(hits) if hits.is_empty() => format!("No message matches {query}."),
      Ok(hits) => hits.join("\n"),
      Err(err) => format!(
        "Can't read the query {query}: {err}. Give words (all must match), \"an exact phrase\", word* for a prefix, OR between alternatives."
      ),
    }
  }

  pub fn date(&self, id: u64) -> String {
    self
      .message(id)
      .map_or_else(|| format!("No message {id}."), |root| root.when())
  }

  /// Where a page of the chat starts: the last message starting a turn
  /// (the user's or a report) the view holds whole; none: the first, or
  /// the end.
  pub fn turn(&self) -> u64 {
    let from = self.view.parts.iter().rposition(|p| p.l > 0).map_or(0, |k| k + 1);
    let whole = &self.view.parts[from..];
    let user = whole.iter().rev().find(|p| {
      self
        .message(p.i)
        .is_some_and(|m| matches!(m.kind, Kind::User | Kind::Work))
    });
    user.or_else(|| whole.first()).map_or(self.count, |p| p.i)
  }

  /// Level l's nodes built, in order.
  pub fn level(&self, l: u32) -> Vec<(Part, String)> {
    let n = 1u64 << l;
    (0..self.count / n)
      .filter_map(|i| self.node(Part::new(l, i)).map(|t| (Part::new(l, i), t)))
      .collect()
  }

  /// The tree's levels: as many as the count's bits.
  pub fn levels(&self) -> u32 {
    u64::BITS - self.count.leading_zeros()
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
