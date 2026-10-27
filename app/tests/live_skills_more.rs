//! Skills, live, the harder cases: a skill with a script of its own, tasks
//! worded without the skill's keywords, a skill edited between two
//! messages, and a hired agent. As in `live_skills`, each skill's rule is
//! a word in capitals that its own text, echoed on screen, never holds.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use std::path::Path;
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
fn skills_with_scripts_loose_wording_edits_and_agents() {
  let dir = tempfile::tempdir().unwrap();
  let home = dir.path().join("home");
  let user = home.join("skills");
  skill(
    &user,
    "changelog",
    "Use when asked to write release notes, a changelog or what shipped",
    "Bullet list. The last line is exactly: SIGNED-OFF: then the word kestrel written in capitals.",
  );
  skill(
    &user,
    "apology-email",
    "Use when writing an email that apologizes to a customer",
    "Three sentences. The last line is exactly: REF: then the word marmot written in capitals.",
  );
  skill(
    &user,
    "standup",
    "Use when asked for a standup update or a daily status",
    "Two bullets. The last line is exactly: DONE: then the word walrus written in capitals.",
  );
  skill(
    &user,
    "launch-notes",
    "Use when asked to write launch notes",
    "Numbered steps. The last line is exactly: SHIPPED: then the word badger written in capitals.",
  );
  skill(
    &user,
    "greeting-card",
    "Use when asked to write a greeting card message",
    "Two lines. The last line is exactly: FROM: then the word falcon written in capitals.",
  );
  skill(
    &user,
    "stamp-report",
    "Use when asked for a stamped report",
    "Run `sh scripts/stamp.sh` from this skill's folder and put its output line last in your reply.",
  );
  std::fs::create_dir_all(user.join("stamp-report/scripts")).unwrap();
  std::fs::write(
    user.join("stamp-report/scripts/stamp.sh"),
    "echo \"STAMP-$((6*7))-OWL\"\n",
  )
  .unwrap();

  let _dump = Dump(dir.path().join("chats/default/chat.db"));

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
      .wait_until(word, Duration::from_secs(120), |s| {
        s.texts().iter().any(|t| t.contains(word))
      })
      .unwrap();
    std::thread::sleep(Duration::from_secs(10)); // let the turn end before the next message cuts in
  };

  // a script beside SKILL.md, run from the skill's folder
  ask(
    &mut app,
    "Give me a stamped report on the build: all green.",
    "STAMP-42-OWL",
  );

  // tasks worded without the skill's own words
  ask(
    &mut app,
    "I need to tell customers what is new in 2.2: a new export button.",
    "KESTREL",
  );
  ask(
    &mut app,
    "A customer's order arrived broken. Draft something to send them, saying sorry.",
    "MARMOT",
  );
  ask(
    &mut app,
    "What do I say at the team's morning sync? I fixed the login bug and started on search.",
    "WALRUS",
  );

  // a skill edited between two messages is read afresh
  ask(
    &mut app,
    "Write a greeting card message for my aunt's birthday.",
    "FALCON",
  );
  skill(
    &user,
    "greeting-card",
    "Use when asked to write a greeting card message",
    "Two lines. The last line is exactly: FROM: then the word puffin written in capitals.",
  );
  ask(
    &mut app,
    "Write a greeting card message for my uncle's retirement.",
    "PUFFIN",
  );

  // a hired agent uses skills as well
  ask(
    &mut app,
    "Hire one agent to write the launch notes for v3 (restart workers, clear cache) and tell me what it reports. Do not write them yourself.",
    "BADGER",
  );
}
