//! Machine
//! =======
//! What touches this Mac, as `luau`'s code calls it: a command run
//! (`shell`), a file read (`text`), written (`write`) or edited (`edit`),
//! each in its caller's dir under the work dir. They follow DeepSeek's
//! harness (deepseek-harness: tool-bash, tool-fs, fs-local,
//! fs-observation-policy): its arguments, results and errors; its
//! foreground bash (no background jobs) on a Mac with no file sandbox.
//!
//! A caller may write over or edit a file only as it last read, wrote or
//! edited it (`SEEN`): one it never read, or that changed since, is refused
//! until it reads it again. A stop kills the job's shell and its process
//! group. As Acorn quits, `end` kills every shell's group, and any process
//! marked ACORN_RUN that left it.

use crate::ai::Spec;
use crate::config;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio_util::sync::CancellationToken;

/// The most a command may run (ms).
const MAX_TIMEOUT_MS: f64 = 600_000.0;
/// From SIGTERM to SIGKILL, as a command times out.
const GRACE: Duration = Duration::from_secs(3);
/// How often a timed out command's group is looked at, to end the call
/// once it is empty.
const GROUP_POLL: Duration = Duration::from_millis(20);
/// The longest a process amid an exec is waited for to show its
/// environment (`marked_pids`).
#[cfg(target_os = "linux")]
const EXEC_WAIT: Duration = Duration::from_millis(20);
/// The bytes of each of a command's streams kept: their tail.
pub const OUTPUT_BYTES: usize = 64_000;
/// The most of a stream its spill file keeps.
const SPILL_BYTES: u64 = 64 * 1024 * 1024;
/// What every command's environment overrides.
const ENV: [(&str, &str); 4] = [
  ("NO_COLOR", "1"),
  ("TERM", "dumb"),
  ("PAGER", "cat"),
  ("GIT_PAGER", "cat"),
];

/// The most of a file a program reads whole (`text`).
const TEXT_BYTES: u64 = 100 * 1024 * 1024;
/// The bytes at a file's start in which a NUL makes it binary.
const BINARY_SAMPLE: usize = 8192;

/// This Acorn's mark, set as ACORN_RUN on every process its shells start
/// (each child inherits it), to find those that left their shell's group.
static RUN: LazyLock<String> = LazyLock::new(|| format!("{:016x}", rand::random::<u64>()));

/// The shells started, by process group, with whether each exited, kept
/// until their groups are empty, so `end` kills a backgrounded job whose
/// shell already exited.
static SHELLS: LazyLock<Mutex<HashMap<i32, Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The most files `SEEN` keeps.
const SEEN_MOST: usize = 8192;

/// What each caller (by its dir) last saw of each file (by its real path):
/// the SEEN_MOST seen last. A file dropped counts as never read, so its
/// caller reads it again before it writes over or edits it.
static SEEN: LazyLock<Mutex<Sights>> = LazyLock::new(|| Mutex::new(Sights::default()));

/// Files as seen, each with when (`tick`: a count of sights).
#[derive(Default)]
struct Sights {
  files: HashMap<(PathBuf, PathBuf), (Seen, u64)>,
  tick: u64,
}

impl Sights {
  /// A file seen; over `most`, the older half is dropped.
  fn see(&mut self, key: (PathBuf, PathBuf), s: Seen, most: usize) {
    self.tick += 1;
    self.files.insert(key, (s, self.tick));
    if self.files.len() > most {
      let mut ticks: Vec<u64> = self.files.values().map(|(_, t)| *t).collect();
      let mid = ticks.len() / 2;
      let (_, &mut oldest_kept, _) = ticks.select_nth_unstable(mid);
      self.files.retain(|_, (_, t)| *t >= oldest_kept);
    }
  }
}

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

// Shell
// =====

/// A command run to its end: its streams, whether it timed out, how it
/// exited.
pub struct Ran {
  out: Stream,
  err: Stream,
  pub timed_out: bool,
  pub status: std::process::ExitStatus,
}

impl Ran {
  /// Its stdout and stderr as text: each its tail alone, no note.
  pub fn tails(&self) -> (String, String) {
    let tail = |s: &Stream| String::from_utf8_lossy(&s.tail).into_owned();
    (tail(&self.out), tail(&self.err))
  }

