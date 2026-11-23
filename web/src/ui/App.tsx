/** @jsxImportSource octane */
// Acorn
// =====
// The window around the assistants' chats, in a browser (app/src/lib.rs):
// the sidebar (the assistants), a header (the sidebar's button, the
// assistant's name and who it is, an import's progress, the server not
// reached), the chat, and a footer (a note, what is pinned, today's cost, the
// images waiting, the message box, the tabs while agents work). You act
// by words alone; hands only look. The keys, all for looking: Esc goes
// back to your chat; Ctrl+Tab to the next tab; ⌘⇧F shows the overview,
// and again the chat; ⌘B hides the sidebar, and again shows it.

import { cn } from '@octanejs/shadcn/cn';
import { TooltipProvider } from '@octanejs/shadcn/base-ui/Tooltip';
import { useEffect } from 'octane';
import * as model from '../model.ts';
import * as store from '../store.ts';
import { progressShow, useHint, usdShow } from '../text.ts';
import { Chat } from './Chat.tsx';
import { Composer } from './Composer.tsx';
import { MobileAssistants, Side, SideChip } from './Side.tsx';
import { Tip } from './Tip.tsx';

export function App() {
  useEffect(() => store.start(), []);
  useEffect(() => {
    const keys = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (e.key === 'Escape' && !e.defaultPrevented) store.back();
      else if (e.ctrlKey && e.key === 'Tab') {
        e.preventDefault();
        store.nextTab();
      } else if (mod && e.shiftKey && e.key.toLowerCase() === 'f') {
        e.preventDefault();
        store.act({ t: 'fold', on: store.get().model.fold === null });
      } else if (mod && !e.shiftKey && e.key.toLowerCase() === 'b') {
        e.preventDefault();
        store.set({ side: !store.get().side });
      } else return;
    };
    window.addEventListener('keydown', keys);
    return () => window.removeEventListener('keydown', keys);
  }, []);

  return (
    <TooltipProvider delay={500}>
      <div
        class="flex h-dvh w-full bg-background text-foreground"
        onDragOver={(e: DragEvent) => e.preventDefault()}
        onDrop={(e: DragEvent) => {
          e.preventDefault();
          void store.addPics(e.dataTransfer?.files ?? []).catch((err) => store.note(String(err)));
        }}
      >
        <Side />
        <main class="flex min-w-0 flex-1 flex-col">
          <Header />
          <Chat />
          <Footer />
        </main>
      </div>
    </TooltipProvider>
  );
}

/** Desktop: sidebar toggle (when hidden), assistant name & about, status
 * badges, overview indicator, and today's cost. Mobile: centered Atelier
 * assistant switcher pill, with status badges neatly below when active. */
function Header() {
  const ui = store.useUi();
  const me = store.me(ui);
  const s = ui.model.state;
  const importing = s?.importing ?? null;
  const today = s?.today && s.today.n > 0 ? s.today : null;
  const overview = ui.model.fold !== null;
  return (
    <div class="shrink-0">
      <header class="relative z-20 flex h-[52px] items-center justify-center border-b border-border px-4 md:justify-between md:px-6">
        <MobileAssistants />

        <div class="hidden min-w-0 items-center gap-3 md:flex">
          {ui.side ? null : <SideChip />}
          <button type="button" class="flex min-w-0 cursor-pointer items-baseline gap-2.5 truncate text-left" onClick={() => store.back()}>
            <span class="font-serif text-[17px] font-medium">{me.name}</span>
            {me.about ? <span class="truncate text-[12.5px] text-muted-foreground">{me.about}</span> : null}
          </button>
        </div>

        <div class="hidden shrink-0 items-center gap-3 md:flex">
          {overview ? (
            <button
              type="button"
              onClick={() => store.back()}
              class="cursor-pointer font-serif text-[12px] italic text-muted-foreground transition-colors hover:text-foreground"
            >
              Overview
            </button>
          ) : null}
          {importing ? (
            <Tip tip="Your past chats going into memory. Ask to pause or resume it." side="bottom">
              <span class="flex h-7 items-center rounded-md bg-muted px-2.5 text-[12.5px] font-medium">{progressShow(importing)}</span>
            </Tip>
          ) : null}
          {ui.down ? (
            <Tip tip={`${ui.down}\nTrying again; what you send meanwhile comes back to the box.`} side="bottom">
              <span class="flex h-7 shrink-0 items-center rounded-md bg-foreground px-2.5 text-[12.5px] font-medium text-background">
                Can't reach the server
              </span>
            </Tip>
          ) : null}
          {today ? (
            <Tip tip={`${useHint(today)}\nToday's calls in this chat: replies, summaries, agents. Ask for usage to see more.`} side="bottom">
              <span class="font-mono text-[11.5px] tabular-nums text-muted-foreground">{`${usdShow(today.usd)} today`}</span>
            </Tip>
          ) : null}
        </div>
      </header>

      {importing || ui.down || overview ? (
        <div class="flex flex-wrap items-center justify-center gap-2 border-b border-border bg-muted/40 px-4 py-1.5 text-[12px] md:hidden">
          {overview ? (
            <button
              type="button"
              onClick={() => store.back()}
              class="cursor-pointer font-serif italic text-muted-foreground hover:text-foreground"
            >
              Overview · tap to return
            </button>
          ) : null}
          {importing ? (
            <span class="rounded-md bg-muted px-2 py-0.5 font-medium">{progressShow(importing)}</span>
          ) : null}
          {ui.down ? (
            <span class="rounded-md bg-foreground px-2 py-0.5 font-medium text-background">Can't reach the server</span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** Flat bottom writing sheet: toast note, what is pinned, composer, and live tabs. */
function Footer() {
  const ui = store.useUi();
  const pins = ui.model.state?.pins ?? [];
  const tabs = model.tabs(ui.model);
  const me = store.me(ui).name;
  return (
    <footer class="shrink-0 border-t border-border bg-background px-5 pt-2.5 pb-3.5 sm:px-7">
      <div class="mx-auto flex w-full max-w-[680px] flex-col gap-2">
        {ui.toast ? (
          <button
            type="button"
            class="cursor-pointer self-center rounded-xl bg-foreground px-4 py-2 text-[13px] text-background"
            onClick={() => store.note(null)}
          >
            {ui.toast}
          </button>
        ) : null}
        {pins.length ? (
          <Tip tip={`${pins.join('\n')}\nRead whole by the assistant on every call. Say so to unpin one.`}>
            <div class="truncate font-serif text-[12.5px] italic text-muted-foreground">
              {`Pinned${pins.length > 1 ? ` (${pins.length})` : ''}: ${pins[pins.length - 1]}`}
            </div>
          </Tip>
        ) : null}
        <Composer />
        {tabs.length > 1 ? (
          <div class="flex flex-wrap items-center gap-4 pt-0.5">
            {tabs.map((name) => {
              const on = name === ui.model.page;
              return (
                <button
                  key={name ?? ''}
                  type="button"
                  class={cn(
                    'flex cursor-pointer items-center gap-1.5 text-[12px] transition-colors',
                    on ? 'font-medium text-foreground' : 'text-muted-foreground hover:text-foreground',
                  )}
                  onClick={() => store.act({ t: 'goto', name })}
                >
                  <span class={cn('size-1.5 rounded-full', on ? 'bg-acorn-accent' : 'bg-muted-foreground/45')} />
                  <span>{name ?? me}</span>
                </button>
              );
            })}
          </div>
        ) : null}
      </div>
    </footer>
  );
}
