//! Imports
//! =======
//! A person's past chats, found (`import::find`) and priced as the
//! assistant asks (the `import` tool), then, once the turn that asked
//! ends, published in one transaction, preserving each message's page
//! boundaries. Their summaries build on their own side, which a
//! pause holds; a quit leaves the batch's row, and the next start goes on
//! with it. Refused amid another turn or import.

use super::{Chat, Engine};
use crate::Kind;
use crate::ai::Out;
use crate::import::{self, Done, Want};
use crate::memory::{JOBS, Part};
use crate::wire::{Event, Progress};
use rusqlite::OptionalExtension;
use std::path::PathBuf;
use std::sync::Arc;

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
  pub(super) spent: f64,
  micros: Vec<Vec<u64>>,
  open: Vec<u64>,
  left: f64,
  all: f64,
  age: u64,
}

impl Batch {
  fn new(a: u64, sizes: &[usize], said: u64, spent: f64, age: u64) -> Self {
    let micros = import::guess(a, sizes);
    let open = micros.iter().map(|l| l.len() as u64).collect();
    let left = micros.iter().flatten().sum::<u64>() as f64;
    let mut batch = Self {
      a,
      b: a + sizes.len() as u64,
      said,
      spent,
      micros,
      open,
      left,
      all: 0.0,
      age,
    };
    batch.all = batch.time();
    batch
  }

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

/// An import's staged pages, let go as it ends, however it ends.
struct Staged<'a>(&'a Engine);

