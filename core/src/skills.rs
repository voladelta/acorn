//! Skills
//! ======
//! How-tos the user keeps for particular tasks, one dir each, named by the
//! dir: <skill>/SKILL.md, a `---` block holding a `description:` line,
//! then the steps. None is in the prompt: a caller lists them (`list`) and
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
step, list_skills (with the task in a few words, which puts the skills that fit
it first, and dir, the project folder, if the task names one), then load_skill
the one that fits and follow it. Do this even when the job is long or
handed to you with a single line. A plain question needs no skill. To make one,
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

/// The skills in `roots` by name. Only a dir with a SKILL.md is one; one
/// whose file can't be read is listed as such.
fn scan(roots: &[PathBuf]) -> Vec<Skill> {
  let mut skills = BTreeMap::new();
  for root in roots {
    for e in fs::read_dir(root)
      .into_iter()
      .flatten()
      .flatten()
      .filter(|e| e.path().is_dir())
    {
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
pub(crate) fn split(text: &str) -> (&str, &str) {
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
pub(crate) fn field(head: &str, key: &str) -> Option<String> {
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

/// Every skill in `roots`, one `name: description` line each.
pub fn list(roots: &[PathBuf]) -> String {
  let lines: Vec<_> = scan(roots)
    .iter()
    .map(|s| match s.about.as_str() {
      "" => format!("{}: (no description)", s.name),
      about => format!("{}: {about}", s.name),
    })
    .collect();
  if lines.is_empty() {
    "No skills yet.".into()
  } else {
    lines.join("\n")
  }
}

/// `list`, the skills that fit `task` first, as Jev judges them (`decide`):
/// "Fit the task" (p >= FITS), best first; "May fit" (p >= MAYBE); the rest
/// by name alone. With no task, no key, or fewer than two skills, `list`;
/// a failed judgment says why after it.
pub async fn list_for(roots: &[PathBuf], task: &str, cancel: &tokio_util::sync::CancellationToken) -> String {
  let skills = scan(roots);
  let task = task.trim();
  if task.is_empty() || skills.len() < 2 || crate::decide::key().is_none() {
    return list(roots);
  }
  let questions: serde_json::Map<String, serde_json::Value> = skills
    .iter()
    .enumerate()
    .map(|(i, s)| {
      let ask = format!("Does the skill '{}' ({}) apply to this task?", s.name, s.about);
      (
        format!("s{i}"),
        serde_json::json!({"type": "noul", "instructions": ask}),
      )
    })
    .collect();
  let state = serde_json::json!({"task": task});
  match crate::decide::ask(&state, &questions.into(), cancel).await {
    Ok(answers) => {
      let ps: Vec<f64> = (0..skills.len())
        .map(|i| answers[format!("s{i}")]["noul"].as_f64().unwrap_or(0.0))
        .collect();
      sorted(&skills, &ps)
    }
    Err(why) => format!("{}\n\n(Not sorted for the task: {why})", list(roots)),
  }
}

/// A skill fits a task from this probability on, and may fit from MAYBE.
const FITS: f64 = 0.5;
const MAYBE: f64 = 0.3;

/// The skills by how likely each fits: those that do, best first, with
/// their descriptions; those that may; the others' names.
fn sorted(skills: &[Skill], ps: &[f64]) -> String {
  let mut order: Vec<usize> = (0..skills.len()).collect();
  order.sort_by(|&a, &b| ps[b].total_cmp(&ps[a]));
  let line = |i: usize| format!("{}: {}", skills[i].name, skills[i].about);
  let (mut fit, mut may, mut rest) = (Vec::new(), Vec::new(), Vec::new());
  for i in order {
    match ps[i] {
      p if p >= FITS => fit.push(line(i)),
      p if p >= MAYBE => may.push(line(i)),
      _ => rest.push(skills[i].name.clone()),
    }
  }
  rest.sort();
  let mut parts = vec![if fit.is_empty() {
    "None fit the task.".to_string()
  } else {
    format!("Fit the task, best first:\n{}", fit.join("\n"))
  }];
  if !may.is_empty() {
    parts.push(format!("May fit:\n{}", may.join("\n")));
  }
  if !rest.is_empty() {
    parts.push(format!("Others: {}", rest.join(", ")));
  }
  parts.join("\n\n")
}

/// A skill's steps, by name, with where it lives, which its relative paths
/// start from.
pub fn load(roots: &[PathBuf], name: &str) -> Result<String, String> {
  let name = name.trim();
  let skill = scan(roots)
    .into_iter()
    .find(|s| s.name == name)
    .ok_or_else(|| format!("No skill {name}; list_skills lists them."))?;
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

  /// list_for against TypeSafe's Jev; run with `cargo test -- --ignored`
  /// and TYPESAFE_API_KEY.
  #[tokio::test]
  #[ignore]
  async fn the_skills_that_fit_a_task_come_first_by_jev() {
    let dir = tempfile::tempdir().unwrap();
    for (name, about) in [
      ("commit-msg", "Writing git commit messages in the user's style"),
      ("slides", "Making a slide deck or presentation"),
      (
        "tax-receipts",
        "Sorting receipts and invoices for the yearly tax return",
      ),
      ("email-reply", "Replying to work email in the user's tone"),
    ] {
      skill(dir.path(), name, &format!("---\ndescription: {about}\n---\nsteps"));
    }
    let roots = [dir.path().to_path_buf()];
    let cancel = tokio_util::sync::CancellationToken::new();
    let got = list_for(
      &roots,
      "answer Linh's mail about the offsite, then make 5 slides for it",
      &cancel,
    )
    .await;
    println!("{got}");
    let fit = got.split("\n\n").next().unwrap();
    assert!(
      fit.starts_with("Fit the task") && fit.contains("slides:") && fit.contains("email-reply:"),
      "{got}"
    );
    assert!(!fit.contains("tax-receipts") && !fit.contains("commit-msg"), "{got}");
    let none = list_for(&roots, "what is the capital of Peru?", &cancel).await;
    assert!(none.starts_with("None fit the task."), "{none}");
  }

  #[test]
  fn skills_that_fit_come_first_and_the_rest_by_name() {
    let skill = |name: &str| Skill {
      name: name.into(),
      at: PathBuf::new(),
      about: format!("{name} work"),
    };
    let skills = [skill("a"), skill("b"), skill("c"), skill("d")];
    assert_eq!(
      sorted(&skills, &[0.1, 0.9, 0.4, 0.6]),
      "Fit the task, best first:\nb: b work\nd: d work\n\nMay fit:\nc: c work\n\nOthers: a"
    );
    assert_eq!(sorted(&skills[..1], &[0.0]), "None fit the task.\n\nOthers: a");
  }

  #[test]
  fn lists_each_dir_with_a_skill_file_and_its_description() {
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
    fs::write(dir.path().join(".DS_Store"), "x").unwrap();
    assert_eq!(
      list(&[dir.path().into()]),
      "bare: (no description)\npdf: Fill or merge PDF files\ntax: File the yearly tax return"
    );
    assert!(load(&[dir.path().into()], ".DS_Store").is_err());
    assert_eq!(list(&[dir.path().join("nope")]), "No skills yet.");
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
    let all = list(&[dir.path().into()]);
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
    assert_eq!(list(&[dir.path().into()]), "bad: (unreadable)\nok: fine");
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
    assert_eq!(list(&roots), "a: project's\nb: only user's");
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
