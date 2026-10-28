//! Clip
//! ====
//! CAP characters: the most text a model gets in one piece (Claude Code
//! keeps a command's output to 30,000 too). Any text kept is never cut, but
//! goes in pages (a message logged, a zoom, a web page). Offsets are in
//! characters, never splitting one.

pub const CAP: usize = 30_000;

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
  fn page_says_where_to_go_on() {
    let text = "a".repeat(CAP + 10);
    assert!(page(&text, 0).ends_with(&format!("go on with at {CAP}]")));
    assert_eq!(page(&text, CAP), "a".repeat(10));
  }
}
