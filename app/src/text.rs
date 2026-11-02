//! Text
//! ====
//! The little reading a message needs beyond Markdown (which `TextView`
//! draws): a tool's result as a file's lines (a read's as its lines, a
//! unified diff as its file and hunks, any other text as lines named
//! "result", so every result looks as a read does); a summary's text with
//! its marks dropped; agents' names (`[FireAnt]`) as links to their pages;
//! an `@Name` being typed, its names and its completion; and how use,
//! tokens, money and time are said.

use acorn_core::{Spend, names};
use std::fmt::Write as _;

/// A line of a diff: kept (' '), dropped ('-') or added ('+'), its number
/// in the old file if dropped, else in the new one.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
  pub s: char,
  pub n: u64,
  pub x: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Out {
  /// A file's lines, from line n, named k (a read's "lines 4-7 of 15",
  /// else "result").
  File { k: String, n: u64, x: Vec<String> },
  /// A diff of file k, its hunks.
  Diff { k: String, x: Vec<Vec<Line>> },
}

/// A tool's result: a read's lines a File, each unified diff in it a Diff,
/// the text around them a File named "result" (none if blank).
pub fn output(s: &str) -> Vec<Out> {
  let s = s.replace('\r', "");
  if let Some(file) = read_envelope(&s) {
    return vec![file];
  }
  let lines: Vec<&str> = s.split('\n').collect();
  let mut out = Vec::new();
  let (mut i, mut at) = (0, 0);
  while i < lines.len() {
    let (diff, next) = diff_parse(&lines, i);
    if let Some(diff) = diff {
      out.extend(result(&lines[at..i]));
      out.push(diff);
      at = next;
    }
    i = next;
  }
  out.extend(result(&lines[at..]));
  out
}

/// A read's result: `<path>`, `<type>file</type>`, then in `<content>`
/// its lines as "n: text", a blank line and a footer in parentheses, which
/// names the file.
fn read_envelope(s: &str) -> Option<Out> {
  let rest = s.strip_prefix("<path>")?;
  let (_, rest) = rest.split_once("</path>\n<type>file</type>\n<content>\n")?;
  let body = rest.strip_suffix("\n</content>")?;
  let (lines, footer) = body.rsplit_once("\n\n").unwrap_or(("", body));
  let k = footer.strip_prefix('(')?.strip_suffix(')')?.to_string();
  let mut n = None;
  let mut x = Vec::new();
  for l in lines.split('\n').filter(|_| !lines.is_empty()) {
    let (num, text) = l.split_once(": ")?;
    n.get_or_insert(num.parse().ok()?);
    x.push(text.to_string());
  }
  Some(Out::File {
    k,
    n: n.unwrap_or(1),
    x,
  })
}

fn result(lines: &[&str]) -> Option<Out> {
  let from = lines.iter().position(|l| !l.trim().is_empty())?;
  let upto = lines.iter().rposition(|l| !l.trim().is_empty())?;
  Some(Out::File {
    k: "result".into(),
    n: 1,
    x: lines[from..=upto].iter().map(ToString::to_string).collect(),
  })
}

/// None: no diff starts at line i (its "---" and "+++" lines, then hunks,
/// each exactly as long as its header counts); and the line after it.
fn diff_parse(lines: &[&str], i: usize) -> (Option<Out>, usize) {
  let file = lines.get(i + 1).and_then(|l| l.strip_prefix("+++ "));
  let (Some(file), true) = (file, lines[i].starts_with("--- ")) else {
    return (None, i + 1);
  };
  let mut j = i + 2;
  let mut hunks = Vec::new();
  while let Some(h) = lines.get(j).and_then(|l| hunk_head(l)) {
    match hunk_parse(lines, j + 1, h) {
      Some((hunk, next)) => {
        hunks.push(hunk);
        j = next;
      }
      None => return (None, i + 1),
    }
  }
  if hunks.is_empty() {
    (None, i + 1)
  } else {
    (
      Some(Out::Diff {
        k: file.to_string(),
        x: hunks,
      }),
      j,
    )
  }
}

