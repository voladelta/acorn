/** @jsxImportSource octane */
// The page shown: its items (a part's rows each, or an agent's entry),
// then what streams, in a list that keeps to its end as it grows while
// it is at its end (within 120px). A chat shown from scratch starts at
// its end.

import { createResizeObserver, memo, useEffect, useLayoutEffect, useMemo, useRef } from 'octane';
import * as model from '../model.ts';
import type { Row } from '../model.ts';
import * as store from '../store.ts';
import { RowView } from './Rows.tsx';

const FOLLOW = 120;

export function Chat() {
  const ui = store.useUi();
  const m = ui.model;
  // each item keyed by its chat too: another's chat never shows in its rows
  const items = useMemo(
    () => model.body(m).map((it) => ({ ...it, key: `${ui.shown}:${it.key}` })),
    [ui.shown, m.parts, m.turn, m.whole, m.nodes, m.open, m.minds, m.fold, m.page, m.pages, m.head, m.state?.agents],
  );
  const tail = model.tail(m);
  const box = useRef<HTMLDivElement | null>(null);
  const inner = useRef<HTMLDivElement | null>(null);
  const atEnd = useRef(true);

  const stick = () => {
    const el = box.current;
    if (el && atEnd.current) el.scrollTop = el.scrollHeight;
  };

  // a chat shown anew starts at its end
  useEffect(() => {
    const f = () => {
      atEnd.current = true;
    };
    store.shownHooks.add(f);
    return () => {
      store.shownHooks.delete(f);
    };
  }, []);

  // what grows (rows, images loading) keeps the end in view
  useLayoutEffect(() => {
    const el = inner.current;
    if (!el) return;
    stick();
    const observer = createResizeObserver(stick);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(stick);

  return (
    <div
      ref={box}
      class="min-h-0 flex-1 overflow-y-auto"
      onScroll={() => {
        const el = box.current!;
        atEnd.current = el.scrollHeight - el.scrollTop - el.clientHeight < FOLLOW;
      }}
    >
      <div ref={inner} class="mx-auto flex w-full max-w-[740px] flex-col gap-3.5 px-5 pt-6 pb-7">
        {items.map((it) => (
          <Item key={it.key} rows={it.rows} chat={ui.shown} now={ui.now} />
        ))}
        {tail.map((row, k) => (
          <RowView key={`tail${k}`} row={row} chat={ui.shown} now={ui.now} />
        ))}
      </div>
    </div>
  );
}

/** A part's rows: drawn again only when they change. */
const Item = memo(
  function Item(props: { rows: Row[]; chat: string; now: number }) {
    return (
      <div class="flex flex-col gap-3.5">
        {props.rows.map((row, k) => (
          <RowView key={k} row={row} chat={props.chat} now={props.now} />
        ))}
      </div>
    );
  },
  (a, b) => a.chat === b.chat && a.now === b.now && JSON.stringify(a.rows) === JSON.stringify(b.rows),
);
