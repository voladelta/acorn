//! A chat's memory in chat.db: logged, built and opened again as it was;
//! searched; a message purged with every node built from it, taken back,
//! and past its grace its secret gone from disk and the search; a view's
//! last lines, the latest messages there before they are summarized.

use acorn_core::Kind;
use acorn_core::effort::Effort;
use acorn_core::memory::{self, Memory, Msg, PURGED, Part};

const SECRET: &str = "hunter2-sk-live-0123456789";

/// Each node not built whose source is, built as its source clipped
/// (no model), till none is left.
fn build(mem: &mut Memory) {
  loop {
    let (free, jobs) = mem.starts();
    let jobs: Vec<_> = jobs
      .into_iter()
      .map(|(p, text)| (p, memory::cut(&memory::flat(&text)).to_string()))
      .collect();
    if free.is_empty() && jobs.is_empty() {
      break;
    }
    for (part, _) in &jobs {
      mem.busy.remove(&part.slot());
    }
    mem.store(&free).unwrap();
    mem.store(&jobs).unwrap();
  }
  mem.fit(false);
}

/// A chat's wipe owed, done as the chat does it once idle and no purge
/// is held in its grace.
fn wipe(mem: &mut Memory) {
  memory::wipe(&mem.dir, i64::MAX).unwrap();
  mem.wiped(mem.purged);
}

/// Whether any of the chat's files holds `text`.
fn on_disk(dir: &std::path::Path, text: &str) -> bool {
  std::fs::read_dir(dir)
    .unwrap()
    .flatten()
    .filter(|e| e.path().is_file())
    .any(|e| {
      let data = std::fs::read(e.path()).unwrap();
      data.windows(text.len()).any(|w| w == text.as_bytes())
    })
}

/// The config's dir, a test's own, set before any read: a memory reads the
/// view's size from it, never from the ~/.acorn of who runs the tests.
fn configured() {
  static SET: std::sync::Once = std::sync::Once::new();
  SET.call_once(|| {
    let dir = std::env::temp_dir().join(format!("acorn-memory-test-{}", std::process::id()));
    // SAFETY: every test starts here, so none reads the environment yet.
    unsafe { std::env::set_var("ACORN_DIR", &dir) };
  });
}

/// Seed a chat with 12 messages (including `SECRET` at id 5) plus a long
/// multi-page echo, then build its summary tree.
fn seed(dir: &std::path::Path) -> Memory {
  let mut mem = Memory::load(dir).unwrap();
  for k in 0..12 {
    let text = if k == 5 {
      format!("my key is {SECRET}")
    } else {
      format!("message {k} {}", "x".repeat(k * 90))
    };
    mem
      .push(vec![Msg::new(if k % 2 == 0 { Kind::User } else { Kind::Acorn }, text)])
      .unwrap();
  }
  mem.push(vec![Msg::new(Kind::Echo, "y".repeat(70_000))]).unwrap();
  build(&mut mem);
  mem
}

/// The rendered summary ids must tile exactly the requested history,
/// even when the cutoff is inside an evicted subtree.
fn assert_prefix(blocks: Vec<acorn_core::Block>, end: u64) {
  let mut next = 0;
  for s in blocks.iter().flat_map(|b| b.text.lines()).filter(|s| s.contains('|')) {
    let (id, n) = s.split_once('|').unwrap().0.split_once('+').unwrap();
    assert_eq!(id.parse::<u64>().unwrap(), next, "{s}");
    next += n.parse::<u64>().unwrap();
  }
  assert_eq!(next, end);
}

#[test]
fn compacted_views_cover_every_prefix_even_after_a_rewind() {
  configured();
  let home = tempfile::tempdir().unwrap();
  let mut mem = Memory::load(home.path()).unwrap();
  mem.view.high = 1100;
  mem.seen.high = 1100;
  mem
    .push(
      (0..16)
        .map(|i| Msg::new(Kind::User, format!("{i} {}", "x".repeat(490))))
        .collect(),
    )
    .unwrap();
  build(&mut mem);
  assert!(!mem.built(Part::leaf(0)));
  for end in 0..=mem.count {
    assert_prefix(mem.view_blocks(end), end);
  }
  mem.purge(None, 1).unwrap();
  build(&mut mem);
  assert_prefix(mem.blocks(mem.count, &mem.seen.parts), mem.count);
  drop(mem);
  let mem = Memory::load(home.path()).unwrap();
  for end in 0..=mem.count {
    assert_prefix(mem.view_blocks(end), end);
  }
}

