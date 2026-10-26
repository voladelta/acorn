//! Machine
//! =======
//! The tools that touch this Mac: shell, read, write, edit (TOOLS), each in
//! its agent's dir under the work dir (computer use is `computer`'s). A
//! stop kills the job's shell and its process group. As Acorn quits,
//! `end` kills every shell's group, and any process marked ACORN_RUN that
//! left it. A file changed (write over one, edit) answers as a unified
//! diff, which the app draws as one.

use crate::ai::{Out, Spec};
use crate::clip::{self, CAP, Clip};
use crate::config;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// The lines kept around each change in a diff.
const CONTEXT: usize = 3;

/// This Acorn's mark, set as ACORN_RUN on every process its shells start
/// (each child inherits it), to find those that left their shell's group.
static RUN: LazyLock<String> = LazyLock::new(|| format!("{:016x}", rand::random::<u64>()));

/// The shells started, by process group, with whether each exited, kept
/// until their groups are empty, so `end` kills a backgrounded job whose
/// shell already exited.
static SHELLS: LazyLock<Mutex<HashMap<i32, Arc<AtomicBool>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn spec(name: &'static str, info: impl Into<String>, fields: Value, need: &[&str]) -> Spec {
    Spec { name, info: info.into(), schema: json!({"type": "object", "properties": fields, "required": need}) }
}

pub fn tools() -> Vec<Spec> {
    let str = json!({"type": "string"});
    let int = json!({"type": "integer"});
    let edit = json!({"type": "object", "properties": {"path": str, "old": str, "new": str, "all": {"type": "boolean"}}, "required": ["path", "old", "new"]});
    vec![
        spec("shell", format!("Run a command with this Mac's shell ($SHELL -c), stdout and stderr together, and return its output, then its exit code and time, as (exit 0 in 0.4 s); output past {CAP} characters keeps its head and tail. Each call is a new shell. The call waits until the command and everything holding its output end, so give background jobs their own output file. Background jobs live only while Acorn is open: quitting it kills every process the calls started."), json!({"command": str}), &["command"]),
        spec("read", format!("Read a text file. offset: the first line to show (from 1); limit: how many lines; at: the character of the first line to start from. It ends saying the lines shown, as [lines 4-7 of 15]; output past {CAP} characters comes in pages, saying where to go on."), json!({"path": str, "offset": int, "limit": int, "at": int}), &["path"]),
        spec("write", "Write a whole file, making its directories. Answers a unified diff of what changed (a new file: all its lines added).", json!({"path": str, "content": str}), &["path", "content"]),
        spec("edit", "Replace text in files: the edits in order, each on its file as the edits before it left it. old must match exactly, whitespace included, and only once, unless all is true, which replaces every match. If any edit fails, no file is changed. Answers a unified diff of each file.", json!({"edits": {"type": "array", "items": edit, "minItems": 1}}), &["edits"]),
    ]
}

/// A tool of TOOLS, run in the dir of its agent (`cwd`, under its
/// assistant's work dir).
pub async fn run(name: &str, input: &Value, cwd: PathBuf, cancel: CancellationToken) -> Out {
    if let Err(err) = fs::create_dir_all(&cwd) {
        return Out::fail(err.to_string());
    }
    let str = |k: &str| input[k].as_str().map_or_else(|| input[k].to_string(), str::to_string);
    let num = |k: &str| input[k].as_f64();
    let file = cwd.join(str("path"));
    let done = match name {
        "shell" => return shell(&cwd, &str("command"), cancel).await,
        "read" => read(&file, num("offset").unwrap_or(1.0), num("limit").unwrap_or(f64::INFINITY), num("at").unwrap_or(0.0)),
        "write" => write(&file, &str("path"), &str("content")),
        "edit" => edit(&cwd, &input["edits"]),
        _ => return Out::fail(format!("No tool {name}.")),
    };
    done.unwrap_or_else(|err| Out::fail(err.to_string()))
}

// Shell
// =====

