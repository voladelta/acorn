//! One person's `acorn-server`, kept up: started when `serve` starts and
//! on join, never stopped for being idle (timers and the Telegram poll run
//! in it), started again after an exit (1 s, doubling to 60 s; back to 1 s
//! after 5 minutes up), and stopped with a TERM when the host stops.

use crate::sandbox::Sandbox;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const FIRST_WAIT: Duration = Duration::from_secs(1);
const MOST_WAIT: Duration = Duration::from_secs(60);
const STEADY: Duration = Duration::from_secs(300);
/// The log kept of a server's output, at most.
const LOG_BYTES: u64 = 10 << 20;

/// How every person's server is run.
#[derive(Clone)]
pub struct Plan {
  pub data: PathBuf,
  pub server: PathBuf,
  pub web: PathBuf,
  pub sandbox: bool,
  pub key: String,
  pub budget: f64,
}

pub struct Runner {
  pub id: String,
  pub home: PathBuf,
  pub run: PathBuf,
  /// The server's token, read at each start.
  token: Mutex<String>,
  /// The pid to TERM: the server itself.
  pid: Mutex<Option<i32>>,
  /// The pid to KILL: bwrap, or the server.
  outer: Mutex<Option<i32>>,
  stop: watch::Sender<bool>,
  done: watch::Sender<bool>,
}

impl Runner {
  pub fn sock(&self) -> PathBuf {
    sock(&self.run)
  }

  pub fn token(&self) -> String {
    self.token.lock().unwrap().clone()
  }

  /// Starts it, and keeps it up until `end`.
  pub fn start(plan: &Plan, id: &str) -> Arc<Runner> {
    let home = plan.data.join("users").join(id);
    let run = run_dir(&plan.data, id);
    let runner = Arc::new(Runner {
      id: id.to_string(),
      home,
      run,
      token: Mutex::new(String::new()),
      pid: Mutex::new(None),
      outer: Mutex::new(None),
      stop: watch::channel(false).0,
      done: watch::channel(false).0,
    });
    let r = runner.clone();
    let plan = plan.clone();
    tokio::spawn(async move { r.keep(plan).await });
    runner
  }

  async fn keep(self: Arc<Self>, plan: Plan) {
    let mut stop = self.stop.subscribe();
    let mut wait = FIRST_WAIT;
    while !*stop.borrow() {
      let began = Instant::now();
      match self.run(&plan, &mut stop).await {
        Ok(status) => eprintln!("acorn-host: {}'s server ended: {status}", self.id),
        Err(e) => eprintln!("acorn-host: {}'s server: {e}", self.id),
      }
      if *stop.borrow() {
        break;
      }
      if began.elapsed() >= STEADY {
        wait = FIRST_WAIT;
      }
      tokio::select! {
        _ = tokio::time::sleep(wait) => {}
        _ = stop.changed() => {}
      }
      wait = (wait * 2).min(MOST_WAIT);
    }
    let _ = self.done.send(true);
  }