#[test]
fn persistence_fts_search_and_zoom() {
  configured();
  let home = tempfile::tempdir().unwrap();
  let dir = home.path().join("chats/main");

  // logged, built, opened again as it was
  let mem = seed(&dir);
  let count = mem.count;
  assert!(count > 13, "a long text goes in pages");
  let view = mem.view.parts.clone();
  let top = mem.node(Part::new(3, 0)).expect("built");
  drop(mem);
  let mut mem = Memory::load(&dir).unwrap();
  assert_eq!((mem.count, &mem.view.parts), (count, &view));
  assert_eq!(mem.message(5).unwrap().text, format!("my key is {SECRET}"));
  assert_eq!(mem.node(Part::new(3, 0)).as_deref(), Some(top.as_str()));

  // searched: the messages' words, newest first; a query FTS5 can't read, why
  assert_eq!(
    mem.search(memory::CHAT, "hunter2", 10),
    format!("5|unii: my key is {SECRET}")
  );
  let ids: Vec<String> = mem
    .search(memory::CHAT, "message", 3)
    .lines()
    .map(|l| l.split('|').next().unwrap().to_string())
    .collect();
  assert_eq!(ids, ["11", "10", "9"]);
  assert!(
    mem
      .search(memory::CHAT, "nowhere", 10)
      .starts_with("No message matches")
  );
  assert!(
    mem
      .search(memory::CHAT, "\"unclosed", 10)
      .starts_with("Can't read the query")
  );
  assert!(mem.zoom(4, 4, 0).starts_with("4+2|") && mem.zoom(3, 2, 0).starts_with("No line"));
  let looks: (i64, i64, i64, i64) = mem
    .db
    .query_row(
      "select sum(tool = 'search'), sum(tool = 'search' and hits > 0), sum(tool = 'zoom'), sum(tool = 'zoom' and hits > 0) from looks",
      [],
      |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap();
  assert_eq!(looks, (4, 2, 2, 1), "each search and zoom logged with what it found");
}

#[test]
fn purge_unpurge_and_wal_erasure() {
  configured();
  let home = tempfile::tempdir().unwrap();
  let dir = home.path().join("chats/main");
  let mut mem = seed(&dir);

  // purged: its text, and every node built from it
  let age = mem.age;
  let said = mem.purge(None, 5).unwrap();
  assert!(said.starts_with("Purged message 5"), "{said}");
  assert_eq!(mem.message(5).unwrap().text, PURGED);
  assert_eq!(mem.age, age + 1);
  for part in [Part::new(0, 5), Part::new(1, 2), Part::new(2, 1), Part::new(3, 0)] {
    assert!(mem.node(part).is_none(), "{part:?} was built from it");
  }
  assert!(
    mem.node(Part::new(0, 4)).is_some() && mem.node(Part::new(1, 3)).is_some(),
    "the rest stays"
  );
  assert!(mem.view.parts.iter().all(|p| mem.text(*p).find(SECRET).is_none()));
  assert!(
    mem
      .search(memory::CHAT, "hunter2", 10)
      .starts_with("No message matches"),
    "a purged message is found no more"
  );

  // taken back: its text, and the nodes over it built anew with it
  let said = mem.unpurge(None, 5).unwrap();
  assert!(said.starts_with("Unpurged message 5"), "{said}");
  assert_eq!(mem.message(5).unwrap().text, format!("my key is {SECRET}"));
  assert!(mem.node(Part::new(0, 5)).is_none() && mem.node(Part::new(3, 0)).is_none());
  build(&mut mem);
  assert!(mem.node(Part::new(0, 5)).is_some_and(|t| t.contains(SECRET)));
  assert!(mem.node(Part::new(3, 0)).is_some());
  assert!(mem.search(memory::CHAT, "hunter2", 10).starts_with("5|"));
  assert!(mem.unpurge(None, 5).unwrap().contains("isn't held"));
  mem.purge(None, 5).unwrap();

  build(&mut mem);
  assert!(
    mem.node(Part::new(3, 0)).is_some_and(|t| !t.contains(SECRET)),
    "built anew without it"
  );
  let owed = dir.join("chat.db.wipe");
  assert!(owed.exists(), "a wipe is owed");
  drop(mem);
  let mut mem = Memory::load(&dir).unwrap();
  assert_eq!(mem.message(5).unwrap().text, PURGED);
  assert!(mem.purged > mem.wiped, "a quit doesn't lose the wipe owed");
  assert!(mem.held_till().is_some_and(|t| t > acorn_core::now_ms()));
  memory::wipe(&dir, acorn_core::now_ms() - memory::GRACE).unwrap();
  assert!(on_disk(&dir, SECRET), "held in its grace, a wipe leaves it");
  let cutoff = acorn_core::now_ms() - memory::GRACE;
  mem.db.execute("update held set at = ?", [cutoff]).unwrap();
  assert!(mem.unpurge(None, 5).unwrap().contains("ten minutes are over"));
  assert_eq!(
    mem.message(5).unwrap().text,
    PURGED,
    "expiry is independent of the idle wipe"
  );
  wipe(&mut mem);
  assert!(mem.held_till().is_none(), "past its grace, a wipe deletes it");
  assert!(
    !owed.exists() && !on_disk(&dir, SECRET),
    "erased from chat.db and its WAL"
  );
  assert!(mem.purge(None, 999).unwrap().starts_with("No message"));
}

#[test]
fn oversized_summaries_are_bounded_in_storage_and_on_legacy_reads() {
  configured();
  let home = tempfile::tempdir().unwrap();
  let mut mem = Memory::load(home.path()).unwrap();
  mem.push(vec![Msg::new(Kind::User, "source")]).unwrap();
  let fits = "a".repeat(memory::NODE - 1);
  let oversized = format!("{fits}é");
  let leaf = Part::leaf(0);
  mem.store(&[(leaf, oversized.clone())]).unwrap();
  let stored: String = mem.db.query_row("select text from nodes", [], |r| r.get(0)).unwrap();
  assert_eq!(stored, fits);
  assert_eq!(mem.view.size, fits.len());
  mem.db.execute("update nodes set text = ?", [oversized]).unwrap();
  drop(mem);
  let mem = Memory::load(home.path()).unwrap();
  assert_eq!(mem.text(leaf), fits);
  assert_eq!(mem.view.size, fits.len());
}

#[test]
fn long_message_purge_and_concurrent_wipe() {
  configured();
  let home = tempfile::tempdir().unwrap();
  let dir = home.path().join("chats/main");
  let mut mem = Memory::load(&dir).unwrap();
  mem.push(vec![Msg::new(Kind::User, "first")]).unwrap();
  let long: String = (0..2500).map(|k| format!("line {k} {SECRET}\n")).collect();
  mem.push(vec![Msg::new(Kind::User, long)]).unwrap();
  let id = mem.count - 1;
  build(&mut mem);
  assert!(on_disk(&dir, SECRET));
  let said = mem.purge(None, id).unwrap();
  assert!(said.contains("its pages"), "{said}");
  let owed = dir.join("chat.db.wipe");
  let upto = mem.purged;
  mem.purge(None, 0).unwrap();
  memory::wipe(&dir, i64::MAX).unwrap();
  mem.wiped(upto);
  assert!(owed.exists(), "a purge during a wipe owes another");
  wipe(&mut mem);
  assert!(
    !owed.exists() && !on_disk(&dir, SECRET),
    "no stale copy left in chat.db"
  );
}

#[test]
fn interrupted_message_and_agent_log() {
  configured();
  let home = tempfile::tempdir().unwrap();
  let dir = home.path().join("chats/main");
  let mut mem = Memory::load(&dir).unwrap();
  mem.push(vec![Msg::new(Kind::User, "hello")]).unwrap();

  // a quit in the middle of a message: its pages so far dropped
  let count = mem.count;
  mem
    .push(vec![Msg {
      more: true,
      ..Msg::new(Kind::Acorn, "half")
    }])
    .unwrap();
  drop(mem);
  let mut mem = Memory::load(&dir).unwrap();
  assert_eq!(mem.count, count);
  mem.push(vec![Msg::new(Kind::User, "next")]).unwrap();
  assert_eq!(mem.message(count).unwrap().text, "next");

  // an agent's log
  use acorn_core::model::Model;
  let name = mem.agent_new(Some(Effort::Low), Model::Hydrogen).unwrap();
  drop(mem);
  let mut mem = Memory::load(&dir).unwrap();
  assert_eq!(mem.agent_settings(&name), (Some(Effort::Low), Model::Hydrogen));
  mem.agent_push(&name, Kind::Note, "subagent").unwrap();
  mem
    .agent_push(&name, Kind::Echo, &format!("cat .env: {SECRET}"))
    .unwrap();
  assert_eq!(mem.agent_names(), std::slice::from_ref(&name));
  assert_eq!(mem.agent(&name).unwrap().len(), 2);
  let owed = dir.join("chat.db.wipe");
  mem.purge(Some(&name), 1).unwrap();
  assert!(mem.agent_show(&name).contains(PURGED) && !mem.agent_show(&name).contains(SECRET));
  assert!(owed.exists());
  wipe(&mut mem);
  assert!(!on_disk(&dir, SECRET));
  assert!(mem.agent("Nobody").is_none());
}

#[test]
fn legacy_agent_schema_and_view_lines() {
  configured();
  let home = tempfile::tempdir().unwrap();

  // an agent from before efforts were kept: its column added, at the run's
  let old = home.path().join("chats/old");
  std::fs::create_dir_all(&old).unwrap();
  rusqlite::Connection::open(old.join("chat.db"))
    .unwrap()
    .execute_batch("create table agents (name text primary key) without rowid; insert into agents values ('Ann');")
    .unwrap();
  let was = Memory::load(&old).unwrap();
  assert!(was.agent("Ann").is_some());
  assert_eq!(was.agent_settings("Ann"), (None, acorn_core::model::Model::Neon));

  // a view's last lines: messages not summarized yet as their kind and text
  let mut thread = Memory::load(&home.path().join("chats/max/with/main")).unwrap();
  for (kind, text) in [
    (Kind::User, "first"),
    (Kind::User, "is the deploy safe?"),
    (Kind::Acorn, "no:\nthe key leaks"),
  ] {
    thread.push(vec![Msg::new(kind, text)]).unwrap();
  }
  thread.fit(false);
  assert_eq!(
    thread.view_lines(2),
    ["1+1|user: is the deploy safe?", "2+1|unii: no: the key leaks"]
  );
  assert_eq!(thread.view_lines(usize::MAX).len(), 3);
}
