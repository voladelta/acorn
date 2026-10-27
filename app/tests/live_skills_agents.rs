//! Skills used by hired agents, live. Acorn is told to hand over the job
//! word for word (it loads skills too, and would pass their rules on), so
//! an agent that follows one found it itself; the chat's db shows who called
//! load_skill (an agent's log is named by the agent, Acorn's has no name).

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

/// Calls to `tool` by each log of the chat: (log, count), Acorn's log named "".
fn calls(db: &Path, tool: &str) -> Vec<(String, u32)> {
  let sql = format!("select log, count(*) from msgs where kind = 'tool' and text like '%{tool} {{%' group by log;");
  let out = Command::new("sqlite3")
    .arg("-readonly")
    .arg(db)
    .arg(sql)
    .output()
    .unwrap();
  String::from_utf8_lossy(&out.stdout)
    .lines()
    .filter_map(|l| {
      let (log, n) = l.rsplit_once('|')?;
      Some((log.to_string(), n.parse().ok()?))
    })
    .collect()
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
fn an_agent_finds_and_follows_skills_nobody_told_it_of() {
  let dir = tempfile::tempdir().unwrap();
  let home = dir.path().join("home");
  let db = dir.path().join("chats/default/chat.db");
  let proj = dir.path().join("data-proj");
  skill(
    &home.join("skills"),
    "changelog",
    "Use when asked to write release notes or a changelog",
    "Bullet list. The last line is exactly: SIGNED-OFF: then the word kestrel written in capitals.",
  );
  skill(
    &home.join("skills"),
    "standup",
    "Use when asked for a standup update",
    "Two bullets. The last line is exactly: DONE: then the word walrus written in capitals.",
  );
  skill(
    &proj.join(".acorn/skills"),
    "data-report",
    "Use when asked to write a data report",
    "Three short lines. The last line is exactly: CHARTED: then the word osprey written in capitals.",
  );

  let _dump = Dump(db.clone());

  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("HOME", dir.path().join("user"));
    std::env::set_var("ACORN_DIR", &home);
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
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
      .wait_until(word, Duration::from_secs(240), |s| {
        s.texts().iter().any(|t| t.contains(word))
      })
      .unwrap();
    std::thread::sleep(Duration::from_secs(10));
  };
  let by_agents = |tool: &str| calls(&db, tool).into_iter().filter(|(log, _)| !log.is_empty()).count();
  let p = proj.display();

  // A. a global skill: the agent's task is only the job
  ask(
    &mut app,
    "Hire one agent whose task is exactly this text, word for word, with nothing added or changed: \"Write the release notes for v4: faster startup, a fixed crash.\" Then tell me its report.",
    "KESTREL",
  );
  println!("A: load_skill by log: {:?}", calls(&db, "load_skill"));
  assert_eq!(
    by_agents("load_skill"),
    1,
    "the agent loaded no skill itself: {:?}",
    calls(&db, "load_skill")
  );

  // B. a project's skill: the agent is told only the folder
  ask(
    &mut app,
    &format!(
      "Hire one agent whose task is exactly this text, word for word, with nothing added or changed: \"Work in the project folder {p} and write a data report: sales up 5%, returns down 2%.\" Then tell me its report."
    ),
    "OSPREY",
  );
  println!("B: load_skill by log: {:?}", calls(&db, "load_skill"));
  assert_eq!(
    by_agents("load_skill"),
    2,
    "the second agent loaded no skill itself: {:?}",
    calls(&db, "load_skill")
  );
}
