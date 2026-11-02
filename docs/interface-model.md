# Interface model: Acorn (the app)

Status: built · Last updated: 2026-11-02

**As built, where it differs from the text below:**
- Consult goes one level deep: an assistant answering in a thread has its work tools only (no `ask_assistant`, facts, goal, import or delegating), so asks can't loop.
- `usage` sums every chat and thread; the footer's cost is the shown chat's today.
- No `/goal` alias: slash words are gone. Dropping files on the window adds their paths to the message being written ("import this").
- Esc goes back to your chat from an agent's page, a thread or the overview.
- An assistant made before abouts shows its name alone.
- `@Name` is the one word the box reads itself: alone, it opens your chat with that assistant (no call); in a message, the words go to the one you talk to, whose prompt reads `@Name` as "ask them". The box suggests names as you type `@`: ↑ and ↓ pick one, Return or Tab takes it (Return sends a word already a name), a click takes the one clicked, Esc closes the names for that word.
- An assistant answering in a thread hears nothing of what only your chat has: no facts, purges, goals, asking or `@Name`.
- An assistant remembers its threads in its chat with you: each turn sees each thread's latest 8 lines ("Your threads", after the agents at work), and `zoom("Acorn")` gives one whole. Talk to Max the day after he warned Acorn, and he knows.
- "Hire" a named person who should stay ("hire a stylist called Marrie") makes an assistant; agents get tasks by `delegate`, so the user's word and the tool never collide.
- One `import` tool finds, starts, pauses and resumes an import (its `do`).
- An assistant made can be archived (off the sidebar, its files kept) and restored, by words: `archive_assistant`, `restore_assistant`.
- When no block mentions what it needs, an assistant searches the messages' words (`search`) and opens the hits; how often zooms and searches find something is part of `usage`.
- Purge needs no confirmation: it is held ten minutes, and `unpurge` takes it back.

## The principle

**The chat is the command line. Look by hand, act by word.**

You and each assistant share one conversation that never ends, as you would with people who work for you. You don't press a button on Alfred to make him stop: you say "stop", or you cut in mid-sentence. So:

- **Every act is said.** Typing now, voice later. Whatever the app can do, you can ask for in plain words, and the assistant does it through a tool. No act needs a button, a menu, a slash command or a message number.
- **Hands only look.** Clicks and keys move your eyes: switch assistant, open a block, look at an agent's work or at two assistants' talk, open an image, hide the sidebar. Looking never changes anything.
- **The screen shows state, not controls.** Who someone is, what is going on (talking, agents at work, a goal, an import), today's cost: always visible, as facts.
- **Cutting in is a message.** A message sent while the assistant talks or works stops it at once; it reads you and answers. "Stop" and it stays stopped; "also check the tests" and it goes on with that.
- **People, not settings.** An assistant is introduced by who it is ("Personal stylist"), never by how it is tuned ("high effort"). How hard it thinks is part of its character, set when it is created, and not shown.

Voice comes for free: if everything is sayable and nothing needs pointing, a microphone is one more way to fill the box.

## Frame

- **Users and tasks:** one person on their own Mac, talking many times a day with a few long-lived assistants that remember everything, act on the Mac and consult each other.
- **Posture:** sovereign, conversational. The chat is the whole interface; the chrome reports, it doesn't ask.
- **The cast** (each: name · about; effort is set at creation and never shown):
  - **Acorn** · General assistant. The everyday one, there for anything: the butler. (High effort, not shown.)
  - **Max** · Thoughtful expert. Thinks long before answering; you, or Acorn, go to Max when it matters. (Max effort, not shown.)
  - **Created**, by asking any assistant: **Marrie** · Personal stylist. **Doug** · Friendly neighbour who loves cracking jokes.
- **Assumptions:** single user, native app; voice later must need no new verbs.

## How deepseek-harness does turns, and what Acorn takes from it

`~/Codehub/deepseek-harness/packages/core/agent-loop` gives every agent an inbox with two slots and three ways in (`agent.ts`, `inbox.ts`):

| Harness verb | Lands in | Effect |
|---|---|---|
| `followup(msg)` | next-turn | queued; starts a new turn once the current one ends |
| `steer(msg)` | next-step | joins the current turn after the step in flight (stream + its tool calls) finishes |
| `cancel(cause, { keepInbox })` | – | aborts the stream and running tools now; the stop button keeps the inbox, so queued input then wakes a fresh turn (`send`: "waking input cannot join an aborted activity, so it starts the next turn") |

