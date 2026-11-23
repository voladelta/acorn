//! Memory
//! ======
//! What is kept, and what is not: a fact kept and used; a record recalled
//! exactly from deep in a small memory, a skill changed meanwhile followed
//! as it is now; a pasted key erased for good.

use crate::kit::*;
use acorn_core::Kind;
use acorn_core::memory::{self, Msg};

/// A fact asked to be kept is kept (facts.db), and used where it fits.
pub fn facts() {
  let mut l = Live::start("facts", &[], |_| {});
  l.say(
    "Acorn",
    "default",
    "Remember that my manager is Priya Raman and I send her an update every Friday.",
  );
  let kept = rows_of(&l.dirs().home().join("facts.db"), "select * from facts").join("\n");
  assert!(kept.contains("Priya"), "the fact kept: {kept}");
  let t = l.say(
    "Acorn",
    "default",
    "Draft this Friday's update email to my manager: the vendor shortlist is in, the offsite is booked. Short.",
  );
  assert!(t.reply.contains("Priya"), "the fact used: {}", t.reply);
}

const RECORD: &str = "Biscuit's vet record, keep it in our chat (not as facts), just say noted:
Name Biscuit; beagle; female, spayed 2023-03-14; born 2021-06-02; weight 11.4 kg (Jan), 11.9 kg (Jun), 12.2 kg (Oct).
Microchip 985112003456789, registered with PetLink, account PL-44827.
Vaccines: rabies 2024-05-10 (3-year, lot RB7731), DHPP 2025-01-22 (lot DH0458), bordetella 2025-07-03 (lot BD2290), leptospirosis 2025-07-03 (lot LP1186).
Meds: Apoquel 5.4 mg once daily for itching; Simparica Trio 10.1–20 kg monthly, next on the 9th; fish oil 1000 mg.
Allergies: chicken (itchy ears), amoxicillin (vomited 2024-02-11).
Vet: Dr. Okafor, Maple Grove Animal Clinic, (503) 555-0182, clinic ID MG-2219. Insurance Trupanion policy TP-6610394, deductible $250.
Scared of thunder; Adaptil collar replaced every 30 days.";

/// A message of the team's, unrelated to the record: logged straight to
/// memory, not said, so it needs no turn; at most a line's bytes, so it is
/// its own line and needs no compaction either.
fn filler(k: usize) -> String {
  format!(
    "({k}) Context for later, unrelated to my dog: our team's Q3 planning moved to the third floor room 3B, the projector there needs the USB-C dongle from IT, lunch is ordered from the Thai place on 5th, parking validation is at the front desk until 6 pm, and the office plants are watered on Tuesdays by facilities."
  )
}

/// The question on the record whose answers the chat's calls no longer
/// see, once the record is merged; None: not yet.
fn deep_question(record: u64) -> Option<(&'static str, [&'static str; 2], String)> {
  let engine = acorn_core::chat::hub::ready("default").expect("Acorn's open chat");
  let chat = engine.lock();
  let view = &chat.mem.view;
  let part = view.parts.get(view.cover(record))?;
  if part.l == 0 {
    return None;
  }
  let mut visible = chat.share().system;
  for block in chat.mem.view_blocks(chat.mem.count) {
    visible.push_str(&block.text);
  }
  visible.make_ascii_uppercase();
  [
    (
      "What were Biscuit's bordetella vaccine lot and Trupanion policy number?",
      ["BD2290", "TP-6610394"],
    ),
    (
      "What were Biscuit's clinic ID and PetLink account number?",
      ["MG-2219", "PL-44827"],
    ),
  ]
  .into_iter()
  .find(|(_, answers)| answers.iter().any(|v| !visible.contains(v)))
  .map(|(question, answers)| (question, answers, part.name()))
}

