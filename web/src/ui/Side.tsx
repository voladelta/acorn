/** @jsxImportSource octane */
// The sidebar (app/src/side.rs): Acorn's name, then the assistants
// (Acorn, Max, then those they created, oldest first), each a row, who it
// is in its tooltip; the one you talk to has a bar and a fill, one at work
// (with you, or answering another) a dot. ⌘B (or the button atop it)
// hides it; on a narrow screen it floats over the chat.

import PanelLeft from '@octanejs/lucide/icons/panel-left';
import { Button } from '@octanejs/shadcn/base-ui/Button';
import { cn } from '@octanejs/shadcn/cn';
import * as link from '../link.ts';
import * as store from '../store.ts';
import type { Assistant } from '../wire.ts';
import { Tip } from './Tip.tsx';

export function SideChip() {
  return (
    <Tip tip="Hide or show the assistants (⌘B)" side="bottom">
      <Button variant="ghost" size="icon" aria-label="Hide or show the assistants" onClick={() => store.set({ side: !store.get().side })}>
        <PanelLeft />
      </Button>
    </Tip>
  );
}

export function Side() {
  const ui = store.useUi();
  if (!ui.side) return null;
  return (
    <>
      <div class="fixed inset-0 z-30 bg-black/20 md:hidden" onClick={() => store.set({ side: false })} />
      <aside class="fixed inset-y-0 left-0 z-40 flex w-[232px] shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground md:static">
        <div class="flex h-[52px] items-center gap-2 border-b border-sidebar-border pr-3 pl-5">
          <span class="text-xl">🌰</span>
          <span class="font-brand text-xl font-semibold">Acorn</span>
          <span class="flex-1" />
          <SideChip />
        </div>
        <nav class="flex-1 overflow-y-auto px-3 pb-4">
          <p class="px-2.5 pt-3 pb-1.5 text-[11px] font-semibold tracking-[0.6px] text-muted-foreground/70">ASSISTANTS</p>
          <div class="flex flex-col gap-0.5">
            {ui.assistants.map((a, k) => (
              <Person
                key={a.id}
                a={a}
                acorn={k === 0}
                on={ui.talk === a.id}
                works={[...ui.working].some((chat) => link.whose(chat) === a.id)}
              />
            ))}
          </div>
        </nav>
      </aside>
    </>
  );
}

/** An assistant: its mark (Acorn's, an acorn; another's, its initial),
 * its name (the one you talk to in ink, the rest muted), a dot while it
 * works. */
function Person(props: { a: Assistant; acorn: boolean; on: boolean; works: boolean }) {
  const { a, on } = props;
  const tip = a.about ? `${a.name} · ${a.about}` : a.name;
  return (
    <Tip tip={tip} side="right">
      <button
        type="button"
        class={cn(
          'relative flex h-9 w-full cursor-pointer items-center gap-2.5 rounded-lg px-2.5 text-left transition-colors hover:bg-sidebar-accent',
          on && 'bg-sidebar-accent',
        )}
        onClick={() => {
          store.talkTo(a.id);
          if (store.narrow()) store.set({ side: false });
        }}
      >
        <span class={cn('absolute top-[9px] left-0 h-[18px] w-[3px] rounded-sm bg-foreground', !on && 'opacity-0')} />
        <span class={cn('flex size-[22px] items-center justify-center rounded-md', on ? 'bg-background' : 'bg-muted')}>
          {props.acorn ? (
            <span class="text-[15px]">🌰</span>
          ) : (
            <span class="text-xs font-semibold text-muted-foreground">{a.name.charAt(0).toUpperCase()}</span>
          )}
        </span>
        <span class={cn('flex-1 truncate text-sm font-medium', on ? 'text-foreground' : 'text-muted-foreground')}>{a.name}</span>
        <span class={cn('size-[7px] rounded-full bg-foreground', !props.works && 'opacity-0')} />
      </button>
    </Tip>
  );
}
