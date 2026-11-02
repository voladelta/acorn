//! Assistants made as `create_assistant` makes them: saved, listed after
//! Acorn and Max, each with its own chat id, work dir and prompt; archived
//! and restored, their files kept.

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

  let abc = assistant::add(
    " Abc ",
    " Personal stylist ",
    "Help me with xyz.",
    " Hello! This is Abc.\n\nShow me your closet. ",
    Effort::Low,
  )
  .unwrap();
  assert_eq!(abc.id, "abc-2", "a chat on disk it didn't start is never taken");
  assert_eq!((abc.about.as_str(), abc.effort), ("Personal stylist", Effort::Low));
  assert!(abc.work().ends_with("work/assistants/abc-2"));
  assert!(
    abc
      .personality()
      .contains("calls you Abc: Personal stylist.\n\nHelp me with xyz.")
  );
  assert_eq!(
    abc.hello().0,
    "Hello! This is Abc.\n\nShow me your closet.",
    "its welcome, as made"
  );
  assert_eq!(
    abc.hello().1,
    "unii: hello: Abc, Personal stylist: Hello! This is Abc. Show me your closet."
  );
  assert!(abc.asking().contains("your thread with Abc (Personal stylist)"));
  assert!(
    assistant::add("abc", "Stylist", "", "", Effort::High).is_err(),
    "names are unique"
  );
  assert!(assistant::add("  ", "Stylist", "", "", Effort::High).is_err());
  assert!(
    assistant::add("Doug", " ", "", "", Effort::High).is_err(),
    "everyone says who they are"
  );

  let max = assistant::add("Max Two", "Second opinion", "", "", Effort::Max).unwrap();
  assert_eq!(max.id, "maxtwo");
  assert!(
    max.hello().0.starts_with("Hello! This is Max Two.\n\n"),
    "none given: a plain hello"
  );
  let hellos = [Assistant::acorn().hello(), Assistant::max().hello()];
  assert!(hellos[0].0.starts_with("Hello! This is Acorn.") && hellos[1].0.starts_with("Hello! This is Max."));
  assert!(
    hellos[0]
      .1
      .starts_with("unii: hello: Acorn, General assistant: Hello! This is Acorn. One chat between us")
      && hellos[1]
        .1
        .starts_with("unii: hello: Max, Thoughtful expert: Hello! This is Max."),
    "their lines made as a made one's are"
  );
  assert!(hellos[0].1.chars().count() <= 240 + "unii: hello: Acorn, General assistant: ".len());
  assert_ne!(
    hellos[0].0.replace("Acorn", "Max"),
    hellos[1].0,
    "Max greets in its own words"
  );
  assert_eq!(names(assistant::all()), ["Acorn", "Max", "Abc", "Max Two"]);
  assert_eq!(assistant::all()[2], abc, "as saved, read back");
  assert!(Assistant::acorn().personality().is_empty() && Assistant::acorn().work().ends_with("home/work"));
  assert_eq!(
    (Assistant::acorn().about, Assistant::max().about),
    ("General assistant".to_string(), "Thoughtful expert".to_string())
  );
  assert_eq!(Assistant::max().effort, Effort::Max);

  // Archived: off the list, its files kept, its name held; restored: back as it was.
  let chat = dir.path().join("chats/abc-2");
  std::fs::create_dir_all(&chat).unwrap();
  assert!(
    assistant::archive("Acorn", "max").is_err() && assistant::archive("max", "abc-2").is_err(),
    "Acorn and Max stay"
  );
  assert!(assistant::archive("abc", "abc-2").is_err(), "not yourself");
  assert!(assistant::archive("Nobody", "max").is_err());
  let gone = assistant::archive(" ABC ", "max").unwrap();
  assert!(gone.archived && gone.name == "Abc");
  assert_eq!(names(assistant::all()), ["Acorn", "Max", "Max Two"]);
  assert_eq!(names(assistant::archived()), ["Abc"]);
  assert!(chat.is_dir(), "nothing deleted");
  assert!(assistant::archive("abc", "max").is_err(), "archived already");
  assert!(
    assistant::add("Abc", "Another", "", "", Effort::Low)
      .unwrap_err()
      .contains("archived"),
    "its name held"
  );
  let back = assistant::restore("abc").unwrap();
  assert_eq!(back, abc);
  assert_eq!(names(assistant::all()), ["Acorn", "Max", "Abc", "Max Two"]);
  assert!(assistant::archived().is_empty() && assistant::restore("abc").is_err());
}
