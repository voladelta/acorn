//! Luau
//! ====
//! Jobs too big or too many-stepped for one tool call each, which Acorn
//! does in Luau, checked against what the scenario computed from the data
//! it made; a picture only seen can answer; plugins changed mid-chat.

use crate::kit::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

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

/// What the data made for `jobs` holds, to check the answers by.
#[derive(Default)]
struct Answers {
  north: u64,
  top: String,
  top_units: u64,
  configs: BTreeMap<String, String>,
  doubled: u64,
  lead: String,
  lead_added: u64,
  error_users: u64,
  worst: String,
  worst_errors: u64,
}

/// The data, made in `proj`: sales too long to read whole, configs to
/// change some of, a git repo, events as JSON, notes two of which are
/// missing.
fn data(proj: &Path) -> Answers {
  let mut rng = Lcg(42);
  let mut a = Answers::default();
  let regions = ["North", "South", "East", "West"];
  let products = ["acorn", "birch", "cedar", "maple", "oak", "pine", "willow"];
  let mut csv = String::from("date,region,product,qty,price\n");
  let mut units = BTreeMap::<&str, u64>::new();
  for k in 0..60_000u64 {
    let region = regions[rng.below(4) as usize];
    // a skew, so one product clearly sells the most
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
      a.north += qty * price;
    }
    *units.entry(product).or_default() += qty;
  }
  write(&proj.join("sales.csv"), &csv);
  let (top, n) = units.iter().max_by_key(|(_, n)| **n).unwrap();
  (a.top, a.top_units) = (top.to_string(), *n);

  for k in 0..16u64 {
    let timeout = 5 + rng.below(50);
    let name = format!("svc{k:02}.cfg");
    let body = |t: u64| format!("name = svc{k:02}\ntimeout = {t}\nretries = 3\n");
    write(&proj.join("conf").join(&name), &body(timeout));
    let after = if timeout < 30 {
      a.doubled += 1;
      timeout * 2
    } else {
      timeout
    };
    a.configs.insert(name, body(after));
  }

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
  let mut by: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
  for (k, who) in [
    "Ada", "Ben", "Ada", "Cy", "Ada", "Ben", "Ada", "Cy", "Ben", "Ada", "Cy", "Ada", "Ben",
  ]
  .iter()
  .enumerate()
  {
    let lines = 3 + rng.below(40);
    write(
      &repo.join(format!("f{k}.txt")),
      &(0..lines).map(|n| format!("line {n}\n")).collect::<String>(),
    );
    git(&["add", "."]);
    let (id, mail) = (
      format!("user.name={who}"),
      format!("user.email={}@x.test", who.to_lowercase()),
    );
    git(&["-c", &id, "-c", &mail, "commit", "-q", "-m", &format!("add f{k}")]);
    let e = by.entry(who).or_default();
    e.0 += 1;
    e.1 += lines;
  }
  let (lead, (_, added)) = by.iter().max_by_key(|(_, (c, _))| *c).unwrap();
  (a.lead, a.lead_added) = (lead.to_string(), *added);

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
  write(&proj.join("events.json"), &serde_json::to_string(&events).unwrap());
  let (worst, n) = errors.iter().max_by_key(|(_, n)| **n).unwrap();
  (a.worst, a.worst_errors, a.error_users) = (worst.clone(), *n, errors.len() as u64);

  for (name, lines) in [("a.txt", 3), ("b.txt", 5), ("d.txt", 7)] {
    write(
      &proj.join(name),
      &(0..lines).map(|n| format!("note {n}\n")).collect::<String>(),
    );
  }
  a
}

