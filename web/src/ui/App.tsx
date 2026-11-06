/** @jsxImportSource octane */
// Acorn
// =====
// The window around the assistants' chats, in a browser (app/src/lib.rs):
// the sidebar (the assistants), a header (the sidebar's button, the
// assistant's name and who it is, an import's progress), the chat, and a
// footer (a note, the goal, today's cost, the images waiting, the message
// box, the tabs while agents work). You act by words alone; hands only
// look. The keys, all for looking: Esc goes back to your chat; Ctrl+Tab to
// the next tab; ⌘⇧F shows the overview, and again the chat; ⌘B hides the
// sidebar, and again shows it.

import { cn } from '@octanejs/shadcn/cn';
import { TooltipProvider } from '@octanejs/shadcn/base-ui/Tooltip';
import { useEffect } from 'octane';
import * as model from '../model.ts';
import * as store from '../store.ts';
import { progressShow, useHint, usdShow } from '../text.ts';
import { Chat } from './Chat.tsx';
import { Composer } from './Composer.tsx';
import { Side, SideChip } from './Side.tsx';
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
          void store.addPics(e.dataTransfer?.files ?? []);
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

/** The sidebar's button (while the sidebar is hidden), the assistant's
 * name (back to its chat) and who it is, an import's progress. */
function Header() {
  const ui = store.useUi();
  const me = store.me(ui);
  const importing = ui.model.state?.importing ?? null;
  return (
    <header class="flex h-[52px] shrink-0 items-center gap-3 border-b px-5">
      {ui.side ? null : <SideChip />}
      <button type="button" class="min-w-0 cursor-pointer truncate text-left" onClick={() => store.back()}>
        <span class="font-brand text-lg font-semibold">{me.name}</span>
        {me.about ? <span class="text-[13px] text-muted-foreground">{`  ·  ${me.about}`}</span> : null}
      </button>
      <span class="flex-1" />
      {importing ? (
        <Tip tip="Your past chats going into memory. Ask to pause or resume it." side="bottom">
          <span class="flex h-8 items-center rounded-lg bg-muted px-3 text-[13.5px] font-medium">{progressShow(importing)}</span>
        </Tip>
      ) : null}
    </header>
  );
}

/** The note shown, the goal, today's cost, the message box, the tabs. */
function Footer() {
  const ui = store.useUi();
  const s = ui.model.state;
  const today = s?.today && s.today.n > 0 ? s.today : null;
  const tabs = model.tabs(ui.model);
  const me = store.me(ui).name;
  return (
    <footer class="flex shrink-0 justify-center px-5 pt-2 pb-3">
      <div class="flex w-full max-w-[740px] flex-col gap-2">
        {ui.toast ? (
          <button
            type="button"
            class="cursor-pointer self-center rounded-xl bg-foreground px-4 py-2 text-[13.5px] text-background"
            onClick={() => store.set({ toast: null })}
          >
            {ui.toast}
          </button>
        ) : null}
        <div class="flex items-center gap-3 text-[12.5px] text-muted-foreground">
          {s?.goal ? (
            <Tip tip={`${s.goal}\nIt keeps working till a check finds this met. Say so to drop it.`}>
              <span class="min-w-0 truncate">{`Goal: ${s.goal}`}</span>
            </Tip>
          ) : null}
          <span class="flex-1" />
          {today ? (
            <Tip tip={`${useHint(today)}\nToday's calls in this chat: replies, summaries, agents. Ask for usage to see more.`}>
              <span class="tabular-nums">{`${usdShow(today.usd)} today`}</span>
            </Tip>
          ) : null}
        </div>
        <Composer />
        {tabs.length > 1 ? (
          <div class="flex flex-wrap gap-4">
            {tabs.map((name) => {
              const on = name === ui.model.page;
              return (
                <button
                  key={name ?? ''}
                  type="button"
                  class={cn('cursor-pointer text-[13px]', on ? 'font-semibold text-foreground' : 'text-muted-foreground')}
                  onClick={() => store.act({ t: 'goto', name })}
                >
                  {`${on ? '●' : '◯'} ${name ?? me}`}
                </button>
              );
            })}
          </div>
        ) : null}
      </div>
    </footer>
  );
}