  /// One run, until it exits.
  async fn run(&self, plan: &Plan, stop: &mut watch::Receiver<bool>) -> Result<String, String> {
    let acorn = self.home.join(".acorn");
    let sock_dir = self.run.join("sock");
    crate::store::make_dir(&self.home)?;
    crate::store::make_dir(&self.run)?;
    crate::store::make_dir(&sock_dir)?;
    *self.token.lock().unwrap() = token(&self.run)?;
    let token = self.token();
    let log = open_log(&self.run.join("server.log"))?;
    let sock = self.sock();
    let server_args = |c: &mut std::process::Command| {
      c.arg(&plan.server).arg("--unix").arg(&sock).arg("--web").arg(&plan.web);
    };
    // the pipe bwrap tells its child's pid through
    let mut info: Option<(OwnedFd, OwnedFd)> = None;
    let mut cmd = if plan.sandbox {
      let (r, w) = pipe()?;
      let show = [plan.server.clone(), plan.web.clone()];
      let sandbox = Sandbox {
        data: &plan.data,
        home: &self.home,
        sock: &sock_dir,
        show: &show,
        info_fd: Some(w.as_raw_fd()),
      };
      let mut c = std::process::Command::new("bwrap");
      c.args(sandbox.args());
      server_args(&mut c);
      info = Some((r, w));
      c
    } else {
      let mut c = std::process::Command::new(&plan.server);
      c.arg("--unix").arg(&sock).arg("--web").arg(&plan.web);
      c
    };
    cmd.env_clear();
    for var in ["PATH", "LANG", "TZ"] {
      if let Some(v) = std::env::var_os(var) {
        cmd.env(var, v);
      }
    }
    cmd
      .env("HOME", &self.home)
      .env("ACORN_DIR", &acorn)
      .env("ACORN_TOKEN", token)
      .env("DEEPSEEK_API_KEY", &plan.key)
      .env("ACORN_BUDGET", plan.budget.to_string())
      .current_dir(&self.home)
      .stdin(std::process::Stdio::null())
      .stdout(log.try_clone().map_err(|e| e.to_string())?)
      .stderr(log);
    if let Some((_, w)) = &info {
      let fd = w.as_raw_fd();
      use std::os::unix::process::CommandExt as _;
      // SAFETY: only async-signal-safe calls between fork and exec
      unsafe {
        cmd.pre_exec(move || {
          // the pipe is made close-on-exec; bwrap must keep this end
          if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
            return Err(std::io::Error::last_os_error());
          }
          Ok(())
        });
      }
    }
    let mut child = tokio::process::Command::from(cmd)
      .kill_on_drop(true)
      .spawn()
      .map_err(|e| format!("can't start: {e}"))?;
    let outer = child.id().map(|p| p as i32);
    *self.outer.lock().unwrap() = outer;
    *self.pid.lock().unwrap() = outer;
    if let Some((r, w)) = info {
      drop(w);
      // bwrap writes {"child-pid": N} and closes it; N is its init in the
      // namespace, whose child is the server
      let text = tokio::task::spawn_blocking(move || {
        let mut f = std::fs::File::from(r);
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut f, &mut s);
        s
      })
      .await
      .unwrap_or_default();
      let init = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["child-pid"].as_i64())
        .map(|p| p as i32);
      let child = match init {
        Some(pid) => first_child(pid).await,
        None => None,
      };
      *self.pid.lock().unwrap() = child.or(outer);
    }
    let status = tokio::select! {
      s = child.wait() => s.map_err(|e| e.to_string())?,
      _ = stop.changed() => child.wait().await.map_err(|e| e.to_string())?,
    };
    *self.pid.lock().unwrap() = None;
    *self.outer.lock().unwrap() = None;
    Ok(status.to_string())
  }

  /// TERMs it; KILLs it if it is still up after `grace`.
  pub async fn end(&self, grace: Duration) {
    let _ = self.stop.send(true);
    if let Some(pid) = *self.pid.lock().unwrap() {
      // SAFETY: a signal to a pid of ours
      unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let mut done = self.done.subscribe();
    if tokio::time::timeout(grace, done.wait_for(|&d| d)).await.is_err() {
      if let Some(pid) = *self.outer.lock().unwrap() {
        // bwrap's --die-with-parent and pid namespace take the rest down
        unsafe { libc::kill(pid, libc::SIGKILL) };
      }
      let _ = tokio::time::timeout(Duration::from_secs(2), done.wait_for(|&d| d)).await;
    }
  }
}

/// The host-owned runtime directory for a person's server.
pub fn run_dir(data: &Path, id: &str) -> PathBuf {
  data.join("run").join(id)
}

/// Where a person's server listens: a path only its sandbox and the host can reach.
pub fn sock(run: &Path) -> PathBuf {
  run.join("sock").join("server.sock")
}

/// The socket at `path`, held open: `None` for a link to one elsewhere.
/// The person's agents can write the directory it is in, so the name could be
/// swapped for a link between a look at it and a connect to it; a connect to
/// [`Held::path`] reaches the file that was looked at whatever the name is by then.
pub fn hold(path: &Path) -> Option<Held> {
  let file = std::fs::OpenOptions::new()
    .read(true)
    .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
    .open(path)
    .ok()?;
  file.metadata().ok()?.file_type().is_socket().then_some(Held(file))
}

/// A socket held open by [`hold`].
pub struct Held(std::fs::File);

impl Held {
  /// A path to connect to, good for as long as this is held.
  pub fn path(&self) -> PathBuf {
    format!("/proc/self/fd/{}", self.0.as_raw_fd()).into()
  }
}

