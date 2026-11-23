//! Pins
//! ====
//! A pin is a message's id, kept beside the chat (`state`). A call reads
//! the pinned messages whole, however old (`chat::turns::pinned`); the
//! tree summarizes them like any other, so a pin changes no node and no
//! view. One purged shows nowhere while it is, pinned or not; its purge
//! taken back, it is pinned as before.

use super::purge::{no_message, pages};
use super::{Memory, PURGED, Root, flat};
use crate::clip;
use rusqlite::OptionalExtension;

/// The most bytes of text pinned at once: every call reads them again.
pub const PINS: usize = 32_000;
/// The characters of a pinned message said back, and shown to the user.
const HEAD: usize = 120;

/// How a pinned message begins, on one line.
pub fn head(root: &Root) -> String {
  let text = flat(&root.text);
  match (text.is_empty(), root.images.len()) {
    (true, 0) => String::new(),
    (true, n) => format!("[{n} image{}]", if n == 1 { "" } else { "s" }),
    (false, _) if text.chars().count() > HEAD => format!("{}…", clip::head(&text, HEAD)),
    (false, _) => text,
  }
}

impl Memory {
  /// The message of pages a..=b as one, unless purged: its texts joined,
  /// its images named.
  fn joined(&self, a: u64, b: u64) -> Option<Root> {
    let mut root = self.message(a)?;
    if root.text == PURGED && root.images.is_empty() {
      return None;
    }
    for i in a + 1..=b {
      let page = self.message(i)?;
      root.text.push_str(&page.text);
      root.images.extend(page.images);
    }
    Some(root)
  }

  /// The pinned messages, each whole, oldest first.
  pub fn pinned(&self) -> Vec<Root> {
    let whole = |&id| self.pages_of(None, id).and_then(|(a, b)| self.joined(a, b));
    self.pins.iter().filter_map(whole).collect()
  }

  /// The user's latest message before message `before`, not an imported
  /// chat's.
  fn last_said(&self, before: u64) -> Option<u64> {
    self
      .db
      .query_row(
        "select i from msgs where log = '' and i < ? and kind = 'user' and imported = 0 order by i desc limit 1",
        [before.min(self.count)],
        |r| r.get(0),
      )
      .optional()
      .ok()
      .flatten()
  }

  /// Message `id` pinned (none: the user's latest before `before`), if
  /// the pinned texts stay within PINS. Answers what it pinned, else why
  /// not. The caller saves.
  pub fn pin(&mut self, id: Option<u64>, before: u64) -> Result<String, String> {
    let id = id
      .or_else(|| self.last_said(before))
      .ok_or("No message of the user's to pin: give id.")?;
    let (a, b) = self.pages_of(None, id).ok_or_else(|| no_message(None, id))?;
    let root = self
      .joined(a, b)
      .ok_or_else(|| format!("Message {a} is purged: nothing to pin."))?;
    let said = format!("message {a}{} (\"{}\")", pages(a, b), head(&root));
    let Err(at) = self.pins.binary_search(&a) else {
      return Ok(format!("Already pinned: {said}."));
    };
    let held: usize = self.pinned().iter().map(|r| r.text.len()).sum();
    if held + root.text.len() > PINS {
      return Err(format!(
        "Not pinned: {said} is {} bytes, and {} of the {PINS} that pins hold are left. Unpin what is done, or pin a shorter message.",
        root.text.len(),
        PINS.saturating_sub(held)
      ));
    }
    self.pins.insert(at, a);
    Ok(format!(
      "Pinned {said}: every call reads it whole from now on, till unpin({a})."
    ))
  }

  /// Message `id` no longer pinned. Answers so, else what is pinned. The
  /// caller saves.
  pub fn unpin(&mut self, id: u64) -> Result<String, String> {
    let a = self.pages_of(None, id).map_or(id, |(a, _)| a);
    match self.pins.binary_search(&a) {
      Ok(at) => {
        self.pins.remove(at);
        Ok(format!("Unpinned message {a}: it stays in the chat, summarized."))
      }
      Err(_) if self.pins.is_empty() => Err("Nothing is pinned.".into()),
      Err(_) => {
        let ids: Vec<String> = self.pinned().iter().map(|r| r.i.to_string()).collect();
        Err(format!("Message {id} isn't pinned. Pinned: {}.", ids.join(", ")))
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Kind;
  use crate::memory::Msg;

  fn said(mem: &mut Memory, kind: Kind, text: &str) -> u64 {
    mem.push(vec![Msg::new(kind, text)]).unwrap()[0].i
  }

  fn ids(mem: &Memory) -> Vec<u64> {
    mem.pinned().iter().map(|r| r.i).collect()
  }

  /// A pin outlives a restart. Its message purged, no call reads it;
  /// the purge taken back, it is pinned as it was.
  #[test]
  fn a_pin_is_kept_and_shows_nothing_purged() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    said(&mut mem, Kind::User, "hello");
    let spec = said(&mut mem, Kind::User, "the spec: three endpoints");
    mem.pin(Some(spec), u64::MAX).unwrap();
    mem.save().unwrap();
    drop(mem);
    let mut mem = Memory::load(dir.path()).unwrap();
    assert_eq!(ids(&mem), [spec], "kept across a restart");
    mem.purge(None, spec).unwrap();
    assert_eq!(ids(&mem), [0; 0], "purged: read by no call");
    mem.unpurge(None, spec).unwrap();
    assert_eq!(mem.pinned()[0].text, "the spec: three endpoints", "back, pinned");
    mem.unpin(spec).unwrap();
    assert_eq!(ids(&mem), [0; 0]);
    assert!(mem.unpin(spec).is_err(), "nothing to unpin");
  }

  /// Without an id, the pin is the user's latest message before the one
  /// given (a message still waiting was not read): not Acorn's, and a
  /// long one whole, by its first page, whichever page names it.
  #[test]
  fn no_id_pins_the_users_latest_message_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    said(&mut mem, Kind::User, "hello");
    let long = "y".repeat(clip::CAP + 50);
    let spec = said(&mut mem, Kind::User, &long);
    said(&mut mem, Kind::Acorn, "On it.");
    let waiting = said(&mut mem, Kind::User, "one more thing");
    assert_eq!(waiting, spec + 3, "the spec takes two pages");
    mem.pin(None, waiting).unwrap();
    assert_eq!(ids(&mem), [spec]);
    assert_eq!(mem.pinned()[0].text, long, "its pages as one");
    assert!(mem.pin(Some(spec + 1), u64::MAX).unwrap().starts_with("Already pinned"));
    assert_eq!(mem.pins, [spec], "once, by its first page");
    mem.unpin(spec + 1).unwrap();
    assert_eq!(ids(&mem), [0; 0], "unpinned by any page");
  }

  /// The pinned texts hold PINS bytes at most: one over is refused, and
  /// fits once another is unpinned.
  #[test]
  fn the_pinned_texts_stay_within_their_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    let big = said(&mut mem, Kind::User, &"x".repeat(PINS - 10));
    let small = said(&mut mem, Kind::User, &"y".repeat(20));
    mem.pin(Some(big), u64::MAX).unwrap();
    let why = mem.pin(Some(small), u64::MAX).unwrap_err();
    assert!(why.contains("10 of the 32000"), "{why}");
    assert_eq!(ids(&mem), [big]);
    mem.unpin(big).unwrap();
    mem.pin(Some(small), u64::MAX).unwrap();
    assert_eq!(ids(&mem), [small]);
  }
}
