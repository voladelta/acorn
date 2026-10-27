//! People using Acorn, live: the window, headless, on the real engine and
//! DeepSeek, each over chats of their own. A developer builds and changes
//! a small package; a knowledge worker turns notes into tasks, has a fact
//! kept and hires an editor who stays; a student learns and asks Max; a
//! teacher makes a quiz writer, lets them go and brings them back; a
//! researcher searches and fetches the web. Each checks what can be
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
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: set once, before any thread of the test starts.
        unsafe {
            std::env::set_var("ACORN_DIR", dir.path().join("home"));
            std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
        }
        let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
        app.first_render().unwrap();
        app.wait_for_within("Hello! This is Acorn.", Duration::from_secs(20)).unwrap();
        Person { app, dir, name, said: format!("# {name}\n") }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn chat(&self, id: &str) -> PathBuf {
        self.dir.path().join("chats").join(id)
    }

    /// An assistant picked in the sidebar, its chat shown; its welcome, read.
    fn open(&mut self, name: &str, id: &str) -> String {
        self.app.press(&format!("text:{name}")).unwrap();
        self.app.wait_for_within(&format!("label:Message {name}"), Duration::from_secs(20)).unwrap();
        let hello = db(&self.chat(id), "select text from msgs where log = '' and i = 0");
        self.said.push_str(&format!("\n---\n## Opened {name}; its welcome\n\n{hello}\n"));
        hello
    }

    /// Words to `name` (chat `id`), its turn waited out: its last reply.
    fn say(&mut self, name: &str, id: &str, text: &str) -> String {
        let chat = self.chat(id);
        let from = count(&chat);
        self.app.type_text(&format!("label:Message {name}"), text).unwrap();
        self.app.submit(&format!("label:Message {name}")).unwrap();
        let started = Instant::now();
        // a turn's use, logged on its last message, past the user's
        while db(&chat, "select coalesce(max(msg), -1) from turns").parse::<i64>().unwrap_or(-1) < from {
            assert!(started.elapsed() < TURN, "{name}'s turn after: {text}");
            self.app.wait(Duration::from_secs(2)).unwrap();
        }
        let tools = db(&chat, &format!("select group_concat(substr(replace(text, char(10), ' '), 1, 160), char(10)) from msgs where log = '' and i > {from} and kind = 'tool'"));
        let reply = db(&chat, &format!("select text from msgs where log = '' and i > {from} and kind = 'unii' order by i desc limit 1"));
        let secs = started.elapsed().as_secs();
        let tools = if tools.is_empty() { String::new() } else { format!("\n### Tools\n\n```\n{tools}\n```\n") };
        self.said.push_str(&format!("\n---\n## To {name} ({secs} s)\n\n{text}\n{tools}\n### {name}\n\n{reply}\n"));
        reply
    }

    /// The sidebar's names, as shown.
    fn sidebar(&self) -> Vec<String> {
        let texts: Vec<String> = self.app.screen().texts().iter().map(|t| t.to_string()).collect();
        let at = texts.iter().position(|t| t == "ASSISTANTS").unwrap_or(0);
        texts[at + 1..].iter().take_while(|t| !t.contains("  ·  ")).filter(|t| t.chars().count() > 1).cloned().collect()
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
            .map(|c| db(&c.path(), "select coalesce(sum(micros), 0) from uses").parse::<f64>().unwrap_or(0.0) / 1e6)
            .sum();
        self.said.push_str(&format!("\n---\nSpent: ${spent:.4}\n"));
        if std::env::var_os("ACORN_PERSONAS_DUMP").is_some() {
            for c in std::fs::read_dir(self.dir.path().join("chats")).into_iter().flatten().flatten() {
                let rows = db(&c.path(), "select log, i, kind, size, substr(replace(text, char(10), ' '), 1, 100), '  LINE:', substr(line, 1, 100) from msgs order by log, i; select '--turns', * from turns; select '--uses', * from uses;");
                self.said.push_str(&format!("\n## {}\n```\n{rows}\n```\n", c.path().display()));
            }
        }
        let file = out.join(format!("{}.md", self.name));
        let _ = std::fs::write(&file, &self.said);
        eprintln!("transcript: {}", file.display());
    }
}

/// The messages in a chat's own log.
fn count(chat: &Path) -> i64 {
    db(chat, "select count(*) from msgs where log = ''").parse().unwrap_or(0)
}

