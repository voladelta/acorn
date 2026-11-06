//! Kit
//! ===
//! What every scenario needs: a window, headless, on the real engine and
//! DeepSeek, over dirs of its own (`Live`); words sent and the turn
//! waited out by the chat's own record (`turns`), never by a sleep or by
//! words on the screen; what the turn did, read from the chat's db
//! (`Turn`); and the exchange written down as it goes, the whole log
//! with it if the scenario fails.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a turn may take, tools and agents and all.
pub const TURN: Duration = Duration::from_secs(600);

/// How often a wait looks again (the window runs meanwhile).
const POLL: Duration = Duration::from_millis(500);

/// A window of a scenario's own, on dirs of its own:
/// `home` (Acorn's dir; its work dir `home/work/main`), `chats`, `user`
/// (HOME, so no real skill is found) and `proj` (a project folder).
pub struct Live {
  pub app: Headless,
  root: tempfile::TempDir,
  name: String,
  said: String,
  /// The assistant talked to, whose message box shows.
  talking: String,
}

/// What a turn did after the message it answered (`from`): its replies,
/// the last one, its tool calls (`name {json}`) and their results, in
/// Acorn's own log.
pub struct Turn {
  pub from: i64,
  pub reply: String,
  pub replies: String,
  pub calls: Vec<String>,
  pub echoes: Vec<String>,
}

impl Turn {
  /// Its luau calls.
  pub fn luau(&self) -> Vec<&String> {
    self.calls.iter().filter(|c| c.starts_with("luau {")).collect()
  }

  /// Whether a call of `tool` was made, its input holding `has`.
  pub fn called(&self, tool: &str, has: &str) -> bool {
    let head = format!("{tool} {{");
    self.calls.iter().any(|c| c.starts_with(&head) && c.contains(has))
  }
}

impl Live {
  /// A window opened after `prep` has laid out its dirs, with config set
  /// as `ACORN_<KEY>`; Acorn's hello shown.
  pub fn start(name: &str, config: &[(&str, &str)], prep: impl FnOnce(&Dirs)) -> Live {
    let root = tempfile::tempdir().unwrap();
    let dirs = Dirs(root.path().to_path_buf());
    for d in [dirs.home(), dirs.work(), dirs.user(), dirs.proj()] {
      std::fs::create_dir_all(d).unwrap();
    }
    prep(&dirs);
    // SAFETY: set once, before any thread of the scenario starts (its own process).
    unsafe {
      std::env::set_var("HOME", dirs.user());
      std::env::set_var("ACORN_DIR", dirs.home());
      std::env::set_var("ACORN_CHATS", dirs.chats());
      for (key, value) in config {
        std::env::set_var(format!("ACORN_{}", key.to_uppercase()), value);
      }
    }
    let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
    app.first_render().unwrap();
    app
      .wait_for_within("Hello! This is Acorn.", Duration::from_secs(30))
      .unwrap();
    Live {
      app,
      root,
      name: name.to_string(),
      said: format!("# {name}\n"),
      talking: "Acorn".into(),
    }
  }

  pub fn dirs(&self) -> Dirs {
    Dirs(self.root.path().to_path_buf())
  }

  /// A chat's dir, by id (`default`: Acorn's; `max/with/default`: a thread).
  pub fn chat(&self, id: &str) -> PathBuf {
    self.dirs().chats().join(id)
  }

  /// Words to `who` (chat `id`), its turn waited out: what it did.
  pub fn say(&mut self, who: &str, id: &str, text: &str) -> Turn {
    let from = self.send(who, id, text);
    self.until(&format!("{who}'s turn after: {}", short(text)), |live| {
      turned(&live.chat(id)) >= from
    });
    self.since(id, from)
  }

  /// Words sent, not waited on: the user's message's index.
  pub fn send(&mut self, who: &str, id: &str, text: &str) -> i64 {
    let from = count(&self.chat(id));
    let field = format!("label:Message {who}");
    self.app.type_text(&field, text).unwrap();
    self.app.submit(&field).unwrap();
    self.said.push_str(&format!("\n---\n## To {who}\n\n{text}\n"));
    from
  }

  /// Waited, the window running, till `done` holds (at most `TURN`).
  pub fn until(&mut self, what: &str, done: impl Fn(&Live) -> bool) {
    let started = Instant::now();
    while !done(self) {
      assert!(started.elapsed() < TURN, "never: {what}");
      self.app.wait(POLL).unwrap();
    }
    let secs = started.elapsed().as_secs();
    self.said.push_str(&format!("\n_({what}: {secs} s)_\n"));
  }

