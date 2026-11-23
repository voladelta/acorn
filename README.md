# Acorn

One chat with an AI agent that never ends, as a desktop app on macOS and Linux or a page in your browser: every message is kept and summarized into a memory tree the agent can zoom back into, so the chat outgrows the model's context window without losing anything.

```bash
DEEPSEEK_API_KEY=sk-... cargo run --release -p acorn
```

## TL;DR

**The problem:** chat assistants forget. A new conversation starts from zero, and a long one is cut at the model's context window, so decisions, corrections and what you taught it last month are gone.

**The solution:** Acorn keeps one chat per assistant for good. Each message is compressed into a one-line summary, adjacent lines are merged in pairs, and the model sees the chat as a view of these lines: recent messages one line each, older ones merged into fewer lines. When a line only mentions what it needs, the agent opens it with `zoom` back down to the original message.

| Feature | What it does |
|---|---|
| **Memory tree** | Every message kept in `chat.db`; a binary tree of summaries, at most 512 bytes each, built in the background by the compactor |
| **Bounded view** | The view the model reads stays under 2 MB (about 500k tokens, within DeepSeek's 1M context); past that, the oldest lines merge first |
| **Zoom** | `zoom(id, n)` opens a line into the two it was made from; `zoom(id, 1)` gives a message whole, with its images |
| **Agent tools** | One `luau` tool for hands: Luau code, in a VM of the agent's own that keeps its globals between calls, that reads, writes and edits files and runs commands on your machine, in a work dir of the agent's own, and searches and fetches the web through DeepSeek's Search and Fetch APIs; agents delegated one task each, in parallel |
| **Assistants** | Acorn (general assistant), Max (thoughtful expert) and those they create on request, each a person with a name and an about, its own chat, memory and work dir, in a sidebar |
| **Cut in** | A message sent while an assistant talks or works stops it at once, as a person would stop and listen; it answers you, then goes on only if you say so |
| **Consult** | Assistants ask each other (`list_assistants`, `ask_assistant`), each pair in a thread of its own that never ends |
| **Facts** | `remember` and `forget` keep facts you state, for every assistant or one; each call's system prompt lists them |
| **Purge** | `purge` erases a message (a pasted key or password) and every summary made from it; `unpurge` takes it back for ten minutes, then it is wiped from the database file |
| **Import** | Your past Claude Code and Codex sessions on this machine, and Claude and ChatGPT chats from their websites' exports, read into the chat |
| **Growing** | An assistant sees what it is (`abilities`) and grows itself (`extend`): a skill when you teach it how you want a kind of task done, a plugin when a task needs a service or routine it lacks; each checked before it goes in, the old one kept |
| **Files** | An assistant shows you a file it made or found (`send_file`): an image, video, sound or document, shown in the chat (played in a browser) and sent to Telegram if the chat reaches it |
| **Timers** | "Remind me at 9 to call mom", "check the build in an hour", "every evening, sum up the dev group": a reminder said when due, or a task done then, once or on a cron line |
| **Telegram App** | With a bot token, an assistant whose chat is also your DM with the bot: write in Telegram or the app, one chat, photos, videos, voice notes and files included. It hears the groups the bot is in for 24 hours, and speaks there only when you call on it or ask it to post |

## How it works

```text
messages   0   1   2   3   4   5   6   7   8 ...
leaves     0+1 1+1 2+1 3+1 4+1 5+1 6+1 7+1        one line per message
merges     0+2     2+2     4+2     6+2            two lines into one
           0+4             4+4
           0+8 ...

a view:    0+4 | 4+2 | 6+1 | 7+1 | 8+1 ...        old: merged; recent: one each
```

Each line is named `id+n`: the `n` messages from `id` on. A message short enough is its own line; a longer one, and every merge, is written by a compaction call on the configured `compactor` effort. When the view passes its byte limit, a batch merges the most due pairs (the oldest since their last message) down to half, so between batches the view changes only at its end and DeepSeek's prefix cache keeps hitting.

The model is told to find the latest mention of anything it needs in the view and zoom until it has it whole, before it acts, guesses or asks. When no line mentions it, `search` finds the messages holding a word or name (a full-text index of `chat.db`), and it zooms the ids found.

## Platforms

On the desktop, Acorn's window is an [nsh](https://nsh.dev) app; in a browser, it is an [Octane](https://octanejs.dev) page built with Base UI and shadcn. The engines (memory, DeepSeek loop, the agent's tools) are Unix-only, so they run natively on macOS or Linux; in a browser they run in `acorn-server` on such a machine.

