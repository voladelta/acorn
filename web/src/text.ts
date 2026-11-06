// Text
// ====
// The little reading a message needs beyond Markdown (app/src/text.rs): a
// tool's result as a file's lines (a read's as its lines, a unified diff
// as its file and hunks, any other text as lines named "result"); a
// summary's text with its marks dropped; agents' names (`[FireAnt]`) as
// links to their pages; an `@Name` being typed, its names and its
// completion; and how use, tokens, money and time are said. Offsets here
// are the string's own (UTF-16), as a textarea's caret counts.

import { isName } from './names.ts';
import type { Progress, Spend } from './wire.ts';

/** A line of a diff: kept (' '), dropped ('-') or added ('+'), its number
 * in the old file if dropped, else in the new one. */
export interface Line {
  s: ' ' | '-' | '+';
  n: number;
  x: string;
}

export type Out =
  /** A file's lines, from line n, named k. */
  | { file: true; k: string; n: number; x: string[] }
  /** A diff of file k, its hunks. */
  | { file: false; k: string; x: Line[][] };

/** A tool's result: a read's lines a file, each unified diff in it a
 * diff, the text around them a file named "result" (none if blank). */
export function output(raw: string): Out[] {
  const s = raw.replaceAll('\r', '');
  const read = readEnvelope(s);
  if (read) return [read];
  const lines = s.split('\n');
  const out: Out[] = [];
  let i = 0;
  let at = 0;
  while (i < lines.length) {
    const [diff, next] = diffParse(lines, i);
    if (diff) {
      const r = result(lines.slice(at, i));
      if (r) out.push(r);
      out.push(diff);
      at = next;
    }
    i = next;
  }
  const r = result(lines.slice(at));
  if (r) out.push(r);
  return out;
}

/** A read's result: `<path>`, `<type>file</type>`, then in `<content>`
 * its lines as "n: text", a blank line and a footer in parentheses. */
function readEnvelope(s: string): Out | null {
  if (!s.startsWith('<path>')) return null;
  const mark = '</path>\n<type>file</type>\n<content>\n';
  const m = s.indexOf(mark);
  if (m < 0) return null;
  const rest = s.slice(m + mark.length);
  if (!rest.endsWith('\n</content>')) return null;
  const body = rest.slice(0, -'\n</content>'.length);
  const cut = body.lastIndexOf('\n\n');
  const [lines, footer] = cut < 0 ? ['', body] : [body.slice(0, cut), body.slice(cut + 2)];
  if (!footer.startsWith('(') || !footer.endsWith(')')) return null;
  let n: number | null = null;
  const x: string[] = [];
  if (lines) {
    for (const l of lines.split('\n')) {
      const c = l.indexOf(': ');
      if (c < 0) return null;
      const num = Number(l.slice(0, c));
      if (!Number.isInteger(num) || num < 0) return null;
      n ??= num;
      x.push(l.slice(c + 2));
    }
  }
  return { file: true, k: footer.slice(1, -1), n: n ?? 1, x };
}

function result(lines: string[]): Out | null {
  const from = lines.findIndex((l) => l.trim());
  if (from < 0) return null;
  let upto = lines.length - 1;
  while (!lines[upto].trim()) upto--;
  return { file: true, k: 'result', n: 1, x: lines.slice(from, upto + 1) };
}

/** null: no diff starts at line i; and the line after it. */
function diffParse(lines: string[], i: number): [Out | null, number] {
  const file = lines[i + 1]?.startsWith('+++ ') ? lines[i + 1].slice(4) : null;
  if (file === null || !lines[i].startsWith('--- ')) return [null, i + 1];
  let j = i + 2;
  const hunks: Line[][] = [];
  for (let h = hunkHead(lines[j]); h; h = hunkHead(lines[j])) {
    const got = hunkParse(lines, j + 1, h);
    if (!got) return [null, i + 1];
    hunks.push(got[0]);
    j = got[1];
  }
  return hunks.length ? [{ file: false, k: file, x: hunks }, j] : [null, i + 1];
}

