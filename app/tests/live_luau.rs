//! The luau tool, live: jobs too big or too many-stepped for one tool call
//! each, which Acorn does in Luau. The answers are checked against what the
//! test computed from the data it made, the files on disk, and the luau
//! calls in the chat's db: data loaded once and kept in the VM, many files
//! edited in one call, a shell's output parsed, JSON read and written,
//! failures caught.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn sql(db: &Path, query: &str) -> String {
  let out = Command::new("sqlite3")
    .arg("-readonly")
    .arg(db)
    .arg(query)
    .output()
    .unwrap();
  String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Acorn's tool calls (its log, named ""), in order, one per line.
fn tool_calls(db: &Path) -> Vec<String> {
  sql(
    db,
    "select replace(text, char(10), ' ') from msgs where log = '' and kind = 'tool' order by i;",
  )
  .lines()
  .map(String::from)
  .collect()
}

/// Acorn's tool results, in order, one per line.
fn echoes(db: &Path) -> Vec<String> {
  sql(
    db,
    "select replace(text, char(10), ' ') from msgs where log = '' and kind = 'echo' order by i;",
  )
  .lines()
  .map(String::from)
  .collect()
}

/// On a panic, prints Acorn's log, since the temp dir goes with the test.
struct Dump(std::path::PathBuf);

impl Drop for Dump {
  fn drop(&mut self) {
    if std::thread::panicking() {
      let all = "select i, kind, replace(substr(text, 1, 600), char(10), ' / ') from msgs where log = '' order by i;";
      println!("---- chat log at failure ----\n{}", sql(&self.0, all));
    }
  }
}

/// A fixed stream of numbers, so the data and its answers are the same
/// each run.
struct Lcg(u64);

impl Lcg {
  fn below(&mut self, n: u64) -> u64 {
    self.0 = self
      .0
      .wrapping_mul(6364136223846793005)
      .wrapping_add(1442695040888963407);
    (self.0 >> 33) % n
  }
}

/// The number after `key=` in `text`, if any.
fn number(text: &str, key: &str) -> Option<u64> {
  let at = text.rfind(&format!("{key}="))? + key.len() + 1;
  let digits: String = text[at..].chars().take_while(char::is_ascii_digit).collect();
  digits.parse().ok()
}

/// The word after `key=` in `text`, if any.
fn word<'a>(text: &'a str, key: &str) -> Option<&'a str> {
  let at = text.rfind(&format!("{key}="))? + key.len() + 1;
  let rest = &text[at..];
  let end = rest
    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ',')))
    .unwrap_or(rest.len());
  Some(rest[..end].trim_end_matches(['.', ',']))
}