/// Runs a command in its own process group, every process marked
/// ACORN_RUN, its output decoded as one stream and cut as it comes. A stop
/// kills the group and ends the wait at once, whatever still holds its
/// output.
pub async fn shell(cwd: &Path, command: &str, cancel: CancellationToken) -> Out {
    SHELLS.lock().retain(|&pid, exited| group(pid, exited));
    let start = Instant::now();
    let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut cmd = tokio::process::Command::new(sh);
    cmd.arg("-c")
        .arg(format!("exec 2>&1\n{command}"))
        .current_dir(cwd)
        .env("ACORN_RUN", &*RUN)
        .env_remove(config::KEY_VAR)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => return Out::fail(err.to_string()),
    };
    let pid = child.id().map_or(0, |p| p as i32);
    let exited = Arc::new(AtomicBool::new(false));
    SHELLS.lock().insert(pid, exited.clone());
    let mut stdout = child.stdout.take().expect("its output, piped");
    let mut out = Clip::default();
    let mut buf = vec![0u8; 1 << 16];
    let mut rest: Vec<u8> = Vec::new();
    loop {
        let got = tokio::select! {
            got = stdout.read(&mut buf) => got,
            _ = cancel.cancelled() => {
                kill(-pid);
                break;
            }
        };
        match got {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                rest.extend_from_slice(&buf[..n]);
                let whole = match std::str::from_utf8(&rest) {
                    Ok(_) => rest.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(_) => rest.len(),
                };
                out.add(&String::from_utf8_lossy(&rest[..whole]));
                rest.drain(..whole);
            }
        }
    }
    out.add(&String::from_utf8_lossy(&rest));
    let status = child.wait().await;
    exited.store(true, Ordering::Relaxed);
    let time = format!("{:.1} s", start.elapsed().as_secs_f64());
    let exit = match status {
        _ if cancel.is_cancelled() => "stopped".to_string(),
        Ok(s) if s.signal().is_some() => format!("killed by {}", signal_name(s.signal().unwrap())),
        Ok(s) => format!("exit {}", s.code().unwrap_or(-1)),
        Err(err) => err.to_string(),
    };
    let text = out.text();
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let head = if text.is_empty() { String::new() } else { format!("{text}\n") };
    Out { text: format!("{head}({exit} in {time})"), fail: exit != "exit 0", imgs: vec![] }
}

fn signal_name(sig: i32) -> String {
    let names = [(1, "SIGHUP"), (2, "SIGINT"), (3, "SIGQUIT"), (6, "SIGABRT"), (9, "SIGKILL"), (10, "SIGBUS"), (11, "SIGSEGV"), (13, "SIGPIPE"), (14, "SIGALRM"), (15, "SIGTERM")];
    names.iter().find(|(n, _)| *n == sig).map_or_else(|| format!("signal {sig}"), |(_, name)| name.to_string())
}

/// Whether a shell's process group may still hold a job: the shell runs,
/// or it is gone but its id names no live process and its group still has
/// one.
fn group(pid: i32, exited: &AtomicBool) -> bool {
    !exited.load(Ordering::Relaxed) || (!alive(pid) && alive(-pid))
}

/// Whether a process (pid) or a process group (-pid) exists.
fn alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 sends nothing; it only checks.
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