/// "@@ -o,olds +n,news @@"
fn hunk_head(l: &str) -> Option<(u64, u64, u64, u64)> {
  let rest = l.strip_prefix("@@ -")?;
  let (old, rest) = rest.split_once(" +")?;
  let (new, _) = rest.split_once(" @@")?;
  let pair = |s: &str| -> Option<(u64, u64)> {
    match s.split_once(',') {
      Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
      None => Some((s.parse().ok()?, 1)),
    }
  };
  let ((o, olds), (n, news)) = (pair(old)?, pair(new)?);
  Some((o, olds, n, news))
}

fn hunk_parse(
  lines: &[&str],
  mut j: usize,
  (mut o, mut olds, mut n, mut news): (u64, u64, u64, u64),
) -> Option<(Vec<Line>, usize)> {
  let mut out = Vec::new();
  while olds > 0 || news > 0 {
    let line = lines.get(j).copied().unwrap_or_default();
    let s = line.chars().next();
    let x = line.get(s.map_or(0, char::len_utf8)..).unwrap_or_default().to_string();
    match s {
      Some(' ') if olds > 0 && news > 0 => {
        out.push(Line { s: ' ', n, x });
        n += 1;
        o += 1;
        olds -= 1;
        news -= 1;
      }
      Some('-') if olds > 0 => {
        out.push(Line { s: '-', n: o, x });
        o += 1;
        olds -= 1;
      }
      Some('+') if news > 0 => {
        out.push(Line { s: '+', n, x });
        n += 1;
        news -= 1;
      }
      Some('\\') => {}
      _ => return None,
    }
    j += 1;
  }
  Some((out, j))
}

/// A text with its marks dropped (a summary is plain text): code, bold,
/// italics, links.
pub fn flat(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut rest = s;
  while let Some(c) = rest.chars().next() {
    let next = &rest[c.len_utf8()..];
    if c == '['
      && let Some(close) = next.find("](")
      && let Some(end) = next[close + 2..].find(')')
      && !next[..close].contains('\n')
    {
      out.push_str(&next[..close]);
      rest = &next[close + 3 + end..];
      continue;
    }
    if c == '`' || (c == '*' && (next.starts_with('*') || next.starts_with(|c: char| !c.is_whitespace()))) {
      rest = next.strip_prefix('*').unwrap_or(next);
      continue;
    }
    out.push(c);
    rest = next;
  }
  out
}

/// Markdown with each agent's name (`[FireAnt]`, not a link already) a
/// link to its page: `agent:FireAnt`.
pub fn link_names(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut rest = s;
  while let Some(at) = rest.find('[') {
    out.push_str(&rest[..at]);
    let tail = &rest[at + 1..];
    match tail.find(']') {
      Some(close) if names::is_name(&tail[..close]) && !tail[close + 1..].starts_with('(') => {
        let name = &tail[..close];
        let _ = write!(out, "[{name}](agent:{name})");
        rest = &tail[close + 1..];
      }
      _ => {
        out.push('[');
        rest = tail;
      }
    }
  }
  out.push_str(rest);
  out
}

// Use
// ===
// What a turn used shows under its last message, quiet: its cost, tokens
// in (and how many came from the cache), out, its requests if more than
// one, and when it ended.

pub fn use_text(u: &Spend, now: i64) -> String {
  let all = u.input + u.read;
  let cached = if u.read > 0 && all > 0 {
    format!(" ({}% cached)", (100.0 * u.read as f64 / all as f64).round())
  } else {
    String::new()
  };
  let mut parts = vec![
    usd_show(u.usd),
    format!("{} in{cached}", tokens_show(all)),
    format!("{} out", tokens_show(u.output)),
  ];
  if u.n > 1 {
    parts.push(format!("{} calls", u.n));
  }
  parts.push(ago_show(u.at, now));
  parts.join(" · ")
}

/// The exact numbers and date of a use.
pub fn use_hint(u: &Spend) -> String {
  let req = if u.n == 1 {
    "1 request".into()
  } else {
    format!("{} requests", grouped(u.n))
  };
  format!(
    "{}\n{req}: {} tokens in ({} cached), {} out, ${:.6}",
    clock_full(u.at),
    grouped(u.input + u.read),
    grouped(u.read),
    grouped(u.output),
    u.usd
  )
}

/// 950, 18.2k, 1.4M
pub fn tokens_show(n: u64) -> String {
  match n {
    0..1000 => n.to_string(),
    1000..10_000 => format!("{:.1}k", n as f64 / 1e3),
    10_000..1_000_000 => format!("{:.0}k", n as f64 / 1e3),
    _ => format!("{:.1}M", n as f64 / 1e6),
  }
}

