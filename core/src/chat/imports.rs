//! Imports
//! =======
//! A person's past chats, found (`import::find`) and priced as the
//! assistant asks (the `import` tool), then, once the turn that asked
//! ends, logged as one: every page `more` but the last, so none is in the
//! chat till all are. Their summaries build on their own side, which a
//! pause holds; a quit leaves the batch's file, and the next start goes on
//! with it. Refused amid another turn or import.

use super::{Chat, Engine};
use crate::Kind;
use crate::ai::Out;
use crate::import::{self, Done, Want};
use crate::memory::{JOBS, Msg, Part};
use crate::wire::{Event, Progress};
use std::path::PathBuf;
use std::sync::Arc;

/// An import running: its batch's file in the chat's dir.
const BATCH: &str = "import.json";

/// Why an import's summaries wait, as told and kept: the person paused it.
const PAUSED: &str = "paused";

/// How many messages are logged at once as an import goes in.
const CHUNK: usize = 1024;

/// How many sessions a find lists, at most.
const LISTED: usize = 200;

/// An import as kept: messages [a, b), those sent (not notes), USD spent;
/// as run: by level what each node costs (micro-USD, `import::guess`) and
/// how many are not built; their cost; its time in all.
pub struct Batch {
  pub a: u64,
  pub b: u64,
  said: u64,
  spent: f64,
  micros: Vec<Vec<u64>>,
  open: Vec<u64>,
  left: f64,
  all: f64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
  a: u64,
  b: u64,
  said: u64,
  spent: f64,
  #[serde(default)]
  why: Option<String>,
}

impl Batch {
  /// Node j of level l built (none such: the chat's).
  pub fn built(&mut self, l: usize, j: i64) {
    if let Some(&cost) = usize::try_from(j).ok().and_then(|j| self.micros.get(l)?.get(j)) {
      self.left -= cost as f64;
      self.open[l] -= 1;
    }
  }

  /// Its time left in micro-USD (a call's, as its cost, is its thinking):
  /// its cost left over JOBS, then each level not built's last call (the
  /// tree's top builds a call a level).
  fn time(&self) -> f64 {
    let tops: u64 = self
      .micros
      .iter()
      .zip(&self.open)
      .filter(|(_, o)| **o > 0)
      .filter_map(|(level, _)| level.last())
      .sum();
    self.left / JOBS as f64 + tops as f64
  }
}

impl Engine {
  /// The `import` tool: find, start (as the turn ends), pause, resume.
  pub(super) async fn import_tool(self: &Arc<Self>, input: &serde_json::Value) -> Out {
    let strs = |k: &str| -> Vec<String> {
      input[k]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect()
    };
    match input["do"].as_str().unwrap_or_default() {
      "find" => {
        let home = crate::config::home();
        let files: Vec<PathBuf> = strs("files")
          .iter()
          .map(|f| f.strip_prefix("~/").map_or_else(|| PathBuf::from(f), |r| home.join(r)))
          .collect();
        match tokio::task::spawn_blocking(move || import::find(&files))
          .await
          .unwrap_or_else(|e| Err(e.to_string()))
        {
          Ok(sessions) => {
            let mut chat = self.lock();
            chat.found = sessions;
            Out::ok(chat.listed())
          }
          Err(why) => Out::fail(why),
        }
      }
      "start" => {
        let want = Want {
          off: strs("skip"),
          since: input["since"].as_str().unwrap_or_default().trim().into(),
          tools: input["tools"].as_bool().unwrap_or(false),
        };
        let mut chat = self.lock();
        if chat.found.is_empty() {
          return Out::fail("Nothing found yet: find first.");
        }
        if chat.batch.is_some() {
          return Out::fail("An import is under way already.");
        }
        let done = chat.imported();
        let plan = import::plan(&chat.found, &want, &done, chat.mem.count);
        if plan.msgs == 0 {
          return Out::fail("Nothing to import: no new messages as asked.");
        }
        chat.then = Some(want);
        Out::ok(format!(
          "It begins as your turn ends: {}, about {}.",
          import::count(plan.msgs, "new "),
          import::guess_usd(plan.usd)
        ))
      }
      "pause" => Out::ok(self.lock().go(false)),
      "resume" => Out::ok(self.lock().go(true)),
      other => Out::fail(format!("No import do {other:?}: find, start, pause or resume.")),
    }
  }

