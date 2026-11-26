// Model
// =====
// What the window holds and draws of the chat, with no view of its own
// (app/src/model.rs): `hear` takes the engine's events, `act` the
// person's acts, and both answer the model anew and what to send back.
// The main chat shows what its agent sees: the view's parts as blocks,
// from the turn on each message whole and its thoughts, then what streams
// or what the turn waits on. An agent's page is its log, kept live. A
// thread opens with its caption. The only act that changes anything is a
// message; the rest look. A model is never changed in place: each field
// changed is new, so what draws from the rest is kept. What no page can
// show any more is dropped (`prune`, `tidy`), so a window open for long
// holds the chat shown, not all it ever showed.

import * as call from './call.ts';
import type { Call } from './call.ts';
import { flat, progressShow } from './text.ts';
import { type Command, type Entry, type Event, type Kind, type Live, livePush, type Node, type Seg, type Spend, type State } from './wire.ts';

const WAIT = 'summarizing…';
const SUMS = 'summarizing earlier messages…';
const FOLD = 'Overview: the whole chat in the fewest blocks. Esc goes back.';
const FOLDING = 'finding the fewest blocks…';

/** A Seg as a map's key. */
export const key = ([id, n]: Seg) => `${id}+${n}`;

/** An agent's page: its log (null while it loads), what streams. */
export interface Page {
  log: Entry[] | null;
  live: Live[];
}

/** The overview: its tops (null till they come). */
export interface Fold {
  parts: Seg[] | null;
}

/** The chat as the window knows it. head: what it opens with (a thread's
 * caption); fold: the overview (null: not shown); nodes and whole: the
 * summaries and messages known; open: the blocks opened (true) or shut by
 * hand; minds: the thoughts after each message; pages: the agents' asked
 * for; page: the one shown (null: the chat). */
export interface Model {
  head: string | null;
  state: State | null;
  parts: Seg[];
  turn: number;
  fold: Fold | null;
  nodes: Map<string, Node>;
  whole: Map<number, Entry>;
  open: Map<string, boolean>;
  live: Live[];
  minds: Map<number, string[]>;
  pages: Map<string, Page>;
  page: string | null;
}

export function empty(head: string | null = null): Model {
  return {
    head,
    state: null,
    parts: [],
    turn: Infinity,
    fold: null,
    nodes: new Map(),
    whole: new Map(),
    open: new Map(),
    live: [],
    minds: new Map(),
    pages: new Map(),
    page: null,
  };
}

/** What a page draws, top to bottom. */
export type Row =
  /** A block: of n messages, its summary (call: a tool call's). */
  | { t: 'sum'; seg: Seg; text: string; call: Call | null }
  /** A block open: its bar, its halves. */
  | { t: 'box'; seg: Seg; text: string; rows: Row[] }
  /** A message whole: who said it, its text, its call if a tool call. */
  | { t: 'msg'; kind: Kind; who: string; text: string; call: Call | null; images: string[] }
  /** What a turn used, under its last message. */
  | { t: 'use'; spend: Spend }
  /** A line about the page. */
  | { t: 'cap'; text: string };

/** A person's act: a message, or a look. */
export type Act =
  | { t: 'send'; text: string; images: string[] }
  | { t: 'open'; seg: Seg }
  | { t: 'shut'; seg: Seg }
  | { t: 'fold'; on: boolean }
  | { t: 'goto'; name: string | null }
  /** Back to the chat: off the agent's page, out of the overview. */
  | { t: 'home' };

/** The model anew, and what it leads to: commands, notes to show. */
export interface Then {
  m: Model;
  cmds: Command[];
  notes: string[];
}

