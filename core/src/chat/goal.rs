//! Goal
//! ====
//! The finish line the user named (the `goal` tool), kept till it is met
//! or dropped. As a turn would end (`turns`), a check reads the chat and
//! says whether it is met; unmet, the user's nudge keeps the turn going.

use super::{Chat, Engine, Phase};
use crate::ai;
use crate::memory::{self, Part};
use crate::{Block, Kind, Served, config};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A goal's check: its own system prompt (Acorn's, which says a call is a
/// turn or a compaction, made the model summarize the chat instead of
/// answering), asked after the chat (the view, its latest messages whole)
/// of its goal; unmet, the user's nudge.
const GOAL: &str = "You check a goal. Below is a chat between a user and an AI assistant: its older messages as summary lines (id+n|text), then its latest messages whole, then the goal the assistant set itself from the user's words. Answer yes if the chat shows the goal fully satisfied now, by evidence in it (tool results, files, outputs), not by the assistant's word alone; else no. Write that one word, nothing else.";
const GOAL_ASK: &str = "\nThe goal:\n\n";
const GOAL_NO: &str = "The goal isn't satisfied yet: keep working. Treat the current workspace, tool results and files as authoritative; inspect them instead of assuming earlier narration is still current. Make concrete progress and verify the result. Before saying the goal is done, gather evidence that the whole of it is achieved.";

impl Engine {
  /// A goal check's answer, as the turn's call's is: unmet (any answer
  /// but yes), the user's nudge goes to the turn as their message; met,
  /// the goal ends. A check that fails, a stop, a message (which stops
  /// the check: its answer would be moot) or the goal dropped meanwhile
  /// end it with no nudge.
  pub(super) async fn checked(self: Arc<Self>, call: ai::Call, goal: String, halt: CancellationToken) {
    let end = call.end().await;
    let nudge = {
      let mut chat = self.lock();
      chat.phase = Phase::Settling;
      let on = !halt.is_cancelled() && chat.mem.goal.as_deref() == Some(goal.as_str()) && chat.wait.is_empty();
      if on && !end.ok {
        chat.note(format!(
          "No goal check: {}. The goal stays till it is dropped.",
          end.text.trim_end_matches('.')
        ));
      } else if on && says_yes(&end.text) {
        chat.mem.goal = None;
        chat.save();
        chat.note("Goal met.");
      }
      chat.change();
      on && end.ok && !says_yes(&end.text)
    };
    if nudge {
      let me = self.clone();
      self
        .writing(async move || {
          let mut chat = me.lock();
          let span = chat.push(None, Kind::User, GOAL_NO);
          chat.take(span);
        })
        .await;
    }
  }
}

