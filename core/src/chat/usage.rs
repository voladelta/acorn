//! Usage
//! =====
//! Each request a call of the chat makes is counted as it ends (`ai::Meter`):
//! kept in chat.db, summed into the running turn's if it serves the turn
//! (its call, its goal check), and today's total told. A turn ending puts
//! its sum on its last message. What zooms and searches found (`looks`,
//! the memory's) is told beside, a day a line. Past the config's `budget`, today's spend
//! of every open chat together, no request goes (`ai::agent`).

use super::{Chat, hub};
use crate::ai::Used;
use crate::wire::Event;
use crate::{Served, Spend};
use chrono::{Local, TimeZone};
use rusqlite::{OptionalExtension, params};

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
pub(super) struct Usage {
  day: String,
  kind: String,
  spend: Spend,
}

/// Every chat's use, the last `days` days (here), as a model reads it: a
/// line a day and kind, newest first, then their total; then a line a day
/// of what zooms and searches found.
pub(super) fn told(days: i64) -> String {
  let mut all: std::collections::BTreeMap<(String, String), Spend> = Default::default();
  let mut looks: std::collections::BTreeMap<String, [i64; 4]> = Default::default();
  for engine in hub::all() {
    let chat = engine.lock();
    for u in chat.usage(days) {
      let at = all.entry((u.day, u.kind)).or_default();
      *at = *at + u.spend;
    }
    for (day, n) in chat.looks(days) {
      let at = looks.entry(day).or_default();
      for k in 0..4 {
        at[k] += n[k];
      }
    }
  }
  let looked = looks.iter().rev().map(|(day, [found, lost, hit, missed])| {
    format!("{day} memory: {found} zooms found, {lost} not; {hit} searches with hits, {missed} without")
  });
  if all.is_empty() {
    return std::iter::once("No usage in these days.".to_string())
      .chain(looked)
      .collect::<Vec<_>>()
      .join("\n");
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
  lines.join("\n")
}

/// Why no request may go now: every open chat's spend today has reached
/// the config's budget (None: under it, or no budget).
pub fn over_budget() -> Option<String> {
  let cap = crate::config::get().budget?;
  let spent = hub::all().iter().filter_map(|e| e.lock().today()).map(|s| s.usd).sum();
  over(cap, spent)
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
  pub(super) fn used(&mut self, kind: Served, used: Used) {
    let input = used.usage.input + used.usage.write;
    let row = params![
      used.at,
      kind.as_str(),
      input,
      used.usage.read,
      used.usage.output,
      used.micros
    ];
    if let Err(err) = self.mem.db.execute("insert into uses values (?, ?, ?, ?, ?, ?)", row) {
      eprintln!("acorn: usage: {err}");
    }
    if kind == Served::Chat && self.busy() {
      let spend = Spend {
        n: 1,
        input,
        read: used.usage.read,
        output: used.usage.output,
        usd: used.micros as f64 / 1e6,
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
    let midnight = Local::now().date_naive().and_hms_opt(0, 0, 0)?;
    let from = Local.from_local_datetime(&midnight).earliest()?.timestamp_millis();
    self.spend_since(from)
  }

  /// The last `days` days' zooms and searches, by day (here): zooms that
  /// found a line or message and that didn't, searches with hits and
  /// without.
  pub(super) fn looks(&self, days: i64) -> Vec<(String, [i64; 4])> {
    let sql = "select date(at / 1000, 'unixepoch', 'localtime') day, sum(tool = 'zoom' and hits > 0), sum(tool = 'zoom' and hits = 0),
            sum(tool = 'search' and hits > 0), sum(tool = 'search' and hits = 0) from looks where at >= ? group by day";
    let from = crate::now_ms() - days * 86_400_000;
    let rows = self.mem.db.prepare(sql).and_then(|mut q| {
      q.query_map([from], |r| {
        Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]))
      })?
      .collect()
    });
    rows.unwrap_or_else(|err| {
      eprintln!("acorn: looks: {err}");
      Vec::new()
    })
  }

  /// The last `days` days' use, by day (here) and kind, newest first.
  pub(super) fn usage(&self, days: i64) -> Vec<Usage> {
    let sql = "select date(at / 1000, 'unixepoch', 'localtime') day, kind, count(*), sum(input), sum(read), sum(output), sum(micros), max(at)
            from uses where at >= ? group by day, kind order by day desc, sum(micros) desc";
    let from = crate::now_ms() - days * 86_400_000;
    let rows = self.mem.db.prepare(sql).and_then(|mut q| {
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
    });
    rows.unwrap_or_else(|err| {
      eprintln!("acorn: usage: {err}");
      Vec::new()
    })
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn a_daily_budget_is_reached_at_the_cap() {
    assert_eq!(super::over(5.0, 4.99), None);
    let why = super::over(5.0, 5.004).unwrap();
    assert!(why.contains("$5.00") && why.contains("daily budget of $5.00"), "{why}");
  }
}
