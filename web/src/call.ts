// Call
// ====
// A tool call as people read it (app/src/call.rs). Logged as its tool's
// name, a space and its input's JSON (`zoom {"id":6243,"n":1}`), it shows
// as its emoji, its name in bold and a head, as its tool reads: a command
// as itself; a file tool its path, read's lines after it, and write's
// content and edit's replacement as a diff; zoom and date the message
// (`#6243`, a block `#6243+8`, an agent by name); delegate its tasks, tell
// its name and text in a block. Any other call shows its name and short
// values as arguments, over a block for each other value, keyed by its
// name unless alone. A call still streaming reads as its input so far.
//
// An agent's report is logged as `[Name]` on its own line, its replies,
// and a line naming its whole chat; it shows as its replies alone.

const SHORT = 60;

/** A block of a call: its key ("" if none), its text. */
export interface Code {
  k: string;
  x: string;
}

/** name: the bare tool name ("": no call); head: the rest of its line. */
export interface Call {
  name: string;
  head: string;
  blocks: Code[];
}

type Obj = Record<string, unknown>;

/** An agent's report, read: its agent, its replies. */
export function reportRead(text: string): [string, string] | null {
  if (!text.startsWith('[')) return null;
  const rest = text.slice(1);
  const close = rest.indexOf(']');
  if (close < 0) return null;
  const name = rest.slice(0, close);
  if (!name || name.includes('\n')) return null;
  let body = rest.slice(close + 1).trim();
  const k = body.lastIndexOf('Full chat: zoom("');
  if (k >= 0 && body.slice(k).trimEnd().endsWith('")') && !body.slice(k).includes('\n')) {
    body = body.slice(0, k).trimEnd();
  }
  return [name, body];
}

export function parse(text: string): Call {
  const plain = (): Call => ({ name: '', head: text, blocks: [] });
  const sp = text.indexOf(' ');
  if (sp <= 0) return plain();
  let name = text.slice(0, sp);
  const json = text.slice(sp + 1);
  if (/\s/.test(name) || !json.startsWith('{')) return plain();
  const input = inputParse(json);
  if (!input) return plain();
  if (name.startsWith('mcp__')) {
    const rest = name.slice(5);
    const at = rest.indexOf('__');
    if (at >= 0) name = rest.slice(at + 2);
  }
  const [head, blocks] = shown(name, input);
  return { name, head, blocks };
}

/** The call on one line, after its name: its head, then each block,
 * spaces folded. */
export function line(c: Call): string {
  const blocks = c.blocks.map((b) => (b.k ? `${b.k}: ${b.x}` : b.x));
  return [c.head, ...blocks].join(' ').split(/\s+/).filter(Boolean).join(' ');
}

function shown(name: string, x: Obj): [string, Code[]] {
  const str = (k: string) => (typeof x[k] === 'string' ? (x[k] as string) : '');
  switch (name) {
    case 'bash':
      return [str('command'), []];
    case 'read':
      return [str('file_path') + linesShow(x.offset, x.limit), []];
    case 'write': {
      const p = str('file_path');
      return [p, diffBlock(p, '', str('content'))];
    }
    case 'edit': {
      const p = str('file_path');
      return [p, diffBlock(p, str('old_string'), str('new_string'))];
    }
    case 'zoom': {
      const at = 'at' in x ? ` from char ${JSON.stringify(x.at)}` : '';
      return [msgShow(x.id, x.n) + at, []];
    }
    case 'date':
      return [msgShow(x.id, undefined), []];
    case 'web_search':
      return [str('query'), []];
    case 'web_fetch':
      return [str('url'), []];
    case 'luau':
      return [str('description'), [{ k: '', x: str('code') }]];
    case 'delegate':
    case 'hire':
    case 'spawn': {
      const tasks = Array.isArray(x.tasks) ? x.tasks : [];
      const n = tasks.length;
      const effort = typeof x.effort === 'string' ? ` · ${x.effort}` : '';
      return [`${n} ${n === 1 ? 'agent' : 'agents'}${effort}`, tasks.flatMap(textBlock)];
    }
    case 'tell':
      return [str('name'), 'message' in x ? textBlock(x.message) : []];
    default:
      return inputShown(x);
  }
}