/// /^\W*yes\b/i
fn says_yes(text: &str) -> bool {
  let rest = text.trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
  rest.get(..3).is_some_and(|w| w.eq_ignore_ascii_case("yes"))
    && !rest[3..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

impl Chat {
  /// The goal, if there is one, asked of the chat's model, without
  /// tools, on the chat as it is now: its check, and the goal checked.
  pub(super) fn check_goal(&mut self) -> Option<(ai::Call, String)> {
    let goal = self.mem.goal.clone()?;
    let ask = ai::Ask {
      system: GOAL.into(),
      blocks: self.goal_blocks(&goal),
      meter: Some(self.meter(Served::Chat)),
      ..self.share()
    };
    let check = ai::ask(ask, Arc::new(|_| {}));
    self.phase = Phase::Checking(check.handle.clone());
    Some((check, goal))
  }

  /// A goal check's prompt: the view, the chat's latest messages whole,
  /// the goal. The whole ones take half the view's bytes at most, and
  /// start where the last check's did while they fit, else at a quarter:
  /// a goal's checks, one after each turn, then begin alike, and DeepSeek
  /// reads what they share from its cache. Those not summarized yet go
  /// whole as a turn's do (`from`): the view has no line of them.
  fn goal_blocks(&mut self, goal: &str) -> Vec<Block> {
    let high = config::get().view;
    let least = self.mem.whole_in(0, high / 2);
    let at = match self.checked {
      Some(at) if at >= least => at,
      _ => self.mem.whole_in(0, high / 4),
    };
    self.checked = Some(at);
    let at = at.min(self.mem.whole(self.mem.first(0, 0)));
    let tail: String = (at..self.mem.count)
      .filter_map(|i| self.mem.message(i))
      .map(|m| {
        format!(
          "{}\n",
          memory::line(Part::leaf(m.i), &format!("{}: {}", m.kind.as_str(), m.text))
        )
      })
      .collect();
    let mut blocks = self.mem.view_blocks(at);
    blocks.push(Block::new(format!("{tail}{GOAL_ASK}{goal}"), true));
    blocks
  }

  /// The goal set, or dropped (None), and told.
  pub(super) fn set_goal(&mut self, goal: Option<String>) {
    if goal != self.mem.goal {
      self.mem.goal = goal;
      self.save();
      self.tell_state();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::memory::Msg;

  #[test]
  fn yes_is_a_word() {
    assert!(says_yes("Yes."));
    assert!(says_yes("**yes**"));
    assert!(!says_yes("yesterday"));
    assert!(!says_yes("no"));
  }

  /// Every summary ready built, a call's line cut from its source.
  fn build(mem: &mut memory::Memory) {
    loop {
      let (mut built, jobs) = mem.starts();
      if built.is_empty() && jobs.is_empty() {
        break;
      }
      for (part, text) in jobs {
        mem.busy.remove(&part.slot());
        built.push((part, memory::cut(&memory::flat(&text)).to_string()));
      }
      mem.store(&built).unwrap();
    }
    mem.fit(false);
  }

  #[tokio::test]
  async fn a_goals_checks_begin_alike_as_the_chat_grows() {
    let dir = tempfile::tempdir().unwrap();
    let engine = crate::chat::tests::open(dir.path());
    let mut chat = engine.lock();
    let high = config::get().view;
    let turn = |chat: &mut Chat, n: u64| {
      for k in 0..n {
        let (kind, len) = if k % 2 == 0 {
          (Kind::User, 200)
        } else {
          (Kind::Acorn, 6000)
        };
        let id = chat.mem.count;
        chat
          .mem
          .push(vec![Msg::new(kind, format!("{id} {}", "w".repeat(len)))])
          .unwrap();
      }
      build(&mut chat.mem);
      let prompt: String = chat.goal_blocks("done").iter().map(|b| b.text.as_str()).collect();
      prompt
    };
    let alike = |a: &str, b: &str| a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    // a chat past the view's bytes: its latest go whole, a quarter of them
    let first = turn(&mut chat, (high / 3100 + 2) as u64);
    let from = chat.checked.unwrap();
    assert!(from > 0);
    let whole = |chat: &Chat, from: u64| {
      (from..chat.mem.count)
        .map(|i| chat.mem.message(i).unwrap().size)
        .sum::<usize>()
    };
    assert!(whole(&chat, from) <= high / 4 && whole(&chat, from - 1) > high / 4);
    assert!(first.len() < high / 2);
    // a turn later, the check starts where the last did: all it had, then more
    let next = turn(&mut chat, 6);
    assert_eq!(chat.checked, Some(from));
    let goal = format!("{GOAL_ASK}done");
    assert_eq!(alike(&first, &next), first.len() - goal.len());
    assert!(next.ends_with(&goal));
    // past half the view's bytes, it starts over at a quarter
    let mut last = next;
    while chat.checked == Some(from) {
      assert!(whole(&chat, from) <= high / 2);
      last = turn(&mut chat, 40);
    }
    let from = chat.checked.unwrap();
    assert!(whole(&chat, from) <= high / 4 && whole(&chat, from - 1) > high / 4);
    assert!(last.len() < high);
    // what no summary is built of yet goes whole, before that too
    let unbuilt = chat.mem.count;
    for _ in 0..(high / 3 / 6000) {
      let id = chat.mem.count;
      chat
        .mem
        .push(vec![Msg::new(Kind::Echo, format!("{id} {}", "w".repeat(6000)))])
        .unwrap();
    }
    let prompt: String = chat.goal_blocks("done").iter().map(|b| b.text.as_str()).collect();
    assert!(chat.checked.unwrap() > unbuilt);
    for id in unbuilt..chat.mem.count {
      assert!(
        prompt.contains(&format!("\n{}|echo: {id} w", Part::leaf(id).name())),
        "{id}"
      );
    }
  }
}
