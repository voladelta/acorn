//! A chat's memory in chat.db: logged, built and opened again as it was;
//! a message purged with every node built from it, its secret gone from
//! disk; facts kept, shadowed and forgotten across assistants.

use acorn_core::memory::{self, Memory, Msg, Part, PURGED};
use acorn_core::{Kind, facts};

const SECRET: &str = "hunter2-sk-live-0123456789";

/// Each node not built whose source is, built as its source clipped
/// (no model), till none is left.
fn build(mem: &mut Memory) {
    loop {
        let (free, jobs) = mem.starts();
        let jobs: Vec<_> = jobs.into_iter().map(|(p, text)| (p, memory::cut(&memory::flat(&text)).to_string())).collect();
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

/// A chat's wipe owed, done as the chat does it once idle.
fn wipe(mem: &mut Memory) {
    memory::wipe(&mem.dir).unwrap();
    mem.wiped(mem.purged);
}

/// Whether any of the chat's files holds `text`.
fn on_disk(dir: &std::path::Path, text: &str) -> bool {
    std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().is_file()).any(|e| {
        let data = std::fs::read(e.path()).unwrap();
        data.windows(text.len()).any(|w| w == text.as_bytes())
    })
}

#[test]
fn memory_in_sqlite() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: set once, before any thread of the test starts.
    unsafe {
        std::env::set_var("ACORN_DIR", home.path().join("home"));
        std::env::set_var("ACORN_CHATS", home.path().join("chats"));
    }
    let dir = home.path().join("chats/main");

    // logged, built, opened again as it was
    let mut mem = Memory::load(&dir).unwrap();
    for k in 0..12 {
        let text = if k == 5 { format!("my key is {SECRET}") } else { format!("message {k} {}", "x".repeat(k * 90)) };
        mem.push(vec![Msg::new(if k % 2 == 0 { Kind::User } else { Kind::Acorn }, text)]).unwrap();
    }
    mem.push(vec![Msg::new(Kind::Echo, "y".repeat(70_000))]).unwrap();
    build(&mut mem);
    let count = mem.count;
    assert!(count > 13, "a long text goes in pages");
    let view = mem.view.parts.clone();
    let top = mem.node(Part::new(3, 0)).expect("built");
    drop(mem);
    let mut mem = Memory::load(&dir).unwrap();
    assert_eq!((mem.count, &mem.view.parts), (count, &view));
    assert_eq!(mem.message(5).unwrap().text, format!("my key is {SECRET}"));
    assert_eq!(mem.node(Part::new(3, 0)).as_deref(), Some(top.as_str()));

    // purged: its text, and every node built from it
    let age = mem.age;
    let said = mem.purge(None, 5).unwrap();
    assert!(said.starts_with("Purged message 5"), "{said}");
    assert_eq!(mem.message(5).unwrap().text, PURGED);
    assert_eq!(mem.age, age + 1);
    for part in [Part::new(0, 5), Part::new(1, 2), Part::new(2, 1), Part::new(3, 0)] {
        assert!(mem.node(part).is_none(), "{part:?} was built from it");
    }
    assert!(mem.node(Part::new(0, 4)).is_some() && mem.node(Part::new(1, 3)).is_some(), "the rest stays");
    assert!(mem.view.parts.iter().all(|p| mem.text(*p).find(SECRET).is_none()));
    build(&mut mem);
    assert!(mem.node(Part::new(3, 0)).is_some_and(|t| !t.contains(SECRET)), "built anew without it");
    let owed = dir.join("chat.db.wipe");
    assert!(owed.exists(), "a wipe is owed");
    drop(mem);
    let mut mem = Memory::load(&dir).unwrap();
    assert_eq!(mem.message(5).unwrap().text, PURGED);
    assert!(mem.purged > mem.wiped, "a quit doesn't lose the wipe owed");
    wipe(&mut mem);
    assert!(!owed.exists() && !on_disk(&dir, SECRET), "erased from chat.db and its WAL");
    assert!(mem.purge(None, 999).unwrap().starts_with("No message"));

    // a long message, its secret on every line, purged from every page
    let long: String = (0..2500).map(|k| format!("line {k} {SECRET}\n")).collect();
    mem.push(vec![Msg::new(Kind::User, long)]).unwrap();
    let id = mem.count - 1;
    build(&mut mem);
    assert!(on_disk(&dir, SECRET));
    let said = mem.purge(None, id).unwrap();
    assert!(said.contains("its pages"), "{said}");
    let upto = mem.purged;
    mem.purge(None, 0).unwrap();
    memory::wipe(&dir).unwrap();
    mem.wiped(upto);
    assert!(owed.exists(), "a purge during a wipe owes another");
    wipe(&mut mem);
    assert!(!owed.exists() && !on_disk(&dir, SECRET), "no stale copy left in chat.db");
    build(&mut mem);

    // a quit in the middle of a message: its pages so far dropped
    let count = mem.count;
    mem.push(vec![Msg { more: true, ..Msg::new(Kind::Acorn, "half") }]).unwrap();
    drop(mem);
    let mut mem = Memory::load(&dir).unwrap();
    assert_eq!(mem.count, count);
    mem.push(vec![Msg::new(Kind::User, "next")]).unwrap();
    assert_eq!(mem.message(count).unwrap().text, "next");

    // an agent's log
    let name = mem.agent_new().unwrap();
    mem.agent_push(&name, Kind::Note, "subagent").unwrap();
    mem.agent_push(&name, Kind::Echo, &format!("cat .env: {SECRET}")).unwrap();
    assert_eq!(mem.agent_names(), std::slice::from_ref(&name));
    assert_eq!(mem.agent(&name).unwrap().len(), 2);
    mem.purge(Some(&name), 1).unwrap();
    assert!(mem.agent_show(&name).contains(PURGED) && !mem.agent_show(&name).contains(SECRET));
    assert!(owed.exists());
    wipe(&mut mem);
    assert!(!on_disk(&dir, SECRET));
    assert!(mem.agent("Nobody").is_none());

    // facts, across assistants
    let fact = |mine, subject: &str, predicate: &str, value: &str, many| facts::Fact { mine, subject: subject.into(), predicate: predicate.into(), value: value.into(), many, chat: "main".into(), msg: 3 };
    assert!(facts::remember("main", fact(false, "User", "editor", "vim", false)).unwrap().contains("every assistant"));
    let said = facts::remember("max", fact(false, " user ", "Editor", "Helix", false)).unwrap();
    assert!(said.contains("replaces #1 vim"), "{said}");
    facts::remember("max", fact(true, "user", "editor", "emacs", false)).unwrap();
    facts::remember("main", fact(false, "user", "language", "Rust", true)).unwrap();
    facts::remember("main", fact(false, "user", "language", "Go", true)).unwrap();
    assert!(facts::remember("main", fact(false, "user", "language", "Go", true)).unwrap().starts_with("Already kept"));
    let main = facts::prompt("main");
    assert!(main.contains("user / editor: Helix") && main.contains("language: Rust") && main.contains("language: Go") && !main.contains("emacs"), "{main}");
    let max = facts::prompt("max");
    assert!(max.contains("user / editor: emacs (yours)") && !max.contains("Helix"), "max's own shadows everyone's: {max}");
    assert!(facts::forget("main", 3).unwrap().starts_with("No fact"), "main doesn't see max's");
    assert!(facts::forget("max", 3).unwrap().starts_with("Forgot"));
    assert!(facts::prompt("max").contains("Helix"));
    facts::remember("main", fact(false, "user", "password", SECRET, false)).unwrap();
    let id = facts::prompt("main").lines().find(|l| l.contains(SECRET)).and_then(|l| l[1..].split(' ').next()?.parse::<u64>().ok()).unwrap();
    facts::forget("main", id).unwrap();
    assert!(!on_disk(&home.path().join("home"), SECRET), "a fact forgotten is erased");
    assert!(facts::prompt("nobody").contains("Helix") && !facts::prompt("nobody").contains("emacs"));
}
