# Acorn

A native macOS app for one chat with an AI agent that never ends: every message is kept and summarized into a memory tree the agent can zoom back into, so the chat outgrows the model's context window without losing anything.

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
| **Agent tools** | Shell, read, write and edit on your Mac, in a work dir of the agent's own; subagents in parallel; computer use (screen, mouse, keyboard) when you allow it |
| **Assistants** | Acorn (general assistant), Max (thoughtful expert) and those they hire on request, each a person with a name and an about, its own chat, memory and work dir, in a sidebar |
| **Cut in** | A message sent while an assistant talks or works stops it at once, as a person would stop and listen; it answers you, then goes on only if you say so |
| **Consult** | Assistants ask each other (`list_assistants`, `search_assistants`, `ask_assistant`), each pair in a thread of its own that never ends |
| **Facts** | `remember` and `forget` keep facts you state, for every assistant or one; each call's system prompt lists them |
| **Purge** | `purge` erases a message (a pasted key or password) and every summary made from it, then wipes it from the database file |
| **Import** | Your past Claude Code and Codex sessions on this Mac, and Claude and ChatGPT chats from their websites' exports, read into the chat |
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

The model is told to find the latest mention of anything it needs in the view and zoom until it has it whole, before it acts, guesses or asks.

## Install

Acorn builds from source and runs on macOS only (`main.rs` stops with a compile error elsewhere). It needs a Rust toolchain with the 2024 edition and a DeepSeek API key.


```bash
git clone https://github.com/coderotator/acorn
cd acorn
cargo build --release -p acorn
```

<!-- TODO: an installable .app bundle; `nsh package` exists, but its flags for Acorn are not documented here yet. -->

## Quick start

1. Give Acorn your DeepSeek key, either in the environment or in `~/.acorn/config.json` (an app opened from the Finder has no shell variables):

   ```json
   { "deepseek_api_key": "sk-..." }
   ```

2. Run it:

   ```bash
   cargo run --release -p acorn
   ```

3. Type a message. Acorn answers in its chat; its tools run in `~/.acorn/work/main`.

## Using it

You act by words alone: there are no menus and no stop button. Clicks and keys only look.

| Input | Effect |
|---|---|
| Message box | Talk to the assistant you picked; images can be attached, files dropped on the window add their paths. Sent while it works, a message cuts in |
| Sidebar | Pick an assistant (who it is shows beside its name) |
| A name in a report | Open that agent's work, or the thread where that assistant answered |
| Esc | Back to your chat from an agent's page, a thread or the overview |
| Ctrl+Tab | Next tab (the chat, each agent at work) |
| Cmd+Shift+F | The overview: the chat in the fewest blocks, and back |
| Cmd+B | Hide or show the sidebar |

Ask for what you want in plain words: "stop", "stop Scout", "keep going until the build is green", "what did I spend this week?", "bring in my Claude Code chats since June", "export your memory", "show me your memory tree", "remember that I deploy with fly", "forget fact #12", "purge the message with my token", "ask Max what he thinks", "hire a personal stylist called Marrie".

### Tools the agent has

| Tool | Who | What |
|---|---|---|
| `zoom`, `date` | everyone | Open a line of the view; a message's date |
| `shell`, `read`, `write`, `edit` | assistants, subagents | Run on this Mac in the caller's own dir under the work dir |
| `spawn`, `tell`, `stop` | assistants | Start subagents in parallel; message one at work (it cuts in); stop one or all |
| `computer` | assistants | Start a computer-use task (needs `computer` set to 1) |
| `create_assistant` | with you | Hire an assistant: name, about, effort, instructions |
| `list_assistants`, `search_assistants`, `ask_assistant` | with you | Find another assistant and ask it, in your two's thread; its reply comes back as a message |
| `remember`, `forget`, `purge` | with you | Keep and drop facts; erase a message |
| `goal`, `usage`, `import`, `export`, `tree` | with you | Set or drop the goal; read costs; import past chats; save the memory as JSON; open the tree as a page |

An assistant answering another in a thread has its work tools only: it can't consult further, keep facts or change the user's settings.

## Configuration

Settings come from `$ACORN_DIR/config.json` (default `~/.acorn/config.json`). Any key can instead be set as `ACORN_<KEY>` in the environment, which wins. Paths may start with `~/`.

| Key | Default | Meaning |
|---|---|---|
| `deepseek_api_key` | none | The API key; the `DEEPSEEK_API_KEY` variable wins. No program the agent runs inherits it |
| `chat` | `default` | Acorn's own chat id (letters, digits, `_`, `-`) |
| `chats` | `~/.chats` | Where the chats live, a dir each by id |
| `work` | `$ACORN_DIR/work` | Where the agents' tools work |
| `pick` | `high` | Acorn's effort: `none`, `low`, `high`, `max`, or `deepseek-flash:<effort>` (Max's is max; a hired one's is set when hired) |
| `compactor` | `low` | The effort of every summary call |
| `agents` | their assistant's | The effort of subagents |
| `view` | `2000000` | Bytes past which a view merges to half; capped at 2 MB |
| `computer` | off | `1` lets computer tasks use the screen, mouse and keyboard |
| `rules` | none | A file appended to every system prompt: who you are, how you want work done |
| `after` | none | A shell command run after each turn |

Computer use compiles a small Swift helper into `$ACORN_DIR` on first use; macOS then asks for the Screen Recording and Accessibility permissions.

## Files

| Path | Holds |
|---|---|
| `~/.acorn/config.json` | The settings above |
| `~/.acorn/assistants.json` | The assistants created, oldest first |
| `~/.acorn/facts.db` | Facts, shared by all assistants |
| `~/.acorn/work/` | The agents' work dirs: `main` for Acorn, `assistants/<id>` for others |
| `~/.chats/<id>/chat.db` | A chat: messages, agent logs, the summary tree, the view and usage (SQLite, WAL) |
| `~/.chats/<id>/docs/images/` | The chat's images, by hash |
| `~/.chats/<id>/chat.db.wipe` | Present while a purge's wipe is owed |
| `~/.chats/<id>/with/<asker>/` | The thread where assistant `<asker>` asks `<id>`, kept as a chat is |

`export` writes the whole memory as JSON to `~/Downloads/acorn-memory-<date-time>.json`; `tree` writes the tree as an HTML page to the temp dir and opens it.

## Privacy

- The chat and its summaries are sent to DeepSeek on every call, as is any fact an assistant sees.
- `purge` erases a message at once from everything a call or the app can see. Its old bytes stay in `chat.db`'s pages until a wipe rewrites the file, which runs once the chat is idle (no turn, agent or summary at work) and is retried at the next launch if it fails or Acorn quits first.
- Summaries written while a purged message's summary was in the compactor's context may still echo it.
- Copies the disk itself keeps, such as APFS snapshots and Time Machine backups, are beyond Acorn's reach; FileVault protects them.

## Limitations

- macOS only; no CLI and no web interface.
- One model: DeepSeek Flash, at four efforts.
- Chats kept by UniiChat (JSONL day files) are not read; Acorn starts them anew.
- The view loses detail with age by design: an old line covers many messages, and what its summary left out is found only by zooming.
- Facts beyond 16 KB are left out of the system prompt, oldest first.
- A wipe holds up writes to its chat while it runs (about 2 s for a 450 MB `chat.db`).

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets
```

The workspace has two crates: `core` (`acorn-core`: the memory tree, compactor, DeepSeek loop, tools, import, facts) and `app` (`acorn`: the window, on nsh). One test in `app/tests/live.rs` calls the real API and is ignored by default.

## License

Apache-2.0. See [LICENSE](../LICENSE).