  /// The sessions picked, logged as one, summarized on the compactor; a
  /// turn at work begins it as it ends.
  pub(super) async fn import(self: Arc<Self>, want: Want) {
    let (picked, a) = {
      let mut chat = self.lock();
      if chat.busy() {
        // a turn began meanwhile: its end begins the import
        chat.then = Some(want);
        return;
      }
      if chat.batch.is_some() {
        chat.all(Event::Note("An import is under way: import again once it ends.".into()));
        return;
      }
      let done = chat.imported();
      let plan = import::plan(&chat.found, &want, &done, chat.mem.count);
      if plan.msgs == 0 {
        chat.all(Event::Note("Nothing to import: no new messages found.".into()));
        return;
      }
      (plan.picked, chat.mem.count)
    };
    let me = self.clone();
    self
      .writing(async move || {
        me.lock().mem.paused = false;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<import::Msg>>(4);
        tokio::task::spawn_blocking(move || {
          let mut chunk = Vec::with_capacity(CHUNK);
          for s in &picked {
            s.body(&mut |m| {
              chunk.push(m);
              if chunk.len() == CHUNK {
                let _ = tx.blocking_send(std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK)));
              }
            });
          }
          let _ = tx.blocking_send(chunk);
        });
        let mut said = 0;
        let mut last: Option<import::Msg> = None;
        let tag = |m: import::Msg, more: bool| Msg {
          date: Some(m.date),
          imported: true,
          more,
          ..Msg::new(m.kind, m.text)
        };
        while let Some(chunk) = rx.recv().await {
          said += chunk.iter().filter(|m| m.kind != Kind::Note).count() as u64;
          let mut msgs: Vec<Msg> = last.take().into_iter().chain(chunk).map(|m| tag(m, true)).collect();
          let held = msgs.pop().map(|m| import::Msg {
            kind: m.kind,
            text: m.text,
            date: m.date.unwrap_or_default(),
            more: false,
          });
          last = held;
          if !msgs.is_empty()
            && let Err(err) = me.lock().mem.push(msgs)
          {
            eprintln!("acorn: import: {err}");
          }
        }
        let mut chat = me.lock();
        if let Some(m) = last
          && let Err(err) = chat.mem.push(vec![tag(m, false)])
        {
          eprintln!("acorn: import: {err}");
        }
        let b = chat.mem.count;
        chat.batch = Some(chat.batch_of(a, b, said, 0.0));
        chat.show();
        chat.pump();
        chat.progress();
      })
      .await;
  }
}

impl Chat {
  /// The sessions found, as a model reads them: the import of all, then
  /// a line each.
  fn listed(&mut self) -> String {
    let done = self.imported();
    let want = Want::default();
    let found = import::plan(&self.found, &want, &done, self.mem.count).found(&want);
    if found.rows.is_empty() {
      return import::OLD.into();
    }
    let exports = import::EXPORTS
      .iter()
      .map(|(app, how)| format!("{app}: {how}"))
      .collect::<Vec<_>>()
      .join("; ");
    let mut lines = vec![
      format!(
        "Found {} sessions: {}, from {} on; importing all costs about {}.",
        found.rows.len(),
        import::count(found.msgs, "new "),
        found.first,
        import::guess_usd(found.usd)
      ),
      format!("(Website chats come as an export the user drops on the window: {exports}.)"),
      "id | app | day | messages | where".into(),
    ];
    lines.extend(found.rows.iter().take(LISTED).map(|r| {
      format!(
        "{} | {} | {} | {} | {}",
        r.id, r.app, r.cells[0], r.cells[1], r.cells[2]
      )
    }));
    if found.rows.len() > LISTED {
      lines.push(format!("and {} more.", found.rows.len() - LISTED));
    }
    lines.join("\n")
  }

