//! acorn-host: see the library's docs, and `acorn-host help`.

use acorn_host::proxy::{self, Host};
use acorn_host::runner::{self, Plan};
use acorn_host::sandbox;
use acorn_host::store::{self, Store};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const USAGE: &str = "usage:
  acorn-host serve [--addr 127.0.0.1:8080] [--public URL] [--data DIR] [--server PATH] [--web DIR] [--no-sandbox]
  acorn-host invite <name> [--public URL] [--data DIR]   a new person, and their join link
  acorn-host link <user-id> [--public URL] [--data DIR]  another join link (a second device)
  acorn-host signout <user-id> [--data DIR]              sign them out of every device; their unused links die too
  acorn-host users [--data DIR]                          id, name, created, whether their server is up";

struct Args {
  addr: String,
  public: Option<String>,
  data: PathBuf,
  server: Option<PathBuf>,
  web: Option<PathBuf>,
  sandbox: bool,
  rest: Vec<String>,
}

fn fail(msg: impl std::fmt::Display) -> ! {
  eprintln!("acorn-host: {msg}");
  std::process::exit(1);
}

fn parse(mut it: impl Iterator<Item = String>) -> Args {
  let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
  let mut a = Args {
    addr: "127.0.0.1:8080".into(),
    public: None,
    data: std::env::var_os("ACORN_HOST_DIR").map_or_else(|| home.join(".acorn-host"), PathBuf::from),
    server: None,
    web: None,
    sandbox: true,
    rest: Vec::new(),
  };
  let value = |it: &mut dyn Iterator<Item = String>, flag: &str| {
    it.next()
      .unwrap_or_else(|| fail(format!("{flag} needs a value\n{USAGE}")))
  };
  while let Some(arg) = it.next() {
    match arg.as_str() {
      "--addr" => a.addr = value(&mut it, &arg),
      "--public" => a.public = Some(value(&mut it, &arg).trim_end_matches('/').to_string()),
      "--data" => a.data = value(&mut it, &arg).into(),
      "--server" => a.server = Some(value(&mut it, &arg).into()),
      "--web" => a.web = Some(value(&mut it, &arg).into()),
      "--no-sandbox" => a.sandbox = false,
      "-h" | "--help" | "help" => {
        println!("{USAGE}");
        std::process::exit(0);
      }
      s if s.starts_with("--") => fail(format!("unknown flag {s}\n{USAGE}")),
      _ => a.rest.push(arg),
    }
  }
  a.data = std::path::absolute(&a.data).unwrap_or_else(|e| fail(e));
  a
}

fn main() {
  let mut it = std::env::args().skip(1);
  let Some(cmd) = it.next() else { fail(USAGE) };
  let a = parse(it);
  let st = Store::new(&a.data);
  let public = a.public.clone().unwrap_or_else(|| format!("http://{}", a.addr));
  match (cmd.as_str(), a.rest.as_slice()) {
    ("serve", []) => serve(a),
    ("invite", [name]) => {
      let (_, code) = st.invite(name).unwrap_or_else(|e| fail(e));
      println!("{public}/join/{code}");
    }
    ("link", [id]) => {
      let code = st.link(id).unwrap_or_else(|e| fail(e));
      println!("{public}/join/{code}");
    }
    ("signout", [id]) => match st.signout(id).unwrap_or_else(|e| fail(e)) {
      Some(n) => println!("{id}: signed out of {n} device{}", if n == 1 { "" } else { "s" }),
      None => fail(format!("no person {id} (see `acorn-host users`)")),
    },
    ("users", []) => {
      let book = st.read().unwrap_or_else(|e| fail(e));
      for p in book.people {
        let sock = runner::sock(&runner::run_dir(&st.dir, &p.id));
        let up = runner::hold(&sock).is_some_and(|held| std::os::unix::net::UnixStream::connect(held.path()).is_ok());
        println!(
          "{}\t{}\t{}\t{}",
          p.id,
          p.name,
          day(p.created),
          if up { "up" } else { "down" }
        );
      }
    }
    ("-h" | "--help" | "help", _) => println!("{USAGE}"),
    _ => fail(USAGE),
  }
}

/// A Unix time as YYYY-MM-DD (UTC).
fn day(secs: u64) -> String {
  // days since 1970 to a civil date (Howard Hinnant's algorithm)
  let z = (secs / 86400) as i64 + 719_468;
  let era = z.div_euclid(146_097);
  let doe = z - era * 146_097;
  let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let d = doy - (153 * mp + 2) / 5 + 1;
  let m = if mp < 10 { mp + 3 } else { mp - 9 };
  let y = yoe + era * 400 + i64::from(m <= 2);
  format!("{y:04}-{m:02}-{d:02}")
}

