//! Pick
//! ====
//! What a chat's calls run on: effort alone. DeepSeek Flash is the model
//! at the protocol boundary (`ai::deepseek`). A chat that saved none takes
//! the config's. Up and down step through the efforts.

use serde::{Deserialize, Serialize};

/// The thinking efforts DeepSeek's Responses API takes.
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

    /// What it means, said where it is picked.
    pub fn hint(self) -> String {
        format!("{} effort: how long it thinks first; more is smarter, slower and costs more", self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pick {
    pub effort: Effort,
}

/// A key's change to the pick: a step (ends stop it), or around.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
    Around,
}

impl Pick {
    pub fn label(self) -> &'static str {
        self.effort.as_str()
    }

    /// The pick a move makes; `to`: to the effort named, or the only one
    /// whose name holds it (None: no such).
    pub fn moved(self, mv: Move, to: Option<&str>) -> Option<Pick> {
        let n = EFFORTS.len() as i32;
        let at = EFFORTS.iter().position(|&e| e == self.effort).unwrap_or(0) as i32;
        let effort = match to {
            Some(to) => {
                let like: Vec<Effort> = EFFORTS.into_iter().filter(|e| e.as_str().contains(&to.to_lowercase())).collect();
                Effort::parse(to).or(if like.len() == 1 { Some(like[0]) } else { None })?
            }
            None => {
                let next = match mv {
                    Move::Up => (at + 1).min(n - 1),
                    Move::Down => (at - 1).max(0),
                    Move::Around => (at + 1) % n,
                };
                EFFORTS[next as usize]
            }
        };
        Some(Pick { effort })
    }

    /// As the chat saved it (its state's `pick`); None: none, or not one
    /// offered here.
    pub fn read(x: &serde_json::Value) -> Option<Pick> {
        let model = x.get("model");
        if model.is_some_and(|m| m != crate::ai::deepseek::MODEL) {
            return None;
        }
        Effort::parse(x.get("effort")?.as_str()?).map(|effort| Pick { effort })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_stop_at_the_ends() {
        let p = Pick { effort: Effort::Max };
        assert_eq!(p.moved(Move::Up, None).unwrap().effort, Effort::Max);
        assert_eq!(p.moved(Move::Around, None).unwrap().effort, Effort::None);
        assert_eq!(p.moved(Move::Around, Some("lo")).unwrap().effort, Effort::Low);
        assert!(p.moved(Move::Around, Some("q")).is_none());
        assert_eq!(Pick::read(&serde_json::json!({"effort": "low"})), Some(Pick { effort: Effort::Low }));
        assert_eq!(Pick::read(&serde_json::json!({"model": "gpt", "effort": "low"})), None);
    }
}
