//! Usage
//! =====
//! Each request a call of the chat makes is counted as it ends (`ai::Meter`):
//! kept in chat.db, summed into the running turn's if it serves the turn
//! (its call, its goal check), and today's total told. A turn ending puts
//! its sum on its last message. What zooms and searches found (`looks`,
//! the memory's) is told beside, a day a line. Past the config's `budget`, today's spend
//! of every chat together, archived ones too, no request goes (`ai::agent`).

use super::{Chat, hub};
use crate::ai::Used;
use crate::wire::Event;
use crate::{Served, Spend};
use anyhow::Context as _;
use chrono::{Local, TimeZone};
use parking_lot::{RwLock, RwLockUpgradableReadGuard};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::sync::atomic::{AtomicU64, Ordering};

/// Local midnight and micro-USD since then. Read from disk once per day;
/// a meter holds a shared lock across its commit so a rebuild cannot miss
/// or double-count it. Checking a warm cap never opens a database.
static DAY: RwLock<Option<(i64, AtomicU64)>> = RwLock::new(None);

fn read_chats(mut read: impl FnMut(&Connection) -> rusqlite::Result<()>) -> anyhow::Result<()> {
  for path in hub::databases()? {
    Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
      .and_then(|db| read(&db))
      .with_context(|| format!("reading usage in {}", path.display()))?;
  }
  Ok(())
}

/// A closed chat can predate either accounting table; do not migrate it.
fn has(db: &Connection, table: &str) -> rusqlite::Result<bool> {
  db.query_row(
    "select exists(select 1 from sqlite_schema where type = 'table' and name = ?)",
    [table],
    |r| r.get(0),
  )
}