fn kill(pid: i32) {
    if pid != 0 && pid != -1 {
        // SAFETY: a signal to a process or group this Acorn started.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// As Acorn quits: kills every shell's process group (a job still in it
/// dies), then any process that left its group (ACORN_RUN), so none is
/// left orphaned.
pub fn end() {
    for (pid, exited) in SHELLS.lock().iter() {
        if group(*pid, exited) {
            kill(-pid);
        }
    }
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() }.to_string();
    let Ok(ps) = std::process::Command::new("ps").args(["-wwE", "-U", &uid, "-o", "pid=,command="]).output() else { return };
    let mark = format!("ACORN_RUN={}", *RUN);
    for line in String::from_utf8_lossy(&ps.stdout).lines().filter(|l| l.contains(&mark)) {
        if let Some(pid) = line.split_whitespace().next().and_then(|p| p.parse().ok()) {
            kill(pid);
        }
    }
}

// Files
// =====

/// `limit` lines from line `offset`, from its character `at`: a page of
/// them, and past it where to go on; failed if there are none.
pub fn read(file: &Path, offset: f64, limit: f64, at: f64) -> anyhow::Result<Out> {
    let data = fs::read(file)?;
    if data.contains(&0) {
        return Ok(Out::fail(format!("A binary file of {} bytes.", data.len())));
    }
    let text = String::from_utf8_lossy(&data);
    let lines: Vec<&str> = text.strip_suffix('\n').unwrap_or(&text).split('\n').collect();
    let from = offset.max(1.0) as usize;
    let rows: Vec<&str> = lines.iter().skip(from - 1).take(if limit.is_finite() { limit.max(0.0) as usize } else { usize::MAX }).copied().collect();
    if rows.is_empty() {
        return Ok(Out::fail(format!("No lines from line {from}: the file has {}.", lines.len())));
    }
    let text = rows.join("\n");
    let n = text.chars().count();
    let at = (at.max(0.0) as usize).min(n);
    let end = clip::page_end(&text, at);
    let whole = end == n;
    let head: Vec<&str> = text[..clip::byte_at(&text, end)].split('\n').collect();
    let next = from + head.len() - 1;
    let col = head.last().map_or(0, |l| l.chars().count());
    let last = if whole { from + rows.len() - 1 } else if col == 0 { next - 1 } else { next };
    let go = if whole { String::new() } else if col == 0 { format!("; go on with offset {next}") } else { format!("; go on with offset {next}, at {col}") };
    let page = &text[clip::byte_at(&text, at)..clip::byte_at(&text, end)];
    let page = page.strip_suffix('\n').unwrap_or(page);
    Ok(Out::ok(format!("{page}\n[lines {from}-{last} of {}{go}]", lines.len())))
}

/// A file (named as given: shown) written whole: its diff (a new one's,
/// all its lines added), so a write reads as an edit.
pub fn write(file: &Path, shown: &str, content: &str) -> anyhow::Result<Out> {
    let was = fs::read_to_string(file).unwrap_or_default();
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, content)?;
    Ok(Out::ok(diff(shown, &was, content)))
}

/// The edits, in order, each on its file as the ones before left it;
/// every file is written only once all of them apply, so a failed one
/// changes none. Each file, named as its first edit gives it, answers its
/// diff.
pub fn edit(cwd: &Path, edits: &Value) -> anyhow::Result<Out> {
    struct File {
        shown: String,
        was: String,
        text: String,
    }
    let mut files: Vec<(PathBuf, File)> = Vec::new();
    for (k, e) in edits.as_array().into_iter().flatten().enumerate() {
        let path = e["path"].as_str().map_or_else(|| e["path"].to_string(), str::to_string);
        let file = cwd.join(&path);
        let known = files.iter().position(|(f, _)| *f == file);
        let was = match known {
            Some(k) => Some(files[k].1.was.clone()),
            None => fs::read_to_string(&file).ok(),
        };
        let text = known.map_or_else(|| was.clone().unwrap_or_default(), |k| files[k].1.text.clone());
        let old = e["old"].as_str().unwrap_or_default();
        let count = if old.is_empty() { 0 } else { text.matches(old).count() };
        let why = match () {
            _ if was.is_none() => "no such file".to_string(),
            _ if count == 0 => "old was not found".to_string(),
            _ if count > 1 && e["all"] != true => format!("old matches {count} times; give more context, or all"),
            _ => String::new(),
        };
        if !why.is_empty() {
            return Ok(Out::fail(format!("Edit {} ({path}): {why}. No file was changed.", k + 1)));
        }
        let text = text.replace(old, e["new"].as_str().unwrap_or_default());
        match known {
            Some(k) => files[k].1.text = text,
            None => files.push((file, File { shown: path, was: was.unwrap_or_default(), text })),
        }
    }
    if files.is_empty() {
        return Ok(Out::fail("edits is empty: give a list of {path, old, new}."));
    }
    for (file, f) in &files {
        fs::write(file, &f.text)?;
    }
    Ok(Out::ok(files.iter().map(|(_, f)| diff(&f.shown, &f.was, &f.text)).collect::<Vec<_>>().join("\n")))
}

// Diff
// ====

