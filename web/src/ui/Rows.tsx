/** @jsxImportSource octane */
// Rows
// ====
// The chat as drawn (app/src/rows.rs): a block is a summary on the
// block's fill, its count of messages at its left (a click opens it into
// its halves); an open block, its bar over its halves in a frame; the
// user's words in a bubble at the right; a report framed, its agent's or
// assistant's name on its edge (a click: its page, or the thread); the
// assistant's words bare, as Markdown, a reply cut off ending so;
// thinking and notes faint; a tool call on its fill, its emoji and name
// first; a result as a file's lines, numbered, a diff's in green and red.

import Markdown, { defaultUrlTransform } from '@octanejs/markdown';
import remarkGfm from 'remark-gfm';
import * as callText from '../call.ts';
import type { Call } from '../call.ts';
import * as link from '../link.ts';
import type { Row } from '../model.ts';
import * as store from '../store.ts';
import { cutOff, linkNames, nameRuns, type Out, output, useHint, useText } from '../text.ts';
import type { Kind } from '../wire.ts';
import { Tip } from './Tip.tsx';

/** Each tool's emoji; any other's is 🔧. */
const GLYPHS: Record<string, string> = {
  zoom: '🔍',
  date: '📅',
  list_skills: '🧰',
  load_skill: '🧰',
  bash: '⌨️',
  read: '📖',
  write: '📄',
  edit: '📝',
  web_search: '🌐',
  web_fetch: '🔗',
  luau: '🌙',
  delegate: '🤖',
  hire: '🤖',
  spawn: '🤖',
  tell: '💬',
  fire: '✋',
  stop: '✋',
  create_assistant: '🐿️',
  list_assistants: '👥',
  search_assistants: '👥',
  ask_assistant: '🗣️',
  goal: '🎯',
  usage: '💵',
  import: '📥',
  export: '💾',
  tree: '🌳',
};

export function RowView(props: { row: Row; chat: string; now: number }) {
  const r = props.row;
  switch (r.t) {
    case 'cap':
      return <p class="font-serif text-[14.5px] italic text-muted-foreground">{r.text}</p>;
    case 'use':
      return (
        <Tip tip={useHint(r.spend)}>
          <p class="-mt-1.5 w-fit font-mono text-xs tabular-nums text-muted-foreground/70">{useText(r.spend, props.now)}</p>
        </Tip>
      );
    case 'sum':
      return (
        <Tip tip={`${r.seg[1]} messages, summarized: click to open`}>
          <div
            role="button"
            tabIndex={0}
            class="group flex cursor-pointer flex-col gap-1 border-b border-border py-3 text-left transition-opacity first:border-t hover:opacity-80 sm:flex-row sm:items-baseline sm:gap-5"
            onClick={() => store.act({ t: 'open', seg: r.seg })}
            onKeyDown={(e: KeyboardEvent) => e.key === 'Enter' && store.act({ t: 'open', seg: r.seg })}
          >
            <span class="shrink-0 font-mono text-[11px] tabular-nums text-muted-foreground sm:w-12">
              {`${r.seg[1]} msg`}
            </span>
            {r.call ? (
              <CallHead call={r.call} folded />
            ) : (
              <span class="flex-1 font-serif text-[15.5px] leading-[1.6]">{r.text}</span>
            )}
          </div>
        </Tip>
      );
    case 'box':
      return (
        <div class="flex flex-col gap-3 border-l border-border pl-4 sm:ml-12 sm:pl-5">
          <button
            type="button"
            class="cursor-pointer border-b border-border pb-1.5 text-left font-serif text-[13.5px] italic text-muted-foreground transition-colors hover:text-foreground"
            onClick={() => store.act({ t: 'shut', seg: r.seg })}
          >
            {r.text}
          </button>
          {r.rows.map((row, k) => (
            <RowView key={k} row={row} chat={props.chat} now={props.now} />
          ))}
        </div>
      );
    case 'msg':
      return <Message kind={r.kind} who={r.who} text={r.text} call={r.call} images={r.images} chat={props.chat} />;
  }
}