  /// Where the whole of its stdout and stderr went, each if cut and kept.
  pub fn spills(&self) -> (Option<String>, Option<String>) {
    let spill = |s: &Stream| s.spill.as_ref().map(|(p, ..)| p.display().to_string());
    (spill(&self.out), spill(&self.err))
  }

  /// Whether either stream was cut to its tail.
  pub fn truncated(&self) -> bool {
    self.out.truncated || self.err.truncated
  }

  /// Its exit code; killed by a signal, 128 plus the signal, as a shell
  /// reports it.
  pub fn code(&self) -> i32 {
    self
      .status
      .code()
      .unwrap_or_else(|| 128 + self.status.signal().unwrap_or(0))
  }
}

/// Runs a command with `bash -c` in `workdir`, in its own process group,
/// every process marked ACORN_RUN, its streams kept apart. On its timeout
/// (ms, at most MAX_TIMEOUT_MS) the group gets SIGTERM, then SIGKILL after
/// GRACE if any of it is left (the shell, or a job it started).
/// It ends when the shell has exited and its streams have closed, or GRACE
/// after the shell exited, whatever still holds them (a job left running).
/// Timed out, it ends no sooner than its group is empty or got that
/// SIGKILL: no signal is sent after it returns, when the group's id may
/// name another.
/// A stop kills the group and ends the wait at once.
pub async fn shell(command: &str, workdir: &Path, timeout: f64, cancel: CancellationToken) -> Result<Ran, String> {
  let timeout = timeout.min(MAX_TIMEOUT_MS);
  SHELLS.lock().retain(|&pid, exited| group(pid, exited));
  let mut cmd = tokio::process::Command::new("bash");
  cmd
    .arg("-c")
    .arg(command)
    .current_dir(workdir)
    .envs(ENV)
    .env("ACORN_RUN", &*RUN)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .process_group(0);
  for key in config::hidden() {
    cmd.env_remove(key);
  }
  let mut child = cmd.spawn().map_err(|err| err.to_string())?;
  let pid = child.id().map_or(0, |p| p as i32);
  let exited = Arc::new(AtomicBool::new(false));
  let mut reap = Reap(pid, exited.clone());
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
  let kill = tokio::time::sleep(Duration::MAX / 4);
  tokio::pin!(kill);
  let (mut timed_out, mut killed, mut closed, mut status) = (false, false, false, None);
  loop {
    let settled = closed && status.is_some();
    if settled && (!timed_out || killed || !group(pid, &exited)) {
      break;
    }
    tokio::select! {
      _ = &mut streams, if !closed => closed = true,
      got = &mut wait, if status.is_none() => {
        exited.store(true, Ordering::Relaxed);
        match got {
          Ok(got) => status = Some(got),
          Err(why) => return Err(why.to_string()),
        }
        drain.as_mut().reset(tokio::time::Instant::now() + GRACE);
      }
      () = &mut drain, if status.is_some() && !closed => closed = true,
      () = &mut expiry, if !timed_out => {
        timed_out = true;
        if group(pid, &exited) {
          signal(-pid, libc::SIGTERM);
          kill.as_mut().reset(tokio::time::Instant::now() + GRACE);
        } else {
          killed = true;
        }
      }
      () = &mut kill, if timed_out && !killed => {
        killed = true;
        if group(pid, &exited) {
          signal(-pid, libc::SIGKILL);
        }
      }
      () = tokio::time::sleep(GROUP_POLL), if settled => {}
      () = cancel.cancelled() => {
        if group(pid, &exited) {
          signal(-pid, libc::SIGKILL);
        }
        reap.0 = 0;
        if status.is_none() {
          let _ = wait.await;
          exited.store(true, Ordering::Relaxed);
        }
        return Err("tool call aborted".into());
      }
    }
  }
  let status = status.expect("the loop ends once the shell exited");
  reap.0 = 0;
  let take = |s: &Mutex<Stream>| std::mem::take(&mut *s.lock());
  Ok(Ran {
    out: take(&out),
    err: take(&err),
    timed_out,
    status,
  })
}

/// A command's process group, with whether its shell exited: killed, if
/// still there, when its run is dropped before it ends (a program gave the
/// call up), so nothing it started runs on; 0 once it ended.
struct Reap(i32, Arc<AtomicBool>);

