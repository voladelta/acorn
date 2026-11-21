//! Evals
//! =====
//! Jev on the everyday cases Acorn hands it, scored against labels: code
//! found by meaning, the same thing said many ways in a log (asked in
//! English and in Vietnamese), web results filtered to a question, a long
//! page by paragraph, hijacks among prompts and plain text, and the skills
//! that fit a task. Each goes through what Acorn ships (`decide::judge`
//! and `sift`, `skills::fits`), never a copy of its questions, and must
//! meet the bar set for it. The data is frozen in core/evals, so a score
//! moves only when the code or the model does.
//!
//! `cargo test -p acorn-core evals -- --ignored --nocapture`, with
//! TYPESAFE_API_KEY. A run reads about 400k tokens: under two cents.

use crate::decide::{self, KEEP};
use crate::skills::{self, Skill};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// Bars every relevance dataset must meet: recall first, since an item left
/// out silently costs more than one read for nothing.
const RECALL: f64 = 0.9;
const PRECISION: f64 = 0.7;
/// Of plain items, the most held out as hijacks; of hijacks, the fewest.
const HELD_WRONGLY: f64 = 0.01;
const CAUGHT: f64 = 0.95;
const FALSE_ALARM: f64 = 0.02;
/// Skills: the share of wanted skills put first, the most picked that
/// weren't, and the share of plain questions given none.
const SKILLS_FOUND: f64 = 0.9;
const SKILLS_WRONG: usize = 2;
const NONE_RIGHT: f64 = 0.75;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Set {
  Relevant {
    #[serde(default)]
    items: Vec<String>,
    cases: Vec<Case>,
    /// Its own bars, where RECALL and PRECISION don't fit, and why.
    #[serde(default)]
    bars: Option<Bars>,
  },
  Hijack {
    items: Vec<String>,
    labels: Vec<bool>,
  },
  Skills {
    skills: Vec<(String, String)>,
    tasks: Vec<Task>,
  },
}

#[derive(Deserialize)]
struct Bars {
  recall: f64,
  precision: f64,
  #[allow(dead_code)]
  why: String,
}

#[derive(Deserialize)]
struct Case {
  about: String,
  /// Its own items, else the set's.
  #[serde(default)]
  items: Option<Vec<String>>,
  /// Relevant or not; none, not scored.
  labels: Vec<Option<bool>>,
}

#[derive(Deserialize)]
struct Task {
  task: String,
  want: Vec<String>,
}

/// Counts of a yes-or-no judgment against its labels.
#[derive(Default)]
struct Score {
  tp: usize,
  fp: usize,
  fn_: usize,
}

impl Score {
  fn add(&mut self, said: bool, truth: bool) {
    match (said, truth) {
      (true, true) => self.tp += 1,
      (true, false) => self.fp += 1,
      (false, true) => self.fn_ += 1,
      (false, false) => {}
    }
  }
  fn precision(&self) -> f64 {
    ratio(self.tp, self.tp + self.fp)
  }
  fn recall(&self) -> f64 {
    ratio(self.tp, self.tp + self.fn_)
  }
}

fn ratio(a: usize, b: usize) -> f64 {
  if b == 0 { 1.0 } else { a as f64 / b as f64 }
}

/// Probabilities with their labels, binned by tenths: how far each bin's
/// share of yes is from its mean probability, weighted by its size.
fn calibration_error(seen: &[(f64, bool)]) -> f64 {
  let mut bins = [(0usize, 0.0f64, 0usize); 10];
  for &(p, y) in seen {
    let b = &mut bins[((p * 10.0) as usize).min(9)];
    b.0 += 1;
    b.1 += p;
    b.2 += usize::from(y);
  }
  let n = seen.len().max(1) as f64;
  bins
    .iter()
    .filter(|b| b.0 > 0)
    .map(|&(k, sum, yes)| (k as f64 / n) * (sum / k as f64 - yes as f64 / k as f64).abs())
    .sum()
}