/// The server's token, kept in the host-owned runtime directory.
fn token(run: &Path) -> Result<String, String> {
  let file = run.join("server-token");
  if let Ok(t) = std::fs::read_to_string(&file)
    && t.trim().len() >= 32
  {
    return Ok(t.trim().to_string());
  }
  let t = crate::store::random_hex(24);
  std::fs::OpenOptions::new()
    .create(true)
    .truncate(true)
    .write(true)
    .mode(0o600)
    .open(&file)
    .and_then(|mut f| std::io::Write::write_all(&mut f, t.as_bytes()))
    .map_err(|e| format!("{}: {e}", file.display()))?;
  Ok(t)
}

/// The log, begun again once it passes LOG_BYTES (the last kept as .old).
fn open_log(path: &Path) -> Result<std::fs::File, String> {
  if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_BYTES) {
    let _ = std::fs::rename(path, path.with_extension("log.old"));
  }
  std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .mode(0o600)
    .open(path)
    .map_err(|e| format!("{}: {e}", path.display()))
}

fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
  let mut fds = [0; 2];
  // SAFETY: pipe2 fills two fds we then own
  if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
    return Err(format!("pipe: {}", std::io::Error::last_os_error()));
  }
  Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// The first child of `pid`, waited for a moment (bwrap's init forks it).
async fn first_child(pid: i32) -> Option<i32> {
  for _ in 0..50 {
    if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
      for t in tasks.flatten() {
        if let Ok(text) = std::fs::read_to_string(t.path().join("children"))
          && let Some(c) = text.split_whitespace().next().and_then(|c| c.parse().ok())
        {
          return Some(c);
        }
      }
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Read as _;
  use std::os::unix::net::{UnixListener, UnixStream};
  use std::sync::atomic::{AtomicBool, Ordering};

  /// A listener at `path` that says `name` to whoever connects.
  fn says(path: &Path, name: u8) {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
      for s in listener.incoming().flatten() {
        let _ = std::io::Write::write_all(&mut &s, &[name]);
      }
    });
  }

  fn heard(held: &Held) -> Option<u8> {
    let mut s = UnixStream::connect(held.path()).ok()?;
    let mut b = [0u8; 1];
    s.read_exact(&mut b).ok()?;
    Some(b[0])
  }

  #[test]
  fn holds_a_socket_and_no_link_to_one() {
    let dir = tempfile::tempdir().unwrap();
    let (own, other, link) = (
      dir.path().join("own"),
      dir.path().join("other"),
      dir.path().join("link"),
    );
    says(&own, b'a');
    says(&other, b'b');
    std::os::unix::fs::symlink(&other, &link).unwrap();
    assert_eq!(heard(&hold(&own).unwrap()), Some(b'a'));
    assert!(hold(&link).is_none(), "a link to a socket is not the person's socket");
    assert!(hold(&dir.path().join("none")).is_none());
    std::fs::write(dir.path().join("file"), "").unwrap();
    assert!(hold(&dir.path().join("file")).is_none());
  }

  /// The name swapped for a link to another socket while the host connects:
  /// what a person's agent can do in the socket directory it is given.
  #[test]
  fn a_held_socket_is_the_one_connected_to() {
    let dir = tempfile::tempdir().unwrap();
    let (name, other, link) = (
      dir.path().join("server.sock"),
      dir.path().join("other"),
      dir.path().join("link"),
    );
    says(&name, b'a');
    says(&other, b'b');
    std::os::unix::fs::symlink(&other, &link).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let swap = {
      let stop = stop.clone();
      let [a, b] = [&name, &link].map(|p| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap());
      std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
          // server.sock: the socket, then the link, then the socket again
          let done = unsafe {
            libc::renameat2(
              libc::AT_FDCWD,
              a.as_ptr(),
              libc::AT_FDCWD,
              b.as_ptr(),
              libc::RENAME_EXCHANGE,
            )
          };
          assert_eq!(done, 0);
        }
      })
    };
    let end = Instant::now() + Duration::from_secs(2);
    let mut reached = 0;
    while Instant::now() < end {
      if let Some(held) = hold(&name) {
        assert_eq!(heard(&held), Some(b'a'), "connected to the socket the link names");
        reached += 1;
      }
    }
    stop.store(true, Ordering::Relaxed);
    swap.join().unwrap();
    assert!(reached > 0);
  }
}
