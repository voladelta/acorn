//! Purges
//! ======
//! A message erased, held GRACE so it can be taken back (`unpurge`), and
//! chat.db wiped of its old bytes.

use super::*;

/// How long a purged message is held, to take back, before a wipe may
/// erase it for good.
pub const GRACE: i64 = 10 * 60 * 1000;

/// Its table in chat.db: what each page purged held (its images as JSON),
/// and when (ms).
pub(super) fn tables(db: &Connection) -> rusqlite::Result<()> {
  db.execute_batch(
    "create table if not exists held (log text not null, i integer not null, kind text not null, text text not null,
        images text not null, line text, at integer not null, primary key (log, i)) without rowid;",
  )
}

impl Memory {
  /// Purges a message: the chat's message id (each of its pages), or
  /// message id of an agent's log (`agent`). Its text becomes PURGED, its
  /// line and images go (held GRACE in `held`, for `unpurge`), and, in
  /// the chat, every node built from it: those in a view open into their
  /// halves, and it and those above build anew (a call building any node
  /// now is not stored: the caller stops them). A wipe of chat.db is then
  /// owed. Answers what it erased.
  pub fn purge(&mut self, agent: Option<&str>, id: u64) -> Result<String> {
    let log = agent.unwrap_or(CHAT);
    let Some((a, b)) = self.pages_of(agent, id) else {
      return Ok(no_message(agent, id));
    };
    let gone = self.built_from(agent, a, b);
    let wipe = self.dir.join(WIPE);
    fs::write(&wipe, "")?;
    disk::wrote(&wipe);
    let tx = self.db.transaction()?;
    for i in a..=b {
      let Some(mut root) = tx
        .query_row("select * from msgs where log = ? and i = ?", params![log, i], from_row)
        .optional()?
      else {
        continue;
      };
      if root.text != PURGED || !root.images.is_empty() {
        tx.execute(
          "insert or replace into held values (?, ?, ?, ?, ?, ?, ?)",
          params![
            log,
            i,
            root.kind.as_str(),
            root.text,
            serde_json::to_string(&root.images)?,
            root.line,
            crate::now_ms()
          ],
        )?;
      }
      root.text = PURGED.into();
      root.images.clear();
      root.line = None;
      root.size = root.show().len();
      tx.execute(
        "update msgs set text = ?, images = '[]', line = null, size = ? where log = ? and i = ?",
        params![root.text, root.size, log, i],
      )?;
    }
    for &slot in &gone {
      tx.execute("delete from nodes where slot = ?", [slot])?;
    }
    tx.commit()?;
    let mut said = match agent {
      Some(name) => format!("Purged message {id} of {name}'s chat."),
      None => {
        self.unbuild(a, b, &gone);
        format!(
          "Purged message {id}{} and the {} summaries made from it; they are being written anew without it.",
          pages(a, b),
          gone.len()
        )
      }
    };
    self.purged += 1;
    said += " unpurge takes it back for ten minutes; then";
    said += if self.wipe == super::Wipe::Stuck {
      " its old bytes are wiped from Acorn's files when Acorn starts again."
    } else {
      " its old bytes are wiped from Acorn's files once the chat is idle."
    };
    Ok(said)
  }

  /// A purge taken back, while its message is held (`purge`): its text,
  /// images and line back, and, in the chat, the nodes built without it
  /// let go, so it and those above build anew with it. Answers what it
  /// did.
  pub fn unpurge(&mut self, agent: Option<&str>, id: u64) -> Result<String> {
    let log = agent.unwrap_or(CHAT);
    let Some((a, b)) = self.pages_of(agent, id) else {
      return Ok(no_message(agent, id));
    };
    let held: u64 = self.db.query_row(
      "select count(*) from held where log = ? and i between ? and ?",
      params![log, a, b],
      |r| r.get(0),
    )?;
    if held == 0 {
      return Ok(format!(
        "Message {id} isn't held: it was never purged, or its ten minutes are over and it is erased for good."
      ));
    }
    let gone = self.built_from(agent, a, b);
    let tx = self.db.transaction()?;
    for i in a..=b {
      let Some(mut root) = tx
        .query_row("select * from msgs where log = ? and i = ?", params![log, i], from_row)
        .optional()?
      else {
        continue;
      };
      let back = tx
        .query_row(
          "select text, images, line from held where log = ? and i = ?",
          params![log, i],
          |r| {
            Ok((
              r.get::<_, String>(0)?,
              r.get::<_, String>(1)?,
              r.get::<_, Option<String>>(2)?,
            ))
          },
        )
        .optional()?;
      let Some((text, images, line)) = back else { continue };
      root.text = text;
      root.images = serde_json::from_str(&images)?;
      root.line = line;
      root.size = root.show().len();
      tx.execute(
        "update msgs set text = ?, images = ?, line = ?, size = ? where log = ? and i = ?",
        params![root.text, images, root.line, root.size, log, i],
      )?;
      tx.execute("delete from held where log = ? and i = ?", params![log, i])?;
    }
    for &slot in &gone {
      tx.execute("delete from nodes where slot = ?", [slot])?;
    }
    tx.commit()?;
    Ok(match agent {
      Some(name) => format!("Unpurged message {id} of {name}'s chat: it is back as it was."),
      None => {
        self.unbuild(a, b, &gone);
        format!(
          "Unpurged message {id}{}: it is back as it was, and the {} summaries over it are being written anew with it.",
          pages(a, b),
          gone.len()
        )
      }
    })
  }

