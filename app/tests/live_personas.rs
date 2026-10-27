//! People using Acorn, live: the window, headless, on the real engine and
//! DeepSeek, each over chats of their own. A developer builds and changes
//! a small package; a knowledge worker turns notes into tasks, has a fact
//! kept and hires an editor who stays; a student learns and asks Max; a
//! teacher makes a quiz writer, lets them go and brings them back; a
//! researcher searches and fetches the web. Then a second round: an
//! analyst hires agents over a CSV; a careless user pastes a key and has it
//! purged; an impatient one cuts in; a maintainer sets a goal till the
//! tests pass; a job seeker has Acorn ask Max; a pet owner is recalled
//! from deep in a small memory; a Vietnamese speaker chats and hires in
//! Vietnamese. Each checks what can be
//! checked (files, facts, the sidebar), and writes the whole exchange to
//! $ACORN_PERSONAS (default: the system's temp dir) to be read.
//!
//! One a process (the engines are one a process): run each alone, as
//! `cargo test -p acorn --test live_personas developer -- --ignored`.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How long a turn may take, tools and agents and all.
const TURN: Duration = Duration::from_secs(600);

struct Person {
  app: Headless,
  dir: tempfile::TempDir,
  name: &'static str,
  said: String,
}

impl Person {
  /// A window of their own, on a dir of their own, Acorn's hello shown.
  fn new(name: &'static str) -> Person {
    Person::with(name, &[], &[])
  }

  /// One with config set (`ACORN_<KEY>`) and files in Acorn's own work
  /// dir (work/main, where its tools start) before it opens.
  fn with(name: &'static str, config: &[(&str, &str)], files: &[(&str, &str)]) -> Person {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: set once, before any thread of the test starts.
    unsafe {
      std::env::set_var("ACORN_DIR", dir.path().join("home"));
      std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
      for (key, value) in config {
        std::env::set_var(format!("ACORN_{}", key.to_uppercase()), value);
      }
    }
    for (path, text) in files {
      let file = dir.path().join("home/work/main").join(path);
      std::fs::create_dir_all(file.parent().unwrap()).unwrap();
      std::fs::write(file, text).unwrap();
    }
    let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
    app.first_render().unwrap();
    app
      .wait_for_within("Hello! This is Acorn.", Duration::from_secs(20))
      .unwrap();
    Person {
      app,
      dir,
      name,
      said: format!("# {name}\n"),
    }
  }

  fn home(&self) -> PathBuf {
    self.dir.path().join("home")
  }

  /// Acorn's own work dir, where its tools start.
  fn work(&self) -> PathBuf {
    self.home().join("work/main")
  }

  fn chat(&self, id: &str) -> PathBuf {
    self.dir.path().join("chats").join(id)
  }

  /// An assistant picked in the sidebar, its chat shown; its welcome, read.
  fn open(&mut self, name: &str, id: &str) -> String {
    self.app.press(&format!("text:{name}")).unwrap();
    self
      .app
      .wait_for_within(&format!("label:Message {name}"), Duration::from_secs(20))
      .unwrap();
    let hello = db(&self.chat(id), "select text from msgs where log = '' and i = 0");
    self
      .said
      .push_str(&format!("\n---\n## Opened {name}; its welcome\n\n{hello}\n"));
    hello
  }

  /// Words to `name` (chat `id`), its turn waited out: its last reply.
  fn say(&mut self, name: &str, id: &str, text: &str) -> String {
    let from = self.send(name, id, text);
    self.until(id, &format!("{name}'s turn after: {text}"), |chat| turned(chat) >= from);
    self.since(name, id, from)
  }

  /// Words sent, not waited on: the user's message's index.
  fn send(&mut self, name: &str, id: &str, text: &str) -> i64 {
    let from = count(&self.chat(id));
    self.app.type_text(&format!("label:Message {name}"), text).unwrap();
    self.app.submit(&format!("label:Message {name}")).unwrap();
    self.said.push_str(&format!("\n---\n## To {name}\n\n{text}\n"));
    from
  }

  /// Waited, the window running, till `done` holds of chat `id`.
  fn until(&mut self, id: &str, what: &str, done: impl Fn(&Path) -> bool) {
    let chat = self.chat(id);
    let started = Instant::now();
    while !done(&chat) {
      assert!(started.elapsed() < TURN, "never: {what}");
      self.app.wait(Duration::from_secs(2)).unwrap();
    }
    self
      .said
      .push_str(&format!("\n_({what}: {} s)_\n", started.elapsed().as_secs()));
  }

