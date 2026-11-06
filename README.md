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
| **Goals** | Name a finish line ("keep at it until the tests pass") and the assistant works until a check says it is met; the goal shows above the message box |

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
| Web | `cd web && npm ci && npm run build` | Octane, Base UI and shadcn (Tailwind 4), built by Vite | `acorn-server`, on macOS or Linux |

Windows is not a target: `main.rs` stops with a compile error there.

## Install

Acorn builds from source. Every target needs a Rust toolchain with the 2024 edition and a DeepSeek API key.

- **macOS:** nothing more.
- **Linux:** GTK 4.14 or later and clang to build (`libgtk-4-dev clang` on Debian and Ubuntu 24.04+, `gtk4 clang` on Arch, `gtk4-devel clang` on Fedora); `xdg-open` for `tree`.
- **Web:** Node 22 or later and npm, for the page (a few seconds to build).

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
(cd web && npm ci && npm run build)               # the page into web/dist
cp .env.example .env                             # then put your DeepSeek key in it
cargo run --release -p acorn-server
```

Open the link it prints (`http://127.0.0.1:8484/?token=...`): the token becomes a cookie, and every request needs it, since the agent runs commands there. The token is kept in `$ACORN_DIR/server-token`. The server reads `.env` from where it starts (gitignored), without overriding what the environment already sets. `--addr HOST:PORT` (or `ACORN_ADDR`) listens elsewhere; on anything but localhost, put it behind HTTPS. `--web DIR` (or `ACORN_WEB`) serves another build.

The server and the desktop app both hear every chat, so don't run both on the same `$ACORN_DIR` at once.

