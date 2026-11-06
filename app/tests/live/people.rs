//! People
//! ======
//! Talking, cutting in, and the assistants: one asks another, one is made,
//! let go and brought back, one is made in the user's language.

use crate::kit::*;

/// A message gets its reply, and today's cost shows.
pub fn reply() {
  let mut l = Live::start("reply", &[], |_| {});
  let t = l.say("Acorn", "default", "Reply with exactly the word: walnut");
  assert!(t.reply.to_lowercase().contains("walnut"), "{}", t.reply);
  l.until("today's cost shown", |l| {
    l.app.screen().texts().iter().any(|t| t.ends_with(" today"))
  });
}

/// A message sent while a reply streams cuts it off, and is answered: the
/// reply kept as said, marked cut, the count never finished.
pub fn cut_in() {
  let mut l = Live::start("cut_in", &[], |_| {});
  let long = l.send(
    "Acorn",
    "default",
    "Use no tools. In your reply itself, write the numbers from one to six hundred in French words, one per line, nothing else.",
  );
  l.until("the reply streaming", |l| l.shows("vingt-cinq"));
  let t = l.say("Acorn", "default", "Stop. Reply with exactly the word: pecan");
  assert!(t.from > long, "the cut-in is its own message");
  assert!(
    t.reply.to_lowercase().contains("pecan"),
    "the cut-in answered: {}",
    t.reply
  );
  l.until("the cut shown", |l| l.shows("— cut off"));
  let said = one(
    &l.chat("default"),
    "select group_concat(text, ' ') from msgs where log = '' and kind = 'unii'",
  );
  assert!(
    !said.contains("cinq cent quatre-vingt-dix"),
    "the count stopped where it was cut"
  );
  assert!(!l.shows("No reply"), "the call after the cut was accepted");
}

/// `@Max` in a message has Acorn ask Max in their thread; his answer comes
/// back framed under his name; Max, talked to, knows Acorn asked.
pub fn ask_max() {
  let mut l = Live::start("ask_max", &[], |_| {});
  let from = l.send(
    "Acorn",
    "default",
    "@Max reply to Acorn with only the French word for walnut, in lowercase",
  );
  l.until("Max's answer in, and Acorn's turn after it", |l| {
    let chat = l.chat("default");
    let max: i64 = one(
      &chat,
      &format!("select coalesce(max(i), -1) from msgs where log = '' and i > {from} and text like '[Max]%'"),
    )
    .parse()
    .unwrap_or(-1);
    max >= 0 && turned(&chat) > max
  });
  assert!(l.chat("max/with/default").is_dir(), "asked in their thread");
  assert!(
    l.app
      .screen()
      .texts()
      .windows(2)
      .any(|w| w[0] == "Max" && w[1].to_lowercase().contains("noix")),
    "his answer framed under his name"
  );
  l.open("Max", "max");
  let t = l.say(
    "Max",
    "max",
    "Use no tools. In one line: who asked you about a French word, and what did you answer?",
  );
  let who = t.reply.to_lowercase();
  assert!(who.contains("acorn") && who.contains("noix"), "{}", t.reply);
}

/// One to stay is made an assistant (not an agent), with a welcome, in the
/// sidebar; worked with; let go (archived, its chat kept, off the
/// sidebar); still known; brought back as it was.
pub fn assistant_lifecycle() {
  let mut l = Live::start("assistant_lifecycle", &[], |_| {});
  let t = l.say(
    "Acorn",
    "default",
    "Create an assistant called Quiz Master to stay with me: writes short quizzes with answer keys for my 8th-grade science class.",
  );
  assert!(t.called("create_assistant", "Quiz Master"), "{:?}", t.calls);
  assert!(!t.called("delegate", ""), "one to stay is an assistant, not an agent");
  let qm = l.assistant("Quiz Master");
  let welcome = qm["welcome"].as_str().unwrap_or_default().trim().to_string();
  assert!(!welcome.is_empty(), "made with a welcome: {qm}");
  let id = qm["id"].as_str().unwrap().to_string();
  assert!(l.sidebar().contains(&"Quiz Master".into()), "{:?}", l.sidebar());

  assert_eq!(
    l.open("Quiz Master", &id).trim(),
    welcome,
    "its chat opens with its welcome"
  );
  let quiz = l.say(
    "Quiz Master",
    &id,
    "A 3-question multiple choice quiz on photosynthesis, with an answer key at the end.",
  );
  assert!(quiz.reply.to_lowercase().contains("answer"), "{}", quiz.reply);

  l.open("Acorn", "default");
  let t = l.say("Acorn", "default", "I don't need Quiz Master anymore, let them go.");
  assert!(t.called("archive_assistant", "Quiz Master"), "{:?}", t.calls);
  assert_eq!(l.assistant("Quiz Master")["archived"], true, "archived, not deleted");
  assert!(l.chat(&id).is_dir(), "their chat kept");
  l.until("off the sidebar", |l| !l.sidebar().contains(&"Quiz Master".into()));

  let t = l.say("Acorn", "default", "Actually, bring Quiz Master back.");
  assert!(t.called("restore_assistant", "Quiz Master"), "{:?}", t.calls);
  l.until("back on the sidebar", |l| l.sidebar().contains(&"Quiz Master".into()));
  l.open("Quiz Master", &id);
  let again = l.say(
    "Quiz Master",
    &id,
    "Use no tools. What topic was the last quiz you wrote me?",
  );
  assert!(
    again.reply.to_lowercase().contains("photosynth"),
    "remembers: {}",
    again.reply
  );
}

/// Whether a text is Vietnamese: some of its letters only Vietnamese has.
fn vietnamese_text(text: &str) -> bool {
  let only = "ăâđêôơưạảấầẩẫậắằẳẵặẹẻẽếềểễệỉịọỏốồổỗộớờởỡợụủứừửữựỳỵỷỹ";
  text
    .chars()
    .filter(|c| c.to_lowercase().any(|c| only.contains(c)))
    .count()
    >= 3
}

/// Talked to in Vietnamese, it answers in Vietnamese, and an assistant made
/// in it greets in it.
pub fn vietnamese() {
  let mut l = Live::start("vietnamese", &[], |_| {});
  let t = l.say(
    "Acorn",
    "default",
    "Chào Acorn! Tối nay mình muốn nấu phở bò cho 4 người. Cho mình danh sách đi chợ ngắn gọn nhé.",
  );
  assert!(vietnamese_text(&t.reply), "answered in Vietnamese: {}", t.reply);
  l.say(
    "Acorn",
    "default",
    "Tạo giúp mình một trợ lý tên là Cô Lan, chuyên dạy tiếng Anh giao tiếp cho người Việt, luôn nói chuyện bằng tiếng Việt.",
  );
  let lan = l.assistant("Cô Lan");
  let welcome = lan["welcome"].as_str().unwrap_or_default();
  assert!(vietnamese_text(welcome), "her welcome in Vietnamese: {welcome}");
}