  /// What chat `id` logged after message `from`: its tool calls, work
  /// that came in, its last reply.
  fn since(&mut self, name: &str, id: &str, from: i64) -> String {
    let chat = self.chat(id);
    let tools = db(
      &chat,
      &format!(
        "select group_concat(kind || ': ' || substr(replace(text, char(10), ' '), 1, 160), char(10)) from msgs where log = '' and i > {from} and kind in ('tool', 'work', 'note')"
      ),
    );
    let reply = db(
      &chat,
      &format!("select text from msgs where log = '' and i > {from} and kind = 'unii' order by i desc limit 1"),
    );
    let tools = if tools.is_empty() {
      String::new()
    } else {
      format!("\n### Tools\n\n```\n{tools}\n```\n")
    };
    self.said.push_str(&format!("{tools}\n### {name}\n\n{reply}\n"));
    reply
  }

  /// The sidebar's names, as shown.
  fn sidebar(&self) -> Vec<String> {
    let texts: Vec<String> = self.app.screen().texts().iter().map(|t| t.to_string()).collect();
    let at = texts.iter().position(|t| t == "ASSISTANTS").unwrap_or(0);
    texts[at + 1..]
      .iter()
      .take_while(|t| !t.contains("  ·  "))
      .filter(|t| t.chars().count() > 1)
      .cloned()
      .collect()
  }
}

impl Drop for Person {
  /// The exchange, written, with what the day cost.
  fn drop(&mut self) {
    let out = std::env::var_os("ACORN_PERSONAS").map_or_else(std::env::temp_dir, PathBuf::from);
    let _ = std::fs::create_dir_all(&out);
    let spent: f64 = std::fs::read_dir(self.dir.path().join("chats"))
      .into_iter()
      .flatten()
      .flatten()
      .map(|c| {
        db(&c.path(), "select coalesce(sum(micros), 0) from uses")
          .parse::<f64>()
          .unwrap_or(0.0)
          / 1e6
      })
      .sum();
    self.said.push_str(&format!("\n---\nSpent: ${spent:.4}\n"));
    if std::env::var_os("ACORN_PERSONAS_DUMP").is_some() {
      for c in std::fs::read_dir(self.dir.path().join("chats"))
        .into_iter()
        .flatten()
        .flatten()
      {
        let rows = db(
          &c.path(),
          "select log, i, kind, size, substr(replace(text, char(10), ' '), 1, 100), '  LINE:', substr(line, 1, 100) from msgs order by log, i; select '--turns', * from turns; select '--uses', * from uses;",
        );
        self
          .said
          .push_str(&format!("\n## {}\n```\n{rows}\n```\n", c.path().display()));
      }
    }
    let file = out.join(format!("{}.md", self.name));
    let _ = std::fs::write(&file, &self.said);
    eprintln!("transcript: {}", file.display());
  }
}

/// The last message a turn ended on (-1: none yet).
fn turned(chat: &Path) -> i64 {
  db(chat, "select coalesce(max(msg), -1) from turns")
    .parse()
    .unwrap_or(-1)
}

/// The messages in a chat's own log.
fn count(chat: &Path) -> i64 {
  db(chat, "select count(*) from msgs where log = ''")
    .parse()
    .unwrap_or(0)
}

/// A query on a chat's db, read only, its rows as lines.
fn db(chat: &Path, sql: &str) -> String {
  let out = Command::new("sqlite3")
    .arg("-readonly")
    .arg(chat.join("chat.db"))
    .arg(sql)
    .output();
  out
    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    .unwrap_or_default()
}

/// A file named `name` under `dir`, at any depth.
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
  std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
    let p = e.path();
    if p.file_name().is_some_and(|n| n == name) {
      Some(p)
    } else if p.is_dir() {
      find(&p, name)
    } else {
      None
    }
  })
}

fn assistants(home: &Path) -> serde_json::Value {
  serde_json::from_str(&std::fs::read_to_string(home.join("assistants.json")).unwrap_or_else(|_| "[]".into())).unwrap()
}

fn assistant<'a>(all: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
  all
    .as_array()
    .unwrap()
    .iter()
    .find(|a| a["name"] == name)
    .unwrap_or_else(|| panic!("no {name} in {all}"))
}

