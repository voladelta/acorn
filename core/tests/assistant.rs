//! Assistants made as `create_assistant` makes them: saved, listed after
//! Acorn and Max, each with its own chat id, work dir and prompt.

use acorn_core::assistant::{self, Assistant};
use acorn_core::effort::Effort;

#[test]
fn made_assistants_are_saved_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: set once, before any thread of the test starts.
    unsafe {
        std::env::set_var("ACORN_DIR", dir.path().join("home"));
        std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
    }
    std::fs::create_dir_all(dir.path().join("chats/abc")).unwrap();

    let names = |all: Vec<Assistant>| all.into_iter().map(|a| a.name).collect::<Vec<_>>();
    assert_eq!(names(assistant::all()), ["Acorn", "Max"]);

    let abc = assistant::add(" Abc ", " Personal stylist ", "Help me with xyz.", Effort::Low).unwrap();
    assert_eq!(abc.id, "abc-2", "a chat on disk it didn't start is never taken");
    assert_eq!((abc.about.as_str(), abc.effort), ("Personal stylist", Effort::Low));
    assert!(abc.work().ends_with("work/assistants/abc-2"));
    assert!(abc.personality().contains("calls you Abc: Personal stylist.\n\nHelp me with xyz."));
    assert!(abc.asking().contains("your thread with Abc (Personal stylist)"));
    assert!(assistant::add("abc", "Stylist", "", Effort::High).is_err(), "names are unique");
    assert!(assistant::add("  ", "Stylist", "", Effort::High).is_err());
    assert!(assistant::add("Doug", " ", "", Effort::High).is_err(), "everyone says who they are");

    let max = assistant::add("Max Two", "Second opinion", "", Effort::Max).unwrap();
    assert_eq!(max.id, "maxtwo");
    assert_eq!(names(assistant::all()), ["Acorn", "Max", "Abc", "Max Two"]);
    assert_eq!(assistant::all()[2], abc, "as saved, read back");
    assert!(Assistant::acorn().personality().is_empty() && Assistant::acorn().work().ends_with("home/work"));
    assert_eq!((Assistant::acorn().about, Assistant::max().about), ("General assistant".to_string(), "Thoughtful expert".to_string()));
    assert_eq!(Assistant::max().effort, Effort::Max);
}