/// Asks the model; run with `cargo test -- --ignored` and DEEPSEEK_API_KEY.
#[test]
#[ignore]
fn luau_does_the_big_jobs_live() {
  let dir = tempfile::tempdir().unwrap();
  let proj = dir.path().join("proj");
  std::fs::create_dir_all(&proj).unwrap();
  let db = dir.path().join("chats/default/chat.db");
  let _dump = Dump(db.clone());
  let mut rng = Lcg(42);

  // Sales: too long to read whole with read (2000 lines, 50 KB at most).
  let regions = ["North", "South", "East", "West"];
  let products = ["acorn", "birch", "cedar", "maple", "oak", "pine", "willow"];
  let mut csv = String::from("date,region,product,qty,price\n");
  let (mut north, mut units) = (0u64, BTreeMap::<&str, u64>::new());
  for k in 0..60_000u64 {
    let region = regions[rng.below(4) as usize];
    // A skew, so one product clearly sells the most.
    let product = if rng.below(10) == 0 {
      "maple"
    } else {
      products[rng.below(7) as usize]
    };
    let (qty, price) = (1 + rng.below(20), 100 + rng.below(900));
    csv.push_str(&format!(
      "2026-{:02}-{:02},{region},{product},{qty},{price}\n",
      1 + k % 12,
      1 + k % 28
    ));
    if region == "North" {
      north += qty * price;
    }
    *units.entry(product).or_default() += qty;
  }
  std::fs::write(proj.join("sales.csv"), csv).unwrap();
  let (top, top_units) = units.iter().max_by_key(|(_, n)| **n).map(|(p, n)| (*p, *n)).unwrap();

  // Configs: some timeouts to double, some to leave.
  let conf = proj.join("conf");
  std::fs::create_dir_all(&conf).unwrap();
  let mut want = BTreeMap::new();
  let mut doubled = 0;
  for k in 0..16u64 {
    let timeout = 5 + rng.below(50);
    let name = format!("svc{k:02}.cfg");
    let body = |t: u64| format!("name = svc{k:02}\ntimeout = {t}\nretries = 3\n");
    std::fs::write(conf.join(&name), body(timeout)).unwrap();
    let after = if timeout < 30 {
      doubled += 1;
      timeout * 2
    } else {
      timeout
    };
    want.insert(name, body(after));
  }

  // A repo: whose commits, and how many lines each added.
  let repo = proj.join("repo");
  std::fs::create_dir_all(&repo).unwrap();
  let git = |args: &[&str]| {
    let ok = Command::new("git").args(args).current_dir(&repo).output().unwrap();
    assert!(
      ok.status.success(),
      "git {args:?}: {}",
      String::from_utf8_lossy(&ok.stderr)
    );
  };
  git(&["init", "-q"]);
  let authors = [
    "Ada", "Ben", "Ada", "Cy", "Ada", "Ben", "Ada", "Cy", "Ben", "Ada", "Cy", "Ada", "Ben",
  ];
  let mut by: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
  for (k, who) in authors.iter().enumerate() {
    let lines = 3 + rng.below(40);
    let text: String = (0..lines).map(|n| format!("line {n}\n")).collect();
    std::fs::write(repo.join(format!("f{k}.txt")), text).unwrap();
    git(&["add", "."]);
    let id = format!("user.name={who}");
    let mail = format!("user.email={}@x.test", who.to_lowercase());
    git(&["-c", &id, "-c", &mail, "commit", "-q", "-m", &format!("add f{k}")]);
    let e = by.entry(who).or_default();
    e.0 += 1;
    e.1 += lines;
  }
  let (lead, (_, lead_added)) = by.iter().max_by_key(|(_, (c, _))| *c).unwrap();

  // Events: who had errors, and who the most.
  let mut events = Vec::new();
  let mut errors: BTreeMap<String, u64> = BTreeMap::new();
  for k in 0..3000u64 {
    let user = if k % 97 == 0 {
      "u7".to_string()
    } else {
      format!("u{}", rng.below(60))
    };
    let kind = if user == "u7" && k % 97 == 0 {
      "error"
    } else {
      ["view", "click", "view", "error", "click", "view"][rng.below(6) as usize]
    };
    if kind == "error" {
      *errors.entry(user.clone()).or_default() += 1;
    }
    events.push(serde_json::json!({"user": user, "type": kind, "ms": rng.below(5000)}));
  }
  std::fs::write(proj.join("events.json"), serde_json::to_string(&events).unwrap()).unwrap();
  let (worst, worst_n) = errors.iter().max_by_key(|(_, n)| **n).unwrap();
  let error_users = errors.len() as u64;

  // Notes, two of them missing.
  for (name, lines) in [("a.txt", 3), ("b.txt", 5), ("d.txt", 7)] {
    let text: String = (0..lines).map(|n| format!("note {n}\n")).collect();
    std::fs::write(proj.join(name), text).unwrap();
  }

  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("HOME", dir.path().join("user"));
    std::env::set_var("ACORN_DIR", dir.path().join("home"));
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
  }
  let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
  app.first_render().unwrap();
  app
    .wait_for_within("Hello! This is Acorn.", Duration::from_secs(20))
    .unwrap();

  // Sends a message, waits until Acorn's reply after it says `word`, then
  // until its turn has ended; returns its replies, and its tool calls
  // since the message.
  let ask = |app: &mut Headless, what: &str, word: &str| -> (String, Vec<String>) {
    let before = sql(&db, "select coalesce(max(i), -1) from msgs where log = '';")
      .trim()
      .to_string();
    let calls_before = tool_calls(&db).len();
    app.type_text("label:Message Acorn", what).unwrap();
    app.submit("label:Message Acorn").unwrap();
    let said =
      format!("select count(*) from msgs where log = '' and kind = 'unii' and i > {before} and text like '%{word}%';");
    app
      .wait_until(word, Duration::from_secs(400), |_| sql(&db, &said).trim() != "0")
      .unwrap();
    std::thread::sleep(Duration::from_secs(10)); // let the turn end before the next message cuts in
    let reply = sql(
      &db,
      &format!("select text from msgs where log = '' and kind = 'unii' and i > {before} order by i;"),
    );
    let calls = tool_calls(&db)[calls_before..].to_vec();
    println!("---- {word} ----\nreply: {reply}\ncalls: {calls:#?}");
    (reply, calls)
  };
  let luau = |calls: &[String]| -> Vec<String> { calls.iter().filter(|c| c.starts_with("luau {")).cloned().collect() };
  let p = proj.display();

  // A. a file too big to read: loaded once, into a global, and summed
  let (reply, calls) = ask(
    &mut app,
    &format!(
      "{p}/sales.csv has a header and then one sale per line: date,region,product,qty,price. With your luau tool, load it into a global you keep for later questions, and tell me the North region's revenue (the sum of qty*price), as digits only, written as REV=<number>."
    ),
    "REV=",
  );
  assert_eq!(number(&reply, "REV"), Some(north), "North's revenue");
  let a = luau(&calls);
  assert!(
    a.iter().any(|c| c.contains("sales.csv")),
    "luau loaded the file: {calls:#?}"
  );
  assert!(
    !calls.iter().any(|c| c.starts_with("read {") && c.contains("sales.csv")),
    "paged through it with read"
  );

  // B. the next question, on the data the VM kept: a guarded fallback
  // read may be in the code, but none runs
  let echoes_before = echoes(&db).len();
  let (reply, calls) = ask(
    &mut app,
    "From the sales you already loaded in luau (don't read the file again), which product sold the most units in all? Answer as PROD=<name> UNITS=<number>.",
    "UNITS=",
  );
  assert_eq!(word(&reply, "PROD"), Some(top), "the top product");
  assert_eq!(number(&reply, "UNITS"), Some(top_units), "its units");
  let b = luau(&calls);
  assert!(!b.is_empty(), "answered without luau: {calls:#?}");
  let ran = echoes(&db)[echoes_before..].to_vec();
  assert!(
    !ran.iter().any(|e| e.contains("[calls: ") && e.contains("read \""))
      && !calls.iter().any(|c| c.starts_with("read {") || c.starts_with("bash {")),
    "read the file again rather than use the VM's global: {ran:#?}"
  );

  // C. many files edited in one go, each read first, nothing else touched
  let (reply, calls) = ask(
    &mut app,
    &format!(
      "In {p}/conf, each .cfg file has a line `timeout = N`. With one luau call, double the timeout in every file where it is under 30, changing nothing else in any file. Then tell me how many files you changed, as EDITS=<number>."
    ),
    "EDITS=",
  );
  for (name, body) in &want {
    assert_eq!(&std::fs::read_to_string(conf.join(name)).unwrap(), body, "{name}");
  }
  assert_eq!(number(&reply, "EDITS"), Some(doubled), "files changed");
  let c = luau(&calls);
  assert!(!c.is_empty() && c.len() <= 3, "{} luau calls for the edits", c.len());
  assert!(
    !calls
      .iter()
      .any(|c| c.starts_with("edit {") || c.starts_with("write {")),
    "edited with the native tools: {calls:#?}"
  );

  // D. a command's output parsed in Luau
  let (reply, calls) = ask(
    &mut app,
    &format!(
      "In the git repo {p}/repo, using luau with sh (git log --numstat), find the author with the most commits and the total number of lines they added. Answer as AUTHOR=<name> ADDED=<number>."
    ),
    "ADDED=",
  );
  assert_eq!(word(&reply, "AUTHOR"), Some(*lead), "the top author");
  assert_eq!(number(&reply, "ADDED"), Some(*lead_added), "lines they added");
  assert!(
    luau(&calls).iter().any(|c| c.contains("sh(") && c.contains("numstat")),
    "git run from luau: {calls:#?}"
  );

  // E. JSON in, JSON out
  let (_, calls) = ask(
    &mut app,
    &format!(
      "{p}/events.json is a JSON array of events, each {{user, type, ms}}. With luau and its json functions, count the distinct users with at least one event of type \"error\", and find the user with the most errors. Write {p}/report.json as a JSON object {{\"error_users\": <count>, \"worst\": <user>, \"worst_errors\": <their error count>}}, then reply REPORT-DONE."
    ),
    "REPORT-DONE",
  );
  let report: serde_json::Value =
    serde_json::from_str(&std::fs::read_to_string(proj.join("report.json")).expect("report.json written")).unwrap();
  assert_eq!(report["error_users"], error_users, "{report}");
  assert_eq!(report["worst"], worst.as_str(), "{report}");
  assert_eq!(report["worst_errors"], *worst_n, "{report}");
  assert!(
    luau(&calls).iter().any(|c| c.contains("json.")),
    "used luau's json: {calls:#?}"
  );

  // F. failures caught, the rest still done, in one call
  let (reply, calls) = ask(
    &mut app,
    &format!(
      "In one luau call, count the lines of {p}/a.txt, {p}/b.txt, {p}/c.txt, {p}/d.txt and {p}/e.txt. Some may not exist: catch that with pcall and go on. Answer as TOTAL=<lines of those that exist> MISSING=<their file names, comma-separated, no spaces>."
    ),
    "MISSING=",
  );
  assert_eq!(number(&reply, "TOTAL"), Some(15), "lines of the files there");
  let missing: BTreeSet<&str> = word(&reply, "MISSING")
    .unwrap_or_default()
    .split(',')
    .map(|name| name.trim_end_matches(".txt"))
    .collect();
  assert_eq!(missing, BTreeSet::from(["c", "e"]), "{reply}");
  let f = luau(&calls);
  assert!(f.iter().any(|c| c.contains("pcall")), "caught with pcall: {calls:#?}");

  // The VM's results stayed small: nothing dumped a file whole.
  let biggest = echoes(&db).iter().map(String::len).max().unwrap_or(0);
  assert!(biggest < 20_000, "a result of {biggest} bytes came back");
}