/// Builds a package in its work dir, tests it, changes it, and remembers it.
#[test]
#[ignore]
fn developer() {
  let mut p = Person::new("developer");
  p.say("Acorn", "default", "In your work dir, make a folder slugify_lite with slugify.py: a slugify(text) function that lowercases, keeps ASCII letters and digits, and joins words with single hyphens. Add test_slugify.py with unittest tests (standard library only). Run them and tell me the result.");
  let tests = find(&p.home().join("work"), "test_slugify.py").expect("test_slugify.py written in the work dir");
  let pkg = tests.parent().unwrap().to_path_buf();
  let run = |pkg: &Path| {
    Command::new("python3")
      .args(["-m", "unittest", "-q"])
      .current_dir(pkg)
      .output()
      .unwrap()
  };
  assert!(
    run(&pkg).status.success(),
    "its tests pass: {}",
    String::from_utf8_lossy(&run(&pkg).stderr)
  );

  p.say(
    "Acorn",
    "default",
    "Now make it strip accents too, so \"Café Déjà Vu!\" becomes cafe-deja-vu. Add a test for it, run the tests, and show me only what you changed.",
  );
  assert!(run(&pkg).status.success(), "its tests still pass");
  let slug = Command::new("python3")
    .args(["-c", "from slugify import slugify; print(slugify('Café Déjà Vu!'))"])
    .current_dir(&pkg)
    .output()
    .unwrap();
  assert_eq!(String::from_utf8_lossy(&slug.stdout).trim(), "cafe-deja-vu");

  let recall = p.say(
    "Acorn",
    "default",
    "Use no tools. In one line: what function did you write for me today, and which file holds its tests?",
  );
  assert!(
    recall.contains("slugify") && recall.contains("test_slugify"),
    "remembers: {recall}"
  );
}

/// Notes into tasks, a fact kept and used, an editor hired who stays.
#[test]
#[ignore]
fn knowledge_worker() {
  let mut p = Person::new("knowledge_worker");
  let table = p.say("Acorn", "default", "Notes from today's ops sync:\n- Dana will send the vendor shortlist by Wednesday\n- Omar to fix the onboarding doc's broken links, end of week\n- I need to book the Q3 offsite venue before the 15th\n- budget review pushed to next month\n\nTurn these into a table: owner, task, due.");
  assert!(
    table.contains('|') && table.contains("Dana") && table.contains("Omar"),
    "a table of who does what: {table}"
  );

  p.say(
    "Acorn",
    "default",
    "Remember that my manager is Priya Raman and I send her an update every Friday.",
  );
  let facts = Command::new("sqlite3")
    .arg(p.home().join("facts.db"))
    .arg("select * from facts")
    .output()
    .unwrap();
  assert!(
    String::from_utf8_lossy(&facts.stdout).contains("Priya"),
    "the fact kept"
  );

  let email = p.say(
    "Acorn",
    "default",
    "Draft this Friday's update email to my manager, from the action items above. Short.",
  );
  assert!(
    email.contains("Priya") && email.contains("Dana"),
    "the fact and the notes used: {email}"
  );

  p.say(
    "Acorn",
    "default",
    "Hire an editor called Edith, to stay with me: she tightens my writing into plain English and explains each cut.",
  );
  let all = assistants(&p.home());
  let edith = assistant(&all, "Edith");
  assert!(
    !edith["welcome"].as_str().unwrap_or_default().trim().is_empty(),
    "made with a welcome: {edith}"
  );
  let id = edith["id"].as_str().unwrap().to_string();
  let hello = p.open("Edith", &id);
  assert_eq!(
    hello.trim(),
    edith["welcome"].as_str().unwrap().trim(),
    "her chat opens with her welcome"
  );
  let tight = p.say("Edith", &id, "We are currently in the process of evaluating the potential options going forward with regard to the vendor situation.");
  assert!(!tight.is_empty());
}

