//! What a window is told of a chat: a block's halves and the overview's
//! tops again each time they are asked for, as a window may have dropped
//! them since (web/src/model.ts `prune`).

use acorn_core::assistant;
use acorn_core::chat::hub;
use acorn_core::wire::{Command, Event};

#[tokio::test]
async fn what_is_asked_for_again_is_told_again() {
  let dir = tempfile::tempdir().unwrap();
  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("ACORN_DIR", dir.path().join("home"));
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
    std::env::remove_var("DEEPSEEK_API_KEY");
  }
  let (tx, mut heard) = tokio::sync::mpsc::unbounded_channel();
  assert!(hub::listen(tx));
  let engine = hub::chat(&assistant::all()[0]).unwrap();

  // the messages and summaries each command tells
  let mut told = |cmd: Command| {
    while heard.try_recv().is_ok() {}
    engine.command(cmd);
    let mut known = 0;
    while let Ok((_, ev)) = heard.try_recv() {
      if let Event::Know { whole, nodes } = ev {
        known += whole.len() + nodes.len();
      }
    }
    known
  };
  let first = told(Command::Attach);
  assert!(first > 0, "a chat opens with its welcome");
  assert_eq!(
    told(Command::Root),
    first,
    "the overview's tops, though the view told them"
  );
  assert_eq!(told(Command::Root), first, "and again");
  engine.close();
}
