//! Machine
//! =======
//! The tools that touch this Mac: bash, read, write, edit (`tools`), each in
//! its caller's dir under the work dir. They are DeepSeek's harness's
//! (deepseek-harness: tool-bash, tool-fs, fs-local, fs-observation-policy),
//! word for word, as DeepSeek's models were trained on them: their names,
//! arguments, descriptions, results and errors; the harness's foreground
//! bash (no background jobs) on a Mac with no file sandbox.
//!
//! A caller may write over or edit a file only as it last read, wrote or
//! edited it (`SEEN`): one it never read, or that changed since, is refused
//! until it reads it again. A stop kills the job's shell and its process
//! group. As Acorn quits, `end` kills every shell's group, and any process
//! marked ACORN_RUN that left it.

use crate::ai::{Out, Spec};
use crate::config;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio_util::sync::CancellationToken;

/// bash's timeout: by default, and the most a call may ask.
const TIMEOUT_MS: f64 = 120_000.0;
const MAX_TIMEOUT_MS: f64 = 600_000.0;
/// From SIGTERM to SIGKILL, as a command times out.
const GRACE: Duration = Duration::from_secs(3);
/// The bytes of each of a command's streams kept: their tail.
const OUTPUT_BYTES: usize = 64_000;
/// The most of a stream its spill file keeps.
const SPILL_BYTES: u64 = 64 * 1024 * 1024;
/// What every command's environment overrides.
const ENV: [(&str, &str); 4] = [
  ("NO_COLOR", "1"),
  ("TERM", "dumb"),
  ("PAGER", "cat"),
  ("GIT_PAGER", "cat"),
];

/// read's caps: lines by default and at most, characters a line (UTF-16
/// units, as JavaScript counts them), bytes of the lines shown.
const READ_LIMIT: usize = 2000;
const READ_LINE_CHARS: usize = 2000;
const READ_BYTES: usize = 50 * 1024;
/// The bytes at a file's start in which a NUL makes it binary.
const BINARY_SAMPLE: usize = 8192;

/// This Acorn's mark, set as ACORN_RUN on every process its shells start
/// (each child inherits it), to find those that left their shell's group.
static RUN: LazyLock<String> = LazyLock::new(|| format!("{:016x}", rand::random::<u64>()));

/// The shells started, by process group, with whether each exited, kept
/// until their groups are empty, so `end` kills a backgrounded job whose
/// shell already exited.
static SHELLS: LazyLock<Mutex<HashMap<i32, Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// What each caller (by its dir) last saw of each file (by its real path).
static SEEN: LazyLock<Mutex<HashMap<(PathBuf, PathBuf), Seen>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Held while a write or edit checks and changes a file.
static WRITING: Mutex<()> = Mutex::new(());

/// A file as a caller saw it: there, at a version, or not there.
#[derive(Clone, PartialEq)]
enum Seen {
  Present(String),
  Absent,
}

pub fn spec(name: &'static str, info: impl Into<String>, fields: Value, need: &[&str]) -> Spec {
  Spec {
    name,
    info: info.into(),
    schema: json!({"type": "object", "properties": fields, "required": need}),
  }
}

pub fn tools() -> Vec<Spec> {
  vec![
    spec(
      "bash",
      "Execute a bash command (`bash -c`) and return its stdout/stderr. \
Each call runs in a fresh shell; pass `workdir` instead of using `cd`. \
Long output is truncated to its tail; the full output is saved to a file whose path is reported when available. \
Provide `description` before `command` in the arguments. \
Before any delete or move, verify that the resolved absolute target path is the intended one; never run it against a computed path you have not checked. \
An unset variable expands to an empty string, so guard variables in such paths with `${VAR:?}`. \
Commands may run under a file sandbox; a blocked file operation is reported as `[sandbox: file access denied under <mode> mode]`, a policy denial: do not retry another way.",
      json!({
        "description": {"type": "string", "description": "Clear, concise description of what this command does in active voice, 5-10 words (shown in the UI). Examples: \"ls\" → \"List files in current directory\"; \"git status\" → \"Show working tree status\"; \"npm install\" → \"Install package dependencies\"."},
        "command": {"type": "string", "description": "The bash command to execute."},
        "timeoutMs": {"type": "number", "description": "Timeout in milliseconds. The executor applies its configured default and cap, and kills the command on expiry."},
        "workdir": {"type": "string", "description": "Working directory for this command. Defaults to the session workspace; a relative path is resolved against it."},
      }),
      &["description", "command"],
    ),
    spec(
      "read",
      "Read a UTF-8 text file and return line-numbered content.",
      json!({
        "file_path": {"type": "string", "description": "Path to read, resolved by the filesystem backend."},
        "offset": {"type": "number", "description": "1-based first line to return. Defaults to 1."},
        "limit": {"type": "number", "description": format!("Maximum number of lines to return. Defaults to {READ_LIMIT}.")},
      }),
      &["file_path"],
    ),
    spec(
      "write",
      "Create or fully replace a UTF-8 text file.",
      json!({
        "file_path": {"type": "string", "description": "Path to write, resolved by the filesystem backend. Provide `file_path` before `content` in the arguments."},
        "content": {"type": "string", "description": "Full UTF-8 text content to write."},
      }),
      &["file_path", "content"],
    ),
    spec(
      "edit",
      "Edit an existing UTF-8 text file by replacing literal text.",
      json!({
        "file_path": {"type": "string", "description": "Path to edit, resolved by the filesystem backend. Provide `file_path` before `old_string` and `new_string` in the arguments."},
        "old_string": {"type": "string", "description": "Literal text to replace."},
        "new_string": {"type": "string", "description": "Literal replacement text. Use an empty string to delete the match."},
        "replace_all": {"type": "boolean", "description": "Replace all matches. Defaults to false; when false, old_string must appear exactly once."},
      }),
      &["file_path", "old_string", "new_string"],
    ),
  ]
}

