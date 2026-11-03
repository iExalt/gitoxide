const { test } = require('node:test');
const assert = require('node:assert/strict');
const { qaMode } = require('./qa-mode.cjs');

test('ordinary PRs run the routine selection', () => {
  assert.equal(qaMode({ eventName: 'pull_request', headRef: 'perf/fork-qa-speed' }), 'fast');
});

test('release PRs run the deep set before a release can merge', () => {
  assert.equal(
    qaMode({ eventName: 'pull_request', headRef: 'release-please--branches--dev--components--gitoxide-fork' }),
    'exhaustive',
  );
});

test('dev pushes and manual runs run the deep set', () => {
  assert.equal(qaMode({ eventName: 'push', headRef: '' }), 'exhaustive');
  assert.equal(qaMode({ eventName: 'workflow_dispatch', headRef: '' }), 'exhaustive');
});
