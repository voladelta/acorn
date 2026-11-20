# Code Quality Review Issues

## 1. Bugs & Crash Risks

### 1.1 ~~Linux orphan-process cleanup is broken (`ps -wwE` is BSD/macOS-only)~~ (fixed)
- **Status:** Fixed in [#5](https://github.com/voladelta/acorn/pull/5): on Linux `machine::end()` reads `/proc/<pid>/environ` (`marked_pids`).
- **Location:** `core/src/machine.rs:313-328` (before the fix)
- **Problem:** `machine::end()` invokes `ps -wwE -U <uid> -o pid=,command=` to find processes carrying `ACORN_RUN=...`. On Linux (`procps-ng`), `-E` is rejected (`error: unsupported SysV option`), so `end()` silently fails to reap detached processes (e.g., browsers or background jobs) when a standalone `acorn-server` exits on Linux. (Under `acorn-host`, bwrap's PID namespace and `--die-with-parent` already take such processes down.)
- **Fix:** On Linux, scan `/proc/<pid>/environ` (filtered by UID) instead of spawning `ps -wwE`.

### 1.2 UTF-8 byte-index panic in Telegram `@mention` stripping
- **Status:** Fixed in #6.
- **Location:** `bridges/telegram/src/lib.rs:492-500`
- **Problem:** `text.to_lowercase().find(&name)` computes a byte offset in the lowercased string and passes it to `out.replace_range(at..at + name.len(), "")` on the original string. Characters whose lowercase changes UTF-8 byte length (e.g. `İ` 2 bytes -> `i\u{307}` 3 bytes) corrupt the string or panic on an invalid char boundary / out-of-bounds range.
- **Fix:** Match `@<username>` case-insensitively on ASCII bytes directly against the original string (Telegram usernames are ASCII `[A-Za-z0-9_]`), so byte offsets remain valid in `text`.

### 1.3 PID-reuse kill race on shell command timeout
- **Status:** Fixed in #8.
- **Location:** `core/src/machine.rs:185-192` (before the fix)
- **Problem:** When a command times out, `shell()` sends `SIGTERM` to `-pid` and spawns a detached task that sleeps `GRACE` (3 s) and unconditionally calls `signal(-pid, libc::SIGKILL)`. If the child exits immediately on `SIGTERM` and is reaped, that task still sends `SIGKILL` 3 seconds later—potentially to a reused PID/PGID.
- **Fix:** Make the `SIGKILL` grace timer a `tokio::time::Sleep` arm inside the existing `tokio::select!` loop, guarded by `timed_out` and the group still existing (`alive(-pid)`), not by `status.is_none()`: the current SIGKILL also takes down background jobs in the group that ignore SIGTERM after the shell itself exits, and that must keep working.

### 1.4 Integer overflow / underflow / `Duration` panics on external inputs
- **Status:** Fixed in #6.
- **`server/src/main.rs:283-294`:** `?after=18446744073709551615` (`u64::MAX`) overflows `after + 1` (panics that request in debug; wraps to `0` in release, which has no `overflow-checks`). Use `saturating_add`/`saturating_sub`.
- **`bridges/telegram/src/html.rs:80-86`:** `let room = PIECE - if code { open.len() + 8 } else { 0 };` underflows if a fenced code block's opening line exceeds ~3,492 bytes, and mixes byte length (`open.len()`) with char count (`PIECE`). Use `saturating_sub` with a floor and consistent units.
- **`core/src/timers.rs:182`:** `now + duration(&after)?` uses unchecked `i64` addition even though `duration()` uses `checked_mul`/`checked_add`. Use `now.checked_add(...)`.
- **`core/src/ai/agent.rs:301-308`:** `Duration::from_secs_f64(wait + jitter)` panics if `Retry-After` parses as `inf`, `NaN`, or a huge value. Filter with `is_finite()` and clamp to a sensible maximum.
- **`core/src/memory/images.rs:173-177`:** `resize_exact((w as f64 * k).round() as u32, (h as f64 * k).round() as u32, ...)` rounds to `0` for extreme aspect ratios (e.g. `10000x1`), failing JPEG encoding. Clamp both dimensions with `.max(1)`.

### 1.5 Size and pixel caps checked after full allocation/decode (OOM / decompression-bomb hazard)
- **`core/src/luau.rs:854-861`:** `__view` calls `std::fs::read(&file)` before checking `bytes.len() > VIEW_BYTES` (unlike `send_file` and `machine::text`, which check `metadata()` first). Check `metadata().is_file()` and `metadata().len() <= VIEW_BYTES` before reading.
  - **Status:** Fixed in #6.
- **`core/src/memory/images.rs:161-172`:** `fitted()` calls `image::load_from_memory(bytes)` *before* checking `(w as u64) * (h as u64) <= PIXELS`, so that check never guards the decode. `image` 0.25's default `Limits` (512 MiB `max_alloc`) still bounds a decompression bomb, so this is low severity; inspect dimensions first via `ImageReader::into_dimensions()` or set explicit `image::Limits`.
- **`bridges/telegram/src/api.rs:162-183`:** `download()` has no streaming size cap when `file_size` is absent, and ignores `cancel` during the HTTP GET and `res.bytes().await`.

### 1.6 Missing RAII guards on shared state across `.await` / panics
- **`core/src/ai/agent.rs:257-313`:** `pace::hold` / `pace::free` is tracked with a manual `bool` (`asking`). Any panic or early future drop permanently leaks `room.busy > 0`, starving yielding/import calls for the rest of the process lifetime. Wrap in a `Drop` guard like `pace::Seat` and `cache::Flight`.
  - **Status:** Fixed in #6.
- **`core/src/chat/mod.rs:312-323`:** `Engine::writing` sets `chat.held = Some(Vec::new())` and only drains it after `write().await`. If `write()` panics or is cancelled, `chat.held` remains `Some(...)`, silently buffering subsequent `chat.hold(...)` callbacks.

### 1.7 Cancellation ignored in Telegram reconnect sleeps
- **Status:** Fixed in #6.
- **Location:** `bridges/telegram/src/lib.rs:189-192`, `bridges/telegram/src/lib.rs:240-245`
- **Problem:** `tokio::time::sleep(wait).await` does not `select!` on `self.stop.cancelled()` (unlike `rooms::sweeper`), so shutdown can stall during backoff: up to 5 minutes on the error path (line 244), `POLL / 2` on the conflict path (line 240).
- **Fix:** Select on `self.stop.cancelled()` in both sleep sites.

### 1.8 Unhandled `nil` dereference in `plugins/lightpanda.luau`
- **Status:** Fixed in #6.
- **Location:** `plugins/lightpanda.luau:490-493`
- **Problem:** If Jev returns a `SELECT` target without a `:index` suffix, `el.options[nil]` evaluates to `nil` and `o.value` raises an uncaught Luau runtime error.
- **Fix:** Guard `if o then ...` and fall through to the unusable-target branch when `o` is `nil`.

---

## 2. Security & Data Integrity

### 2.1 TOCTOU world-readable window on secrets and `0o600` files
- **Status:** Fixed in #6.
- **`server/src/main.rs:229-235`:** `std::fs::write(&file, &t)` creates `server-token` with default umask (typically `0o644`) before calling `set_permissions(..., 0o600)`. Use `OpenOptions::new().mode(0o600)` at creation time (matching `host/src/runner.rs:262`).
- **`core/src/machine.rs:645-653`:** `commit()` creates its temp file with default umask and writes full contents before applying `was.permissions().mode()`, briefly exposing `0o600` files like `config.json` during edits. Set permissions via `OpenOptionsExt::mode` when creating the temp file.

### 2.2 Unquoted shell argument in `plugins/github.luau`
- **Status:** Fixed in #6.
- **Location:** `plugins/github.luau:50-59`
- **Problem:** `path` is shell-quoted with `quote(path)`, but `method` is interpolated raw into `GH .. " api -X " .. method .. " " .. quote(path)`.
- **Note:** The model already has `sh`, so this is a correctness/robustness issue rather than privilege escalation.
- **Fix:** Quote `method` with `quote(method)` (or validate against `^[A-Z]+$`).

### 2.3 Purged secrets bypass redaction in streaming deltas and thinking entries
- **Location:** `core/src/chat/turns.rs:479-515`
- **Problem:** `Chat::purge_text` records purged secrets in `self.secrets` so `Chat::push` redacts future logged messages, but `AiEvent::Delta` and `Kind::Mind` entries are emitted directly via `self.all(...)` without passing through `self.redact(...)`.
- **Fix:** Apply `self.redact(...)` to `Kind::Mind` entries and (where practical) buffered delta output when `self.secrets` is non-empty.

### 2.4 Open-redirect pattern & missing `Secure` cookie flag in `acorn-server`
- **Status:** Redirect fixed in #6; `Secure` cookie option still open.
- **Location:** `server/src/main.rs:243-252`
- **Problem:** `Redirect::to(req.uri().path())` allows protocol-relative redirects (`//evil.com/?token=...`) if the token is known (low severity: whoever holds the token already has full access). Additionally, the `acorn_token` cookie omits `; Secure` even when deployed behind HTTPS.
- **Fix:** Collapse/reject leading `//` in redirect paths and provide a `--secure` option (or detect HTTPS) to set `; Secure` on the cookie.

### 2.5 Non-atomic writes and concurrent file races
- **`core/src/assistant/store.rs:149-155`:** `assistants.json` is written directly with `std::fs::write`, while `saved()` reads without holding `FILING`. Concurrent readers (e.g. `/api/hello`) can observe truncated JSON and silently fall back to an empty list. Write via temp file + rename.
  - **Status:** Fixed in #6.
- **`bridges/telegram/src/rooms.rs:116-123` vs `168-178`:** `sweep()` reads `<id>.jsonl`, filters old lines, and renames `<id>.part` over it without locking against concurrent `heard()` appends, risking silent loss of messages received mid-sweep.
- **`core/src/extend.rs:83-93`:** `put()` names its temp file using only `std::process::id()`, so concurrent `extend` calls in the same process for the same skill/plugin collide on the same `.tmp` file.
  - **Status:** Fixed in #6.
- **`core/src/timers.rs:192-206, 221-229`:** In-memory `BOOK` is mutated before `save(&book)?`; a failed save leaves in-memory timers out of sync with `timers.json`.

### 2.6 No session expiration or revocation in `acorn-host`
- **Location:** `host/src/store.rs:41-46`
- **Problem:** `Session::created` is stored in `host.json` but never checked for expiration, and there is no CLI command to revoke a compromised device session or disable a user.

### 2.7 Unescaped `"` in Telegram HTML code-block language attribute
- **Status:** Fixed in #6.
- **Location:** `bridges/telegram/src/html.rs:132-135`
- **Problem:** `escape(lang)` escapes `&`, `<`, `>` but not `"` (unlike `<a href>` on line 220), so a fence info string containing `"` breaks `class="language-..."` and forces plain-text fallback.

---

## 3. Performance & Async Runtime Hygiene

### 3.1 Blocking I/O and heavy synchronous work on Tokio async workers
- **`server/src/main.rs:154`:** `tokio::spawn(async { hub::open_all() })` runs blocking SQLite/FS opens directly on an async worker instead of `spawn_blocking`.
- **`server/src/main.rs:289-301`:** `events()` holds the global `s.log` mutex while copying and `join(",")`-ing up to 256 MB (`KEEP_BYTES`) of JSON strings, blocking `Server::tell` and `/api/hello`.
- **`host/src/proxy.rs:128-137`:** `forward()` runs synchronous `host.store.read()` (disk read + JSON parse) on every proxied HTTP request (unlike `preview` and `join`, which use `spawn_blocking`). Cache `Book` by mtime/inode or offload reads.
- **`core/src/luau.rs:768-808, 854`, `core/src/ocr.rs:76-78`, `core/src/facts.rs:111-113, 137`:** `__read`/`__write`/`__edit` (up to 100 MB + `fs::sync_all()`), `ocr::read` (up to 100 MB `fs::read`), and `facts::remember`/`forget` (SQLite `VACUUM` + WAL checkpoint) all run synchronously inside async tool handlers.

### 3.2 $O(N^2)$ compaction queue removal on large imports
- **Location:** `core/src/memory/mod.rs:654-670`
- **Problem:** `self.ready[side]` stores ready indices in sorted `Vec<u64>`s and removes compacted items via `queue.remove(at)`. Draining from the front during a 50k-message import performs ~1.25 billion element shifts.
- **Fix:** Use `BTreeSet<u64>` for $O(\log N)$ insert/remove while keeping `first(JOBS - 1, side)` $O(\text{JOBS}) = O(1)$.

### 3.3 Repeated heap allocations in `names::is_name` (and `names::random`)
- **Status:** Fixed in #6.
- **Location:** `core/src/names.rs:32-49`
- **Problem:** Every `is_name` call splits `ADJS` and `NOUNS` and allocates 256 capitalized `String`s; every `random` call collects both lists into fresh `Vec<&str>`s (unlike `web/src/names.ts`, which precomputes `adjs` and `Set(nouns)` once).
- **Fix:** Cache the capitalized adjective list and noun `HashSet` in `LazyLock` statics.

### 3.4 Unbounded in-memory caches in long-running processes
- **`core/src/machine.rs:68`:** `SEEN` grows indefinitely over the process lifetime.
- **`web/src/model.ts:49-54`:** `whole`, `nodes`, `minds`, and `pages` never prune evicted entries in long-running browser tabs.

---

## 4. Maintainability, Duplication & Tooling

### 4.1 Desktop (`app/src/`) and web (`web/src/`) UI logic duplication with active drift
- **Locations:** `app/src/{model,call,text,look,rows}.rs` vs `web/src/{model,call,text,names,wire}.ts` and `web/src/ui/Rows.tsx`
- **Problem:** ~1,500 LOC of UI state machine, tool-call parsing, diff parsing, name matching, wire types, and glyphs are hand-duplicated across Rust and TypeScript. Drift has already occurred: `app/src/look.rs:62-63` maps `abilities` (`🪞`) and `extend` (`🌱`), while `web/src/ui/Rows.tsx:25-52` omits both.
- **Fix:** Share a fixture-driven conformance test suite (or generate `wire.ts` / `names.ts` / glyph tables from a single source of truth).

### 4.2 Web TypeScript type-checking fails and has no package script
- **Location:** `web/package.json`, `web/tsconfig.json`
- **Problem:** `bunx tsc --noEmit` in `web/` fails with 11 errors: 2 for missing `@types/node` in `src/logic.test.ts`, 4 for missing `@octanejs/shadcn/base-ui/*` declarations in `src/ui/`, and 5 inside `node_modules` (`@octanejs/lucide` importing untyped `@lucide/icons/icons/*`, `@octanejs/markdown` importing `./markdown-hooks.tsrx`), and `web/package.json` has no `typecheck` or `lint` script.

### 4.3 Silent attachment drops and unhandled promise rejections in web client
- **Location:** `web/src/store.ts:256-269`, `web/src/ui/App.tsx:52`, `web/src/ui/Composer.tsx:153, 176`
- **Problem:** `addPics()` silently skips unsupported MIME types or files over 50 MB with no user toast via `note(...)`, and callers invoke `void store.addPics(...)` without catching `FileReader` rejections.

### 4.4 Minor cleanup items
- **Workspace dependency version skew:** `core/Cargo.toml` depends on `sha2 = "0.11"` while `host/Cargo.toml` depends on `sha2 = "0.10"`, pulling both trees into `Cargo.lock`.
- **Redundant `select!` in `host/src/runner.rs:193-196`:** Both arms simply call `child.wait()`.
  - **Status:** Fixed in #6.
- **Dead calculation in `core/src/timers.rs:404-412`:** `Cron::next_ms` truncates `ms` to the minute and then takes `.max(exact)`, which always equals `exact`.
  - **Status:** Fixed in #6.
- **Monolithic `core/src/luau.rs` (2,512 lines):** ~1,080 lines of production code and ~1,430 lines of inline tests; splitting tool bindings and tests into submodules (`luau/mod.rs`, `luau/tools.rs`, `luau/tests.rs`) would improve navigability.
