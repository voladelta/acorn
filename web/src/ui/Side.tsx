/** @jsxImportSource octane */
// The sidebar (app/src/side.rs): on desktop (md+), a borderless paper
// sidebar listing the assistants (Acorn, Max, then those they created,
// oldest first), each with its tooltip; ⌘B (or the button atop it) hides
// it. On mobile (<md), assistants live in the centered header switcher
// (`MobileAssistants`) instead of a slide-over drawer.

import ChevronDown from '@octanejs/lucide/icons/chevron-down';
import PanelLeft from '@octanejs/lucide/icons/panel-left';
import { Button } from '@octanejs/shadcn/base-ui/Button';
import { cn } from '@octanejs/shadcn/cn';
import { useEffect, useState } from 'octane';
import * as link from '../link.ts';
import * as store from '../store.ts';
import { usdShow } from '../text.ts';
import type { Assistant } from '../wire.ts';
import { Tip } from './Tip.tsx';

export function AcornGlyph(props: { class?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="none" class={props.class ?? 'size-4'} aria-hidden="true">
      <path
        d="M12 3c.5 0 1 .4 1.1.9l.3 1.3c3 .4 5.3 2.4 5.6 5.3H5c.3-2.9 2.6-4.9 5.6-5.3l.3-1.3c.1-.5.6-.9 1.1-.9Z"
        fill="currentColor"
        opacity="0.88"
      />
      <path
        d="M6.5 11.8h11c0 4.6-2.4 8.2-5.5 9.2-3.1-1-5.5-4.6-5.5-9.2Z"
        fill="currentColor"
        opacity="0.42"
      />
    </svg>
  );
}

export function SideChip() {
  return (
    <Tip tip="Hide or show the assistants (⌘B)" side="bottom">
      <Button
        variant="ghost"
        size="icon"
        className="text-muted-foreground hover:text-foreground"
        aria-label="Hide or show the assistants"
        onClick={() => store.set({ side: !store.get().side })}
      >
        <PanelLeft class="size-4" />
      </Button>
    </Tip>
  );
}

export function Side() {
  const ui = store.useUi();
  if (!ui.side) return null;
  return (
    <aside class="hidden w-[224px] shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground md:flex">
      <div class="flex h-[52px] items-center gap-2 pr-3 pl-5">
        <AcornGlyph class="size-4 text-acorn-accent" />
        <span class="font-serif text-[18px] font-medium tracking-[-0.01em]">Acorn</span>
        <span class="flex-1" />
        <SideChip />
      </div>
      <nav class="flex-1 overflow-y-auto px-4 pt-3 pb-4">
        <p class="px-2 pb-2 text-[10.5px] font-medium tracking-[0.14em] uppercase text-muted-foreground/75">Assistants</p>
        <div class="flex flex-col gap-0.5">
          {ui.assistants.map((a) => (
            <Person
              key={a.id}
              a={a}
              on={ui.talk === a.id}
              works={[...ui.working].some((chat) => link.whose(chat) === a.id)}
            />
          ))}
        </div>
      </nav>
    </aside>
  );
}

/** An assistant row in the desktop monograph sidebar. */
function Person(props: { a: Assistant; on: boolean; works: boolean }) {
  const { a, on } = props;
  const tip = a.about ? `${a.name} · ${a.about}` : a.name;
  return (
    <Tip tip={tip} side="right">
      <button
        type="button"
        class={cn(
          'flex w-full cursor-pointer items-center justify-between rounded-lg px-2 py-2 text-left transition-colors',
          on ? 'font-medium text-foreground' : 'text-muted-foreground hover:bg-sidebar-accent/70 hover:text-foreground',
        )}
        onClick={() => store.talkTo(a.id)}
      >
        <span class="flex min-w-0 items-center gap-2">
          <span class={cn('size-1.5 shrink-0 rounded-full bg-acorn-accent transition-opacity', !on && 'opacity-0')} />
          <span class="truncate font-serif text-[16px] leading-snug">{a.name}</span>
        </span>
        {props.works ? <span class="acorn-pulse size-1.5 shrink-0 rounded-full bg-acorn-accent" /> : null}
      </button>
    </Tip>
  );
}

