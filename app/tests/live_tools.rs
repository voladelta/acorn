//! The Mac's tools (bash, read, write, edit, as deepseek-harness has them),
//! live, as luau's code calls them (sh, read, write, edit): Acorn does each
//! job by words alone, and the files on disk and the luau calls in the
//! chat's db show it did them with these, as they work.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
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

/// A call of a tool luau hides: the Mac's and the web's own.
fn hidden(call: &str) -> bool {
  ["bash", "read", "write", "edit", "web_search", "web_fetch"]
    .iter()
    .any(|name| call.starts_with(&format!("{name} {{")))
}

/// Acorn's luau calls' code, in order, one per line.
fn luau(db: &Path) -> Vec<String> {
  tool_calls(db).into_iter().filter(|c| c.starts_with("luau {")).collect()
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
      let all = "select i, kind, replace(substr(text, 1, 400), char(10), ' / ') from msgs where log = '' order by i;";
      println!("---- chat log at failure ----\n{}", sql(&self.0, all));
    }
  }
}

/// Asks the model; run with `cargo test -- --ignored` and DEEPSEEK_API_KEY.
#[test]
#[ignore]
fn the_harness_tools_work_live() {
  let dir = tempfile::tempdir().unwrap();
  let proj = dir.path().join("proj");
  std::fs::create_dir_all(&proj).unwrap();
  let config = proj.join("config.toml");
  std::fs::write(&config, "name = \"demo\"\nport = 8080\ndebug = false\n").unwrap();
  let notes = proj.join("notes.txt");
  std::fs::write(&notes, "alpha\r\nbeta\r\n").unwrap();
  let db = dir.path().join("chats/default/chat.db");
  let _dump = Dump(db.clone());

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

  // Sends a message and waits until Acorn's reply after it says `word`,
  // then until its turn has ended.
  let ask = |app: &mut Headless, what: &str, word: &str| {
    let before = sql(&db, "select coalesce(max(i), -1) from msgs where log = '';")
      .trim()
      .to_string();
    app.type_text("label:Message Acorn", what).unwrap();
    app.submit("label:Message Acorn").unwrap();
    let said =
      format!("select count(*) from msgs where log = '' and kind = 'unii' and i > {before} and text like '%{word}%';");
    app
      .wait_until(word, Duration::from_secs(300), |_| sql(&db, &said).trim() != "0")
      .unwrap();
    std::thread::sleep(Duration::from_secs(10)); // let the turn end before the next message cuts in
  };
  let p = proj.display();

  // A. sh: an exit code is reported, not raised
  ask(
    &mut app,
    &format!(
      "In {p}, run exactly this command and nothing else (add nothing to it, no echo): `test -f missing.txt`. Tell me its exit code, written as EXIT=<code>."
    ),
    "EXIT=1",
  );
  assert!(
    luau(&db)
      .iter()
      .any(|c| c.contains("sh(") && c.contains("test -f missing.txt")),
    "{:?}",
    tool_calls(&db)
  );
  assert!(
    !tool_calls(&db).iter().any(|c| hidden(c)),
    "a hidden tool was called: {:?}",
    tool_calls(&db)
  );

  // B. edit: a file is read before it is edited, and only the edit lands
  ask(
    &mut app,
    &format!("In {p}/config.toml set the port to 9090. Change nothing else, then reply DONE-PORT."),
    "DONE-PORT",
  );
  assert_eq!(
    std::fs::read_to_string(&config).unwrap(),
    "name = \"demo\"\nport = 9090\ndebug = false\n"
  );
  let calls = luau(&db);
  println!("B: calls {calls:#?}");
  let first_change = calls
    .iter()
    .position(|c| (c.contains("edit(") || c.contains("write(")) && c.contains("config.toml"))
    .expect("an edit of config.toml");
  let changing = &calls[first_change];
  let read_first = calls[..first_change]
    .iter()
    .any(|c| c.contains("read(") && c.contains("config.toml"))
    || changing.find("read(").is_some_and(|r| {
      changing
        .find("edit(")
        .or(changing.find("write("))
        .is_some_and(|e| r < e)
    });
  assert!(
    read_first || echoes(&db).iter().any(|e| e.contains("file has not been read")),
    "edited without reading, and nothing stopped it"
  );

  // C. a file changed outside since it was read: the next edit keeps that change
  std::thread::sleep(Duration::from_millis(50));
  std::fs::write(&config, "name = \"demo\"\nport = 9090\ndebug = true\n").unwrap();
  ask(
    &mut app,
    &format!("In {p}/config.toml set the name to \"acorn\". Change nothing else, then reply DONE-NAME."),
    "DONE-NAME",
  );
  assert_eq!(
    std::fs::read_to_string(&config).unwrap(),
    "name = \"acorn\"\nport = 9090\ndebug = true\n",
    "the change made outside was lost"
  );

  // D. a CRLF file keeps its line endings through an edit
  ask(
    &mut app,
    &format!("In {p}/notes.txt replace the word beta with gamma, using luau's edit (not sh), then reply DONE-CRLF."),
    "DONE-CRLF",
  );
  assert_eq!(std::fs::read_to_string(&notes).unwrap(), "alpha\r\ngamma\r\n");
  assert!(luau(&db).iter().any(|c| c.contains("edit(") && c.contains("notes.txt")));

  // E. write, then run what was written
  ask(
    &mut app,
    &format!(
      "Create {p}/hello.sh, a script that prints HELLO- followed by the result of $((6*7)). Run it with bash and tell me the line it printed."
    ),
    "HELLO-42",
  );
  assert!(std::fs::read_to_string(proj.join("hello.sh")).unwrap().contains("6*7"));
  assert!(luau(&db).iter().any(|c| c.contains("write(") && c.contains("hello.sh")));

  // F. long output keeps its tail and says where the rest went
  ask(
    &mut app,
    "Run exactly `seq 1 300000`, with nothing piped or added to it, and tell me only the last number it printed, written as LAST=<number>.",
    "LAST=300000",
  );
  assert!(
    luau(&db)
      .iter()
      .any(|c| c.contains("sh(") && c.contains("seq 1 300000"))
  );

  // G. a timeout the call asks for kills the command and says so
  ask(
    &mut app,
    "Run `sleep 30; echo woke` with a timeout of 2 seconds. Tell me what happened, and if it timed out end your reply with TIMEOUT-SEEN.",
    "TIMEOUT-SEEN",
  );
  assert!(
    luau(&db)
      .iter()
      .any(|c| c.contains("sleep 30") && c.contains("timeout"))
  );
  assert!(
    !tool_calls(&db).iter().any(|c| hidden(c)),
    "a hidden tool was called: {:?}",
    tool_calls(&db)
  );
}