/** "@@ -o,olds +n,news @@" */
function hunkHead(l: string | undefined): [number, number, number, number] | null {
  const m = l?.match(/^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/);
  if (!m) return null;
  return [Number(m[1]), m[2] === undefined ? 1 : Number(m[2]), Number(m[3]), m[4] === undefined ? 1 : Number(m[4])];
}

function hunkParse(lines: string[], j: number, h: [number, number, number, number]): [Line[], number] | null {
  let [o, olds, n, news] = h;
  const out: Line[] = [];
  while (olds > 0 || news > 0) {
    const line = lines[j] ?? '';
    const s = line[0];
    const x = line.slice(1);
    if (s === ' ' && olds > 0 && news > 0) {
      out.push({ s: ' ', n, x });
      n++;
      o++;
      olds--;
      news--;
    } else if (s === '-' && olds > 0) {
      out.push({ s: '-', n: o, x });
      o++;
      olds--;
    } else if (s === '+' && news > 0) {
      out.push({ s: '+', n, x });
      n++;
      news--;
    } else if (s !== '\\') {
      return null;
    }
    j++;
  }
  return [out, j];
}

/** A text with its marks dropped (a summary is plain text): code, bold,
 * italics, links. */
export function flat(s: string): string {
  let out = '';
  let i = 0;
  while (i < s.length) {
    const c = s[i];
    const next = s.slice(i + 1);
    if (c === '[') {
      const close = next.indexOf('](');
      const end = close < 0 ? -1 : next.indexOf(')', close + 2);
      if (close >= 0 && end >= 0 && !next.slice(0, close).includes('\n')) {
        out += next.slice(0, close);
        i += 1 + end + 1;
        continue;
      }
    }
    if (c === '`' || (c === '*' && (next.startsWith('*') || /^\S/.test(next)))) {
      i += next.startsWith('*') ? 2 : 1;
      continue;
    }
    out += c;
    i++;
  }
  return out;
}

/** Markdown with each agent's name (`[FireAnt]`, not a link already) a
 * link to its page: `agent:FireAnt`. */
export function linkNames(s: string): string {
  return s.replace(/\[([^\]\n]*)\](?!\()/g, (all, name: string) =>
    isName(name) ? `[${name}](agent:${name})` : all,
  );
}

/** A text's runs: plain, or an agent's name (`[FireAnt]`). */
export function nameRuns(s: string): { text: string; name?: string }[] {
  const out: { text: string; name?: string }[] = [];
  let at = 0;
  for (const m of s.matchAll(/\[([^\]\n]*)\]/g)) {
    if (!isName(m[1])) continue;
    if (m.index > at) out.push({ text: s.slice(at, m.index) });
    out.push({ text: m[0], name: m[1] });
    at = m.index + m[0].length;
  }
  if (at < s.length || !out.length) out.push({ text: s.slice(at) });
  return out;
}

// Use
// ===

export function useText(u: Spend, now: number): string {
  const all = u.input + u.read;
  const cached = u.read > 0 && all > 0 ? ` (${Math.round((100 * u.read) / all)}% cached)` : '';
  const parts = [usdShow(u.usd), `${tokensShow(all)} in${cached}`, `${tokensShow(u.output)} out`];
  if (u.n > 1) parts.push(`${u.n} calls`);
  parts.push(agoShow(u.at, now));
  return parts.join(' · ');
}

/** The exact numbers and date of a use. */
export function useHint(u: Spend): string {
  const req = u.n === 1 ? '1 request' : `${grouped(u.n)} requests`;
  return `${clockFull(u.at)}\n${req}: ${grouped(u.input + u.read)} tokens in (${grouped(u.read)} cached), ${grouped(u.output)} out, $${u.usd.toFixed(6)}`;
}

