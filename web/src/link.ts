// Link
// ====
// How the window reaches the assistants' engines in acorn-server: a
// command is a POST, each sent after the one before; the events come by
// long polling (server/src/main.rs). Every request carries the server's
// token as a cookie, set by opening the link the server printed.
//
// The server is reached, or not since a request failed (why), till a
// request gets through. Events lost (too old, or another run of the
// server's) are told after the assistants anew, as what was lost of them
// a roster replaces.

import type { Assistant, Command, Event } from './wire.ts';

/** What the window is told. */
export interface Sink {
  /** The server's assistants, all: at first, and anew once events were lost. */
  roster(assistants: Assistant[]): void;
  /** A chat's event. */
  hear(chat: string, ev: Event): void;
  /** Events were lost: the chat shown is to be told anew. */
  lost(): void;
  /** Why the server can't be reached; null: it is again. */
  reach(down: string | null): void;
}

/** The assistant a chat is with (a thread's: its answerer). */
export const whose = (chat: string) => chat.split('/')[0];

/** How long a command may take to get there. */
const SEND_WAIT = 15_000;
/** How long after a failed poll the next. */
const RETRY = 2000;

interface Hello {
  assistants: Assistant[];
}

interface Batch {
  lost: boolean;
  events: [string, Event][];
}

let down: string | null = null;
let told: (down: string | null) => void = () => {};

function reach(why: string | null) {
  if (why === down) return;
  down = why;
  told(why);
}

const why = (err: unknown) => (err instanceof Error ? err.message : String(err));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The server's assistants, then every chat's events, polled, till
 * `signal` aborts. Epoch and seq are numbers the server counts from: an
 * epoch is nanoseconds, past 2^53, so they go back as they came, as text.
 * Lost events are polled on from where the batch ends, after the
 * assistants anew: those told twice are told the same. */
export async function listen(sink: Sink, signal: AbortSignal) {
  told = sink.reach;
  let at: [string, string] | null = null;
  let lost = false;
  while (!signal.aborted) {
    try {
      if (!at || lost) {
        const raw = await get('/api/hello', signal);
        at ??= cursor(raw);
        reach(null);
        sink.roster((JSON.parse(raw) as Hello).assistants);
        if (lost) {
          lost = false;
          sink.lost();
        }
        continue;
      }
      const raw = await get(`/api/events?epoch=${at[0]}&after=${at[1]}`, signal);
      const batch = JSON.parse(raw) as Batch;
      at = cursor(raw);
      reach(null);
      for (const [chat, ev] of batch.events) sink.hear(chat, ev);
      lost = batch.lost;
    } catch (err) {
      if (signal.aborted) return;
      reach(why(err));
      await sleep(RETRY);
    }
  }
}

async function get(url: string, signal: AbortSignal): Promise<string> {
  const res = await fetch(url, { credentials: 'same-origin', signal });
  const text = await res.text();
  if (!res.ok) throw new Error(`${res.status} ${text}`.trim());
  return text;
}

/** A reply's `epoch` and `seq`, as written (JSON numbers past 2^53 lose
 * digits as numbers). */
function cursor(raw: string): [string, string] {
  const epoch = raw.match(/"epoch":(\d+)/)?.[1] ?? '0';
  const seq = raw.match(/"seq":(\d+)/)?.[1] ?? '0';
  return [epoch, seq];
}

let queue: Promise<unknown> = Promise.resolve();

/** A command to a chat's engine, on the server, after those sent before
 * it: whether it got there. */
export function send(chat: string, cmd: Command): Promise<boolean> {
  const sent = queue.then(async () => {
    try {
      const res = await fetch(`/api/cmd?chat=${encodeURIComponent(chat)}`, {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(cmd),
        signal: AbortSignal.timeout(SEND_WAIT),
      });
      // answered, it is reached; failing on its side (5xx), not
      const why = res.ok ? null : `${res.status} ${await res.text()}`.trim();
      reach(res.status >= 500 ? why : null);
      return res.ok;
    } catch (err) {
      reach(why(err));
      return false;
    }
  });
  queue = sent;
  return sent;
}

/** Where a chat's image is shown from: the server. */
export const image = (chat: string, name: string) =>
  `/api/image?chat=${encodeURIComponent(chat)}&name=${encodeURIComponent(name)}`;