/// Its tables in chat.db (the memory's, `memory`): each request its calls
/// made, what each turn used, by its last message.
pub(super) fn tables(db: &rusqlite::Connection) -> rusqlite::Result<()> {
  db.execute_batch(
        "create table if not exists uses  (at integer not null, kind text not null, input integer not null, read integer not null, output integer not null, micros integer not null);
        create index if not exists uses_at on uses (at);
        create table if not exists turns (msg integer primary key, use text not null);",
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
  n: u64,
  input: u64,
  read: u64,
  output: u64,
  usd: f64,
  at: i64,
}

impl From<Kept> for Spend {
  fn from(k: Kept) -> Spend {
    Spend {
      n: k.n,
      input: k.input,
      read: k.read,
      output: k.output,
      usd: k.usd,
      at: k.at,
    }
  }
}

/// A day's use of a kind of call: its day (YYYY-MM-DD, here), what the
/// calls served, their sum.
struct Usage {
  day: String,
  kind: String,
  spend: Spend,
}

/// Every chat's use, the last `days` days (here), as a model reads it: a
/// line a day and kind, newest first, then their total; then a line a day
/// of what zooms and searches found.
pub(super) fn told(days: i64) -> anyhow::Result<String> {
  let mut all: std::collections::BTreeMap<(String, String), Spend> = Default::default();
  let mut looks: std::collections::BTreeMap<String, [i64; 4]> = Default::default();
  let from = crate::now_ms() - days * 86_400_000;
  read_chats(|db| {
    for u in usage(db, from)? {
      let at = all.entry((u.day, u.kind)).or_default();
      *at = *at + u.spend;
    }
    for (day, n) in memory_looks(db, from)? {
      let at = looks.entry(day).or_default();
      for k in 0..4 {
        at[k] += n[k];
      }
    }
    Ok(())
  })?;
  let looked = looks.iter().rev().map(|(day, [found, lost, hit, missed])| {
    format!("{day} memory: {found} zooms found, {lost} not; {hit} searches with hits, {missed} without")
  });
  if all.is_empty() {
    return Ok(
      std::iter::once("No usage in these days.".to_string())
        .chain(looked)
        .collect::<Vec<_>>()
        .join("\n"),
    );
  }
  let kind = |k: &str| -> String {
    match k {
      "chat" => "replies",
      "compactor" => "summaries",
      "import" => "import summaries",
      "agent" => "agents",
      k => k,
    }
    .to_string()
  };
  let line = |what: String, s: &Spend| {
    format!(
      "{what}: {} calls, {} tokens in ({} cached), {} out, ${:.4}",
      s.n,
      s.input + s.read,
      s.read,
      s.output,
      s.usd
    )
  };
  let total = all.values().fold(Spend::default(), |a, s| a + *s);
  let mut lines: Vec<String> = all
    .iter()
    .rev()
    .map(|((day, k), s)| line(format!("{day} {}", kind(k)), s))
    .collect();
  lines.push(line(format!("All {days} days"), &total));
  lines.extend(looked);
  Ok(lines.join("\n"))
}

fn midnight() -> Option<i64> {
  let start = Local::now().date_naive().and_hms_opt(0, 0, 0)?;
  Some(Local.from_local_datetime(&start).earliest()?.timestamp_millis())
}

fn daily(from: i64) -> anyhow::Result<u64> {
  let day = DAY.upgradable_read();
  if let Some((start, micros)) = &*day
    && *start == from
  {
    return Ok(micros.load(Ordering::Relaxed));
  }
  let mut day = RwLockUpgradableReadGuard::upgrade(day);
  let mut micros = 0;
  read_chats(|db| {
    if has(db, "uses")? {
      micros += db.query_row("select coalesce(sum(micros), 0) from uses where at >= ?", [from], |r| {
        r.get::<_, u64>(0)
      })?;
    }
    Ok(())
  })?;
  *day = Some((from, AtomicU64::new(micros)));
  Ok(micros)
}

/// Why no request may go now: every chat's spend today has reached
/// the config's budget (None: under it, or no budget).
pub fn over_budget() -> Option<String> {
  let cap = crate::config::get().budget?;
  match daily(midnight()?) {
    Ok(micros) => over(cap, micros as f64 / 1e6),
    Err(e) => Some(format!("Cannot check the daily budget: {e:#}")),
  }
}

/// Why today's spend is past a cap (None: it is under).
fn over(cap: f64, spent: f64) -> Option<String> {
  (spent >= cap).then(|| {
    format!(
      "today's spend, ${spent:.2}, has reached the daily budget of ${cap:.2} (budget in config.json); calls go again tomorrow, or once it is raised"
    )
  })
}

impl Chat {
  pub(super) fn used(&mut self, kind: Served, part: Option<crate::memory::Part>, used: Used) {
    let input = used.usage.input + used.usage.write;
    let row = params![
      used.at,
      kind.as_str(),
      input,
      used.usage.read,
      used.usage.output,
      used.micros
    ];
    let batch = self
      .batch
      .as_mut()
      .filter(|b| kind == Served::Import && part.is_some_and(|p| (b.a..b.b).contains(&p.span().1)));
    let usd = used.micros as f64 / 1e6;
    let logged = self.mem.db.transaction().and_then(|tx| {
      tx.execute("insert into uses values (?, ?, ?, ?, ?, ?)", row)?;
      if batch.is_some() {
        tx.execute("update import_batch set spent = spent + ?", [usd])?;
      }
      let day = crate::config::get().budget.map(|_| DAY.read());
      tx.commit()?;
      if let Some((start, total)) = day.as_deref().and_then(Option::as_ref)
        && used.at >= *start
      {
        total.fetch_add(used.micros, Ordering::Relaxed);
      }
      Ok(())
    });
    match (logged, batch) {
      (Ok(()), Some(b)) => b.spent += usd,
      (Err(err), _) => eprintln!("acorn: usage: {err}"),
      _ => {}
    }
    if kind == Served::Chat && self.busy() {
      let spend = Spend {
        n: 1,
        input,
        read: used.usage.read,
        output: used.usage.output,
        usd,
        at: used.at,
      };
      self.spend = self.spend + spend;
    }
    self.tell_state();
  }

  /// The turn's use, on its last message once all it logs is; one ending
  /// where another did (nothing logged since) adds to it.
  pub(super) fn spent(&mut self) {
    let spend = std::mem::take(&mut self.spend);
    if spend.n == 0 {
      return;
    }
    self.hold(move |chat| {
      let id = chat.mem.count.saturating_sub(1);
      let spend = chat.turn_use(id).unwrap_or_default() + spend;
      let kept = Kept {
        n: spend.n,
        input: spend.input,
        read: spend.read,
        output: spend.output,
        usd: spend.usd,
        at: spend.at,
      };
      let json = serde_json::to_string(&kept).unwrap_or_default();
      if let Err(err) = chat
        .mem
        .db
        .execute("insert or replace into turns values (?, ?)", params![id, json])
      {
        eprintln!("acorn: usage: {err}");
      }
      chat.all(Event::Use { id, spend });
    });
  }

  /// What the turn ending at message id used (None: none ended there).
  pub(super) fn turn_use(&self, id: u64) -> Option<Spend> {
    let json: String = self
      .mem
      .db
      .query_row("select use from turns where msg = ?", [id], |r| r.get(0))
      .optional()
      .ok()??;
    serde_json::from_str::<Kept>(&json).ok().map(Spend::from)
  }

  /// What the chat's calls used since `from` (ms; None: none).
  pub(super) fn spend_since(&self, from: i64) -> Option<Spend> {
    let sql = "select count(*), sum(input), sum(read), sum(output), sum(micros), max(at) from uses where at >= ?";
    let spend = self
      .mem
      .db
      .query_row(sql, [from], |r| {
        let micros: Option<i64> = r.get(4)?;
        Ok(Spend {
          n: r.get::<_, i64>(0)? as u64,
          input: r.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
          read: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
          output: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
          usd: micros.unwrap_or(0) as f64 / 1e6,
          at: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
        })
      })
      .ok()?;
    (spend.n > 0).then_some(spend)
  }

  /// What every call used today, here.
  pub(super) fn today(&self) -> Option<Spend> {
    self.spend_since(midnight()?)
  }
}

/// The last `days` days' zooms and searches, by day (here): zooms that
/// found a line or message and that didn't, searches with hits and
/// without.
fn memory_looks(db: &Connection, from: i64) -> rusqlite::Result<Vec<(String, [i64; 4])>> {
  if !has(db, "looks")? {
    return Ok(Vec::new());
  }
  let sql = "select date(at / 1000, 'unixepoch', 'localtime') day, sum(tool = 'zoom' and hits > 0), sum(tool = 'zoom' and hits = 0),
            sum(tool = 'search' and hits > 0), sum(tool = 'search' and hits = 0) from looks where at >= ? group by day";
  db.prepare(sql).and_then(|mut q| {
    q.query_map([from], |r| {
      Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]))
    })?
    .collect()
  })
}

