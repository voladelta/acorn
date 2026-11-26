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

  /// The model's recommended default reasoning effort in Gemini 4.
  pub const fn default_effort(self) -> Effort {
    match self {
      Self::Neon | Self::Hydrogen => Effort::High,
      Self::Carbon => Effort::Medium,
    }
  }

  pub const fn thinking(self, effort: Effort) -> &'static str {
    match (self, effort) {
      (_, Effort::Low) => "low",
      (_, Effort::Medium) => "medium",
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn models_map_efforts_and_recommended_defaults() {
    assert_eq!(Model::Neon.default_effort(), Effort::High);
    assert_eq!(Model::Hydrogen.default_effort(), Effort::High);
    assert_eq!(Model::Carbon.default_effort(), Effort::Medium);

    assert_eq!(Model::Neon.thinking(Effort::Low), "low");
    assert_eq!(Model::Neon.thinking(Effort::Medium), "medium");
    assert_eq!(Model::Neon.thinking(Effort::High), "high");
    assert_eq!(Model::Neon.thinking(Effort::Max), "xhigh");

    assert_eq!(Model::Carbon.thinking(Effort::Medium), "medium");
    assert_eq!(Model::Carbon.thinking(Effort::Max), "high");
  }
}
