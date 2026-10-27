//! Price
//! =====
//! What a request costs, in micro-USD: its usage, as DeepSeek tells it, at
//! its list rates (USD per million tokens), peak or off-peak by when it
//! ran. Shown after each turn (`chat`).

use chrono::{DateTime, Timelike, Utc};

/// Tokens of a request: new input, input written to the cache, input read
/// from it, output.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
  pub input: u64,
  pub write: u64,
  pub read: u64,
  pub output: u64,
}

/// USD per million tokens, each kind.
struct Rate {
  input: f64,
  write: f64,
  read: f64,
  output: f64,
}

const PEAK: Rate = Rate {
  input: 0.27,
  write: 0.27,
  read: 0.006,
  output: 1.10,
};
const OFF_PEAK: Rate = Rate {
  input: 0.14,
  write: 0.14,
  read: 0.003,
  output: 0.28,
};

/// DeepSeek's off-peak: 16:30 to 08:30 UTC.
pub fn off_peak(at: DateTime<Utc>) -> bool {
  let mins = at.hour() * 60 + at.minute();
  !(8 * 60 + 30..16 * 60 + 30).contains(&mins)
}

pub fn cost(use_: Usage, at: DateTime<Utc>) -> u64 {
  let r = if off_peak(at) { OFF_PEAK } else { PEAK };
  let micros = use_.input as f64 * r.input
    + use_.write as f64 * r.write
    + use_.read as f64 * r.read
    + use_.output as f64 * r.output;
  micros.ceil() as u64
}
