// The window's reading, as app/src's tests check it.

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parse, reportRead } from './call.ts';
import { act, body, empty, hear, type Model, resync } from './model.ts';
import { flat, linkNames, mentionAlone, mentionAt, mentionDone, namesFor, output, tokensShow, usdShow } from './text.ts';
import type { Entry, Kind } from './wire.ts';

const entry = (kind: Kind, text: string): Entry => ({ kind, text, date: '2026-10-26T10:00:00.000Z', images: [], spend: null });

test('calls read as their tools', () => {
  let c = parse('zoom {"id":6243,"n":8}');
  assert.deepEqual([c.name, c.head], ['zoom', '#6243+8']);
  assert.equal(parse('bash {"description":"List files","command":"ls -la"}').head, 'ls -la');
  c = parse('bash {"description":"Run tests","command":"cargo te');
  assert.deepEqual([c.name, c.head], ['bash', 'cargo te']);
  assert.equal(parse('read {"file_path":"a.rs","offset":4,"limit":2}').head, 'a.rs:4-5');
  c = parse('edit {"file_path":"a.rs","old_string":"x\\ny","new_string":"z"}');
  assert.equal(c.head, 'a.rs');
  assert.equal(c.blocks[0].x, '--- a.rs\n+++ a.rs\n@@ -1,2 +1,1 @@\n-x\n-y\n+z');
  assert.equal(parse('lookup {"q":"bend","deep":{"a":1}}').head, '"bend" {"a":1}');
  assert.equal(parse('web_search {"query":"acorn site:github.com","count":3}').head, 'acorn site:github.com');
  c = parse('luau {"description":"Count rows","code":"print(#rows)"}');
  assert.deepEqual([c.head, c.blocks[0].x], ['Count rows', 'print(#rows)']);
  assert.equal(parse('no call').name, '');
  assert.deepEqual(reportRead('[RedAnt]\ndone\n\nFull chat: zoom("RedAnt")'), ['RedAnt', 'done']);
});

test('results read as files', () => {
  const read = output(
    '<path>/a/f</path>\n<type>file</type>\n<content>\n4: a\n5: b: c\n\n(Showing lines 4-5 of 9. Use offset=6 to continue.)\n</content>',
  );
  assert.deepEqual(read, [{ file: true, k: 'Showing lines 4-5 of 9. Use offset=6 to continue.', n: 4, x: ['a', 'b: c'] }]);
  const none = output('<path>/a/f</path>\n<type>file</type>\n<content>\n(End of file - total 0 lines)\n</content>');
  assert.ok(none[0].file && none[0].x.length === 0);
  const diff = output('--- f\n+++ f\n@@ -1,2 +1,2 @@\n a\n-b\n+B');
  assert.ok(!diff[0].file && diff[0].k === 'f' && diff[0].x[0].length === 3);
  assert.deepEqual(output('ok\n[exit code: 1]'), [{ file: true, k: 'result', n: 1, x: ['ok', '[exit code: 1]'] }]);
  assert.equal(flat('**bold** and `code` [x](http://y)'), 'bold and code x');
  assert.equal(linkNames('ask [RedAnt] and [x](y)'), 'ask [RedAnt](agent:RedAnt) and [x](y)');
  assert.equal(tokensShow(18_200), '18k');
  assert.equal(usdShow(0.0031), '$0.0031');
});

test('an @ word names an assistant', () => {
  const names = ['Acorn', 'Max', 'Marrie'];
  assert.deepEqual(mentionAt('ask @Ma', 7), [4, 'Ma']);
  assert.equal(mentionAt('ask @Ma now', 11), null);
  assert.equal(mentionAt('mail a@b', 8), null);
  assert.deepEqual(mentionAt('é @', 3), [2, '']);
  assert.deepEqual(namesFor('ma', names), ['Max', 'Marrie']);
  assert.deepEqual(namesFor('rr', names), ['Marrie']);
  assert.deepEqual(mentionDone('ask @ma', 4, 7, 'Max'), ['ask @Max ', 9]);
  assert.deepEqual(mentionDone('ask @ma why', 4, 7, 'Max'), ['ask @Max why', 9]);
  assert.equal(mentionAlone(' @max ', names), 'Max');
  assert.equal(mentionAlone('@Max why', names), null);
});