/// What the system prompt says of the tools, as the harness's sections do.
pub const GUIDE: &str = "Check the [exit code: N] marker on every bash result; investigate failures before moving on.

Use the read tool — not shell commands like cat — to inspect text files. Use offset and limit to continue reading large files.

Read an existing file before overwriting it with write (the default fs-observation-policy requires it) and prefer edit for targeted changes.

Read a file before editing it (the default fs-observation-policy requires it), unless you just created or edited it in this session.";

/// A tool of `tools`, run in the dir of its caller (`cwd`, under its
/// assistant's work dir), which is also whose reads count.
pub async fn run(name: &str, input: &Value, cwd: PathBuf, cancel: CancellationToken) -> Out {
  if let Err(why) = check(name, input) {
    return Out::fail(why);
  }
  if let Err(err) = fs::create_dir_all(&cwd) {
    return Out::fail(err.to_string());
  }
  let done = match name {
    "bash" => return bash(&cwd, input, cancel).await,
    "read" => read(&cwd, input),
    "write" => write(&cwd, input),
    "edit" => edit(&cwd, input),
    _ => return Out::fail(format!("No tool {name}.")),
  };
  done.map_or_else(Out::fail, Out::ok)
}

/// A call's input against its tool's schema, as the harness checks it
/// before running anything: each required field there, then each field's
/// type.
fn check(name: &str, input: &Value) -> Result<(), String> {
  let Some(tool) = tools().into_iter().find(|t| t.name == name) else {
    return Ok(());
  };
  let Some(given) = input.as_object() else {
    return Err("invalid arguments: \"arguments\" must be an object".into());
  };
  let mut wrong: Vec<String> = tool.schema["required"]
    .as_array()
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .filter(|k| !given.contains_key(*k))
    .map(|k| format!("missing required property \"{k}\""))
    .collect();
  for (k, field) in tool.schema["properties"].as_object().into_iter().flatten() {
    let Some(v) = given.get(k) else { continue };
    let kind = field["type"].as_str().unwrap_or_default();
    let fits = match kind {
      "string" => v.is_string(),
      "number" => v.is_number(),
      "boolean" => v.is_boolean(),
      _ => true,
    };
    if !fits {
      wrong.push(format!("\"{k}\" must be a {kind}"));
    }
  }
  if wrong.is_empty() {
    Ok(())
  } else {
    Err(format!("invalid arguments: {}", wrong.join("; ")))
  }
}

// Bash
// ====

/// Runs a command with `bash -c` in its own process group, every process
/// marked ACORN_RUN, its streams kept apart. On its timeout it gets SIGTERM,
/// then SIGKILL after GRACE. It ends when the shell has exited and its
/// streams have closed, or GRACE after the shell exited, whatever still
/// holds them (a job left running). A stop kills the group and ends the
/// wait at once.
async fn bash(cwd: &Path, input: &Value, cancel: CancellationToken) -> Out {
  let text = |k: &str| input[k].as_str().unwrap_or_default();
  let (command, description) = (text("command"), text("description"));
  if command.trim().is_empty() {
    return Out::fail("invalid command: expected a non-empty string");
  }
  if description.trim().is_empty() {
    return Out::fail("invalid description: expected a non-empty string");
  }
  let asked = &input["timeoutMs"];
  let timeout = match asked.as_f64() {
    None if asked.is_null() => TIMEOUT_MS,
    Some(ms) if ms.is_finite() && ms > 0.0 => ms.min(MAX_TIMEOUT_MS),
    _ => return Out::fail(format!("invalid timeoutMs: expected a positive number, got {asked}")),
  };
  let workdir = match input["workdir"].as_str() {
    Some(dir) => cwd.join(dir),
    None => cwd.to_path_buf(),
  };
  SHELLS.lock().retain(|&pid, exited| group(pid, exited));
  let mut cmd = tokio::process::Command::new("bash");
  cmd
    .arg("-c")
    .arg(command)
    .current_dir(&workdir)
    .envs(ENV)
    .env("ACORN_RUN", &*RUN)
    .env_remove(config::KEY_VAR)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .process_group(0);
  let mut child = match cmd.spawn() {
    Ok(child) => child,
    Err(err) => return Out::fail(err.to_string()),
  };
  let pid = child.id().map_or(0, |p| p as i32);
  let exited = Arc::new(AtomicBool::new(false));
  SHELLS.lock().insert(pid, exited.clone());
  let (out, err) = (
    Arc::new(Mutex::new(Stream::default())),
    Arc::new(Mutex::new(Stream::default())),
  );
  let (stdout, stderr) = (
    child.stdout.take().expect("its output, piped"),
    child.stderr.take().expect("its errors, piped"),
  );
  let streams = async { tokio::join!(collect(stdout, &out), collect(stderr, &err)) };
  tokio::pin!(streams);
  let wait = child.wait();
  tokio::pin!(wait);
  let expiry = tokio::time::sleep(Duration::from_secs_f64(timeout / 1000.0));
  tokio::pin!(expiry);
  let drain = tokio::time::sleep(Duration::MAX / 4);
  tokio::pin!(drain);
  let (mut timed_out, mut closed, mut status) = (false, false, None);
  while !(closed && status.is_some()) {
    tokio::select! {
      _ = &mut streams, if !closed => closed = true,
      got = &mut wait, if status.is_none() => {
        exited.store(true, Ordering::Relaxed);
        match got {
          Ok(got) => status = Some(got),
          Err(why) => return Out::fail(why.to_string()),
        }
        drain.as_mut().reset(tokio::time::Instant::now() + GRACE);
      }
      () = &mut drain, if status.is_some() => closed = true,
      () = &mut expiry, if !timed_out => {
        timed_out = true;
        signal(-pid, libc::SIGTERM);
        tokio::spawn(async move {
          tokio::time::sleep(GRACE).await;
          signal(-pid, libc::SIGKILL);
        });
      }
      () = cancel.cancelled() => {
        signal(-pid, libc::SIGKILL);
        if status.is_none() {
          let _ = wait.await;
          exited.store(true, Ordering::Relaxed);
        }
        return Out::fail("tool call aborted");
      }
    }
  }
  let status = status.expect("the loop ends once the shell exited");
  Out::ok(render(&out.lock(), &err.lock(), timed_out, timeout, &status))
}

