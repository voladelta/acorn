//! Facts kept, shadowed, and forgotten across assistants, in their own
//! test binary so setting `ACORN_DIR` happens before `config::get()` is
//! ever initialized in this process.

use acorn_core::facts;

const SECRET: &str = "hunter2-sk-live-0123456789";

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

#[test]
fn facts_shared_shadowed_and_erased_on_forget() {
  let home = tempfile::tempdir().unwrap();
  // SAFETY: sole test in this binary, before any `config::get()` call.
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