/// A cost, to two figures at least: $0.0031, $0.042, $1.27.
pub fn usd_show(usd: f64) -> String {
  match usd {
    0.0 => "$0".into(),
    u if u < 0.01 => format!("${u:.4}"),
    u if u < 1.0 => format!("${u:.3}"),
    u => format!("${u:.2}"),
  }
}

pub fn grouped(n: u64) -> String {
  let digits = n.to_string();
  let mut out = String::new();
  for (k, c) in digits.chars().enumerate() {
    if k > 0 && (digits.len() - k).is_multiple_of(3) {
      out.push(',');
    }
    out.push(c);
  }
  out
}

/// Milliseconds since 1970.
pub fn now_ms() -> i64 {
  nsh_core::time::unix_ms() as i64
}

fn local(at: i64) -> chrono::DateTime<chrono::FixedOffset> {
  let utc = chrono::DateTime::from_timestamp_millis(at).unwrap_or_default();
  utc.with_timezone(&offset(utc))
}

/// Local time's offset from UTC at a time: the system's.
#[cfg(not(target_arch = "wasm32"))]
fn offset(at: chrono::DateTime<chrono::Utc>) -> chrono::FixedOffset {
  use chrono::Offset;
  at.with_timezone(&chrono::Local).offset().fix()
}

/// Local time's offset from UTC at a time: the browser's (nsh's page
/// runtime answers it, daylight saving time included).
#[cfg(target_arch = "wasm32")]
fn offset(at: chrono::DateTime<chrono::Utc>) -> chrono::FixedOffset {
  #[link(wasm_import_module = "nsh")]
  unsafe extern "C" {
    #[link_name = "local_offset"]
    fn local_offset(ms: f64) -> f64;
  }
  // SAFETY: a pure function of the page's.
  let east = unsafe { local_offset(at.timestamp_millis() as f64) };
  chrono::FixedOffset::east_opt(if east.is_finite() { east as i32 } else { 0 }).unwrap_or(chrono::FixedOffset::east_opt(0).unwrap())
}

fn clock_full(at: i64) -> String {
  local(at).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 14:03 (today), else Oct 25 14:03.
pub fn clock_show(at: i64) -> String {
  let d = local(at);
  if d.date_naive() == local(now_ms()).date_naive() {
    d.format("%H:%M").to_string()
  } else {
    d.format("%b %-d %H:%M").to_string()
  }
}

/// just now, 3m ago, 2h ago, else the clock.
pub fn ago_show(at: i64, now: i64) -> String {
  let s = ((now - at) as f64 / 1000.0).max(0.0);
  match s {
    s if s < 60.0 => "just now".into(),
    s if s < 3600.0 => format!("{}m ago", (s / 60.0).floor()),
    s if s < 6.0 * 3600.0 => format!("{}h ago", (s / 3600.0).floor()),
    _ => clock_show(at),
  }
}

/// A file URI's path, its %XX escapes read.
pub fn percent_decoded(s: &str) -> String {
  let mut out = Vec::with_capacity(s.len());
  let b = s.as_bytes();
  let mut k = 0;
  while k < b.len() {
    match (b[k], s.get(k + 1..k + 3).and_then(|h| u8::from_str_radix(h, 16).ok())) {
      (b'%', Some(v)) => {
        out.push(v);
        k += 3;
      }
      (c, _) => {
        out.push(c);
        k += 1;
      }
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

/// A reply a message cut in on: its text without the mark, and whether
/// it was cut.
pub fn cut_off(s: &str) -> (&str, bool) {
  s.strip_suffix("[cut off]")
    .map_or((s, false), |said| (said.trim_end(), true))
}

/// The `@` word the caret ends (at `caret`, in UTF-16 units, as inputs
/// count): its byte start and what follows the `@` (`"ask @Ma|"` → (4,
/// "Ma")). A word starts the text or follows whitespace.
pub fn mention_at(s: &str, caret: u32) -> Option<(usize, &str)> {
  let typed = &s[..utf16_byte(s, caret)];
  let start = typed
    .char_indices()
    .rev()
    .find(|(_, c)| c.is_whitespace())
    .map_or(0, |(i, c)| i + c.len_utf8());
  typed[start..].strip_prefix('@').map(|w| (start, w))
}

/// The names an `@` word may mean: those it starts first, then those it
/// is in, ignoring case.
pub fn names_for<'a>(word: &str, names: &'a [String]) -> Vec<&'a str> {
  let word = word.to_lowercase();
  let (mut starts, mut has): (Vec<&str>, Vec<&str>) = (vec![], vec![]);
  for name in names {
    let low = name.to_lowercase();
    if low.starts_with(&word) {
      starts.push(name);
    } else if low.contains(&word) {
      has.push(name);
    }
  }
  starts.extend(has);
  starts
}

/// The text with the `@` word from `start` to the caret made `@name`, a
/// space after it, and the caret after that (UTF-16).
pub fn mention_done(s: &str, start: usize, caret: u32, name: &str) -> (String, u32) {
  let end = utf16_byte(s, caret);
  let space = if s[end..].starts_with(char::is_whitespace) {
    ""
  } else {
    " "
  };
  let head = format!("{}@{name}{space}", &s[..start]);
  let at = head.encode_utf16().count() as u32 + u32::from(space.is_empty());
  (format!("{head}{}", &s[end..]), at)
}

/// The name a message is alone (`"@Max"`), ignoring case.
pub fn mention_alone<'a>(s: &str, names: &'a [String]) -> Option<&'a str> {
  let word = s.trim().strip_prefix('@')?.trim().to_lowercase();
  names.iter().find(|n| n.to_lowercase() == word).map(String::as_str)
}