/// A picture only seen can answer: a square of one colour, which view
/// shows the model. Run alone (one engine a process).
#[test]
#[ignore]
fn luau_views_an_image_live() {
  let dir = tempfile::tempdir().unwrap();
  let proj = dir.path().join("proj");
  std::fs::create_dir_all(&proj).unwrap();
  let db = dir.path().join("chats/default/chat.db");
  let _dump = Dump(db.clone());
  let (name, rgb) = [
    ("red", [220, 30, 30]),
    ("green", [30, 170, 60]),
    ("blue", [30, 70, 220]),
  ][std::process::id() as usize % 3];
  let mut img = image::RgbImage::from_pixel(400, 300, image::Rgb([255, 255, 255]));
  for x in 100..300 {
    for y in 50..250 {
      img.put_pixel(x, y, image::Rgb(rgb));
    }
  }
  img.save(proj.join("shape.png")).unwrap();

  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("HOME", dir.path().join("user"));
    std::env::set_var("ACORN_DIR", dir.path().join("home"));
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
  }
  let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
  app.first_render().unwrap();
  app
    .wait_for_within("Hello! This is Acorn.", Duration::from_secs(20))
    .unwrap();
  app
    .type_text(
      "label:Message Acorn",
      &format!(
        "Look at the picture {}/shape.png. What colour is the square in it? Answer as COLOR=<one word>.",
        proj.display()
      ),
    )
    .unwrap();
  app.submit("label:Message Acorn").unwrap();
  let said = "select group_concat(text, ' ') from msgs where log = '' and kind = 'unii' and text like '%COLOR=%';";
  app
    .wait_until("COLOR=", Duration::from_secs(300), |_| {
      !sql(&db, said).trim().is_empty()
    })
    .unwrap();
  let reply = sql(&db, said).to_lowercase();
  let calls = tool_calls(&db);
  println!("reply: {reply}\ncalls: {calls:#?}");
  assert!(
    calls.iter().any(|c| c.starts_with("luau {") && c.contains("view(")),
    "looked without view: {calls:#?}"
  );
  assert!(
    reply.contains(&format!("color={name}")),
    "the square is {name}: {reply}"
  );
}

