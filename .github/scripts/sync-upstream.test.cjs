const { test } = require('node:test');
const assert = require('node:assert/strict');
const sync = require('./sync-upstream.cjs');

test('sync reads upstream and updates only the mirror without force', async () => {
  const calls = [];
  const github = { rest: {
    repos: { getBranch: async args => {
      calls.push(args);
      return { data: { commit: { sha: 'upstream-tip' } } };
    } },
    git: { updateRef: async args => { calls.push(args); } },
  } };
  await sync(github, { info() {} });
  assert.deepEqual(calls, [
    { owner: 'GitoxideLabs', repo: 'gitoxide', branch: 'main' },
    { owner: 'iExalt', repo: 'gitoxide', ref: 'heads/upstream/main', sha: 'upstream-tip', force: false },
  ]);
});

test('a rejected update fails without retrying with force', async () => {
  let attempts = 0;
  const github = { rest: {
    repos: { getBranch: async () => ({ data: { commit: { sha: 'upstream-tip' } } }) },
    git: { updateRef: async () => { attempts++; throw new Error('not a fast-forward'); } },
  } };
  await assert.rejects(sync(github, { info() {} }), /not a fast-forward/);
  assert.equal(attempts, 1);
});
