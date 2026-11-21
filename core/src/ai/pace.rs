//! Pace
//! ====
//! Who asks a busy model, and when requests start.
//!
//! Rooms: a request asking a busy model again holds its room, its model's
//! (if it doesn't yield) or its import's (one request at a time; the rest
//! of that import wait on it), and a request that yields sends nothing
//! while a request that doesn't holds its model's room, nor while another
//! holds its import's: the room a busy service has goes to the chat first,
//! and imports share what is left alike, however many requests each runs.
//! A busy model is logged as its first request asks again and as its last
//! is done, never each try.
//!
//! Pace: requests to a model start under its in-flight limit, none till it
//! first answers a rate limit (429, 529), then CUT of the rate it saw,
//! growing back by RISE a request; those over it wait, a call that doesn't
//! yield before one that does.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

const CUT: f64 = 0.7;
const RISE: f64 = 0.2;
const MIN_CONCURRENCY: f64 = 1.0;
const MAX_CONCURRENCY: f64 = 128.0;

// Rooms
// =====

#[derive(Default)]
struct Room {
  busy: usize,
  asking: usize,
}

static ROOMS: LazyLock<Mutex<HashMap<String, Room>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static FREED: Notify = Notify::const_new();

/// Whether some request asking again holds a room.
pub fn held(key: &str) -> bool {
  ROOMS.lock().get(key).is_some_and(|r| r.busy > 0)
}

/// A request asks `model` again, holding `own`, its model's room or its
/// import's; the model's first says why.
pub fn hold(model: &str, own: &str, why: &str) -> Hold {
  let mut rooms = ROOMS.lock();
  let room = rooms.entry(model.to_string()).or_default();
  room.asking += 1;
  if room.asking == 1 {
    eprintln!("{model}: {}: its requests ask again", crate::clip::head(why, 300));
  }
  rooms.entry(own.to_string()).or_default().busy += 1;
  Hold {
    model: model.to_string(),
    own: own.to_string(),
  }
}

pub struct Hold {
  model: String,
  own: String,
}

impl Drop for Hold {
  fn drop(&mut self) {
    free(&self.model, &self.own);
  }
}

/// ... and lets go of it, answered or not: the last frees those waiting.
fn free(model: &str, own: &str) {
  let mut rooms = ROOMS.lock();
  if let Some(room) = rooms.get_mut(own) {
    room.busy -= 1;
  }
  if let Some(room) = rooms.get_mut(model) {
    room.asking -= 1;
    if room.asking == 0 {
      eprintln!("{model}: none asks again");
    }
  }
  rooms.retain(|_, r| r.busy > 0 || r.asking > 0);
  FREED.notify_waiters();
}

/// A request that yields waits while a request that doesn't holds its
/// model's room, and while another holds its import's.
pub async fn yielded(keys: &[&str], cancel: &CancellationToken) {
  loop {
    let freed = FREED.notified();
    tokio::pin!(freed);
    freed.as_mut().enable();
    if cancel.is_cancelled() || !keys.iter().any(|k| held(k)) {
      return;
    }
    tokio::select! {
        _ = freed => {}
        _ = cancel.cancelled() => return,
    }
  }
}

// Pace
// ====

/// A model's in-flight limit: requests in flight, its limit, the round of
/// its last cut, those waiting (calls that don't yield, then those that
/// do), when its requests started in the last second.
struct Sem {
  in_flight: usize,
  concurrency: f64,
  round: u64,
  wait: [VecDeque<oneshot::Sender<u64>>; 2],
  starts: VecDeque<Instant>,
}

static SEMS: LazyLock<Mutex<HashMap<String, Sem>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

impl Sem {
  fn start(&mut self) {
    self.in_flight += 1;
    let now = Instant::now();
    self.starts.push_back(now);
    while self.starts.front().is_some_and(|&t| now - t > Duration::from_secs(1)) {
      self.starts.pop_front();
    }
  }

  /// Those waiting start, as the limit lets them.
  fn dispatch(&mut self) {
    while (self.in_flight as f64) < self.concurrency.floor() {
      let Some(go) = self.wait[0].pop_front().or_else(|| self.wait[1].pop_front()) else {
        break;
      };
      // one stopped while it waited is gone: the next goes
      if go.send(self.round).is_ok() {
        self.start();
      }
    }
  }
}

/// A request's place in flight under its model's limit (`fly`), held till
/// it is let go: as its answer lands (`land`), or dropped first, its
/// request cut, which tells the limit nothing. Either way its place goes
/// to the next.
pub struct Seat {
  model: String,
  round: u64,
  limited: Option<bool>,
}

impl Seat {
  fn new(model: &str, round: u64) -> Seat {
    Seat {
      model: model.to_string(),
      round,
      limited: None,
    }
  }

