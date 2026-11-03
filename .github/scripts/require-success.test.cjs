const { test } = require('node:test');
const assert = require('node:assert/strict');
const { failures } = require('./require-success.cjs');

test('the gate passes only when every dependency succeeded', () => {
  assert.deepEqual(failures({ plan: { result: 'success' }, run: { result: 'success' } }), []);
});

test('failed, skipped and cancelled dependencies all fail the gate', () => {
  assert.deepEqual(
    failures({
      plan: { result: 'success' },
      run: { result: 'failure' },
      msrv: { result: 'skipped' },
      docs: { result: 'cancelled' },
    }),
    ['run: failure', 'msrv: skipped', 'docs: cancelled'],
  );
});

test('a gate without dependencies fails rather than passing vacuously', () => {
  assert.deepEqual(failures({}), ['the gate has no dependencies']);
});