On a cancel it keeps what was already streamed as an `assistant/message` with `interrupted: true` (`live.interruptedBlocks()`), and session repair marks cut tool calls `TOOL_NOT_STARTED` or `TOOL_OUTCOME_UNKNOWN`, so the model knows exactly where it was cut. Its web composer makes you choose Queue or Steer, and stopping is a separate button.

**Acorn today is steer-only.** A message mid-turn goes into the live call and is read after the step's stream and all its tools finish (`core/src/ai/agent.rs`, the `try_recv` loop). Stop is a separate command (`Command::Stop`, the ■ button).

**Acorn's barge-in = the harness's cancel(keepInbox) + followup, as one act, triggered by any message.** No Queue/Steer choice, no stop button. Concretely:

1. Your message lands; the step in flight is cancelled at once: the stream, and tool calls still running.
2. What was said so far is kept and shown, ending "— cut off". Each cut tool call is logged with its outcome: not started, or cut while running (output so far, if any).
3. A new turn starts with your message in view; the assistant answers you.
4. Agents are not cut. They are other people; you ask the assistant to stop or redirect them.

Taken from the harness as-is: the interrupted marker on the partial reply, and the not-started / outcome-unknown records for tool calls. Left out: steer (Acorn has no way for you to say "don't stop, just note this", and a person who is spoken to stops and listens), and the two-button composer.

## Audit: what breaks the principle today

Most structural first. Symptom → cause → fix → on screen.

1. **model: cutting in doesn't cut in.** Symptom: "stop, wrong folder" waits until the reply and its tools finish. Cause: steer-only (above). Fix: barge-in. On screen: the reply stops within a moment of Enter, ends "— cut off", your message under it.

2. **interaction: stopping is a button, a key and a menu item.** Symptom: ■ on the send button; empty Enter and Esc interrupt; "Stop all work" in ☰ (and it stops only the shown assistant). Fix: all three go. "Stop", "hold on", "wait" are messages. The send button is always ↑; empty Enter does nothing; Esc only closes what you opened to look at.

3. **model: assistants are introduced by their tuning.** Symptom: the header says "Acorn  high effort"; the sidebar tooltip says "Doug · max effort". Cause: `Assistant` has `name`, `instructions` (the system prompt, too long to show) and `effort`, but nothing that says who it is. Fix: add **`about`** to `Assistant`, a short line saying who it is ("Personal stylist"), required by `create_assistant`, written from what you said. Acorn's and Max's are built in. Effort stays in the model and the prompt, never in the UI. On screen: the header is "Marrie · Personal stylist"; the sidebar tooltip is the same line.

4. **model: assistants can't talk to each other.** Symptom: Acorn can't ask Max for a second opinion or Marrie what to wear; you copy between chats. Cause: a missing concept. Fix: **Consult** (below): `list_assistants`, `ask_assistant`, with a lasting chat per pair of assistants.

5. **model: the assistant can't stop its own agents.** Symptom: "stop Scout" has nowhere to go. Fix: a `fire` tool (an agent's name, or all of this assistant's), beside `tell`.

6. **model: commands live in a menu and slash words.** Symptom: Import, Usage, Export memory, Memory tree, Fold all, Stop in ☰; `/goal`, `/halt`; "the ☰ menu has the commands". Fix: each is said and done by a tool (table under *Acts*). The ☰ menu goes; `/goal` may stay a while as a typed alias.

7. **model: Goal is invisible.** Symptom: the assistant keeps taking turns and nothing says why. Fix: `goal` in `wire::State`, shown as a quiet line above the box. You drop it by saying so.

8. **model: an agent's page silences you.** Symptom: the box goes read-only there. Fix: the box always talks to the shown assistant; sending from an agent's page (or a consult thread) returns you to the chat, where the reply comes.

9. **model: "Acorn" names four things.** Symptom: in Doug's chat, "Acorn is an AI…", "Acorn's turn", "Acorn's memory". Cause: Acorn is the app, the first assistant, every assistant's speaker (`Kind::Acorn`) and every turn's owner (`TURN`). Fix: chat-level text names the assistant shown.

