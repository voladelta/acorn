//! The host's book, `<data>/host.json`: the people, their unused join
//! codes and their sessions. Codes and sessions are kept as SHA-256 only.
//! `serve` and the commands (`invite`, `link`) both change it, so every
//! change holds `<data>/host.lock` and writes by temp file and rename.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// How long a join code is good for.
pub const CODE_SECS: u64 = 7 * 24 * 3600;
/// How long a session signs its device in: as long as its cookie lasts.
pub const SESSION_SECS: u64 = 365 * 24 * 3600;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
  #[serde(default)]
  pub people: Vec<Person>,
  #[serde(default)]
  pub codes: Vec<Code>,
  #[serde(default)]
  pub sessions: Vec<Session>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Person {
  pub id: String,
  pub name: String,
  /// Unix seconds.
  pub created: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Code {
  pub hash: String,
  pub user: String,
  pub expires: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
  pub hash: String,
  pub user: String,
  pub created: u64,
}

impl Session {
  /// Whether it still signs its device in at `now`.
  pub fn live(&self, now: u64) -> bool {
    now < self.created.saturating_add(SESSION_SECS)
  }
}

pub fn now() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_secs())
}

/// `n` random bytes, in hex.
pub fn random_hex(n: usize) -> String {
  let mut bytes = vec![0u8; n];
  std::fs::File::open("/dev/urandom")
    .and_then(|mut f| f.read_exact(&mut bytes))
    .expect("random bytes");
  bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `u` and 10 random lowercase letters and digits.
pub fn new_id() -> String {
  const ABC: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
  let mut bytes = [0u8; 10];
  std::fs::File::open("/dev/urandom")
    .and_then(|mut f| f.read_exact(&mut bytes))
    .expect("random bytes");
  // 252 is 7 * 36: the bytes above it would favor some letters
  let mut id = String::from("u");
  for b in bytes {
    let mut b = b;
    while b >= 252 {
      let mut one = [0u8; 1];
      std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut one))
        .expect("random bytes");
      b = one[0];
    }
    id.push(ABC[(b % 36) as usize] as char);
  }
  id
}

pub fn sha256(text: &str) -> String {
  Sha256::digest(text.as_bytes())
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}

/// Equal, in time that doesn't tell where they differ.
pub fn same(a: &str, b: &str) -> bool {
  a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// Whether `id` is one of ours in shape (it names a directory).
pub fn is_id(id: &str) -> bool {
  id.len() == 11 && id.starts_with('u') && id[1..].bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

pub struct Store {
  pub dir: PathBuf,
}

impl Store {
  pub fn new(dir: &Path) -> Store {
    Store { dir: dir.to_path_buf() }
  }

  fn file(&self) -> PathBuf {
    self.dir.join("host.json")
  }

  pub fn home(&self, id: &str) -> PathBuf {
    self.dir.join("users").join(id)
  }

  /// The book as it is on disk now.
  pub fn read(&self) -> Result<Book, String> {
    match std::fs::read_to_string(self.file()) {
      Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", self.file().display())),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
      Err(e) => Err(format!("{}: {e}", self.file().display())),
    }
  }

  /// Changes the book under the lock; what `f` returns is passed on.
  pub fn change<T>(&self, f: impl FnOnce(&mut Book) -> T) -> Result<T, String> {
    make_dir(&self.dir)?;
    let lock = std::fs::OpenOptions::new()
      .create(true)
      .truncate(false)
      .write(true)
      .mode(0o600)
      .open(self.dir.join("host.lock"))
      .map_err(|e| format!("host.lock: {e}"))?;
    // SAFETY: flock on an fd we hold open; it is let go when `lock` drops
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
      return Err(format!("host.lock: {}", std::io::Error::last_os_error()));
    }
    let mut book = self.read()?;
    let out = f(&mut book);
    let now = now();
    book.codes.retain(|c| c.expires > now);
    book.sessions.retain(|s| s.live(now));
    let text = serde_json::to_string_pretty(&book).map_err(|e| e.to_string())?;
    let tmp = self.dir.join(format!("host.json.{}", std::process::id()));
    std::fs::OpenOptions::new()
      .create(true)
      .truncate(true)
      .write(true)
      .mode(0o600)
      .open(&tmp)
      .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()).and_then(|()| f.sync_all()))
      .map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, self.file()).map_err(|e| format!("host.json: {e}"))?;
    Ok(out)
  }

  /// A new person, and a join code for them.
  pub fn invite(&self, name: &str) -> Result<(String, String), String> {
    let id = new_id();
    let code = random_hex(32);
    let hash = sha256(&code);
    let person = Person {
      id: id.clone(),
      name: name.to_string(),
      created: now(),
    };
    self.change(|book| {
      book.people.push(person);
      book.codes.push(Code {
        hash,
        user: id.clone(),
        expires: now() + CODE_SECS,
      });
    })?;
    make_dir(&self.home(&id))?;
    Ok((id, code))
  }

  /// Another join code for a person there is.
  pub fn link(&self, id: &str) -> Result<String, String> {
    let code = random_hex(32);
    let hash = sha256(&code);
    let found = self.change(|book| {
      let found = book.people.iter().any(|p| p.id == id);
      if found {
        book.codes.push(Code {
          hash,
          user: id.to_string(),
          expires: now() + CODE_SECS,
        });
      }
      found
    })?;
    if !found {
      return Err(format!("no person {id} (see `acorn-host users`)"));
    }
    Ok(code)
  }

  /// Whether `code` would still sign in, without burning it.
  pub fn live(&self, code: &str) -> Result<bool, String> {
    let hash = sha256(code);
    let now = now();
    Ok(
      self
        .read()?
        .codes
        .iter()
        .any(|c| same(&c.hash, &hash) && c.expires > now),
    )
  }

  /// Burns a good code: its person and a new session value.
  pub fn join(&self, code: &str) -> Result<Option<(String, String)>, String> {
    let hash = sha256(code);
    let session = random_hex(32);
    let session_hash = sha256(&session);
    self.change(|book| {
      let now = now();
      let i = book
        .codes
        .iter()
        .position(|c| same(&c.hash, &hash) && c.expires > now)?;
      let user = book.codes.remove(i).user;
      book.sessions.push(Session {
        hash: session_hash,
        user: user.clone(),
        created: now,
      });
      Some((user, session))
    })
  }

  /// Signs a person out of every device, their unused join codes gone
  /// too: how many sessions ended (None: no such person). `link` lets
  /// them back in.
  pub fn signout(&self, id: &str) -> Result<Option<usize>, String> {
    self.change(|book| {
      book.people.iter().any(|p| p.id == id).then(|| {
        let before = book.sessions.len();
        book.sessions.retain(|s| s.user != id);
        book.codes.retain(|c| c.user != id);
        before - book.sessions.len()
      })
    })
  }
}