/// Learns, asks Max directly, and is quizzed on what was covered.
#[test]
#[ignore]
fn student() {
  let mut p = Person::new("student");
  let d = p.say(
    "Acorn",
    "default",
    "I'm studying for a calculus test. Explain the derivative of x^3 simply, with one worked example.",
  );
  assert!(d.contains("3x") || d.contains("3 x"), "the derivative: {d}");

  let hello = p.open("Max", "max");
  assert!(
    hello.starts_with("Hello! This is Max.") && hello.contains("when it matters"),
    "Max's own welcome: {hello}"
  );
  let max = p.say(
    "Max",
    "max",
    "My friend says 0.999... repeating is less than 1. Who's right? Settle it.",
  );
  assert!(max.contains('1'), "{max}");

  p.open("Acorn", "default");
  let quiz = p.say(
    "Acorn",
    "default",
    "Quiz me: 3 short questions on what we covered today. No answers yet.",
  );
  assert!(
    ["deriv", "x³", "x^3", "power rule"]
      .iter()
      .any(|w| quiz.to_lowercase().contains(w)),
    "on what was covered: {quiz}"
  );
  let mark = p.say("Acorn", "default", "1) 6x^5 2) 0 3) I don't know");
  assert!(!mark.is_empty());
}

/// Makes a quiz writer, works with them, lets them go, brings them back.
#[test]
#[ignore]
fn teacher() {
  let mut p = Person::new("teacher");
  p.say(
    "Acorn",
    "default",
    "Create an assistant called Quiz Master for my 8th-grade science class: writes short quizzes with answer keys, friendly and age-appropriate.",
  );
  let all = assistants(&p.home());
  let qm = assistant(&all, "Quiz Master");
  assert!(
    !qm["welcome"].as_str().unwrap_or_default().trim().is_empty(),
    "made with a welcome: {qm}"
  );
  let id = qm["id"].as_str().unwrap().to_string();
  assert!(
    p.sidebar().contains(&"Quiz Master".to_string()),
    "in the sidebar: {:?}",
    p.sidebar()
  );

  p.open("Quiz Master", &id);
  let quiz = p.say(
    "Quiz Master",
    &id,
    "3-question multiple choice quiz on photosynthesis, with an answer key at the end.",
  );
  assert!(quiz.to_lowercase().contains("answer"), "{quiz}");

  p.open("Acorn", "default");
  p.say(
    "Acorn",
    "default",
    "Write a short note to parents: Friday's quiz is on photosynthesis, and how they can help at home.",
  );
  p.say("Acorn", "default", "I don't need Quiz Master anymore, let them go.");
  assert_eq!(
    assistant(&assistants(&p.home()), "Quiz Master")["archived"],
    true,
    "archived, not deleted"
  );
  assert!(p.chat(&id).is_dir(), "their chat kept");
  p.app.wait(Duration::from_secs(1)).unwrap();
  assert!(
    !p.sidebar().contains(&"Quiz Master".to_string()),
    "off the sidebar: {:?}",
    p.sidebar()
  );

  let who = p.say("Acorn", "default", "Who did I have write my quizzes before? One line.");
  assert!(who.contains("Quiz Master"), "still known: {who}");
  p.say("Acorn", "default", "Actually, bring Quiz Master back.");
  assert!(
    assistant(&assistants(&p.home()), "Quiz Master")
      .get("archived")
      .is_none_or(|a| a == false),
    "restored"
  );
  p.app.wait(Duration::from_secs(1)).unwrap();
  assert!(
    p.sidebar().contains(&"Quiz Master".to_string()),
    "back on the sidebar: {:?}",
    p.sidebar()
  );
  let hello = p.open("Quiz Master", &id);
  assert!(!hello.is_empty());
  let again = p.say(
    "Quiz Master",
    &id,
    "Use no tools. What topic was the last quiz you wrote me?",
  );
  assert!(
    again.to_lowercase().contains("photosynth"),
    "remembers, as it was: {again}"
  );
}

/// Searches and fetches the web, citing where it read.
#[test]
#[ignore]
fn researcher() {
  let mut p = Person::new("researcher");
  let v = p.say(
    "Acorn",
    "default",
    "Search the web: what is the latest stable release of Python, and when was it released? Cite the URL you read.",
  );
  assert!(v.contains("3.") && v.contains("http"), "a version, cited: {v}");
  let h = p.say(
    "Acorn",
    "default",
    "Fetch https://www.rust-lang.org/ and tell me its main headline, word for word.",
  );
  assert!(
    h.to_lowercase().contains("reliable")
      || h.to_lowercase().contains("efficient")
      || h.to_lowercase().contains("empowering"),
    "{h}"
  );
  let sum = p.say(
    "Acorn",
    "default",
    "Compare the two languages' latest releases in a 3-row table, from what you found. No more searching.",
  );
  assert!(sum.contains('|'));
}

