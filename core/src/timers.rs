//! Timers
//! ======
//! What the user asked to be told, or done, later: a reminder, said as
//! the assistant's own message when it comes due (no call made, so it is
//! on time and costs nothing), or a task, a message "[Timer #id] what"
//! that starts a turn then. Each fires in the chat that set it, an
//! assistant's with the user; Telegram App's goes to the user's phone.
//! One fires once, at a time or after a while, or again and again on a
//! cron line read in this machine's time zone.
//!
//! They are kept in $ACORN_DIR/timers.json, so a quit loses none: one due
//! while Acorn was down fires as it starts, said to be late, and one that
//! repeats fires once for all it missed.

use crate::chat::hub;
use crate::config;
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{LazyLock, Once};
use std::time::Duration;
use tokio::sync::Notify;

const FILE: &str = "timers.json";
/// A timer fired this long after it was due is said to be late.
const LATE_MS: i64 = 2 * 60_000;
/// The longest the loop sleeps between looks: a clock set anew, or a
/// machine woken, is seen within it.
const LOOK_MS: i64 = 30_000;
/// The longest a cron line is searched ahead for its next time.
const AHEAD_DAYS: i64 = 366 * 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timer {
  pub id: u64,
  /// The chat it fires in.
  pub chat: String,
  /// When it fires next: ms since the epoch.
  pub next: i64,
  pub what: String,
  /// A task (a turn), else a reminder.
  #[serde(default)]
  pub task: bool,
  /// A cron line it fires again on.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub repeat: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
  /// The last id given.
  #[serde(default)]
  last: u64,
  #[serde(default)]
  timers: Vec<Timer>,
}

static BOOK: LazyLock<Mutex<Book>> = LazyLock::new(|| Mutex::new(load()));
/// Told when a timer is set or cancelled: the loop looks again.
static CHANGED: Notify = Notify::const_new();

fn load() -> Book {
  let file = config::get().dir.join(FILE);
  let Ok(text) = std::fs::read_to_string(&file) else {
    return Book::default();
  };
  serde_json::from_str(&text).unwrap_or_else(|err| {
    eprintln!("acorn: {FILE}: {err}");
    Book::default()
  })
}

fn save(book: &Book) -> Result<(), String> {
  let dir = &config::get().dir;
  let text = serde_json::to_string_pretty(book).map_err(|e| e.to_string())?;
  let part = dir.join(format!("{FILE}.part"));
  std::fs::create_dir_all(dir)
    .and_then(|_| std::fs::write(&part, text))
    .and_then(|_| std::fs::rename(&part, dir.join(FILE)))
    .map_err(|e| format!("{FILE}: {e}"))
}

impl Book {
  /// The timers due by `now`, each with how late it is: one that
  /// repeats set to its next time after now, the others gone.
  fn due(&mut self, now: i64) -> Vec<(Timer, i64)> {
    let mut due = Vec::new();
    self.timers.retain_mut(|t| {
      if t.next > now {
        return true;
      }
      due.push((t.clone(), now - t.next));
      match t
        .repeat
        .as_deref()
        .and_then(|r| Cron::parse(r).ok())
        .and_then(|c| c.next_ms(now))
      {
        Some(next) => {
          t.next = next;
          true
        }
        None => false,
      }
    });
    due
  }

  /// When the next timer is due.
  fn soonest(&self) -> Option<i64> {
    self.timers.iter().map(|t| t.next).min()
  }
}

/// The timers' loop, started once (on the runtime): each fired as it comes
/// due, those due while Acorn was down at once.
pub fn start() {
  static STARTED: Once = Once::new();
  STARTED.call_once(|| {
    tokio::spawn(async {
      loop {
        let now = crate::now_ms();
        let (due, soonest) = {
          let mut book = BOOK.lock();
          let due = book.due(now);
          if !due.is_empty()
            && let Err(err) = save(&book)
          {
            eprintln!("acorn: {err}");
          }
          (due, book.soonest())
        };
        for (timer, late) in due {
          fire(timer, late).await;
        }
        let wait = soonest.map_or(LOOK_MS, |at| (at - crate::now_ms()).clamp(0, LOOK_MS));
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(wait as u64)) => {}
            _ = CHANGED.notified() => {}
        }
      }
    });
  });
}