/// A file too big to read loaded once into a global and summed; the next
/// question answered from it, the file not read again; many files edited
/// in one call; a command's output parsed; JSON in and out; failures
/// caught and the rest done; no result dumping a file whole.
pub fn jobs() {
  let mut answers = Answers::default();
  let mut l = Live::start("luau_jobs", &[], |d| answers = data(&d.proj()));
  let proj = l.dirs().proj();
  let p = proj.display();

  let t = l.say(
    "Acorn",
    "default",
    &format!(
      "{p}/sales.csv has a header and then one sale per line: date,region,product,qty,price. With your luau tool, load it into a global you keep for later questions, and tell me the North region's revenue (the sum of qty*price), as digits only, written as REV=<number>."
    ),
  );
  assert_eq!(
    number(&t.replies, "REV"),
    Some(answers.north),
    "North's revenue: {}",
    t.replies
  );
  assert!(
    t.luau().iter().any(|c| c.contains("sales.csv")),
    "luau loaded the file: {:?}",
    t.calls
  );
  assert!(!t.called("read", "sales.csv"), "paged through it with read");

  let t = l.say(
    "Acorn",
    "default",
    "From the sales you already loaded in luau (don't read the file again), which product sold the most units in all? Answer as PROD=<name> UNITS=<number>.",
  );
  assert_eq!(
    word(&t.replies, "PROD"),
    Some(answers.top.as_str()),
    "the top product: {}",
    t.replies
  );
  assert_eq!(number(&t.replies, "UNITS"), Some(answers.top_units), "its units");
  assert!(!t.luau().is_empty(), "answered without luau: {:?}", t.calls);
  assert!(
    !t.echoes.iter().any(|e| e.contains("[calls: ") && e.contains("read \""))
      && !t
        .calls
        .iter()
        .any(|c| c.starts_with("read {") || c.starts_with("bash {")),
    "read the file again rather than use the VM's global: {:?}",
    t.echoes
  );

  let t = l.say(
    "Acorn",
    "default",
    &format!(
      "In {p}/conf, each .cfg file has a line `timeout = N`. With one luau call, double the timeout in every file where it is under 30, changing nothing else in any file. Then tell me how many files you changed, as EDITS=<number>."
    ),
  );
  for (name, body) in &answers.configs {
    assert_eq!(
      &std::fs::read_to_string(proj.join("conf").join(name)).unwrap(),
      body,
      "{name}"
    );
  }
  assert_eq!(
    number(&t.replies, "EDITS"),
    Some(answers.doubled),
    "files changed: {}",
    t.replies
  );
  let n = t.luau().len();
  assert!((1..=3).contains(&n), "{n} luau calls for the edits");
  assert!(
    !t.calls
      .iter()
      .any(|c| c.starts_with("edit {") || c.starts_with("write {")),
    "edited with the native tools: {:?}",
    t.calls
  );

  let t = l.say(
    "Acorn",
    "default",
    &format!(
      "In the git repo {p}/repo, using luau with sh (git log --numstat), find the author with the most commits and the total number of lines they added. Answer as AUTHOR=<name> ADDED=<number>."
    ),
  );
  assert_eq!(
    word(&t.replies, "AUTHOR"),
    Some(answers.lead.as_str()),
    "the top author: {}",
    t.replies
  );
  assert_eq!(
    number(&t.replies, "ADDED"),
    Some(answers.lead_added),
    "lines they added"
  );
  assert!(
    t.luau().iter().any(|c| c.contains("sh(") && c.contains("numstat")),
    "git run from luau"
  );

  let t = l.say(
    "Acorn",
    "default",
    &format!(
      "{p}/events.json is a JSON array of events, each {{user, type, ms}}. With luau and its json functions, count the distinct users with at least one event of type \"error\", and find the user with the most errors. Write {p}/report.json as a JSON object {{\"error_users\": <count>, \"worst\": <user>, \"worst_errors\": <their error count>}}, then reply REPORT-DONE."
    ),
  );
  let report: serde_json::Value =
    serde_json::from_str(&std::fs::read_to_string(proj.join("report.json")).expect("report.json written")).unwrap();
  assert_eq!(report["error_users"], answers.error_users, "{report}");
  assert_eq!(report["worst"], answers.worst.as_str(), "{report}");
  assert_eq!(report["worst_errors"], answers.worst_errors, "{report}");
  assert!(
    t.luau().iter().any(|c| c.contains("json.")),
    "used luau's json: {:?}",
    t.calls
  );

  let t = l.say(
    "Acorn",
    "default",
    &format!(
      "In one luau call, count the lines of {p}/a.txt, {p}/b.txt, {p}/c.txt, {p}/d.txt and {p}/e.txt. Some may not exist: catch that with pcall and go on. Answer as TOTAL=<lines of those that exist> MISSING=<their file names, comma-separated, no spaces>."
    ),
  );
  assert_eq!(
    number(&t.replies, "TOTAL"),
    Some(15),
    "lines of the files there: {}",
    t.replies
  );
  let missing: BTreeSet<&str> = word(&t.replies, "MISSING")
    .unwrap_or_default()
    .split(',')
    .map(|name| name.trim_end_matches(".txt"))
    .collect();
  assert_eq!(missing, BTreeSet::from(["c", "e"]), "{}", t.replies);
  assert!(
    t.luau().iter().any(|c| c.contains("pcall")),
    "caught with pcall: {:?}",
    t.calls
  );

  let echoes = rows(
    &l.chat("default"),
    "select length(text) from msgs where log = '' and kind = 'echo'",
  );
  let biggest = echoes.iter().filter_map(|n| n.parse::<usize>().ok()).max().unwrap_or(0);
  assert!(biggest < 20_000, "a result of {biggest} bytes came back");
}