/// Plugins on a chat that goes on, Acorn never restarted: search as shipped,
/// then Exa's plugin dropped in, then TinyFish's for it, then Acorn installing
/// Exa's itself when asked. Run alone (one engine a process), with
/// DEEPSEEK_API_KEY, EXA_API_KEY and TINYFISH_API_KEY.
#[test]
#[ignore]
fn plugins_change_search_mid_chat_live() {
  let dir = tempfile::tempdir().unwrap();
  let db = dir.path().join("chats/default/chat.db");
  let _dump = Dump(db.clone());
  let home = dir.path().join("home");
  let plugins = home.join("plugins");
  std::fs::create_dir_all(&plugins).unwrap();
  let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins");
  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("HOME", dir.path().join("user"));
    std::env::set_var("ACORN_DIR", &home);
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
  }
  let mut app = Headless::start(EngineConfig::new(Root::of::<acorn::App>())).unwrap();
  app.first_render().unwrap();
  app
    .wait_for_within("Hello! This is Acorn.", Duration::from_secs(20))
    .unwrap();
  let mut ask = |tag: &str, what: &str| {
    app
      .type_text(
        "label:Message Acorn",
        &format!("{what} Search with web_search in luau, once, and answer as {tag}=<the url you found>."),
      )
      .unwrap();
    app.submit("label:Message Acorn").unwrap();
    let said = format!("select text from msgs where log = '' and kind = 'unii' and text like '%{tag}=%';");
    app
      .wait_until(tag, Duration::from_secs(300), |_| !sql(&db, &said).trim().is_empty())
      .unwrap();
    println!("{tag}: {}", sql(&db, &said).trim());
    echoes(&db).join("\n")
  };

  let first = ask("FIRST", "Find the Rust language's home page.");
  assert!(
    !first.contains("http \""),
    "shipped search goes through no plugin: {first}"
  );

  std::fs::copy(shipped.join("exa.luau"), plugins.join("exa.luau")).unwrap();
  let exa = ask("SECOND", "Find the Python language's home page.");
  assert!(exa.contains("http \"https://api.exa.ai/search\""), "{exa}");
  assert!(exa.contains("plugins changed"), "{exa}");
  assert!(!exa.contains("did not load"), "{exa}");

  std::fs::remove_file(plugins.join("exa.luau")).unwrap();
  std::fs::copy(shipped.join("tinyfish.luau"), plugins.join("tinyfish.luau")).unwrap();
  let fish = ask("THIRD", "Find the Go language's home page.");
  assert!(fish.contains("http \"https://api.search.tinyfish.ai\""), "{fish}");
  assert!(!fish.contains("did not load"), "{fish}");

  let asked = format!(
    "Use Exa instead of TinyFish for search from now on: the plugin is at {}, so remove TinyFish's and install that as your plugins note says. Then find the Zig language's home page.",
    shipped.join("exa.luau").display()
  );
  let done = ask("FOURTH", &asked);
  assert!(plugins.join("exa.luau").exists() && !plugins.join("tinyfish.luau").exists());
  assert_eq!(done.matches("http \"https://api.exa.ai/search\"").count(), 2, "{done}");
  assert!(!done.contains("did not load"), "{done}");
}
