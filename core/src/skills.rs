//! Skills
//! ======
//! How-tos the user keeps for particular tasks, one dir each, named by the
//! dir: <skill>/SKILL.md, a `---` block holding a `description:` line,
//! then the steps. None is in the prompt: a caller searches (`search`) and
//! loads the one that fits (`load`). They are read from disk on every
//! call, so an edit or a new skill counts at once; Acorn makes one by
//! writing the file.

use crate::config;
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const FILE: &str = "SKILL.md";

/// Where the skills are, in order: a later dir's skill replaces an earlier
/// one's of the same name, so a project's beat the user's. The projects
/// are the caller's work dir and `project`, a folder it was told to work
/// in (relative to the work dir, as the file tools' paths are), each with
/// .agents/skills and .acorn/skills.
pub fn roots(work: &Path, project: &str) -> Vec<PathBuf> {
  let mut roots = vec![config::home().join(".agents/skills"), config::get().dir.join("skills")];
  let project = match project.trim() {
    "" => None,
    "~" => Some(config::home()),
    p => Some(work.join(config::expand(p))),
  };
  for at in [Some(work.to_path_buf()), project].into_iter().flatten() {
    roots.extend([at.join(".agents/skills"), at.join(".acorn/skills")]);
  }
  roots
}

/// What the prompts say of skills.
pub fn note() -> String {
  let (agents, acorn) = (config::home().join(".agents/skills"), config::get().dir.join("skills"));
  format!(
    "# Skills

The user keeps skills, how-tos for particular tasks: a dir each with a {FILE}, in
{agents} and {acorn}, and in .agents/skills and .acorn/skills of
your work dir and of a project folder you are told to work in (pass it to the
skill tools as dir). A later one's skill replaces an earlier one's of the same
name. A skill holds the user's own rules for a kind of task (format, tone,
steps) that you cannot guess, however simple the task looks. So at the start of
any task that makes something (notes, code, a file, a plan), before its first
step, search_skills (with dir, the project folder, if the task names one), then
load_skill the one that fits and follow it. Do this even when the job is long or
handed to you with a single line. A plain question needs no search. To make one,
write {acorn}/<name>/{FILE}, or in a project's .acorn/skills to keep it to that
project, starting with a --- block of a description: line saying when it applies.",
    agents = agents.display(),
    acorn = acorn.display(),
  )
}

struct Skill {
  /// Its dir's name, which is the skill's.
  name: String,
  /// Its dir.
  at: PathBuf,
  /// What its head says it is for.
  about: String,
}

/// The skills in `roots` by name. A dir without a SKILL.md is none; one
/// that can't be read is listed as such.
fn scan(roots: &[PathBuf]) -> Vec<Skill> {
  let mut skills = BTreeMap::new();
  for root in roots {
    for e in fs::read_dir(root).into_iter().flatten().flatten() {
      let about = match fs::read_to_string(e.path().join(FILE)) {
        Ok(text) => field(split(&text).0, "description").unwrap_or_default(),
        Err(err) if err.kind() == ErrorKind::NotFound => continue,
        Err(_) => "(unreadable)".into(),
      };
      let name = e.file_name().to_string_lossy().into_owned();
      skills.insert(
        name.clone(),
        Skill {
          name,
          at: e.path(),
          about,
        },
      );
    }
  }
  skills.into_values().collect()
}

/// A file's `---` block and what follows; no block, no head. A byte-order
/// mark and CRLF lines are as the plain ones.
fn split(text: &str) -> (&str, &str) {
  let text = text.trim_start_matches('\u{feff}');
  let mut lines = text.split_inclusive('\n');
  let Some(first) = lines.next().filter(|l| l.trim_end() == "---") else {
    return ("", text);
  };
  let (start, mut at) = (first.len(), first.len());
  for line in lines {
    if line.trim_end() == "---" {
      return (&text[start..at], &text[at + line.len()..]);
    }
    at += line.len();
  }
  ("", text)
}

