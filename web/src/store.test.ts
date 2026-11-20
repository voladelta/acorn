import assert from 'node:assert/strict';
import { test } from 'node:test';
import * as store from './store.ts';

test('attachments report skipped files and preserve an empty selection', async () => {
  const before = store.get().pics;
  await store.addPics([]);
  assert.equal(store.get().pics, before);
  await store.addPics([new File(['text'], 'notes.txt', { type: 'text/plain' })]);
  assert.match(store.get().toast!, /notes.txt: unsupported file type/);
  const large = new File([], 'large.png', { type: 'image/png' });
  Object.defineProperty(large, 'size', { value: (50 << 20) + 1 });
  await store.addPics([large]);
  assert.match(store.get().toast!, /large.png: exceeds the 50 MB/);
  await store.addPics([new File(['text'], 'a.txt', { type: 'text/plain' }), large]);
  assert.match(store.get().toast!, /a.txt: unsupported file type.*large.png: exceeds the 50 MB/);
  assert.equal(store.get().pics, before);
  store.note(null);
});

test('attachments read supported files and report reader failures', async () => {
  const original = globalThis.FileReader;
  let fail = false;
  class Reader {
    result = 'data:image/png;base64,aGVsbG8=';
    error = new Error('Could not read attachment');
    onload = () => {};
    onerror = () => {};
    readAsDataURL() {
      if (fail) this.onerror();
      else this.onload();
    }
  }
  globalThis.FileReader = Reader as unknown as typeof FileReader;
  try {
    await store.addPics([new File(['hello'], 'picture.png', { type: 'image/png' })]);
    assert.equal(store.get().pics.at(-1)?.b64, 'aGVsbG8=');
    assert.equal(store.get().pics.at(-1)?.name, 'picture.png');
    store.dropPic(store.get().pics.length - 1);
    fail = true;
    await store.addPics([new File(['hello'], 'broken.pdf', { type: 'application/pdf' })]);
    assert.match(store.get().toast!, /broken.pdf: Could not read attachment/);
    assert.equal(store.get().pics.length, 0);
    // a failure mid-batch keeps the files read and reports the rest
    class Mixed extends Reader {
      readAsDataURL() {
        fail = !fail;
        super.readAsDataURL();
      }
    }
    globalThis.FileReader = Mixed as unknown as typeof FileReader;
    fail = true;
    await store.addPics([
      new File(['a'], 'a.png', { type: 'image/png' }),
      new File(['b'], 'b.png', { type: 'image/png' }),
      new File(['c'], 'c.txt', { type: 'text/plain' }),
    ]);
    assert.deepEqual(store.get().pics.map((p) => p.name), ['a.png']);
    assert.match(store.get().toast!, /b.png: Could not read attachment.*c.txt: unsupported file type/);
    store.dropPic(0);
    store.note(null);
  } finally {
    globalThis.FileReader = original;
  }
});
