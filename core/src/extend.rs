//! Extend
//! ======
//! How an assistant knows itself and grows. `abilities` says what it is
//! and has: who it is, its tools, luau's globals and which plugin gives
//! each, the plugins and what is wrong with them, the skills, and where
//! each lives, Acorn's own source among them. `extend` installs, replaces
//! or removes a skill or a plugin, checked first: a plugin is loaded with
//! all the others, as a VM would load them, before it is put in place,
//! so a broken one never replaces a working one. What it replaces or
//! removes goes to the attic ($ACORN_DIR/attic), so it can be taken back.

use crate::ai::Spec;
use crate::{config, luau, skills};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// What the prompts say of growing.
pub fn note() -> String {
  format!(
    "# Growing

You can see what you are and grow. abilities lists your tools, luau's globals and
the plugin behind each, the plugins and skills, and where each lives, Acorn's
source among them. extend installs a skill or a plugin, checked before it is
put in place, or removes one; what it replaces goes to {attic}, where
extend can bring it back from.

Grow when it pays: when the user corrects how you do a kind of task twice, or
gives you steps to keep, write them as a skill; when a task needs a service, an
API or a routine you lack and will need again, write a plugin. Say what you
added and why. Never grow to get around the user's word, a key you weren't
given or a limit they set. A change to Acorn itself (a new tool, the app) is
code in its source: do it only when asked, and say it needs a rebuild.",
    attic = attic(&config::get().dir).display(),
  )
}

fn attic(dir: &Path) -> PathBuf {
  dir.join("attic")
}

/// A name for a skill's dir or a plugin's file: letters, digits, `_`, `-`.
fn valid(name: &str) -> Result<&str, String> {
  let name = name.trim();
  let ok = !name.is_empty()
    && name.len() <= 64
    && !name.starts_with(['-', '.'])
    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
  if ok {
    Ok(name)
  } else {
    Err(format!(
      "{name:?} can't be a name: use letters, digits, _ and - (64 at most)."
    ))
  }
}

fn now() -> u128 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |d| d.as_millis())
}

/// `from` moved to the attic as `<kind>-<name>-<time><ext>`; where it went.
fn shelve(dir: &Path, from: &Path, kind: &str, name: &str, ext: &str) -> Result<PathBuf, String> {
  let attic = attic(dir);
  fs::create_dir_all(&attic).map_err(|e| format!("attic: {e}"))?;
  let stem = format!("{kind}-{name}-{}", now());
  // Two in one millisecond: the later one gets a number, never the same name.
  let to = (0..)
    .map(|n| match n {
      0 => attic.join(format!("{stem}{ext}")),
      n => attic.join(format!("{stem}-{n}{ext}")),
    })
    .find(|p| !p.exists())
    .expect("some name is free");
  fs::rename(from, &to).map_err(|e| format!("attic: {e}"))?;
  Ok(to)
}

/// `text` written to `to` whole or not at all.
fn put(to: &Path, text: &str) -> Result<(), String> {
  let tmp = to.with_file_name(format!(
    ".{}.{}.tmp",
    to.file_name().unwrap_or_default().to_string_lossy(),
    std::process::id()
  ));
  fs::write(&tmp, text).and_then(|()| fs::rename(&tmp, to)).map_err(|e| {
    let _ = fs::remove_file(&tmp);
    e.to_string()
  })
}

/// Install, replace or (with no text) remove a skill or a plugin, under
/// Acorn's dir `dir`.
pub fn extend(dir: &Path, kind: &str, name: &str, text: &str) -> Result<String, String> {
  let name = valid(name)?;
  match kind.trim() {
    "skill" => skill(dir, name, text),
    "plugin" => plugin(dir, name, text),
    other => Err(format!("No kind {other:?}: skill or plugin.")),
  }
}

