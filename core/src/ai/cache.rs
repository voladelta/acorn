//! Cache
//! =====
//! A marked block ends a prefix DeepSeek caches. Single flight, as a
//! request is sent: one whose prefix no call cached within TTL leads; one
//! whose prefix a leader is writing waits until the leader's response
//! begins (only then can it read the entry). So a leader is always a
//! request being sent, never one waiting or yielding (`pace`), which every
//! call of its prefix would wait on.

use super::Ask;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How long we reuse a DeepSeek cache entry.
const TTL: Duration = Duration::from_secs(300);

/// A prefix cached: until when; while its leader's response has not begun,
/// what its waiters wait on.
struct Cached {
  until: Instant,
  ready: Option<watch::Receiver<bool>>,
}

/// The prefixes cached, by key; a process's calls share them.
static CACHE: LazyLock<Mutex<HashMap<u64, Cached>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A request's place in single flight: its response began; it ended.
pub struct Flight {
  lead: Option<(u64, watch::Sender<bool>)>,
}

impl Flight {
  pub fn begun(&mut self) {
    if let Some((key, tx)) = self.lead.take() {
      if let Some(c) = CACHE.lock().get_mut(&key) {
        c.ready = None;
        c.until = Instant::now() + TTL;
      }
      let _ = tx.send(true);
    }
  }

  pub fn end(&mut self) {
    if let Some((key, tx)) = self.lead.take() {
      CACHE.lock().remove(&key);
      let _ = tx.send(true);
    }
  }
}

impl Drop for Flight {
  fn drop(&mut self) {
    self.end();
  }
}

/// The keys of a call's prefix at each block end through its last mark
/// before its end: what it runs on, its system prompt and blocks, each
/// block's hash seeded by the one before.
fn keys(ask: &Ask) -> Vec<u64> {
  let blocks = &ask.blocks[..ask.blocks.len().saturating_sub(1)];
  let Some(last) = blocks.iter().rposition(|b| b.mark) else {
    return Vec::new();
  };
  let mut h = DefaultHasher::new();
  (super::deepseek::MODEL, ask.effort.as_str(), &ask.system, &ask.user).hash(&mut h);
  let mut key = h.finish();
  blocks[..=last]
    .iter()
    .map(|b| {
      let mut h = DefaultHasher::new();
      (key, &b.text).hash(&mut h);
      key = h.finish();
      key
    })
    .collect()
}

/// Waits while a leader writes the longest cached part of the call's
/// prefix; then leads, unless its whole prefix is cached.
pub async fn wait(ask: &Ask) -> Flight {
  let keys = keys(ask);
  loop {
    let wait = {
      let mut cache = CACHE.lock();
      let now = Instant::now();
      let live = |c: &Cached| c.ready.is_some() || c.until > now;
      let k = keys.iter().rposition(|key| cache.get(key).is_some_and(live));
      match k.map(|k| (k, cache.get_mut(&keys[k]).unwrap())) {
        Some((_, got)) if got.ready.is_some() => got.ready.clone(),
        found => {
          if let Some((_, got)) = found {
            got.until = now + TTL;
          }
          if keys.is_empty() || k == Some(keys.len() - 1) {
            return Flight { lead: None };
          }
          let key = *keys.last().unwrap();
          cache.retain(|_, c| live(c));
          let (tx, rx) = watch::channel(false);
          cache.insert(
            key,
            Cached {
              until: now,
              ready: Some(rx),
            },
          );
          return Flight { lead: Some((key, tx)) };
        }
      }
    };
    if let Some(mut rx) = wait {
      let _ = rx.wait_for(|&done| done).await;
    }
  }
}