10. **model: effort is a dial nobody turns.** `Command::Pick` is never sent; the `pick` config key and the "Acorn's pick" idea outlive their use. Fix: effort is fixed per assistant at creation. Drop `Pick` (keep `pick` only as Acorn's default if wanted).

11. **model: the words on screen aren't the words you'd say.** "8x", "line", "compressing", "fold" vs "Fold all". Fix: Block ("8 messages"), Open / Collapse, Overview, Summarizing.

12. **interaction: usage and import are modal boxes.** Fix: they go with the menu; what remains is state (today's cost, import progress), with tooltips for detail.

13. **surface: nothing says what you can ask.** Fix: a new chat opens with the assistant saying who it is (its `about`) and three things it can do.

## Objects

| Object | Key attributes | Relationships | Home |
|---|---|---|---|
| Assistant | name, about, at work? (effort, instructions: hidden) | has a Chat with you; a Thread with each assistant it consults; own Facts | sidebar row (name, about in tooltip) |
| Chat | you ↔ one assistant; goal; today's cost | Messages, Blocks, Agents; one Import | main page |
| Thread | assistant ↔ assistant, lasting | Messages and Blocks, like a Chat | its own page, reached from the consult in either chat |
| Message | who, date, text, images | in a Chat or Thread | in it |
| Block | n messages, summary | two halves, down to Messages | in the Chat or Thread |
| Agent | name, task, at work / done, log | delegated one task by one Chat's assistant | its page (tab) |
| Fact | text, scope (everyone / one) | held by all assistants or one | said back on request |
| Goal | text | one per Chat | line above the box |
| Import | sessions, price, progress | into one Chat | progress in the header |

## Concepts

| Concept | Purpose | Operational principle |
|---|---|---|
| Conversation | one channel for every act | You say it; the assistant does it with a tool and tells you what it did. |
| Barge-in | interrupt like a person | Send while it talks or works: it stops at once, keeps what it said marked "cut off", reads you, answers. |
| Consult | assistants ask each other | Acorn asks Max; Max answers in their own lasting thread; Acorn tells you what Max said. |
| Memory tree | remember everything | Old messages merge into Blocks; open one to see what it was made of. |
| Agent | hands for one task | The assistant delegates one task to a freelancer; it works in the background, reports once and is gone. "Fire Scout" ends it early. |
| Goal | persistence | Name the finish line; it keeps taking turns until a check says it's met or you call it off. |
| Fact | knowledge across chats | "Remember X" and every assistant (or one) knows X until you say forget it. |
| Search | find what no summary mentions | The assistant searches the messages for a word or name, then opens the hits. |
| Purge | erase a secret | "Purge the message with my token": it and its summaries vanish; "unpurge it" brings it back within ten minutes, then it is wiped from disk. |
| Create assistant | more people | Ask for someone with a name and an about; they appear in the sidebar and stay. |
| Mention | name an assistant in the box | `@Max` alone opens your chat with Max; `@Max what do you think?` has the one you talk to ask Max. |

**Ask or delegate.** An assistant asks another assistant for judgment (a part, taste, expertise, memory of its own) and delegates labour to an agent (research, a long build, many files) that can run while it goes on. Assistants stay; agents are gone after one report. Agents delegate to no one and ask no one.

### Consult

**Purpose:** let any assistant draw on another's character, effort and memory, the way Alfred calls in Lucius Fox, without you carrying messages.

**Tools (every assistant has them, in its chat with you):**

| Tool | Input | Answer |
|---|---|---|
| `list_assistants` | query (optional) | every other assistant: name, about (not instructions or effort), then the archived; with a query, those whose name, about or instructions match, best first (e.g. "clothes" → Marrie) |
| `ask_assistant` | name, message | sends the message in the thread with that assistant; the answer comes back as a message "[Max] reply", like `tell`'s, so a slow thinker doesn't hold up the turn |

**Threads.** Each pair of assistants has one thread, kept like a chat: never ends, memory tree, summaries, purge. Max answering Acorn is Max in his thread with Acorn: his own instructions and effort, his tools, the shared Facts, and what Acorn and he said before. Your chat with Max keeps its own messages, but Max remembers the thread there: its latest lines show on each of his turns with you.

**Connections:**
- An assistant asked is busy in that thread, not in your chat with it: you can talk to Max while he answers Acorn. Its sidebar dot shows it at work either way.
- Barge-in in your chat with Acorn doesn't cut Max's answer in their thread; say "never mind asking Max" and Acorn tells him (barge-in on the thread).
- A consult can't loop: an assistant answering in a thread can consult a third, but not the one asking (and a depth limit holds).
- What the thread says is the assistants' business, but nothing is hidden from you: every consult shows in the asking chat as a call with a link to the thread.

**On screen:** in Acorn's chat, a call row "Asked Max: …" whose link opens the Acorn ↔ Max thread page (read-only to you, like an agent's page, caption "Acorn and Max"); then Max's reply as a framed message named "Max", like an agent's report.

## Acts: who does what, by what

| Act | Said to the assistant (tool) | By hand (looking only) |
|---|---|---|
| Interrupt, redirect, stop the turn | any message (barge-in) | – |
| Look back in memory | `zoom`, `date`, `search` (the assistant's own) | – |
| Work on the Mac and the web | `luau`; `list_skills`, `load_skill` | – |
| Delegate to / message / fire an agent | `delegate`, `tell`, `fire` | – |
| Ask another assistant | `list_assistants`, `ask_assistant`; `@Name` in a message | – |
| Create / archive / restore an assistant | `create_assistant` (name, about, effort, instructions, welcome), `archive_assistant`, `restore_assistant` | – |
| Set / drop goal | `goal` | – |
| Remember / forget / purge / unpurge | `remember`, `forget`, `purge`, `unpurge` | – |
| Usage, import, export, tree | `usage`, `import` (find, start, pause, resume), `export`, `tree` | – |
| Switch assistant | `@Name` alone (voice later: "let me talk to Max") | sidebar, Cmd+B |
| Open / collapse a block | – | click |
| Overview | – | Cmd+Shift+F |
| Look at an agent or a thread | – | tab, `[Name]` link, Ctrl+Tab |
| Attach an image | part of the message | paste, drop, "+" |

## Lexicon

| Term | Means | Don't say |
|---|---|---|
| Acorn | the app; and the general assistant | "Acorn" for whoever is shown |
| Assistant | a long-lived, person-like agent with a name and an about | bot, model |
| About | the one line saying who an assistant is | role, effort, description (in UI) |
| Chat | you and one assistant, forever | session, conversation (in UI) |
| Thread | two assistants, forever | chat (for these) |
| Ask | one assistant consulting another | call |
| Agent | a freelancer an assistant delegates one task to; it reports once and is gone | worker, subagent, helper |
| Delegate / Fire | give an agent its task / let it go | spawn, start, hire, stop, kill (for agents) |
| Archive / Restore | take an assistant off the sidebar, its files kept / bring it back | delete, fire (for assistants) |
| Mention | `@Name` in the box | tag, ping |
| Block | a summary standing for n messages | line, part, node, nx |
| Open / Collapse | a block's halves / back to its summary | fold, unfold, zoom |
| Overview | the whole chat in the fewest blocks | fold all |
| Cut off | what barge-in does to a reply | interrupted, halted |
| Summarizing | the compactor at work | compressing |
| Fact | something remembered across chats | memory |
| Search | find the messages holding a word | grep |
| Purge / Unpurge | erase a message / take that back within ten minutes | delete, redact |
| Skill | a how-to the user keeps for a kind of task | recipe, playbook |

## Interaction contract

| Act | Feedback | Undo / confirm |
|---|---|---|
| Any message | appears at once; a reply in progress ends "— cut off" | – |
| A tool act | the assistant says what it did | reversible ones by asking ("undo that": remember↔forget, goal↔drop, purge↔unpurge for ten minutes) |
| Ask | call row linking the thread; reply framed under the asker's name | "never mind" cuts it |
| Create assistant | row appears in the sidebar with its about | – |
| Delegate | a tab while it works; its report framed under its name | "fire it" |
| `@Name` alone | that assistant's chat shown | Esc, or the sidebar |
| Purge | the message vanishes | no confirmation; "unpurge it" brings it back within ten minutes, after which it is wiped for good |
| Import | sessions and price stated, then asked | your yes; "pause the import" |

**Surviving modes:** Overview (caption; a key, because it's looking). Goal (visible line).

## Placement

- **Content layer:** the chat; an agent's page; a thread's page.
- **Functional layer, report-only:** sidebar (name, mark, a dot while at work, about on hover); header (name · about, import progress); footer (goal line, today's cost, the box, tabs for agents and open threads).
- **No menus, no modals.** Purge needs no confirmation: it is undone by asking, for ten minutes.

## Open questions

- ~~Does what an assistant hears in a thread reach its chat with you?~~ Yes, as people do: his chat sees his threads' latest lines (as built).
- Barge-in and a long tool (a 5-minute build): cut it, as a person who is spoken to stops and listens, at the cost of redoing it? Recommend cut, and later let the assistant start such commands as background jobs that survive a barge-in.
- Switching by spoken word, for voice ("let me talk to Max"): `@Name` covers typing.
- A last-resort stop if an assistant won't obey "stop" for its agents: quitting the app, or a hidden hard key?