fn skill(dir: &Path, name: &str, text: &str) -> Result<String, String> {
  let at = dir.join("skills").join(name);
  let file = at.join("SKILL.md");
  if text.trim().is_empty() {
    if !at.is_dir() {
      return Err(format!("No skill {name} in {}.", dir.join("skills").display()));
    }
    let to = shelve(dir, &at, "skill", name, "")?;
    return Ok(format!("Removed skill {name}; it is kept in {}.", to.display()));
  }
  let (head, body) = skills::split(text);
  let Some(about) = skills::field(head, "description") else {
    return Err("A skill starts with a --- block holding a description: line saying when it applies.".into());
  };
  if body.trim().is_empty() {
    return Err("The skill has no steps after its --- block.".into());
  }
  fs::create_dir_all(&at).map_err(|e| e.to_string())?;
  let kept = if file.exists() {
    let to = shelve(dir, &file, "skill", name, ".md")?;
    format!(" The old one is kept in {}.", to.display())
  } else {
    String::new()
  };
  put(&file, text)?;
  Ok(format!(
    "Installed skill {name} ({about}) in {}; list_skills shows it now.{kept}",
    file.display()
  ))
}

fn plugin(dir: &Path, name: &str, text: &str) -> Result<String, String> {
  let plugins = dir.join("plugins");
  let file = plugins.join(format!("{name}.luau"));
  if text.trim().is_empty() {
    if !file.is_file() {
      return Err(format!("No plugin {name} in {}.", plugins.display()));
    }
    let to = shelve(dir, &file, "plugin", name, ".luau")?;
    return Ok(format!(
      "Removed plugin {name}; it is kept in {}. It counts from your next luau call.",
      to.display()
    ));
  }
  let said = check(dir, &plugins, name, text)?;
  fs::create_dir_all(&plugins).map_err(|e| e.to_string())?;
  let kept = if file.exists() {
    let to = shelve(dir, &file, "plugin", name, ".luau")?;
    format!(" The old one is kept in {}.", to.display())
  } else {
    String::new()
  };
  put(&file, text)?;
  Ok(format!(
    "Installed plugin {name} in {}; it counts from your next luau call.\n{said}{kept}",
    file.display()
  ))
}

/// The plugin loaded beside every other, in a dir of their copies: what it
/// gives and what is wrong. One that fails to load is refused, but for a
/// key not set yet: that the user sets, and it loads from then on.
fn check(dir: &Path, plugins: &Path, name: &str, text: &str) -> Result<String, String> {
  let lab = attic(dir).join(format!(".check-{}-{}", std::process::id(), now()));
  fs::create_dir_all(&lab).map_err(|e| e.to_string())?;
  let got = (|| {
    for e in fs::read_dir(plugins).into_iter().flatten().flatten() {
      let path = e.path();
      if path.extension().is_some_and(|x| x == "luau") && path.file_stem().is_some_and(|s| s != name) {
        fs::copy(&path, lab.join(e.file_name())).map_err(|e| e.to_string())?;
      }
    }
    fs::write(lab.join(format!("{name}.luau")), text).map_err(|e| e.to_string())?;
    luau::layer_of(&lab)
  })();
  let _ = fs::remove_dir_all(&lab);
  let (docs, provided, warns) = got?;
  let failed = format!("plugin {name} did not load: ");
  let mut said = Vec::new();
  for warn in &warns {
    if let Some(why) = warn.strip_prefix(&failed) {
      if !why.contains("no key: ") {
        return Err(format!("Not installed: it did not load: {why}"));
      }
      said.push(format!(
        "It loads once its key is set ({why}): tell the user to put it in {} themselves, never in a message.",
        dir.join("config.json").display()
      ));
    } else if warn.contains(&format!("plugin {name}")) {
      said.push(warn.clone());
    }
  }
  let mine: Vec<_> = provided
    .iter()
    .filter(|(_, by)| by.as_str() == name)
    .filter_map(|(global, _)| docs.get(global).cloned())
    .collect();
  if mine.is_empty() && said.is_empty() {
    said.push("It gives no globals.".into());
  }
  Ok(
    mine
      .iter()
      .map(|d| format!("+ {d}"))
      .chain(said)
      .collect::<Vec<_>>()
      .join("\n"),
  )
}