  /// What chat `id` logged after message `from`, written down.
  pub fn since(&mut self, id: &str, from: i64) -> Turn {
    let chat = self.chat(id);
    let of = |kind: &str| {
      rows(
        &chat,
        &format!("select text from msgs where log = '' and i > {from} and kind = '{kind}' order by i"),
      )
    };
    let replies = of("unii");
    let turn = Turn {
      from,
      reply: replies.last().cloned().unwrap_or_default(),
      replies: replies.join("\n\n"),
      calls: of("tool"),
      echoes: of("echo"),
    };
    if !turn.calls.is_empty() {
      let calls: Vec<String> = turn.calls.iter().map(|c| short(c)).collect();
      self
        .said
        .push_str(&format!("\n### Tools\n\n```\n{}\n```\n", calls.join("\n")));
    }
    self.said.push_str(&format!("\n### Reply\n\n{}\n", turn.reply));
    turn
  }

  /// Every tool call in a chat's own log, in order.
  pub fn calls(&self, id: &str) -> Vec<String> {
    rows(
      &self.chat(id),
      "select text from msgs where log = '' and kind = 'tool' order by i",
    )
  }

  /// An assistant talked to (`@Name` alone), its chat shown: its welcome.
  /// Not by the sidebar: a name framed in the chat is pressed as well.
  pub fn open(&mut self, name: &str, id: &str) -> String {
    let field = format!("label:Message {}", self.talking);
    self.app.type_text(&field, &format!("@{name}")).unwrap();
    self.app.submit(&field).unwrap();
    self
      .app
      .wait_for_within(&format!("label:Message {name}"), Duration::from_secs(30))
      .unwrap();
    self.talking = name.to_string();
    let hello = one(&self.chat(id), "select text from msgs where log = '' and i = 0");
    self.said.push_str(&format!("\n---\n## Opened {name}\n\n{hello}\n"));
    hello
  }

  /// The sidebar's names, as shown.
  pub fn sidebar(&self) -> Vec<String> {
    let texts: Vec<String> = self.app.screen().texts().iter().map(|t| t.to_string()).collect();
    let at = texts.iter().position(|t| t == "ASSISTANTS").map_or(0, |k| k + 1);
    texts[at..]
      .iter()
      .take_while(|t| !t.contains("  ·  "))
      .filter(|t| t.chars().count() > 1)
      .cloned()
      .collect()
  }

  /// Whether the screen shows a text holding `what`.
  pub fn shows(&self, what: &str) -> bool {
    self.app.screen().texts().iter().any(|t| t.contains(what))
  }

  /// The assistants as saved (assistants.json).
  pub fn assistants(&self) -> Vec<serde_json::Value> {
    std::fs::read_to_string(self.dirs().home().join("assistants.json"))
      .ok()
      .and_then(|t| serde_json::from_str(&t).ok())
      .unwrap_or_default()
  }

  /// The assistant saved as `name`.
  pub fn assistant(&self, name: &str) -> serde_json::Value {
    let all = self.assistants();
    all
      .iter()
      .find(|a| a["name"] == name)
      .cloned()
      .unwrap_or_else(|| panic!("no assistant {name} in {all:?}"))
  }

  /// A note in the transcript.
  pub fn note(&mut self, text: &str) {
    self.said.push_str(&format!("\n_{text}_\n"));
  }

  /// What every chat cost, in USD.
  fn spent(&self) -> f64 {
    chats(&self.dirs().chats())
      .iter()
      .map(|c| {
        one(c, "select coalesce(sum(micros), 0) from uses")
          .parse::<f64>()
          .unwrap_or(0.0)
          / 1e6
      })
      .sum()
  }
}

impl Drop for Live {
  /// The exchange written to `$ACORN_LIVE_OUT/<name>.md`, every chat's
  /// whole log with it if the scenario failed; what it cost, said.
  fn drop(&mut self) {
    let spent = self.spent();
    if std::thread::panicking() {
      for c in chats(&self.dirs().chats()) {
        let log = rows(
          &c,
          "select log || '#' || i || ' ' || kind || ': ' || replace(substr(text, 1, 400), char(10), ' / ') from msgs order by log, i",
        );
        self.said.push_str(&format!(
          "\n---\n## Log of {}\n\n```\n{}\n```\n",
          c.display(),
          log.join("\n")
        ));
      }
    }
    self.said.push_str(&format!("\n---\nSpent: ${spent:.4}\n"));
    let file = out().join(format!("{}.md", self.name));
    let _ = std::fs::write(&file, &self.said);
    println!("transcript: {}", file.display());
    println!("spent: {spent:.4}");
  }
}

/// A scenario's dirs (see `Live`).
#[derive(Clone)]
pub struct Dirs(PathBuf);

