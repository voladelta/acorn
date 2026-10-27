//! Skills over time, live: a hired agent on a many-step job still lists
//! before it writes, and a skill is found again after the chat's memory
//! has compacted what loaded it (the view is kept tiny to force that, and
//! the skill is edited, so only a fresh load can pass).

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn skill(root: &Path, folder: &str, when: &str, steps: &str) {
  let at = root.join(folder);
  std::fs::create_dir_all(&at).unwrap();
  std::fs::write(
    at.join("SKILL.md"),
    format!("---\nname: {folder}\ndescription: {when}\n---\n{steps}\n"),
  )
  .unwrap();
}

fn sql(db: &Path, query: &str) -> String {
  let out = Command::new("sqlite3")
    .arg("-readonly")
    .arg(db)
    .arg(query)
    .output()
    .unwrap();
  String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// On a panic, prints the chat's whole log (Acorn's and every agent's), since
/// the temp dir goes with the test.
struct Dump(std::path::PathBuf);

impl Drop for Dump {
  fn drop(&mut self) {
    if !std::thread::panicking() {
      return;
    }
    let sql = "select log, i, kind, replace(substr(text, 1, 400), char(10), ' / ') from msgs order by log, i;";
    let out = std::process::Command::new("sqlite3")
      .arg("-readonly")
      .arg(&self.0)
      .arg(sql)
      .output();
    println!(
      "---- chat log at failure ----\n{}",
      out
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
    );
  }
}

/// Asks the model; run with `cargo test -- --ignored` and DEEPSEEK_API_KEY.
#[test]
#[ignore]
fn skills_survive_long_agent_jobs_and_compaction() {
  let dir = tempfile::tempdir().unwrap();
  let home = dir.path().join("home");
  let db = dir.path().join("chats/default/chat.db");
  let user = home.join("skills");
  skill(
    &user,
    "changelog",
    "Use when asked to write release notes or a changelog",
    "Bullet list. The last line is exactly: SIGNED-OFF: then the word kestrel written in capitals.",
  );
  skill(
    &user,
    "launch-notes",
    "Use when asked to write launch notes",
    "Numbered steps. The last line is exactly: SHIPPED: then the word badger written in capitals.",
  );

  let _dump = Dump(db.clone());

  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("HOME", dir.path().join("user"));
    std::env::set_var("ACORN_DIR", &home);
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
    std::env::set_var("ACORN_VIEW", "3000");
  }
  let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
  app.first_render().unwrap();
  app
    .wait_for_within("Hello! This is Acorn.", Duration::from_secs(20))
    .unwrap();
  let ask = |app: &mut Headless, what: &str, word: &str| {
    app.type_text("label:Message Acorn", what).unwrap();
    app.submit("label:Message Acorn").unwrap();
    app
      .wait_until(word, Duration::from_secs(300), |s| {
        s.texts().iter().any(|t| t.contains(word))
      })
      .unwrap();
    std::thread::sleep(Duration::from_secs(10));
  };

  // a long agent job: many steps first, the writing last
  ask(
    &mut app,
    "Hire one agent whose task is exactly this text, word for word, with nothing added or changed: \"Run these shell commands one by one, each as its own call: `echo one`, `date`, `echo two`, `uname`, `echo three`, `pwd`; then write the launch notes for v5 (restart workers, clear cache).\" Then tell me its report.",
    "BADGER",
  );
  let agent_loads = sql(
    &db,
    "select count(*) from msgs where log <> '' and kind = 'tool' and text like '%load_skill {%';",
  );
  assert_eq!(agent_loads, "1", "the agent never loaded the skill itself");

  // use the skill once, then bury it under other talk until memory compacts
  ask(
    &mut app,
    "Write the release notes for v2: dark mode added, a crash on startup fixed.",
    "KESTREL",
  );
  for (q, a) in [
    ("Name three rivers in Europe, one line.", "Rhine"),
    ("What is the capital of Australia? One word.", "Canberra"),
    ("Name three primary colors, one line.", "blue"),
    ("What is 15 times 15? Number only.", "225"),
    ("Name the largest ocean, one word.", "Pacific"),
    ("What is the chemical symbol for gold? Two letters.", "Au"),
  ] {
    ask(&mut app, q, a);
  }
  let nodes = sql(&db, "select count(*) from nodes;");
  println!("compacted nodes: {nodes}");
  assert_ne!(nodes, "0", "memory never compacted, so the test proves nothing");

  // the skill changes; only a fresh list and load can follow it now
  skill(
    &user,
    "changelog",
    "Use when asked to write release notes or a changelog",
    "Bullet list. The last line is exactly: SIGNED-OFF: then the word condor written in capitals.",
  );
  ask(
    &mut app,
    "Write the release notes for v3: faster search, a fixed login bug.",
    "CONDOR",
  );
}
