//! A chat's memory in chat.db: logged, built and opened again as it was;
//! searched; a message purged with every node built from it, taken back,
//! and past its grace its secret gone from disk and the search; facts
//! kept, shadowed and forgotten across assistants; a view's last lines,
//! the latest messages there before they are summarized.

use acorn_core::effort::Effort;
use acorn_core::memory::{self, Memory, Msg, PURGED, Part};
use acorn_core::{Kind, facts};

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

#[test]
fn persistence_fts_search_and_zoom() {
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
  wipe(&mut mem);
  assert!(mem.held_till().is_none(), "past its grace, a wipe deletes it");
  assert!(
    !owed.exists() && !on_disk(&dir, SECRET),
    "erased from chat.db and its WAL"
  );
  assert!(mem.purge(None, 999).unwrap().starts_with("No message"));
}

#[test]
fn long_message_purge_and_concurrent_wipe() {
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
  let name = mem.agent_new(Some(Effort::Low)).unwrap();
  assert_eq!(mem.agent_effort(&name), Some(Effort::Low));
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
  let home = tempfile::tempdir().unwrap();

  // an agent from before efforts were kept: its column added, at the run's
  let old = home.path().join("chats/old");
  std::fs::create_dir_all(&old).unwrap();
  rusqlite::Connection::open(old.join("chat.db"))
    .unwrap()
    .execute_batch("create table agents (name text primary key) without rowid; insert into agents values ('Ann');")
    .unwrap();
  let was = Memory::load(&old).unwrap();
  assert!(was.agent("Ann").is_some() && was.agent_effort("Ann").is_none());

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

#[test]
fn facts_shared_shadowed_and_erased_on_forget() {
  let home = tempfile::tempdir().unwrap();
  // SAFETY: only this test in this binary reads `ACORN_DIR` via `config::get()`.
  unsafe {
    std::env::set_var("ACORN_DIR", home.path().join("home"));
    std::env::set_var("ACORN_CHATS", home.path().join("chats"));
  }

  let fact = |mine, subject: &str, predicate: &str, value: &str, many| facts::Fact {
    mine,
    subject: subject.into(),
    predicate: predicate.into(),
    value: value.into(),
    many,
    chat: "main".into(),
    msg: 3,
  };
  assert!(
    facts::remember("main", fact(false, "User", "editor", "vim", false))
      .unwrap()
      .contains("every assistant")
  );
  let said = facts::remember("max", fact(false, " user ", "Editor", "Helix", false)).unwrap();
  assert!(said.contains("replaces #1 vim"), "{said}");
  facts::remember("max", fact(true, "user", "editor", "emacs", false)).unwrap();
  facts::remember("main", fact(false, "user", "language", "Rust", true)).unwrap();
  facts::remember("main", fact(false, "user", "language", "Go", true)).unwrap();
  assert!(
    facts::remember("main", fact(false, "user", "language", "Go", true))
      .unwrap()
      .starts_with("Already kept")
  );
  let main = facts::prompt("main");
  assert!(
    main.contains("user / editor: Helix")
      && main.contains("language: Rust")
      && main.contains("language: Go")
      && !main.contains("emacs"),
    "{main}"
  );
  let max = facts::prompt("max");
  assert!(
    max.contains("user / editor: emacs (yours)") && !max.contains("Helix"),
    "max's own shadows everyone's: {max}"
  );
  assert!(
    facts::forget("main", 3).unwrap().starts_with("No fact"),
    "main doesn't see max's"
  );
  assert!(facts::forget("max", 3).unwrap().starts_with("Forgot"));
  assert!(facts::prompt("max").contains("Helix"));
  facts::remember("main", fact(false, "user", "password", SECRET, false)).unwrap();
  let id = facts::prompt("main")
    .lines()
    .find(|l| l.contains(SECRET))
    .and_then(|l| l[1..].split(' ').next()?.parse::<u64>().ok())
    .unwrap();
  facts::forget("main", id).unwrap();
  assert!(
    !on_disk(&home.path().join("home"), SECRET),
    "a fact forgotten is erased"
  );
  assert!(facts::prompt("nobody").contains("Helix") && !facts::prompt("nobody").contains("emacs"));
}
