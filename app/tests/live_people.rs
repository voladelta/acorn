//! Asking and hiring, live: the window, headless, on the real engine and
//! DeepSeek, over a chat of its own (its own process: the engines are one
//! a process). `@Max` in a message has Acorn ask Max, whose reply comes
//! back framed under his name; work Acorn is told to hire for goes to an
//! agent, whose report comes back the same way.

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
fn acorn_asks_max_and_hires_an_agent() {
    let dir = chat();
    let chats = dir.path().join("chats");
    let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
    app.first_render().unwrap();
    app.wait_for_within("Hello! This is Acorn.", Duration::from_secs(20)).unwrap();

    app.type_text("label:Message Acorn", "@Max reply to Acorn with only the French word for walnut, in lowercase").unwrap();
    app.submit("label:Message Acorn").unwrap();
    let thread = chats.join("max/with/default");
    app.wait_until("Acorn asking Max, in their thread", Duration::from_secs(180), |_| thread.is_dir()).unwrap();
    // Max's name on the frame, the word in it: not thinking that names it
    app.wait_until("Max's reply, framed under his name", Duration::from_secs(300), |s| {
        s.texts().windows(2).any(|w| w[0] == "Max" && w[1].trim().trim_end_matches('.').eq_ignore_ascii_case("noix"))
    })
    .unwrap();

    app.type_text("label:Message Acorn", "Hire one agent to run echo acorn-$((37*43)) in the shell, and tell me its report.").unwrap();
    app.submit("label:Message Acorn").unwrap();
    app.wait_until("the agent's report told", Duration::from_secs(300), |s| s.texts().iter().any(|t| t.contains("acorn-1591"))).unwrap();
    assert!(logged(&chats.join("default"), "hire {"), "an agent hired, not the work done by Acorn");
}

/// Whether a chat's files hold `what` (its db and the db's write-ahead log).
fn logged(chat: &std::path::Path, what: &str) -> bool {
    ["chat.db", "chat.db-wal"].iter().filter_map(|f| std::fs::read(chat.join(f)).ok()).any(|b| b.windows(what.len()).any(|w| w == what.as_bytes()))
}