/// A timer come due, in its chat.
async fn fire(timer: Timer, late: i64) {
  let chat = timer.chat.clone();
  let Ok(Some(engine)) = tokio::task::spawn_blocking(move || hub::ready(&chat)).await else {
    eprintln!("acorn: timer #{}: no chat {}", timer.id, timer.chat);
    return;
  };
  let late = if late > LATE_MS {
    format!("\n\n(Late by {}: Acorn wasn't running when it was due.)", span(late))
  } else {
    String::new()
  };
  if timer.task {
    engine.wake(format!("[Timer #{}] {}{late}", timer.id, timer.what));
  } else {
    engine.remind(format!("⏰ {}{late}", timer.what));
  }
}

/// A timer tool of an assistant's chat with the user (`chat`): its answer,
/// or why it failed.
pub fn tool(chat: &str, name: &str, input: &Value) -> Result<String, String> {
  let str = |k: &str| input[k].as_str().unwrap_or_default().trim().to_string();
  let now = crate::now_ms();
  match name {
    "set_timer" => {
      let what = str("what");
      if what.is_empty() {
        return Err("Say what: the reminder's words, or the task to do.".into());
      }
      let repeat = Some(str("repeat")).filter(|r| !r.is_empty());
      let cron = repeat.as_deref().map(Cron::parse).transpose()?;
      let next = match (str("at"), str("in")) {
        (at, after) if !at.is_empty() && !after.is_empty() => return Err("Give at or in, not both.".into()),
        (at, _) if !at.is_empty() => moment(&at)?,
        (_, after) if !after.is_empty() => now
          .checked_add(duration(&after)?)
          .ok_or("That timer is too far away.")?,
        _ => match &cron {
          Some(cron) => cron.next_ms(now).ok_or("That cron line never comes due.")?,
          None => return Err("Say when: at a time, in a while, or repeat on a cron line.".into()),
        },
      };
      if next <= now {
        return Err(format!("{} has passed; it is {} now.", shown(next), shown(now)));
      }
      let task = input["task"].as_bool().unwrap_or(false);
      let mut book = BOOK.lock();
      book.last += 1;
      let timer = Timer {
        id: book.last,
        chat: chat.to_string(),
        next,
        what,
        task,
        repeat,
      };
      book.timers.push(timer.clone());
      save(&book)?;
      drop(book);
      CHANGED.notify_one();
      Ok(format!("Set: {}", line(&timer, now)))
    }
    "list_timers" => {
      let book = BOOK.lock();
      let mut mine: Vec<&Timer> = book.timers.iter().filter(|t| t.chat == chat).collect();
      mine.sort_by_key(|t| t.next);
      Ok(match mine.is_empty() {
        true => "No timers.".into(),
        false => mine.iter().map(|t| line(t, now)).collect::<Vec<_>>().join("\n"),
      })
    }
    "cancel_timer" => {
      let Some(id) = input["id"].as_u64() else {
        return Err("Give id: the timer's number.".into());
      };
      let mut book = BOOK.lock();
      let Some(k) = book.timers.iter().position(|t| t.id == id && t.chat == chat) else {
        return Err(format!("No timer #{id} here; list_timers names them."));
      };
      let gone = book.timers.remove(k);
      save(&book)?;
      drop(book);
      CHANGED.notify_one();
      Ok(format!("Cancelled: {}", line(&gone, now)))
    }
    _ => Err(format!("No tool {name}.")),
  }
}

/// A timer as listed: its id, when, in how long, what, and how it repeats.
fn line(t: &Timer, now: i64) -> String {
  let kind = if t.task { "task" } else { "reminder" };
  let again = t
    .repeat
    .as_deref()
    .map_or_else(String::new, |r| format!("; repeats on {r}"));
  format!(
    "#{} {} (in {}), {kind}: {}{again}",
    t.id,
    shown(t.next),
    span(t.next - now),
    t.what
  )
}

/// A time, as the user reads it here.
fn shown(ms: i64) -> String {
  Local
    .timestamp_millis_opt(ms)
    .single()
    .map_or_else(|| ms.to_string(), |t| t.format("%a %-d %b %Y %H:%M (%:z)").to_string())
}