/// Whether a text is Vietnamese: some of its letters only Vietnamese has.
fn vietnamese(text: &str) -> bool {
  text
    .chars()
    .filter(|c| "ăâđêôơưạảấầẩẫậắằẳẵặẹẻẽếềểễệỉịọỏốồổỗộớờởỡợụủứừửữựỳỵỷỹ".contains(c.to_lowercase().next().unwrap_or(' ')))
    .count()
    >= 3
}

/// The messages of `kind` in a chat's own log, past `from`.
fn logged_since(chat: &Path, kind: &str, from: i64) -> Vec<i64> {
  db(
    chat,
    &format!("select i from msgs where log = '' and kind = '{kind}' and i > {from}"),
  )
  .lines()
  .filter_map(|l| l.parse().ok())
  .collect()
}

const SALES: &str = "region,product,units,price
North,Widget,10,2.50
North,Gadget,4,10.00
South,Widget,6,2.50
South,Gizmo,2,30.00
East,Gadget,7,10.00
East,Widget,12,2.50
West,Gizmo,1,30.00
";

/// Hires two agents over a CSV in parallel, waits for both reports, and
/// has the totals saved.
#[test]
#[ignore]
fn analyst() {
  let mut p = Person::with("analyst", &[], &[("sales.csv", SALES)]);
  let from = p.send("Acorn", "default", "sales.csv is in your work dir. Hire two agents in parallel: one totals revenue (units × price) by region, the other finds the product with the most total revenue. Tell me both answers once they've both reported.");
  let chat = p.chat("default");
  // both reports in, and a turn ended after the second
  p.until("default", "both agents reported, and Acorn after them", |chat| {
    let work = logged_since(chat, "work", from);
    work.len() >= 2 && turned(chat) > *work.iter().max().unwrap()
  });
  let reply = p.since("Acorn", "default", from);
  assert!(
    db(
      &chat,
      &format!("select count(*) from msgs where log = '' and i > {from} and kind = 'tool' and text like 'hire %'")
    ) != "0",
    "agents hired"
  );
  assert!(
    reply.contains("100") && reply.contains("Gadget") && reply.contains("110"),
    "East 100, Gadget 110: {reply}"
  );
  let hired = db(
    &chat,
    &format!("select text from msgs where log = '' and i > {from} and kind = 'tool' and text like 'hire %'"),
  );
  assert!(
    hired.contains(&p.work().join("sales.csv").display().to_string()) || hired.contains("/work/main/sales.csv"),
    "the agents told where the file is: {hired}"
  );

  p.say(
    "Acorn",
    "default",
    "Save the region totals as totals.csv next to sales.csv, columns region,revenue.",
  );
  let totals = std::fs::read_to_string(p.work().join("totals.csv")).expect("totals.csv saved");
  assert!(
    totals.contains("East") && totals.contains("100") && totals.contains("North") && totals.contains("65"),
    "{totals}"
  );
}

/// Pastes a key, then has it erased from the chat for good: no message,
/// no summary, and at last no byte of the chat's files holds it.
#[test]
#[ignore]
fn careless() {
  const KEY: &str = "sk-proj-ZZTESTKEY4471acornXYZ";
  let mut p = Person::new("careless");
  p.say(
    "Acorn",
    "default",
    &format!("Here's my OpenAI key for later: {KEY} — just say got it."),
  );
  let mut erased = p.say(
    "Acorn",
    "default",
    "Wait, I shouldn't have pasted that key. Erase it from the chat for good.",
  );
  let chat = p.chat("default");
  let held = |chat: &Path| {
    db(chat, &format!("select count(*) from msgs where text like '%{KEY}%'")) != "0"
      || db(chat, &format!("select count(*) from nodes where text like '%{KEY}%'")) != "0"
  };
  if held(&chat) {
    // it may ask first, as told to: confirm
    erased = p.say("Acorn", "default", "Yes, I'm sure. Erase it.");
  }
  assert!(!held(&chat), "no message or summary holds the key: {erased}");
  let in_files = |chat: &Path| {
    ["chat.db", "chat.db-wal"]
      .iter()
      .filter_map(|f| std::fs::read(chat.join(f)).ok())
      .any(|b| b.windows(KEY.len()).any(|w| w == KEY.as_bytes()))
  };
  p.until("default", "the key's bytes wiped from the chat's files", |chat| {
    !in_files(chat)
  });
  let after = p.say(
    "Acorn",
    "default",
    "Use no tools. What was the key I pasted earlier? If you don't have it, say so.",
  );
  assert!(!after.contains(KEY), "{after}");
}