/// Where Acorn's own source is, if this machine has it.
fn source() -> Option<PathBuf> {
  let core = Path::new(env!("CARGO_MANIFEST_DIR"));
  let root = core.parent()?;
  root.join("Cargo.toml").is_file().then(|| root.to_path_buf())
}

/// A tool's doc, cut to its first sentence.
fn gist(info: &str) -> &str {
  let end = info.find(". ").map_or(info.len(), |i| i + 1);
  &info[..end]
}

/// What the caller is and has: `who` (its name and part), its tools
/// (`specs`), its work dir, and Acorn's dirs under `dir`.
pub fn abilities(dir: &Path, who: &str, specs: &[Spec], work: &Path, project: &str) -> String {
  let mut out = format!("# You\n\n{who}\nWork dir: {}\n", work.display());
  out += &format!("Acorn's dir: {} (config.json, plugins, skills, attic)\n", dir.display());
  match source() {
    Some(src) => out += &format!("Acorn's source: {} (Rust; a change needs a rebuild)\n", src.display()),
    None => out += "Acorn's source: https://github.com/voladelta/acorn (not on this machine)\n",
  }
  out += "\n# Tools\n\n";
  for s in specs {
    out += &format!("- {}: {}\n", s.name, gist(&s.info));
  }
  let plugins = dir.join("plugins");
  out += &format!("\n# luau's globals (plugins in {})\n\n", plugins.display());
  match luau::layer_of(&plugins) {
    Ok((docs, provided, warns)) => {
      for (name, doc) in &docs {
        match provided.get(name) {
          Some(by) => out += &format!("- {doc}   [plugin {by}]\n"),
          None => out += &format!("- {doc}\n"),
        }
      }
      for w in warns {
        out += &format!("! {w}\n");
      }
    }
    Err(why) => out += &format!("! cannot load them: {why}\n"),
  }
  out += "\n# Skills\n\n";
  out += &skills::list(&skills::roots(work, project));
  out += "\n\nextend adds, replaces or removes a skill or a plugin.";
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ls(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
      .into_iter()
      .flatten()
      .flatten()
      .map(|e| e.file_name().to_string_lossy().into_owned())
      .collect();
    names.sort();
    names
  }

  const GOOD: &str =
    "return { shout = { doc = 'shout(s: string): string -- loud', value = function(s) return s:upper() end } }";

  #[test]
  fn a_plugin_that_loads_is_installed_and_one_that_breaks_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let said = extend(dir.path(), "plugin", "loud", GOOD).unwrap();
    assert!(said.contains("+ shout(s: string)"), "{said}");
    let file = dir.path().join("plugins/loud.luau");
    assert_eq!(fs::read_to_string(&file).unwrap(), GOOD);

    let why = extend(dir.path(), "plugin", "loud", "return { shout = 1 }").unwrap_err();
    assert!(why.starts_with("Not installed"), "{why}");
    assert_eq!(fs::read_to_string(&file).unwrap(), GOOD, "the working one stays");
    assert!(
      ls(&dir.path().join("attic")).is_empty(),
      "no check dir left: {:?}",
      ls(&dir.path().join("attic"))
    );

    let why = extend(dir.path(), "plugin", "bad", "return {").unwrap_err();
    assert!(why.starts_with("Not installed"), "{why}");
    assert!(!dir.path().join("plugins/bad.luau").exists());
  }

  #[test]
  fn a_replaced_or_removed_plugin_goes_to_the_attic() {
    let dir = tempfile::tempdir().unwrap();
    extend(dir.path(), "plugin", "loud", GOOD).unwrap();
    let again = GOOD.replace("loud", "LOUD");
    let said = extend(dir.path(), "plugin", "loud", &again).unwrap();
    assert!(said.contains("The old one is kept in"), "{said}");
    let kept = ls(&dir.path().join("attic"));
    assert_eq!(kept.len(), 1);
    assert_eq!(
      fs::read_to_string(dir.path().join("attic").join(&kept[0])).unwrap(),
      GOOD
    );

    let said = extend(dir.path(), "plugin", "loud", "  ").unwrap();
    assert!(said.starts_with("Removed plugin loud"), "{said}");
    assert!(!dir.path().join("plugins/loud.luau").exists());
    assert_eq!(ls(&dir.path().join("attic")).len(), 2);
    assert!(extend(dir.path(), "plugin", "loud", "").is_err());
  }

  #[test]
  fn a_plugin_waiting_for_its_key_is_installed_with_a_word_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let src =
      "local k = secret('ACORN_EXTEND_NO_SUCH_KEY')\nreturn { f = { doc = 'f()', value = function() return k end } }";
    let said = extend(dir.path(), "plugin", "keyed", src).unwrap();
    assert!(said.contains("loads once its key is set"), "{said}");
    assert!(dir.path().join("plugins/keyed.luau").exists());
  }

  #[test]
  fn a_plugin_replacing_anothers_global_says_so() {
    let dir = tempfile::tempdir().unwrap();
    extend(dir.path(), "plugin", "a", GOOD).unwrap();
    let said = extend(dir.path(), "plugin", "b", GOOD).unwrap();
    assert!(said.contains("shout of plugin a is replaced by plugin b's"), "{said}");
  }

  #[test]
  fn a_skill_needs_a_description_and_steps() {
    let dir = tempfile::tempdir().unwrap();
    assert!(extend(dir.path(), "skill", "notes", "Just steps.").is_err());
    assert!(extend(dir.path(), "skill", "notes", "---\ndescription: d\n---\n").is_err());
    let text = "---\ndescription: Meeting notes, the user's way\n---\n1. Decisions first.\n";
    let said = extend(dir.path(), "skill", "notes", text).unwrap();
    assert!(
      said.contains("Installed skill notes (Meeting notes, the user's way)"),
      "{said}"
    );
    assert_eq!(
      skills::list(&[dir.path().join("skills")]),
      "notes: Meeting notes, the user's way"
    );
    extend(dir.path(), "skill", "notes", &text.replace("first", "last")).unwrap();
    assert_eq!(ls(&dir.path().join("attic")).len(), 1);
    assert!(
      extend(dir.path(), "skill", "notes", "")
        .unwrap()
        .starts_with("Removed skill notes")
    );
    assert_eq!(skills::list(&[dir.path().join("skills")]), "No skills yet.");
  }

  #[test]
  fn names_stay_in_their_dir() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["../x", "a/b", "", ".hidden", "-x", "a b"] {
      assert!(extend(dir.path(), "plugin", name, GOOD).is_err(), "{name}");
      assert!(
        extend(dir.path(), "skill", name, "---\ndescription: d\n---\nS.").is_err(),
        "{name}"
      );
    }
    assert!(extend(dir.path(), "theme", "x", GOOD).is_err());
  }

  #[test]
  fn abilities_name_the_tools_globals_and_their_plugins() {
    let dir = tempfile::tempdir().unwrap();
    extend(dir.path(), "plugin", "loud", GOOD).unwrap();
    let specs = vec![crate::machine::spec(
      "zoom",
      "Open a line. More words.",
      serde_json::json!({}),
      &[],
    )];
    let said = abilities(
      dir.path(),
      "Acorn · General assistant",
      &specs,
      &dir.path().join("w"),
      "",
    );
    assert!(said.contains("- zoom: Open a line.\n"), "{said}");
    assert!(
      said.contains("shout(s: string): string -- loud   [plugin loud]"),
      "{said}"
    );
    assert!(said.contains("- read(path: string"), "{said}");
    assert!(said.contains("Acorn's source: "), "{said}");
  }
}
