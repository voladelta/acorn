//! Each person's server runs under `bwrap`: the agent runs shell commands,
//! so it sees the system read-only, its own HOME (`<data>/users/<id>`)
//! read-write, and no one else's: not the other homes, not the host's
//! book or key, not another sandbox's processes, and not the session
//! sockets under /run/user (an ssh agent, the user's D-Bus, which starts
//! services). The network stays shared, as the agent needs the web.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What a sandbox for one person shows.
pub struct Sandbox<'a> {
  /// The host's data dir.
  pub data: &'a Path,
  /// The person's HOME, `<data>/users/<id>`.
  pub home: &'a Path,
  /// The host-owned socket directory, mounted at its own path.
  pub sock: &'a Path,
  /// Files and dirs shown read-only at their own paths (the server, the web
  /// window), which may sit in a hidden dir.
  pub show: &'a [PathBuf],
  /// The fd bwrap writes its child's pid to, if any.
  pub info_fd: Option<i32>,
}

impl Sandbox<'_> {
  /// The bwrap arguments before the command.
  pub fn args(&self) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut arg = |s: &str| a.push(s.into());
    for s in [
      "--die-with-parent",
      "--new-session",
      "--unshare-pid",
      "--unshare-ipc",
      "--unshare-uts",
      "--ro-bind",
      "/",
      "/",
      "--proc",
      "/proc",
      "--dev",
      "/dev",
      "--tmpfs",
      "/tmp",
    ] {
      arg(s);
    }
    // hidden first, then shown again inside: bwrap mounts in order
    let mut hide: Vec<PathBuf> = vec!["/home".into(), "/root".into(), "/run/user".into()];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
      && home != Path::new("/")
      && !home.starts_with("/home")
    {
      hide.push(home);
    }
    hide.push(self.data.to_path_buf());
    for dir in hide {
      // a dir that isn't there can't be mounted on (/ is read-only)
      if dir.is_dir() {
        a.push("--tmpfs".into());
        a.push(dir.into());
      }
    }
    for path in self.show {
      a.push("--ro-bind".into());
      a.push(path.into());
      a.push(path.into());
    }
    a.push("--bind".into());
    a.push(self.home.into());
    a.push(self.home.into());
    a.push("--bind".into());
    a.push(self.sock.into());
    a.push(self.sock.into());
    a.push("--chdir".into());
    a.push(self.home.into());
    if let Some(fd) = self.info_fd {
      a.push("--info-fd".into());
      a.push(fd.to_string().into());
    }
    a
  }
}

/// Why bwrap can't run here, if it can't.
pub fn check() -> Result<(), String> {
  let out = Command::new("bwrap")
    .args(["--die-with-parent", "--unshare-pid", "--unshare-ipc", "--unshare-uts"])
    .args(["--ro-bind", "/", "/", "--proc", "/proc", "--dev", "/dev", "true"])
    .output();
  match out {
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      Err("bwrap is not installed (on Debian and Ubuntu: sudo apt install bubblewrap)".into())
    }
    Err(e) => Err(format!("bwrap: {e}")),
    Ok(o) if o.status.success() => Ok(()),
    Ok(o) => Err(format!(
      "bwrap can't make namespaces here: {}",
      String::from_utf8_lossy(&o.stderr).trim()
    )),
  }
}
