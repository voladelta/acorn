//! Facts
//! =====
//! What the user asked an assistant to keep for good, beside the chats:
//! facts.db in Acorn's dir, one for all assistants (each chat's own memory
//! is its tree, `memory`). A fact is a subject's predicate's value, for
//! everyone (every assistant sees it) or one assistant's (only it does, and
//! it shadows everyone's of the same subject and predicate). A new value
//! replaces the last of its slot; `many` keeps each. Every call's system
//! prompt lists those its assistant sees, the newest within BUDGET bytes.
//! A fact forgotten or replaced is erased for good (`memory::erase`).

use crate::config;
use anyhow::Result;
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

const BUDGET: usize = 16_000;
/// The owner of a fact for everyone; else an assistant's id.
const EVERYONE: &str = "";

const SCHEMA: &str = "pragma journal_mode = wal; pragma synchronous = normal; pragma secure_delete = on;
create table if not exists facts (id integer primary key autoincrement, owner text not null, subject text not null, predicate text not null,
    value text not null, many integer not null, chat text not null, msg integer not null, at integer not null);
create index if not exists facts_slot on facts (owner, subject, predicate);";

static DB: Mutex<Option<Connection>> = Mutex::new(None);

/// facts.db, opened once.
fn db() -> Result<MappedMutexGuard<'static, Connection>> {
  let mut db = DB.lock();
  if db.is_none() {
    let dir = &config::get().dir;
    std::fs::create_dir_all(dir)?;
    let conn = Connection::open(dir.join("facts.db"))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch(SCHEMA)?;
    *db = Some(conn);
  }
  Ok(MutexGuard::map(db, |db| db.as_mut().expect("opened")))
}

/// A fact to keep: whose (`mine`: the assistant's alone), what, and where
/// it was said (the chat's id, its message).
pub struct Fact {
  pub mine: bool,
  pub subject: String,
  pub predicate: String,
  pub value: String,
  pub many: bool,
  pub chat: String,
  pub msg: u64,
}

/// A slot's key: its words, trimmed and lowercased, so a correction finds
/// what it corrects.
fn key(text: &str) -> String {
  text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// A fact kept for assistant `me`; answers what was kept and replaced.
pub fn remember(me: &str, fact: Fact) -> Result<String> {
  let (subject, predicate, value) = (key(&fact.subject), key(&fact.predicate), fact.value.trim().to_string());
  if subject.is_empty() || predicate.is_empty() || value.is_empty() {
    return Ok("A fact needs a subject, a predicate and a value.".into());
  }
  let owner = if fact.mine { me } else { EVERYONE };
  let mut db = db()?;
  let tx = db.transaction()?;
  let slot = params![owner, subject, predicate];
  let mut replaced = Vec::new();
  if fact.many {
    let same: Option<i64> = tx
      .query_row(
        "select id from facts where owner = ? and subject = ? and predicate = ? and value = ?",
        params![owner, subject, predicate, value],
        |r| r.get(0),
      )
      .optional()?;
    if let Some(id) = same {
      return Ok(format!("Already kept, as #{id}."));
    }
  } else {
    let mut q = tx.prepare("select id, value from facts where owner = ? and subject = ? and predicate = ?")?;
    replaced = q
      .query_map(slot, |r| {
        Ok(format!("#{} {}", r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
      })?
      .collect::<rusqlite::Result<_>>()?;
    drop(q);
    tx.execute(
      "delete from facts where owner = ? and subject = ? and predicate = ?",
      slot,
    )?;
  }
  tx.execute(
    "insert into facts (owner, subject, predicate, value, many, chat, msg, at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    params![
      owner,
      subject,
      predicate,
      value,
      fact.many,
      fact.chat,
      fact.msg,
      crate::now_ms()
    ],
  )?;
  let id = tx.last_insert_rowid();
  tx.commit()?;
  if !replaced.is_empty() {
    crate::memory::erase(&db)?;
  }
  let whose = if fact.mine {
    "for you alone"
  } else {
    "for every assistant"
  };
  let was = if replaced.is_empty() {
    String::new()
  } else {
    format!("; it replaces {}", replaced.join(", "))
  };
  Ok(format!("Kept #{id} {subject} / {predicate}: {value}, {whose}{was}."))
}

/// Fact #id forgotten for good, if assistant `me` sees it.
pub fn forget(me: &str, id: u64) -> Result<String> {
  let db = db()?;
  if db.execute(
    "delete from facts where id = ? and owner in (?, ?)",
    params![id, EVERYONE, me],
  )? == 0
  {
    return Ok(format!("No fact #{id}."));
  }
  crate::memory::erase(&db)?;
  Ok(format!("Forgot #{id}; it is erased from disk."))
}

/// The facts assistant `me` sees that hold `text` forgotten for good (a
/// purge by text: each would go on into every system prompt). Answers
/// how many went.
pub fn purge(me: &str, text: &str) -> Result<usize> {
  let db = db()?;
  let gone = db.execute(
    "delete from facts where owner in (?1, ?2)
        and (instr(value, ?3) > 0 or instr(subject, ?4) > 0 or instr(predicate, ?4) > 0)",
    params![EVERYONE, me, text, key(text)],
  )?;
  if gone > 0 {
    crate::memory::erase(&db)?;
  }
  Ok(gone)
}

/// The facts assistant `me` sees, for its system prompt (none: "").
pub fn prompt(me: &str) -> String {
  let rows = db().and_then(|db| {
    let mut q =
      db.prepare("select id, owner, subject, predicate, value, many from facts where owner in (?, ?) order by id")?;
    let rows = q.query_map(params![EVERYONE, me], |r| {
      Ok((
        r.get::<_, i64>(0)?,
        r.get::<_, String>(1)? != EVERYONE,
        r.get::<_, String>(2)?,
        r.get::<_, String>(3)?,
        r.get::<_, String>(4)?,
        r.get::<_, bool>(5)?,
      ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
  });
  let rows = rows.unwrap_or_else(|err| {
    eprintln!("facts: {err}");
    Vec::new()
  });
  let mine: HashSet<(&str, &str)> = rows
    .iter()
    .filter(|r| r.1 && !r.5)
    .map(|r| (r.2.as_str(), r.3.as_str()))
    .collect();
  let lines: Vec<String> = rows
    .iter()
    .filter(|r| r.1 || !mine.contains(&(r.2.as_str(), r.3.as_str())))
    .map(|(id, own, subject, predicate, value, _)| {
      format!(
        "#{id} {subject} / {predicate}: {}{}\n",
        crate::memory::flat(value),
        if *own { " (yours)" } else { "" }
      )
    })
    .collect();
  if lines.is_empty() {
    return String::new();
  }
  let mut size = 0;
  let from = lines.iter().rposition(|l| {
    size += l.len();
    size > BUDGET
  });
  let from = from.map_or(0, |k| k + 1);
  let older = if from == 0 {
    String::new()
  } else {
    format!("({from} older facts are not shown.)\n")
  };
  format!(
    "\n\n# Facts\n\nWhat the user asked you to keep (remember, forget), oldest first, as \"#id subject / predicate: value\"; \"(yours)\" marks a fact for you alone, not every assistant. A fact holds until the user changes it.\n\n{older}{}",
    lines[from..].concat()
  )
}