/// A query on a chat's db, read only, its rows as lines.
fn db(chat: &Path, sql: &str) -> String {
    let out = Command::new("sqlite3").arg("-readonly").arg(chat.join("chat.db")).arg(sql).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

/// A file named `name` under `dir`, at any depth.
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == name) { Some(p) } else if p.is_dir() { find(&p, name) } else { None }
    })
}

fn assistants(home: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(home.join("assistants.json")).unwrap_or_else(|_| "[]".into())).unwrap()
}

fn assistant<'a>(all: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    all.as_array().unwrap().iter().find(|a| a["name"] == name).unwrap_or_else(|| panic!("no {name} in {all}"))
}

/// Builds a package in its work dir, tests it, changes it, and remembers it.
#[test]
#[ignore]
fn developer() {
    let mut p = Person::new("developer");
    p.say("Acorn", "default", "In your work dir, make a folder slugify_lite with slugify.py: a slugify(text) function that lowercases, keeps ASCII letters and digits, and joins words with single hyphens. Add test_slugify.py with unittest tests (standard library only). Run them and tell me the result.");
    let tests = find(&p.home().join("work"), "test_slugify.py").expect("test_slugify.py written in the work dir");
    let pkg = tests.parent().unwrap().to_path_buf();
    let run = |pkg: &Path| Command::new("python3").args(["-m", "unittest", "-q"]).current_dir(pkg).output().unwrap();
    assert!(run(&pkg).status.success(), "its tests pass: {}", String::from_utf8_lossy(&run(&pkg).stderr));

    p.say("Acorn", "default", "Now make it strip accents too, so \"Café Déjà Vu!\" becomes cafe-deja-vu. Add a test for it, run the tests, and show me only what you changed.");
    assert!(run(&pkg).status.success(), "its tests still pass");
    let slug = Command::new("python3").args(["-c", "from slugify import slugify; print(slugify('Café Déjà Vu!'))"]).current_dir(&pkg).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&slug.stdout).trim(), "cafe-deja-vu");

    let recall = p.say("Acorn", "default", "Use no tools. In one line: what function did you write for me today, and which file holds its tests?");
    assert!(recall.contains("slugify") && recall.contains("test_slugify"), "remembers: {recall}");
}

/// Notes into tasks, a fact kept and used, an editor hired who stays.
#[test]
#[ignore]
fn knowledge_worker() {
    let mut p = Person::new("knowledge_worker");
    let table = p.say("Acorn", "default", "Notes from today's ops sync:\n- Dana will send the vendor shortlist by Wednesday\n- Omar to fix the onboarding doc's broken links, end of week\n- I need to book the Q3 offsite venue before the 15th\n- budget review pushed to next month\n\nTurn these into a table: owner, task, due.");
    assert!(table.contains('|') && table.contains("Dana") && table.contains("Omar"), "a table of who does what: {table}");

    p.say("Acorn", "default", "Remember that my manager is Priya Raman and I send her an update every Friday.");
    let facts = Command::new("sqlite3").arg(p.home().join("facts.db")).arg("select * from facts").output().unwrap();
    assert!(String::from_utf8_lossy(&facts.stdout).contains("Priya"), "the fact kept");

    let email = p.say("Acorn", "default", "Draft this Friday's update email to my manager, from the action items above. Short.");
    assert!(email.contains("Priya") && email.contains("Dana"), "the fact and the notes used: {email}");

    p.say("Acorn", "default", "Hire an editor called Edith, to stay with me: she tightens my writing into plain English and explains each cut.");
    let all = assistants(&p.home());
    let edith = assistant(&all, "Edith");
    assert!(!edith["welcome"].as_str().unwrap_or_default().trim().is_empty(), "made with a welcome: {edith}");
    let id = edith["id"].as_str().unwrap().to_string();
    let hello = p.open("Edith", &id);
    assert_eq!(hello.trim(), edith["welcome"].as_str().unwrap().trim(), "her chat opens with her welcome");
    let tight = p.say("Edith", &id, "We are currently in the process of evaluating the potential options going forward with regard to the vendor situation.");
    assert!(!tight.is_empty());
}

