// A resource's answers on the JS target, over the runtime's own modules in Bun (js-runtime-harness.mjs).
import { expect, test } from 'bun:test';
import { freshRuntime } from './js-runtime-harness.mjs';

// A Rust source's answer at once outside its shape fails the resource with `shape`, as a TypeScript source's reply
// does: the commit stands and the last answer shows (runner `shaped_answer`).
test('an answer at once outside its shape fails the resource and the commit stands', async () => {
  const { res, W, data, commit, sig, act } = await freshRuntime('shape-now');
  data.answer = (source, [mode]) => (source === 'shapeProbe' ? { v: mode === 'good' ? 1 : 'extra' } : null);
  const mode = sig('good', 's');
  let probe;
  commit(() => { probe = res('shapeProbe', 'shapeProbe', () => [mode()], undefined, undefined, 'n', 0); probe(); });
  act(() => W(mode, 'extra'))();
  expect([mode(), probe(), probe.f(), probe.e()?.[0]]).toEqual(['extra', 1, true, 'shape']);
});
