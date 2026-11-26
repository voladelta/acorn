//! Effort
//! ======
//! How hard a call thinks. An assistant's effort is part of who it is.

use serde::{Deserialize, Serialize};

/// The stored thinking preferences, mapped to the selected model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
  Low,
  Medium,
  #[default]
  High,
  Max,
}

pub const EFFORTS: [Effort; 4] = [Effort::Low, Effort::Medium, Effort::High, Effort::Max];

impl Effort {
  pub const fn as_str(self) -> &'static str {
    match self {
      Effort::Low => "low",
      Effort::Medium => "medium",
      Effort::High => "high",
      Effort::Max => "max",
    }
  }

  pub fn parse(s: &str) -> Option<Effort> {
    EFFORTS.into_iter().find(|e| e.as_str() == s)
  }
}

/// Every effort's name, as calls give them.
pub fn names() -> Vec<&'static str> {
  EFFORTS.map(Effort::as_str).to_vec()
}