export function hear(m: Model, ev: Event): Then {
  const cmds: Command[] = [];
  const notes: string[] = [];
  let n = m;
  if ('Know' in ev) {
    const whole = new Map(m.whole);
    for (const [id, e] of ev.Know.whole) whole.set(id, e);
    const nodes = new Map(m.nodes);
    for (const [seg, node] of ev.Know.nodes) nodes.set(key(seg), node);
    n = { ...m, whole, nodes };
  } else if ('View' in ev) {
    n = prune({ ...m, parts: ev.View.parts, turn: ev.View.turn });
    if (m.fold) cmds.push('Root');
  } else if ('Root' in ev) {
    if (m.fold) n = prune({ ...m, fold: { parts: ev.Root.parts } });
  } else if ('Entry' in ev) {
    const { agent, id, entry } = ev.Entry;
    if (agent !== null) {
      const page = m.pages.get(agent);
      if (page?.log) n = { ...m, pages: new Map(m.pages).set(agent, { ...page, log: [...page.log, entry] }) };
    } else if (id !== null) {
      n = { ...m, whole: new Map(m.whole).set(id, entry), parts: [...m.parts, [id, 1]] };
    } else if (entry.kind === 'mind' && m.parts.length) {
      const [pid, pn] = m.parts[m.parts.length - 1];
      const at = pid + pn - 1;
      n = { ...m, minds: new Map(m.minds).set(at, [...(m.minds.get(at) ?? []), entry.text]) };
    }
  } else if ('Delta' in ev) {
    const { agent, kind, text, fresh } = ev.Delta;
    n = withLive(m, agent, (live) => livePush(live, kind, text, fresh));
  } else if ('Live' in ev) {
    n = withLive(m, ev.Live.agent, () => ev.Live.live);
  } else if ('State' in ev) {
    n = tidy({ ...m, state: ev.State });
  } else if ('Agent' in ev) {
    const { name, log } = ev.Agent;
    if (log) {
      const page = m.pages.get(name);
      if (page) n = { ...m, pages: new Map(m.pages).set(name, { ...page, log }) };
    } else {
      const pages = new Map(m.pages);
      pages.delete(name);
      n = { ...m, pages, page: m.page === name ? null : m.page };
      notes.push(`No agent ${name}.`);
    }
  } else if ('Note' in ev) {
    notes.push(ev.Note);
  } else if ('Use' in ev) {
    const e = m.whole.get(ev.Use.id);
    if (e) n = { ...m, whole: new Map(m.whole).set(ev.Use.id, { ...e, spend: ev.Use.spend }) };
  }
  return { m: n, cmds, notes };
}

export function act(m: Model, a: Act): Then {
  const cmds: Command[] = [];
  let n = m;
  switch (a.t) {
    case 'send': {
      const text = a.text.trim();
      if (text || a.images.length) {
        n = { ...m, page: null, fold: null };
        cmds.push({ Say: { text, images: a.images } });
      }
      break;
    }
    case 'open':
      n = { ...m, open: new Map(m.open).set(key(a.seg), true) };
      if (a.seg[1] > 1) cmds.push({ Zoom: a.seg });
      break;
    case 'shut':
      n = { ...m, open: new Map(m.open).set(key(a.seg), false) };
      break;
    case 'fold':
      // the overview is the chat's: shown, it shows on the chat's tab
      n = { ...m, page: a.on ? null : m.page, open: new Map(), fold: a.on ? { parts: null } : null };
      if (a.on) cmds.push('Root');
      break;
    case 'goto':
      if (a.name === null) {
        n = { ...m, page: null };
      } else {
        const pages = m.pages.has(a.name) ? m.pages : new Map(m.pages).set(a.name, { log: null, live: [] });
        if (!m.pages.has(a.name)) cmds.push({ Agent: a.name });
        n = { ...m, pages, page: a.name };
      }
      break;
    case 'home':
      n = m.fold ? { ...m, page: null, open: new Map(), fold: null } : m.page === null ? m : { ...m, page: null };
      break;
  }
  return { m: tidy(n), cmds, notes: [] };
}

/** The model without what no page can show any more: the summaries,
 * messages and blocks opened that are neither a part of the view or of
 * the overview nor under a block opened or shut by hand, and the thoughts
 * before the turn. Its engine tells a summary or message again once it
 * shows again: as a view's new part, a block's half, the overview's top. */
