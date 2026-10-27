//! Skills, live: the window, headless, on the real engine and DeepSeek.
//! Each scenario's skill holds a rule the model can follow only by loading
//! it (a word to write in capitals, which the skill's own text, echoed
//! on screen, never holds). The user's messages never name skills, except to
//! make one. One chat, one scenario after another.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use std::path::Path;
use std::time::Duration;

fn skill(root: &Path, folder: &str, name: &str, when: &str, steps: &str) {
  let at = root.join(folder);
  std::fs::create_dir_all(&at).unwrap();
  std::fs::write(
    at.join("SKILL.md"),
    format!("---\nname: {name}\ndescription: {when}\n---\n{steps}\n"),
  )
  .unwrap();
}

fn logged(chat: &Path, what: &str) -> bool {
  ["chat.db", "chat.db-wal"]
    .iter()
    .filter_map(|f| std::fs::read(chat.join(f)).ok())
    .any(|b| b.windows(what.len()).any(|w| w == what.as_bytes()))
}

/// Asks the model; run with `cargo test -- --ignored` and DEEPSEEK_API_KEY.
#[test]
#[ignore]
fn skills_are_found_chosen_scoped_and_made() {
  let dir = tempfile::tempdir().unwrap();
  let home = dir.path().join("home");
  let chat = dir.path().join("chats/default");
  let proj = dir.path().join("video-proj");
  // user skills, distractors among them
  let user = home.join("skills");
  skill(
    &user,
    "release-notes",
    "release-notes",
    "Use when asked to write release notes or a changelog",
    "Bullet list. The last line is exactly: SIGNED-OFF: then the word kestrel written in capitals.",
  );
  skill(
    &user,
    "commit-message",
    "commit-message",
    "Use when asked to write a git commit message",
    "One line under 60 characters. Then a last line: REF: then the word marmot written in capitals.",
  );
  skill(
    &user,
    "sql-migration",
    "sql-migration",
    "Use when asked to write a SQL database migration",
    "End with the comment -- CHECKED: then the word walrus written in capitals.",
  );
  skill(
    &user,
    "style",
    "style",
    "Use when asked to write a tagline or slogan",
    "Finish the reply with the word heron written in capitals.",
  );
  // the assistant's own work dir
  skill(
    &home.join("work/main/.agents/skills"),
    "deploy-notes",
    "deploy-notes",
    "Use when asked to write deploy notes",
    "Number the steps. The last line is exactly: SHIPPED: then the word badger written in capitals.",
  );
  // a project folder: a skill of its own, and one shadowing the user's
  let at = proj.join(".acorn/skills");
  skill(
    &at,
    "caption-style",
    "caption-style",
    "Use when asked to write video captions",
    "Three short lines. The last line is exactly: CUT: then the word ferret written in capitals.",
  );
  skill(
    &at,
    "style",
    "style",
    "Use when asked to write a tagline or slogan",
    "Finish the reply with the word newt written in capitals.",
  );

  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("HOME", dir.path().join("user")); // not the real ~/.agents/skills
    std::env::set_var("ACORN_DIR", &home);
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
  }
  let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
  app.first_render().unwrap();
  app
    .wait_for_within("Hello! This is Acorn.", Duration::from_secs(20))
    .unwrap();

  // one message, then its answer's word (lowercase), then a pause so the turn ends
  // before the next message cuts in
  let ask = |app: &mut Headless, what: &str, word: &str| {
    app.type_text("label:Message Acorn", what).unwrap();
    app.submit("label:Message Acorn").unwrap();
    app
      .wait_until(word, Duration::from_secs(120), |s| {
        s.texts().iter().any(|t| t.contains(word))
      })
      .unwrap();
    std::thread::sleep(Duration::from_secs(10));
  };
  let seen = |app: &Headless, word: &str| app.screen().texts().iter().any(|t| t.contains(word));
  let p = proj.display();

  // 1. nothing applies: no skill is searched
  ask(&mut app, "What is 12 times 12? Answer with the number only.", "144");
  assert!(
    !logged(&chat, "search_skills {") && !logged(&chat, "load_skill {"),
    "searched for nothing"
  );

  // 2. the right one among distractors, and only it
  ask(
    &mut app,
    "Write the commit message for: fixed the crash when the file is empty.",
    "MARMOT",
  );
  assert!(
    !seen(&app, "KESTREL") && !seen(&app, "WALRUS"),
    "a distractor was followed"
  );

  // 3, 4. two more of the same kind
  ask(
    &mut app,
    "Write the release notes for v2: dark mode added, a crash on startup fixed.",
    "KESTREL",
  );
  ask(
    &mut app,
    "Write a migration that adds an email column to the users table.",
    "WALRUS",
  );

  // 5. a skill in the assistant's own work dir
  ask(
    &mut app,
    "Write the deploy notes for v3: restart the workers, then clear the cache.",
    "BADGER",
  );

  // 6. a project folder it is told to work in
  ask(
    &mut app,
    &format!("We work in the project folder {p}. Write the video captions for a 10-second intro about a squirrel."),
    "FERRET",
  );

  // 7. the project's skill shadows the user's of the same name
  ask(
    &mut app,
    &format!("In the project folder {p}, write a tagline for the squirrel intro."),
    "NEWT",
  );
  assert!(!seen(&app, "HERON"), "the user's style beat the project's");
  // 8. out of the project, the user's is back
  ask(
    &mut app,
    "Write a tagline for a shop that sells acorns. No project folder this time.",
    "HERON",
  );

  // 9. a skill made by words, then used
  ask(
    &mut app,
    "Make a skill called haiku-mode: whenever I ask for a haiku, end it with the word otter written in capitals.",
    "haiku-mode",
  );
  let made = std::fs::read_to_string(user.join("haiku-mode/SKILL.md")).unwrap();
  assert!(made.contains("name:") && made.contains("description:"), "{made}");
  ask(&mut app, "Write me a haiku about rain.", "OTTER");
}