/// A square of one colour, only seen: `view` shows it, and the colour is
/// named.
pub fn image() {
  let colours = [
    ("red", [220, 30, 30]),
    ("green", [30, 170, 60]),
    ("blue", [30, 70, 220]),
  ];
  let (name, rgb) = colours[std::process::id() as usize % colours.len()];
  let mut l = Live::start("luau_image", &[], |d| {
    let mut img = image::RgbImage::from_pixel(400, 300, image::Rgb([255, 255, 255]));
    for x in 100..300 {
      for y in 50..250 {
        img.put_pixel(x, y, image::Rgb(rgb));
      }
    }
    img.save(d.proj().join("shape.png")).unwrap();
  });
  let shape = l.dirs().proj().join("shape.png");
  let t = l.say(
    "Acorn",
    "default",
    &format!(
      "Look at the picture {}. What colour is the square in it? Answer as COLOR=<one word>.",
      shape.display()
    ),
  );
  assert!(
    t.luau().iter().any(|c| c.contains("view(")),
    "looked without view: {:?}",
    t.calls
  );
  assert!(
    t.replies.to_lowercase().contains(&format!("color={name}")),
    "the square is {name}: {}",
    t.replies
  );
}

/// Plugins changed on a chat that goes on, Acorn never restarted: search
/// as shipped, then Exa's plugin dropped in, then TinyFish's for it, then
/// Acorn installing Exa's itself when asked.
pub fn plugins() {
  let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins");
  let mut l = Live::start("luau_plugins", &[], |d| {
    std::fs::create_dir_all(d.home().join("plugins")).unwrap()
  });
  let plugins = l.dirs().home().join("plugins");
  let ask = |l: &mut Live, tag: &str, what: &str| -> String {
    let t = l.say(
      "Acorn",
      "default",
      &format!("{what} Search with web_search in luau, once, and answer as {tag}=<the url you found>."),
    );
    assert!(t.replies.contains(&format!("{tag}=")), "{}", t.replies);
    t.echoes.join("\n")
  };

  let first = ask(&mut l, "FIRST", "Find the Rust language's home page.");
  assert!(
    !first.contains("http \""),
    "shipped search goes through no plugin: {first}"
  );

  std::fs::copy(shipped.join("exa.luau"), plugins.join("exa.luau")).unwrap();
  let exa = ask(&mut l, "SECOND", "Find the Python language's home page.");
  assert!(exa.contains("http \"https://api.exa.ai/search\""), "{exa}");
  assert!(
    exa.contains("plugins changed") && !exa.contains("did not load"),
    "{exa}"
  );

  std::fs::remove_file(plugins.join("exa.luau")).unwrap();
  std::fs::copy(shipped.join("tinyfish.luau"), plugins.join("tinyfish.luau")).unwrap();
  let fish = ask(&mut l, "THIRD", "Find the Go language's home page.");
  assert!(
    fish.contains("http \"https://api.search.tinyfish.ai\"") && !fish.contains("did not load"),
    "{fish}"
  );

  let asked = format!(
    "Use Exa instead of TinyFish for search from now on: the plugin is at {}, so remove TinyFish's and install that one. Then find the Zig language's home page.",
    shipped.join("exa.luau").display()
  );
  let done = ask(&mut l, "FOURTH", &asked);
  assert!(plugins.join("exa.luau").exists() && !plugins.join("tinyfish.luau").exists());
  assert!(
    done.contains("http \"https://api.exa.ai/search\"") && !done.contains("did not load"),
    "{done}"
  );
}
