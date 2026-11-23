//! Pins
//! ====
//! A pin is a message's id, kept beside the chat (`state`). A call reads
//! the pinned messages whole, however old (`chat::turns::pinned`); the
//! tree summarizes them like any other, so a pin changes no node and no
//! view. A purge unpins its message (`Memory::purge`), for good: taken
//! back, the message is no longer pinned.

use super::purge::{no_message, pages};
use super::{Memory, PURGED, Root, flat};
use crate::clip;

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

  /// The user's messages before message `before` that Acorn has not
  /// replied to yet (a turn's: several, sent in a row or cutting in),
  /// by their first pages; none: the user's latest. Not an imported
  /// chat's.
  fn new_said(&self, before: u64) -> Vec<u64> {
    let ids = |sql: &str| -> Vec<u64> {
      let Ok(mut q) = self.db.prepare(sql) else {
        return Vec::new();
      };
      q.query_map([before.min(self.count)], |r| r.get(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    };
    let user = "log = '' and i < ?1 and kind = 'user' and imported = 0";
    let mut new = ids(&format!(
      "select i from msgs where {user} and i > coalesce((select max(i) from msgs where log = '' and i < ?1 and kind = 'unii'), -1) order by i"
    ));
    let mut end = 0;
    new.retain(|&i| {
      let first = i >= end;
      end = end.max(self.pages_of(None, i).map_or(i, |(_, b)| b) + 1);
      first
    });
    if new.is_empty() {
      new = ids(&format!("select i from msgs where {user} order by i desc limit 1"));
    }
    new
  }

  /// Message `id` pinned (none: the user's new message before `before`,
  /// if it is the only one), if the pinned texts stay within PINS.
  /// Answers what it pinned, else why not. The caller saves.
  pub fn pin(&mut self, id: Option<u64>, before: u64) -> Result<String, String> {
    let id = match (id, self.new_said(before).as_slice()) {
      (Some(id), _) => id,
      (None, &[id]) => id,
      (None, []) => return Err("No message of the user's to pin: give id.".into()),
      (None, new) => {
        let each: Vec<String> = new
          .iter()
          .filter_map(|&i| self.message(i))
          .map(|root| format!("{} (\"{}\")", root.i, head(&root)))
          .collect();
        return Err(format!(
          "Not pinned: the user's new messages are several: {}. Give the id of the one that must hold; pin again for each that must.",
          each.join(", ")
        ));
      }
    };
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

  /// A pin outlives a restart, and goes with its message purged: the
  /// purge taken back, the message is not pinned, so what was pinned in
  /// its place still fits.
  #[test]
  fn a_pin_is_kept_till_its_message_is_purged() {
    let dir = tempfile::tempdir().unwrap();
    let mut mem = Memory::load(dir.path()).unwrap();
    said(&mut mem, Kind::User, "hello");
    let spec = said(&mut mem, Kind::User, &"s".repeat(PINS - 10));
    let other = said(&mut mem, Kind::User, &"o".repeat(PINS - 10));
    mem.pin(Some(spec), u64::MAX).unwrap();
    mem.save().unwrap();
    drop(mem);
    let mut mem = Memory::load(dir.path()).unwrap();
    assert_eq!(ids(&mem), [spec], "kept across a restart");
    mem.purge(None, spec).unwrap();
    assert_eq!(ids(&mem), [0; 0], "purged: unpinned");
    mem.pin(Some(other), u64::MAX).unwrap();
    mem.unpurge(None, spec).unwrap();
    drop(mem);
    let mut mem = Memory::load(dir.path()).unwrap();
    assert_eq!(ids(&mem), [other], "taken back, not pinned again: the pins fit");
    mem.unpin(other).unwrap();
    assert_eq!(ids(&mem), [0; 0]);
    assert!(mem.unpin(other).is_err(), "nothing to unpin");
  }

  /// Without an id, the pin is the user's new message before the one
  /// given (a message still waiting was not read): not Acorn's, and a
  /// long one whole, by its first page, whichever page names it. With
  /// several new, none is guessed: they are named, to pin by id.
  #[test]
  fn no_id_pins_the_users_only_new_message() {
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
    let noted = said(&mut mem, Kind::User, "Just say noted.");
    let why = mem.pin(None, u64::MAX).unwrap_err();
    assert!(
      why.contains(&format!(
        "{waiting} (\"one more thing\"), {noted} (\"Just say noted.\")"
      )),
      "{why}"
    );
    assert_eq!(ids(&mem), [0; 0], "none guessed");
    said(&mut mem, Kind::Tool, "luau {}");
    said(&mut mem, Kind::Acorn, "Noted.");
    mem.pin(None, u64::MAX).unwrap();
    assert_eq!(ids(&mem), [noted], "none new: the user's latest");
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
