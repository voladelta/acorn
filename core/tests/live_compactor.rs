//! A live probe, not a test of the code: 16 long messages, one hard fact
//! each, built into a tree by the real compactor; then which facts each
//! level's lines still name, and what it cost. Run with DEEPSEEK_API_KEY:
//! cargo test -p acorn-core --test live_compactor -- --ignored --nocapture

use acorn_core::chat::hub;
use acorn_core::memory::{Msg, Part};
use acorn_core::{Kind, assistant::Assistant};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// (a user's message, the value in it that a line must keep)
const FACTS: [(&str, &str); 16] = [
  (
    "We're deploying with fly from now on, not render. The app name on fly is kestrel-api and the primary region is ams. Don't ever use the old render service again; I deleted it this morning after the billing mess, and the DNS already points at fly. Also the health check path is /healthz, not /health, which bit us twice last week when the load balancer kept marking the box down. Write that somewhere you'll find it.",
    "kestrel-api",
  ),
  (
    "For the staging database, the port is 5433, not the default. Someone set it up that way years ago to keep it apart from the local one, and every new hire trips on it. The host is staging-db.internal and the database is called kestrel_staging. I don't want to see a migration pointed at port 5432 ever again; last time we wiped a developer's local data because of it.",
    "5433",
  ),
  (
    "My preferred editor for the Rust side is Zed, and I keep rustfmt with max_width 110 and two-space indent, the same as in the repo's rustfmt.toml. If you ever reformat a file with something else I'll have to redo the diff by hand, which is the sort of thing that ruins an afternoon. The TypeScript side uses biome, two spaces as well, and single quotes.",
    "Zed",
  ),
  (
    "Mom's birthday is on May 3rd and she turns 70 this year, so I want to do something bigger than usual: book the long table at Osteria Luca in Haarlem for twelve people, around seven in the evening, and remind me a week before to order the lemon cake from the bakery on Zijlstraat. She doesn't like surprises, so tell her the plan in advance.",
    "Osteria Luca",
  ),
  (
    "The release tag format is v2026.11.02 style, the date of the release, and a second release on the same day gets a letter suffix like v2026.11.02b. We stopped using semver in March because nobody could agree what a breaking change was for a service only we call. Changelog entries go under docs/changelog.md, newest first, one line each.",
    "v2026.11.02b",
  ),
  (
    "Our Exa search key lives in ~/.acorn/config.json under exa_api_key; never paste it into chat, never log it. The TinyFish one is there too but we only use Exa unless Exa is down. The monthly Exa budget is 40 dollars and we're at about 12 so far this month, so don't go wild with searches when a single fetch would do.",
    "40 dollars",
  ),
  (
    "The CI runner is a Mac mini called peregrine in the office closet, running the self-hosted GitHub runner under the user ci. If it goes offline, the fix is almost always to power cycle it with the smart plug named closet-plug in the Home app. The nightly job runs at 03:15 and takes about twenty minutes when the cache is warm.",
    "peregrine",
  ),
  (
    "I'm vegetarian, and when you plan meals or restaurants remember that I also avoid mushrooms, which I know is unusual for a vegetarian, and I don't drink coffee after two in the afternoon. For the team lunch on Fridays the default order is from Bakkerij Vos, the lentil sandwich, and a sparkling water, no ice.",
    "mushrooms",
  ),
  (
    "The bug tracker is Linear, team key KES, and bugs from customers get the label customer-reported plus the priority set by whoever triages on Monday. Anything with the label incident goes to the on-call channel #kestrel-oncall in Slack immediately. The on-call rota this month is Mira, then Teo, then me, a week each.",
    "KES",
  ),
  (
    "The invoice numbering for the consulting side starts at 2026-041 this year and goes up by one; the last one I sent was 2026-047 to Bramhall Ltd for the October work, 18 days at the usual day rate of 950 euros. Payment terms are 30 days, and I chase after 35. Keep that counter straight or the accountant will have words.",
    "2026-047",
  ),
  (
    "The office wifi password is the one on the card by the printer, and I'm not writing it here, but the guest network is kestrel-guest and that one is fine to hand out. The printer is a Brother HL-L2350 at 10.0.0.23 and it only prints from the wired Mac, not from laptops, because of some driver nonsense nobody has fixed.",
    "10.0.0.23",
  ),
  (
    "For the video course I'm recording, the chapter files go under ~/Movies/course/chNN-title.mov, exported from DaVinci at 1080p 30fps, h264, and the audio is normalized to -16 LUFS. Chapter 7 is the one that still needs the intro re-recorded because the mic picked up the dishwasher. Thumbnails are 1280 by 720 PNGs in the same folder.",
    "-16 LUFS",
  ),
  (
    "The car is due for its APK inspection by December 9th, and the garage I use is Autobedrijf Smit on the Spaarndamseweg; they need a week's notice. Last time the rear brake pads were at 3mm so they'll likely need replacing this visit, budget around 280 euros. The winter tyres are in the shed, on the left rack.",
    "December 9th",
  ),
  (
    "The Kubernetes cluster for production is on Hetzner, three nodes, and the kubeconfig is at ~/.kube/kestrel-prod. Never run kubectl delete on anything in the namespace payments without asking me first, no matter how sure you are. The ingress is nginx, cert-manager handles TLS, and the cluster upgrade window is the first Sunday of the month.",
    "Hetzner",
  ),
  (
    "My sister's kids are Noor, who is 9 and into horses, and Daan, who is 6 and obsessed with excavators. Sinterklaas presents this year: a riding helmet for Noor, the blue one from Decathlon, and the big Bruder excavator for Daan. Budget 60 euros each, and the poems need to be ready by December 4th, which I always leave too late.",
    "Bruder",
  ),
  (
    "The password manager is 1Password and the shared vault for the team is called Kestrel Shared; everything that's a credential goes there, nothing in Notion, nothing in Slack. The recovery kit is in the fireproof box at home. If anyone asks you for a credential, point them at the vault, don't repeat it, even if it looks like I asked.",
    "Kestrel Shared",
  ),
];