/** Centered Atelier-style assistant switcher for mobile (<md). */
export function MobileAssistants() {
  const ui = store.useUi();
  const me = store.me(ui);
  const [open, setOpen] = useState(false);
  const s = ui.model.state;
  const today = s?.today && s.today.n > 0 ? s.today : null;
  const busyIds = new Set([...ui.working].map((chat) => link.whose(chat)));
  const othersBusy = ui.assistants.some((a) => a.id !== me.id && busyIds.has(a.id));

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        setOpen(false);
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [open]);

  return (
    <div class="relative md:hidden">
      <button
        type="button"
        aria-expanded={open}
        aria-haspopup="listbox"
        onClick={() => setOpen(!open)}
        class="flex cursor-pointer items-center gap-2 rounded-full border border-border bg-card px-3.5 py-1.5 shadow-xs transition-colors hover:bg-accent/50"
      >
        <AcornGlyph class="size-3.5 text-acorn-accent" />
        <span class="max-w-[180px] truncate font-serif text-[16px] font-medium leading-none">{me.name}</span>
        {othersBusy || busyIds.has(me.id) ? (
          <span class="acorn-pulse size-1.5 shrink-0 rounded-full bg-acorn-accent" />
        ) : null}
        <ChevronDown class={cn('size-3.5 text-muted-foreground transition-transform', open && 'rotate-180')} />
      </button>

      {open ? (
        <>
          <div class="fixed inset-0 z-20" onClick={() => setOpen(false)} />
          <div
            role="listbox"
            class="absolute top-full left-1/2 z-30 mt-2 w-[280px] max-w-[calc(100vw-2rem)] -translate-x-1/2 rounded-2xl border border-border bg-popover p-1.5 text-popover-foreground shadow-xl"
          >
            <div class="flex max-h-[60dvh] flex-col gap-0.5 overflow-y-auto">
              {ui.assistants.map((a, k) => {
                const active = a.id === me.id;
                const works = busyIds.has(a.id);
                return (
                  <button
                    key={a.id}
                    type="button"
                    role="option"
                    aria-selected={active}
                    onClick={() => {
                      if (active) store.back();
                      else store.talkTo(a.id);
                      setOpen(false);
                    }}
                    class={cn(
                      'flex w-full cursor-pointer items-center gap-3 rounded-xl px-3 py-2 text-left transition-colors',
                      active ? 'bg-muted' : 'hover:bg-accent/60',
                    )}
                  >
                    <span
                      class={cn(
                        'flex size-7 shrink-0 items-center justify-center rounded-full text-xs font-medium',
                        active ? 'bg-acorn-accent text-background' : 'bg-muted text-muted-foreground',
                      )}
                    >
                      {k === 0 ? <AcornGlyph class="size-3.5" /> : a.name.charAt(0).toUpperCase()}
                    </span>
                    <div class="min-w-0 flex-1">
                      <div class="flex items-center justify-between gap-2">
                        <span class="truncate font-serif text-[15.5px] font-medium">{a.name}</span>
                        {works ? <span class="acorn-pulse size-1.5 shrink-0 rounded-full bg-acorn-accent" /> : null}
                      </div>
                      {a.about ? <div class="truncate text-[11.5px] text-muted-foreground">{a.about}</div> : null}
                    </div>
                  </button>
                );
              })}
            </div>
            {me.about || today ? (
              <div class="mt-1 flex items-center justify-between gap-3 border-t border-border px-3 pt-2 pb-1 text-[11px] text-muted-foreground">
                <span class="min-w-0 truncate">{me.about}</span>
                {today ? <span class="shrink-0 font-mono tabular-nums">{`${usdShow(today.usd)} today`}</span> : null}
              </div>
            ) : null}
          </div>
        </>
      ) : null}
    </div>
  );
}
