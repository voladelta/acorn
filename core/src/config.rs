//! Config
//! ======
//! Acorn's settings. Its dir ($ACORN_DIR, default ~/.acorn) holds
//! config.json; the chats are in `chats`/<id> (default <dir>/chats). Each
//! key of config.json may be set as ACORN_<KEY> instead; a path may start
//! with "~/". The API key is DEEPSEEK_API_KEY, else config.json's
//! `deepseek_api_key` (an app opened from the Finder has no shell's
//! variables); no program the agent runs inherits it (`machine`). The
//! `budget` caps what every chat's calls spend a day (`chat::usage`).

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
/// The web server's token, when its host hands one in (`acorn-host`).
pub const TOKEN_VAR: &str = "ACORN_TOKEN";

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
  /// What agents run on (none: their assistant's effort).
  pub agents: Option<Effort>,
  pub view: usize,
  /// USD every chat's calls may spend together a day, here (none: no cap).
  pub budget: Option<f64>,
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
        .or_else(|| {
          file
            .get(key)
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
        })
        .unwrap_or_else(|| def.to_string())
    };
    let chat = get("chat", "default");
    if chat.is_empty() || !chat.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
      return Err(format!("chat is a name of letters, digits, _ and -: {chat:?}"));
    }
    let agents = get("agents", "");
    let rules = get("rules", "");
    let key = std::env::var(KEY_VAR)
      .ok()
      .filter(|k| !k.trim().is_empty())
      .unwrap_or_else(|| get("deepseek_api_key", ""));
    Ok(Config {
      chats: expand(&get("chats", &format!("{}/chats", dir.display()))),
      work: expand(&get("work", &format!("{}/work", dir.display()))),
      rules: (!rules.is_empty()).then(|| expand(&rules)),
      after: get("after", ""),
      pick: run("pick", &get("pick", "high"))?,
      compactor: run("compactor", &get("compactor", "low"))?,
      agents: if agents.is_empty() {
        None
      } else {
        Some(run("agents", &agents)?)
      },
      view: get("view", "")
        .parse()
        .ok()
        .filter(|&v| v > 0)
        .map_or(MOST_VIEW, |v: usize| v.min(MOST_VIEW)),
      budget: budget(&get("budget", ""))?,
      key: key.trim().to_string(),
      chat,
      dir,
    })
  }
}

/// The variables Acorn's own keys may be in, kept from every program the
/// agent runs (`machine`): DeepSeek's, Jev's, the server's token, and each
/// bridge's.
pub fn hidden() -> Vec<&'static str> {
  let mut all = vec![KEY_VAR, crate::decide::KEY_VAR, TOKEN_VAR];
  all.extend(crate::bridge::all().iter().flat_map(|b| b.secrets()));
  all
}

/// A key by its variable's name (EXA_API_KEY): the variable, else
/// config.json's lowercase key (exa_api_key), read now, so a key added counts
/// at once.
pub fn secret(name: &str) -> Result<String, String> {
  secret_in(&get().dir, name)
}

fn secret_in(dir: &std::path::Path, name: &str) -> Result<String, String> {
  if let Some(key) = std::env::var(name).ok().filter(|k| !k.trim().is_empty()) {
    return Ok(key.trim().to_string());
  }
  let file = dir.join("config.json");
  let file: HashMap<String, serde_json::Value> = match std::fs::read_to_string(&file) {
    Ok(text) => serde_json::from_str(&text).map_err(|e| format!("config.json: {e}"))?,
    Err(_) => HashMap::new(),
  };
  file
    .get(&name.to_lowercase())
    .and_then(|v| v.as_str())
    .map(str::trim)
    .filter(|k| !k.is_empty())
    .map(str::to_string)
    .ok_or_else(|| format!("no key: set {name}, or {} in config.json", name.to_lowercase()))
}

/// A path, "~/" as home.
pub fn expand(path: &str) -> PathBuf {
  path
    .strip_prefix("~/")
    .map_or_else(|| PathBuf::from(path), |rest| home().join(rest))
}

pub fn home() -> PathBuf {
  std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// A daily budget in USD: none if empty or 0.
fn budget(text: &str) -> Result<Option<f64>, String> {
  match text.trim().trim_start_matches('$') {
    "" => Ok(None),
    usd => match usd.parse::<f64>() {
      Ok(usd) if usd.is_finite() && usd >= 0.0 => Ok(Some(usd).filter(|&u| u > 0.0)),
      _ => Err(format!("budget is USD a day, as 5 or 0.5: {text}")),
    },
  }
}

/// `model:effort` or `effort`, the only model being deepseek-flash.
fn run(key: &str, text: &str) -> Result<Effort, String> {
  let (model, effort) = text.split_once(':').map_or((None, text), |(m, e)| (Some(m), e));
  if model.is_some_and(|m| m != crate::ai::deepseek::MODEL) {
    return Err(format!("{key} is no model:effort offered here: {text}"));
  }
  Effort::parse(effort).ok_or_else(|| format!("{key} is no effort offered here: {text}"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test(flavor = "multi_thread")]
  async fn no_program_the_agent_runs_sees_the_server_token() {
    let test = "config::tests::no_program_the_agent_runs_sees_the_server_token";
    if std::env::var_os("ACORN_TEST_TOKEN_CHILD").is_none() {
      let dir = tempfile::tempdir().unwrap();
      let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("ACORN_TEST_TOKEN_CHILD", "1")
        .env(TOKEN_VAR, "test-server-token")
        .env("ACORN_DIR", dir.path())
        .output()
        .unwrap();
      assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
      );
      return;
    }
    assert_eq!(std::env::var(TOKEN_VAR).unwrap(), "test-server-token");
    let dir = tempfile::tempdir().unwrap();
    let ran = crate::machine::shell(
      "if [ \"${ACORN_TOKEN+x}\" = x ]; then echo inherited; else echo absent; fi",
      dir.path(),
      10_000.0,
      tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(ran.code(), 0);
    assert_eq!(ran.tails(), ("absent\n".into(), String::new()));
  }

  #[test]
  fn a_budget_is_usd_a_day_or_none() {
    assert_eq!(budget(""), Ok(None));
    assert_eq!(budget("0"), Ok(None));
    assert_eq!(budget(" 5 "), Ok(Some(5.0)));
    assert_eq!(budget("$0.5"), Ok(Some(0.5)));
    assert!(budget("-1").is_err() && budget("five").is_err() && budget("inf").is_err());
  }

  #[test]
  fn a_secret_comes_from_the_file_as_it_is_now() {
    let dir = tempfile::tempdir().unwrap();
    let name = "ACORN_TEST_SECRET_UNSET";
    assert!(
      secret_in(dir.path(), name)
        .unwrap_err()
        .contains("acorn_test_secret_unset")
    );
    std::fs::write(dir.path().join("config.json"), r#"{"acorn_test_secret_unset": " k1 "}"#).unwrap();
    assert_eq!(secret_in(dir.path(), name).unwrap(), "k1");
    std::fs::write(dir.path().join("config.json"), r#"{"acorn_test_secret_unset": "k2"}"#).unwrap();
    assert_eq!(secret_in(dir.path(), name).unwrap(), "k2");
  }
}