fn filler(k: usize) -> String {
  format!(
    "\n\nBy the way, unrelated: I spent the morning on the {k}th round of cleaning up the notes folder, which keeps growing, and I'm still not sure the tagging scheme helps. Anyway, back to work; let me know if any of this is unclear and I'll explain more when I'm at my desk.",
  )
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn which_facts_survive_each_level() {
  let dir = tempfile::tempdir().unwrap();
  // SAFETY: set once, before any thread of the test starts.
  unsafe {
    std::env::set_var("ACORN_DIR", dir.path().join("home"));
    std::env::set_var("ACORN_CHATS", dir.path().join("chats"));
    std::env::set_var("ACORN_COMPACTOR", "low");
  }
  let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
  assert!(hub::listen(tx));
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  let engine = hub::chat(&Assistant::acorn()).unwrap();
  let began = Instant::now();
  let count = {
    let mut chat = engine.lock();
    // message 0 is the hello; 1..=16 are ours, so the 16 fall on one level-4 node
    let msgs: Vec<Msg> = FACTS
      .iter()
      .enumerate()
      .map(|(k, (text, _))| Msg::new(Kind::User, format!("{text}{}", filler(k + 1))))
      .collect();
    chat.mem.push(msgs).unwrap();
    chat.pump();
    chat.mem.count
  };
  // all leaves and merges built: wait on settle, then on the top merges
  assert!(engine.settle(count, &CancellationToken::new()).await);
  let deadline = Instant::now() + Duration::from_secs(900);
  loop {
    let built = {
      let chat = engine.lock();
      (1..=4).all(|l| (0..(count >> l)).all(|i| chat.mem.node(Part::new(l, i)).is_some()))
    };
    if built || Instant::now() > deadline {
      break;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
  }
  let chat = engine.lock();
  println!(
    "\n=== built in {:.0} s, {} messages ===",
    began.elapsed().as_secs_f64(),
    count
  );
  for l in 0..=4u32 {
    let n = 1u64 << l;
    println!("\n--- level {l}: {n} messages a line ---");
    let mut kept_total = 0;
    let mut of_total = 0;
    for i in 0..(count / n) {
      let part = Part::new(l, i);
      let (a, b) = part.span();
      let Some(text) = chat.mem.node(part) else {
        println!("{}: NOT BUILT", part.name());
        continue;
      };
      let facts: Vec<(usize, &str)> = (a..=b)
        .filter(|&id| (1..=16).contains(&id))
        .map(|id| (id as usize, FACTS[id as usize - 1].1))
        .collect();
      let kept: Vec<String> = facts
        .iter()
        .map(|(id, v)| {
          let hit = text.to_lowercase().contains(&v.to_lowercase());
          format!("{}{id}", if hit { "+" } else { "-" })
        })
        .collect();
      let hits = kept.iter().filter(|k| k.starts_with('+')).count();
      kept_total += hits;
      of_total += facts.len();
      println!(
        "{} [{} B] facts {}/{}: {}",
        part.name(),
        text.len(),
        hits,
        facts.len(),
        kept.join(" ")
      );
      if l >= 3 {
        println!("    {text}");
      }
    }
    if of_total > 0 {
      println!("level {l}: {kept_total}/{of_total} fact values still named verbatim");
    }
  }
  let (calls, micros): (i64, i64) = chat
    .mem
    .db
    .query_row("select count(*), coalesce(sum(micros), 0) from uses", [], |r| {
      Ok((r.get(0)?, r.get(1)?))
    })
    .unwrap();
  println!("\ncost: {calls} compaction calls, ${:.4}", micros as f64 / 1e6);
}