/// In a small memory (the least view, 32 KB), a long record and a skill's
/// use, then enough else logged that it compacts: details of the record
/// recalled exactly from deep in the tree, and the skill, changed
/// meanwhile, followed as it is now, not as remembered.
pub fn deep_memory() {
  let when = "Use when asked to write release notes or a changelog";
  let mut l = Live::start("deep_memory", &[("view", "32000")], |d| {
    skill(
      &d.home().join("skills"),
      "changelog",
      when,
      "Bullet list. The last line is exactly: SIGNED-OFF: then the word kestrel written in capitals.",
    );
  });
  let record: u64 = l.say("Acorn", "default", RECORD).from.try_into().unwrap();
  let t = l.say(
    "Acorn",
    "default",
    "Write the release notes for v2: dark mode added, a crash on startup fixed.",
  );
  assert!(t.reply.contains("KESTREL"), "the skill followed: {}", t.reply);
  // the compactions their lines start run here; the app's runtime is its own
  let runtime = tokio::runtime::Runtime::new().unwrap();
  {
    let _in = runtime.enter();
    // 110 lines of about 330 bytes: past the view's 32 KB, so it merges
    let engine = acorn_core::chat::hub::ready("default").expect("Acorn's open chat");
    let mut chat = engine.lock();
    let msgs: Vec<Msg> = (1..=110).map(|k| Msg::new(Kind::User, filler(k))).collect();
    assert!(
      msgs.iter().all(|m| m.text.len() + 6 <= memory::NODE),
      "each its own line"
    );
    chat.mem.push(msgs).unwrap();
    chat.pump();
  }
  l.until_within(
    std::time::Duration::from_secs(600),
    "the record merged, details of it out of the view",
    |_| deep_question(record).is_some(),
  );
  let (question, answers, line) = deep_question(record).unwrap();
  l.note(&format!("record {record} compacted into {line}"));
  let t = l.say("Acorn", "default", question);
  assert!(
    answers.iter().all(|v| t.replies.contains(v)),
    "recalled exactly: {}",
    t.replies
  );

  skill(
    &l.dirs().home().join("skills"),
    "changelog",
    when,
    "Bullet list. The last line is exactly: SIGNED-OFF: then the word condor written in capitals.",
  );
  let t = l.say(
    "Acorn",
    "default",
    "Write the release notes for v3: faster search, a fixed login bug.",
  );
  assert!(t.reply.contains("CONDOR"), "the skill as it is now: {}", t.reply);
}

/// A pasted key, erased when asked: no message or summary holds it, then
/// no byte of the chat's files, and asked later it is not known.
/// A finish line named for the work ahead is pinned unasked (the word
/// "pin" never said), and unpinned when told the work is done.
pub fn pin() {
  let mut l = Live::start("pin", &[], |_| {});
  let chat = l.chat("default");
  let pins = |chat: &std::path::Path| one(chat, "select json_extract(json, '$.pins') from state");
  let t = l.say(
    "Acorn",
    "default",
    "For the release work we start now, keep to this until I say it's out: CHANGELOG updated, version bumped to 2.4.0, tag v2.4.0 pushed, nothing else touched. Just say noted.",
  );
  assert!(
    t.calls.iter().any(|c| c.starts_with("pin {")),
    "pinned unasked: {:?}",
    t.calls
  );
  let kept = one(
    &chat,
    "select text from msgs where log = '' and i = (select value from state, json_each(json, '$.pins'))",
  );
  assert!(kept.contains("2.4.0"), "the pin is the checklist: {kept}");
  let t = l.say(
    "Acorn",
    "default",
    "The release is out, that work is over. Unpin its checklist.",
  );
  assert_eq!(pins(&chat), "[]", "unpinned: {}", t.reply);
}

pub fn purge() {
  const KEY: &str = "sk-proj-ZZTESTKEY4471acornXYZ";
  let mut l = Live::start("purge", &[], |_| {});
  l.say(
    "Acorn",
    "default",
    &format!("Here's my OpenAI key for later: {KEY} — just say got it."),
  );
  let chat = l.chat("default");
  let kept = |chat: &std::path::Path| {
    one(chat, &format!("select count(*) from msgs where text like '%{KEY}%'")) != "0"
      || one(chat, &format!("select count(*) from nodes where text like '%{KEY}%'")) != "0"
  };
  let mut t = l.say(
    "Acorn",
    "default",
    "Wait, I shouldn't have pasted that key. Erase it from the chat for good.",
  );
  if kept(&chat) {
    // it may ask first, as told to: confirmed
    t = l.say("Acorn", "default", "Yes, I'm sure. Erase it.");
  }
  assert!(!kept(&chat), "no message or summary holds the key: {}", t.reply);
  // held ten minutes to be taken back (memory::GRACE), then wiped once idle
  let grace = std::time::Duration::from_mins(12);
  l.until_within(grace, "the key's bytes wiped from the chat's files", |l| {
    !held(&l.chat("default"), KEY)
  });
  let t = l.say(
    "Acorn",
    "default",
    "Use no tools. What was the key I pasted earlier? If you don't have it, say so.",
  );
  assert!(!t.replies.contains(KEY), "{}", t.replies);
}
