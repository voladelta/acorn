//! Asking and hiring, live: the window, headless, on the real engine and
//! DeepSeek, over a chat of its own (its own process: the engines are one
//! a process). `@Max` in a message has Acorn ask Max, whose reply comes
//! back framed under his name; a named person to hire who stays is made
//! an assistant; work Acorn is told to hire for goes to an agent, whose
//! report comes back the same way; and Max, talked to, remembers what
//! Acorn asked him.

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
fn acorn_asks_max_creates_marrie_hires_an_agent_and_max_remembers() {
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
        s.texts().windows(2).any(|w| w[0] == "Max" && w[1].to_lowercase().contains("noix"))
    })
    .unwrap();

    app.type_text("label:Message Acorn", "Hire a personal stylist called Marrie, to stay with me.").unwrap();
    app.submit("label:Message Acorn").unwrap();
    let made = dir.path().join("home/assistants.json");
    app.wait_until("Marrie created", Duration::from_secs(300), |_| std::fs::read_to_string(&made).is_ok_and(|a| a.contains("Marrie"))).unwrap();
    app.wait_for_within("text:Marrie", Duration::from_secs(10)).unwrap();
    assert!(!logged(&chats.join("default"), "hire {"), "Marrie is an assistant, not an agent");

    app.type_text("label:Message Acorn", "Hire one agent to run echo acorn-$((37*43)) in the shell, and tell me its report.").unwrap();
    app.submit("label:Message Acorn").unwrap();
    let acorn = chats.join("default");
    app.wait_until("an agent hired", Duration::from_secs(180), |_| logged(&acorn, "hire {")).unwrap();
    // the agent's name on the frame, its report in it: not Acorn's thinking, which may work it out
    app.wait_until("the agent's report", Duration::from_secs(300), |s| {
        let named = |w: &str| w.len() < 24 && w.starts_with(|c: char| c.is_ascii_uppercase()) && w.chars().all(char::is_alphanumeric);
        s.texts().windows(2).any(|w| named(w[0]) && w[1].contains("acorn-1591"))
    })
    .unwrap();

    // Max, talked to, knows Acorn (not you) asked him, and what he answered.
    app.type_text("label:Message Acorn", "@Max").unwrap();
    app.submit("label:Message Acorn").unwrap();
    app.wait_for_within("text:Max  ·  Thoughtful expert", Duration::from_secs(20)).unwrap();
    app.type_text("label:Message Max", "Use no tools. In one line: who asked you about a French word, and what did you answer?").unwrap();
    app.submit("label:Message Max").unwrap();
    app.wait_until("Max remembering his thread", Duration::from_secs(300), |s| {
        s.texts().iter().any(|t| {
            let t = t.to_lowercase();
            t.contains("acorn") && t.contains("noix") && !t.contains("who asked you")
        })
    })
    .unwrap();
}

/// Whether a chat's files hold `what` (its db and the db's write-ahead log).
fn logged(chat: &std::path::Path, what: &str) -> bool {
    ["chat.db", "chat.db-wal"].iter().filter_map(|f| std::fs::read(chat.join(f)).ok()).any(|b| b.windows(what.len()).any(|w| w == what.as_bytes()))
}
