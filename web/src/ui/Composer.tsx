/** @jsxImportSource octane */
// The message box (app/src/lib.rs's composer): Return sends (Shift+Return
// a new line); images, videos, sounds and PDFs come by the attach button,
// a paste or a drop. Sent while the assistant works, a message cuts in.
// An `@` word being typed shows the other assistants it may name, over the
// box: ↑ and ↓ pick one, Return or Tab takes it (Return sends a word
// already a name), a click takes the one clicked, Esc closes it for that
// word: till the caret is out of it (moved off, the word gone, the
// message sent).

import ArrowUp from '@octanejs/lucide/icons/arrow-up';
import Plus from '@octanejs/lucide/icons/plus';
import X from '@octanejs/lucide/icons/x';
import { Button } from '@octanejs/shadcn/base-ui/Button';
import { cn } from '@octanejs/shadcn/cn';
import { useEffect, useLayoutEffect, useRef, useState } from 'octane';
import * as store from '../store.ts';
import { mentionAt, mentionDone, namesFor } from '../text.ts';
import { Tip } from './Tip.tsx';

/** The field, to focus from elsewhere (a key typed on the page). */
export const field = { current: null as HTMLTextAreaElement | null };

export function Composer() {
  const ui = store.useUi();
  const me = store.me(ui);
  const [caret, setCaret] = useState(0);
  const [pick, setPick] = useState(0);
  const [hushed, setHushed] = useState<number | null>(null);
  const files = useRef<HTMLInputElement | null>(null);
  const ref = useRef<HTMLTextAreaElement | null>(null);
  // where the caret goes once the field shows the draft set
  const caretTo = useRef<number | null>(null);
  const ready = ui.draft.trim() !== '' || ui.pics.length > 0;

  // The `@` word's start and the assistants it may name, with whether it is one already.
  const others = ui.assistants.map((a) => a.name).filter((n) => n !== me.name);
  const at = mentionAt(ui.draft, caret);
  const found = at ? namesFor(at[1], others) : [];
  const naming = at && found.length && hushed !== at[0] ? { start: at[0], found, whole: found.some((n) => n.toLowerCase() === at[1].toLowerCase()) } : null;
  const picked = naming ? Math.min(pick, naming.found.length - 1) : 0;

  // a word closed, once the caret is out of it, is closed no more
  const atStart = at?.[0] ?? null;
  useEffect(() => {
    if (hushed !== null && hushed !== atStart) setHushed(null);
  }, [hushed, atStart]);

  useEffect(() => {
    field.current = ref.current;
    ref.current?.focus();
  }, []);

  useLayoutEffect(() => {
    const to = caretTo.current;
    if (to === null || !ref.current) return;
    caretTo.current = null;
    ref.current.focus();
    ref.current.setSelectionRange(to, to);
  });

  const take = (name: string) => {
    if (!naming) return;
    const [text, to] = mentionDone(ui.draft, naming.start, caret, name);
    caretTo.current = to;
    setCaret(to);
    store.set({ draft: text });
  };

  const submit = () => {
    if (naming && !naming.whole) {
      take(naming.found[picked]);
      return;
    }
    void store.submit();
    setCaret(0);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.isComposing) return;
    if (naming) {
      const n = naming.found.length;
      if (e.key === 'ArrowUp') return void (e.preventDefault(), setPick((picked + n - 1) % n));
      if (e.key === 'ArrowDown') return void (e.preventDefault(), setPick((picked + 1) % n));
      if (e.key === 'Tab') return void (e.preventDefault(), take(naming.found[picked]));
      if (e.key === 'Escape') return void (e.preventDefault(), e.stopPropagation(), setHushed(naming.start));
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  };

  return (
    <div class="flex flex-col gap-2">
      {ui.pics.length ? (
        <div class="flex flex-wrap gap-2">
          {ui.pics.map((p, k) => (
            <div key={p.url} class="group relative">
              {p.image ? (
                <img src={p.url} alt={p.name} class="size-14 rounded-lg border object-cover" />
              ) : (
                <div title={p.name} class="flex h-14 max-w-[160px] items-center gap-1.5 rounded-lg border bg-muted px-2.5 text-xs">
                  <span aria-hidden="true">📄</span>
                  <span class="truncate">{p.name}</span>
                </div>
              )}
              <button
                type="button"
                aria-label={`Drop ${p.name || `file ${k + 1}`}`}
                class="absolute -top-1.5 -right-1.5 flex size-5 cursor-pointer items-center justify-center rounded-full bg-foreground text-background"
                onClick={() => store.dropPic(k)}
              >
                <X class="size-3" />
              </button>
            </div>
          ))}
        </div>
      ) : null}
      <div class="relative flex items-end gap-2 rounded-3xl border bg-card py-[7px] pr-[7px] pl-1.5 shadow-xs dark:border-input">
        {naming ? (
          <div role="listbox" class="absolute bottom-full left-4 z-20 mb-1.5 min-w-[220px] rounded-[10px] border bg-popover py-1 text-popover-foreground shadow-md">
            {naming.found.map((name, k) => (
              <div
                key={name}
                role="option"
                aria-selected={k === picked}
                class={cn('flex cursor-pointer gap-2 px-3 py-1.5 hover:bg-accent', k === picked && 'bg-accent')}
                onMouseDown={(e: MouseEvent) => {
                  e.preventDefault();
                  take(name);
                }}
              >
                <span class="text-sm font-medium">{`@${name}`}</span>
                <span class="truncate text-[13px] text-muted-foreground">{ui.assistants.find((a) => a.name === name)?.about ?? ''}</span>
              </div>
            ))}
          </div>
        ) : null}
        <Tip tip="Attach images, videos, sounds or PDFs">
          <Button variant="ghost" size="icon-lg" className="rounded-full text-muted-foreground" aria-label="Attach files" onClick={() => files.current?.click()}>
            <Plus class="size-5" />
          </Button>
        </Tip>
        <input
          ref={files}
          type="file"
          accept={store.ATTACHABLE}
          multiple
          hidden
          onChange={(e: Event) => {
            const input = e.currentTarget as HTMLInputElement;
            void store.addPics(input.files ?? []);
            input.value = '';
          }}
        />
        <textarea
          ref={ref}
          rows={1}
          aria-label={`Message ${me.name}`}
          placeholder={`Message ${me.name}`}
          class="max-h-80 min-h-9 flex-1 resize-none bg-transparent py-[7px] text-base outline-none [field-sizing:content] placeholder:text-muted-foreground/70"
          value={ui.draft}
          onInput={(e: Event) => {
            const t = e.currentTarget as HTMLTextAreaElement;
            store.set({ draft: t.value });
            setCaret(t.selectionEnd);
            setPick(0);
          }}
          onSelect={(e: Event) => setCaret((e.currentTarget as HTMLTextAreaElement).selectionEnd)}
          onKeyDown={onKeyDown}
          onPaste={(e: ClipboardEvent) => {
            const pics = [...(e.clipboardData?.files ?? [])].filter((f) => f.type.startsWith('image/'));
            if (pics.length) {
              if (!e.clipboardData?.getData('text')) e.preventDefault();
              void store.addPics(pics);
            }
          }}
        />
        <Button
          size="icon-lg"
          className={cn('rounded-full', !ready && 'bg-border text-background hover:bg-border')}
          aria-label="Send"
          onClick={submit}
        >
          <ArrowUp class="size-[18px]" />
        </Button>
      </div>
    </div>
  );
}
