//! Price
//! =====
//! What a request costs, in micro-USD: its usage, as DeepSeek tells it, at
//! its list rates (USD per million tokens), peak or off-peak by when it
//! ran. Shown after each turn (`chat`).

use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};

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
  input: 0.30,
  write: 0.30,
  read: 0.006,
  output: 1.20,
};
const OFF_PEAK: Rate = Rate {
  input: 0.15,
  write: 0.15,
  read: 0.003,
  output: 0.60,
};

/// DeepSeek's off-peak: all but 01:00 to 04:00 and 06:00 to 10:00 UTC,
/// Monday to Friday. Its Chinese public holidays, off-peak too, count
/// here as any weekday: a cost is told too high rather than too low.
pub fn off_peak(at: DateTime<Utc>) -> bool {
  let hour = at.hour();
  matches!(at.weekday(), Weekday::Sat | Weekday::Sun) || !((1..4).contains(&hour) || (6..10).contains(&hour))
}

pub fn cost(use_: Usage, at: DateTime<Utc>) -> u64 {
  let r = if off_peak(at) { OFF_PEAK } else { PEAK };
  let micros = use_.input as f64 * r.input
    + use_.write as f64 * r.write
    + use_.read as f64 * r.read
    + use_.output as f64 * r.output;
  micros.ceil() as u64
}

#[cfg(test)]
mod tests {
  use super::*;
  use chrono::TimeZone;

  fn at(day: u32, hour: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, day, hour, min, 0).unwrap()
  }

  #[test]
  fn peak_is_two_spans_of_a_weekday() {
    // the 20th: a Friday
    for (hour, min, off) in [
      (0, 59, true),
      (1, 0, false),
      (3, 59, false),
      (4, 0, true),
      (5, 59, true),
      (6, 0, false),
      (9, 59, false),
      (10, 0, true),
      (16, 0, true),
    ] {
      assert_eq!(off_peak(at(20, hour, min)), off, "Friday {hour}:{min:02}");
    }
    assert!(off_peak(at(21, 2, 0)), "Saturday");
    assert!(off_peak(at(22, 7, 0)), "Sunday");
    assert!(!off_peak(at(23, 7, 0)), "Monday");
  }

  #[test]
  fn a_request_costs_its_tokens_at_the_list_rates() {
    let million = 1_000_000;
    let all = Usage {
      input: million,
      write: million,
      read: million,
      output: million,
    };
    assert_eq!(cost(all, at(20, 2, 0)), 300_000 + 300_000 + 6_000 + 1_200_000);
    assert_eq!(cost(all, at(20, 12, 0)), 150_000 + 150_000 + 3_000 + 600_000);
  }
}
