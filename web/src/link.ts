// Link
// ====
// How the window reaches the assistants' engines in acorn-server: a
// command is a POST, the events come by long polling (server/src/main.rs).
// Every request carries the server's token as a cookie, set by opening
// the link the server printed.

import type { Assistant, Command, Event } from './wire.ts';

/** Said (as a note to no chat) when events were lost: the window shows
 * its chat anew. */
export const LOST = '\u0000lost';

/** The assistant a chat is with (a thread's: its answerer). */
export const whose = (chat: string) => chat.split('/')[0];

interface Hello {
  assistants: Assistant[];
  epoch: number;
  seq: number;
}

interface Batch {
  epoch: number;
  seq: number;
  lost: boolean;
  events: [string, Event][];
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The server's assistants (each heard as an `Assistant` event, the first
 * the one talked to), then every chat's events, polled, till `signal`
 * aborts. Epoch and seq are numbers the server counts from: an epoch is
 * nanoseconds, past 2^53, so they go back as they came, as text. */
export async function listen(hear: (chat: string, ev: Event) => void, signal: AbortSignal) {
  let epoch = '';
  let seq = '0';
  while (!signal.aborted) {
    try {
      const res = await fetch('/api/hello', { credentials: 'same-origin', signal });
      if (!res.ok) throw new Error(`${res.status} ${await res.text()}`);
      const raw = await res.text();
      const hello = JSON.parse(raw) as Hello;
      [epoch, seq] = numbers(raw);
      for (const a of hello.assistants) hear(a.id, { Assistant: a });
      break;
    } catch (err) {
      if (signal.aborted) return;
      hear('', { Note: `Can't reach the server: ${(err as Error).message}` });
      await sleep(3000);
    }
  }
  while (!signal.aborted) {
    try {
      const res = await fetch(`/api/events?epoch=${epoch}&after=${seq}`, { credentials: 'same-origin', signal });
      if (!res.ok) throw new Error(String(res.status));
      const raw = await res.text();
      const batch = JSON.parse(raw) as Batch;
      [epoch, seq] = numbers(raw);
      if (batch.lost) hear('', { Note: LOST });
      for (const [chat, ev] of batch.events) hear(chat, ev);
    } catch {
      if (signal.aborted) return;
      await sleep(2000);
    }
  }
}

/** A reply's `epoch` and `seq`, as written (JSON numbers past 2^53 lose
 * digits as numbers). */
function numbers(raw: string): [string, string] {
  const epoch = raw.match(/"epoch":(\d+)/)?.[1] ?? '0';
  const seq = raw.match(/"seq":(\d+)/)?.[1] ?? '0';
  return [epoch, seq];
}

/** A command to a chat's engine, on the server. */
export function send(chat: string, cmd: Command) {
  void fetch(`/api/cmd?chat=${encodeURIComponent(chat)}`, {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(cmd),
  }).catch(() => {});
}

/** Where a chat's image is shown from: the server. */
export const image = (chat: string, name: string) =>
  `/api/image?chat=${encodeURIComponent(chat)}&name=${encodeURIComponent(name)}`;