/// One stream of a command: the tail of it kept, and the whole of it,
/// once past the tail, in a spill file, while the file is under
/// SPILL_BYTES (past that it is deleted).
#[derive(Default)]
struct Stream {
  tail: Vec<u8>,
  truncated: bool,
  spill: Option<(PathBuf, fs::File, u64)>,
}

/// Reads a stream into `s` as it comes, so what came is kept even if the
/// read is given up.
async fn collect(mut from: impl AsyncRead + Unpin, s: &Mutex<Stream>) {
  let mut buf = vec![0u8; 1 << 16];
  loop {
    let n = match from.read(&mut buf).await {
      Ok(0) | Err(_) => break,
      Ok(n) => n,
    };
    let got = &buf[..n];
    let mut s = s.lock();
    if let Some((path, f, kept)) = &mut s.spill {
      *kept += n as u64;
      if *kept > SPILL_BYTES || f.write_all(got).is_err() {
        let _ = fs::remove_file(&path);
        s.spill = None;
      }
    }
    s.tail.extend_from_slice(got);
    if s.tail.len() > OUTPUT_BYTES {
      if !s.truncated {
        s.truncated = true;
        s.spill = spill_file(&s.tail);
      }
      let cut = s.tail.len() - OUTPUT_BYTES;
      s.tail.drain(..cut);
    }
  }
}

/// A new spill file, holding a stream so far.
fn spill_file(so_far: &[u8]) -> Option<(PathBuf, fs::File, u64)> {
  let dir = std::env::temp_dir().join("acorn-spill");
  fs::create_dir_all(&dir).ok()?;
  let path = dir.join(format!("{:016x}.log", rand::random::<u64>()));
  let mut f = fs::File::create(&path).ok()?;
  f.write_all(so_far).ok()?;
  Some((path, f, so_far.len() as u64))
}

/// A stream's text: its tail, then where the whole of it went.
fn stream_text(s: &Stream) -> String {
  let text = String::from_utf8_lossy(&s.tail).into_owned();
  if !s.truncated {
    return text;
  }
  let spill = s
    .spill
    .as_ref()
    .map_or_else(|| "(unavailable)".to_string(), |(p, ..)| p.display().to_string());
  format!("{text}\n[output truncated; full output: {spill}]")
}

/// A finished command as the model reads it: stdout, a marked stderr
/// section, then its markers, the exit last. A nonzero exit is reported,
/// not failed.
fn render(out: &Stream, err: &Stream, timed_out: bool, timeout: f64, status: &std::process::ExitStatus) -> String {
  let mut body = stream_text(out);
  let err = stream_text(err);
  if !err.is_empty() {
    if !body.is_empty() && !body.ends_with('\n') {
      body.push('\n');
    }
    body.push_str(&format!("[stderr]\n{err}"));
  }
  if body.is_empty() {
    body = "(no output)".into();
  }
  let mut marks = Vec::new();
  if timed_out {
    marks.push(format!("[timed out after {timeout}ms]"));
  }
  match status.signal() {
    Some(sig) => marks.push(format!("[killed by signal: {}]", signal_name(sig))),
    None if status.code() != Some(0) => marks.push(format!("[exit code: {}]", status.code().unwrap_or(-1))),
    None => {}
  }
  if marks.is_empty() {
    return body;
  }
  if !body.ends_with('\n') {
    body.push('\n');
  }
  body + &marks.join("\n")
}