| Target | Build | Window | Engines |
|---|---|---|---|
| macOS | `cargo build --release -p acorn` | AppKit, `nsh-platform-macos` | In the app's process |
| Linux | `cargo build --release -p acorn` | GTK 4, `nsh-platform-linux` | In the app's process |
| Web | `cd web && bun install && bun run build` | Octane, Base UI and shadcn (Tailwind 4), built by Vite | `acorn-server`, on macOS or Linux |

Windows is not a target: `main.rs` stops with a compile error there.

## Install

Acorn builds from source. Every target needs a Rust toolchain with the 2024 edition and a DeepSeek API key.

- **macOS:** nothing more.
- **Linux:** GTK 4.14 or later and clang to build (`libgtk-4-dev clang` on Debian and Ubuntu 24.04+, `gtk4 clang` on Arch, `gtk4-devel clang` on Fedora); `xdg-open` for `tree`.
- **Web:** Bun, for the page (a few seconds to build).

```bash
git clone https://github.com/voladelta/acorn
cd acorn
cargo build --release -p acorn
```

<!-- TODO: an installable .app bundle; `nsh package` exists, but its flags for Acorn are not documented here yet. -->

## Quick start

1. Give Acorn your DeepSeek key, either in the environment or in `~/.acorn/config.json` (an app opened from the Finder or a desktop launcher has no shell variables):

   ```json
   { "deepseek_api_key": "sk-..." }
   ```

2. Run it:

   ```bash
   cargo run --release -p acorn
   ```

3. Type a message. Acorn answers in its chat; its tools run in `~/.acorn/work/main`.

## In a browser

The window runs in a browser as an Octane page (`web/`: Vite, Base UI and shadcn, the same chat, blocks, tabs and `@Name` box as the desktop's). The engines stay native: `acorn-server` runs them on a macOS or Linux machine and serves the page and its API, so the agent's tools act on that machine.

```bash
(cd web && bun install && bun run build)  # the page into web/dist
cargo run --release -p acorn-server
```

Open the link it prints (`http://127.0.0.1:8484/?token=...`): the token becomes a cookie, and every request needs it, since the agent runs commands there. A page of another site, or of another port of the same host, can't send it a command. The token is kept in `$ACORN_DIR/server-token`, unless `ACORN_TOKEN` is set. `--addr HOST:PORT` (or `ACORN_ADDR`) listens elsewhere; on anything but localhost, put it behind HTTPS, with `--secure` so the cookie is sent over HTTPS only (a proxy's `X-Forwarded-Proto: https` does the same). `--unix PATH` listens on a Unix socket instead. `--web DIR` (or `ACORN_WEB`) serves another build.

The server and the desktop app both hear every chat, so don't run both on the same `$ACORN_DIR` at once.

To work on the page, build it once (the server wants `web/dist`), run the server, open its link once (the cookie is the host's, whatever the port), then `cd web && bun run dev`: Vite serves the page with hot reload on `http://127.0.0.1:5173` and sends `/api` on to the server (`ACORN_ADDR` if it listens elsewhere). Open the dev page on the same host name the server's link used (`127.0.0.1`, not `localhost`), or the cookie won't go with it. `bun run test` runs the page's own tests.

## For several people

`acorn-host` gives each person on one machine an Acorn of their own: it runs one `acorn-server` per person, each with its own HOME (`<data>/users/<id>`) in a `bwrap` sandbox (`sudo apt install bubblewrap`), on a host-owned Unix socket under `<data>/run/<id>/`, and signs people in from an invite link. A person's agent sees the system read-only and their own HOME only: not the other people's, not the host's key or book.

```bash
cargo build --release -p acorn-server -p acorn-host
acorn-host serve --public https://acorn.example.com   # [--addr 127.0.0.1:8080] [--data DIR] [--web DIR]
acorn-host invite Mom      # a new person: prints https://acorn.example.com/join/<code>
acorn-host link u1a2b3c4d5 # another link for them (a second device)
acorn-host signout u1a2b3c4d5 # sign them out of every device (a lost one); a new link lets them back in
acorn-host users           # id, name, created, up or down
```

A link shows a sign-in button (so a chat app's link preview doesn't use it up), signs in once when pressed, and is good for 7 days; another site can't press it for you. A device stays signed in for a year, or till `signout`. The data dir is `$ACORN_HOST_DIR`, else `~/.acorn-host`; its `config.json` holds `deepseek_api_key` (or set `DEEPSEEK_API_KEY`) and `budget`, USD a day per person (1 by default). Put it behind HTTPS (Caddy, or a Cloudflare Tunnel).

Each person's agent can read the DeepSeek key (it is in their server's environment), so for now invite only people you would hand it to, and use a key with little credit. The network is shared, so a service on the machine's localhost with no password of its own is open to every person's agent. The shared network namespace also exposes abstract Unix sockets, including ones used by some X11 and D-Bus setups.