  /// The sessions the chat imported, read from its log.
  fn imported(&self) -> Done {
    let mut done = Done::new();
    let mut key = String::new();
    for i in 0..self.mem.count {
      let Some(m) = self.mem.message(i) else { continue };
      if !m.imported {
        continue;
      }
      if m.kind == Kind::Note {
        key = import::key(&m.text, &m.date);
      } else if done.get(&key).is_none_or(|d| m.date > *d) {
        done.insert(key.clone(), m.date);
      }
    }
    done
  }

  /// An import run: its estimate, of its messages' sizes as logged, timed
  /// with none built, then its summaries built counted.
  fn batch_of(&self, a: u64, b: u64, said: u64, spent: f64) -> Batch {
    let sizes: Vec<usize> = (a..b).map(|i| self.mem.message(i).map_or(0, |m| m.size)).collect();
    let micros = import::guess(a, &sizes);
    let open = micros.iter().map(|l| l.len() as u64).collect();
    let left = micros.iter().flatten().sum::<u64>() as f64;
    let mut batch = Batch {
      a,
      b,
      said,
      spent,
      micros,
      open,
      left,
      all: 0.0,
    };
    batch.all = batch.time();
    for l in 0..batch.micros.len() {
      for j in 0..batch.micros[l].len() {
        if self.mem.node(Part::new(l as u32, (a >> l) + j as u64)).is_some() {
          batch.built(l, j as i64);
        }
      }
    }
    batch
  }

  /// Kept as it is till it ends: written whole, then moved in place.
  fn keep(&self) {
    let Some(b) = &self.batch else { return };
    let kept = Kept {
      a: b.a,
      b: b.b,
      said: b.said,
      spent: b.spent,
      why: self.paused(),
    };
    let file = self.mem.dir.join(BATCH);
    let new = self.mem.dir.join(format!("{BATCH}.new"));
    let wrote = serde_json::to_vec(&kept)
      .map_err(std::io::Error::other)
      .and_then(|json| std::fs::write(&new, json))
      .and_then(|_| std::fs::rename(&new, &file));
    if let Err(err) = wrote {
      eprintln!("acorn: {BATCH}: {err}");
    }
  }

  /// The import a quit left (BATCH); None: none.
  pub(super) fn resumed(&mut self) -> Option<Batch> {
    let kept: Kept = serde_json::from_slice(&std::fs::read(self.mem.dir.join(BATCH)).ok()?).ok()?;
    self.mem.paused = kept.why.as_deref() == Some(PAUSED);
    Some(self.batch_of(kept.a, kept.b, kept.said, kept.spent))
  }

  /// Why the import's summaries wait, as told and kept (None: they build).
  fn paused(&self) -> Option<String> {
    self.mem.paused.then(|| PAUSED.into())
  }

  /// An import's progress, as told.
  pub(super) fn importing(&self) -> Option<Progress> {
    let b = self.batch.as_ref()?;
    Some(Progress {
      done: b.all - b.time(),
      total: b.all,
      spent: b.spent,
      left: b.left / 1e6,
      why: self.paused(),
      msgs: b.b - b.a,
      open: b.open.clone(),
    })
  }

  /// As the views change: the state; done when all are built.
  pub(super) fn progress(&mut self) {
    let Some(batch) = &self.batch else { return };
    if self.mem.done {
      return;
    }
    if batch.open.iter().all(|&n| n == 0) {
      return self.digest();
    }
    self.keep();
    self.tell_state();
  }

  fn digest(&mut self) {
    let Some(batch) = self.batch.take() else { return };
    let _ = std::fs::remove_file(self.mem.dir.join(BATCH));
    self.eye = self.eye_from(self.eye.from.max(batch.b));
    self.show();
    self.all(Event::Note(format!(
      "Imported {}.",
      import::count(batch.said as usize, "")
    )));
    self.change();
  }

  /// The import paused, at once (its calls cut), or resumed; said so.
  pub(super) fn go(&mut self, on: bool) -> String {
    if self.batch.is_none() {
      return "No import is running.".into();
    }
    self.mem.paused = !on;
    if !on {
      for (call, _, kind) in &self.compactions {
        if *kind == crate::Served::Import {
          call.stop();
        }
      }
    }
    self.keep();
    self.tell_state();
    self.pump();
    self.change();
    if on {
      "The import goes on.".into()
    } else {
      "The import is paused.".into()
    }
  }
}
