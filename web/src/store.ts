// Store
// =====
// What every part of the window shares (app/src/lib.rs's `Ui`): the model
// of the chat shown, the assistants, the one you talk to, the chat shown
// (its chat with you, or a thread it asked in), the chats at work,
// whether the sidebar shows, why the server can't be reached, the note
// shown, the message being written and the images waiting, the clock.
// One state, never changed in place; components read it with `useUi`,
// and the acts below make it anew.
//
// The chat shown is in step with its engine, or stale: events were lost,
// or a command asking for what it shows failed. Stale, it asks for it all
// anew (`model.resync`): at once if events were lost, else once the
// server is reached again.

import { useSyncExternalStore } from 'octane';
import * as link from './link.ts';
import * as model from './model.ts';
import type { Act, Model } from './model.ts';
import { mentionAlone } from './text.ts';
import type { Assistant, Command, Event } from './wire.ts';

/** An image waiting to go: its bytes in base64, a URL to show it. */
export interface Pic {
  b64: string;
  url: string;
}

export interface Ui {
  model: Model;
  assistants: Assistant[];
  talk: string;
  shown: string;
  working: Set<string>;
  side: boolean;
  down: string | null;
  toast: string | null;
  draft: string;
  pics: Pic[];
  now: number;
}

/** Wide enough for the sidebar beside the chat. */
const WIDE = '(min-width: 768px)';

/** Whether the sidebar floats over the chat. */
export const narrow = () => typeof window !== 'undefined' && !window.matchMedia(WIDE).matches;

let ui: Ui = {
  model: model.empty(),
  assistants: [],
  talk: '',
  shown: '',
  working: new Set(),
  side: !narrow(),
  down: null,
  toast: null,
  draft: '',
  pics: [],
  now: Date.now(),
};

const subs = new Set<() => void>();

function subscribe(f: () => void) {
  subs.add(f);
  return () => subs.delete(f);
}

export const get = () => ui;

export function set(next: Partial<Ui>) {
  ui = { ...ui, ...next };
  for (const f of subs) f();
}

/** The window's state, read in a component. */
export const useUi = () => useSyncExternalStore(subscribe, get, get);

/** The assistant you talk to. */
export function me(u: Ui = ui): Assistant {
  return (
    u.assistants.find((a) => a.id === u.talk) ??
    u.assistants[0] ?? { id: u.talk, name: 'Acorn', about: '' }
  );
}

/** An assistant's name, by id. */
export const named = (id: string) => ui.assistants.find((a) => a.id === id)?.name ?? id;

/** Whether a chat's assistants (a thread's two) are all listed. */
const listed = (chat: string, among: Assistant[] = ui.assistants) =>
  chat.split('/').every((id) => among.some((a) => a.id === id));

/** A person's act, done: whether what it sent got there. */
export const act = (a: Act) => then(model.act(ui.model, a));

let stale = false;

/** A command to the shown chat's engine: whether it got there. One that
 * asks for what is shown, failing, leaves the chat stale. */
export function send(cmd: Command): Promise<boolean> {
  const sent = link.send(ui.shown, cmd);
  if (typeof cmd !== 'object' || !('Say' in cmd)) {
    void sent.then((ok) => {
      if (!ok) stale = true;
    });
  }
  return sent;
}

function then({ m, cmds, notes }: model.Then): Promise<boolean> {
  if (m !== ui.model) set({ model: m });
  if (notes.length) note(notes[notes.length - 1]);
  return Promise.all(cmds.map(send)).then((oks) => oks.every(Boolean));
}

/** The chat shown, asked for anew. */
function resync() {
  stale = false;
  if (ui.shown) void then(model.resync(ui.model));
}

/** A chat's model, empty: a thread's opens with who asks and who answers
 * there. */
function fresh(id: string): Model {
  const at = id.indexOf('/');
  if (at < 0) return model.empty();
  const [answers, asks] = [named(id.slice(0, at)), named(id.slice(at + 1))];
  return model.empty(`${answers}'s thread with ${asks}: ${asks} asks, ${answers} answers. Esc goes back.`);
}

/** A chat shown, from scratch: its engine tells it all anew. The one
 * shown again: back to its chat, from its agents' pages and overview.
 * None (""): nothing. */
export function show(id: string) {
  if (ui.shown === id) {
    void act({ t: 'home' });
    return;
  }
  stale = false;
  set({ model: fresh(id), shown: id });
  if (id) void send('Attach');
}

/** An assistant to talk to: its chat with you shown. */
export function talkTo(id: string) {
  set({ talk: id });
  show(id);
}

/** Back to your chat with the assistant, from whatever you look at: a
 * thread, an agent's page, the overview. */
export const back = () => show(ui.talk);

/** Your words, to the assistant you talk to, its chat shown: sent while
 * it works, they cut in; `@Name` alone, that assistant's chat shown
 * instead. Null: nothing to say, or no one to say it to; else whether
 * they got there. */