/// A while, roughly: 3d 4h, 2h 5m, 40s.
fn span(ms: i64) -> String {
  let s = ms.max(0) / 1000;
  let (d, h, m) = (s / 86_400, s / 3600 % 24, s / 60 % 60);
  match (d, h, m) {
    (0, 0, 0) => format!("{s}s"),
    (0, 0, m) => format!("{m}m"),
    (0, h, 0) => format!("{h}h"),
    (0, h, m) => format!("{h}h {m}m"),
    (d, 0, _) => format!("{d}d"),
    (d, h, _) => format!("{d}d {h}h"),
  }
}

/// A moment: RFC 3339 (2026-11-07T09:00:00+07:00), or a time here
/// (2026-11-07 09:00, or with a T, seconds optional).
fn moment(text: &str) -> Result<i64, String> {
  if let Ok(t) = DateTime::parse_from_rfc3339(text) {
    return Ok(t.timestamp_millis());
  }
  let text = text.replace('T', " ");
  ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
    .iter()
    .find_map(|f| chrono::NaiveDateTime::parse_from_str(&text, f).ok())
    .and_then(|n| Local.from_local_datetime(&n).earliest())
    .map(|t| t.timestamp_millis())
    .ok_or_else(|| format!("No time {text:?}: give 2026-11-07T09:00:00+07:00, or 2026-11-07 09:00 here."))
}

/// A while: a number and a unit (s, m, h, d, w), one or more: 90m, 1h30m.
fn duration(text: &str) -> Result<i64, String> {
  let bad = || format!("No while {text:?}: give 90s, 20m, 2h, 1h30m, 3d or 1w.");
  let (mut ms, mut num, mut any) = (0i64, String::new(), false);
  for c in text.chars().filter(|c| !c.is_whitespace()) {
    if c.is_ascii_digit() {
      num.push(c);
      continue;
    }
    let unit = match c.to_ascii_lowercase() {
      's' => 1_000,
      'm' => 60_000,
      'h' => 3_600_000,
      'd' => 86_400_000,
      'w' => 7 * 86_400_000,
      _ => return Err(bad()),
    };
    let n: i64 = std::mem::take(&mut num).parse().map_err(|_| bad())?;
    ms = n.checked_mul(unit).and_then(|v| ms.checked_add(v)).ok_or_else(bad)?;
    any = true;
  }
  if !any || !num.is_empty() || ms == 0 {
    return Err(bad());
  }
  Ok(ms)
}

/// A cron line: minute, hour, day of month, month, day of week (0 or 7
/// Sunday), each *, a number, a range a-b, a step */n or a-b/n, or a list
/// of those; months and days may be named (jan, mon). A day matches its
/// day of month or of week when both are given, as cron's do. @hourly,
/// @daily, @weekly, @monthly and @yearly stand for their lines.
#[derive(Debug, PartialEq)]
pub struct Cron {
  minutes: u64,
  hours: u32,
  days: u32,
  months: u16,
  weekdays: u8,
  any_day: bool,
  any_weekday: bool,
}

