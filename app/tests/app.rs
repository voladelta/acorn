//! The window, headless, on the real engine, over a chat of its own.

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

#[test]
fn a_new_chat_opens_with_its_hello() {
    let _dir = chat();
    let mut app = open();
    app.wait_for_within("text:Acorn is an AI, not a person, and can make mistakes.", Duration::from_secs(20)).unwrap();
    app.wait_for_within("Hello! This is Acorn.", Duration::from_secs(20)).unwrap();
    assert!(app.shows("high"), "the pick shows the effort");
}
