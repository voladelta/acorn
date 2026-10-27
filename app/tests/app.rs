//! The window, headless, on the real engine, over chats of its own.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use std::time::Duration;

/// A dir of the test's own, set before anything reads the config, with
/// an assistant made before (as `create_assistant` saves one).
fn chat() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("assistants.json"), r#"[{"id": "pip", "name": "Pip", "instructions": "Cheerful and brief.", "effort": "low"}]"#).unwrap();
    // SAFETY: set once, before any thread of the test starts.
    unsafe {
        std::env::set_var("ACORN_DIR", &home);
        std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
    }
    dir
}

fn open() -> Headless {
    let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
    app.first_render().unwrap();
    app
}

/// One test, as the engines are the process's: Acorn's chat opens with
/// its hello; each assistant in the sidebar, picked, shows its own chat,
/// at its own effort, and again when picked again.
#[test]
fn each_assistant_opens_its_own_chat() {
    let dir = chat();
    let mut app = open();
    let wait = Duration::from_secs(20);
    app.wait_for_within("text:Acorn is an AI, not a person, and can make mistakes.", wait).unwrap();
    app.wait_for_within("Hello! This is Acorn.", wait).unwrap();
    app.wait_for_within("text:Acorn  high effort", wait).unwrap();

    for (name, effort) in [("Max", "max"), ("Pip", "low"), ("Acorn", "high"), ("Max", "max")] {
        app.press(&format!("text:{name}")).unwrap();
        app.wait_for_within(&format!("Hello! This is {name}."), wait).unwrap();
        app.wait_for_within(&format!("text:{name}  {effort} effort"), wait).unwrap();
        for other in ["Acorn", "Max", "Pip"].into_iter().filter(|o| *o != name) {
            assert!(!app.shows(&format!("Hello! This is {other}.")), "{name}'s chat, not {other}'s");
        }
    }
    for id in ["default", "max", "pip"] {
        assert!(dir.path().join("chats").join(id).is_dir(), "{id}'s chat on disk");
    }
}