/// Whose session `value` is, in `book`, if it hasn't expired.
pub fn who(book: &Book, value: &str) -> Option<String> {
  let hash = sha256(value);
  let now = now();
  // every one compared, so the time doesn't tell which matched
  let mut found = None;
  for s in &book.sessions {
    if same(&s.hash, &hash) && s.live(now) {
      found = Some(s.user.clone());
    }
  }
  found
}

/// A directory only its owner can enter.
pub fn make_dir(dir: &Path) -> Result<(), String> {
  std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
  std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("{}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn expired_session_is_rejected_and_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let st = Store::new(dir.path());
    let (id, code) = st.invite("Mom").unwrap();
    let (other, other_code) = st.invite("Dad").unwrap();
    let (_, session) = st.join(&code).unwrap().unwrap();
    let (_, others) = st.join(&other_code).unwrap().unwrap();
    assert_eq!(who(&st.read().unwrap(), &session), Some(id));

    // a session older than its cookie signs no one in, and is dropped
    st.change(|book| book.sessions[0].created = now() - SESSION_SECS)
      .unwrap();
    let book = st.read().unwrap();
    assert_eq!(who(&book, &session), None);
    assert_eq!(book.sessions.len(), 1, "the next change dropped it");
    assert_eq!(who(&book, &others), Some(other));
  }

  #[test]
  fn signout_revokes_all_sessions_and_unused_links_for_that_user_only() {
    let dir = tempfile::tempdir().unwrap();
    let st = Store::new(dir.path());
    let (id, code) = st.invite("Mom").unwrap();
    let (other, other_code) = st.invite("Dad").unwrap();
    let (_, session) = st.join(&code).unwrap().unwrap();
    let (_, others) = st.join(&other_code).unwrap().unwrap();
    let unused = st.link(&id).unwrap();

    assert_eq!(st.signout(&id).unwrap(), Some(1));
    let book = st.read().unwrap();
    assert_eq!(who(&book, &session), None);
    assert!(!st.live(&unused).unwrap());
    assert_eq!(who(&book, &others), Some(other), "no one else's");
    assert_eq!(st.signout("u000000000").unwrap(), None);
    assert!(
      st.join(&st.link(&id).unwrap()).unwrap().is_some(),
      "a new link lets them back in"
    );
  }
}