function prune(m: Model): Model {
  const nodes = new Map<string, Node>();
  const whole = new Map<number, Entry>();
  const open = new Map<string, boolean>();
  const keep = (id: number, n: number) => {
    const k = key([id, n]);
    const node = m.nodes.get(k);
    if (node) nodes.set(k, node);
    const on = m.open.get(k);
    if (on !== undefined) open.set(k, on);
    if (n === 1) {
      const entry = m.whole.get(id);
      if (entry) whole.set(id, entry);
    } else if (on !== undefined) {
      keep(id, n / 2);
      keep(id + n / 2, n / 2);
    }
  };
  for (const [id, n] of m.parts) keep(id, n);
  for (const [id, n] of m.fold?.parts ?? []) keep(id, n);
  const minds = new Map([...m.minds].filter(([id]) => id >= m.turn));
  // each a part of what was: as many left, none dropped
  const less = <K, V>(was: Map<K, V>, left: Map<K, V>) => (left.size === was.size ? was : left);
  const n = { nodes: less(m.nodes, nodes), whole: less(m.whole, whole), open: less(m.open, open), minds: less(m.minds, minds) };
  const same = n.nodes === m.nodes && n.whole === m.whole && n.open === m.open && n.minds === m.minds;
  return same ? m : { ...m, ...n };
}

/** The model without the pages of agents done that aren't shown: one
 * shown again is asked for again. */
function tidy(m: Model): Model {
  const at = agents(m);
  const gone = [...m.pages.keys()].filter((name) => name !== m.page && !at.includes(name));
  if (!gone.length) return m;
  const pages = new Map(m.pages);
  for (const name of gone) pages.delete(name);
  return { ...m, pages };
}

/** All the chat shown asks its engine anew, as events were lost or asks
 * failed: the view, the overview, the blocks opened, the agent's page
 * shown (the rest are dropped, asked for once shown). What is known
 * stays till the replies tell it anew. */
export function resync(m: Model): Then {
  const cmds: Command[] = ['Attach'];
  if (m.fold) cmds.push('Root');
  for (const [k, on] of m.open) {
    const [id, n] = k.split('+').map(Number);
    if (on && n > 1) cmds.push({ Zoom: [id, n] });
  }
  const pages = new Map<string, Page>();
  const page = m.page === null ? undefined : m.pages.get(m.page);
  if (m.page !== null && page) {
    pages.set(m.page, page);
    cmds.push({ Agent: m.page });
  }
  return { m: { ...m, pages }, cmds, notes: [] };
}

function withLive(m: Model, agent: string | null, f: (live: Live[]) => Live[]): Model {
  if (agent === null) return { ...m, live: f(m.live) };
  const page = m.pages.get(agent);
  if (!page) return m;
  return { ...m, pages: new Map(m.pages).set(agent, { ...page, live: f(page.live) }) };
}

export const agents = (m: Model): string[] => m.state?.agents ?? [];

/** The chat (null), each agent at work, and the page shown while its
 * agent is done (so it can be left). */
export function tabs(m: Model): (string | null)[] {
  const names = [...agents(m)];
  if (m.page !== null && !names.includes(m.page)) names.push(m.page);
  return [null, ...names];
}

// Rows
// ----

/** An item of a line about the page. */
const capItem = (key: string, text: string) => ({ key, rows: [{ t: 'cap', text } as Row] });

/** The page shown, as items (each a part's rows, or an agent's entry),
 * each with a key that stays while it does. */