export function say(text: string, images: string[]): Promise<boolean> | null {
  if ((!text.trim() && !images.length) || !ui.talk) return null;
  if (!images.length) {
    const name = mentionAlone(
      text,
      ui.assistants.map((a) => a.name),
    );
    const a = name && ui.assistants.find((x) => x.name === name);
    if (a) {
      talkTo(a.id);
      return Promise.resolve(true);
    }
  }
  if (ui.shown !== ui.talk) show(ui.talk);
  return act({ t: 'send', text, images });
}

/** A name in the chat, opened: an assistant's, its thread with the one
 * you talk to; an agent's, its page. */
export function openName(name: string) {
  const a = ui.assistants.find((x) => x.name === name);
  if (a) show(`${a.id}/${ui.talk}`);
  else void act({ t: 'goto', name });
}

let noteTimer: ReturnType<typeof setTimeout> | undefined;

/** A note, shown a while; null: none. */
export function note(text: string | null) {
  set({ toast: text });
  clearTimeout(noteTimer);
  if (text !== null) noteTimer = setTimeout(() => set({ toast: null }), 6000);
}

/** The assistants anew: the chats of those gone no longer at work, and
 * if the one you talk to is gone (or none was yet), the first talked to;
 * a thread shown with one gone, your chat shown instead. */
function roster(assistants: Assistant[]) {
  set({ assistants, working: new Set([...ui.working].filter((c) => listed(c, assistants))) });
  if (!listed(ui.talk, assistants)) talkTo(assistants[0]?.id ?? '');
  else if (!listed(ui.shown, assistants)) show(ui.talk);
}

/** Every chat's events, heard as long as the window: whether each works,
 * an assistant created or restored (listed, or told anew) or archived
 * (dropped), a hidden chat's notes (named), always; the rest, of the
 * chat shown. */
function hear(id: string, ev: Event) {
  if ('State' in ev) {
    const works = (ev.State.busy || ev.State.agents.length > 0) && listed(id);
    if (works !== ui.working.has(id)) {
      const working = new Set(ui.working);
      if (works) working.add(id);
      else working.delete(id);
      set({ working });
    }
  }
  if ('Assistant' in ev) {
    const it = ev.Assistant;
    const known = ui.assistants.some((a) => a.id === it.id);
    roster(known ? ui.assistants.map((a) => (a.id === it.id ? it : a)) : [...ui.assistants, it]);
    return;
  }
  if ('Gone' in ev) {
    roster(ui.assistants.filter((a) => a.id !== ev.Gone));
    return;
  }
  if (ui.shown !== id) {
    if ('Note' in ev) {
      const at = id.indexOf('/');
      const who = at < 0 ? named(id) : `${named(id.slice(0, at))} (asked by ${named(id.slice(at + 1))})`;
      note(`${who}: ${ev.Note}`);
    }
    return;
  }
  void then(model.hear(ui.model, ev));
}

/** The server reached again (null), or why not: a stale chat asked for
 * anew once it is. */
function reach(down: string | null) {
  set({ down });
  if (down === null && stale) resync();
}

/** Images join those waiting: files read as base64. */
export async function addPics(files: Iterable<File>) {
  const pics: Pic[] = [];
  for (const f of files) {
    if (!f.type.startsWith('image/')) continue;
    const b64 = await new Promise<string>((ok, no) => {
      const r = new FileReader();
      r.onload = () => ok(String(r.result).replace(/^data:[^,]*,/, ''));
      r.onerror = () => no(r.error);
      r.readAsDataURL(f);
    });
    pics.push({ b64, url: URL.createObjectURL(f) });
  }
  if (pics.length) set({ pics: [...ui.pics, ...pics] });
}

export function dropPic(k: number) {
  URL.revokeObjectURL(ui.pics[k]?.url ?? '');
  set({ pics: ui.pics.filter((_, i) => i !== k) });
}

/** The draft and the images waiting, said: gone from the box once sent,
 * back in it (before what was written since) if they may not have got
 * there. */
export async function submit() {
  const { draft, pics } = ui;
  const sent = say(draft, pics.map((p) => p.b64));
  if (!sent) return;
  set({ draft: '', pics: [] });
  if (await sent) {
    for (const p of pics) URL.revokeObjectURL(p.url);
    return;
  }
  set({ draft: ui.draft ? `${draft}\n\n${ui.draft}` : draft, pics: [...pics, ...ui.pics] });
  note("Your message may not have got there: it's back in the box.");
}

/** The tabs, the next one shown (Ctrl+Tab). */
export function nextTab() {
  const tabs = model.tabs(ui.model);
  const at = tabs.indexOf(ui.model.page);
  void act({ t: 'goto', name: tabs[(at + 1) % tabs.length] });
}

/** Start: hear the server, tick the clock, the sidebar as wide as the
 * window lets it be. */
export function start(): () => void {
  const stop = new AbortController();
  void link.listen({ roster, hear, lost: resync, reach }, stop.signal);
  const tick = setInterval(() => set({ now: Date.now() }), 60_000);
  const wide = window.matchMedia(WIDE);
  const fit = () => set({ side: wide.matches });
  wide.addEventListener('change', fit);
  return () => {
    stop.abort();
    clearInterval(tick);
    wide.removeEventListener('change', fit);
  };
}
