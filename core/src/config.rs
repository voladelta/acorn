//! Config
//! ======
//! Acorn's settings. Its dir ($ACORN_DIR, default ~/.acorn) holds
//! config.json; the chats are in `chats`/<id>. Each key of config.json may
//! be set as ACORN_<KEY> instead; a path may start with "~/". The API key
//! is DEEPSEEK_API_KEY, else config.json's `deepseek_api_key` (an app
//! opened from the Finder has no shell's variables); no program the agent
//! runs inherits it (`machine`).

use crate::effort::Effort;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The bytes past which a chat's view merges to half (`memory` HIGH): 2 MB
/// at most, ~500k tokens, fits DeepSeek's 1M context (a recall eval: 83% at
/// 128 KB, 100% at 2 MB); less, if set.
const MOST_VIEW: usize = 2_000_000;

/// The variable the API key is read from, kept out of every job's
/// environment.
pub const KEY_VAR: &str = "DEEPSEEK_API_KEY";

#[derive(Clone, Debug)]
pub struct Config {
  /// $ACORN_DIR: config.json.
  pub dir: PathBuf,
  /// Where the chats live, a dir each by id, and Acorn's.
  pub chats: PathBuf,
  pub chat: String,
  /// Where the agents' tools work: work/main (Acorn), work/<Name>.
  pub work: PathBuf,
  /// A file appended to every system prompt.
  pub rules: Option<PathBuf>,
  /// A shell command run after each turn.
  pub after: String,
  /// What Acorn thinks at.
  pub pick: Effort,
  /// What every compaction runs on, a chat's and an import's alike.
  pub compactor: Effort,
  /// What hired agents run on (none: their assistant's effort).
  pub agents: Option<Effort>,
  pub view: usize,
  pub key: String,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The config, read once.
pub fn get() -> &'static Config {
  CONFIG.get_or_init(|| Config::load().unwrap_or_else(|err| panic!("config: {err}")))
}

/// For tests: a config of its own, set before any read.
pub fn set(config: Config) {
  let _ = CONFIG.set(config);
}

impl Config {
  pub fn load() -> Result<Config, String> {
    let dir = expand(&std::env::var("ACORN_DIR").unwrap_or_else(|_| "~/.acorn".into()));
    let file: HashMap<String, serde_json::Value> = std::fs::read_to_string(dir.join("config.json"))
      .ok()
      .map(|text| serde_json::from_str(&text).map_err(|e| format!("config.json: {e}")))
      .transpose()?
      .unwrap_or_default();
    let get = |key: &str, def: &str| -> String {
      std::env::var(format!("ACORN_{}", key.to_uppercase()))
        .ok()
        .or_else(|| file.get(key).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string)))
        .unwrap_or_else(|| def.to_string())
    };
    let chat = get("chat", "default");
    if chat.is_empty() || !chat.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
      return Err(format!("chat is a name of letters, digits, _ and -: {chat:?}"));
    }
    let agents = get("agents", "");
    let rules = get("rules", "");
    let key = std::env::var(KEY_VAR).ok().filter(|k| !k.trim().is_empty()).unwrap_or_else(|| get("deepseek_api_key", ""));
    Ok(Config {
      chats: expand(&get("chats", "~/.chats")),
      work: expand(&get("work", &format!("{}/work", dir.display()))),
      rules: (!rules.is_empty()).then(|| expand(&rules)),
      after: get("after", ""),
      pick: run("pick", &get("pick", "high"))?,
      compactor: run("compactor", &get("compactor", "low"))?,
      agents: if agents.is_empty() { None } else { Some(run("agents", &agents)?) },
      view: get("view", "").parse().ok().filter(|&v| v > 0).map_or(MOST_VIEW, |v: usize| v.min(MOST_VIEW)),
      key: key.trim().to_string(),
      chat,
      dir,
    })
  }
}

/// A path, "~/" as home.
pub fn expand(path: &str) -> PathBuf {
  path.strip_prefix("~/").map_or_else(|| PathBuf::from(path), |rest| home().join(rest))
}

pub fn home() -> PathBuf {
  std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `model:effort` or `effort`, the only model being deepseek-flash.
fn run(key: &str, text: &str) -> Result<Effort, String> {
  let (model, effort) = text.split_once(':').map_or((None, text), |(m, e)| (Some(m), e));
  if model.is_some_and(|m| m != crate::ai::deepseek::MODEL) {
    return Err(format!("{key} is no model:effort offered here: {text}"));
  }
  Effort::parse(effort).ok_or_else(|| format!("{key} is no effort offered here: {text}"))
}