/// A signal's name on macOS, as Node reports it.
fn signal_name(sig: i32) -> String {
  const NAMES: [&str; 31] = [
    "SIGHUP",
    "SIGINT",
    "SIGQUIT",
    "SIGILL",
    "SIGTRAP",
    "SIGABRT",
    "SIGEMT",
    "SIGFPE",
    "SIGKILL",
    "SIGBUS",
    "SIGSEGV",
    "SIGSYS",
    "SIGPIPE",
    "SIGALRM",
    "SIGTERM",
    "SIGURG",
    "SIGSTOP",
    "SIGTSTP",
    "SIGCONT",
    "SIGCHLD",
    "SIGTTIN",
    "SIGTTOU",
    "SIGIO",
    "SIGXCPU",
    "SIGXFSZ",
    "SIGVTALRM",
    "SIGPROF",
    "SIGWINCH",
    "SIGINFO",
    "SIGUSR1",
    "SIGUSR2",
  ];
  usize::try_from(sig - 1)
    .ok()
    .and_then(|k| NAMES.get(k))
    .map_or_else(|| format!("signal {sig}"), |name| (*name).to_string())
}

/// Whether a shell's process group may still hold a job: the shell runs,
/// or it is gone but its id names no live process and its group still has
/// one.
fn group(pid: i32, exited: &AtomicBool) -> bool {
  !exited.load(Ordering::Relaxed) || (!alive(pid) && alive(-pid))
}

/// Whether a process (pid) or a process group (-pid) exists.
fn alive(pid: i32) -> bool {
  // SAFETY: kill with signal 0 sends nothing; it only checks.
  unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

fn signal(pid: i32, sig: i32) {
  if pid != 0 && pid != -1 {
    // SAFETY: a signal to a process or group this Acorn started.
    unsafe { libc::kill(pid, sig) };
  }
}

/// As Acorn quits: kills every shell's process group (a job still in it
/// dies), then any process that left its group (ACORN_RUN), so none is
/// left orphaned.
pub fn end() {
  for (pid, exited) in SHELLS.lock().iter() {
    if group(*pid, exited) {
      signal(-pid, libc::SIGKILL);
    }
  }
  // SAFETY: getuid has no preconditions.
  let uid = unsafe { libc::getuid() }.to_string();
  let Ok(ps) = std::process::Command::new("ps")
    .args(["-wwE", "-U", &uid, "-o", "pid=,command="])
    .output()
  else {
    return;
  };
  let mark = format!("ACORN_RUN={}", *RUN);
  for line in String::from_utf8_lossy(&ps.stdout)
    .lines()
    .filter(|l| l.contains(&mark))
  {
    if let Some(pid) = line.split_whitespace().next().and_then(|p| p.parse().ok()) {
      signal(pid, libc::SIGKILL);
    }
  }
}

// Files
// =====

/// A file named by a call: as shown (absolute, from the caller's dir) and
/// by its real path, which its parts not there yet extend.
struct Target {
  shown: String,
  key: PathBuf,
}

fn target(cwd: &Path, path: &str) -> Result<Target, String> {
  if path.trim().is_empty() {
    return Err("file_path must be a non-empty string".into());
  }
  let home = || config::home().display().to_string();
  let path = match path.strip_prefix('~') {
    Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home()),
    _ => path.to_string(),
  };
  let raw = cwd.join(path);
  let traverses = raw.components().any(|c| c == Component::ParentDir);
  let shown = if traverses {
    raw
  } else {
    raw.components().filter(|c| *c != Component::CurDir).collect()
  };
  let shown_str = shown.display().to_string();
  let not_dir = || format!("cannot resolve \"{shown_str}\": a parent path segment is not a directory");
  let mut missing = Vec::new();
  let mut at = shown.as_path();
  loop {
    match fs::canonicalize(at) {
      Ok(real) => {
        let key = missing.iter().rev().fold(real, |p, m| p.join(m));
        return Ok(Target { shown: shown_str, key });
      }
      Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => return Err(not_dir()),
      Err(_) => match (at.parent(), at.file_name()) {
        (Some(up), Some(name)) => {
          missing.push(name);
          at = up;
        }
        _ => {
          return Err(format!(
            "cannot resolve \"{shown_str}\": parent traversal crosses a missing directory"
          ));
        }
      },
    }
  }
}

/// A file's version: its device, inode, size and times.
fn version(m: &fs::Metadata) -> String {
  let ns = |s: i64, n: i64| i128::from(s) * 1_000_000_000 + i128::from(n);
  format!(
    "{}:{}:{}:{}:{}",
    m.dev(),
    m.ino(),
    m.size(),
    ns(m.mtime(), m.mtime_nsec()),
    ns(m.ctime(), m.ctime_nsec())
  )
}

/// What is at a path: none, or its metadata (following links).
fn probe(key: &Path) -> Result<Option<fs::Metadata>, String> {
  match fs::metadata(key) {
    Ok(m) => Ok(Some(m)),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) => Ok(None),
    Err(e) => Err(e.to_string()),
  }
}

