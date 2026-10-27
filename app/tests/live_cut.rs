//! Cutting in, live: the window, headless, on the real engine and
//! DeepSeek, over a chat of its own (its own process: the engines are one
//! a process). A message sent while the reply streams stops it at once,
//! and the call goes on to answer it, DeepSeek taking the reply cut off
//! back as said.

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

/// Asks the model; run with `cargo test -- --ignored` and DEEPSEEK_API_KEY.
#[test]
#[ignore]
fn a_message_cuts_in_on_a_reply() {
    let _dir = chat();
    let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
    app.first_render().unwrap();
    app.wait_for_within("Hello! This is Acorn.", Duration::from_secs(20)).unwrap();

    // a reply written, not a tool's work: only a reply (or its thinking)
    // streams, and either may be what's cut
    app.type_text("label:Message Acorn", "Use no tools. In your reply itself, write the numbers from one to six hundred in French words, one per line, nothing else.").unwrap();
    app.submit("label:Message Acorn").unwrap();
    app.wait_until("the reply streaming", Duration::from_secs(180), |s| s.texts().iter().any(|t| t.contains("vingt-cinq"))).unwrap();

    app.type_text("label:Message Acorn", "Stop. Reply with exactly the word: pecan").unwrap();
    app.submit("label:Message Acorn").unwrap();
    app.wait_for_within("text:— cut off", Duration::from_secs(10)).unwrap();
    app.wait_until("the answer to the cut-in", Duration::from_secs(180), |s| s.texts().iter().any(|t| t.trim().trim_end_matches('.') == "pecan")).unwrap();
    app.wait_until("the cut-in's call accepted, the count stopped", Duration::from_secs(1), |s| {
        let texts = s.texts();
        assert!(!texts.iter().any(|t| t.starts_with("No reply")), "the request after the cut was accepted: {texts:?}");
        assert!(!texts.iter().any(|t| t.contains("cinq cent quatre-vingt-dix")), "the count stopped where it was cut");
        true
    })
    .unwrap();
}