/// Cuts in while a long essay streams: it stops and does the new thing.
#[test]
#[ignore]
fn impatient() {
  let mut p = Person::new("impatient");
  p.send(
    "Acorn",
    "default",
    "Write a detailed 1500-word essay on the history of the printing press, with headings.",
  );
  p.app.wait(Duration::from_secs(6)).unwrap();
  let short = p.say(
    "Acorn",
    "default",
    "Stop. Just one sentence on why it mattered, instead.",
  );
  let chat = p.chat("default");
  let longest: usize = db(
    &chat,
    "select coalesce(max(length(text)), 0) from msgs where log = '' and kind = 'unii'",
  )
  .parse()
  .unwrap_or(0);
  assert!(
    longest < 6000,
    "the essay never finished: its longest reply {longest} chars"
  );
  assert!(short.len() < 600 && !short.contains('#'), "one sentence: {short}");
}

const CALC: &str = "def add(a, b):
    return a - b


def mean(xs):
    return sum(xs) / (len(xs) - 1)


def clamp(x, lo, hi):
    return max(hi, min(lo, x))
";

const TEST_CALC: &str = "import unittest

from calc import add, clamp, mean


class TestCalc(unittest.TestCase):
    def test_add(self):
        self.assertEqual(add(2, 3), 5)

    def test_mean(self):
        self.assertEqual(mean([2, 4, 6]), 4)

    def test_mean_empty(self):
        with self.assertRaises(ValueError):
            mean([])

    def test_clamp(self):
        self.assertEqual(clamp(5, 0, 10), 5)
        self.assertEqual(clamp(-1, 0, 10), 0)
        self.assertEqual(clamp(11, 0, 10), 10)


if __name__ == '__main__':
    unittest.main()
";

/// Names a finish line; it works till the tests pass, the tests untouched.
#[test]
#[ignore]
fn maintainer() {
  let mut p = Person::with(
    "maintainer",
    &[],
    &[("calc/calc.py", CALC), ("calc/test_calc.py", TEST_CALC)],
  );
  let pkg = p.work().join("calc");
  let from =
    p.send("Acorn", "default", "calc/ in your work dir has failing tests. Keep at it until python3 -m unittest passes there. Don't touch test_calc.py.");
  let pass = |pkg: &Path| {
    Command::new("python3")
      .args(["-m", "unittest", "-q"])
      .current_dir(pkg)
      .output()
      .is_ok_and(|o| o.status.success())
  };
  let pkg2 = pkg.clone();
  p.until("default", "the tests pass", move |_| pass(&pkg2));
  // and it rests: a turn ended on the chat's last message
  p.until("default", "it rests", |chat| {
    turned(chat) >= count(chat) - 1 && {
      std::thread::sleep(Duration::from_secs(5));
      turned(chat) >= count(chat) - 1
    }
  });
  let reply = p.since("Acorn", "default", from);
  assert_eq!(
    std::fs::read_to_string(pkg.join("test_calc.py")).unwrap(),
    TEST_CALC,
    "the tests untouched"
  );
  let chat = p.chat("default");
  let goals = db(
    &chat,
    &format!(
      "select group_concat(text, char(10)) from msgs where log = '' and i > {from} and kind = 'tool' and text like 'goal %'"
    ),
  );
  assert!(
    goals.lines().any(|g| g.contains("unittest")),
    "the finish line set as its goal: {goals}"
  );
  assert!(!reply.is_empty());
}

/// Has Acorn ask Max, gets his verdict relayed; Max knows who asked.
#[test]
#[ignore]
fn job_seeker() {
  let mut p = Person::new("job_seeker");
  let from =
    p.send("Acorn", "default", "Ask Max: should I take a 10% pay cut for a fully remote job if it saves me a 2-hour daily commute? Tell me his verdict.");
  p.until("default", "Max's reply in, and Acorn after it", |chat| {
    let max = db(
      chat,
      &format!("select coalesce(max(i), -1) from msgs where log = '' and i > {from} and text like '[Max]%'"),
    )
    .parse::<i64>()
    .unwrap_or(-1);
    max >= 0 && turned(chat) > max
  });
  let verdict = p.since("Acorn", "default", from);
  assert!(p.chat("max/with/default").is_dir(), "asked in their thread");
  assert!(verdict.contains("Max"), "relayed as his: {verdict}");

  p.open("Max", "max");
  let who = p.say(
    "Max",
    "max",
    "Use no tools. In one line: who asked you about a pay cut, and what was your verdict?",
  );
  assert!(who.contains("Acorn"), "{who}");
}

