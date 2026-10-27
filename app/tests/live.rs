//! The window, headless, on the real engine and DeepSeek, over a chat of
//! its own (its own process: the engine is one a process).

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use std::time::Duration;

/// A chat dir of the test's own, set before anything reads the config.
fn chat() -> tempfile::TempDir {
  let dir = tempfile::tempdir().unwrap();
  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("ACORN_DIR", dir.path().join("home"));
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
  }
  dir
}

fn open() -> Headless {
  let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
  app.first_render().unwrap();
  app
}

/// Asks the model; run with `cargo test -- --ignored` and DEEPSEEK_API_KEY.
#[test]
#[ignore]
fn a_message_gets_a_reply() {
  let _dir = chat();
  let mut app = open();
  app
    .wait_for_within("Hello! This is Acorn.", Duration::from_secs(20))
    .unwrap();
  app
    .type_text("label:Message Acorn", "Reply with exactly the word: walnut")
    .unwrap();
  app.submit("label:Message Acorn").unwrap();
  app
    .wait_for_within("Reply with exactly the word: walnut", Duration::from_secs(10))
    .unwrap();
  app.wait_for_within("walnut", Duration::from_secs(120)).unwrap();
  app
    .wait_until("today's cost", Duration::from_secs(30), |s| {
      s.texts().iter().any(|t| t.ends_with(" today"))
    })
    .unwrap();
}