impl Drop for Staged<'_> {
  fn drop(&mut self) {
    self.0.lock().mem.end_import();
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
    let wrote: anyhow::Result<()> = self
      .writing(async || {
        let (picked, expected) = {
          let mut chat = self.lock();
          if chat.busy() {
            // a turn began meanwhile: its end begins the import
            chat.then = Some(want);
            return Ok(());
          }
          if chat.batch.is_some() {
            chat.all(Event::Note("An import is under way: import again once it ends.".into()));
            return Ok(());
          }
          let done = chat.imported();
          let plan = import::plan(&chat.found, &want, &done, chat.mem.count);
          if plan.msgs == 0 {
            chat.all(Event::Note("Nothing to import: no new messages found.".into()));
            return Ok(());
          }
          chat.mem.begin_import()?;
          (plan.picked, plan.msgs)
        };
        let _staged = Staged(&self);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<import::Msg>>(4);
        let reader = tokio::task::spawn_blocking(move || {
          let mut chunk = Vec::with_capacity(CHUNK);
          for s in &picked {
            if tx.is_closed() {
              return;
            }
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
        let mut sizes = Vec::with_capacity(expected);
        while let Some(chunk) = rx.recv().await {
          said += chunk.iter().filter(|m| m.kind != Kind::Note).count() as u64;
          sizes.extend(self.lock().mem.stage_import(chunk)?);
        }
        reader.await?;
        anyhow::ensure!(
          said == expected as u64,
          "the selected source messages changed during import"
        );
        let mut chat = self.lock();
        let a = chat.mem.count;
        chat.mem.finish_import(said)?;
        chat.mem.paused = false;
        chat.batch = Some(Batch::new(a, &sizes, said, 0.0, chat.mem.age));
        chat.show();
        chat.pump();
        chat.progress();
        Ok(())
      })
      .await;
    if let Err(err) = wrote {
      self.tell(Event::Note(format!("Import failed: {err}")));
    }
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
    let mut batch = Batch::new(a, &sizes, said, spent, self.mem.age);
    for l in 0..batch.micros.len() {
      for j in 0..batch.micros[l].len() {
        if self.mem.node(Part::new(l as u32, (a >> l) + j as u64)).is_some() {
          batch.built(l, j as i64);
        }
      }
    }
    batch
  }

  /// A purge can invalidate summaries already counted by the active
  /// import. Recount from the tree before publishing its progress.
  pub(super) fn count_built(&mut self, made: &[Part]) {
    let Some(b) = &mut self.batch else { return };
    if b.age != self.mem.age {
      let (a, end, said, spent) = (b.a, b.b, b.said, b.spent);
      self.batch = Some(self.batch_of(a, end, said, spent));
    } else {
      for part in made {
        b.built(part.l as usize, part.i as i64 - (b.a >> part.l) as i64);
      }
    }
  }

  /// The batch published with its messages, unless already completed.
  pub(super) fn resumed(&mut self) -> rusqlite::Result<Option<Batch>> {
    let row = self
      .mem
      .db
      .query_row("select a,b,said,spent,paused from import_batch", [], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
      })
      .optional()?;
    Ok(row.map(|(a, b, said, spent, paused)| {
      self.mem.paused = paused;
      self.batch_of(a, b, said, spent)
    }))
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
    self.tell_state();
  }

  fn digest(&mut self) {
    if let Err(err) = self.mem.db.execute("delete from import_batch", []) {
      eprintln!("acorn: completing import: {err}");
      return;
    }
    let Some(batch) = self.batch.take() else { return };
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
    if let Err(err) = self.mem.db.execute("update import_batch set paused = ?", [!on]) {
      return format!("The import could not be updated: {err}");
    }
    self.mem.paused = !on;
    if !on {
      for (call, _, kind) in &self.compactions {
        if *kind == crate::Served::Import {
          call.stop();
        }
      }
    }
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::chat::tests::open;

  #[tokio::test]
  async fn concurrent_starts_publish_the_selected_export_once() {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("conversations.json");
    std::fs::write(&export, r#"[{"name":"test","chat_messages":[{"sender":"human","text":"morning","created_at":"2025-01-01T08:00:00Z"},{"sender":"assistant","text":"hello","created_at":"2025-01-01T08:01:00Z"}]}]"#).unwrap();
    let engine = open(dir.path());
    engine.lock().found = import::export(&export).unwrap();
    tokio::join!(
      engine.clone().import(Want::default()),
      engine.clone().import(Want::default())
    );
    let chat = engine.lock();
    assert_eq!(chat.mem.count, 3);
    assert_eq!(chat.mem.message(1).unwrap().text, "morning");
    assert_eq!(chat.mem.message(2).unwrap().text, "hello");
    assert!(chat.batch.is_none());
    assert!(chat.mem.node(Part::new(1, 0)).is_some());
  }

  #[tokio::test]
  async fn a_failed_import_leaves_nothing_staged_and_the_next_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("conversations.json");
    std::fs::write(
      &export,
      r#"[{"name":"test","chat_messages":[{"sender":"human","text":"morning","created_at":"2025-01-01T08:00:00Z"}]}]"#,
    )
    .unwrap();
    let engine = open(dir.path());
    engine.lock().found = import::export(&export).unwrap();
    let staged = || {
      let chat = engine.lock();
      let q = "select exists (select 1 from temp.sqlite_master where name = 'import_pages')";
      chat.mem.db.query_row(q, [], |r| r.get::<_, bool>(0)).unwrap()
    };
    let refuse = "create temp trigger refuse before insert on msgs begin select raise(abort, 'refused'); end";
    engine.lock().mem.db.execute_batch(refuse).unwrap();
    engine.clone().import(Want::default()).await;
    assert!(!staged());
    assert!(engine.lock().mem.count == 0 && engine.lock().batch.is_none());
    engine.lock().mem.db.execute_batch("drop trigger refuse").unwrap();
    engine.clone().import(Want::default()).await;
    assert!(!staged());
    assert_eq!(engine.lock().mem.message(1).unwrap().text, "morning");
  }

  #[tokio::test]
  async fn paused_import_recounts_a_purged_summary_and_resumes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(dir.path());
    {
      let mut chat = engine.lock();
      chat.mem.begin_import().unwrap();
      chat
        .mem
        .stage_import(
          ["first", "second"]
            .map(|s| import::Msg {
              kind: Kind::User,
              text: s.into(),
              date: "2025-01-01".into(),
              more: false,
            })
            .to_vec(),
        )
        .unwrap();
      chat.mem.finish_import(2).unwrap();
      chat.batch = chat.resumed().unwrap();
      chat.go(false);
      chat.mem.store(&[(Part::leaf(0), "first".into())]).unwrap();
    }
    for (i, micros) in [(0, 125), (2, 200)] {
      let meter = engine.lock().meter_at(crate::Served::Import, Some(Part::leaf(i)));
      meter(crate::ai::Used {
        effort: crate::effort::Effort::Low,
        usage: Default::default(),
        micros,
        at: crate::now_ms(),
      });
    }
    assert_eq!(engine.lock().importing().unwrap().open, [1, 1]);
    engine.lock().purge(None, 0, false);
    assert_eq!(engine.lock().importing().unwrap().open, [2, 1]);
    engine.close();
    drop(engine);
    let engine = open(dir.path());
    let progress = engine.lock().importing().unwrap();
    assert_eq!(progress.why.as_deref(), Some(PAUSED));
    assert_eq!(progress.spent, 0.000125);
    assert_eq!(progress.open, [2, 1]);
    engine.lock().go(true);
    let mut chat = engine.lock();
    assert!(chat.batch.is_none() && chat.resumed().unwrap().is_none());
    assert!(chat.mem.node(Part::new(1, 0)).unwrap().contains(crate::memory::PURGED));
  }
}