const MONTHS: [&str; 12] = [
  "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Cron {
  pub fn parse(line: &str) -> Result<Cron, String> {
    let line = match line.trim() {
      "@hourly" => "0 * * * *",
      "@daily" | "@midnight" => "0 0 * * *",
      "@weekly" => "0 0 * * 0",
      "@monthly" => "0 0 1 * *",
      "@yearly" | "@annually" => "0 0 1 1 *",
      line => line,
    };
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [mi, h, d, mo, w] = fields[..] else {
      return Err(format!(
        "No cron line {line:?}: five fields, minute hour day month weekday, as 0 9 * * 1-5."
      ));
    };
    let weekdays = field(w, 0, 7, &WEEKDAYS, 0)?;
    Ok(Cron {
      minutes: field(mi, 0, 59, &[], 0)?,
      hours: field(h, 0, 23, &[], 0)? as u32,
      days: field(d, 1, 31, &[], 1)? as u32,
      months: field(mo, 1, 12, &MONTHS, 1)? as u16,
      weekdays: ((weekdays | (weekdays >> 7)) & 0x7f) as u8,
      any_day: d == "*",
      any_weekday: w == "*",
    })
  }

  fn day(&self, d: NaiveDate) -> bool {
    let dom = self.days >> d.day() & 1 == 1;
    let dow = self.weekdays >> d.weekday().num_days_from_sunday() & 1 == 1;
    self.months >> d.month() & 1 == 1
      && match (self.any_day, self.any_weekday) {
        (true, true) => true,
        (true, false) => dow,
        (false, true) => dom,
        (false, false) => dom || dow,
      }
  }

  /// Its first time after `after`, to the minute, in `after`'s zone: a
  /// time a clock change skips is passed over, one it repeats taken once.
  pub fn next<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> Option<DateTime<Tz>> {
    let zone = after.timezone();
    let from = after.naive_local().date();
    for k in 0..AHEAD_DAYS {
      let date = from + chrono::Duration::days(k);
      if !self.day(date) {
        continue;
      }
      for h in (0..24).filter(|h| self.hours >> h & 1 == 1) {
        for m in (0..60).filter(|m| self.minutes >> m & 1 == 1) {
          let Some(at) = date
            .and_hms_opt(h, m, 0)
            .and_then(|n| zone.from_local_datetime(&n).earliest())
          else {
            continue;
          };
          if at > *after {
            return Some(at);
          }
        }
      }
    }
    None
  }

  /// Its first time after `ms`, here.
  fn next_ms(&self, ms: i64) -> Option<i64> {
    let after = Local
      .timestamp_millis_opt(ms)
      .single()?
      .with_second(0)?
      .with_nanosecond(0)?;
    let after = after.max(Local.timestamp_millis_opt(ms).single()?);
    self.next(&after).map(|t| t.timestamp_millis())
  }
}

/// A cron field's values as bits, from `lo` to `hi`; names stand for
/// numbers from `base` on.
fn field(text: &str, lo: u32, hi: u32, names: &[&str], base: u32) -> Result<u64, String> {
  let bad = || format!("No cron field {text:?}: from {lo} to {hi}.");
  let num = |s: &str| -> Result<u32, String> {
    let s = s.to_ascii_lowercase();
    names
      .iter()
      .position(|n| *n == s)
      .map(|k| k as u32 + base)
      .or_else(|| s.parse().ok())
      .filter(|n| (lo..=hi).contains(n))
      .ok_or_else(bad)
  };
  let mut bits = 0u64;
  for part in text.split(',') {
    let (range, step) = match part.split_once('/') {
      Some((r, s)) => (r, s.parse::<u32>().ok().filter(|&s| s > 0).ok_or_else(bad)?),
      None => (part, 1),
    };
    let (a, b) = match range {
      "*" => (lo, hi),
      r => match r.split_once('-') {
        Some((a, b)) => (num(a)?, num(b)?),
        None if step > 1 => (num(r)?, hi),
        None => (num(r)?, num(r)?),
      },
    };
    if a > b {
      return Err(bad());
    }
    for v in (a..=b).step_by(step as usize) {
      bits |= 1 << v;
    }
  }
  Ok(bits)
}

#[cfg(test)]
mod tests {
  use super::*;
  use chrono::{FixedOffset, Utc};

