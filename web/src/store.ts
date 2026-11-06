// Store
// =====
// What every part of the window shares (app/src/lib.rs's `Ui`): the model
// of the chat shown, the assistants, the one you talk to, the chat shown
// (its chat with you, or a thread it asked in), the chats at work,
// whether the sidebar shows, the note shown, the message being written
// and the images waiting, the clock. One state, never changed in place;
// components read it with `useUi`, and the acts below make it anew.

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
  toast: string | null;
  draft: string;
  pics: Pic[];
  now: number;
}

let ui: Ui = {
  model: model.empty(),
  assistants: [],
  talk: '',
  shown: '',
  working: new Set(),
  side: typeof window === 'undefined' || window.matchMedia('(min-width: 768px)').matches,
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

/** Something to do when a chat is shown from scratch (the list scrolls
 * to its top). */
export const shownHooks = new Set<() => void>();

/** The assistant you talk to. */
export function me(u: Ui = ui): Assistant {
  return (
    u.assistants.find((a) => a.id === u.talk) ??
    u.assistants[0] ?? { id: u.talk, name: 'Acorn', about: '' }
  );
}

/** An assistant's name, by id. */
export const named = (id: string) => ui.assistants.find((a) => a.id === id)?.name ?? id;

/** A person's act, done. */
export function act(a: Act) {
  then(model.act(ui.model, a));
}

/** A command to the shown chat's engine. */
export const send = (cmd: Command) => link.send(ui.shown, cmd);

function then({ m, cmds, notes }: model.Then) {
  if (m !== ui.model) set({ model: m });
  for (const c of cmds) send(c);
  if (notes.length) note(notes[notes.length - 1]);
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
 * shown again: back to its end. */
export function show(id: string) {
  if (ui.shown === id) {
    act({ t: 'goto', name: null });
    return;
  }
  set({ model: fresh(id), shown: id });
  for (const f of shownHooks) f();
  send('Attach');
}

/** An assistant to talk to: its chat with you shown. */
export function talkTo(id: string) {
  set({ talk: id });
  show(id);
}

/** Back to your chat with the assistant, from whatever you look at: a
 * thread, an agent's page, the overview. */
export function back() {
  if (ui.shown !== ui.talk) {
    show(ui.talk);
  } else {
    act({ t: 'goto', name: null });
    act({ t: 'fold', on: false });
  }
}

/** Your words, to the assistant you talk to, its chat shown: sent while
 * it works, they cut in; `@Name` alone, that assistant's chat shown
 * instead. Whether they went. */
export function say(text: string, images: string[]): boolean {
  if (!text.trim() && !images.length) return false;
  if (!images.length) {
    const name = mentionAlone(
      text,
      ui.assistants.map((a) => a.name),
    );
    const a = name && ui.assistants.find((x) => x.name === name);
    if (a) {
      talkTo(a.id);
      return true;
    }
  }
  if (ui.shown !== ui.talk) show(ui.talk);
  act({ t: 'send', text, images });
  return true;
}

/** A name in the chat, opened: an assistant's, its thread with the one
 * you talk to; an agent's, its page. */
export function openName(name: string) {
  const a = ui.assistants.find((x) => x.name === name);
  if (a) show(`${a.id}/${ui.talk}`);
  else act({ t: 'goto', name });
}

let noteTimer: ReturnType<typeof setTimeout> | undefined;

/** A note, shown a while. */
export function note(text: string) {
  set({ toast: text });
  clearTimeout(noteTimer);
  noteTimer = setTimeout(() => {
    if (ui.toast === text) set({ toast: null });
  }, 6000);
}

/** Every chat's events, heard as long as the window: whether each works,
 * an assistant created or restored (listed) or archived (dropped), a
 * hidden chat's notes (named), always; the rest, of the chat shown. */
export function hear(id: string, ev: Event) {
  if ('State' in ev) {
    const works = ev.State.busy || ev.State.agents.length > 0;
    if (works !== ui.working.has(id)) {
      const working = new Set(ui.working);
      if (works) working.add(id);
      else working.delete(id);
      set({ working });
    }
  }
  if ('Assistant' in ev) {
    const made = ev.Assistant;
    const first = !ui.talk;
    if (!ui.assistants.some((a) => a.id === made.id)) set({ assistants: [...ui.assistants, made] });
    if (first) talkTo(made.id);
    return;
  }
  if (!id) {
    // the link's own notes: events lost, the chat shown told anew
    if ('Note' in ev) {
      if (ev.Note === link.LOST) send('Attach');
      else note(ev.Note);
    }
    return;
  }
  if ('Gone' in ev) {
    const gone = ev.Gone;
    const of = (chat: string) => link.whose(chat) === gone || chat.endsWith(`/${gone}`);
    set({
      assistants: ui.assistants.filter((a) => a.id !== gone),
      working: new Set([...ui.working].filter((c) => !of(c))),
    });
    if (ui.talk === gone || of(ui.shown)) talkTo(ui.assistants[0]?.id ?? '');
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
  then(model.hear(ui.model, ev));
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

/** The draft and the images waiting, said: gone once sent. */
export function submit() {
  if (say(ui.draft, ui.pics.map((p) => p.b64))) {
    for (const p of ui.pics) URL.revokeObjectURL(p.url);
    set({ draft: '', pics: [] });
  }
}

/** The tabs, the next one shown (Ctrl+Tab). */
export function nextTab() {
  const tabs = model.tabs(ui.model);
  const at = tabs.indexOf(ui.model.page);
  act({ t: 'goto', name: tabs[(at + 1) % tabs.length] });
}

/** Start: hear the server, tick the clock. */
export function start(): () => void {
  const stop = new AbortController();
  void link.listen(hear, stop.signal);
  const tick = setInterval(() => set({ now: Date.now() }), 60_000);
  return () => {
    stop.abort();
    clearInterval(tick);
  };
}