function Message(props: { kind: Kind; who: string; text: string; call: Call | null; images: string[]; chat: string }) {
  const { kind, who, text } = props;
  const pics = props.images.length ? <Pictures chat={props.chat} names={props.images} big={kind === 'unii'} /> : null;
  switch (kind) {
    case 'user':
      return (
        <div class="max-w-[85%] self-end rounded-2xl bg-muted px-4 py-2.5 sm:max-w-[540px]">
          <Plain text={text} />
          {pics}
        </div>
      );
    case 'work':
      return (
        <div class="flex flex-col gap-1 border-l-2 border-acorn-accent/60 py-0.5 pl-4">
          <button
            type="button"
            class="w-fit cursor-pointer text-[11px] font-medium tracking-[0.1em] uppercase text-muted-foreground transition-colors hover:text-foreground hover:underline"
            onClick={() => store.openName(who)}
          >
            {who}
          </button>
          <div class="font-serif text-[15.5px] leading-[1.65]">
            <Plain text={text} />
          </div>
        </div>
      );
    case 'tool': {
      const c = props.call ?? { name: '', head: text, blocks: [] };
      return (
        <div class="flex flex-col gap-1.5 border-l border-border pl-3.5">
          <CallHead call={c} />
          {c.blocks.map((b, k) => {
            const outs = output(b.x);
            if (outs.length === 1 && !outs[0].file) return <FileOut key={k} out={outs[0]} />;
            return (
              <Framed key={k} name={b.k || null}>
                <pre class="font-mono text-[12.5px] leading-[19px] whitespace-pre-wrap break-words">{b.x}</pre>
              </Framed>
            );
          })}
        </div>
      );
    }
    case 'echo':
      return (
        <div class="flex flex-col gap-2">
          {output(text).map((o, k) => (
            <FileOut key={k} out={o} />
          ))}
        </div>
      );
    case 'mind':
    case 'note': {
      const [said, cut] = cutOff(text);
      return (
        <div class="flex flex-col gap-1">
          {who ? <span class="text-[11px] font-medium tracking-[0.08em] uppercase text-muted-foreground/75">{who}</span> : null}
          <div class="md font-serif text-[14.5px] leading-[1.65] italic text-muted-foreground">
            <Md text={said} />
          </div>
          {cut ? <CutMark /> : null}
        </div>
      );
    }
    case 'unii': {
      const [said, cut] = cutOff(text);
      return (
        <div class="flex flex-col">
          <div class="md font-serif text-[16.5px] leading-[1.7]">
            <Md text={said} />
          </div>
          {cut ? <CutMark /> : null}
          {pics}
        </div>
      );
    }
  }
}

/** Where a message cut in on what was being said or thought. */
function CutMark() {
  return <p class="font-serif text-[14.5px] italic text-muted-foreground">— cut off</p>;
}

/** Links: an agent's name opens its page; any other, a new tab. */
const urlTransform = (url: string) => (url.startsWith('agent:') ? url : defaultUrlTransform(url));

const components = {
  a({ href, children }: { href?: string; children?: unknown }) {
    if (href?.startsWith('agent:')) {
      const name = href.slice('agent:'.length);
      return (
        <a
          href="#"
          class="underline"
          onClick={(e: MouseEvent) => {
            e.preventDefault();
            store.openName(name);
          }}
        >
          {children}
        </a>
      );
    }
    return (
      <a href={href} target="_blank" rel="noreferrer" class="underline underline-offset-2">
        {children}
      </a>
    );
  },
};

/** Markdown, each agent's name a link to its page. */
function Md(props: { text: string }) {
  return (
    <Markdown remarkPlugins={[remarkGfm]} urlTransform={urlTransform} components={components}>
      {linkNames(props.text)}
    </Markdown>
  );
}

/** Text as it is: paragraphs and lines, each agent's name a link. */
function Plain(props: { text: string }) {
  return (
    <p class="text-base leading-[1.55] whitespace-pre-wrap break-words">
      {nameRuns(props.text).map((run, k) =>
        run.name ? (
          <button key={k} type="button" class="cursor-pointer underline" onClick={() => store.openName(run.name!)}>
            {run.text}
          </button>
        ) : (
          <span key={k}>{run.text}</span>
        ),
      )}
    </p>
  );
}