test('the chat shows blocks, then the turn whole', () => {
  let m: Model = empty();
  m = hear(m, { Know: { whole: [
    [0, entry('user', 'hi')],
    [1, entry('tool', 'bash {"command":"ls"}')],
    [2, entry('echo', 'file.txt')],
    [3, entry('unii', 'Found one file.')],
  ], nodes: [] } }).m;
  m = hear(m, { View: { parts: [[0, 1], [1, 1], [2, 1], [3, 1]], turn: 1 } }).m;
  const items = body(m);
  assert.equal(items[0].rows[0].t, 'box', "a user's message before the turn opens");
  const r = items[1].rows[0];
  assert.ok(r.t === 'msg' && r.call?.name === 'bash');
  assert.deepEqual(items.slice(1).map(item => [item.key, item.rows.map(row => row.t)]), [
    ['1+1', ['msg']], ['2+1', ['msg']], ['3+1', ['msg']],
  ], 'every message of the current turn stays expanded');
  const current = items.slice(1).flatMap(item => item.rows);
  assert.deepEqual(current.map(row => row.t === 'msg' ? [row.kind, row.text] : null), [
    ['tool', 'bash {"command":"ls"}'], ['echo', 'file.txt'], ['unii', 'Found one file.'],
  ]);
});

test('a message is the only act', () => {
  let m = hear(empty(), { State: { busy: true, wait: 'None', agents: ['FireAnt'], pins: ['the spec'], importing: null, today: null } }).m;
  assert.equal(act(m, { t: 'send', text: ' ', images: [] }).cmds.length, 0, 'an empty Enter stops nothing');
  m = act(m, { t: 'goto', name: 'FireAnt' }).m;
  m = act(m, { t: 'fold', on: true }).m;
  const sent = act(m, { t: 'send', text: ' stop ', images: ['aGVsbG8='] });
  assert.deepEqual(sent.cmds, [{ Say: { text: 'stop', images: ['aGVsbG8='] } }]);
  assert.deepEqual([sent.m.page, sent.m.fold], [null, null], 'back to the chat, where the reply comes');
});

test('the overview waits for its tops, and home leaves it', () => {
  let m = act(empty(), { t: 'goto', name: 'FireAnt' }).m;
  const on = act(m, { t: 'fold', on: true });
  assert.deepEqual([on.m.page, on.m.fold, on.cmds], [null, { parts: null }, ['Root']]);
  assert.equal(body(on.m)[1].rows[0].t, 'cap', 'an overview asked for says so, not empty');
  m = hear(on.m, { Root: { parts: [[0, 2]] } }).m;
  assert.deepEqual(m.fold, { parts: [[0, 2]] });
  assert.equal(hear(empty(), { Root: { parts: [[0, 2]] } }).m.fold, null, 'a late overview, unasked, stays unshown');
  m = act(act(m, { t: 'open', seg: [0, 2] }).m, { t: 'goto', name: 'FireAnt' }).m;
  const home = act(m, { t: 'home' }).m;
  assert.deepEqual([home.page, home.fold, home.open.size], [null, null, 0], 'one act, out of both');
  const still = empty();
  assert.equal(act(still, { t: 'home' }).m, still, 'home at home changes nothing');
});

test('a chat resynced asks for all it shows', () => {
  let m = act(empty(), { t: 'open', seg: [0, 4] }).m;
  m = act(m, { t: 'open', seg: [4, 1] }).m;
  m = act(m, { t: 'shut', seg: [8, 2] }).m;
  m = act(m, { t: 'goto', name: 'RedAnt' }).m;
  m = hear(m, { Agent: { name: 'RedAnt', log: [entry('work', 'hi')] } }).m;
  m = act(m, { t: 'goto', name: 'BlueOwl' }).m;
  m = act(m, { t: 'fold', on: true }).m;
  m = act(m, { t: 'goto', name: 'BlueOwl' }).m;
  const { m: n, cmds } = resync(m);
  assert.deepEqual(cmds, ['Attach', 'Root', { Agent: 'BlueOwl' }]);
  assert.deepEqual([...n.pages.keys()], ['BlueOwl'], 'pages not shown are asked for once shown');
  const zoomed = resync(act(empty(), { t: 'open', seg: [0, 4] }).m).cmds;
  assert.deepEqual(zoomed, ['Attach', { Zoom: [0, 4] }], 'an open block of many, its halves asked anew');
});