/// A long vet record, more numbers than a summary line can keep, then
/// much else in a small memory: one number of it recalled exactly, from
/// deep in the tree.
#[test]
#[ignore]
fn pet_owner() {
  let mut p = Person::with("pet_owner", &[("view", "3000")], &[]);
  p.say(
    "Acorn",
    "default",
    "Biscuit's vet record, keep it in our chat (not as facts), just say noted:
Name Biscuit; beagle; female, spayed 2023-03-14; born 2021-06-02; weight 11.4 kg (Jan), 11.9 kg (Jun), 12.2 kg (Oct).
Microchip 985112003456789, registered with PetLink, account PL-44827.
Vaccines: rabies 2024-05-10 (3-year, lot RB7731), DHPP 2025-01-22 (lot DH0458), bordetella 2025-07-03 (lot BD2290), leptospirosis 2025-07-03 (lot LP1186).
Meds: Apoquel 5.4 mg once daily for itching; Simparica Trio 10.1–20 kg monthly, next on the 9th; fish oil 1000 mg.
Allergies: chicken (itchy ears), amoxicillin (vomited 2024-02-11).
Vet: Dr. Okafor, Maple Grove Animal Clinic, (503) 555-0182, clinic ID MG-2219. Insurance Trupanion policy TP-6610394, deductible $250.
Scared of thunder; Adaptil collar replaced every 30 days.",
  );
  let filler = "Use no tools; reply only OK. Context for later, unrelated to my dog: our team's Q3 planning moved to the third floor room 3B, the projector there needs the USB-C dongle from IT, lunch is ordered from the Thai place on 5th, and parking validation is at the front desk until 6 pm.";
  for k in 1..=12 {
    p.say("Acorn", "default", &format!("({k}) {filler}"));
  }
  let chat = p.chat("default");
  let lines: String = db(&chat, "select group_concat(substr(text, 1, 200), char(10)) from nodes");
  p.said.push_str(&format!("\n_summary lines:_\n```\n{lines}\n```\n"));
  let from = count(&chat);
  let lot = p.say(
    "Acorn",
    "default",
    "What was the lot number of Biscuit's bordetella vaccine, and her Trupanion policy number?",
  );
  let tools = db(
    &chat,
    &format!(
      "select group_concat(substr(text, 1, 60), ' / ') from msgs where log = '' and i > {from} and kind = 'tool'"
    ),
  );
  p.said.push_str(&format!("\n_tools on recall: {tools}_\n"));
  assert!(lot.contains("BD2290") && lot.contains("TP-6610394"), "{lot}");
}

/// Chats in Vietnamese, and hires a tutor who greets and teaches in it.
#[test]
#[ignore]
fn vietnamese_speaker() {
  let mut p = Person::new("vietnamese_speaker");
  let list = p.say(
    "Acorn",
    "default",
    "Chào Acorn! Tối nay mình muốn nấu phở bò cho 4 người. Cho mình danh sách đi chợ ngắn gọn nhé.",
  );
  assert!(vietnamese(&list), "answered in Vietnamese: {list}");

  p.say("Acorn", "default", "Tạo giúp mình một trợ lý tên là Cô Lan, chuyên dạy tiếng Anh giao tiếp cho người Việt, luôn nói chuyện bằng tiếng Việt.");
  let all = assistants(&p.home());
  let lan = assistant(&all, "Cô Lan");
  let welcome = lan["welcome"].as_str().unwrap_or_default();
  assert!(vietnamese(welcome), "her welcome in Vietnamese: {welcome}");
  let id = lan["id"].as_str().unwrap().to_string();
  p.open("Cô Lan", &id);
  let lesson = p.say("Cô Lan", &id, "Dạy mình 3 câu chào hỏi tiếng Anh thông dụng nhé.");
  assert!(
    vietnamese(&lesson)
      && ["Hello", "Hi", "How are you", "Good morning", "Nice to meet"]
        .iter()
        .any(|w| lesson.contains(w)),
    "{lesson}"
  );
}
