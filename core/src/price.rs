//! Standard Gemini API rates. Costs are recorded in micro-USD.

use crate::model::Model;
use chrono::{DateTime, Utc};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
  pub input: u64,
  pub write: u64,
  pub read: u64,
  pub output: u64,
}

/// Rate in milli-USD per million tokens: new input, cached input, output.
fn rate(model: Model, at: DateTime<Utc>) -> (u64, u64, u64) {
  match model {
    Model::Neon => (3_000, 150, 15_000),
    Model::Carbon if at.date_naive() < chrono::NaiveDate::from_ymd_opt(2026, 12, 1).unwrap() => (2_000, 100, 10_000),
    Model::Carbon => (4_000, 200, 20_000),
    Model::Hydrogen => (1_500, 75, 7_500),
  }
}

pub fn cost(model: Model, usage: Usage, at: DateTime<Utc>) -> u64 {
  let (input, cached, output) = rate(model, at);
  let total = (u128::from(usage.input) + u128::from(usage.write)) * u128::from(input)
    + u128::from(usage.read) * u128::from(cached)
    + u128::from(usage.output) * u128::from(output);
  u64::try_from(total.div_ceil(1_000)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn standard_rates_vary_by_model_and_apply_the_carbon_price_change() {
    let nov = "2026-11-30T23:59:59Z".parse().unwrap();
    let dec = "2026-12-01T00:00:00Z".parse().unwrap();
    let usage = Usage {
      input: 1_000_000,
      read: 1_000_000,
      output: 1_000_000,
      ..Usage::default()
    };
    assert_eq!(cost(Model::Neon, usage, nov), 18_150_000);
    assert_eq!(cost(Model::Hydrogen, usage, nov), 9_075_000);
    assert_eq!(cost(Model::Carbon, usage, nov), 12_100_000);
    assert_eq!(cost(Model::Carbon, usage, dec), 24_200_000);
  }
}
