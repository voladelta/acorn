//! Live
//! ====
//! Acorn on the real model: scenarios of what people do with it, each
//! checking what can be checked (files, the chat's db, tool calls, the
//! sidebar), never the model's exact wording but for markers it is asked
//! to write. The engines are one a process, so each scenario runs in a
//! process of its own, several at once (they wait on the model, not the
//! CPU).
//!
//! `cargo test -p acorn --test live -- --ignored [NAME…] [--jobs N]`, with
//! DEEPSEEK_API_KEY: every scenario (or those whose name holds a NAME),
//! N at a time (default 4, or $ACORN_LIVE_JOBS). Each one's transcript and
//! output go to $ACORN_LIVE_OUT (default: the temp dir's acorn-live).
//! Without `--ignored` they are listed, not run, as a plain `cargo test`
//! costs nothing.

mod kit;
mod luau;
mod memory;
mod people;
mod skills;
mod work;

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A scenario: its name, what it runs, the keys it needs beyond DeepSeek's.
pub struct Scenario {
  pub name: &'static str,
  pub run: fn(),
  pub needs: &'static [&'static str],
}

const fn scenario(name: &'static str, run: fn()) -> Scenario {
  Scenario { name, run, needs: &[] }
}

const SCENARIOS: &[Scenario] = &[
  scenario("reply", people::reply),
  scenario("cut_in", people::cut_in),
  scenario("ask_max", people::ask_max),
  scenario("assistant_lifecycle", people::assistant_lifecycle),
  scenario("vietnamese", people::vietnamese),
  scenario("purge", memory::purge),
  scenario("pin", memory::pin),
  scenario("facts", memory::facts),
  scenario("deep_memory", memory::deep_memory),
  scenario("tools", work::tools),
  scenario("delegate", work::delegate),
  scenario("web", work::web),
  scenario("luau_jobs", luau::jobs),
  scenario("luau_image", luau::image),
  Scenario {
    name: "luau_plugins",
    run: luau::plugins,
    needs: &["EXA_API_KEY", "TINYFISH_API_KEY"],
  },
  Scenario {
    name: "relevant",
    run: luau::relevant,
    needs: &["TYPESAFE_API_KEY"],
  },
  Scenario {
    name: "handbook",
    run: luau::handbook,
    needs: &["TYPESAFE_API_KEY"],
  },
  Scenario {
    name: "relevant_not_needed",
    run: luau::relevant_not_needed,
    needs: &["TYPESAFE_API_KEY"],
  },
  scenario("skills_scopes", skills::scopes),
  scenario("skills_kinds", skills::kinds),
  scenario("skills_agents", skills::agents),
  scenario("grows", skills::grows),
];

/// Set on a child: the one scenario it runs.
const ONE: &str = "ACORN_LIVE_ONE";

/// How long a scenario may run in all.
const LIMIT: Duration = Duration::from_mins(30);

fn main() {
  if let Ok(name) = std::env::var(ONE) {
    let s = SCENARIOS.iter().find(|s| s.name == name).expect("a scenario");
    (s.run)();
    return;
  }
  let mut args = std::env::args().skip(1);
  let (mut names, mut jobs, mut run, mut list) = (Vec::new(), None, false, false);
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--ignored" | "--include-ignored" => run = true,
      "--list" => list = true,
      "--jobs" => jobs = args.next().and_then(|n| n.parse().ok()),
      a if a.starts_with('-') => {}
      a => names.push(a.to_string()),
    }
  }
  let picked: Vec<&Scenario> = SCENARIOS
    .iter()
    .filter(|s| names.is_empty() || names.iter().any(|n| s.name.contains(n.as_str())))
    .collect();
  if list {
    for s in &picked {
      println!("{}: test", s.name);
    }
    return;
  }
  if !run {
    println!(
      "live: {} scenarios, not run (they cost money): cargo test -p acorn --test live -- --ignored",
      picked.len()
    );
    return;
  }
  if std::env::var_os("DEEPSEEK_API_KEY").is_none() {
    eprintln!("live: DEEPSEEK_API_KEY is not set");
    std::process::exit(1);
  }
  let jobs = jobs
    .or_else(|| std::env::var("ACORN_LIVE_JOBS").ok()?.parse().ok())
    .unwrap_or(4)
    .max(1);
  let out = kit::out();
  println!(
    "live: {} scenarios, {jobs} at a time; transcripts in {}",
    picked.len(),
    out.display()
  );
  let queue = Mutex::new(picked.into_iter().collect::<VecDeque<_>>());
  let failed = Mutex::new(Vec::new());
  let total = Mutex::new(0.0);
  let started = Instant::now();
  std::thread::scope(|scope| {
    for _ in 0..jobs {
      scope.spawn(|| {
        while let Some(s) = queue.lock().unwrap().pop_front() {
          let (status, spent) = child(s);
          *total.lock().unwrap() += spent;
          if status.starts_with("FAIL") {
            failed.lock().unwrap().push(s.name);
          }
          let mut out = std::io::stdout().lock();
          let _ = writeln!(out, "{status}  ${spent:.4}");
        }
      });
    }
  });
  let failed = failed.into_inner().unwrap();
  println!(
    "live: {} failed, in {} s, ${:.4} in all{}",
    failed.len(),
    started.elapsed().as_secs(),
    total.into_inner().unwrap(),
    if failed.is_empty() {
      String::new()
    } else {
      format!(": {}", failed.join(", "))
    }
  );
  if !failed.is_empty() {
    std::process::exit(1);
  }
}

/// A scenario run in a process of its own: how it went, said in a line,
/// and what it cost.
fn child(s: &Scenario) -> (String, f64) {
  if let Some(key) = s.needs.iter().find(|k| std::env::var_os(k).is_none()) {
    return (format!("skip  {:<20} (needs {key})", s.name), 0.0);
  }
  let log = kit::out().join(format!("{}.log", s.name));
  let file = std::fs::File::create(&log).unwrap();
  let started = Instant::now();
  let mut proc = Command::new(std::env::current_exe().unwrap())
    .env(ONE, s.name)
    .stdout(Stdio::from(file.try_clone().unwrap()))
    .stderr(Stdio::from(file))
    .spawn()
    .unwrap();
  let ok = loop {
    if let Some(done) = proc.try_wait().unwrap() {
      break Some(done.success());
    }
    if started.elapsed() > LIMIT {
      let _ = proc.kill();
      let _ = proc.wait();
      break None;
    }
    std::thread::sleep(Duration::from_millis(500));
  };
  let text = std::fs::read_to_string(&log).unwrap_or_default();
  let spent = text
    .lines()
    .rev()
    .find_map(|l| l.strip_prefix("spent: ")?.parse().ok())
    .unwrap_or(0.0);
  let secs = started.elapsed().as_secs();
  let status = match ok {
    Some(true) => format!("ok    {:<20} {secs:>5} s", s.name),
    Some(false) => {
      let why = text
        .lines()
        .skip_while(|l| !l.contains("panicked at"))
        .nth(1)
        .unwrap_or("(no panic message)");
      format!("FAIL  {:<20} {secs:>5} s  {why}  [{}]", s.name, log.display())
    }
    None => format!("FAIL  {:<20} {secs:>5} s  timed out  [{}]", s.name, log.display()),
  };
  (status, spent)
}