/// The last `days` days' use, by day (here) and kind, newest first.
fn usage(db: &Connection, from: i64) -> rusqlite::Result<Vec<Usage>> {
  if !has(db, "uses")? {
    return Ok(Vec::new());
  }
  let sql = "select date(at / 1000, 'unixepoch', 'localtime') day, kind, count(*), sum(input), sum(read), sum(output), sum(micros), max(at)
            from uses where at >= ? group by day, kind order by day desc, sum(micros) desc";
  db.prepare(sql).and_then(|mut q| {
    q.query_map([from], |r| {
      Ok(Usage {
        day: r.get(0)?,
        kind: r.get(1)?,
        spend: Spend {
          n: r.get::<_, i64>(2)? as u64,
          input: r.get::<_, i64>(3)? as u64,
          read: r.get::<_, i64>(4)? as u64,
          output: r.get::<_, i64>(5)? as u64,
          usd: r.get::<_, i64>(6)? as f64 / 1e6,
          at: r.get(7)?,
        },
      })
    })?
    .collect()
  })
}

#[cfg(test)]
mod tests {
  #[tokio::test]
  async fn archived_chats_still_count() {
    use super::*;
    use crate::{assistant::Assistant, effort::Effort};
    use serde_json::json;

    const PHASE: &str = "ACORN_TEST_ARCHIVED_SPEND";
    let Ok(phase) = std::env::var(PHASE) else {
      let dir = tempfile::tempdir().unwrap();
      for phase in ["archive", "restart"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
          .args([
            "--exact",
            "chat::usage::tests::archived_chats_still_count",
            "--nocapture",
          ])
          .env_clear()
          .env("HOME", dir.path())
          .env("ACORN_DIR", dir.path())
          .env("ACORN_BUDGET", "1.00")
          .env(PHASE, phase)
          .status()
          .unwrap();
        assert!(status.success(), "{phase}");
      }
      return;
    };

    let (tx, _events) = tokio::sync::mpsc::unbounded_channel();
    assert!(hub::listen(tx));
    let me = Assistant::acorn();
    let main = hub::chat(&me).unwrap();
    let kit = main.lock().kit(None);
    let call = async |name: &str, args| {
      let out = kit.call(name.into(), args, Default::default()).await;
      assert!(!out.fail, "{name}: {}", out.text);
      out.text
    };
    let check = async || {
      assert!(over_budget().unwrap().contains("$1.10"));
      let report = call("usage", json!({"days":1})).await;
      assert!(
        report.contains("All 1 days: 4 calls, 0 tokens in (0 cached), 0 out, $1.1000"),
        "{report}"
      );
      assert!(
        report.contains("1 zooms found") && report.contains("1 searches with hits"),
        "{report}"
      );
    };
    if phase == "archive" {
      assert!(over_budget().is_none());
      call(
        "create_assistant",
        json!({"name":"Scout","about":"test","effort":"low"}),
      )
      .await;
      let scout = crate::assistant::find("Scout").unwrap();
      let ours = hub::thread(&me, &scout).unwrap();
      let theirs = hub::thread(&scout, &me).unwrap();
      ours.lock().mem.look("zoom", 1);
      theirs.lock().mem.look("search", 1);
      let at = crate::now_ms();
      for (engine, micros, at) in [
        (&main, 5_000_000, at - 2 * 86_400_000),
        (&main, 400_000, at),
        (&hub::chat(&scout).unwrap(), 350_000, at),
        (&ours, 200_000, at),
        (&theirs, 150_000, at),
      ] {
        let meter = engine.lock().meter(Served::Chat);
        meter(Used {
          effort: Effort::Low,
          usage: Default::default(),
          micros,
          at,
        });
      }
      check().await;
      call("archive_assistant", json!({"name":"Scout"})).await;
    }
    check().await;
    for id in ["scout", "scout/default", "default/scout"] {
      assert!(hub::get(id).is_none(), "usage resumed {id}");
    }
    if phase == "restart" {
      call("restore_assistant", json!({"name":"Scout"})).await;
      check().await;
    }
  }

  #[test]
  fn a_daily_budget_is_reached_at_the_cap() {
    assert_eq!(super::over(5.0, 4.99), None);
    let why = super::over(5.0, 5.004).unwrap();
    assert!(why.contains("$5.00") && why.contains("daily budget of $5.00"), "{why}");
  }
}