fn seen(cwd: &Path, key: &Path) -> Option<Seen> {
  SEEN.lock().get(&(cwd.to_path_buf(), key.to_path_buf())).cloned()
}

fn see(cwd: &Path, key: &Path, s: Seen) {
  SEEN.lock().insert((cwd.to_path_buf(), key.to_path_buf()), s);
}

/// A number argument that must be a positive integer, if given.
fn positive(input: &Value, k: &str) -> Result<Option<usize>, String> {
  match &input[k] {
    Value::Null => Ok(None),
    v => v
      .as_f64()
      .filter(|n| n.is_finite() && n.fract() == 0.0 && *n >= 1.0)
      .map(|n| Some(n as usize))
      .ok_or_else(|| format!("{k} must be a positive integer")),
  }
}

/// A window of a text file's lines, numbered, in the harness's envelope.
fn read(cwd: &Path, input: &Value) -> Result<String, String> {
  let path = input["file_path"].as_str().unwrap_or_default();
  if path.trim().is_empty() {
    return Err("file_path must be a non-empty string".into());
  }
  let offset = positive(input, "offset")?.unwrap_or(1);
  let limit = positive(input, "limit")?.unwrap_or(READ_LIMIT);
  if limit > READ_LIMIT {
    return Err(format!("limit must be less than or equal to {READ_LIMIT}"));
  }
  let t = target(cwd, path)?;
  let Some(meta) = probe(&t.key)? else {
    see(cwd, &t.key, Seen::Absent);
    return Err(format!("cannot read \"{}\": not found", t.shown));
  };
  if !meta.is_file() {
    return Err(format!("cannot read \"{}\": not a regular file", t.shown));
  }
  let w = window(&t, offset, limit)?;
  see(cwd, &t.key, Seen::Present(version(&meta)));
  let end = w.lines.last().map_or(offset - 1, |(n, _)| *n);
  let footer = if w.capped {
    format!(
      "(Output capped. Showing lines {offset}-{end}. Use offset={} to continue.)",
      end + 1
    )
  } else if end < w.total {
    format!(
      "(Showing lines {offset}-{end} of {}. Use offset={} to continue.)",
      w.total,
      end + 1
    )
  } else {
    format!("(End of file - total {} lines)", w.total)
  };
  let body = if w.lines.is_empty() {
    footer
  } else {
    let lines: Vec<String> = w.lines.iter().map(|(n, x)| format!("{n}: {x}")).collect();
    format!("{}\n\n{footer}", lines.join("\n"))
  };
  Ok(format!(
    "<path>{}</path>\n<type>file</type>\n<content>\n{body}\n</content>",
    t.key.display()
  ))
}

/// The lines read: numbered, how many the file has, and whether READ_BYTES
/// cut them short.
struct Window {
  lines: Vec<(usize, String)>,
  total: usize,
  capped: bool,
}

/// Scans a file's text a piece at a time, counting all its lines and
/// keeping those asked for, each cut at READ_LINE_CHARS, until READ_BYTES.
fn window(t: &Target, offset: usize, limit: usize) -> Result<Window, String> {
  let fail = |why: &str| format!("cannot read \"{}\": {why}", t.shown);
  let mut file = fs::File::open(&t.key).map_err(|e| e.to_string())?;
  let mut w = Window {
    lines: Vec::new(),
    total: 0,
    capped: false,
  };
  let (mut bytes, mut line, mut line_chars) = (0usize, String::new(), 0usize);
  let mut take = |line: &mut String, w: &mut Window| {
    w.total += 1;
    let text = line.strip_suffix('\r').unwrap_or(line);
    if !w.capped && w.total >= offset && w.lines.len() < limit {
      let text = if text.encode_utf16().count() > READ_LINE_CHARS {
        format!(
          "{}... (line truncated to {READ_LINE_CHARS} chars)",
          head16(text, READ_LINE_CHARS)
        )
      } else {
        text.to_string()
      };
      let size = text.len() + usize::from(!w.lines.is_empty());
      if bytes + size > READ_BYTES {
        w.capped = true;
      } else {
        bytes += size;
        w.lines.push((w.total, text));
      }
    }
    line.clear();
  };
  let mut buf = vec![0u8; 1 << 16];
  let (mut pending, mut sampled, mut first) = (Vec::new(), 0usize, true);
  loop {
    let n = file.read(&mut buf).map_err(|e| e.to_string())?;
    let mut got = &buf[..n];
    let sample = got.len().min(BINARY_SAMPLE.saturating_sub(sampled));
    if got[..sample].contains(&0) {
      return Err(fail("binary file"));
    }
    sampled += sample;
    if first && got.starts_with(b"\xEF\xBB\xBF") {
      got = &got[3..];
    }
    first = false;
    pending.extend_from_slice(got);
    let upto = match std::str::from_utf8(&pending) {
      Ok(_) => pending.len(),
      Err(e) if e.error_len().is_none() && n > 0 => e.valid_up_to(),
      Err(_) => return Err(fail("invalid UTF-8 text")),
    };
    let text = std::str::from_utf8(&pending[..upto]).expect("checked");
    for (k, part) in text.split('\n').enumerate() {
      if k > 0 {
        take(&mut line, &mut w);
        line_chars = 0;
      }
      if line_chars <= READ_LINE_CHARS {
        let room = READ_LINE_CHARS + 1 - line_chars;
        let part = head16(part, room);
        line_chars += part.encode_utf16().count();
        line.push_str(&part);
      }
    }
    pending.drain(..upto);
    if n == 0 {
      break;
    }
  }
  if !line.is_empty() {
    take(&mut line, &mut w);
  }
  if !w.capped && offset > w.total && !(w.total == 0 && offset == 1) {
    return Err(format!(
      "offset {offset} is out of range for \"{}\" ({} lines)",
      t.shown, w.total
    ));
  }
  Ok(w)
}