/// A byte offset of `s` from a UTF-16 one, at a char's start.
fn utf16_byte(s: &str, units: u32) -> usize {
  let mut n = 0;
  for (i, c) in s.char_indices() {
    if n >= units as usize {
      return i;
    }
    n += c.len_utf16();
  }
  s.len()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn results_read_as_files() {
    let read = output(
      "<path>/a/f</path>\n<type>file</type>\n<content>\n4: a\n5: b: c\n\n(Showing lines 4-5 of 9. Use offset=6 to continue.)\n</content>",
    );
    assert_eq!(
      read,
      vec![Out::File {
        k: "Showing lines 4-5 of 9. Use offset=6 to continue.".into(),
        n: 4,
        x: vec!["a".into(), "b: c".into()]
      }]
    );
    let empty = output("<path>/a/f</path>\n<type>file</type>\n<content>\n(End of file - total 0 lines)\n</content>");
    assert!(matches!(&empty[0], Out::File { x, .. } if x.is_empty()));
    let diff = output("--- f\n+++ f\n@@ -1,2 +1,2 @@\n a\n-b\n+B");
    assert!(matches!(&diff[0], Out::Diff { k, x } if k == "f" && x[0].len() == 3));
    assert_eq!(output("ok\n[exit code: 1]").len(), 1);
    assert_eq!(flat("**bold** and `code` [x](http://y)"), "bold and code x");
    assert_eq!(
      link_names("ask [RedAnt] and [x](y)"),
      "ask [RedAnt](agent:RedAnt) and [x](y)"
    );
    assert_eq!(tokens_show(18_200), "18k");
    assert_eq!(usd_show(0.0031), "$0.0031");
  }

  #[test]
  fn an_at_word_names_an_assistant() {
    let names: Vec<String> = ["Acorn", "Max", "Marrie"].map(String::from).into();
    assert_eq!(mention_at("ask @Ma", 7), Some((4, "Ma")));
    assert_eq!(mention_at("ask @Ma now", 11), None);
    assert_eq!(mention_at("mail a@b", 8), None);
    assert_eq!(mention_at("é @", 3), Some((3, "")));
    assert_eq!(names_for("ma", &names), ["Max", "Marrie"]);
    assert_eq!(names_for("rr", &names), ["Marrie"]);
    assert_eq!(mention_done("ask @ma", 4, 7, "Max"), ("ask @Max ".into(), 9));
    assert_eq!(mention_done("ask @ma why", 4, 7, "Max"), ("ask @Max why".into(), 9));
    assert_eq!(mention_alone(" @max ", &names), Some("Max"));
    assert_eq!(mention_alone("@Max why", &names), None);
  }
}