export function body(m: Model): { key: string; rows: Row[] }[] {
  if (m.page !== null) {
    const name = m.page;
    const at = agents(m).includes(name) ? 'at work' : 'done';
    const log = m.pages.get(name)?.log ?? [];
    return [
      capItem('cap', `${name} · ${at}`),
      ...log.map((e, k) => ({ key: `e${k}`, rows: entryRows(e.kind, e.text, e.images) })),
    ].filter((it) => it.rows.length);
  }
  if (m.fold) {
    const cap = capItem('cap', FOLD);
    if (!m.fold.parts) return [cap, capItem('folding', FOLDING)];
    return [cap, ...m.fold.parts.map(([id, n]) => ({ key: key([id, n]), rows: partRows(m, id, n, Infinity, false) }))];
  }
  const head = m.head === null ? [] : [capItem('cap', m.head)];
  return [...head, ...m.parts.map(([id, n]) => ({ key: key([id, n]), rows: partRows(m, id, n, m.turn, true) }))];
}

/** What streams on the page shown, and what the turn waits on. */
export function tail(m: Model): Row[] {
  if (m.page === null && m.fold) return [];
  const live = m.page !== null ? (m.pages.get(m.page)?.live ?? []) : m.live;
  const rows = live.flatMap((l) => entryRows(l.kind, l.text, []));
  const importing = m.state?.importing;
  const load: Row[] = m.page === null && importing ? [{ t: 'cap', text: progressShow(importing) }] : [];
  const wait = m.state?.wait ?? 'None';
  if (m.page !== null || rows.length || wait === 'None') return [...rows, ...load];
  const cap = wait === 'Summaries' ? SUMS : `waiting for ${m.state?.model ?? 'the model'}…`;
  return [{ t: 'cap', text: cap }, ...load];
}

/** t: the turn; talk: whether talk opens by itself (not when folded).
 * Open: the person's choice if made, else a message of the user or the
 * assistant (tool calls, reports and the rest stay shut). */
function partRows(m: Model, id: number, n: number, t: number, talk: boolean): Row[] {
  const entry = m.whole.get(id);
  if (entry && n === 1 && id >= t) {
    const rows = entryRows(entry.kind, entry.text, entry.images);
    if (entry.spend && entry.spend.n > 0) rows.push({ t: 'use', spend: entry.spend });
    for (const mind of m.minds.get(id) ?? []) rows.push(...entryRows('mind', mind, []));
    return rows;
  }
  const talks = talk && n === 1 && !!entry && (entry.kind === 'user' || entry.kind === 'unii');
  if (!(m.open.get(key([id, n])) ?? talks)) return [sumRow(m, id, n)];
  const text = n === 1 ? 'collapse' : `▾ ${n} messages · collapse`;
  let rows: Row[];
  if (n > 1) {
    const half = n / 2;
    rows = [...partRows(m, id, half, t, talk), ...partRows(m, id + half, half, t, talk)];
  } else {
    rows = entry ? entryRows(entry.kind, entry.text, entry.images) : [];
    if (entry?.spend && entry.spend.n > 0) rows.push({ t: 'use', spend: entry.spend });
  }
  return [{ t: 'box', seg: [id, n], text, rows }];
}

/** A block; one whose summary isn't known yet says WAIT. A block of one
 * message that is a tool call word for word shows as the call. */
function sumRow(m: Model, id: number, n: number): Row {
  const text = m.nodes.get(key([id, n]))?.text ?? WAIT;
  let c: Call | null = null;
  if (n === 1 && text.startsWith('tool: ')) {
    const parsed = call.parse(text.slice('tool: '.length));
    if (parsed.name) c = parsed;
  }
  return { t: 'sum', seg: [id, n], text: flat(text), call: c };
}

/** A message's rows: none if it shows nothing. */
export function entryRows(kind: Kind, text: string, images: string[]): Row[] {
  const c = kind === 'tool' ? call.parse(text) : null;
  const report = kind === 'work' ? call.reportRead(text) : null;
  const who = report ? report[0] : kind === 'work' ? 'Report' : kind === 'note' ? 'Note' : '';
  const body = report ? report[1] : text;
  if (!c && !body.trim() && !images.length) return [];
  return [{ t: 'msg', kind, who, text: body, call: c, images }];
}