  /// Its answer landed: limited, the limit is cut to CUT of what it saw
  /// (once a round); else it grows by RISE.
  pub fn land(mut self, limited: bool) {
    self.limited = Some(limited);
  }
}

impl Drop for Seat {
  fn drop(&mut self) {
    let mut sems = SEMS.lock();
    let Some(sem) = sems.get_mut(&self.model) else { return };
    sem.in_flight = sem.in_flight.saturating_sub(1);
    match self.limited {
      Some(true) if self.round == sem.round => {
        let first = sem.concurrency.is_infinite();
        let base = if first {
          (sem.starts.len() as f64).max(1.0)
        } else {
          sem.concurrency
        };
        sem.concurrency = (base * CUT).max(MIN_CONCURRENCY);
        sem.round += 1;
        if first {
          eprintln!(
            "{}: rate limited: paced at {:.1} requests in flight",
            self.model, sem.concurrency
          );
        }
      }
      Some(false) if sem.concurrency < MAX_CONCURRENCY => {
        sem.concurrency = (sem.concurrency + RISE).min(MAX_CONCURRENCY);
      }
      _ => {}
    }
    sem.dispatch();
  }
}

/// A request's turn to start under its model's limit: its seat; None:
/// stopped while it waited.
pub async fn fly(model: &str, yields: bool, cancel: &CancellationToken) -> Option<Seat> {
  let mut rx = {
    let mut sems = SEMS.lock();
    let sem = sems.entry(model.to_string()).or_insert_with(|| Sem {
      in_flight: 0,
      concurrency: f64::INFINITY,
      round: 0,
      wait: [VecDeque::new(), VecDeque::new()],
      starts: VecDeque::new(),
    });
    if cancel.is_cancelled() {
      return None;
    }
    if (sem.in_flight as f64) < sem.concurrency.floor() {
      sem.start();
      return Some(Seat::new(model, sem.round));
    }
    let (tx, rx) = oneshot::channel();
    sem.wait[usize::from(yields)].push_back(tx);
    rx
  };
  tokio::select! {
      got = &mut rx => got.ok().map(|round| Seat::new(model, round)),
      _ = cancel.cancelled() => {
          // let in as it stopped: its seat, dropped, goes to the next
          rx.close();
          drop(rx.try_recv().ok().map(|round| Seat::new(model, round)));
          None
      }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_dropped_hold_frees_its_room_even_on_panic() {
    let model = "pace-test-hold";
    let own = "pace-test-hold-import";
    let guard = hold(model, own, "busy");
    assert!(held(own));
    let result = std::panic::catch_unwind(move || {
      let _hold = guard;
      panic!("cut");
    });
    assert!(result.is_err());
    assert!(!held(own));
    assert!(!ROOMS.lock().contains_key(model));
    let hold = hold(model, own, "busy");
    drop(hold);
    assert!(!held(own));
  }

  fn in_flight(model: &str) -> usize {
    SEMS.lock().get(model).map_or(0, |s| s.in_flight)
  }

  /// A request cut in flight (its seat dropped, never landed) gives its
  /// place to the next, and says nothing of the limit.
  #[tokio::test]
  async fn a_seat_cut_in_flight_goes_to_the_next() {
    let model = "pace-test-cut";
    let go = CancellationToken::new();
    fly(model, false, &go).await.unwrap().land(true);
    let limit = SEMS.lock()[model].concurrency;
    assert_eq!(limit, MIN_CONCURRENCY, "a rate limit paces at one in flight");
    let cut = fly(model, false, &go).await.unwrap();
    let next = tokio::spawn(async move { fly(model, false, &CancellationToken::new()).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!next.is_finished(), "the next waits while one flies");
    drop(cut);
    let next = tokio::time::timeout(Duration::from_secs(1), next)
      .await
      .expect("the cut seat's place goes to the next")
      .unwrap()
      .expect("a seat");
    assert_eq!(SEMS.lock()[model].concurrency, limit, "a cut tells the limit nothing");
    drop(next);
    assert_eq!(in_flight(model), 0);
  }

  /// One stopped while it waits takes no place.
  #[tokio::test]
  async fn a_stop_while_waiting_takes_no_place() {
    let model = "pace-test-stop";
    let go = CancellationToken::new();
    fly(model, false, &go).await.unwrap().land(true);
    let flying = fly(model, false, &go).await.unwrap();
    let stop = CancellationToken::new();
    let waiting = {
      let stop = stop.clone();
      tokio::spawn(async move { fly(model, false, &stop).await.is_none() })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    stop.cancel();
    assert!(waiting.await.unwrap(), "stopped, it gets no seat");
    flying.land(false);
    assert_eq!(in_flight(model), 0);
  }
}