/// Learns, asks Max directly, and is quizzed on what was covered.
#[test]
#[ignore]
fn student() {
    let mut p = Person::new("student");
    let d = p.say("Acorn", "default", "I'm studying for a calculus test. Explain the derivative of x^3 simply, with one worked example.");
    assert!(d.contains("3x") || d.contains("3 x"), "the derivative: {d}");

    let hello = p.open("Max", "max");
    assert!(hello.starts_with("Hello! This is Max.") && hello.contains("when it matters"), "Max's own welcome: {hello}");
    let max = p.say("Max", "max", "My friend says 0.999... repeating is less than 1. Who's right? Settle it.");
    assert!(max.contains('1'), "{max}");

    p.open("Acorn", "default");
    let quiz = p.say("Acorn", "default", "Quiz me: 3 short questions on what we covered today. No answers yet.");
    assert!(["deriv", "x³", "x^3", "power rule"].iter().any(|w| quiz.to_lowercase().contains(w)), "on what was covered: {quiz}");
    let mark = p.say("Acorn", "default", "1) 6x^5 2) 0 3) I don't know");
    assert!(!mark.is_empty());
}

/// Makes a quiz writer, works with them, lets them go, brings them back.
#[test]
#[ignore]
fn teacher() {
    let mut p = Person::new("teacher");
    p.say("Acorn", "default", "Create an assistant called Quiz Master for my 8th-grade science class: writes short quizzes with answer keys, friendly and age-appropriate.");
    let all = assistants(&p.home());
    let qm = assistant(&all, "Quiz Master");
    assert!(!qm["welcome"].as_str().unwrap_or_default().trim().is_empty(), "made with a welcome: {qm}");
    let id = qm["id"].as_str().unwrap().to_string();
    assert!(p.sidebar().contains(&"Quiz Master".to_string()), "in the sidebar: {:?}", p.sidebar());

    p.open("Quiz Master", &id);
    let quiz = p.say("Quiz Master", &id, "3-question multiple choice quiz on photosynthesis, with an answer key at the end.");
    assert!(quiz.to_lowercase().contains("answer"), "{quiz}");

    p.open("Acorn", "default");
    p.say("Acorn", "default", "Write a short note to parents: Friday's quiz is on photosynthesis, and how they can help at home.");
    p.say("Acorn", "default", "I don't need Quiz Master anymore, let them go.");
    assert_eq!(assistant(&assistants(&p.home()), "Quiz Master")["archived"], true, "archived, not deleted");
    assert!(p.chat(&id).is_dir(), "their chat kept");
    p.app.wait(Duration::from_secs(1)).unwrap();
    assert!(!p.sidebar().contains(&"Quiz Master".to_string()), "off the sidebar: {:?}", p.sidebar());

    let who = p.say("Acorn", "default", "Who did I have write my quizzes before? One line.");
    assert!(who.contains("Quiz Master"), "still known: {who}");
    p.say("Acorn", "default", "Actually, bring Quiz Master back.");
    assert!(assistant(&assistants(&p.home()), "Quiz Master").get("archived").is_none_or(|a| a == false), "restored");
    p.app.wait(Duration::from_secs(1)).unwrap();
    assert!(p.sidebar().contains(&"Quiz Master".to_string()), "back on the sidebar: {:?}", p.sidebar());
    let hello = p.open("Quiz Master", &id);
    assert!(!hello.is_empty());
    let again = p.say("Quiz Master", &id, "Use no tools. What topic was the last quiz you wrote me?");
    assert!(again.to_lowercase().contains("photosynth"), "remembers, as it was: {again}");
}

/// Searches and fetches the web, citing where it read.
#[test]
#[ignore]
fn researcher() {
    let mut p = Person::new("researcher");
    let v = p.say("Acorn", "default", "Search the web: what is the latest stable release of Python, and when was it released? Cite the URL you read.");
    assert!(v.contains("3.") && v.contains("http"), "a version, cited: {v}");
    let h = p.say("Acorn", "default", "Fetch https://www.rust-lang.org/ and tell me its main headline, word for word.");
    assert!(h.to_lowercase().contains("reliable") || h.to_lowercase().contains("efficient") || h.to_lowercase().contains("empowering"), "{h}");
    let sum = p.say("Acorn", "default", "Compare the two languages' latest releases in a 3-row table, from what you found. No more searching.");
    assert!(sum.contains('|'));
}
