//! Work
//! ====
//! Acorn at work on the machine: its tools on files and commands, agents
//! hired in parallel, the web.

use crate::kit::*;

/// The tools luau hides: the machine's and the web's own.
fn hidden(call: &str) -> bool {
  ["bash", "read", "write", "edit", "web_search", "web_fetch"]
    .iter()
    .any(|name| call.starts_with(&format!("{name} {{")))
}

/// The tools, through luau: an exit code reported, not raised; a file read
/// before it is edited, only the edit landing; a change made outside since
/// kept; CRLF kept; a script written and run; a timeout that kills and
/// says so; no hidden tool called.
pub fn tools() {
  let mut l = Live::start("tools", &[], |d| {
    write(
      &d.proj().join("config.toml"),
      "name = \"demo\"\nport = 8080\ndebug = false\n",
    );
    write(&d.proj().join("notes.txt"), "alpha\r\nbeta\r\n");
  });
  let proj = l.dirs().proj();
  let p = proj.display();
  let config = proj.join("config.toml");

  let t = l.say(
    "Acorn",
    "default",
    &format!(
      "In {p}, run exactly this command and nothing else (add nothing to it, no echo): `test -f missing.txt`. Tell me its exit code, written as EXIT=<code>."
    ),
  );
  assert!(t.replies.contains("EXIT=1"), "{}", t.replies);
  assert!(
    t.luau()
      .iter()
      .any(|c| c.contains("sh(") && c.contains("test -f missing.txt")),
    "{:?}",
    t.calls
  );

  let t = l.say(
    "Acorn",
    "default",
    &format!("In {p}/config.toml set the port to 9090. Change nothing else, then reply DONE-PORT."),
  );
  assert_eq!(
    std::fs::read_to_string(&config).unwrap(),
    "name = \"demo\"\nport = 9090\ndebug = false\n"
  );
  let luau = t.luau();
  let first = luau
    .iter()
    .position(|c| (c.contains("edit(") || c.contains("write(")) && c.contains("config.toml"))
    .expect("an edit of config.toml");
  let changing = luau[first];
  let read_first = luau[..first]
    .iter()
    .any(|c| c.contains("read(") && c.contains("config.toml"))
    || changing.find("read(").is_some_and(|r| {
      changing
        .find("edit(")
        .or(changing.find("write("))
        .is_some_and(|e| r < e)
    });
  assert!(
    read_first || t.echoes.iter().any(|e| e.contains("file has not been read")),
    "edited without reading, and nothing stopped it"
  );

  write(&config, "name = \"demo\"\nport = 9090\ndebug = true\n");
  l.say(
    "Acorn",
    "default",
    &format!("In {p}/config.toml set the name to \"acorn\". Change nothing else, then reply DONE-NAME."),
  );
  assert_eq!(
    std::fs::read_to_string(&config).unwrap(),
    "name = \"acorn\"\nport = 9090\ndebug = true\n",
    "the change made outside was lost"
  );

  let t = l.say(
    "Acorn",
    "default",
    &format!("In {p}/notes.txt replace the word beta with gamma, using luau's edit (not sh), then reply DONE-CRLF."),
  );
  assert_eq!(
    std::fs::read_to_string(proj.join("notes.txt")).unwrap(),
    "alpha\r\ngamma\r\n"
  );
  assert!(t.luau().iter().any(|c| c.contains("edit(") && c.contains("notes.txt")));

  let t = l.say(
    "Acorn",
    "default",
    &format!(
      "Create {p}/hello.sh, a script that prints HELLO- followed by the result of $((6*7)). Run it with bash and tell me the line it printed."
    ),
  );
  assert!(t.replies.contains("HELLO-42"), "{}", t.replies);
  assert!(std::fs::read_to_string(proj.join("hello.sh")).unwrap().contains("6*7"));

  let t = l.say(
    "Acorn",
    "default",
    "Run `sleep 30; echo woke` with a timeout of 2 seconds. Tell me what happened, and if it timed out end your reply with TIMEOUT-SEEN.",
  );
  assert!(t.replies.contains("TIMEOUT-SEEN"), "{}", t.replies);
  assert!(t.luau().iter().any(|c| c.contains("sleep 30") && c.contains("timeout")));

  let calls = l.calls("default");
  assert!(!calls.iter().any(|c| hidden(c)), "a hidden tool was called: {calls:?}");
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

/// Two agents hired in parallel over a CSV, told where it is; both reports
/// in, the answers right; the totals saved after.
pub fn delegate() {
  let mut l = Live::start("delegate", &[], |d| write(&d.work().join("sales.csv"), SALES));
  let t = l.say_till_reports(
    "Acorn",
    "default",
    "sales.csv is in your work dir. Hire two agents in parallel: one totals revenue (units × price) by region, the other finds the product with the most total revenue. Tell me both answers once they've both reported.",
    2,
  );
  let hired: Vec<&String> = t.calls.iter().filter(|c| c.starts_with("delegate {")).collect();
  assert!(!hired.is_empty(), "agents hired: {:?}", t.calls);
  assert!(
    hired.iter().any(|c| c.contains("sales.csv")),
    "the agents told where the file is: {hired:?}"
  );
  assert!(
    t.replies.contains("100") && t.replies.contains("Gadget") && t.replies.contains("110"),
    "East 100, Gadget 110: {}",
    t.replies
  );
  l.say(
    "Acorn",
    "default",
    "Save the region totals as totals.csv next to sales.csv, columns region,revenue.",
  );
  let totals = std::fs::read_to_string(l.dirs().work().join("totals.csv")).expect("totals.csv saved");
  assert!(
    totals.contains("East") && totals.contains("100") && totals.contains("North") && totals.contains("65"),
    "{totals}"
  );
}

/// The web searched and a page fetched, citing where it read.
pub fn web() {
  let mut l = Live::start("web", &[], |_| {});
  let t = l.say(
    "Acorn",
    "default",
    "Search the web: what is the latest stable release of Python, and when was it released? Cite the URL you read.",
  );
  assert!(
    t.reply.contains("3.") && t.reply.contains("http"),
    "a version, cited: {}",
    t.reply
  );
  let t = l.say(
    "Acorn",
    "default",
    "Fetch https://www.rust-lang.org/ and tell me its main headline, word for word.",
  );
  let h = t.reply.to_lowercase();
  assert!(
    ["reliable", "efficient", "empowering"].iter().any(|w| h.contains(w)),
    "{}",
    t.reply
  );
}
