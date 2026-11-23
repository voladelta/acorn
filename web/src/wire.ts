// Wire
// ====
// What acorn-server says and is sent, as core/src/wire.rs puts it on the
// wire (serde's JSON: enums externally tagged, a Seg a pair).

/** A message's kind; Acorn's reply is logged as "unii". */
export type Kind = 'user' | 'work' | 'mind' | 'unii' | 'tool' | 'echo' | 'note';

/** The n messages from id on. */
export type Seg = [number, number];

export interface Spend {
  n: number;
  input: number;
  read: number;
  output: number;
  usd: number;
  at: number;
}

export interface Entry {
  kind: Kind;
  text: string;
  date: string;
  images: string[];
  spend: Spend | null;
}

export interface Node {
  text: string;
  to: string;
}

export interface Live {
  kind: Kind;
  text: string;
}

export interface Progress {
  done: number;
  total: number;
  spent: number;
  left: number;
  why: string | null;
  msgs: number;
  open: number[];
}

export type Wait = 'None' | 'Summaries' | 'Model';

export interface State {
  busy: boolean;
  wait: Wait;
  agents: string[];
  importing: Progress | null;
  today: Spend | null;
}

export interface Assistant {
  id: string;
  name: string;
  about: string;
  effort?: string;
  archived?: boolean;
}

export type Event =
  | { Know: { whole: [number, Entry][]; nodes: [Seg, Node][] } }
  | { View: { parts: Seg[]; turn: number } }
  | { Zoom: { seg: Seg; parts: Seg[] } }
  | { Root: { parts: Seg[] } }
  | { Entry: { agent: string | null; id: number | null; entry: Entry } }
  | { Delta: { agent: string | null; kind: Kind; text: string; fresh: boolean } }
  | { Live: { agent: string | null; live: Live[] } }
  | { State: State }
  | { Agent: { name: string; log: Entry[] | null } }
  | { Note: string }
  | { Use: { id: number; spend: Spend } }
  | { Assistant: Assistant }
  | { Gone: string };

export type Command =
  | { Say: { text: string; images: string[] } }
  | 'Attach'
  | { Zoom: Seg }
  | 'Root'
  | { Agent: string };

/** The one model every call runs on. */
export const MODEL = 'deepseek-flash';

/** A piece joins the message streaming, if of its kind, unless it starts
 * one (fresh: a tool call, as two calls never join). */
export function livePush(live: Live[], kind: Kind, text: string, fresh: boolean): Live[] {
  const top = live[live.length - 1];
  if (top && top.kind === kind && !fresh) {
    return [...live.slice(0, -1), { kind, text: top.text + text }];
  }
  return [...live, { kind, text }];
}