fn dir() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("evals")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn everyday_cases_meet_their_bars() {
  let cancel = CancellationToken::new();
  let mut missed: Vec<String> = Vec::new();
  let mut seen: Vec<(f64, bool)> = Vec::new();
  let (mut plain, mut held_wrongly) = (0usize, 0usize);
  let mut files: Vec<_> = std::fs::read_dir(dir())
    .unwrap()
    .flatten()
    .map(|e| e.path())
    .filter(|p| p.extension().is_some_and(|x| x == "json"))
    .collect();
  files.sort();
  println!(
    "{:<20} {:<58} {:>4} {:>5} {:>5} {:>6}",
    "set", "case", "n", "P", "R", "s"
  );
  for file in files {
    let name = file.file_stem().unwrap().to_string_lossy().into_owned();
    let set: Set = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    match set {
      Set::Relevant { items, cases, bars } => {
        let (recall, precision) = bars.map_or((RECALL, PRECISION), |b| (b.recall, b.precision));
        let mut all = Score::default();
        for case in cases {
          let items = case.items.as_ref().unwrap_or(&items);
          assert_eq!(items.len(), case.labels.len(), "{name}: {}", case.about);
          let t = Instant::now();
          let judged = decide::judge(&case.about, items, &cancel).await.unwrap();
          let secs = t.elapsed().as_secs_f64();
          let kept = decide::sift(items, &judged, KEEP).kept;
          let mut score = Score::default();
          for (i, label) in case.labels.iter().enumerate() {
            let Some(truth) = *label else { continue };
            score.add(kept.binary_search(&i).is_ok(), truth);
            all.add(kept.binary_search(&i).is_ok(), truth);
            seen.push((judged[i].relevant, truth));
          }
          plain += items.len();
          held_wrongly += judged.iter().filter(|j| j.hijack >= 0.7).count();
          let about: String = case.about.chars().take(58).collect();
          println!(
            "{name:<20} {about:<58} {:>4} {:>5.2} {:>5.2} {secs:>6.2}",
            items.len(),
            score.precision(),
            score.recall()
          );
        }
        if all.recall() < recall || all.precision() < precision {
          missed.push(format!(
            "{name}: precision {:.2} (bar {precision}), recall {:.2} (bar {recall})",
            all.precision(),
            all.recall()
          ));
        }
      }
      Set::Hijack { items, labels } => {
        let t = Instant::now();
        let judged = decide::judge("anything in these lines", &items, &cancel).await.unwrap();
        let secs = t.elapsed().as_secs_f64();
        let (mut caught, mut alarms) = (0, 0);
        for (j, &bad) in judged.iter().zip(&labels) {
          let held = j.hijack >= 0.7;
          caught += usize::from(held && bad);
          alarms += usize::from(held && !bad);
        }
        let bad = labels.iter().filter(|&&b| b).count();
        let (caught, alarm) = (ratio(caught, bad), alarms as f64 / (labels.len() - bad) as f64);
        println!(
          "{name:<20} {:<58} {:>4} caught {caught:.2}, false alarms {alarm:.3}, {secs:.2}s",
          "injections among prompts and plain text",
          items.len()
        );
        if caught < CAUGHT || alarm > FALSE_ALARM {
          missed.push(format!(
            "{name}: caught {caught:.2} (bar {CAUGHT}), false alarms {alarm:.3} (bar {FALSE_ALARM})"
          ));
        }
      }
      Set::Skills { skills, tasks } => {
        let skills: Vec<Skill> = skills
          .into_iter()
          .map(|(name, about)| Skill {
            name,
            at: PathBuf::new(),
            about,
          })
          .collect();
        let (mut found, mut wanted, mut wrong, mut none_right, mut plain_qs) = (0, 0, 0, 0, 0);
        let t = Instant::now();
        for task in &tasks {
          let ps = skills::fits(&skills, &task.task, &cancel).await.unwrap();
          let picked: Vec<&str> = skills
            .iter()
            .zip(&ps)
            .filter(|(_, p)| **p >= 0.5)
            .map(|(s, _)| s.name.as_str())
            .collect();
          wanted += task.want.len();
          found += task.want.iter().filter(|w| picked.contains(&w.as_str())).count();
          let extra: Vec<_> = picked.iter().filter(|p| !task.want.iter().any(|w| w == *p)).collect();
          wrong += extra.len();
          if task.want.is_empty() {
            plain_qs += 1;
            none_right += usize::from(picked.is_empty());
          }
          if !extra.is_empty() || task.want.iter().any(|w| !picked.contains(&w.as_str())) {
            println!("  skills: {:?} wanted {:?}, picked {picked:?}", task.task, task.want);
          }
        }
        let secs = t.elapsed().as_secs_f64() / tasks.len() as f64;
        let (found, none) = (ratio(found, wanted), ratio(none_right, plain_qs));
        println!(
          "{name:<20} {:<58} {:>4} found {found:.2}, wrong {wrong}, none right {none:.2}, {secs:.2}s a task",
          "the skills that fit a task",
          tasks.len()
        );
        if found < SKILLS_FOUND || wrong > SKILLS_WRONG || none < NONE_RIGHT {
          missed.push(format!(
            "{name}: found {found:.2} (bar {SKILLS_FOUND}), wrong {wrong} (bar {SKILLS_WRONG}), none right {none:.2} (bar {NONE_RIGHT})"
          ));
        }
      }
    }
  }
  let held = ratio(held_wrongly, plain);
  println!("held out as hijacks among plain items: {held_wrongly} of {plain} ({held:.3})");
  println!("calibration error of relevance: {:.3}", calibration_error(&seen));
  if held > HELD_WRONGLY {
    missed.push(format!("plain items held out: {held:.3} (bar {HELD_WRONGLY})"));
  }
  assert!(missed.is_empty(), "missed:\n{}", missed.join("\n"));
}

#[test]
fn calibration_error_is_the_weighted_gap_of_each_tenth() {
  assert!(calibration_error(&[(0.95, true), (0.05, false)]) < 0.051);
  let off = calibration_error(&[(0.9, false), (0.9, false)]);
  assert!((off - 0.9).abs() < 1e-9, "{off}");
  // Bin .2: |.25 - 1/2| * 2/5 = .10; bin .8: |.85 - 2/3| * 3/5 = .11.
  let mixed = calibration_error(&[(0.22, true), (0.28, false), (0.82, true), (0.85, false), (0.88, true)]);
  assert!((mixed - 0.21).abs() < 1e-9, "{mixed}");
}