impl Dirs {
  pub fn home(&self) -> PathBuf {
    self.0.join("home")
  }
  pub fn work(&self) -> PathBuf {
    self.home().join("work/main")
  }
  pub fn chats(&self) -> PathBuf {
    self.0.join("chats")
  }
  pub fn user(&self) -> PathBuf {
    self.0.join("user")
  }
  pub fn proj(&self) -> PathBuf {
    self.0.join("proj")
  }
}

/// Where transcripts go: $ACORN_LIVE_OUT, else the temp dir's acorn-live.
pub fn out() -> PathBuf {
  let dir = std::env::var_os("ACORN_LIVE_OUT").map_or_else(|| std::env::temp_dir().join("acorn-live"), PathBuf::from);
  let _ = std::fs::create_dir_all(&dir);
  dir
}

/// A file written, its dirs made.
pub fn write(path: &Path, text: &str) {
  std::fs::create_dir_all(path.parent().unwrap()).unwrap();
  std::fs::write(path, text).unwrap();
}

/// A skill in `dir`: `name`/SKILL.md, saying when it applies and its steps.
pub fn skill(dir: &Path, name: &str, when: &str, steps: &str) {
  write(
    &dir.join(name).join("SKILL.md"),
    &format!("---\nname: {name}\ndescription: {when}\n---\n\n{steps}\n"),
  );
}

/// A query on a chat's db, read only: each row, its columns joined by `|`.
pub fn rows(chat: &Path, sql: &str) -> Vec<String> {
  rows_of(&chat.join("chat.db"), sql)
}

/// A query on any db file, read only (as `rows`).
pub fn rows_of(file: &Path, sql: &str) -> Vec<String> {
  let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
  let Ok(db) = Connection::open_with_flags(file, flags) else {
    return Vec::new();
  };
  let Ok(mut q) = db.prepare(sql) else {
    return Vec::new();
  };
  let cols = q.column_count();
  q.query_map([], |row| {
    Ok(
      (0..cols)
        .map(|k| match row.get_ref(k) {
          Ok(ValueRef::Integer(n)) => n.to_string(),
          Ok(ValueRef::Real(x)) => x.to_string(),
          Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
          _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("|"),
    )
  })
  .map(|it| it.flatten().collect())
  .unwrap_or_default()
}

/// A query's first row ("" if none).
pub fn one(chat: &Path, sql: &str) -> String {
  rows(chat, sql).into_iter().next().unwrap_or_default()
}

/// The last message a turn ended on (-1: none yet).
pub fn turned(chat: &Path) -> i64 {
  one(chat, "select coalesce(max(msg), -1) from turns")
    .parse()
    .unwrap_or(-1)
}

/// The messages in a chat's own log.
pub fn count(chat: &Path) -> i64 {
  one(chat, "select count(*) from msgs where log = ''")
    .parse()
    .unwrap_or(0)
}

/// Whether any byte of a chat's db (and its write-ahead log) holds `what`.
pub fn held(chat: &Path, what: &str) -> bool {
  ["chat.db", "chat.db-wal"]
    .iter()
    .filter_map(|f| std::fs::read(chat.join(f)).ok())
    .any(|b| b.windows(what.len()).any(|w| w == what.as_bytes()))
}

/// The number after the last `key=` in `text`, if any.
pub fn number(text: &str, key: &str) -> Option<u64> {
  let at = text.rfind(&format!("{key}="))? + key.len() + 1;
  let digits: String = text[at..].chars().take_while(char::is_ascii_digit).collect();
  digits.parse().ok()
}

/// The word after the last `key=` in `text` (letters, digits, `_-.,`), if
/// any.
pub fn word<'a>(text: &'a str, key: &str) -> Option<&'a str> {
  let at = text.rfind(&format!("{key}="))? + key.len() + 1;
  let rest = &text[at..];
  let end = rest
    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ',')))
    .unwrap_or(rest.len());
  Some(rest[..end].trim_end_matches(['.', ','])).filter(|w| !w.is_empty())
}

/// Every chat dir under `chats` (an assistant's, and the threads in it).
fn chats(dir: &Path) -> Vec<PathBuf> {
  let mut all = Vec::new();
  for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
    let p = e.path();
    if p.join("chat.db").exists() {
      all.push(p.clone());
    }
    if p.is_dir() {
      all.extend(chats(&p));
    }
  }
  all
}

/// A text's first line, cut at 160 characters.
fn short(text: &str) -> String {
  let line = text.lines().next().unwrap_or_default();
  match line.char_indices().nth(160) {
    Some((at, _)) => format!("{}…", &line[..at]),
    None => line.to_string(),
  }
}