To work on the page, build it once (the server wants `web/dist`), run the server, open its link once (the cookie is the host's, whatever the port), then `cd web && npm run dev`: Vite serves the page with hot reload on `http://127.0.0.1:5173` and sends `/api` on to the server (`ACORN_ADDR` if it listens elsewhere). Open the dev page on the same host name the server's link used (`127.0.0.1`, not `localhost`), or the cookie won't go with it. `npm test` runs the page's own tests.

## Using it

You act by words alone: there are no menus and no stop button. Clicks and keys only look.

| Input | Effect |
|---|---|
| Message box | Talk to the assistant you picked; images can be attached, files dropped on the window add their paths (in a browser, images dropped are attached). Sent while it works, a message cuts in |
| Sidebar | Pick an assistant (who it is shows beside its name) |
| A name in a report | Open that agent's work, or the thread where that assistant answered |
| Esc | Back to your chat from an agent's page, a thread or the overview |
| Ctrl+Tab | Next tab (the chat, each agent at work); most browsers keep this key for their own tabs, so click a tab there |
| Cmd+Shift+F (Ctrl+Shift+F off macOS) | The overview: the chat in the fewest blocks, and back |
| Cmd+B (Ctrl+B off macOS) | Hide or show the sidebar |

Ask for what you want in plain words: "stop", "fire Scout", "have three agents check each crate", "keep going until the build is green", "what did I spend this week?", "bring in my Claude Code chats since June", "export your memory", "show me your memory tree", "remember that I deploy with fly", "forget fact #12", "what can you do?", "learn how I like meeting notes", "purge the message with my token", "ask Max what he thinks" (or `@Max what do you think?`; `@Max` alone opens Max's chat), "make me a personal stylist called Marrie".

### Tools the agent has

| Tool | Who | What |
|---|---|---|
| `zoom`, `date`, `search` | everyone | Open a line of the view; a message's date; find the messages whose words match (full text, newest first) |
| `list_skills`, `load_skill` | everyone | The user's skills (how-tos for kinds of task), and one's steps |
| `abilities` | everyone | What the caller is and has: who it is, its tools, luau's globals and the plugin giving each, broken plugins, skills, and where Acorn's dir and source are |
| `luau` | everyone | Runs Luau code in the caller's own VM (globals kept between calls), with `read`, `write`, `edit`, `sh`, `web_search`, `web_fetch` and `json`, on the engines' machine in the caller's own dir under the work dir (write and edit need a read first; 120 s of Luau time a call, sh up to 600 s) |
| `delegate`, `tell`, `fire` | assistants | Give agents one task each, in parallel, in the background; message one at work (it cuts in); fire one or all |
| `create_assistant`, `archive_assistant`, `restore_assistant` | with you | Make an assistant (name, about, effort, instructions, welcome); take one off the sidebar, its files kept; bring it back |
| `list_assistants`, `ask_assistant` | with you | List the other assistants (or those a query matches) and ask one, in your two's thread; its reply comes back as a message |
| `extend` | with you | Install, replace or remove a skill or a plugin; a plugin is load-checked with all the others first and refused if it fails (one only missing its key is installed); what it replaces goes to the attic |
| `remember`, `forget`, `purge`, `unpurge` | with you | Keep and drop facts; erase a message, or take that back within ten minutes |
| `goal`, `usage`, `import`, `export`, `tree` | with you | Set or drop the goal; read costs, and each day's zooms and searches that found something or not; import past chats; save the memory as JSON; open the tree as a page |

An assistant answering another in a thread has its work tools only: it can't consult further, keep facts or change the user's settings.

### Plugins

A plugin adds a global to `luau`, or replaces one, without restarting Acorn. Each is a Luau file in `~/.acorn/plugins`, and it counts from the caller's next `luau` call: edit or remove it and that counts too, and the agent's own globals stay. Ask Acorn for one in plain words ("use Exa for search", "learn to roll dice"): it writes the plugin and installs it with `extend`, which loads it beside the others first and refuses one that fails, so a broken plugin never replaces a working one. Or copy one from [plugins](plugins):

| Plugin | Replaces | Key |
|---|---|---|
| [exa.luau](plugins/exa.luau) | `web_search`, `web_fetch` | `exa_api_key` in `config.json`, or `EXA_API_KEY` |
| [tinyfish.luau](plugins/tinyfish.luau) | `web_search`, `web_fetch` | `tinyfish_api_key` in `config.json`, or `TINYFISH_API_KEY` |
| [github.luau](plugins/github.luau) | adds `github`, GitHub's REST API | none with [`gh`](https://cli.github.com) signed in; else `github_token` in `config.json`, or `GITHUB_TOKEN` (public reads need none) |

Install one search provider at a time: with both, the last by name wins, and each call says so. A file returns its globals, each with a one-line doc that goes in the tool's description:

```lua
local key = secret("EXA_API_KEY")
local function search(query)
  return http{ url = "https://api.exa.ai/search", headers = { ["x-api-key"] = key }, json = { query = query } }.json
end
return { web_search = { doc = "web_search(query: string): table -- via Exa", value = search } }
```

A plugin has what `luau`'s code has, plus `http` and `secret`, which that code does not. One that fails to load is left out, whatever it would have replaced stays, and every call reports it until it is fixed. Plugins run as the agent does, so read one from elsewhere before installing it. A key kept in `config.json` stays out of the chat, but it doesn't stop an agent that means to read it.

## Configuration

Settings come from `$ACORN_DIR/config.json` (default `~/.acorn/config.json`). Any key can instead be set as `ACORN_<KEY>` in the environment, which wins. Paths may start with `~/`.

| Key | Default | Meaning |
|---|---|---|
| `deepseek_api_key` | none | The API key; the `DEEPSEEK_API_KEY` variable wins. No program the agent runs inherits it |
| `chat` | `default` | Acorn's own chat id (letters, digits, `_`, `-`) |
| `chats` | `$ACORN_DIR/chats` | Where the chats live, a dir each by id |
| `work` | `$ACORN_DIR/work` | Where the agents' tools work |
| `pick` | `high` | Acorn's effort: `none`, `low`, `high`, `max`, or `deepseek-flash:<effort>` (Max's is max; a created one's is set when created) |
| `compactor` | `low` | The effort of every summary call |
| `agents` | their assistant's | The effort of agents |
| `view` | `2000000` | Bytes past which a view merges to half; capped at 2 MB |
| `budget` | none | USD a day (here) that every chat's calls may spend together; once reached, no request goes till midnight: a turn says why, summaries wait. `0`: none |
| `rules` | none | A file appended to every system prompt: who you are, how you want work done |
| `after` | none | A shell command run after each turn |

## Files

| Path | Holds |
|---|---|
| `~/.acorn/config.json` | The settings above |
| `~/.acorn/assistants.json` | The assistants created, oldest first |
| `~/.acorn/facts.db` | Facts, shared by all assistants |
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
- A search or fetch goes to DeepSeek, or to the service of a plugin that replaces it, as does whatever else a plugin sends.
- `purge` erases a message at once from everything a call or the app can see. It is held in `chat.db` for ten minutes, so `unpurge` can take it back; its text and old bytes stay in the file until a wipe deletes it and rewrites the file, which runs once the chat is idle (no turn, agent or summary at work) and no purge is still in its ten minutes, and is retried at the next launch if it fails or Acorn quits first.
- Summaries written while a purged message's summary was in the compactor's context may still echo it.
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

The workspace has three crates: `core` (`acorn-core`: the memory tree, compactor, DeepSeek loop, tools, import, facts), `app` (`acorn`: the window, on nsh, and the macOS and Linux binary), and `server` (`acorn-server`: the engines and API for the web window); the web window itself is the npm package in `web/`. One test in `app/tests/live.rs` calls the real API and is ignored by default.

## License

Apache-2.0. See [LICENSE](LICENSE).