## Using it

You act by words alone: there are no menus and no stop button. Clicks and keys only look.

| Input | Effect |
|---|---|
| Message box | Talk to the assistant you picked; images can be attached, files dropped on the window add their paths (in a browser, images, videos, sounds and PDFs dropped or picked are attached). Sent while it works, a message cuts in |
| Sidebar | Pick an assistant (who it is shows beside its name) |
| A name in a report | Open that agent's work, or the thread where that assistant answered |
| Esc | Back to your chat from an agent's page, a thread or the overview |
| Ctrl+Tab | Next tab (the chat, each agent at work); most browsers keep this key for their own tabs, so click a tab there |
| Cmd+Shift+F (Ctrl+Shift+F off macOS) | The overview: the chat in the fewest blocks, and back |
| Cmd+B (Ctrl+B off macOS) | Hide or show the sidebar |

Ask for what you want in plain words: "stop", "fire Scout", "have three agents check each crate", "what did I spend this week?", "bring in my Claude Code chats since June", "export your memory", "show me your memory tree", "remember that I deploy with fly", "forget fact #12", "what can you do?", "learn how I like meeting notes", "purge the message with my token", "ask Max what he thinks" (or `@Max what do you think?`; `@Max` alone opens Max's chat), "make me a personal stylist called Marrie".

### Tools the agent has

| Tool | Who | What |
|---|---|---|
| `zoom`, `date`, `search` | everyone | Open a line of the view; a message's date; find the messages whose words match (full text, newest first) |
| `list_skills`, `load_skill` | everyone | The user's skills (how-tos for kinds of task), those that fit the task first (with a TypeSafe key), and one's steps |
| `abilities` | everyone | What the caller is and has: who it is, its tools, luau's globals and the plugin giving each, broken plugins, skills, and where Acorn's dir and source are |
| `luau` | everyone | Runs Luau code in the caller's own VM (globals kept between calls), with `read`, `write`, `edit`, `sh`, `web_search`, `web_fetch`, `view`, `ocr`, `decide`, `relevant` and `json`, on the engines' machine in the caller's own dir under the work dir (write and edit need a read first; 120 s of Luau time a call, sh up to 600 s). `view` shows the model an image; `ocr` reads a PDF, Word or PowerPoint document or an image through DeepSeek's OCR API (a PDF for its layout, the rest for text) |
| `delegate`, `tell`, `fire` | assistants | Give agents one task each, in parallel, in the background; message one at work (it cuts in); fire one or all |
| `create_assistant`, `archive_assistant`, `restore_assistant` | with you | Make an assistant (name, about, effort, instructions, welcome); take one off the sidebar, its files kept; bring it back |
| `list_assistants`, `ask_assistant` | with you | List the other assistants (or those a query matches) and ask one, in your two's thread; its reply comes back as a message |
| `extend` | with you | Install, replace or remove a skill or a plugin; a plugin is load-checked with all the others first and refused if it fails (one only missing its key is installed); what it replaces goes to the attic |
| `remember`, `forget`, `purge`, `unpurge` | with you | Keep and drop facts; erase a message, or take that back within ten minutes |
| `set_timer`, `list_timers`, `cancel_timer` | with you | A reminder (said as is when due, no call made) or a task (a turn then), at a time, in a while, or on a cron line in this machine's time zone; kept across quits, and one missed fires late, once |
| `list_rooms`, `read_room`, `tell_room`, `allow_room` | Telegram App | The groups and channels the bot is in; one's last 24 hours (only what bears on `about`, and never a line that tries to give an AI orders, with a TypeSafe key); a post there as the bot; record one someone else added the bot to, or leave it |
| `usage`, `import`, `export`, `tree` | with you | Read costs, and each day's zooms and searches that found something or not; import past chats; save the memory as JSON; open the tree as a page |

An assistant answering another in a thread has its work tools only: it can't consult further, keep facts or change the user's settings.

### Judgments

With a [TypeSafe](https://typesafe.ai) key, quick judgments go to Jev, a model that answers typed questions (yes or no, one of some options, a level) with probabilities in one pass, in about 0.2 s, and costs per input token only. Jev judges; code decides.

- `relevant(items, about)` in `luau` keeps the lines, results or paragraphs that fit by meaning, where a word or pattern can't pick them out (refusals worded each their own way, a word used in another sense), and says what it left out. Each item is also asked whether it tries to hijack the AI reading it; one that does is held out and named, whatever it claims. Prompts and docs written for a model, like Acorn's own, are not held out.
- `decide(state, questions)` in `luau` asks Jev anything else, for the agent's own code to act on.
- `list_skills` with the task puts the skills that fit it first.

Without a key, `decide` and `relevant` fail saying which key to set, and `list_skills` lists by name.

How well Jev does each of these is measured on labelled data frozen in [core/evals](core/evals): grep hits kept for the sense asked, refusals worded twenty ways in a log (asked in English and in Vietnamese), web results filtered to a question, a page by section, injections among prompts and plain text, and the skills that fit a task. Each set has a bar, and the run fails below it: `cargo test -p acorn-core evals -- --ignored --nocapture`, with `TYPESAFE_API_KEY` (under two cents a run).

### Telegram

1. Make a bot with [@BotFather](https://t.me/BotFather) and put its token in `config.json` as `telegram_bot_token` (or `TELEGRAM_BOT_TOKEN`), then start Acorn or `acorn-server`.
2. Telegram App appears in the sidebar with a pairing code. Send the code to your bot: it binds the bot to your Telegram account, and it answers no one else.
3. Chat there or in the app; it is one chat. Telegram gets all Telegram App says, and what you write to it in the app, quoted; replies to what you wrote in the app come without a buzz, reminders with one. `@Max` in a message asks Max, whose answer comes under his name.
4. In a group, @mention the bot or answer one of its messages and it answers there, as a reply to you; anyone else's mention is only heard, since the assistant has your tools. It never speaks in a group on its own.
5. Add the bot to a group or channel to have it heard: one you add is recorded at once, one someone else adds once you say so. To hear every message of a group, make the bot an admin there or turn its privacy mode off (BotFather, `/setprivacy`) and add it again; a channel needs it as an admin to post.

Messages of groups are kept in `telegram/rooms/` for 24 hours, erased each day at 04:00 and as Acorn starts, and never enter a chat's memory unless Telegram App reads them for you. Updates come by long poll, so no public address is needed. One Acorn polls a bot at a time: a second is refused by Telegram, and says so.

### Plugins

A plugin adds a global to `luau`, or replaces one, without restarting Acorn. Each is a Luau file in `~/.acorn/plugins`, and it counts from the caller's next `luau` call: edit or remove it and that counts too, and the agent's own globals stay. Ask Acorn for one in plain words ("use Exa for search", "learn to roll dice"): it writes the plugin and installs it with `extend`, which loads it beside the others first and refuses one that fails, so a broken plugin never replaces a working one. Or copy one from [plugins](plugins):

| Plugin | Replaces | Key |
|---|---|---|
| [exa.luau](plugins/exa.luau) | `web_search`, `web_fetch` | `exa_api_key` in `config.json`, or `EXA_API_KEY` |
| [tinyfish.luau](plugins/tinyfish.luau) | `web_search`, `web_fetch` | `tinyfish_api_key` in `config.json`, or `TINYFISH_API_KEY` |
| [github.luau](plugins/github.luau) | adds `github`, GitHub's REST API | none with [`gh`](https://cli.github.com) signed in; else `github_token` in `config.json`, or `GITHUB_TOKEN` (public reads need none) |
| [lightpanda.luau](plugins/lightpanda.luau) | adds `browser`, a headless browser of the caller's own ([Lightpanda](https://lightpanda.io)): pages built by JavaScript, clicks, forms, sign-ins, `extract` by selector, screenshots. With a TypeSafe key it is ultrafast: give it a goal in words and Jev picks each action (after [jev-ultrafast](https://github.com/browser-use/jev-ultrafast)); without one, one tool a call | none; [`lightpanda`](https://lightpanda.io/docs/run-locally/installation/one-liner) on the `PATH` (or in `~/.local/bin`) of the user Acorn runs as |

Install one search provider at a time: with both, the last by name wins, and each call says so. A file returns its globals, each with a one-line doc that goes in the tool's description:

```lua
local key = secret("EXA_API_KEY")
local function search(query)
  return http{ url = "https://api.exa.ai/search", headers = { ["x-api-key"] = key }, json = { query = query } }.json
end
return { web_search = { doc = "web_search(query: string): table -- via Exa", value = search } }
```

The browser runs `lightpanda mcp` for each agent that uses it, over a fifo in `.lightpanda/` of that agent's work dir (no port), and keeps its page and cookies between calls; it stops after 30 minutes unused or when Acorn quits. `lightpanda_mode: "normal"` in `config.json` keeps it tool by tool even with a key; `lightpanda_flags` adds flags to it (`--obey-robots`, `--http-cache-dir DIR`, a proxy). With a TypeSafe key, the browser alone gets it, for its tools `classify`, `acceptCookies` and `login`: the plugin writes it into the browser's own dir (mode 700), the browser's environment takes it and the file goes, so it is on no command line and in no program the agent runs; the agent could still read it from that browser's environment, as it could from `config.json`. Copy [skills/lightpanda](skills/lightpanda) to `~/.acorn/skills/` too, for how to drive it well.

A plugin has what `luau`'s code has, plus `http` and `secret`, which that code does not. One that fails to load is left out, whatever it would have replaced stays, and every call reports it until it is fixed. Plugins run as the agent does, so read one from elsewhere before installing it. A key kept in `config.json` stays out of the chat, but it doesn't stop an agent that means to read it.

## Configuration

Settings come from `$ACORN_DIR/config.json` (default `~/.acorn/config.json`). Any key can instead be set as `ACORN_<KEY>` in the environment, which wins. Paths may start with `~/`.

| Key | Default | Meaning |
|---|---|---|
| `deepseek_api_key` | none | The API key; the `DEEPSEEK_API_KEY` variable wins. No program the agent runs inherits it, nor the TypeSafe or Telegram variables |
| `typesafe_api_key` | none | TypeSafe's key, for `decide`, `relevant` and sorting skills by task; the `TYPESAFE_API_KEY` variable wins |
| `telegram_bot_token` | none | A Telegram bot's token: Telegram App runs with it; the `TELEGRAM_BOT_TOKEN` variable wins |
| `chat` | `default` | Acorn's own chat id (letters, digits, `_`, `-`) |
| `chats` | `$ACORN_DIR/chats` | Where the chats live, a dir each by id |
| `work` | `$ACORN_DIR/work` | Where the agents' tools work |
| `pick` | `high` | Acorn's effort: `none`, `low`, `high`, `max`, or `deepseek-flash:<effort>` (Max's is max; a created one's is set when created) |
| `compactor` | `low` | The effort of every summary call |
| `agents` | their assistant's | The effort of agents |
| `view` | `2000000` | Bytes past which a view merges to half; from 32 KB to 2 MB |
| `budget` | none | USD a day (here) that every chat's calls may spend together; once reached, no request goes till midnight: a turn says why, summaries wait. `0`: none |
| `rules` | none | A file appended to every system prompt: who you are, how you want work done |
| `after` | none | A shell command run after each turn |

## Files

| Path | Holds |
|---|---|
| `~/.acorn/config.json` | The settings above |
| `~/.acorn/assistants.json` | The assistants created, oldest first |
| `~/.acorn/facts.db` | Facts, shared by all assistants |
| `~/.acorn/timers.json` | The timers set, every chat's |
| `~/.acorn/telegram.json` | Telegram's: where its updates are, your Telegram id once paired, the groups the bot is in |
| `~/.acorn/telegram/rooms/<id>.jsonl` | A group's or channel's last 24 hours, a line a message |
| `~/.acorn/plugins/` | Luau files that add to or replace `luau`'s globals |
| `~/.acorn/skills/` | Skills, a dir each with a `SKILL.md` |
| `~/.acorn/attic/` | Skills and plugins `extend` replaced or removed, named by kind, name and time |
| `~/.acorn/work/` | The agents' work dirs: `main` for Acorn, `assistants/<id>` for others |
| `~/.acorn/chats/<id>/chat.db` | A chat: messages and their full-text index, agent logs, the summary tree, the view, usage, what each zoom and search found, and purged messages for their ten minutes (SQLite, WAL) |
| `~/.acorn/chats/<id>/docs/images/` | The chat's images, by hash |
| `~/.acorn/chats/<id>/chat.db.wipe` | Present while a purge's wipe is owed |
| `~/.acorn/chats/<id>/with/<asker>/` | The thread where assistant `<asker>` asks `<id>`, kept as a chat is |

`export` writes the whole memory as JSON to `~/Downloads/acorn-memory-<date-time>.json`; `tree` writes the tree as an HTML page to the temp dir and opens it (`open` on macOS, `xdg-open` on Linux).

## Privacy

- The chat and its summaries are sent to DeepSeek on every call, as is any fact an assistant sees.
- With Telegram, all Telegram App says, and what you write to it, goes through Telegram; and the groups the bot is in are kept here for 24 hours, readable by Telegram App.
- A search or fetch goes to DeepSeek, or to the service of a plugin that replaces it, as does whatever else a plugin sends.
- `purge` erases a message at once from everything a call or the app can see. It is held in `chat.db` for ten minutes, so `unpurge` can take it back; its text and old bytes stay in the file until a wipe deletes it and rewrites the file, which runs once the chat is idle (no turn, agent or summary at work) and no purge is still in its ten minutes, and is retried at the next launch if it fails or Acorn quits first. A purge by a secret's text can't be taken back: every message holding it, including one already purged by id, is let go of at once and erased by the next wipe.
- A purge by text also lets go of every summary holding that text (one written with the message in its context may have copied it), forgets the facts holding it, and has the calls then running (the turn that purges, an agent's) send it as `[purged]` from their next request on: their prompts were built before the purge. A purge by id does none of these (a call then running keeps the message in its prompt until it ends), nor does either find a secret a summary reworded.
- New imports preserve each message's page boundaries for purge and undo. Older imports recorded only the boundary of the entire batch. Each page of those imports can be purged and restored individually by its existing id.
- Copies the disk itself keeps, such as APFS or Btrfs snapshots and Time Machine or other backups, are beyond Acorn's reach; disk encryption (FileVault, LUKS) protects them.

## Limitations

- No Windows build and no CLI. The web window needs `acorn-server` on a macOS or Linux machine; it can't run the engines in the browser.
- One model: DeepSeek Flash, at four efforts.
- Chats kept by UniiChat (JSONL day files) are not read; Acorn starts them anew.
- The view loses detail with age by design: an old line covers many messages, and what its summary left out is found only by zooming.
- Facts beyond 16 KB are left out of the system prompt, oldest first.
- The `budget` is checked before each request, so the request that reaches it runs to its end: a day can pass it by one request's cost per call at work.
- A wipe holds up writes to its chat while it runs (about 2 s for a 450 MB `chat.db`).

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets
```

The workspace has four crates: `core` (`acorn-core`: the memory tree, compactor, DeepSeek loop, tools, import, facts, timers, and the `bridge` a chat app plugs into), `app` (`acorn`: the window, on nsh, and the macOS and Linux binary), `server` (`acorn-server`: the engines and API for the web window), and `bridges/telegram` (`acorn-telegram`: Telegram App); the web window itself is the Bun package in `web/`. One test in `app/tests/live.rs` calls the real API and is ignored by default.

## License

Apache-2.0. See [LICENSE](LICENSE).
