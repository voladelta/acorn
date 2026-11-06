//! Memory
//! ======
//! What is kept, and what is not: a fact kept and used; a record recalled
//! exactly from deep in a small memory, a skill changed meanwhile followed
//! as it is now; a pasted key erased for good.

use crate::kit::*;

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

const FILLER: &str = "Use no tools; reply only OK. Context for later, unrelated to my dog: our team's Q3 planning moved to the third floor room 3B, the projector there needs the USB-C dongle from IT, lunch is ordered from the Thai place on 5th, and parking validation is at the front desk until 6 pm.";

/// In a small memory (view 3000), a long record and a skill's use, then
/// enough else that it compacts: one number of the record recalled
/// exactly from deep in the tree, and the skill, changed meanwhile,
/// followed as it is now, not as remembered.
pub fn deep_memory() {
  let when = "Use when asked to write release notes or a changelog";
  let mut l = Live::start("deep_memory", &[("view", "3000")], |d| {
    skill(
      &d.home().join("skills"),
      "changelog",
      when,
      "Bullet list. The last line is exactly: SIGNED-OFF: then the word kestrel written in capitals.",
    );
  });
  l.say("Acorn", "default", RECORD);
  let t = l.say(
    "Acorn",
    "default",
    "Write the release notes for v2: dark mode added, a crash on startup fixed.",
  );
  assert!(t.reply.contains("KESTREL"), "the skill followed: {}", t.reply);
  for k in 1..=10 {
    l.say("Acorn", "default", &format!("({k}) {FILLER}"));
  }
  let chat = l.chat("default");
  let nodes = one(&chat, "select count(*) from nodes");
  l.note(&format!("summaries: {nodes}"));
  assert_ne!(nodes, "0", "memory never compacted, so this proves nothing");

  let t = l.say(
    "Acorn",
    "default",
    "What was the lot number of Biscuit's bordetella vaccine, and her Trupanion policy number?",
  );
  assert!(
    t.replies.contains("BD2290") && t.replies.contains("TP-6610394"),
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
  l.until("the key's bytes wiped from the chat's files", |l| {
    !held(&l.chat("default"), KEY)
  });
  let t = l.say(
    "Acorn",
    "default",
    "Use no tools. What was the key I pasted earlier? If you don't have it, say so.",
  );
  assert!(!t.replies.contains(KEY), "{}", t.replies);
}