/// `<data>/config.json`: the DeepSeek key (DEEPSEEK_API_KEY wins) and each
/// person's budget, USD a day.
fn config(data: &Path) -> (String, f64) {
  let file = data.join("config.json");
  let conf: HashMap<String, serde_json::Value> = match std::fs::read_to_string(&file) {
    Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| fail(format!("{}: {e}", file.display()))),
    Err(_) => HashMap::new(),
  };
  let key = std::env::var("DEEPSEEK_API_KEY")
    .ok()
    .filter(|k| !k.trim().is_empty())
    .or_else(|| {
      conf
        .get("deepseek_api_key")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    })
    .unwrap_or_default();
  let budget = match conf.get("budget") {
    None => 1.0,
    Some(v) => v
      .as_f64()
      .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
      .filter(|b| *b >= 0.0)
      .unwrap_or_else(|| fail(format!("{}: budget is USD a day, as 1 or 0.5", file.display()))),
  };
  (key.trim().to_string(), budget)
}

/// Next to this binary, else `name` on PATH.
fn beside(name: &str) -> PathBuf {
  std::env::current_exe()
    .ok()
    .and_then(|exe| exe.parent().map(|d| d.join(name)))
    .filter(|p| p.exists())
    .unwrap_or_else(|| {
      std::env::var_os("PATH")
        .and_then(|path| std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.exists()))
        .unwrap_or_else(|| fail(format!("no {name} beside acorn-host or on PATH (--server)")))
    })
}

fn serve(a: Args) {
  let server =
    std::path::absolute(a.server.clone().unwrap_or_else(|| beside("acorn-server"))).unwrap_or_else(|e| fail(e));
  let web = web(&a, &server);
  walled(&a);
  store::make_dir(&a.data).unwrap_or_else(|e| fail(e));
  let (key, budget) = config(&a.data);
  if key.is_empty() {
    eprintln!(
      "acorn-host: no DeepSeek key (DEEPSEEK_API_KEY, or deepseek_api_key in {}/config.json)",
      a.data.display()
    );
  }
  let st = Store::new(&a.data);
  let book = st.read().unwrap_or_else(|e| fail(e));
  let host = Arc::new(Host {
    store: st,
    plan: Plan {
      data: a.data.clone(),
      server,
      web,
      sandbox: a.sandbox,
      key,
      budget,
    },
    secure: a.public.as_deref().is_some_and(|p| p.starts_with("https://")),
    runners: Mutex::new(HashMap::new()),
  });
  tokio::runtime::Runtime::new().expect("runtime").block_on(async move {
    for p in &book.people {
      host.runner(&p.id);
    }
    let listener = tokio::net::TcpListener::bind(&a.addr)
      .await
      .unwrap_or_else(|e| fail(format!("can't listen on {}: {e}", a.addr)));
    let addr = listener.local_addr().map_or(a.addr.clone(), |a| a.to_string());
    println!("acorn-host: on http://{addr}, {} people", book.people.len());
    run(&host, listener).await;
  });
}

/// The built web window: `--web`, else `web` beside the server, else
/// web/dist here.
fn web(a: &Args, server: &Path) -> PathBuf {
  let web = std::path::absolute(a.web.clone().unwrap_or_else(|| {
    let near = server
      .parent()
      .map(|d| d.join("web"))
      .filter(|d| d.join("index.html").exists());
    near.unwrap_or_else(|| "web/dist".into())
  }))
  .unwrap_or_else(|e| fail(e));
  if !web.join("index.html").exists() {
    fail(format!(
      "no web window in {} (--web; cd web && bun run build)",
      web.display()
    ));
  }
  web
}

/// Fails unless people are kept apart: by a sandbox that works here, or
/// (`--no-sandbox`) by listening on loopback only.
fn walled(a: &Args) {
  if a.sandbox {
    if let Err(why) = sandbox::check() {
      fail(format!("{why}\n(--no-sandbox runs without one, for development only)"));
    }
    return;
  }
  let ip = a
    .addr
    .rsplit_once(':')
    .map_or(a.addr.as_str(), |(h, _)| h)
    .trim_matches(['[', ']']);
  let loopback = ip == "localhost" || ip.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
  if !loopback {
    fail(format!("--no-sandbox listens on loopback only, not {}", a.addr));
  }
  eprintln!("acorn-host: WARNING: no sandbox: each person's agent can read every other person's files and the key");
}

/// Serves until Ctrl-C or a TERM, then ends every person's server.
async fn run(host: &Arc<Host>, listener: tokio::net::TcpListener) {
  let app = proxy::app(host.clone());
  let (stop, stopped) = tokio::sync::watch::channel(false);
  tokio::spawn(async move {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("a TERM handler");
    tokio::select! {
      _ = tokio::signal::ctrl_c() => {}
      _ = term.recv() => {}
    }
    let _ = stop.send(true);
  });
  let mut on_stop = stopped.clone();
  let serving = tokio::spawn(async move {
    let _ = axum::serve(listener, app)
      .with_graceful_shutdown(async move {
        let _ = on_stop.wait_for(|&s| s).await;
      })
      .await;
  });
  let mut on_stop = stopped;
  let _ = on_stop.wait_for(|&s| s).await;
  // every server gets its TERM at once, and 10 s; the polls open on them
  // end with them
  let runners: Vec<_> = host.runners.lock().unwrap().values().cloned().collect();
  let ends: Vec<_> = runners
    .into_iter()
    .map(|r| tokio::spawn(async move { r.end(Duration::from_secs(10)).await }))
    .collect();
  for end in ends {
    let _ = end.await;
  }
  let _ = tokio::time::timeout(Duration::from_secs(2), serving).await;
}