/** A replacement as a unified diff of its texts alone, as a block (none
 * if both are empty): where in the file it falls, the call doesn't say. */
function diffBlock(path: string, oldText: string, newText: string): Code[] {
  const lines = (t: string) => (t ? (t.endsWith('\n') ? t.slice(0, -1) : t).split('\n') : []);
  const [o, n] = [lines(oldText), lines(newText)];
  if (!o.length && !n.length) return [];
  const at = (k: number) => (k > 0 ? 1 : 0);
  let x = `--- ${path}\n+++ ${path}\n@@ -${at(o.length)},${o.length} +${at(n.length)},${n.length} @@`;
  for (const l of o) x += `\n-${l}`;
  for (const l of n) x += `\n+${l}`;
  return [{ k: '', x }];
}

/** Any tool's input: its short values as arguments, the rest in blocks. */
function inputShown(input: Obj): [string, Code[]] {
  const keys = Object.keys(input);
  const alone = keys.length <= 1;
  const args: string[] = [];
  const blocks: Code[] = [];
  for (const k of keys) {
    const v = input[k];
    const json = JSON.stringify(v);
    const word = typeof v !== 'string' || !/[\s"\\]/.test(v);
    if ([...json].length <= SHORT && word) {
      args.push(json);
    } else {
      blocks.push({ k: alone ? '' : k, x: typeof v === 'string' ? v : JSON.stringify(v, null, 2) });
    }
  }
  return [args.join(' '), blocks];
}

function textBlock(x: unknown): Code[] {
  if (typeof x !== 'string' || !x) return [];
  return [{ k: '', x: x.endsWith('\n') ? x.slice(0, -1) : x }];
}

/** The message or block zoom and date name: `#6243`, `#6243+8` (n > 1),
 * an agent's name as is. */
function msgShow(id: unknown, n: unknown): string {
  const many = typeof n === 'number' && Number.isInteger(n) && n > 1 ? `+${n}` : '';
  if (typeof id === 'number') return `#${id}${many}`;
  if (typeof id === 'string') return id;
  if (id === undefined) return '';
  return JSON.stringify(id);
}

/** A read's lines, as path:4-7 shows them ("": the whole file). */
function linesShow(offset: unknown, limit: unknown): string {
  if (offset === undefined && limit === undefined) return '';
  const from = typeof offset === 'number' ? Math.trunc(offset) : 1;
  const to = typeof limit === 'number' ? String(from + Math.trunc(limit) - 1) : '';
  return `:${from}-${to}`;
}

/** null: no JSON object, nor a prefix of one. A prefix is closed: its
 * open string and brackets, else cut at its last whole field. */
function inputParse(text: string): Obj | null {
  const shut: string[] = [];
  let quote = false;
  let esc = false;
  let field = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quote) {
      quote = esc || c !== '"';
      esc = !esc && c === '\\';
    } else if (c === '"') {
      quote = true;
    } else if (c === '{' || c === '[') {
      shut.push(c === '{' ? '}' : ']');
      if (shut.length === 1) field = i + 1;
    } else if (c === '}' || c === ']') {
      shut.pop();
    } else if (c === ',' && shut.length === 1) {
      field = i;
    }
  }
  const open = quote ? `${esc ? text.slice(0, -1) : text}"` : text;
  const full = open.replace(/[\s,]+$/, '') + [...shut].reverse().join('');
  return object(full) ?? object(`${text.slice(0, field)}}`);
}

function object(text: string): Obj | null {
  try {
    const v = JSON.parse(text);
    return v && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : null;
  } catch {
    return null;
  }
}