/// A file's change as a unified diff: its name as old and new file, then
/// each run of changes in a hunk with CONTEXT lines around it (runs that
/// close share one), clipped as a command's output. Lines keep their "\n",
/// so a last one without it differs, and says so as git does.
pub fn diff(file: &str, was: &str, now: &str) -> String {
    let (a, b) = (text_lines(was), text_lines(now));
    let ops = lines_diff(&a, &b);
    let mut at = Vec::with_capacity(ops.len());
    let (mut i, mut j) = (0, 0);
    for (s, _) in &ops {
        at.push((i, j));
        i += usize::from(*s != '+');
        j += usize::from(*s != '-');
    }
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for k in ops.iter().enumerate().filter(|(_, (s, _))| *s != ' ').map(|(k, _)| k) {
        match hunks.last_mut() {
            Some(top) if k - top.1 <= 2 * CONTEXT => top.1 = k + 1,
            _ => hunks.push((k, k + 1)),
        }
    }
    if hunks.is_empty() {
        return format!("{file} is unchanged.");
    }
    let mut out = Clip::default();
    out.add(&format!("--- {file}\n+++ {file}"));
    for (lo, hi) in hunks {
        let (a, b) = (lo.saturating_sub(CONTEXT), (hi + CONTEXT).min(ops.len()));
        let run = &ops[a..b];
        let olds = run.iter().filter(|(s, _)| *s != '+').count();
        let news = run.iter().filter(|(s, _)| *s != '-').count();
        let head = format!("@@ -{},{olds} +{},{news} @@", at[a].0 + usize::from(olds > 0), at[a].1 + usize::from(news > 0));
        let body: String = run.iter().map(|(s, x)| format!("\n{s}{}", x.strip_suffix('\n').unwrap_or(x))).collect();
        out.add(&format!("\n{head}{body}"));
    }
    out.text()
}

/// A text's lines, each with its "\n" (the last may lack it), and the
/// "\ No newline at end of file" git puts after such a line in a diff.
fn text_lines(text: &str) -> Vec<String> {
    text.split_inclusive('\n')
        .map(|l| if l.ends_with('\n') { l.to_string() } else { format!("{l}\n\\ No newline at end of file") })
        .collect()
}

/// The shortest script from lines a to lines b: their lines kept (the
/// most), and in each gap between two the old lines dropped, then the new
/// added, as git shows them. A line only one text has is never kept, so
/// the match runs without them.
fn lines_diff(a: &[String], b: &[String]) -> Vec<(char, String)> {
    use std::collections::HashSet;
    let (in_a, in_b): (HashSet<&str>, HashSet<&str>) = (a.iter().map(String::as_str).collect(), b.iter().map(String::as_str).collect());
    let ia: Vec<usize> = (0..a.len()).filter(|&i| in_b.contains(a[i].as_str())).collect();
    let ib: Vec<usize> = (0..b.len()).filter(|&j| in_a.contains(b[j].as_str())).collect();
    let xa: Vec<&str> = ia.iter().map(|&i| a[i].as_str()).collect();
    let xb: Vec<&str> = ib.iter().map(|&j| b[j].as_str()).collect();
    let mut pairs = Vec::new();
    lines_match(&xa, &xb, (0, xa.len(), 0, xb.len()), &mut pairs);
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    let push = |ops: &mut Vec<(char, String)>, s: char, xs: &[String], from: usize, to: usize| ops.extend(xs[from..to].iter().map(|x| (s, x.clone())));
    for (p, q) in pairs {
        push(&mut ops, '-', a, i, ia[p]);
        push(&mut ops, '+', b, j, ib[q]);
        ops.push((' ', a[ia[p]].clone()));
        (i, j) = (ia[p] + 1, ib[q] + 1);
    }
    push(&mut ops, '-', a, i, a.len());
    push(&mut ops, '+', b, j, b.len());
    ops
}

type Box4 = (usize, usize, usize, usize);

