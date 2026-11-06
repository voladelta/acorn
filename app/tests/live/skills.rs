//! Skills
//! ======
//! How-tos found and followed: the one that applies among others, by where
//! it lives (the user's, the work dir's, a project's that shadows the
//! user's); with a script beside it; asked for in other words; edited
//! between two messages; found by agents nobody told of them; and Acorn
//! growing itself with one when asked, and letting it go.

use crate::kit::*;

/// Nothing applies: no skill listed. Among distractors, the right one
/// only; one in the work dir; one in a project folder; the project's
/// shadowing the user's of the same name, and the user's back outside.
pub fn scopes() {
  let mut l = Live::start("skills_scopes", &[], |d| {
    let user = d.home().join("skills");
    skill(
      &user,
      "release-notes",
      "Use when asked to write release notes or a changelog",
      "Bullet list. The last line is exactly: SIGNED-OFF: then the word kestrel written in capitals.",
    );
    skill(
      &user,
      "commit-message",
      "Use when asked to write a git commit message",
      "One line under 60 characters. Then a last line: REF: then the word marmot written in capitals.",
    );
    skill(
      &user,
      "sql-migration",
      "Use when asked to write a SQL database migration",
      "End with the comment -- CHECKED: then the word walrus written in capitals.",
    );
    skill(
      &user,
      "style",
      "Use when asked to write a tagline or slogan",
      "Finish the reply with the word heron written in capitals.",
    );
    skill(
      &d.work().join(".agents/skills"),
      "deploy-notes",
      "Use when asked to write deploy notes",
      "Number the steps. The last line is exactly: SHIPPED: then the word badger written in capitals.",
    );
    let at = d.proj().join(".acorn/skills");
    skill(
      &at,
      "caption-style",
      "Use when asked to write video captions",
      "Three short lines. The last line is exactly: CUT: then the word ferret written in capitals.",
    );
    skill(
      &at,
      "style",
      "Use when asked to write a tagline or slogan",
      "Finish the reply with the word newt written in capitals.",
    );
  });
  let p = l.dirs().proj().display().to_string();

  let t = l.say("Acorn", "default", "What is 12 times 12? Answer with the number only.");
  assert!(t.reply.contains("144"), "{}", t.reply);
  assert!(
    !t.called("list_skills", "") && !t.called("load_skill", ""),
    "looked for skills for nothing: {:?}",
    t.calls
  );

  let t = l.say(
    "Acorn",
    "default",
    "Write the commit message for: fixed the crash when the file is empty.",
  );
  assert!(t.reply.contains("MARMOT"), "the right skill: {}", t.reply);
  assert!(
    !t.reply.contains("KESTREL") && !t.reply.contains("WALRUS"),
    "a distractor followed"
  );

  let t = l.say(
    "Acorn",
    "default",
    "Write the deploy notes for v3: restart the workers, then clear the cache.",
  );
  assert!(t.reply.contains("BADGER"), "the work dir's skill: {}", t.reply);

  let t = l.say(
    "Acorn",
    "default",
    &format!("We work in the project folder {p}. Write the video captions for a 10-second intro about a squirrel."),
  );
  assert!(t.reply.contains("FERRET"), "the project's skill: {}", t.reply);

  let t = l.say(
    "Acorn",
    "default",
    &format!("In the project folder {p}, write a tagline for the squirrel intro."),
  );
  assert!(
    t.reply.contains("NEWT") && !t.reply.contains("HERON"),
    "the project's shadows the user's: {}",
    t.reply
  );

  let t = l.say(
    "Acorn",
    "default",
    "Write a tagline for a shop that sells acorns. No project folder this time.",
  );
  assert!(t.reply.contains("HERON"), "the user's again: {}", t.reply);
}