/** A tool call's head: its emoji, its name in bold, then the call. */
function CallHead(props: { call: Call; folded?: boolean }) {
  const c = props.call;
  const head = props.folded ? callText.line(c) : c.head;
  return (
    <p class="flex-1 font-mono text-[12.5px] leading-5 break-words whitespace-pre-wrap">
      <span class="text-[15px]">{`${GLYPHS[c.name] ?? '🔧'}  `}</span>
      <b>{`${c.name} `}</b>
      {head}
    </p>
  );
}

/** Lines in a frame, a name on its top edge. */
function Framed(props: { name: string | null; children: unknown }) {
  return (
    <div class="flex flex-col gap-1 overflow-x-auto rounded-lg border border-border bg-muted/50 px-3 py-2">
      {props.name ? <span class="font-mono text-[11.5px] text-muted-foreground">{props.name}</span> : null}
      {props.children}
    </div>
  );
}

/** A result's file: its lines numbered, a diff's marked and colored. */
function FileOut(props: { out: Out }) {
  const o = props.out;
  if (o.file) {
    const width = String(o.n + o.x.length).length;
    return (
      <Framed name={o.k}>
        <pre class="font-mono text-[12px] leading-[19px]">
          {o.x.map((l, k) => (
            <span key={k} class="block">
              <span class="text-muted-foreground/70 select-none">{`${String(o.n + k).padStart(width)} `}</span>
              {l}
            </span>
          ))}
        </pre>
      </Framed>
    );
  }
  const width = String(Math.max(1, ...o.x.flat().map((l) => l.n))).length;
  return (
    <Framed name={o.k}>
      <pre class="font-mono text-[12px] leading-[19px]">
        {o.x.map((hunk, h) => (
          <span key={h}>
            {h > 0 ? <span class="block text-muted-foreground/70">{'⋯'.padStart(width)}</span> : null}
            {hunk.map((l, k) => (
              <span
                key={k}
                class={
                  l.s === '+'
                    ? 'block bg-diff-add-bg text-diff-add'
                    : l.s === '-'
                      ? 'block bg-diff-del-bg text-diff-del'
                      : 'block text-muted-foreground'
                }
              >
                <span class="text-muted-foreground/70 select-none">{`${String(l.n).padStart(width)} `}</span>
                {`${l.s}${l.x}`}
              </span>
            ))}
          </span>
        ))}
      </pre>
    </Framed>
  );
}

/** A message's images, each opened by a click. */
function Pictures(props: { chat: string; names: string[]; big?: boolean }) {
  return (
    <div class="mt-2 flex flex-wrap gap-2">
      {props.names.map((name) => {
        const url = link.image(props.chat, name);
        switch (link.mediaOf(name)) {
          case 'image':
            return (
              <a key={name} href={url} target="_blank" rel="noreferrer">
                <img
                  src={url}
                  alt={name}
                  loading="lazy"
                  class={
                    props.big
                      ? 'max-h-[360px] max-w-full rounded-lg border object-contain'
                      : 'h-[120px] w-[160px] rounded-lg border object-cover'
                  }
                />
              </a>
            );
          case 'video':
            return (
              <video key={name} src={url} controls preload="metadata" playsInline class="max-h-[360px] max-w-full rounded-lg border bg-black" />
            );
          case 'audio':
            return <audio key={name} src={url} controls preload="metadata" class="w-[320px] max-w-full" />;
          case 'file':
            return (
              <a
                key={name}
                href={url}
                target="_blank"
                rel="noreferrer"
                class="flex items-center gap-2 rounded-lg border bg-muted px-3 py-2 text-[13px] hover:bg-accent"
              >
                <span aria-hidden="true">📄</span>
                <span class="font-medium">{`${link.extOf(name).toUpperCase() || 'A'} file`}</span>
              </a>
            );
        }
      })}
    </div>
  );
}
