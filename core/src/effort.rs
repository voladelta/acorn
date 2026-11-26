//! Effort
//! ======
//! How hard a call thinks. An assistant's effort is part of who it is.

use serde::{Deserialize, Serialize};

/// The stored thinking preferences, mapped to the selected model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
  None,
  Low,
  High,
  Max,
}

pub const EFFORTS: [Effort; 4] = [Effort::None, Effort::Low, Effort::High, Effort::Max];

impl Effort {
  pub fn as_str(self) -> &'static str {
    match self {
      Effort::None => "none",
      Effort::Low => "low",
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