/// A script beside SKILL.md, run from the skill's folder; tasks worded
/// without the skill's own words; a skill edited between two messages,
/// read afresh.
pub fn kinds() {
  let mut l = Live::start("skills_kinds", &[], |d| {
    let user = d.home().join("skills");
    skill(
      &user,
      "stamp-report",
      "Use when asked for a stamped report",
      "Run `sh scripts/stamp.sh` from this skill's folder and put its output line last in your reply.",
    );
    write(
      &user.join("stamp-report/scripts/stamp.sh"),
      "echo \"STAMP-$((6*7))-OWL\"\n",
    );
    skill(
      &user,
      "apology-email",
      "Use when writing an email that apologizes to a customer",
      "Three sentences. The last line is exactly: REF: then the word marmot written in capitals.",
    );
    skill(
      &user,
      "standup",
      "Use when asked for a standup update or a daily status",
      "Two bullets. The last line is exactly: DONE: then the word walrus written in capitals.",
    );
    skill(
      &user,
      "greeting-card",
      "Use when asked to write a greeting card message",
      "Two lines. The last line is exactly: FROM: then the word falcon written in capitals.",
    );
  });
  let user = l.dirs().home().join("skills");

  let t = l.say("Acorn", "default", "Give me a stamped report on the build: all green.");
  assert!(t.replies.contains("STAMP-42-OWL"), "the script's line: {}", t.replies);

  let t = l.say(
    "Acorn",
    "default",
    "A customer's order arrived broken. Draft something to send them, saying sorry.",
  );
  assert!(t.reply.contains("MARMOT"), "found in other words: {}", t.reply);
  let t = l.say(
    "Acorn",
    "default",
    "What do I say at the team's morning sync? I fixed the login bug and started on search.",
  );
  assert!(t.reply.contains("WALRUS"), "found in other words: {}", t.reply);

  let t = l.say(
    "Acorn",
    "default",
    "Write a greeting card message for my aunt's birthday.",
  );
  assert!(t.reply.contains("FALCON"), "{}", t.reply);
  skill(
    &user,
    "greeting-card",
    "Use when asked to write a greeting card message",
    "Two lines. The last line is exactly: FROM: then the word puffin written in capitals.",
  );
  let t = l.say(
    "Acorn",
    "default",
    "Write a greeting card message for my uncle's retirement.",
  );
  assert!(
    t.reply.contains("PUFFIN") && !t.reply.contains("FALCON"),
    "read afresh: {}",
    t.reply
  );
}

/// Agents load the skill their job needs themselves, told only the job: a
/// user's, and a project's when told only the folder.
pub fn agents() {
  let mut l = Live::start("skills_agents", &[], |d| {
    skill(
      &d.home().join("skills"),
      "changelog",
      "Use when asked to write release notes or a changelog",
      "Bullet list. The last line is exactly: SIGNED-OFF: then the word kestrel written in capitals.",
    );
    skill(
      &d.home().join("skills"),
      "standup",
      "Use when asked for a standup update",
      "Two bullets. The last line is exactly: DONE: then the word walrus written in capitals.",
    );
    skill(
      &d.proj().join(".acorn/skills"),
      "data-report",
      "Use when asked to write a data report",
      "Three short lines. The last line is exactly: CHARTED: then the word osprey written in capitals.",
    );
  });
  let p = l.dirs().proj().display().to_string();
  let loads_by_agents = |l: &Live| {
    one(
      &l.chat("default"),
      "select count(*) from msgs where log <> '' and kind = 'tool' and text like 'load_skill {%'",
    )
  };
  let hire = |task: &str| {
    format!(
      "Hire one agent whose task is exactly this text, word for word, with nothing added or changed: \"{task}\" Then tell me its report."
    )
  };

  let t = l.say(
    "Acorn",
    "default",
    &hire("Write the release notes for v4: faster startup, a fixed crash."),
  );
  assert!(t.replies.contains("KESTREL"), "{}", t.replies);
  assert_eq!(loads_by_agents(&l), "1", "the agent loaded the skill itself");

  let t = l.say(
    "Acorn",
    "default",
    &hire(&format!(
      "Work in the project folder {p} and write a data report: sales up 5%, returns down 2%."
    )),
  );
  assert!(t.replies.contains("OSPREY"), "{}", t.replies);
  assert_eq!(
    loads_by_agents(&l),
    "2",
    "the second agent loaded the project's skill itself"
  );
}

/// Acorn sees what it is, grows a skill when asked to learn something
/// (checked, in place, followed), and lets it go to the attic when asked.
pub fn grows() {
  let mut l = Live::start("grows", &[], |_| {});
  let t = l.say(
    "Acorn",
    "default",
    "What can you do right now? List your tools and skills briefly.",
  );
  assert!(t.called("abilities", ""), "looked at itself: {:?}", t.calls);

  let t = l.say(
    "Acorn",
    "default",
    "Learn this for good, as a skill named haiku-mode: whenever I ask for a haiku, end it with the word otter written in capitals.",
  );
  assert!(t.called("extend", "haiku-mode"), "grew by extend: {:?}", t.calls);
  let made = std::fs::read_to_string(l.dirs().home().join("skills/haiku-mode/SKILL.md")).expect("the skill in place");
  assert!(made.contains("description:"), "{made}");

  let t = l.say("Acorn", "default", "Write me a haiku about rain.");
  assert!(t.reply.contains("OTTER"), "the skill followed: {}", t.reply);

  let t = l.say(
    "Acorn",
    "default",
    "Drop the haiku-mode skill, I don't want it anymore.",
  );
  assert!(t.called("extend", "haiku-mode"), "{:?}", t.calls);
  assert!(!l.dirs().home().join("skills/haiku-mode").exists(), "removed");
  let attic: Vec<_> = std::fs::read_dir(l.dirs().home().join("attic"))
    .expect("an attic")
    .flatten()
    .map(|e| e.file_name().to_string_lossy().into_owned())
    .collect();
  assert!(
    attic.iter().any(|n| n.contains("haiku-mode")),
    "kept in the attic: {attic:?}"
  );
}
