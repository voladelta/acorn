//! Purges
//! ======
//! A message erased, held GRACE so it can be taken back (`unpurge`), and
//! chat.db wiped of its old bytes.

use super::*;

/// How long a purged message is held, to take back, before a wipe may
/// erase it for good.
pub const GRACE: i64 = 10 * 60 * 1000;

/// When a message holding a secret purged as text (`kept_from`) was held:
/// never, so it is past its GRACE at once, and the next wipe erases it.
const SECRET_AT: i64 = 0;

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
  /// the chat, its pin and every node built from it: those in a view open into their
  /// halves, and it and those above build anew (a call building any node
  /// now is not stored: the caller stops them). A wipe of chat.db is then
  /// owed. Answers what it erased.
  pub fn purge(&mut self, agent: Option<&str>, id: u64) -> Result<String> {
    let log = agent.unwrap_or(CHAT);
    let Some((a, b)) = self.pages_of(agent, id) else {
      return Ok(no_message(agent, id));
    };
    let gone = self.built_from(agent, a, b);
    self.owe_wipe()?;
    let tx = self.db.transaction()?;
    let at = crate::now_ms();
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
            at
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
        self.unbuild((a..=b).map(Part::leaf), &gone);
        // unpinned for good: taken back, it would not count in what the
        // pins since were let hold (`pins::PINS`)
        if let Ok(at) = self.pins.binary_search(&a) {
          self.pins.remove(at);
          self.save()?;
        }
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
  /// did. A message holding a secret purged as text (`kept_from`) is not
  /// taken back.
  pub fn unpurge(&mut self, agent: Option<&str>, id: u64) -> Result<String> {
    let log = agent.unwrap_or(CHAT);
    let Some((a, b)) = self.pages_of(agent, id) else {
      return Ok(no_message(agent, id));
    };
    let gone = self.built_from(agent, a, b);
    let tx = self
      .db
      .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let oldest: Option<i64> = tx.query_row(
      "select min(at) from held where log = ? and i between ? and ?",
      params![log, a, b],
      |r| r.get(0),
    )?;
    if oldest == Some(SECRET_AT) {
      return Ok(format!(
        "Message {id} holds a secret purged as text: it stays purged, as that secret does everywhere."
      ));
    }
    if oldest.is_none_or(|at| at <= crate::now_ms() - GRACE) {
      return Ok(format!(
        "Message {id} isn't held for undo: it was never purged, or its ten minutes are over."
      ));
    }
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
        self.unbuild((a..=b).map(Part::leaf), &gone);
        format!(
          "Unpurged message {id}{}: it is back as it was, and the {} summaries over it are being written anew with it.",
          pages(a, b),
          gone.len()
        )
      }
    })
  }

  /// Every message held (`purge`) holding `text`, in a page's text or
  /// line or across a page's end, never to be taken back: each of its
  /// pages past its GRACE at once, so the next wipe erases them, and a
  /// wipe owed. Answers how many messages.
  pub fn kept_from(&mut self, text: &str) -> Result<usize> {
    let held: Vec<(String, u64)> = {
      let mut q = self.db.prepare(
        "select log, i from held h where instr(text, ?1) > 0 or instr(line, ?1) > 0
          or (exists (select 1 from msgs where log = h.log and i = h.i and more) and instr(
            substr(text, 1 - length(?1)) || coalesce((select substr(text, 1, length(?1) - 1) from held
              where log = h.log and i = h.i + 1), ''), ?1) > 0)",
      )?;
      let rows = q.query_map([text], |r| Ok((r.get(0)?, r.get(1)?)))?;
      rows.collect::<rusqlite::Result<_>>()?
    };
    let mut messages = HashSet::new();
    for (log, i) in held {
      let agent = (log != CHAT).then_some(log.as_str());
      let (a, b) = self.pages_of(agent, i).unwrap_or((i, i));
      messages.insert((log, a, b));
    }
    if messages.is_empty() {
      return Ok(0);
    }
    self.owe_wipe()?;
    let tx = self.db.transaction()?;
    for (log, a, b) in &messages {
      tx.execute(
        "update held set at = ? where log = ? and i between ? and ?",
        params![SECRET_AT, log, a, b],
      )?;
    }
    tx.commit()?;
    self.purged += 1;
    Ok(messages.len())
  }

  /// Every summary holding `text` let go, and those built from one: a
  /// compaction may have copied it from its context, off a message purged
  /// since. They build anew, as after a purge (not held: nothing to take
  /// back), and a wipe of chat.db is owed. Answers how many went.
  pub fn sweep(&mut self, text: &str) -> Result<usize> {
    let held: Vec<Part> = {
      let mut q = self.db.prepare("select slot from nodes where instr(text, ?) > 0")?;
      let slots = q.query_map([text], |r| r.get(0))?;
      slots.map(|slot| slot.map(Part::at)).collect::<rusqlite::Result<_>>()?
    };
    if held.is_empty() {
      return Ok(0);
    }
    let mut gone = HashSet::new();
    for &part in &held {
      self.above(part, &mut gone);
    }
    self.owe_wipe()?;
    let tx = self.db.transaction()?;
    for &slot in &gone {
      tx.execute("delete from nodes where slot = ?", [slot])?;
    }
    tx.commit()?;
    self.unbuild(held.into_iter(), &gone);
    self.purged += 1;
    Ok(gone.len())
  }

  /// A wipe of chat.db owed: its marker written first, so that a wipe
  /// cut short by a quit is done at the next launch.
  fn owe_wipe(&self) -> Result<()> {
    let wipe = self.dir.join(WIPE);
    fs::write(&wipe, "")?;
    disk::wrote(&wipe);
    Ok(())
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
  /// The first page holding `text` (or its line), or its start (the rest
  /// on the page after, of the same message): the chat's (None), else an
  /// agent's log's, by name; and its index.
  pub fn holding(&self, text: &str) -> Option<(Option<String>, u64)> {
    self
      .db
      .query_row(
        "select log, i from msgs m where instr(text, ?1) > 0 or instr(line, ?1) > 0 or (more and instr(
            substr(text, 1 - length(?1)) || (select substr(text, 1, length(?1) - 1) from msgs
              where log = m.log and i = m.i + 1), ?1) > 0)
          order by log <> ?2, log, i limit 1",
        params![text, CHAT],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)),
      )
      .optional()
      .ok()
      .flatten()
      .map(|(log, i)| ((log != CHAT).then_some(log), i))
  }

  pub(super) fn pages_of(&self, agent: Option<&str>, id: u64) -> Option<(u64, u64)> {
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
      self.above(Part::leaf(i), &mut gone);
    }
    gone
  }

  /// `part` and the nodes above it, those built, added to `gone` by slot.
  fn above(&self, mut part: Part, gone: &mut HashSet<u64>) {
    while part.span().1 < self.count {
      if self.node(part).is_some() {
        gone.insert(part.slot());
      }
      part = part.up();
    }
  }

  /// The nodes `gone` (by slot) let go, the views' open into the halves
  /// left, `parts` and those above them to build anew: each of `parts`
  /// ready, but one above another, which waits on it as those above do.
  fn unbuild(&mut self, parts: impl Iterator<Item = Part> + Clone, gone: &HashSet<u64>) {
    self.age += 1;
    self.nodes.retain(|slot, _| !gone.contains(slot));
    for part in parts.clone() {
      self.ready(part, true);
    }
    for part in parts {
      let mut part = part.up();
      while part.span().1 < self.count {
        self.ready(part, false);
        part = part.up();
      }
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

pub(super) fn no_message(agent: Option<&str>, id: u64) -> String {
  match agent {
    Some(name) => format!("No message {id} in {name}'s chat."),
    None => format!("No message {id}."),
  }
}

/// A message's pages, said if more than one.
pub(super) fn pages(a: u64, b: u64) -> String {
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
    let mut q = tx.prepare("select images from held where at <= ?")?;
    let lists = q.query_map([expired], |r| r.get::<_, String>(0))?;
    let mut names = Vec::new();
    for list in lists {
      names.extend(serde_json::from_str::<Vec<String>>(&list?)?);
    }
    names
  };
  tx.execute("delete from held where at <= ?", [expired])?;
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

#[cfg(test)]
mod tests {
  use super::*;

  /// Every page holding a text is found, the chat's first, then the
  /// agents'; purged, none holds it, and the rest stay as they were.
  #[test]
  fn what_holds_a_secret_is_found_till_none_does() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    let key = "sk-test-ABCD1234";
    mem.push(vec![Msg::new(Kind::User, "hello")]).unwrap();
    mem.push(vec![Msg::new(Kind::User, format!("my key: {key}"))]).unwrap();
    mem
      .agent_push("Scout", Kind::Echo, &format!("found {key} in config"))
      .unwrap();
    mem
      .push(vec![Msg::new(Kind::Echo, format!("1|user: my key: {key}"))])
      .unwrap();
    assert_eq!(mem.holding(key), Some((None, 1)), "the chat's first");
    let mut purged = Vec::new();
    while let Some(at) = mem.holding(key) {
      assert!(!purged.contains(&at), "{at:?} purged twice");
      mem.purge(at.0.as_deref(), at.1).unwrap();
      purged.push(at);
    }
    assert_eq!(purged, [(None, 1), (None, 2), (Some("Scout".into()), 0)]);
    assert_eq!(mem.holding("hello"), Some((None, 0)), "the rest kept");
  }

  /// A text cut by a page's end is found, and goes with its message.
  #[test]
  fn a_secret_across_a_page_cut_is_found() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    let key = "sk-test-ABCD1234-EFGH5678";
    mem.push(vec![Msg::new(Kind::User, "hello")]).unwrap();
    // one long line, so the page ends 10 characters into the key
    let text = format!("{}{key}{}", "é".repeat(crate::clip::CAP - 10), "y".repeat(50));
    mem.push(vec![Msg::new(Kind::User, text)]).unwrap();
    assert_eq!(mem.count, 3, "two pages");
    assert!(!mem.message(1).unwrap().text.contains(key) && !mem.message(2).unwrap().text.contains(key));
    assert_eq!(mem.holding(key), Some((None, 1)));
    // two messages that only together spell it hold none
    mem.push(vec![Msg::new(Kind::User, &key[..10])]).unwrap();
    mem.push(vec![Msg::new(Kind::User, &key[10..])]).unwrap();
    mem.purge(None, 1).unwrap();
    assert_eq!(mem.holding(key), None);
    assert_eq!(mem.message(2).unwrap().text, PURGED, "each page of it");
  }

  /// A message held holding a secret purged as text is never taken back,
  /// after a restart too, however it holds it: across a page's end, in its
  /// line, or purged by id before the secret was; the next wipe erases it,
  /// though in its GRACE. Others held are as they were.
  #[test]
  fn a_message_holding_a_secret_is_not_held_for_undo() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    let key = "sk-test-ABCD1234-EFGH5678";
    mem.push(vec![Msg::new(Kind::User, "hello")]).unwrap();
    // 1, 2: the page ends 10 characters into the key
    let text = format!("{}{key}{}", "é".repeat(crate::clip::CAP - 10), "y".repeat(50));
    mem.push(vec![Msg::new(Kind::User, text)]).unwrap();
    // 3: in its line only
    let line = Msg {
      line: Some(format!("the key {key}")),
      ..Msg::new(Kind::Acorn, "a reply")
    };
    mem.push(vec![line]).unwrap();
    // 4: an aside; 5, 6: the key's halves, which only together spell it
    mem.push(vec![Msg::new(Kind::User, "an aside")]).unwrap();
    mem.push(vec![Msg::new(Kind::User, &key[..10])]).unwrap();
    mem.push(vec![Msg::new(Kind::User, &key[10..])]).unwrap();
    assert_eq!(mem.count, 7);
    // each purged by id first, as the user may before purging the key
    for id in [1, 3, 4, 5, 6] {
      mem.purge(None, id).unwrap();
    }
    assert_eq!(mem.holding(key), None, "no message shows it");
    let purged = mem.purged;
    assert_eq!(mem.kept_from(key).unwrap(), 2);
    assert_eq!(mem.purged, purged + 1, "a wipe is owed");
    assert_eq!(mem.kept_from("not held anywhere").unwrap(), 0);
    let at = |mem: &Memory, i: u64| -> i64 {
      mem
        .db
        .query_row("select at from held where log = ? and i = ?", params![CHAT, i], |r| {
          r.get(0)
        })
        .unwrap()
    };
    assert_eq!(
      (at(&mem, 1), at(&mem, 2), at(&mem, 3)),
      (SECRET_AT, SECRET_AT, SECRET_AT)
    );
    assert!(at(&mem, 5) > SECRET_AT && at(&mem, 6) > SECRET_AT);
    for _ in 0..2 {
      for id in [1, 2, 3] {
        let said = mem.unpurge(None, id).unwrap();
        assert!(said.contains("stays purged"), "{id}: {said}");
        assert_eq!(mem.message(id).unwrap().text, PURGED);
        assert_eq!(mem.message(id).unwrap().line, None);
      }
      drop(mem);
      mem = Memory::load(dir.path()).unwrap();
    }
    wipe(dir.path(), crate::now_ms() - GRACE).unwrap();
    let held: Vec<u64> = {
      let mut q = mem.db.prepare("select i from held order by i").unwrap();
      q.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(held, [4, 5, 6], "wiped, though in its grace");
    for id in [4, 5] {
      assert!(mem.unpurge(None, id).unwrap().starts_with("Unpurged"));
    }
    assert_eq!(mem.message(4).unwrap().text, "an aside");
    assert_eq!(mem.message(5).unwrap().text, &key[..10]);
  }

  /// An agent's message held holding the secret is not taken back either.
  #[test]
  fn an_agents_message_holding_a_secret_is_not_held_for_undo() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    let key = "sk-test-ABCD1234";
    mem.agent_push("Scout", Kind::Echo, "nothing here").unwrap();
    mem.agent_push("Scout", Kind::Echo, &format!("found {key}")).unwrap();
    mem.purge(Some("Scout"), 0).unwrap();
    mem.purge(Some("Scout"), 1).unwrap();
    assert_eq!(mem.kept_from(key).unwrap(), 1);
    assert!(mem.unpurge(Some("Scout"), 1).unwrap().contains("stays purged"));
    assert!(mem.unpurge(Some("Scout"), 0).unwrap().starts_with("Unpurged"));
  }

  /// Every node ready built, a call's line cut from its source, till
  /// none is.
  fn build(mem: &mut Memory) {
    loop {
      let (mut built, jobs) = mem.starts();
      if built.is_empty() && jobs.is_empty() {
        break;
      }
      for (part, text) in jobs {
        mem.busy.remove(&part.slot());
        built.push((part, cut(&flat(&text)).to_string()));
      }
      mem.store(&built).unwrap();
      mem.fit(false);
    }
  }

  /// How many nodes logged hold a text.
  fn nodes_holding(mem: &Memory, text: &str) -> u64 {
    let q = "select count(*) from nodes where instr(text, ?) > 0";
    mem.db.query_row(q, [text], |r| r.get(0)).unwrap()
  }

  /// A key and the node of it are one another's, at every level.
  #[test]
  fn a_slot_names_its_node() {
    for l in 0..12 {
      for i in 0..200 {
        let part = Part::new(l, i);
        assert_eq!(Part::at(part.slot()), part);
      }
    }
    for slot in 0..4096 {
      assert_eq!(Part::at(slot).slot(), slot);
    }
  }

  /// A summary that copied a secret from its context outlives the purge
  /// of the message that held it: a sweep lets it go, and those built
  /// from it, out of the views and chat.db; they build anew without it,
  /// the rest as they were, and so it is found after a restart.
  #[test]
  fn a_sweep_lets_go_of_the_summaries_holding_a_secret() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    let key = "sk-test-ABCD1234";
    // 85 messages: a tree with roots of four sizes, each past a line
    for k in 0..85 {
      let text = if k == 70 {
        format!("my key: {key}")
      } else {
        format!("message {k}: {}", "words ".repeat(400))
      };
      mem.push(vec![Msg::new(Kind::User, text)]).unwrap();
    }
    build(&mut mem);
    assert!(mem.seen.parts.iter().any(|p| p.l > 0), "a view merged");
    // message 5's summary copied the key, and so did the merges above it
    let (leaf, pair, four) = (Part::leaf(5), Part::new(1, 2), Part::new(2, 1));
    for part in [leaf, pair, four] {
      let line = format!("{}+{}|the user's key is {key}", part.span().0, part.n());
      mem.store(&[(part, line)]).unwrap();
    }
    mem.fit(false);
    while let Some(at) = mem.holding(key) {
      mem.purge(at.0.as_deref(), at.1).unwrap();
    }
    assert_eq!(mem.message(70).unwrap().text, PURGED);
    assert_eq!(nodes_holding(&mem, key), 3, "a purge leaves them");
    let kept = mem.node(Part::new(2, 3)).unwrap();
    let purged = mem.purged;

    // the three, and the four above them
    let above = [Part::new(3, 0), Part::new(4, 0), Part::new(5, 0), Part::new(6, 0)];
    assert_eq!(mem.sweep(key).unwrap(), 7);
    assert_eq!(mem.purged, purged + 1, "a wipe is owed");
    assert!(dir.path().join(WIPE).exists());
    assert_eq!(nodes_holding(&mem, key), 0);
    for part in [leaf, pair, four].into_iter().chain(above) {
      assert_eq!(mem.node(part), None, "{part:?} is let go");
    }
    assert_eq!(mem.node(Part::new(2, 3)), Some(kept.clone()), "the rest stays");
    let shown = |mem: &Memory| -> String { mem.view_blocks(mem.count).into_iter().map(|b| b.text).collect() };
    assert!(!shown(&mem).contains(key));
    for view in [&mem.view, &mem.seen] {
      let size: usize = view.parts.iter().map(|&p| mem.size(p)).sum();
      assert_eq!(view.size, size);
      assert!(view.parts.iter().all(|&p| p.l == 0 || mem.built(p)));
    }
    assert_eq!(mem.sweep(key).unwrap(), 0, "none holds it now");

    build(&mut mem);
    assert_eq!(
      mem.node(leaf),
      Some(cut(&flat(&mem.message(5).unwrap().show())).to_string())
    );
    for part in [pair, four].into_iter().chain(above) {
      assert!(mem.node(part).is_some(), "{part:?} is built anew");
    }
    assert_eq!(nodes_holding(&mem, key), 0);
    let text = shown(&mem);
    drop(mem);
    let mut mem = Memory::load(dir.path()).unwrap();
    assert_eq!(shown(&mem), text, "as it was, after a restart");
    build(&mut mem);
    assert_eq!(shown(&mem), text, "nothing was left to build");
  }
}
