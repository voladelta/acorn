//! What a window is told of a chat: a block's halves and the overview's
//! tops again each time they are asked for, as a window may have dropped
//! them since (web/src/model.ts `prune`).

use acorn_core::Kind;
use acorn_core::assistant;
use acorn_core::chat::hub;
use acorn_core::memory::{Msg, Part};
use acorn_core::wire::{Command, Event};

#[tokio::test]
async fn what_is_asked_for_again_is_told_again() {
  let dir = tempfile::tempdir().unwrap();
  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("ACORN_DIR", dir.path().join("home"));
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
    std::env::remove_var("GEMINI_API_KEY");
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
  {
    let mut chat = engine.lock();
    assert_eq!(chat.mem.count, 1);
    chat.mem.push(vec![Msg::new(Kind::User, "a second message")]).unwrap();
    chat
      .mem
      .store(&[
        (Part::leaf(1), "second summary".into()),
        (Part::new(1, 0), "both messages".into()),
      ])
      .unwrap();
  }
  let mut zoom = || {
    while heard.try_recv().is_ok() {}
    engine.command(Command::Zoom((0, 2)));
    let (mut messages, mut summaries, mut opened) = (Vec::new(), Vec::new(), Vec::new());
    while let Ok((_, ev)) = heard.try_recv() {
      match ev {
        Event::Know { whole, nodes } => {
          messages.extend(whole.into_iter().map(|(id, entry)| (id, entry.text)));
          summaries.extend(nodes.into_iter().map(|(seg, node)| (seg, node.text)));
        }
        Event::Zoom { seg, parts } => opened.push((seg, parts)),
        _ => {}
      }
    }
    (messages, summaries, opened)
  };
  for _ in 0..2 {
    let (messages, summaries, opened) = zoom();
    assert_eq!(
      messages,
      [(0, acorn_core::chat::HELLO.into()), (1, "a second message".into())]
    );
    assert_eq!(
      summaries.iter().map(|(seg, _)| *seg).collect::<Vec<_>>(),
      [(0, 1), (1, 1)]
    );
    assert_eq!(summaries[1].1, "second summary");
    assert_eq!(opened, [((0, 2), vec![(0, 1), (1, 1)])]);
  }
  engine.close();
}
