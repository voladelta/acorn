//! Disk
//! ====
//! Durability by group commit. What Acorn writes goes at once to the OS's
//! cache, so no chat waits on the disk; a thread of its own makes it
//! durable, one flush at a time, each an fsync of every file written
//! before it began. A process crash loses nothing; a machine's, about the
//! last flush.

use parking_lot::{Condvar, Mutex};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

struct Disk {
  dirty: Mutex<HashSet<PathBuf>>,
  wake: Condvar,
}

static DISK: OnceLock<Disk> = OnceLock::new();

fn disk() -> &'static Disk {
  DISK.get_or_init(|| {
    std::thread::Builder::new().name("acorn-disk".into()).spawn(flusher).expect("the disk thread starts");
    Disk { dirty: Mutex::new(HashSet::new()), wake: Condvar::new() }
  })
}

/// A file written: flushed soon.
pub fn wrote(file: &Path) {
  let d = disk();
  d.dirty.lock().insert(file.to_path_buf());
  d.wake.notify_one();
}

fn flusher() {
  let d = disk();
  loop {
    let files = {
      let mut dirty = d.dirty.lock();
      while dirty.is_empty() {
        d.wake.wait(&mut dirty);
      }
      std::mem::take(&mut *dirty)
    };
    for file in files {
      // a file gone since needs none
      match std::fs::File::open(&file).and_then(|f| f.sync_all()) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
          eprintln!("disk: a flush of {} failed: {err}", file.display());
        }
        _ => {}
      }
    }
  }
}
