//! acorn-host with the real acorn-server (`cargo build -p acorn-server`
//! first): sign-in, forwarding, restarts, and the sandbox.

use acorn_host::sandbox::{self, Sandbox};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Bytes;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn host_bin() -> PathBuf {
  PathBuf::from(env!("CARGO_BIN_EXE_acorn-host"))
}

fn server_bin() -> PathBuf {
  let s = host_bin().with_file_name("acorn-server");
  assert!(s.exists(), "no {}: cargo build -p acorn-server first", s.display());
  s
}

fn sandboxed() -> bool {
  match sandbox::check() {
    Ok(()) => true,
    Err(why) => {
      eprintln!("without a sandbox: {why}");
      false
    }
  }
}

struct Reply {
  status: u16,
  headers: hyper::HeaderMap,
  body: Vec<u8>,
}

async fn ask(addr: &str, method: &str, path: &str, cookie: Option<&str>, body: Vec<u8>) -> Reply {
  let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
  let (mut send, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
    .await
    .expect("handshake");
  tokio::spawn(conn);
  let mut req = hyper::Request::builder()
    .method(method)
    .uri(path)
    .header("host", "acorn.test");
  if let Some(c) = cookie {
    req = req.header("cookie", c);
  }
  if !body.is_empty() {
    req = req.header("content-type", "application/json");
  }
  let res = send
    .send_request(req.body(Full::new(Bytes::from(body))).unwrap())
    .await
    .expect("send");
  let status = res.status().as_u16();
  let headers = res.headers().clone();
  let body = res.into_body().collect().await.expect("body").to_bytes().to_vec();
  Reply { status, headers, body }
}

async fn get(addr: &str, path: &str, cookie: Option<&str>) -> Reply {
  ask(addr, "GET", path, cookie, Vec::new()).await
}

struct Running {
  child: Child,
  addr: String,
}

impl Drop for Running {
  fn drop(&mut self) {
    unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
    let _ = self.child.wait();
  }
}

/// A host on its data dir, on a free port.
fn serve(data: &Path, web: &Path, sandbox: bool) -> Running {
  let mut cmd = Command::new(host_bin());
  cmd
    .args(["serve", "--addr", "127.0.0.1:0", "--data"])
    .arg(data)
    .arg("--server")
    .arg(server_bin())
    .arg("--web")
    .arg(web)
    .stdout(Stdio::piped());
  if !sandbox {
    cmd.arg("--no-sandbox");
  }
  let mut child = cmd.spawn().expect("acorn-host serve");
  let mut line = String::new();
  std::io::BufReader::new(child.stdout.take().unwrap())
    .read_line(&mut line)
    .unwrap();
  let addr = line
    .split("http://")
    .nth(1)
    .and_then(|s| s.split(',').next())
    .unwrap_or_else(|| panic!("no address in {line:?}"))
    .to_string();
  Running { child, addr }
}

fn invite(data: &Path, name: &str) -> String {
  let out = Command::new(host_bin())
    .args(["invite", name, "--public", "http://acorn.test", "--data"])
    .arg(data)
    .output()
    .unwrap();
  assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
  let link = String::from_utf8(out.stdout).unwrap();
  link
    .trim()
    .strip_prefix("http://acorn.test")
    .expect("a link on --public")
    .to_string()
}

/// Joins: the session cookie, as a Cookie header.
async fn join(addr: &str, path: &str) -> String {
  let preview = get(addr, path, None).await;
  assert_eq!(preview.status, 200);
  assert_eq!(preview.headers["cache-control"], "no-store");
  assert!(!preview.headers.contains_key("set-cookie"));
  let r = ask(addr, "POST", path, None, Vec::new()).await;
  assert_eq!(r.status, 303, "{}", String::from_utf8_lossy(&r.body));
  assert_eq!(r.headers["location"], "/");
  let set = r.headers["set-cookie"].to_str().unwrap();
  assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"), "{set}");
  set.split(';').next().unwrap().to_string()
}

fn people(data: &Path) -> Vec<String> {
  let book: serde_json::Value =
    serde_json::from_str(&std::fs::read_to_string(data.join("host.json")).unwrap()).unwrap();
  book["people"]
    .as_array()
    .unwrap()
    .iter()
    .map(|p| p["id"].as_str().unwrap().to_string())
    .collect()
}

/// A temp dir with a short path (a socket's path is at most 108 bytes).
fn temp() -> tempfile::TempDir {
  tempfile::Builder::new().prefix("ah").tempdir_in("/tmp").unwrap()
}

/// The pid of the acorn-server listening on `sock`.
fn server_pid(sock: &Path) -> Option<i32> {
  let want = sock.to_string_lossy().to_string();
  std::fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
    let pid: i32 = e.file_name().to_str()?.parse().ok()?;
    let cmd = std::fs::read(e.path().join("cmdline")).ok()?;
    let args: Vec<String> = cmd
      .split(|&b| b == 0)
      .map(|a| String::from_utf8_lossy(a).to_string())
      .collect();
    (args.first()?.ends_with("acorn-server") && args.contains(&want)).then_some(pid)
  })
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_people_apart() {
  let dir = temp();
  let data = dir.path().join("data");
  let web = dir.path().join("web");
  std::fs::create_dir_all(&web).unwrap();
  std::fs::write(web.join("index.html"), "<!doctype html>acorn").unwrap();
  let sandbox = sandboxed();

  let link_a = invite(&data, "A");
  let link_b = invite(&data, "B");
  let ids = people(&data);
  let (a_id, b_id) = (&ids[0], &ids[1]);
  let host = serve(&data, &web, sandbox);
  let addr = host.addr.clone();

  // 1. a join link signs in once
  let preview = get(&addr, &link_a, None).await;
  assert_eq!(preview.status, 200);
  let a = join(&addr, &link_a).await;
  let again = ask(&addr, "POST", &link_a, None, Vec::new()).await;
  assert_eq!(again.status, 410);
  assert_eq!(
    String::from_utf8_lossy(&again.body).trim(),
    "This link was used or has expired. Ask for a new one."
  );
  assert_eq!(get(&addr, "/join/00", None).await.status, 200);
  assert_eq!(ask(&addr, "POST", "/join/00", None, Vec::new()).await.status, 410);
  let b = join(&addr, &link_b).await;

  // 2. no cookie, or a made-up one: 401
  let none = get(&addr, "/api/hello", None).await;
  assert_eq!(none.status, 401);
  assert_eq!(String::from_utf8_lossy(&none.body).trim(), "Open your invite link.");
  let made_up = format!("acorn_session={}", "ab".repeat(32));
  assert_eq!(get(&addr, "/api/hello", Some(&made_up)).await.status, 401);
  assert_eq!(get(&addr, "/", Some(&made_up)).await.status, 401);

  // 3. each to their own
  let hello = get(&addr, "/api/hello", Some(&a)).await;
  assert_eq!(hello.status, 200, "{}", String::from_utf8_lossy(&hello.body));
  assert_eq!(get(&addr, "/api/hello", Some(&b)).await.status, 200);
  assert_eq!(get(&addr, "/", Some(&a)).await.body, b"<!doctype html>acorn");
  let images = data.join("users").join(a_id).join(".acorn/chats/default/docs/images");
  std::fs::create_dir_all(&images).unwrap();
  std::fs::write(images.join("marker.txt"), "A's").unwrap();
  let marker = "/api/image?chat=default&name=marker.txt";
  let got = get(&addr, marker, Some(&a)).await;
  assert_eq!((got.status, got.body.as_slice()), (200, b"A's".as_slice()));
  assert_eq!(get(&addr, marker, Some(&b)).await.status, 404);

  // 4. B's server token with A's session still reaches A
  let b_token = std::fs::read_to_string(data.join("run").join(b_id).join("server-token")).unwrap();
  let both = format!("acorn_token={}; {a}", b_token.trim());
  assert_eq!(get(&addr, marker, Some(&both)).await.status, 200);
  let both = format!("{b}; acorn_token={}", b_token.trim());
  assert_eq!(get(&addr, marker, Some(&both)).await.status, 404);

  // 5. a 20 MB command reaches the server, which says it is no command
  let big = ask(&addr, "POST", "/api/cmd?chat=default", Some(&a), vec![b'x'; 20 << 20]).await;
  assert_eq!(big.status, 400, "{}", String::from_utf8_lossy(&big.body));

  // 6. a poll held its 25 s comes back
  let mut held = false;
  for _ in 0..3 {
    let hello: serde_json::Value = serde_json::from_slice(&get(&addr, "/api/hello", Some(&a)).await.body).unwrap();
    let path = format!("/api/events?after={}&epoch={}", hello["seq"], hello["epoch"]);
    let began = Instant::now();
    let r = get(&addr, &path, Some(&a)).await;
    assert_eq!(r.status, 200);
    if began.elapsed() >= Duration::from_secs(20) {
      held = true;
      break;
    }
  }
  assert!(held, "no poll waited its 25 s");

  // 7. a server killed answers again within 5 s
  let sock = data.join("run").join(a_id).join("sock/server.sock");
  let pid = server_pid(&sock).expect("A's server");
  unsafe { libc::kill(pid, libc::SIGKILL) };
  let killed = Instant::now();
  while server_pid(&sock) == Some(pid) {
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  let again = get(&addr, "/api/hello", Some(&a)).await;
  assert_eq!(again.status, 200);
  assert!(killed.elapsed() < Duration::from_secs(5), "took {:?}", killed.elapsed());
  assert_ne!(server_pid(&sock), Some(pid));

  // a new person's link, for a second device, and the list
  let out = Command::new(host_bin())
    .args(["users", "--data"])
    .arg(&data)
    .output()
    .unwrap();
  let list = String::from_utf8_lossy(&out.stdout);
  assert!(
    list.lines().count() == 2 && list.contains("\tA\t") && list.contains("up"),
    "{list}"
  );
  let out = Command::new(host_bin())
    .args(["link", b_id, "--public", "http://acorn.test", "--data"])
    .arg(&data)
    .output()
    .unwrap();
  let second = String::from_utf8(out.stdout).unwrap();
  let b2 = join(&addr, second.trim().strip_prefix("http://acorn.test").unwrap()).await;
  assert_eq!(get(&addr, marker, Some(&b2)).await.status, 404);

  // the book keeps hashes only, and only its owner reads it
  let text = std::fs::read_to_string(data.join("host.json")).unwrap();
  assert!(!text.contains(a.trim_start_matches("acorn_session=")));
  use std::os::unix::fs::PermissionsExt as _;
  assert_eq!(
    std::fs::metadata(data.join("host.json")).unwrap().permissions().mode() & 0o777,
    0o600
  );

  // a TERM stops the servers too
  let mut host = host;
  unsafe { libc::kill(host.child.id() as i32, libc::SIGTERM) };
  let began = Instant::now();
  let status = host.child.wait().unwrap();
  assert!(status.success(), "{status}");
  assert!(began.elapsed() < Duration::from_secs(12));
  assert_eq!(server_pid(&sock), None);
}

/// Host-owned runtime paths must not follow links in an agent's HOME.
#[tokio::test(flavor = "multi_thread")]
async fn host_does_not_follow_home_symlinks() {
  let modes = if sandbox::check().is_ok() {
    vec![true, false]
  } else {
    vec![false]
  };
  for sandbox in modes {
    let dir = temp();
    let data = dir.path().join("data");
    let web = dir.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html>acorn").unwrap();
    let link_a = invite(&data, "A");
    let link_b = invite(&data, "B");
    let ids = people(&data);
    let (a_id, b_id) = (&ids[0], &ids[1]);
    let mut host = serve(&data, &web, sandbox);
    let a = join(&host.addr, &link_a).await;
    let b = join(&host.addr, &link_b).await;
    let b_hello: serde_json::Value =
      serde_json::from_slice(&get(&host.addr, "/api/hello", Some(&b)).await.body).unwrap();

    let a_home = data.join("users").join(a_id);
    let b_home = data.join("users").join(b_id);
    let a_sock = data.join("run").join(a_id).join("sock/server.sock");
    let old_acorn = a_home.join(".acorn.original");
    let _ = server_pid(&a_sock).expect("A's server");
    unsafe { libc::kill(server_pid(&a_sock).unwrap(), libc::SIGKILL) };
    while server_pid(&a_sock).is_some() {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
    std::fs::rename(a_home.join(".acorn"), &old_acorn).unwrap();
    std::os::unix::fs::symlink(b_home.join(".acorn"), a_home.join(".acorn")).unwrap();

    let a_hello = loop {
      let response = get(&host.addr, "/api/hello", Some(&a)).await;
      if response.status == 200 {
        break serde_json::from_slice::<serde_json::Value>(&response.body).unwrap();
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_ne!(a_hello["epoch"], b_hello["epoch"], "A reached B's server");

    // A link in the agent's HOME must not redirect the host's log either.
    unsafe { libc::kill(server_pid(&a_sock).unwrap(), libc::SIGKILL) };
    while server_pid(&a_sock).is_some() {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
    std::fs::remove_file(a_home.join(".acorn")).unwrap();
    std::fs::rename(&old_acorn, a_home.join(".acorn")).unwrap();
    let outside = data.join("outside.log");
    std::fs::write(&outside, "sentinel\n").unwrap();
    std::os::unix::fs::symlink(&outside, a_home.join(".acorn/server.log")).unwrap();
    loop {
      let response = get(&host.addr, "/api/hello", Some(&a)).await;
      if response.status == 200 {
        break;
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "sentinel\n");
    unsafe { libc::kill(host.child.id() as i32, libc::SIGTERM) };
    let _ = host.child.wait();
  }
}

/// 8. what a person's agent sees in its sandbox
#[test]
fn sandbox_shows_its_own_home_only() {
  if let Err(why) = sandbox::check() {
    eprintln!("skipped: {why}");
    return;
  }
  let dir = temp();
  let data = dir.path().join("data");
  for id in ["uaaaaaaaaaa", "ubbbbbbbbbb"] {
    std::fs::create_dir_all(data.join("users").join(id)).unwrap();
  }
  std::fs::write(data.join("host.json"), "{}").unwrap();
  std::fs::write(data.join("config.json"), "{}").unwrap();
  let home = data.join("users/uaaaaaaaaaa");
  let sock = data.join("run/sock");
  std::fs::create_dir_all(&sock).unwrap();
  let me = std::env::current_exe().unwrap();
  let show = [me.clone()];
  let args = Sandbox {
    data: &data,
    home: &home,
    sock: &sock,
    show: &show,
    info_fd: None,
  }
  .args();
  let d = data.display();
  let script = format!(
    r#"
    echo "users: $(ls {d}/users | tr '\n' ' ')"
    cat {d}/host.json >/dev/null 2>&1 && echo "read host.json"
    cat {d}/config.json >/dev/null 2>&1 && echo "read config.json"
    echo "procs: $(cat /proc/[0-9]*/comm | sort -u | tr '\n' ' ')"
    echo "run/user: $(ls -A /run/user | wc -l)"
    test -r {src} && echo "read the source"
    test -x {me} || echo "no test binary"
    echo kept > $HOME/note.txt
    echo "home: $HOME"
    "#,
    src = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
    me = me.display(),
  );
  let out = Command::new("bwrap")
    .args(args)
    .args(["sh", "-c", &script])
    .env_clear()
    .env("PATH", "/usr/bin:/bin")
    .env("HOME", &home)
    .output()
    .unwrap();
  let said = String::from_utf8_lossy(&out.stdout);
  assert!(out.status.success(), "{said}{}", String::from_utf8_lossy(&out.stderr));
  assert!(said.contains("users: uaaaaaaaaaa \n"), "{said}");
  assert!(
    !said.contains("read host.json") && !said.contains("read config.json"),
    "{said}"
  );
  // the shell's own, and bwrap's init: none from outside
  let procs = said.lines().find_map(|l| l.strip_prefix("procs: ")).unwrap();
  for comm in procs.split_whitespace() {
    assert!(
      ["bwrap", "sh", "cat", "sort", "tr"].contains(&comm),
      "{comm} seen: {said}"
    );
  }
  assert!(said.contains("run/user: 0"), "{said}");
  assert!(!said.contains("read the source"), "{said}");
  assert!(!said.contains("no test binary"), "{said}");
  assert_eq!(std::fs::read_to_string(home.join("note.txt")).unwrap(), "kept\n");
}