test('what the view no longer shows is dropped', () => {
  const node = (text: string) => ({ text, to: '2026-10-26T10:00:00.000Z' });
  let m: Model = empty();
  m = hear(m, { Know: { whole: [[0, entry('user', 'a')], [1, entry('unii', 'b')]], nodes: [] } }).m;
  m = hear(m, { View: { parts: [[0, 1], [1, 1]], turn: 0 } }).m;
  m = hear(m, { Entry: { agent: null, id: null, entry: entry('mind', 'hm') } }).m;
  assert.deepEqual([m.whole.size, m.minds.size], [2, 1], 'the view as it is shown is all kept');
  // the two summed, then the block opened, and a half of it
  m = hear(m, { Know: { whole: [], nodes: [[[0, 2], node('ab')], [[4, 4], node('efgh')], [[8, 1], node('i')]] } }).m;
  m = hear(m, { Know: { whole: [[8, entry('user', 'i')]], nodes: [] } }).m;
  m = hear(m, { View: { parts: [[0, 2], [4, 4], [8, 1]], turn: 8 } }).m;
  assert.deepEqual([[...m.whole.keys()], [...m.nodes.keys()], m.minds.size], [[8], ['0+2', '4+4', '8+1'], 0]);
  m = act(act(m, { t: 'open', seg: [4, 4] }).m, { t: 'open', seg: [4, 2] }).m;
  m = hear(m, { Know: { whole: [[4, entry('user', 'e')]], nodes: [[[4, 2], node('ef')], [[6, 2], node('gh')], [[4, 1], node('e')], [[5, 1], node('f')]] } }).m;
  m = act(m, { t: 'shut', seg: [4, 4] }).m;
  const kept = hear(m, { View: { parts: [[0, 2], [4, 4], [8, 1]], turn: 8 } }).m;
  assert.ok(kept.nodes === m.nodes && kept.whole === m.whole && kept.open === m.open, 'a view that drops nothing keeps what is known as it is');
  assert.ok(kept.nodes.has('5+1') && kept.whole.has(4), 'a block shut by hand keeps what it opens to');
  // the view sums on: the halves, and the blocks opened under them, go
  m = hear(m, { Know: { whole: [], nodes: [[[0, 8], node('a-h')]] } }).m;
  m = hear(m, { View: { parts: [[0, 8], [8, 1]], turn: 8 } }).m;
  assert.deepEqual([[...m.whole.keys()], [...m.nodes.keys()], m.open.size], [[8], ['0+8', '8+1'], 0]);
  // the overview's tops are kept while it shows
  m = act(m, { t: 'fold', on: true }).m;
  m = hear(m, { Know: { whole: [], nodes: [[[0, 16], node('all')]] } }).m;
  m = hear(m, { Root: { parts: [[0, 16]] } }).m;
  m = hear(m, { View: { parts: [[0, 8], [8, 1]], turn: 8 } }).m;
  assert.ok(m.nodes.has('0+16') && m.nodes.has('0+8'));
  m = hear(act(m, { t: 'home' }).m, { View: { parts: [[0, 8], [8, 1]], turn: 8 } }).m;
  assert.ok(!m.nodes.has('0+16'), 'and go once it is left');
});

test('the page of an agent done is dropped once left', () => {
  const state = (agents: string[]) => ({ State: { busy: true, wait: 'None' as const, agents, pins: [], importing: null, today: null } });
  let m = hear(empty(), state(['RedAnt', 'BlueOwl'])).m;
  m = act(m, { t: 'goto', name: 'RedAnt' }).m;
  m = act(m, { t: 'goto', name: 'BlueOwl' }).m;
  assert.deepEqual([...m.pages.keys()], ['RedAnt', 'BlueOwl'], 'agents at work keep their pages');
  m = hear(m, state([])).m;
  assert.deepEqual([...m.pages.keys()], ['BlueOwl'], 'the one shown stays, so it can be left');
  const left = act(m, { t: 'goto', name: null });
  assert.equal(left.m.pages.size, 0);
  assert.deepEqual(act(left.m, { t: 'goto', name: 'BlueOwl' }).cmds, [{ Agent: 'BlueOwl' }], 'shown again, asked for again');
});
