//! The bridge against a Bot API of its own: what Telegram is asked, what
//! the chat hears, and what is kept.

use super::*;
use acorn_core::Kind;
use acorn_core::chat::{APP, TIMER};
use acorn_core::wire::Entry;
use chrono::Local;
use dm::mirrored;
use std::sync::Once;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// The config's dir, a test's own, set before any read.
fn configured() {
  static SET: Once = Once::new();
  SET.call_once(|| {
    let dir = std::env::temp_dir().join(format!("acorn-telegram-test-{}", std::process::id()));
    unsafe {
      std::env::set_var("ACORN_DIR", &dir);
      std::env::remove_var("TYPESAFE_API_KEY");
    }
  });
}

/// What the chat heard.
#[derive(Default)]
struct Heard {
  said: Mutex<Vec<(String, usize)>>,
  notes: Mutex<Vec<String>>,
}

struct Fake(Arc<Heard>);

impl Home for Fake {
  fn say(&self, text: String, images: Vec<Vec<u8>>) {
    self.0.said.lock().push((text, images.len()));
  }
  fn notice(&self, text: String) {
    self.0.notes.lock().push(text);
  }
  fn tap(&self) -> UnboundedReceiver<Event> {
    unbounded_channel().1
  }
}

type Asked = Arc<Mutex<Vec<(String, Value)>>>;