/** 950, 18.2k, 1.4M */
export function tokensShow(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1e3).toFixed(1)}k`;
  if (n < 1_000_000) return `${(n / 1e3).toFixed(0)}k`;
  return `${(n / 1e6).toFixed(1)}M`;
}

/** A cost, to two figures at least: $0.0031, $0.042, $1.27. */
export function usdShow(usd: number): string {
  if (usd === 0) return '$0';
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  if (usd < 1) return `$${usd.toFixed(3)}`;
  return `$${usd.toFixed(2)}`;
}

export function grouped(n: number): string {
  return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
}

const pad = (n: number) => String(n).padStart(2, '0');

function clockFull(at: number): string {
  const d = new Date(at);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** 14:03 (today), else Oct 25 14:03. */
export function clockShow(at: number): string {
  const d = new Date(at);
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (d.toDateString() === new Date().toDateString()) return hm;
  return `${d.toLocaleString('en', { month: 'short' })} ${d.getDate()} ${hm}`;
}

/** just now, 3m ago, 2h ago, else the clock. */
export function agoShow(at: number, now: number): string {
  const s = Math.max(0, (now - at) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 6 * 3600) return `${Math.floor(s / 3600)}h ago`;
  return clockShow(at);
}

/** A reply a message cut in on: its text without the mark, and whether
 * it was cut. */
export function cutOff(s: string): [string, boolean] {
  return s.endsWith('[cut off]') ? [s.slice(0, -'[cut off]'.length).trimEnd(), true] : [s, false];
}

/** The `@` word the caret ends: its start and what follows the `@`
 * (`"ask @Ma|"` → [4, "Ma"]). A word starts the text or follows
 * whitespace. */
export function mentionAt(s: string, caret: number): [number, string] | null {
  const typed = s.slice(0, caret);
  const m = typed.match(/(^|\s)(\S*)$/);
  const start = typed.length - (m?.[2].length ?? 0);
  const word = typed.slice(start);
  return word.startsWith('@') ? [start, word.slice(1)] : null;
}

/** The names an `@` word may mean: those it starts first, then those it
 * is in, ignoring case. */
export function namesFor(word: string, names: string[]): string[] {
  const w = word.toLowerCase();
  const starts = names.filter((n) => n.toLowerCase().startsWith(w));
  const has = names.filter((n) => !n.toLowerCase().startsWith(w) && n.toLowerCase().includes(w));
  return [...starts, ...has];
}

/** The text with the `@` word from `start` to the caret made `@name`, a
 * space after it, and the caret after that. */
export function mentionDone(s: string, start: number, caret: number, name: string): [string, number] {
  const space = /^\s/.test(s.slice(caret)) ? '' : ' ';
  const head = `${s.slice(0, start)}@${name}${space}`;
  return [head + s.slice(caret), head.length + (space ? 0 : 1)];
}

/** The name a message is alone (`"@Max"`), ignoring case. */
export function mentionAlone(s: string, names: string[]): string | null {
  const t = s.trim();
  if (!t.startsWith('@')) return null;
  const word = t.slice(1).trim().toLowerCase();
  return names.find((n) => n.toLowerCase() === word) ?? null;
}

/** An import's minutes a micro-USD of its time (the compactor, measured
 * on a real import: 990 s for 162,900). */
const PACE = 1e-4;

/** An import's progress. */
export function progressShow(p: Progress): string {
  const merges = p.open.reduce((a, b) => a + b, 0);
  let phase: string;
  if ((p.open[0] ?? 0) > 0) {
    phase = `Summarizing messages: ${grouped(p.msgs - p.open[0])} of ${grouped(p.msgs)}`;
  } else {
    const minutes = Math.ceil((p.total - p.done) * PACE);
    phase = `Merging the top of the tree: ${merges} ${merges === 1 ? 'merge' : 'merges'} left · about ${minutes} min`;
  }
  return p.why ? `${phase} · paused` : phase;
}