/// The lines of a and b kept in a box of them, as their pairs pushed in
/// order: a longest common subsequence (Myers, in linear space): the
/// common head and tail; between them the middle snake, and each part
/// around it done alike. Time (n+m)·d, d the lines changed.
fn lines_match(a: &[&str], b: &[&str], (mut a0, mut a1, mut b0, mut b1): Box4, pairs: &mut Vec<(usize, usize)>) {
    while a0 < a1 && b0 < b1 && a[a0] == b[b0] {
        pairs.push((a0, b0));
        a0 += 1;
        b0 += 1;
    }
    let mut tail = 0;
    while a0 < a1 && b0 < b1 && a[a1 - 1] == b[b1 - 1] {
        a1 -= 1;
        b1 -= 1;
        tail += 1;
    }
    if a0 < a1 && b0 < b1 {
        let (x, u, y, v) = snake(a, b, (a0, a1, b0, b1));
        lines_match(a, b, (a0, x, b0, y), pairs);
        pairs.extend((0..u - x).map(|k| (x + k, y + k)));
        lines_match(a, b, (u, a1, v, b1), pairs);
    }
    pairs.extend((0..tail).map(|k| (a1 + k, b1 + k)));
}

/// A box's middle snake, as the box it spans: where a shortest path from
/// its top corner meets one from its bottom, both grown a step d at a
/// time, each keeping the furthest x reached on each diagonal k = x - y
/// (vf from the top, vb from the bottom, counted back).
fn snake(a: &[&str], b: &[&str], (a0, a1, b0, b1): Box4) -> Box4 {
    let (n, m) = ((a1 - a0) as isize, (b1 - b0) as isize);
    let delta = n - m;
    let off = n + m + 1;
    let mut vf = vec![0isize; (2 * off + 1) as usize];
    let mut vb = vec![0isize; (2 * off + 1) as usize];
    let ix = |k: isize| (off + k) as usize;
    for d in 0.. {
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && vf[ix(k - 1)] < vf[ix(k + 1)]) { vf[ix(k + 1)] } else { vf[ix(k - 1)] + 1 };
            let x0 = x;
            while x < n && x - k < m && a[a0 + x as usize] == b[b0 + (x - k) as usize] {
                x += 1;
            }
            vf[ix(k)] = x;
            let c = delta - k;
            if delta % 2 != 0 && c.abs() < d && x + vb[ix(c)] >= n {
                return (a0 + x0 as usize, a0 + x as usize, (b0 as isize + x0 - k) as usize, (b0 as isize + x - k) as usize);
            }
            k += 2;
        }
        let mut c = -d;
        while c <= d {
            let mut x = if c == -d || (c != d && vb[ix(c - 1)] < vb[ix(c + 1)]) { vb[ix(c + 1)] } else { vb[ix(c - 1)] + 1 };
            let x0 = x;
            while x < n && x - c < m && a[a1 - 1 - x as usize] == b[(b1 as isize - 1 - x + c) as usize] {
                x += 1;
            }
            vb[ix(c)] = x;
            let k = delta - c;
            if delta % 2 == 0 && k.abs() <= d && x + vf[ix(k)] >= n {
                return (a1 - x as usize, a1 - x0 as usize, (b1 as isize - x + c) as usize, (b1 as isize - x0 + c) as usize);
            }
            c += 2;
        }
    }
    unreachable!("a snake meets")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_read_as_git() {
        let was = "a\nb\nc\nd\n";
        let now = "a\nB\nc\nd\ne";
        let d = diff("f", was, now);
        assert_eq!(d, "--- f\n+++ f\n@@ -1,4 +1,5 @@\n a\n-b\n+B\n c\n d\n+e\n\\ No newline at end of file");
        assert_eq!(diff("f", "x\n", "x\n"), "f is unchanged.");
        assert!(diff("new", "", "1\n2\n").starts_with("--- new\n+++ new\n@@ -0,0 +1,2 @@\n+1\n+2"));
    }

    #[test]
    fn diffs_find_the_fewest_changes() {
        let a: String = (0..200).map(|i| format!("{i}\n")).collect();
        let b: String = (0..200).filter(|i| i % 7 != 0).map(|i| format!("{i}\n")).collect();
        let d = diff("f", &a, &b);
        assert_eq!(d.lines().filter(|l| l.starts_with('-') && !l.starts_with("---")).count(), 29);
        assert_eq!(d.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).count(), 0);
    }

    #[test]
    fn reads_page_lines() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        fs::write(&f, "one\ntwo\nthree\n").unwrap();
        assert_eq!(read(&f, 2.0, 1.0, 0.0).unwrap().text, "two\n[lines 2-2 of 3]");
        assert!(read(&f, 9.0, f64::INFINITY, 0.0).unwrap().fail);
    }
}