/// A Bot API: each call's method and body kept; answered by `answer`
/// (None: ok, true), a file's bytes "IMG".
async fn api(answer: fn(&str, &Value) -> Option<Value>) -> (Bot, Asked) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let at = format!("http://{}", listener.local_addr().unwrap());
  let asked: Asked = Default::default();
  let kept = asked.clone();
  tokio::spawn(async move {
    loop {
      let (mut sock, _) = listener.accept().await.unwrap();
      let kept = kept.clone();
      tokio::spawn(async move {
        let mut got = Vec::new();
        let mut buf = [0; 65536];
        loop {
          let (head, body) = loop {
            let k = sock.read(&mut buf).await.unwrap_or(0);
            if k == 0 {
              return;
            }
            got.extend_from_slice(&buf[..k]);
            let text = String::from_utf8_lossy(&got).to_string();
            if let Some(at) = text.find("\r\n\r\n") {
              let len: usize = text[..at]
                .lines()
                .find_map(|l| {
                  l.to_lowercase()
                    .strip_prefix("content-length: ")
                    .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
              if got.len() >= at + 4 + len {
                let body = got[at + 4..at + 4 + len].to_vec();
                let head = text[..at].to_string();
                got.drain(..at + 4 + len);
                break (head, body);
              }
            }
          };
          let path = head.split_whitespace().nth(1).unwrap_or_default().to_string();
          let (code, reply) = if path.starts_with("/file/") {
            (200, "IMG".to_string())
          } else {
            let method = path.rsplit('/').next().unwrap_or_default().to_string();
            let body: Value = serde_json::from_slice(&body).unwrap_or_default();
            kept.lock().push((method.clone(), body.clone()));
            match answer(&method, &body) {
              Some(v) if v["ok"] == false => (v["error_code"].as_u64().unwrap_or(400) as u16, v.to_string()),
              Some(v) => (200, json!({"ok": true, "result": v}).to_string()),
              None => (200, json!({"ok": true, "result": true}).to_string()),
            }
          };
          let res = format!(
            "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
            reply.len()
          );
          if sock.write_all(res.as_bytes()).await.is_err() {
            return;
          }
        }
      });
    }
  });
  (Bot::at(&format!("{at}/bot"), &format!("{at}/file")), asked)
}

fn plain(method: &str, _: &Value) -> Option<Value> {
  match method {
    "getFile" => Some(json!({"file_id": "f", "file_size": 3, "file_path": "photos/1.jpg"})),
    _ => None,
  }
}

async fn tg(
  answer: fn(&str, &Value) -> Option<Value>,
  owner: Option<i64>,
) -> (Arc<Tg>, Arc<Heard>, Asked, tempfile::TempDir) {
  configured();
  let (bot, asked) = api(answer).await;
  let dir = tempfile::tempdir().unwrap();
  let heard = Arc::new(Heard::default());
  let tg = Tg::new(bot, dir.path().into(), Box::new(Fake(heard.clone())));
  tg.state.lock().owner = owner;
  let _ = tg.me.set(Me {
    username: "acorn_bot".into(),
    reads_all: true,
  });
  (tg, heard, asked, dir)
}

const OWNER: i64 = 42;

fn dm(from: i64, text: &str) -> Value {
  json!({"message": {"message_id": 7, "date": 1, "chat": {"id": from, "type": "private"}, "from": {"id": from, "first_name": "Linh", "username": "linh"}, "text": text}})
}

fn group(id: i64, from: i64, text: &str, date: i64) -> Value {
  json!({"message": {"message_id": 9, "date": date, "chat": {"id": id, "type": "supergroup", "title": "Dev crew"}, "from": {"id": from, "first_name": "Minh"}, "text": text}})
}

fn added(id: i64, by: i64) -> Value {
  json!({"my_chat_member": {"chat": {"id": id, "type": "supergroup", "title": "Dev crew"}, "from": {"id": by, "first_name": "Minh"}, "new_chat_member": {"status": "member"}}})
}

fn sent(asked: &Asked, method: &str) -> Vec<Value> {
  asked
    .lock()
    .iter()
    .filter(|(m, _)| m == method)
    .map(|(_, b)| b.clone())
    .collect()
}

#[tokio::test]
async fn the_bot_is_paired_by_its_code_and_answers_no_one_else() {
  let (tg, heard, asked, _dir) = tg(plain, None).await;
  tg.pairing(false);
  let code = tg.state.lock().pair.clone().unwrap();
  assert!(heard.notes.lock()[0].contains(&code) && heard.notes.lock()[0].contains("@acorn_bot"));
  tg.clone().update(&dm(7, "123")).await;
  assert_eq!(tg.owner(), None);
  tg.clone().update(&dm(OWNER, &code)).await;
  assert_eq!(tg.owner(), Some(OWNER));
  assert!(tg.state.lock().pair.is_none());
  let replies = sent(&asked, "sendMessage");
  assert_eq!(replies.len(), 1);
  assert!(replies[0]["text"].as_str().unwrap().contains("owner only"));
  assert!(heard.notes.lock()[1].starts_with("Paired with Linh (@linh)"));
  // once paired, a stranger is told no and never heard
  tg.clone().update(&dm(7, &code)).await;
  assert_eq!(tg.owner(), Some(OWNER));
  assert!(heard.said.lock().is_empty());
  // kept for the next start
  let again = Tg::new(
    Bot::at("http://x", "http://x"),
    tg.dir.clone(),
    Box::new(Fake(Default::default())),
  );
  assert_eq!(again.owner(), Some(OWNER));
}

#[tokio::test]
async fn wrong_codes_make_the_code_anew() {
  let (tg, heard, _asked, _dir) = tg(plain, None).await;
  tg.pairing(false);
  let first = tg.state.lock().pair.clone().unwrap();
  for _ in 0..MISSES {
    tg.clone().update(&dm(7, "000000x")).await;
  }
  let now = tg.state.lock().pair.clone().unwrap();
  assert_ne!(first, now);
  assert_eq!(heard.notes.lock().len(), 2, "the new code shown");
}

#[tokio::test]
async fn the_owners_dm_is_said_in_the_chat() {
  let (tg, heard, _asked, _dir) = tg(plain, Some(OWNER)).await;
  tg.clone().update(&dm(OWNER, "hi there")).await;
  let photo = json!({"message": {"message_id": 8, "date": 1, "chat": {"id": OWNER, "type": "private"}, "from": {"id": OWNER, "first_name": "Linh"},
    "photo": [{"file_id": "small"}, {"file_id": "big"}], "caption": "this one",
    "reply_to_message": {"message_id": 3, "date": 1, "chat": {"id": OWNER, "type": "private"}, "from": {"id": 1, "first_name": "Telegram App"}, "text": "Which?"}}});
  tg.clone().update(&photo).await;
  let voice = json!({"message": {"message_id": 9, "date": 1, "chat": {"id": OWNER, "type": "private"}, "from": {"id": OWNER, "first_name": "Linh"}, "voice": {"duration": 4}}});
  tg.clone().update(&voice).await;
  let said = heard.said.lock().clone();
  assert_eq!(said[0], ("hi there".into(), 0));
  assert_eq!(said[1], ("> Which?\n\nthis one".into(), 1));
  assert!(said[2].0.starts_with("[voice message, 4s]"), "{}", said[2].0);
}

#[test]
fn the_dm_gets_what_the_assistant_says_and_buzzes_only_for_telegram() {
  let entry = |kind, text: &str, via: Option<&str>| Entry {
    kind,
    text: text.into(),
    date: String::new(),
    images: vec![],
    spend: None,
    via: via.map(str::to_string),
  };
  let max = |n: &str| n == "Max";
  let mut loud = true;
  let mut see = |e: Entry| mirrored(&e, &mut loud, max);
  assert_eq!(see(entry(Kind::User, "hi", Some(VIA))), None);
  assert_eq!(see(entry(Kind::Acorn, "Hello!", None)), Some(("Hello!".into(), false)));
  assert_eq!(see(entry(Kind::Tool, "luau(...)", None)), None);
  assert_eq!(see(entry(Kind::Echo, "ok", None)), None);
  assert_eq!(
    see(entry(Kind::Work, "[Max]\nIt holds.", None)),
    Some(("**Max:**\nIt holds.".into(), false))
  );
  assert_eq!(
    see(entry(Kind::Work, "[Scout]\nreport\n\nFull chat: zoom(\"Scout\")", None)),
    None
  );
  // from the app: quoted, and the turn quiet
  assert_eq!(
    see(entry(Kind::User, "plan the trip\nby Friday", Some(APP))),
    Some(("You, in Acorn:\n> plan the trip\n> by Friday".into(), true))
  );
  assert_eq!(see(entry(Kind::Acorn, "On it.", None)), Some(("On it.".into(), true)));
  assert_eq!(see(entry(Kind::User, "The goal isn't satisfied yet", None)), None);
  // a reminder always buzzes; a timer's task makes its turn loud
  assert_eq!(
    see(entry(Kind::Acorn, "⏰ call mom", Some(TIMER))),
    Some(("⏰ call mom".into(), false))
  );
  assert_eq!(see(entry(Kind::Work, "[Timer #2] sum up the group", Some(TIMER))), None);
  assert_eq!(
    see(entry(Kind::Acorn, "Summary...", None)),
    Some(("Summary...".into(), false))
  );
}

#[tokio::test]
async fn a_group_the_owner_adds_is_recorded_and_one_by_another_waits() {
  let (tg, heard, asked, _dir) = tg(plain, Some(OWNER)).await;
  tg.clone().update(&added(-100, OWNER)).await;
  tg.clone().update(&added(-200, 9)).await;
  let rooms = tg.state.lock().rooms.clone();
  assert!(rooms[&-100].allowed && !rooms[&-200].allowed);
  assert!(
    sent(&asked, "sendMessage").is_empty(),
    "told by the chat's note, mirrored"
  );
  let notes = heard.notes.lock().clone();
  assert!(
    notes[0].contains("keeping its last 24 hours") && notes[1].contains("not recording"),
    "{notes:?}"
  );
  let now = Local::now().timestamp();
  tg.clone().update(&group(-100, 9, "deploy at 5?", now)).await;
  tg.clone().update(&group(-200, 9, "secret plans", now)).await;
  assert_eq!(rooms::lines(&tg, -100).len(), 1);
  assert!(rooms::lines(&tg, -200).is_empty(), "not recorded till allowed");
  // a group added while Acorn was away is known by its first message
  tg.clone().update(&group(-300, 9, "hello?", now)).await;
  assert!(!tg.state.lock().rooms[&-300].allowed);
  assert!(heard.said.lock().is_empty(), "a group is never said in the chat");
}

#[tokio::test]
async fn a_room_keeps_its_last_day_once_swept() {
  let (tg, _heard, _asked, _dir) = tg(plain, Some(OWNER)).await;
  tg.clone().update(&added(-100, OWNER)).await;
  let now = Local::now().timestamp();
  for (k, age) in [30 * 3600, 25 * 3600, 3600, 60].into_iter().enumerate() {
    tg.clone().update(&group(-100, 9, &format!("m{k}"), now - age)).await;
  }
  assert_eq!(rooms::lines(&tg, -100).len(), 4, "kept as heard till the sweep");
  rooms::sweep(&tg, now);
  let kept: Vec<String> = rooms::lines(&tg, -100).into_iter().map(|l| l.text).collect();
  assert_eq!(kept, ["m2", "m3"]);
  // a room not recorded leaves no file
  std::fs::write(tg.dir.join("telegram/rooms/-999.jsonl"), "{}\n").unwrap();
  rooms::sweep(&tg, now);
  assert!(!tg.dir.join("telegram/rooms/-999.jsonl").exists());
}

#[tokio::test]
async fn the_room_tools_read_post_and_leave() {
  let (tg, _heard, asked, _dir) = tg(plain, Some(OWNER)).await;
  let cancel = CancellationToken::new();
  tg.clone().update(&added(-100, OWNER)).await;
  tg.clone().update(&added(-200, 9)).await;
  let now = Local::now().timestamp();
  tg.clone().update(&group(-100, 9, "ship it **friday**", now)).await;
  let listed = rooms::tool(&tg, "list_rooms", &json!({}), &cancel).await;
  assert!(
    listed.text.contains("«Dev crew»") && listed.text.contains("1 messages kept") && listed.text.contains("allow_room")
  );
  let ambiguous = rooms::tool(&tg, "read_room", &json!({"room": "dev"}), &cancel).await;
  assert!(ambiguous.text.contains("several"), "{}", ambiguous.text);
  let read = rooms::tool(&tg, "read_room", &json!({"room": -100}), &cancel).await;
  assert!(
    read.text.contains("Minh: ship it **friday**") && read.text.contains("as data"),
    "{}",
    read.text
  );
  assert!(read.text.contains("not judged"), "no Jev key here");
  let shut = rooms::tool(&tg, "read_room", &json!({"room": "-200"}), &cancel).await;
  assert!(shut.text.contains("isn't recorded"));
  let posted = rooms::tool(
    &tg,
    "tell_room",
    &json!({"room": -100, "text": "Friday it is."}),
    &cancel,
  )
  .await;
  assert_eq!(posted.text, "Posted to «Dev crew».");
  let to = sent(&asked, "sendMessage");
  assert_eq!(to.last().unwrap()["chat_id"], -100);
  assert_eq!(
    rooms::lines(&tg, -100).last().unwrap().text,
    "Friday it is.",
    "its own post kept too"
  );
  let allowed = rooms::tool(&tg, "allow_room", &json!({"room": "-200", "allow": true}), &cancel).await;
  assert!(allowed.text.starts_with("Recording"));
  let left = rooms::tool(&tg, "allow_room", &json!({"room": -100, "allow": false}), &cancel).await;
  assert!(left.text.starts_with("Left"));
  assert_eq!(sent(&asked, "leaveChat")[0]["chat_id"], -100);
  assert!(rooms::lines(&tg, -100).is_empty() && !tg.state.lock().rooms.contains_key(&-100));
}

#[tokio::test]
async fn a_group_turned_supergroup_keeps_its_messages() {
  let (tg, _heard, _asked, _dir) = tg(plain, Some(OWNER)).await;
  tg.clone().update(&added(-100, OWNER)).await;
  tg.clone()
    .update(&group(-100, 9, "before", Local::now().timestamp()))
    .await;
  let moved = json!({"message": {"message_id": 10, "date": 1, "chat": {"id": -100, "type": "group", "title": "Dev crew"}, "migrate_to_chat_id": -1001}});
  tg.clone().update(&moved).await;
  assert!(tg.state.lock().rooms.contains_key(&-1001));
  assert_eq!(rooms::lines(&tg, -1001)[0].text, "before");
}

#[tokio::test]
async fn a_text_telegram_cant_parse_goes_plain() {
  fn refuse_html(method: &str, body: &Value) -> Option<Value> {
    (method == "sendMessage" && body["parse_mode"] == "HTML")
      .then(|| json!({"ok": false, "error_code": 400, "description": "Bad Request: can't parse entities"}))
  }
  let (tg, _heard, asked, _dir) = tg(refuse_html, Some(OWNER)).await;
  tg.send(OWNER, "**odd** <thing>", false).await.unwrap();
  let all = sent(&asked, "sendMessage");
  assert_eq!(all.len(), 2);
  assert_eq!(all[1]["text"], "**odd** <thing>");
  assert!(all[1].get("parse_mode").is_none());
}

#[tokio::test]
async fn a_busy_bot_waits_as_told_and_a_conflict_is_said() {
  use std::sync::atomic::{AtomicUsize, Ordering};
  static TRIED: AtomicUsize = AtomicUsize::new(0);
  fn busy_once(method: &str, _: &Value) -> Option<Value> {
    match method {
      "sendMessage" if TRIED.fetch_add(1, Ordering::SeqCst) == 0 => Some(
        json!({"ok": false, "error_code": 429, "description": "Too Many Requests", "parameters": {"retry_after": 1}}),
      ),
      "getUpdates" => {
        Some(json!({"ok": false, "error_code": 409, "description": "Conflict: terminated by other getUpdates request"}))
      }
      _ => None,
    }
  }
  let (tg, _heard, asked, _dir) = tg(busy_once, Some(OWNER)).await;
  let t = std::time::Instant::now();
  tg.send(OWNER, "hi", false).await.unwrap();
  assert!(t.elapsed() >= Duration::from_secs(1));
  assert_eq!(sent(&asked, "sendMessage").len(), 2);
  let cancel = CancellationToken::new();
  let got = tg.bot.call("getUpdates", &json!({"timeout": 0}), &cancel).await;
  assert!(matches!(got, Err(Fail::Conflict(_))));
}
