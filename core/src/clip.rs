//! Clip
//! ====
//! CAP characters: the most text a model gets in one piece (Claude Code
//! keeps a command's output to 30,000 too). A command's output is clipped
//! to its head and tail as it streams (`machine`); any text kept is never
//! cut, but goes in pages (a message logged, a zoom, a read). Offsets are
//! in characters, never splitting one.

pub const CAP: usize = 30_000;

/// A text taken in pieces, keeping its first CAP/2 characters and its
/// last ones.
#[derive(Default)]
pub struct Clip {
  head: String,
  head_n: usize,
  tail: String,
  tail_n: usize,
  gone: usize,
}

const HALF: usize = CAP / 2;
const KEEP: usize = HALF - 64;

impl Clip {
  pub fn add(&mut self, s: &str) {
    let mut rest = s;
    if self.head_n < HALF {
      let at = byte_at(rest, HALF - self.head_n);
      let (h, t) = rest.split_at(at);
      self.head.push_str(h);
      self.head_n += h.chars().count();
      rest = t;
    }
    self.tail.push_str(rest);
    self.tail_n += rest.chars().count();
    if self.tail_n > CAP {
      let cut = self.tail_n - KEEP;
      self.tail.drain(..byte_at(&self.tail, cut));
      self.gone += cut;
      self.tail_n = KEEP;
    }
  }

  pub fn text(&self) -> String {
    if self.gone == 0 && self.head_n + self.tail_n <= CAP {
      return format!("{}{}", self.head, self.tail);
    }
    let cut = self.tail_n.saturating_sub(KEEP);
    let tail = &self.tail[byte_at(&self.tail, cut)..];
    format!("{}\n[... {} characters cut ...]\n{}", self.head, self.gone + cut, tail)
  }
}

/// The byte index of character `n` of a text (its end, past it).
pub fn byte_at(s: &str, n: usize) -> usize {
  s.char_indices().nth(n).map_or(s.len(), |(b, _)| b)
}

/// A text's first `n` characters.
pub fn head(s: &str, n: usize) -> &str {
  &s[..byte_at(s, n)]
}

/// Where the page of a text from character `at` ends (a character offset):
/// CAP characters on, after its last newline if it has one.
pub fn page_end(s: &str, at: usize) -> usize {
  let from = byte_at(s, at);
  let rest = &s[from..];
  let cap = byte_at(rest, CAP);
  if cap == rest.len() {
    return s.chars().count();
  }
  rest[..cap]
    .rfind('\n')
    .map_or(at + CAP, |nl| at + rest[..=nl].chars().count())
}

/// A text in pages.
pub fn pages(s: &str) -> Vec<String> {
  let mut out = Vec::new();
  let mut rest = s;
  loop {
    let end = byte_at(rest, page_end(rest, 0));
    out.push(rest[..end].to_string());
    rest = &rest[end..];
    if rest.is_empty() {
      return out;
    }
  }
}

/// A text's page from character `at`, and past it where to go on.
pub fn page(s: &str, at: usize) -> String {
  let n = s.chars().count();
  let at = at.min(n);
  let end = page_end(s, at);
  let text = &s[byte_at(s, at)..byte_at(s, end)];
  if end < n {
    format!("{text}\n[characters {}-{end} of {n}; go on with at {end}]", at + 1)
  } else {
    text.to_string()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pages_split_after_newlines() {
    let line = "x".repeat(99) + "\n";
    let text = line.repeat(700);
    let ps = pages(&text);
    assert_eq!(ps.concat(), text);
    assert!(ps.iter().all(|p| p.chars().count() <= CAP && p.ends_with('\n')));
    assert_eq!(pages("short"), vec!["short"]);
  }

  #[test]
  fn clip_keeps_head_and_tail() {
    let mut c = Clip::default();
    for _ in 0..100 {
      c.add(&"é".repeat(1000));
    }
    let t = c.text();
    assert!(t.contains("characters cut"));
    assert!(t.chars().count() < CAP + 100);
  }

  #[test]
  fn page_says_where_to_go_on() {
    let text = "a".repeat(CAP + 10);
    assert!(page(&text, 0).ends_with(&format!("go on with at {CAP}]")));
    assert_eq!(page(&text, CAP), "a".repeat(10));
  }
}