/// A text's first `units` UTF-16 units, as JavaScript's substring cuts it;
/// half a surrogate pair, which Rust can't hold, as U+FFFD.
fn head16(s: &str, units: usize) -> String {
  let mut out = String::new();
  let mut n = 0;
  for c in s.chars() {
    let w = c.len_utf16();
    if n + w > units {
      if n < units {
        out.push('\u{FFFD}');
      }
      break;
    }
    out.push(c);
    n += w;
  }
  out
}

/// "Read the file first": the harness's remedy for a file not read, or
/// changed since.
fn not_read(shown: &str) -> String {
  format!("cannot modify \"{shown}\": file has not been read — read the file, then retry")
}

fn stale(why: String) -> String {
  format!("{why} — re-read the file, then retry")
}

/// A whole file written: over one only as last seen, else a new one.
fn write(cwd: &Path, input: &Value) -> Result<String, String> {
  let path = input["file_path"].as_str().unwrap_or_default();
  if path.trim().is_empty() {
    return Err("file_path must be a non-empty string".into());
  }
  let content = input["content"].as_str().unwrap_or_default();
  let t = target(cwd, path)?;
  let _lock = WRITING.lock();
  let was = probe(&t.key)?;
  if was.as_ref().is_some_and(|m| !m.is_file()) {
    return Err(format!("cannot write \"{}\": not a regular file", t.shown));
  }
  match (seen(cwd, &t.key), &was) {
    (Some(Seen::Present(_)), None) => {
      return Err(stale(format!("cannot write \"{}\": file no longer exists", t.shown)));
    }
    (Some(Seen::Present(v)), Some(m)) if *v != version(m) => {
      return Err(stale(format!(
        "cannot write \"{}\": file changed since it was read",
        t.shown
      )));
    }
    (Some(Seen::Present(_)), Some(_)) => {}
    (_, Some(_)) => return Err(not_read(&t.shown)),
    (_, None) => {}
  }
  if let Some(dir) = t.key.parent() {
    fs::create_dir_all(dir).map_err(|e| format!("cannot write \"{}\": {e}", t.shown))?;
  }
  let key = commit(&t, content, was.as_ref())?;
  let made = fs::metadata(&key).map_err(|e| e.to_string())?;
  see(cwd, &key, Seen::Present(version(&made)));
  let verb = if was.is_some() { "Updated" } else { "Created" };
  Ok(format!(
    "<path>{}</path>\n<type>file</type>\n<content>\n{verb} file\n</content>",
    key.display()
  ))
}

/// One literal replacement in a file the caller read as it is now.
fn edit(cwd: &Path, input: &Value) -> Result<String, String> {
  let text = |k: &str| input[k].as_str().unwrap_or_default();
  let (path, old, new) = (text("file_path"), text("old_string"), text("new_string"));
  let all = input["replace_all"].as_bool().unwrap_or(false);
  if path.trim().is_empty() {
    return Err("file_path must be a non-empty string".into());
  }
  if old.is_empty() {
    return Err("old_string must be a non-empty string".into());
  }
  if old == new {
    return Err("old_string and new_string must differ".into());
  }
  let t = target(cwd, path)?;
  let read_as = match seen(cwd, &t.key) {
    None => return Err(not_read(&t.shown)),
    Some(Seen::Absent) => return Err(format!("cannot edit \"{}\": not found", t.shown)),
    Some(Seen::Present(v)) => v,
  };
  let _lock = WRITING.lock();
  let changed = || stale(format!("cannot edit \"{}\": file changed since it was read", t.shown));
  let Some(meta) = probe(&t.key)? else {
    return Err(changed());
  };
  if !meta.is_file() {
    return Err(format!("cannot edit \"{}\": not a regular file", t.shown));
  }
  if version(&meta) != read_as {
    return Err(changed());
  }
  let bytes = fs::read(&t.key).map_err(|e| e.to_string())?;
  if bytes.contains(&0) {
    return Err(format!("cannot edit \"{}\": binary file", t.shown));
  }
  let raw = std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
    .map_err(|_| format!("cannot edit \"{}\": invalid UTF-8 text", t.shown))?;
  let crlf = {
    let sample = crate::clip::head(raw, 4096);
    let crlfs = sample.matches("\r\n").count();
    crlfs > sample.matches('\n').count() - crlfs
  };
  let content = raw.replace("\r\n", "\n");
  let (old, new) = (old.replace("\r\n", "\n"), new.replace("\r\n", "\n"));
  let n = content.matches(old.as_str()).count();
  if n == 0 {
    return Err(format!("old_string was not found in \"{}\"", t.shown));
  }
  if n > 1 && !all {
    return Err(format!(
      "old_string matched {n} times in \"{}\"; provide a more specific old_string or set replace_all to true",
      t.shown
    ));
  }
  let edited = content.replace(old.as_str(), &new);
  let edited = if crlf {
    edited.replace("\r\n", "\n").replace('\n', "\r\n")
  } else {
    edited
  };
  let key = commit(&t, &edited, Some(&meta))?;
  let made = fs::metadata(&key).map_err(|e| e.to_string())?;
  see(cwd, &key, Seen::Present(version(&made)));
  Ok(if all {
    format!(
      "The file {} has been updated. All occurrences were successfully replaced.",
      key.display()
    )
  } else {
    format!("The file {} has been updated successfully.", key.display())
  })
}

