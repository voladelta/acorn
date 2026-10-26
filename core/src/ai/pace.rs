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
pub fn hold(model: &str, own: &str, why: &str) {
    let mut rooms = ROOMS.lock();
    let room = rooms.entry(model.to_string()).or_default();
    room.asking += 1;
    if room.asking == 1 {
        eprintln!("{model}: {}: its requests ask again", clip(why));
    }
    rooms.entry(own.to_string()).or_default().busy += 1;
}

/// ... and lets go of it, answered or not: the last frees those waiting.
pub fn free(model: &str, own: &str) {
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

fn clip(why: &str) -> &str {
    &why[..crate::clip::byte_at(why, 300)]
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
            let Some(go) = self.wait[0].pop_front().or_else(|| self.wait[1].pop_front()) else { break };
            // one stopped while it waited is gone: the next goes
            if go.send(self.round).is_ok() {
                self.start();
            }
        }
    }
}

/// A request's turn to start under its model's limit: the round it starts
/// in; None: stopped while it waited.
pub async fn fly(model: &str, yields: bool, cancel: &CancellationToken) -> Option<u64> {
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
            return Some(sem.round);
        }
        let (tx, rx) = oneshot::channel();
        sem.wait[usize::from(yields)].push_back(tx);
        rx
    };
    tokio::select! {
        got = &mut rx => got.ok(),
        _ = cancel.cancelled() => {
            // let in as it stopped: its place goes to the next
            rx.close();
            if rx.try_recv().is_ok() && let Some(sem) = SEMS.lock().get_mut(model) {
                sem.in_flight -= 1;
                sem.dispatch();
            }
            None
        }
    }
}

/// ... and its answer lands: limited, the limit is cut to CUT of what it
/// saw (once a round); else it grows by RISE.
pub fn landed(model: &str, round: u64, limited: bool) {
    let mut sems = SEMS.lock();
    let Some(sem) = sems.get_mut(model) else { return };
    sem.in_flight = sem.in_flight.saturating_sub(1);
    if limited && round == sem.round {
        let first = sem.concurrency.is_infinite();
        let base = if first { (sem.starts.len() as f64).max(1.0) } else { sem.concurrency };
        sem.concurrency = (sem.concurrency.min(base.max(1.0)) * CUT).max(MIN_CONCURRENCY);
        sem.round += 1;
        if first {
            eprintln!("{model}: rate limited: paced at {:.1} requests in flight", sem.concurrency);
        }
    } else if !limited && sem.concurrency < MAX_CONCURRENCY {
        sem.concurrency = (sem.concurrency + RISE).min(MAX_CONCURRENCY);
    }
    sem.dispatch();
}