impl Drop for Reap {
  fn drop(&mut self) {
    if self.0 != 0 && group(self.0, &self.1) {
      signal(-self.0, libc::SIGKILL);
    }
  }
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

/// The user's own spill dir: one shared by every user would be the first
/// one's, its files unwritable for the rest and readable by all.
fn spill_dir() -> Option<PathBuf> {
  use std::os::unix::fs::{DirBuilderExt, MetadataExt};
  // SAFETY: getuid cannot fail.
  let uid = unsafe { libc::getuid() };
  let dir = std::env::temp_dir().join(format!("acorn-spill-{uid}"));
  let _ = fs::DirBuilder::new().mode(0o700).create(&dir);
  let meta = fs::symlink_metadata(&dir).ok()?;
  (meta.is_dir() && meta.uid() == uid && meta.mode() & 0o077 == 0).then_some(dir)
}

/// A new spill file, holding a stream so far.
fn spill_file(so_far: &[u8]) -> Option<(PathBuf, fs::File, u64)> {
  let dir = spill_dir()?;
  let path = dir.join(format!("{:016x}.log", rand::random::<u64>()));
  let mut f = fs::File::create(&path).ok()?;
  f.write_all(so_far).ok()?;
  Some((path, f, so_far.len() as u64))
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

/// The processes of this user's whose environment holds `mark`
/// (`KEY=VALUE`), but for this one. A process amid an exec shows no
/// environment for a moment: one that shows none is read again for up
/// to EXEC_WAIT, unless it is a zombie, which never has one.
#[cfg(target_os = "linux")]
fn marked_pids(mark: &str) -> Vec<i32> {
  // SAFETY: getuid has no preconditions.
  let uid = unsafe { libc::getuid() };
  let me = std::process::id() as i32;
  let Ok(entries) = fs::read_dir("/proc") else {
    return Vec::new();
  };
  let mut pids = Vec::new();
  for entry in entries.flatten() {
    let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
      continue;
    };
    let dir = entry.path();
    if pid == me || !fs::metadata(&dir).is_ok_and(|m| m.uid() == uid) {
      continue;
    }
    let since = std::time::Instant::now();
    let env = loop {
      // one that may not be read cannot be told, and is left
      let Ok(env) = fs::read(dir.join("environ")) else {
        break Vec::new();
      };
      if !env.is_empty() || since.elapsed() >= EXEC_WAIT || zombie(&dir) {
        break env;
      }
      std::thread::sleep(Duration::from_millis(1));
    };
    if env.split(|&b| b == 0).any(|kv| kv == mark.as_bytes()) {
      pids.push(pid);
    }
  }
  pids
}

/// Whether a process (by its /proc dir) is a zombie, or gone.
#[cfg(target_os = "linux")]
fn zombie(dir: &Path) -> bool {
  let stat = fs::read_to_string(dir.join("stat")).unwrap_or_default();
  // its state follows its name, which is in the last parentheses
  stat
    .rsplit_once(')')
    .is_none_or(|(_, rest)| rest.trim_start().starts_with(['Z', 'X']))
}

/// Processes owned by this user whose environment carries `mark`
/// (`KEY=VALUE`), excluding this process itself.
#[cfg(not(target_os = "linux"))]
fn marked_pids(mark: &str) -> Vec<i32> {
  // SAFETY: getuid has no preconditions.
  let uid = unsafe { libc::getuid() }.to_string();
  let me = std::process::id() as i32;
  let Ok(ps) = std::process::Command::new("ps")
    .args(["-wwE", "-U", &uid, "-o", "pid=,command="])
    .output()
  else {
    return Vec::new();
  };
  String::from_utf8_lossy(&ps.stdout)
    .lines()
    .filter(|l| l.contains(mark))
    .filter_map(|l| l.split_whitespace().next().and_then(|p| p.parse::<i32>().ok()))
    .filter(|&pid| pid > 0 && pid != me)
    .collect()
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
  let mark = format!("ACORN_RUN={}", *RUN);
  for pid in marked_pids(&mark) {
    signal(pid, libc::SIGKILL);
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
  let sights = SEEN.lock();
  let (s, _) = sights.files.get(&(cwd.to_path_buf(), key.to_path_buf()))?;
  Some(s.clone())
}

fn see(cwd: &Path, key: &Path, s: Seen) {
  SEEN.lock().see((cwd.to_path_buf(), key.to_path_buf()), s, SEEN_MOST);
}

/// A text file as a program reads it (luau's `read`): whole, or `limit`
/// lines from line `offset`, unnumbered and uncut, up to TEXT_BYTES. It
/// counts as read, so the caller may then write or edit the file.
pub fn text(cwd: &Path, path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String, String> {
  let t = target(cwd, path)?;
  let fail = |why: &str| format!("cannot read \"{}\": {why}", t.shown);
  let Some(meta) = probe(&t.key)? else {
    see(cwd, &t.key, Seen::Absent);
    return Err(fail("not found"));
  };
  if !meta.is_file() {
    return Err(fail("not a regular file"));
  }
  if meta.len() > TEXT_BYTES {
    return Err(fail(&format!(
      "{} bytes, over {} MB: read it in parts with sh (head, sed -n)",
      meta.len(),
      TEXT_BYTES >> 20
    )));
  }
  let bytes = fs::read(&t.key).map_err(|e| fail(&e.to_string()))?;
  if bytes[..bytes.len().min(BINARY_SAMPLE)].contains(&0) {
    return Err(fail("binary file"));
  }
  let raw = std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
    .map_err(|_| fail("invalid UTF-8 text"))?;
  see(cwd, &t.key, Seen::Present(version(&meta)));
  if offset.is_none() && limit.is_none() {
    return Ok(raw.to_string());
  }
  let from = offset.unwrap_or(1).max(1) - 1;
  Ok(
    raw
      .split_inclusive('\n')
      .skip(from)
      .take(limit.unwrap_or(usize::MAX))
      .collect(),
  )
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
pub fn write(cwd: &Path, input: &Value) -> Result<String, String> {
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
pub fn edit(cwd: &Path, input: &Value) -> Result<String, String> {
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
    let mut f = fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(was.map_or(0o666, |m| m.permissions().mode()))
      .open(&temp)?;
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

  #[test]
  fn spill_files_go_in_a_dir_of_the_users_own() {
    use std::os::unix::fs::MetadataExt;
    let dir = spill_dir().expect("a spill dir");
    let meta = fs::metadata(&dir).unwrap();
    // SAFETY: getuid cannot fail.
    assert_eq!(meta.uid(), unsafe { libc::getuid() });
    assert_eq!(meta.mode() & 0o777, 0o700);
  }

  fn run(command: &str, timeout: f64) -> Ran {
    tokio::runtime::Runtime::new()
      .unwrap()
      .block_on(shell(command, Path::new("/tmp"), timeout, CancellationToken::new()))
      .unwrap()
  }

  fn change(cwd: &Path, path: &str, old: &str, new: &str, all: bool) -> Result<String, String> {
    edit(
      cwd,
      &json!({"file_path": path, "old_string": old, "new_string": new, "replace_all": all}),
    )
  }

  #[test]
  fn text_reads_lines_whole() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    fs::write(cwd.join("x"), "one\ntwo\r\nthree\n").unwrap();
    assert_eq!(text(cwd, "x", Some(2), Some(1)).unwrap(), "two\r\n");
    assert_eq!(text(cwd, "x", None, None).unwrap(), "one\ntwo\r\nthree\n");
    assert!(text(cwd, "nope", None, None).unwrap_err().ends_with("\": not found"));
    let mut late_nul = "a".repeat(9000).into_bytes();
    late_nul.push(0);
    fs::write(cwd.join("n"), &late_nul).unwrap();
    assert!(text(cwd, "n", None, None).is_ok(), "only the first 8 KB are sampled");
    fs::write(cwd.join("b"), b"a\0b").unwrap();
    assert!(text(cwd, "b", None, None).unwrap_err().ends_with("binary file"));
    let home = text(cwd, "~/acorn-surely-not-here", None, None).unwrap_err();
    assert!(home.contains(&config::home().join("acorn-surely-not-here").display().to_string()));
  }

  #[test]
  fn writes_and_edits_only_what_was_read() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    fs::write(cwd.join("f"), "a\nb\n").unwrap();
    let blind = write(cwd, &json!({"file_path": "f", "content": "x"})).unwrap_err();
    assert!(blind.contains("file has not been read — read the file, then retry"));
    assert!(
      change(cwd, "f", "a", "A", false)
        .unwrap_err()
        .contains("file has not been read")
    );
    text(cwd, "f", None, None).unwrap();
    let ok = change(cwd, "f", "a", "A", false).unwrap();
    assert!(ok.ends_with("has been updated successfully."), "{ok}");
    assert!(
      change(cwd, "f", "b", "B", false).is_ok(),
      "an edit's own change needs no new read"
    );
    assert_eq!(fs::read_to_string(cwd.join("f")).unwrap(), "A\nB\n");
    std::thread::sleep(Duration::from_millis(5));
    fs::write(cwd.join("f"), "changed\n").unwrap();
    let stale = change(cwd, "f", "changed", "c", false).unwrap_err();
    assert!(stale.contains("file changed since it was read — re-read the file, then retry"));
    let made = write(cwd, &json!({"file_path": "d/new", "content": "n\n"})).unwrap();
    assert!(made.contains("Created file"), "{made}");
  }

  #[test]
  fn edits_need_one_match_and_keep_crlf() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    fs::write(cwd.join("f"), "x\r\ny\r\nx\r\ny\r\n").unwrap();
    text(cwd, "f", None, None).unwrap();
    let two = change(cwd, "f", "x\ny", "z\nw", false).unwrap_err();
    assert!(two.contains("old_string matched 2 times in"));
    let all = change(cwd, "f", "x\ny", "z\nw", true).unwrap();
    assert!(all.ends_with("All occurrences were successfully replaced."), "{all}");
    assert_eq!(fs::read_to_string(cwd.join("f")).unwrap(), "z\r\nw\r\nz\r\nw\r\n");
  }

  #[test]
  fn writes_never_clobber_what_they_did_not_see() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    std::os::unix::fs::symlink(cwd.join("gone"), cwd.join("link")).unwrap();
    let out = write(cwd, &json!({"file_path": "link", "content": "x"})).unwrap_err();
    assert!(out.ends_with("link\": not a regular file"), "{out}");
    assert!(fs::symlink_metadata(cwd.join("link")).unwrap().file_type().is_symlink());
    fs::write(cwd.join("c"), "a\r\nb\r\n").unwrap();
    text(cwd, "c", None, None).unwrap();
    change(cwd, "c", "a", "x\r", false).unwrap();
    assert_eq!(fs::read_to_string(cwd.join("c")).unwrap(), "x\r\nb\r\n");
  }

  #[test]
  fn shell_keeps_streams_apart_and_exits() {
    let ran = run("echo hi; echo oops >&2; exit 3", 10_000.0);
    assert_eq!(ran.tails(), ("hi\n".into(), "oops\n".into()));
    assert_eq!(ran.code(), 3);
    let slow = run("sleep 5", 100.0);
    assert!(slow.timed_out && slow.code() == 128 + libc::SIGTERM);
    let big = run("head -c 100000 /dev/zero | tr '\\0' a", 10_000.0);
    assert!(big.truncated() && big.tails().0.len() == OUTPUT_BYTES);
    assert!(
      big
        .spills()
        .0
        .is_some_and(|f| fs::metadata(f).unwrap().len() == 100_000)
    );
  }

  #[test]
  fn shell_settles_when_it_exits() {
    let t = std::time::Instant::now();
    let bg = run("sleep 8 & echo started", 10_000.0);
    assert_eq!(bg.tails().0, "started\n");
    assert!(
      t.elapsed() < Duration::from_secs(6),
      "a job left running ends the call GRACE after its shell"
    );
    let shut = run("exec >&- 2>&-; sleep 3", 300.0);
    assert!(shut.timed_out && shut.code() == 128 + libc::SIGTERM);
    assert_eq!(run("kill -USR1 $$", 10_000.0).code(), 128 + libc::SIGUSR1);
  }

  #[test]
  fn shell_timeout_escalates_to_sigkill_for_shell_and_group() {
    let t = std::time::Instant::now();
    let fast = run("sleep 5 | sleep 5", 100.0);
    assert!(fast.timed_out && fast.code() == 128 + libc::SIGTERM);
    assert!(t.elapsed() < GRACE, "exiting on SIGTERM settles without waiting GRACE");

    let stubborn = run("trap '' TERM; exec sleep 60", 100.0);
    assert!(stubborn.timed_out && stubborn.code() == 128 + libc::SIGKILL);

    let bg = run(
      "(trap '' TERM; exec sleep 60 >/dev/null 2>&1) & echo $!; sleep 10",
      100.0,
    );
    assert!(bg.timed_out);
    let pid: i32 = bg.tails().0.trim().parse().unwrap();
    // killed, it may wait a moment to be reaped
    #[cfg(target_os = "linux")]
    let dead = || zombie(Path::new(&format!("/proc/{pid}")));
    #[cfg(not(target_os = "linux"))]
    let dead = || !alive(pid);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !dead() && std::time::Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(10));
    }
    let gone = dead();
    if !gone {
      signal(pid, libc::SIGKILL);
    }
    assert!(gone, "background job ignoring SIGTERM should be killed after GRACE");
  }

  #[test]
  fn marked_pids_finds_detached_child_by_env() {
    use std::os::unix::process::CommandExt as _;
    let token = format!("{:016x}", rand::random::<u64>());
    let mark = format!("ACORN_RUN={token}");
    let mut child = std::process::Command::new("sleep")
      .arg("60")
      .env("ACORN_RUN", &token)
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .process_group(0)
      .spawn()
      .unwrap();
    let pid = child.id() as i32;
    let found = marked_pids(&mark);
    let prefix_miss = marked_pids(&format!("ACORN_RUN={token}x"));
    let key_miss = marked_pids(&format!("OTHER_ACORN_RUN={token}"));
    signal(pid, libc::SIGKILL);
    let _ = child.wait();
    assert!(found.contains(&pid), "expected pid {pid} in {found:?}");
    assert!(!prefix_miss.contains(&pid));
    assert!(!key_miss.contains(&pid));
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn a_job_that_left_its_shells_group_is_still_marked() {
    let ran = run("setsid sleep 60 >/dev/null 2>&1 & echo $!", 10_000.0);
    let pid: i32 = ran.tails().0.trim().parse().unwrap();
    // SAFETY: getpgid only reads.
    let left = || unsafe { libc::getpgid(pid) } == pid;
    let t = std::time::Instant::now();
    // the job takes a moment to leave the group
    while !left() && t.elapsed() < Duration::from_secs(5) {
      std::thread::sleep(Duration::from_millis(1));
    }
    let (left, found) = (left(), marked_pids(&format!("ACORN_RUN={}", *RUN)));
    signal(pid, libc::SIGKILL);
    assert!(left, "setsid gives the job its own group");
    assert!(found.contains(&pid), "expected pid {pid} in {found:?}");
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn a_zombie_is_told_from_a_process_that_runs() {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dir = PathBuf::from(format!("/proc/{}", child.id()));
    let t = std::time::Instant::now();
    while !zombie(&dir) && t.elapsed() < Duration::from_secs(5) {
      std::thread::sleep(Duration::from_millis(1));
    }
    assert!(zombie(&dir), "exited and not yet waited for");
    assert!(!zombie(Path::new("/proc/self")));
    child.wait().unwrap();
    assert!(zombie(&dir), "gone");
  }

  #[test]
  fn the_files_seen_longest_ago_are_dropped() {
    let key = |k: usize| (PathBuf::from("/dir"), PathBuf::from(format!("/dir/{k}")));
    let mut sights = Sights::default();
    for k in 0..8 {
      sights.see(key(k), Seen::Absent, 8);
    }
    sights.see(key(0), Seen::Present("v".into()), 8); // seen again: now the newest
    assert_eq!(sights.files.len(), 8);
    sights.see(key(8), Seen::Absent, 8);
    assert!(sights.files.len() <= 8, "kept within its bound");
    assert!(sights.files.contains_key(&key(0)) && sights.files.contains_key(&key(8)));
    assert!(!sights.files.contains_key(&key(1)), "the oldest go first");
    for k in 9..1000 {
      sights.see(key(k), Seen::Absent, 8);
    }
    assert!(sights.files.len() <= 8);
    assert!(sights.files.contains_key(&key(999)));
  }
}