/// Writes a file whole, at once: a file beside it, renamed over it,
/// keeping its old permissions; a new one (`was` none) linked in, so it
/// never replaces what came there meanwhile, or a dangling link. Returns
/// its real path.
fn commit(t: &Target, content: &str, was: Option<&fs::Metadata>) -> Result<PathBuf, String> {
  let fail = |e: std::io::Error| format!("cannot write \"{}\": {e}", t.shown);
  let dir = t.key.parent().unwrap_or(Path::new("/"));
  let name = t.key.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
  let temp = dir.join(format!(".{name}.{:016x}.tmp", rand::random::<u64>()));
  let written = (|| {
    let mut f = fs::File::create(&temp)?;
    f.write_all(content.as_bytes())?;
    if let Some(m) = was {
      f.set_permissions(fs::Permissions::from_mode(m.permissions().mode()))?;
    }
    f.sync_all()?;
    if was.is_some() {
      fs::rename(&temp, &t.key)
    } else {
      fs::hard_link(&temp, &t.key)
    }
  })();
  let _ = fs::remove_file(&temp);
  match written {
    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
      return Err(match fs::symlink_metadata(&t.key) {
        Ok(m) if !m.is_file() => format!("cannot write \"{}\": not a regular file", t.shown),
        _ => not_read(&t.shown),
      });
    }
    Err(e) => return Err(fail(e)),
    Ok(()) => {}
  }
  Ok(fs::canonicalize(&t.key).unwrap_or_else(|_| t.key.clone()))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn call(name: &str, cwd: &Path, input: Value) -> Out {
    tokio::runtime::Runtime::new()
      .unwrap()
      .block_on(run(name, &input, cwd.to_path_buf(), CancellationToken::new()))
  }

  #[test]
  fn reads_number_lines_in_the_envelope() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("x"), "one\ntwo\r\nthree\n").unwrap();
    let real = fs::canonicalize(dir.path().join("x")).unwrap();
    let out = call("read", dir.path(), json!({"file_path": "x", "offset": 2, "limit": 1}));
    assert_eq!(
      out.text,
      format!(
        "<path>{}</path>\n<type>file</type>\n<content>\n2: two\n\n(Showing lines 2-2 of 3. Use offset=3 to continue.)\n</content>",
        real.display()
      )
    );
    let all = call("read", dir.path(), json!({"file_path": "x"}));
    assert!(
      all
        .text
        .contains("1: one\n2: two\n3: three\n\n(End of file - total 3 lines)")
    );
    let past = call("read", dir.path(), json!({"file_path": "x", "offset": 9}));
    assert!(past.fail && past.text.starts_with("offset 9 is out of range for"));
    let none = call("read", dir.path(), json!({"file_path": "nope"}));
    assert!(none.fail && none.text.ends_with("\": not found"));
  }

  #[test]
  fn writes_and_edits_only_what_was_read() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    fs::write(cwd.join("f"), "a\nb\n").unwrap();
    let blind = call("write", cwd, json!({"file_path": "f", "content": "x"}));
    assert!(
      blind.fail
        && blind
          .text
          .contains("file has not been read — read the file, then retry")
    );
    let unread = call(
      "edit",
      cwd,
      json!({"file_path": "f", "old_string": "a", "new_string": "A"}),
    );
    assert!(unread.fail && unread.text.contains("file has not been read"));
    call("read", cwd, json!({"file_path": "f"}));
    let ok = call(
      "edit",
      cwd,
      json!({"file_path": "f", "old_string": "a", "new_string": "A"}),
    );
    assert!(
      !ok.fail && ok.text.ends_with("has been updated successfully."),
      "{}",
      ok.text
    );
    let again = call(
      "edit",
      cwd,
      json!({"file_path": "f", "old_string": "b", "new_string": "B"}),
    );
    assert!(!again.fail, "an edit's own change needs no new read: {}", again.text);
    assert_eq!(fs::read_to_string(cwd.join("f")).unwrap(), "A\nB\n");
    std::thread::sleep(Duration::from_millis(5));
    fs::write(cwd.join("f"), "changed\n").unwrap();
    let stale = call(
      "edit",
      cwd,
      json!({"file_path": "f", "old_string": "changed", "new_string": "c"}),
    );
    assert!(
      stale.fail
        && stale
          .text
          .contains("file changed since it was read — re-read the file, then retry")
    );
    let made = call("write", cwd, json!({"file_path": "d/new", "content": "n\n"}));
    assert!(!made.fail && made.text.contains("Created file"), "{}", made.text);
  }

  #[test]
  fn edits_need_one_match() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    fs::write(cwd.join("f"), "x x\r\ny\r\n").unwrap();
    call("read", cwd, json!({"file_path": "f"}));
    let two = call(
      "edit",
      cwd,
      json!({"file_path": "f", "old_string": "x", "new_string": "z"}),
    );
    assert!(two.fail && two.text.contains("old_string matched 2 times in"));
    let all = call(
      "edit",
      cwd,
      json!({"file_path": "f", "old_string": "x\ny", "new_string": "z\nw", "replace_all": true}),
    );
    assert!(
      all.text.ends_with("All occurrences were successfully replaced."),
      "{}",
      all.text
    );
    assert_eq!(fs::read_to_string(cwd.join("f")).unwrap(), "x z\r\nw\r\n");
  }

  #[test]
  fn bash_reports_streams_and_exits() {
    let dir = tempfile::tempdir().unwrap();
    let out = call(
      "bash",
      dir.path(),
      json!({"description": "Test", "command": "echo hi; echo oops >&2; exit 3"}),
    );
    assert!(!out.fail);
    assert_eq!(out.text, "hi\n[stderr]\noops\n[exit code: 3]");
    let quiet = call("bash", dir.path(), json!({"description": "Test", "command": "true"}));
    assert_eq!(quiet.text, "(no output)");
    let slow = call(
      "bash",
      dir.path(),
      json!({"description": "Test", "command": "sleep 5", "timeoutMs": 100}),
    );
    assert_eq!(
      slow.text,
      "(no output)\n[timed out after 100ms]\n[killed by signal: SIGTERM]"
    );
    let big = call(
      "bash",
      dir.path(),
      json!({"description": "Test", "command": "head -c 100000 /dev/zero | tr '\\0' a"}),
    );
    assert!(big.text.contains("[output truncated; full output: "));
  }

  #[test]
  fn bash_settles_when_its_shell_exits() {
    let dir = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let bg = call(
      "bash",
      dir.path(),
      json!({"description": "Test", "command": "sleep 8 & echo started"}),
    );
    assert_eq!(bg.text, "started\n");
    assert!(
      t.elapsed() < Duration::from_secs(6),
      "a job left running ends the call GRACE after its shell"
    );
    let shut = call(
      "bash",
      dir.path(),
      json!({"description": "Test", "command": "exec >&- 2>&-; sleep 3", "timeoutMs": 300}),
    );
    assert_eq!(
      shut.text,
      "(no output)\n[timed out after 300ms]\n[killed by signal: SIGTERM]"
    );
    let usr = call(
      "bash",
      dir.path(),
      json!({"description": "Test", "command": "kill -USR1 $$"}),
    );
    assert_eq!(usr.text, "(no output)\n[killed by signal: SIGUSR1]");
  }

  #[test]
  fn calls_are_checked_against_their_schema() {
    let dir = tempfile::tempdir().unwrap();
    let out = call("bash", dir.path(), json!({"command": 5, "timeoutMs": "5"}));
    assert_eq!(
      out.text,
      "invalid arguments: missing required property \"description\"; \"command\" must be a string; \"timeoutMs\" must be a number"
    );
  }

  #[test]
  fn reads_as_javascript_would() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let mut late_nul = "a".repeat(9000).into_bytes();
    late_nul.push(0);
    fs::write(cwd.join("n"), &late_nul).unwrap();
    assert!(
      !call("read", cwd, json!({"file_path": "n"})).fail,
      "only the first 8 KB are sampled"
    );
    fs::write(cwd.join("e"), "😀".repeat(1500)).unwrap();
    let e = call("read", cwd, json!({"file_path": "e"}));
    assert!(
      e.text
        .contains(&format!("1: {}... (line truncated to 2000 chars)", "😀".repeat(1000)))
    );
    let home = call("read", cwd, json!({"file_path": "~/acorn-surely-not-here"}));
    assert!(
      home
        .text
        .contains(&config::home().join("acorn-surely-not-here").display().to_string())
    );
  }

  #[test]
  fn writes_never_clobber_what_they_did_not_see() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    std::os::unix::fs::symlink(cwd.join("gone"), cwd.join("link")).unwrap();
    let out = call("write", cwd, json!({"file_path": "link", "content": "x"}));
    assert!(
      out.fail && out.text.ends_with("link\": not a regular file"),
      "{}",
      out.text
    );
    assert!(fs::symlink_metadata(cwd.join("link")).unwrap().file_type().is_symlink());
    fs::write(cwd.join("c"), "a\r\nb\r\n").unwrap();
    call("read", cwd, json!({"file_path": "c"}));
    call(
      "edit",
      cwd,
      json!({"file_path": "c", "old_string": "a", "new_string": "x\r"}),
    );
    assert_eq!(fs::read_to_string(cwd.join("c")).unwrap(), "x\r\nb\r\n");
  }
}
