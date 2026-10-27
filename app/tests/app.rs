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
    std::fs::write(home.join("assistants.json"), r#"[{"id": "pip", "name": "Pip", "about": "Personal stylist", "instructions": "Cheerful and brief.", "effort": "low"}]"#).unwrap();
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
/// named as who it is (never its effort), and again when picked again.
#[test]
fn each_assistant_opens_its_own_chat() {
    let dir = chat();
    let mut app = open();
    let wait = Duration::from_secs(20);
    app.wait_for_within("text:Acorn is an AI, not a person, and can make mistakes.", wait).unwrap();
    app.wait_for_within("Hello! This is Acorn.", wait).unwrap();
    app.wait_for_within("text:Acorn  ·  General assistant", wait).unwrap();

    for (name, about) in [("Max", "Thoughtful expert"), ("Pip", "Personal stylist"), ("Acorn", "General assistant"), ("Max", "Thoughtful expert")] {
        app.press(&format!("text:{name}")).unwrap();
        app.wait_for_within(&format!("Hello! This is {name}."), wait).unwrap();
        app.wait_for_within(&format!("text:{name}  ·  {about}"), wait).unwrap();
        assert!(!app.shows("effort"), "who it is, not how hard it thinks");
        for other in ["Acorn", "Max", "Pip"].into_iter().filter(|o| *o != name) {
            assert!(!app.shows(&format!("Hello! This is {other}.")), "{name}'s chat, not {other}'s");
        }
    }
    // `@` names the others as you type; Return takes the first, and `@Name` alone talks to them.
    app.type_text("label:Message Max", "@pi").unwrap();
    app.wait_for_within("text:@Pip", wait).unwrap();
    app.submit("label:Message Max").unwrap();
    app.submit("label:Message Max").unwrap();
    app.wait_for_within("text:Pip  ·  Personal stylist", wait).unwrap();
    assert!(!app.shows("text:@Pip"), "the names gone with the word");

    for id in ["default", "max", "pip"] {
        assert!(dir.path().join("chats").join(id).is_dir(), "{id}'s chat on disk");
    }
}