  fn utc(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc)
  }

  fn next(line: &str, after: &str) -> String {
    Cron::parse(line).unwrap().next(&utc(after)).unwrap().to_rfc3339()
  }

  #[test]
  fn a_cron_line_finds_its_next_minute() {
    assert_eq!(next("0 9 * * *", "2026-11-06T08:59:30Z"), "2026-11-06T09:00:00+00:00");
    assert_eq!(next("0 9 * * *", "2026-11-06T09:00:00Z"), "2026-11-07T09:00:00+00:00");
    assert_eq!(
      next("*/15 * * * *", "2026-11-06T10:07:00Z"),
      "2026-11-06T10:15:00+00:00"
    );
    // weekdays: Friday 6 Nov, so Monday next
    assert_eq!(
      next("30 8 * * 1-5", "2026-11-06T09:00:00Z"),
      "2026-11-09T08:30:00+00:00"
    );
    assert_eq!(next("0 0 * * sun", "2026-11-06T09:00:00Z"), "2026-11-08T00:00:00+00:00");
    assert_eq!(next("0 0 * * 7", "2026-11-06T09:00:00Z"), "2026-11-08T00:00:00+00:00");
    assert_eq!(
      next("0 0 29 feb *", "2026-11-06T09:00:00Z"),
      "2028-02-29T00:00:00+00:00"
    );
    assert_eq!(next("@monthly", "2026-11-06T09:00:00Z"), "2026-12-01T00:00:00+00:00");
    // a day of month or of week, both given: either
    assert_eq!(
      next("0 12 13 * fri", "2026-11-07T00:00:00Z"),
      "2026-11-13T12:00:00+00:00"
    );
    assert_eq!(
      next("0 12 1 * fri", "2026-11-07T00:00:00Z"),
      "2026-11-13T12:00:00+00:00"
    );
  }

  #[test]
  fn a_cron_line_reads_its_own_zone() {
    let hcm = FixedOffset::east_opt(7 * 3600).unwrap();
    let after = utc("2026-11-06T03:00:00Z").with_timezone(&hcm); // 10:00 there
    let at = Cron::parse("0 21 * * *").unwrap().next(&after).unwrap();
    assert_eq!(at.to_rfc3339(), "2026-11-06T21:00:00+07:00");
  }

  #[test]
  fn a_bad_cron_line_says_what_it_takes() {
    for bad in [
      "",
      "0 9 * *",
      "60 * * * *",
      "0 24 * * *",
      "* * 0 * *",
      "5-1 * * * *",
      "*/0 * * * *",
      "0 9 * * funday",
    ] {
      assert!(Cron::parse(bad).is_err(), "{bad:?}");
    }
    assert!(Cron::parse("0 9 * * 1-5").is_ok() && Cron::parse("0,30 8-18/2 * jan-mar *").is_ok());
  }

  #[test]
  fn a_while_is_numbers_and_units() {
    assert_eq!(duration("90m"), Ok(90 * 60_000));
    assert_eq!(duration("1h30m"), Ok(90 * 60_000));
    assert_eq!(duration("1h 30m"), Ok(90 * 60_000));
    assert_eq!(duration("2d"), Ok(2 * 86_400_000));
    assert_eq!(duration("1w"), Ok(7 * 86_400_000));
    for bad in ["", "90", "m", "0m", "5 minutes", "-5m"] {
      assert!(duration(bad).is_err(), "{bad:?}");
    }
  }

  #[test]
  fn a_moment_is_rfc3339_or_a_time_here() {
    assert_eq!(
      moment("2026-11-07T09:00:00+07:00"),
      Ok(utc("2026-11-07T02:00:00Z").timestamp_millis())
    );
    let here = Local.with_ymd_and_hms(2026, 11, 7, 9, 0, 0).unwrap().timestamp_millis();
    assert_eq!(moment("2026-11-07 09:00"), Ok(here));
    assert_eq!(moment("2026-11-07T09:00"), Ok(here));
    assert!(moment("tomorrow at nine").is_err());
  }

  #[test]
  fn a_timer_due_fires_once_and_one_that_repeats_goes_on() {
    let now = utc("2026-11-06T10:00:30Z").timestamp_millis();
    let at = |s: &str| utc(s).timestamp_millis();
    let timer = |id, next, repeat: Option<&str>| Timer {
      id,
      chat: "acorn".into(),
      next,
      what: format!("t{id}"),
      task: false,
      repeat: repeat.map(str::to_string),
    };
    let mut book = Book {
      last: 3,
      timers: vec![
        timer(1, at("2026-11-06T10:00:00Z"), None),
        timer(2, at("2026-11-06T07:00:00Z"), Some("* * * * *")),
        timer(3, at("2026-11-06T11:00:00Z"), None),
      ],
    };
    let due = book.due(now);
    assert_eq!(due.iter().map(|(t, _)| t.id).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(due[1].1, now - at("2026-11-06T07:00:00Z"), "late by its wait");
    assert_eq!(book.timers.iter().map(|t| t.id).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(
      book.timers[0].next,
      at("2026-11-06T10:01:00Z"),
      "missed runs fire once, then on"
    );
    assert_eq!(book.soonest(), Some(at("2026-11-06T10:01:00Z")));
    assert!(book.due(now).is_empty());
  }

  #[test]
  fn a_span_reads_roughly() {
    assert_eq!(span(40_000), "40s");
    assert_eq!(span(125 * 60_000), "2h 5m");
    assert_eq!(span(3 * 86_400_000 + 4 * 3_600_000 + 60_000), "3d 4h");
  }
}