/// The value of `key:` in a head. One that opens a block (`>`, `|`) or is
/// empty takes the indented lines after it.
fn field(head: &str, key: &str) -> Option<String> {
  let mut lines = head.lines();
  let value = lines
    .find_map(|l| l.strip_prefix(key)?.trim_start().strip_prefix(':'))?
    .trim();
  let value = if value.is_empty() || value.starts_with(['>', '|']) {
    lines
      .take_while(|l| l.starts_with([' ', '\t']))
      .map(str::trim)
      .collect::<Vec<_>>()
      .join(" ")
  } else {
    unquote(value).to_string()
  };
  Some(value).filter(|v| !v.is_empty())
}

/// A value without the one pair of quotes around it.
fn unquote(value: &str) -> &str {
  ['"', '\'']
    .iter()
    .find_map(|q| value.strip_prefix(*q)?.strip_suffix(*q))
    .unwrap_or(value)
}

/// The skills in `roots` that match a query, best first (each word in the
/// name or description counts one); with no query, all.
pub fn search(roots: &[PathBuf], query: &str) -> String {
  let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
  let mut found: Vec<(usize, Skill)> = scan(roots)
    .into_iter()
    .map(|s| {
      let hay = format!("{} {}", s.name, s.about).to_lowercase();
      (words.iter().filter(|w| hay.contains(w.as_str())).count(), s)
    })
    .filter(|(hits, _)| words.is_empty() || *hits > 0)
    .collect();
  found.sort_by_key(|(hits, _)| std::cmp::Reverse(*hits));
  match (found.is_empty(), words.is_empty()) {
    (false, _) => found
      .iter()
      .map(|(_, s)| match s.about.as_str() {
        "" => format!("{}: (no description)", s.name),
        about => format!("{}: {about}", s.name),
      })
      .collect::<Vec<_>>()
      .join("\n"),
    (true, true) => "No skills yet.".into(),
    (true, false) => "None matches; search with no query lists them all.".into(),
  }
}

