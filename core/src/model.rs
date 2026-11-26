use crate::effort::Effort;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Model {
  #[default]
  #[serde(rename = "gemini-4-neon")]
  Neon,
  #[serde(rename = "gemini-4-carbon")]
  Carbon,
  #[serde(rename = "gemini-4-hydrogen")]
  Hydrogen,
}

impl Model {
  pub const fn name(self) -> &'static str {
    match self {
      Self::Neon => "gemini-4-neon",
      Self::Carbon => "gemini-4-carbon",
      Self::Hydrogen => "gemini-4-hydrogen",
    }
  }

  /// Older effort settings remain readable. Gemini 4's minimum is low.
  pub const fn thinking(self, effort: Effort) -> &'static str {
    match (self, effort) {
      (_, Effort::None | Effort::Low) => "low",
      (_, Effort::High) | (Self::Carbon, Effort::Max) => "high",
      (_, Effort::Max) => "xhigh",
    }
  }

  pub fn parse(name: &str) -> Option<Self> {
    match name.trim() {
      "neon" | "gemini-4-neon" => Some(Self::Neon),
      "carbon" | "gemini-4-carbon" => Some(Self::Carbon),
      "hydrogen" | "gemini-4-hydrogen" => Some(Self::Hydrogen),
      _ => None,
    }
  }
}