  /// When the last message held stops being held (ms); None: none is.
  pub fn held_till(&self) -> Option<i64> {
    let at: Option<i64> = self
      .db
      .query_row("select max(at) from held", [], |r| r.get(0))
      .unwrap_or_else(|err| {
        eprintln!("memory: held: {err}");
        None
      });
    at.map(|at| at + GRACE)
  }

  /// A wipe done of the purges up to `upto`; none owed after, its marker
  /// goes.
  pub fn wiped(&mut self, upto: u64) {
    self.wiped = upto;
    if self.wiped == self.purged {
      let _ = fs::remove_file(self.dir.join(WIPE));
    }
  }

  /// The pages [a, b] of message id: the chat's (all of its), or an
  /// agent's (one); None: no such message.
  fn pages_of(&self, agent: Option<&str>, id: u64) -> Option<(u64, u64)> {
    let (mut a, mut b) = (id, id);
    match agent {
      Some(name) => {
        self.read(name, id)?;
      }
      None if id >= self.count => return None,
      None => {
        while a > 0 && self.message(a - 1).is_some_and(|m| m.more) {
          a -= 1;
        }
        while b + 1 < self.count && self.message(b).is_some_and(|m| m.more) {
          b += 1;
        }
      }
    }
    Some((a, b))
  }

  /// The nodes built from the chat's leaves a..=b, by slot (an agent's
  /// log has none).
  fn built_from(&self, agent: Option<&str>, a: u64, b: u64) -> HashSet<u64> {
    let mut gone = HashSet::new();
    if agent.is_some() {
      return gone;
    }
    for i in a..=b {
      let mut part = Part::leaf(i);
      while part.span().1 < self.count {
        if self.node(part).is_some() {
          gone.insert(part.slot());
        }
        part = part.up();
      }
    }
    gone
  }

  /// The nodes `gone` (by slot) let go, the views' open into the halves
  /// left, the leaves a..=b and those above them to build anew.
  fn unbuild(&mut self, a: u64, b: u64, gone: &HashSet<u64>) {
    self.age += 1;
    self.nodes.retain(|slot, _| !gone.contains(slot));
    for i in a..=b {
      let mut part = Part::leaf(i).up();
      while part.span().1 < self.count {
        self.ready(part, false);
        part = part.up();
      }
      self.ready(Part::leaf(i), true);
    }
    for k in 0..2 {
      let view = if k == 0 { &self.view } else { &self.seen };
      let parts: Vec<Part> = view
        .parts
        .clone()
        .into_iter()
        .flat_map(|p| self.open_up(p, gone))
        .collect();
      let size = parts.iter().map(|&p| self.size(p)).sum();
      let view = if k == 0 { &mut self.view } else { &mut self.seen };
      (view.parts, view.size) = (parts, size);
    }
    self.fit(false);
    if let Err(err) = self.save() {
      eprintln!("memory: state: {err}");
    }
  }

  /// A view's line, or, if gone, the lines under it left, kept.
  fn open_up(&mut self, part: Part, gone: &HashSet<u64>) -> Vec<Part> {
    match part.halves() {
      Some(halves) if gone.contains(&part.slot()) => halves.into_iter().flat_map(|h| self.open_up(h, gone)).collect(),
      _ => {
        if let Some(text) = self.node(part) {
          self.nodes.insert(part.slot(), text);
        }
        vec![part]
      }
    }
  }
}

fn no_message(agent: Option<&str>, id: u64) -> String {
  match agent {
    Some(name) => format!("No message {id} in {name}'s chat."),
    None => format!("No message {id}."),
  }
}

/// A message's pages, said if more than one.
fn pages(a: u64, b: u64) -> String {
  if a == b {
    String::new()
  } else {
    format!(" (its pages {a} to {b})")
  }
}

/// Deleted rows erased from a db's file: rewritten whole (secure_delete
/// zeroes only cells and pages freed, not old bytes left in a page still
/// used), its WAL truncated. It takes as long as the db is big (2 s for
/// 450 MB) and holds up every write meanwhile.
pub fn erase(db: &Connection) -> Result<()> {
  db.execute_batch("vacuum")?;
  let busy: i64 = db.query_row("pragma wal_checkpoint(truncate)", [], |r| r.get(0))?;
  anyhow::ensure!(busy == 0, "its WAL is in use");
  Ok(())
}

/// The chat in `dir` wiped, on a connection of its own: blocking. The
/// messages held since before `expired` (ms) go first, and their images
/// no message or held one still names; then its search index is merged
/// whole, as FTS5 keeps a deleted text's words in its older segments till
/// then; then the file is erased (`erase`).
pub fn wipe(dir: &Path, expired: i64) -> Result<()> {
  let mut db = Connection::open(dir.join("chat.db"))?;
  db.busy_timeout(std::time::Duration::from_secs(60))?;
  tables(&db)?;
  let tx = db.transaction()?;
  let images: Vec<String> = {
    let mut q = tx.prepare("select images from held where at < ?")?;
    let lists = q.query_map([expired], |r| r.get::<_, String>(0))?;
    let mut names = Vec::new();
    for list in lists {
      names.extend(serde_json::from_str::<Vec<String>>(&list?)?);
    }
    names
  };
  tx.execute("delete from held where at < ?", [expired])?;
  tx.commit()?;
  for name in images {
    let used: bool = db.query_row(
      "select exists (select 1 from msgs where images like '%' || ?1 || '%')
        or exists (select 1 from held where images like '%' || ?1 || '%')",
      [&name],
      |r| r.get(0),
    )?;
    if !used {
      let images = dir.join(IMAGES);
      for file in [images.join(&name), images.join(format!("{name}.jpg"))] {
        let _ = fs::remove_file(file);
      }
    }
  }
  db.execute("insert into search (search) values ('optimize')", [])?;
  erase(&db)
}