/// A skill's steps, by name, with where it lives, which its relative paths
/// start from.
pub fn load(roots: &[PathBuf], name: &str) -> Result<String, String> {
  let name = name.trim();
  let skill = scan(roots)
    .into_iter()
    .find(|s| s.name == name)
    .ok_or_else(|| format!("No skill {name}; search_skills lists them."))?;
  let text = fs::read_to_string(skill.at.join(FILE)).map_err(|e| format!("Skill {name}: {e}"))?;
  Ok(format!(
    "Skill {name}, in {} (its paths start there):\n\n{}",
    skill.at.display(),
    split(&text).1.trim()
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::Path;

  fn skill(dir: &Path, folder: &str, text: &str) {
    fs::create_dir_all(dir.join(folder)).unwrap();
    fs::write(dir.join(folder).join(FILE), text).unwrap();
  }

  #[test]
  fn searches_by_words_and_lists_all_with_none() {
    let dir = tempfile::tempdir().unwrap();
    skill(
      dir.path(),
      "pdf",
      "---\nname: pdf\ndescription: \"Fill or merge PDF files\"\n---\nSteps.\n",
    );
    skill(
      dir.path(),
      "tax",
      "---\nname: tax\ndescription: >\n  File the yearly\n  tax return\n---\nSteps.\n",
    );
    skill(dir.path(), "bare", "No head at all.");
    fs::create_dir_all(dir.path().join("empty")).unwrap();
    let all = search(&[dir.path().into()], "");
    assert_eq!(
      all,
      "bare: (no description)\npdf: Fill or merge PDF files\ntax: File the yearly tax return"
    );
    assert_eq!(
      search(&[dir.path().into()], "merge pdf"),
      "pdf: Fill or merge PDF files"
    );
    assert_eq!(
      search(&[dir.path().into()], "return"),
      "tax: File the yearly tax return"
    );
    assert!(search(&[dir.path().into()], "zebra").starts_with("None matches"));
    assert!(search(&[dir.path().join("nope")], "").starts_with("No skills yet"));
  }

  #[test]
  fn loads_the_body_by_dir_and_nothing_outside() {
    let dir = tempfile::tempdir().unwrap();
    skill(
      dir.path(),
      "tx",
      "---\nname: tax\ndescription: d\n---\nStep one.\nStep two.\n",
    );
    let said = load(&[dir.path().into()], "tx").unwrap();
    assert!(said.starts_with("Skill tx, in "), "{said}");
    assert!(said.ends_with("Step one.\nStep two."), "{said}");
    assert!(said.contains(&dir.path().join("tx").display().to_string()));
    fs::write(dir.path().join("secret"), "x").unwrap();
    for name in ["../secret", "..", "secret", "missing", "tax", ""] {
      assert!(load(&[dir.path().into()], name).is_err(), "{name}");
    }
  }

  #[test]
  fn heads_read_with_crlf_a_bom_an_empty_block_and_quotes() {
    let dir = tempfile::tempdir().unwrap();
    skill(
      dir.path(),
      "crlf",
      "\u{feff}---\r\ndescription: windows\r\n---\r\nSteps.\r\n",
    );
    skill(dir.path(), "empty", "---\n---\nTop.\n---\nAfter the rule.\n");
    skill(dir.path(), "quoted", "---\ndescription: \"Say 'hi'\"\n---\nB.\n");
    let all = search(&[dir.path().into()], "");
    assert_eq!(all, "crlf: windows\nempty: (no description)\nquoted: Say 'hi'");
    assert!(load(&[dir.path().into()], "crlf").unwrap().ends_with("Steps."));
    assert!(
      load(&[dir.path().into()], "empty")
        .unwrap()
        .ends_with("Top.\n---\nAfter the rule.")
    );
  }

  #[test]
  fn a_file_that_cannot_be_read_is_listed_as_such() {
    let dir = tempfile::tempdir().unwrap();
    skill(dir.path(), "ok", "---\ndescription: fine\n---\nB.");
    fs::create_dir_all(dir.path().join("bad")).unwrap();
    fs::write(dir.path().join("bad").join(FILE), [0xff, 0xfe, 0xfd]).unwrap();
    assert_eq!(search(&[dir.path().into()], ""), "bad: (unreadable)\nok: fine");
    assert!(load(&[dir.path().into()], "bad").is_err());
  }

  #[test]
  fn a_later_dir_replaces_an_earlier_ones_skill() {
    let (user, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    skill(user.path(), "a", "---\nname: a\ndescription: user's\n---\nUser steps.");
    skill(user.path(), "b", "---\nname: b\ndescription: only user's\n---\nB.");
    skill(
      project.path(),
      "a",
      "---\nname: a\ndescription: project's\n---\nProject steps.",
    );
    let roots = [user.path().into(), project.path().into()];
    assert_eq!(search(&roots, ""), "a: project's\nb: only user's");
    assert!(load(&roots, "a").unwrap().ends_with("Project steps."));
  }

  #[test]
  fn roots_are_the_users_then_the_work_dir_then_the_project() {
    let roots = roots(Path::new("/w/main"), " /p/video ");
    let tail: Vec<_> = roots[2..].iter().map(|r| r.to_str().unwrap()).collect();
    assert_eq!(
      tail,
      [
        "/w/main/.agents/skills",
        "/w/main/.acorn/skills",
        "/p/video/.agents/skills",
        "/p/video/.acorn/skills"
      ]
    );
    assert_eq!(super::roots(Path::new("/w/main"), "").len(), 4);
  }

  #[test]
  fn a_relative_project_starts_in_the_work_dir_and_a_tilde_is_home() {
    let work = Path::new("/w/main");
    assert_eq!(
      roots(work, "video/cut")[4],
      Path::new("/w/main/video/cut/.agents/skills")
    );
    assert_eq!(roots(work, "~")[4], config::home().join(".agents/skills"));
    assert_eq!(roots(work, "~/p")[5], config::home().join("p/.acorn/skills"));
  }
}
