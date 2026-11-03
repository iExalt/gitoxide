import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import test from 'node:test';
import {
  assertGitVersion,
  checkRebase,
  evaluateCandidateChecks,
  exportResolutions,
  importResolutions,
  normalizeUpstream,
  parseDeletionResolutions,
  parseReleaseTags,
  publishRebase,
  pushCandidate,
  replayDev,
  signPrepared,
  writePrepared,
  loadPrepared,
  verifyMapping,
  assertSameTree,
  releaseBoundary,
  releaseConfigUpdate,
  replayCommitter,
  setConfigSha,
  signingIdentity,
  snapshotRefs,
  validateCandidate,
} from './rebase-dev.mjs';

// Detached auto-maintenance runs rerere-gc, which takes MERGE_RR.lock and races the
// fixture's own rebase picks.
const NO_AUTO_MAINTENANCE = ['-c', 'maintenance.auto=false'];
process.env.GIT_CONFIG_GLOBAL = '/dev/null';
process.env.GIT_CONFIG_NOSYSTEM = '1';
process.env.GIT_AUTHOR_NAME = 'Rebase fixture';
process.env.GIT_AUTHOR_EMAIL = 'fixture@example.invalid';
process.env.GIT_COMMITTER_NAME = 'Rebase fixture';
process.env.GIT_COMMITTER_EMAIL = 'fixture@example.invalid';
delete process.env.GITHUB_ENV;

const SIGNER = 'signer@example.invalid';
const OTHER_KEPT = 'other@example.invalid';
const identity = signingIdentity({
  GITOXIDE_REBASE_SIGNING_NAME: 'Rebase Signer',
  GITOXIDE_REBASE_SIGNING_EMAIL: SIGNER,
  GITOXIDE_REBASE_KEEP_COMMITTER_EMAILS: ` ${OTHER_KEPT} ,`,
});

const root = mkdtempSync(join(tmpdir(), 'gitoxide-rebase-dev-test-'));
test.after(() => rmSync(root, { recursive: true, force: true }));
const keyPath = join(root, 'fixture-signing-key');
execFileSync('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-f', keyPath]);
const signingKey = readFileSync(keyPath, 'utf8');
const publicKey = readFileSync(`${keyPath}.pub`, 'utf8').trim();
const wrongKeyPath = join(root, 'wrong-signing-key');
execFileSync('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-f', wrongKeyPath]);
const allowedSigners = join(root, 'allowed-signers');
writeFileSync(allowedSigners, `${SIGNER},${OTHER_KEPT} ${publicKey.split(/\s+/).slice(0, 2).join(' ')}\n`);

// The replay needs git 2.55 (see assertGitVersion); older local git skips only the fixtures.
const gitVersionError = (() => {
  try {
    assertGitVersion(execFileSync('git', ['--version'], { encoding: 'utf8' }));
    return false;
  } catch (error) {
    return error.message;
  }
})();
const fixtureTest = (name, body) => test(name, { skip: gitVersionError }, body);
// CI must not go green while testing nothing.
if (gitVersionError && process.env.CI) {
  test('git is new enough for the rebase fixtures', () => assert.fail(`${gitVersionError}; CI needs the fixtures to run`));
}

const recorded = JSON.parse(readFileSync(new URL('./rebase-dev.fixtures.json', import.meta.url), 'utf8'));
const RECORDED_SHA = '8b21b7aa05e1e5fddb602ac40e479cdfb96883d5';
const RECORDED_BRANCH = 'linearize/candidate';

function git(cwd, ...args) {
  return gitWithEnv(cwd, {}, ...args);
}

function gitWithEnv(cwd, extraEnv, ...args) {
  return execFileSync(
    'git',
    [...NO_AUTO_MAINTENANCE, '-c', 'commit.gpgsign=false', '-c', 'init.defaultBranch=main', ...args],
    { cwd, encoding: 'utf8', env: { ...process.env, LC_ALL: 'C', ...extraEnv }, stdio: ['pipe', 'pipe', 'pipe'] },
  ).trimEnd();
}

function commit(cwd, message, metadata = {}) {
  git(cwd, 'add', '-A');
  gitWithEnv(cwd, metadata, 'commit', '--quiet', '-m', message);
}

function devMetadata(authorName, authorEmail, authorDate, committerEmail = SIGNER, committerDate = authorDate) {
  return {
    GIT_AUTHOR_NAME: authorName,
    GIT_AUTHOR_EMAIL: authorEmail,
    GIT_AUTHOR_DATE: authorDate,
    GIT_COMMITTER_NAME: 'Committer',
    GIT_COMMITTER_EMAIL: committerEmail,
    GIT_COMMITTER_DATE: committerDate,
  };
}

function content({ target = 'base target', far = 'base far' } = {}) {
  return [
    'base marker',
    ...Array.from({ length: 12 }, (_, index) => `context ${index}`),
    target,
    ...Array.from({ length: 12 }, (_, index) => `tail ${index}`),
    far,
  ].join('\n') + '\n';
}

const manifest = (version) => `[package]\nname = "fixture"\nversion = "${version}"\n`;
const releaseConfig = (bootstrap) => `{
  "bootstrap-sha": "${bootstrap}",
  "packages": {
    ".": { "release-type": "simple", "component": "gitoxide-fork" }
  }
}
`;

function initRepo(path) {
  mkdirSync(path, { recursive: true });
  git(path, 'init', '--quiet', '-b', 'main');
}

// What the normalizer produces, without cargo: the upstream tree with its manifest on 0.0.0, as
// the only child of upstream, with the trailer.
function normalizeCommit(cwd, upstream, env = {}) {
  const index = join(cwd, '.git', 'normalize-index');
  const indexEnv = { GIT_INDEX_FILE: index };
  gitWithEnv(cwd, indexEnv, 'read-tree', upstream);
  const blob = execFileSync('git', ['hash-object', '-w', '--stdin'], {
    cwd,
    input: git(cwd, 'show', `${upstream}:Cargo.toml`).replace(/version = "[^"]*"/, 'version = "0.0.0"') + '\n',
    encoding: 'utf8',
  }).trim();
  gitWithEnv(cwd, indexEnv, 'update-index', '--cacheinfo', `100644,${blob},Cargo.toml`);
  const tree = gitWithEnv(cwd, indexEnv, 'write-tree');
  rmSync(index);
  return gitWithEnv(
    cwd,
    env,
    'commit-tree', tree, '-p', upstream,
    '-m', 'chore(gix): normalize upstream manifests',
    '-m', `Normalized-upstream: ${upstream}`,
  );
}

async function fakeNormalizer(cwd, upstream, dev, env) {
  const base = git(cwd, 'log', '-1', '--first-parent', '--format=%H', '--grep=^Normalized-upstream: ', dev);
  return `base=${base}\nnormalized=${normalizeCommit(cwd, upstream, env)}\n`;
}

// A dev edit to a file upstream deleted, and nothing else.
function commitPatchOnlyEdit(seed) {
  writeFileSync(join(seed, 'renamed.patch'), content({ target: 'patch-only edit' }));
  commit(seed, 'dev patch-only edit', devMetadata('Patch Author', 'patch@example.invalid', '1700000350 +0000'));
}

async function makeFixture(name, { deletion, deletionOnly, renameRename } = {}) {
  const directory = join(root, name);
  const remote = join(directory, 'origin.git');
  const seed = join(directory, 'seed');
  const recorder = join(directory, 'recorder');
  mkdirSync(directory, { recursive: true });
  git(directory, 'init', '--quiet', '--bare', remote);

  initRepo(seed);
  writeFileSync(join(seed, 'shared.txt'), content());
  writeFileSync(join(seed, 'Cargo.toml'), manifest('0.5.0'));
  if (deletion || renameRename) writeFileSync(join(seed, 'old.patch'), content());
  commit(seed, 'upstream base');
  const upstreamBase = git(seed, 'rev-parse', 'HEAD');
  git(seed, 'remote', 'add', 'origin', remote);
  git(seed, 'push', '--quiet', 'origin', 'HEAD:refs/heads/upstream/main');

  const baseSha = normalizeCommit(seed, upstreamBase);
  git(seed, 'checkout', '--quiet', '-b', 'dev', baseSha);
  mkdirSync(join(seed, '.github', 'workflows'), { recursive: true });
  writeFileSync(join(seed, '.github', 'workflows', 'fork.yml'), 'on: push\n');
  writeFileSync(join(seed, 'release-please-config.json'), releaseConfig(baseSha));
  commit(seed, 'chore(gix): set up the fork', devMetadata('Setup Author', 'setup@example.invalid', '1700000050 +0000', OTHER_KEPT));
  writeFileSync(join(seed, 'shared.txt'), content({ target: 'dev target' }));
  if (deletion) {
    git(seed, 'mv', 'old.patch', 'renamed.patch');
    writeFileSync(join(seed, 'renamed.patch'), content({ target: 'updated patch' }));
  }
  if (renameRename) git(seed, 'mv', 'old.patch', 'fork.patch');
  // Like a GitHub rebase merge, committed by GitHub; the message has no final newline, which a
  // conflicted pick adds back.
  git(seed, 'add', '-A');
  writeFileSync(join(directory, 'message'), 'dev target edit\n\nconflicting replay body');
  gitWithEnv(seed, devMetadata('Zoë Author', 'zoe@example.invalid', '1700000300 +0930', 'noreply@github.com', '1700000301 -0330'),
    'commit', '--quiet', '--cleanup=verbatim', '-F', join(directory, 'message'));
  if (deletionOnly) commitPatchOnlyEdit(seed);
  git(seed, 'push', '--quiet', 'origin', 'HEAD:refs/heads/dev');
  git(remote, 'symbolic-ref', 'HEAD', 'refs/heads/dev');
  const devSha = git(seed, 'rev-parse', 'HEAD');

  // Upstream edits the same line, bumps its own version and deletes or renames the patch.
  git(seed, 'checkout', '--quiet', '-b', 'upstream', upstreamBase);
  writeFileSync(join(seed, 'shared.txt'), content({ target: 'upstream target' }));
  writeFileSync(join(seed, 'Cargo.toml'), manifest('0.6.0'));
  if (deletion) git(seed, 'rm', '--quiet', 'old.patch');
  if (renameRename) git(seed, 'mv', 'old.patch', 'upstream.patch');
  commit(seed, 'upstream target edit');
  git(seed, 'push', '--quiet', 'origin', 'HEAD:refs/heads/upstream/main');
  const upstreamSha = git(seed, 'rev-parse', 'HEAD');

  // Record the resolutions by rebasing as FORK.md's reproduction steps do.
  git(directory, 'clone', '--quiet', '--branch', 'dev', remote, recorder);
  const recordedBase = normalizeCommit(recorder, upstreamSha);
  const rerere = ['-c', 'rerere.enabled=true', '-c', 'rerere.autoupdate=true'];
  const stopped = spawnSync('git', [...NO_AUTO_MAINTENANCE, ...rerere, 'rebase', '--merge', '--keep-empty', '--empty=stop', '--onto', recordedBase, baseSha], {
    cwd: recorder, encoding: 'utf8', env: { ...process.env, LC_ALL: 'C' },
  });
  assert.notEqual(stopped.status, 0, 'fixture must record a real rebase conflict');
  const deletionRecords = [];
  const stagesOf = (path) => git(recorder, 'ls-files', '-u', '--', path).split('\n').map((line) => {
    const [mode, oid, stage] = line.split(/\s+/);
    return { mode, oid, stage: Number(stage) };
  });
  const recordDeletion = () => {
    deletionRecords.push({ upstream: upstreamSha, commit: git(recorder, 'rev-parse', 'REBASE_HEAD'), path: 'renamed.patch', stages: stagesOf('renamed.patch') });
    git(recorder, 'rm', '--quiet', 'renamed.patch');
  };
  if (deletion) recordDeletion();
  if (renameRename) {
    const replayed = git(recorder, 'rev-parse', 'REBASE_HEAD');
    for (const [path, action] of [['old.patch', 'delete'], ['upstream.patch', 'delete'], ['fork.patch', 'keep']]) {
      deletionRecords.push({ upstream: upstreamSha, commit: replayed, path, action, stages: stagesOf(path) });
    }
    git(recorder, 'rm', '--quiet', '--', 'old.patch', 'upstream.patch');
    git(recorder, 'add', '--', 'fork.patch');
  }
  writeFileSync(join(recorder, 'shared.txt'), content({ target: 'resolved target' }));
  git(recorder, 'add', 'shared.txt');
  if (deletionOnly) {
    // The patch-only edit stops on its own modify/delete conflict, which leaves it empty.
    const next = spawnSync('git', [...NO_AUTO_MAINTENANCE, '-c', 'core.editor=true', 'rebase', '--continue'], { cwd: recorder, encoding: 'utf8' });
    assert.notEqual(next.status, 0, 'fixture must stop on the patch-only edit');
    recordDeletion();
    git(recorder, 'commit', '--quiet', '--allow-empty', '-C', 'REBASE_HEAD');
  }
  git(recorder, ...rerere, '-c', 'core.editor=true', 'rebase', '--continue');

  const cache = join(recorder, '.git', 'rr-cache');
  const ids = readdirSync(cache).filter((entry) => /^[0-9a-f]{40}$/.test(entry));
  assert.equal(ids.length, 1, 'fixture should record one rerere id');
  const [id] = ids;
  const resolverPath = join(directory, 'resolver');
  initRepo(resolverPath);
  git(resolverPath, 'checkout', '--quiet', '-b', 'rebase-resolutions');
  writeFileSync(join(cache, id, 'thisimage'), 'active resolution state must not export\n');
  const activeId = 'a'.repeat(40);
  mkdirSync(join(cache, activeId), { recursive: true });
  writeFileSync(join(cache, activeId, 'preimage'), 'unfinished preimage\n');
  assert.equal(await exportResolutions({ repositoryPath: recorder, destinationPath: resolverPath }), 1);
  assert.equal(existsSync(join(resolverPath, 'resolutions', activeId)), false, 'incomplete records stay local');
  assert.equal(existsSync(join(resolverPath, 'resolutions', id, 'thisimage')), false, 'active state stays local');
  if (deletionRecords.length > 0) writeFileSync(join(resolverPath, 'deletions.json'), JSON.stringify(deletionRecords) + '\n');
  commit(resolverPath, 'seed completed rerere resolution');
  git(resolverPath, 'remote', 'add', 'origin', remote);
  git(resolverPath, 'push', '--quiet', 'origin', 'HEAD:refs/heads/rebase-resolutions');

  return { directory, remote, seed, devSha, upstreamSha, baseSha, resolverPath, recorder, id, deletionRecords };
}

// Moves upstream past the recorded resolution, changing context the resolution doesn't cover.
function advanceUpstream(fixture, far, message) {
  git(fixture.seed, 'checkout', '--quiet', 'upstream');
  writeFileSync(join(fixture.seed, 'shared.txt'), content({ target: 'upstream target', far }));
  commit(fixture.seed, message);
  git(fixture.seed, 'push', '--quiet', 'origin', 'HEAD:refs/heads/upstream/main');
  fixture.upstreamSha = git(fixture.seed, 'rev-parse', 'HEAD');
}

// Adds commits to dev after the resolution was recorded.
function extendDev(fixture, write) {
  git(fixture.seed, 'checkout', '--quiet', 'dev');
  write(fixture.seed);
  git(fixture.seed, 'push', '--quiet', 'origin', 'HEAD:refs/heads/dev');
  fixture.devSha = git(fixture.seed, 'rev-parse', 'HEAD');
}

// Most tests start from the same plain fixture; build it once and copy it, re-pointing remotes.
let plainTemplate;
async function plainFixture(name) {
  plainTemplate ??= makeFixture('plain-template');
  const template = await plainTemplate;
  const directory = join(root, name);
  cpSync(template.directory, directory, { recursive: true });
  const moved = (path) => join(directory, relative(template.directory, path));
  const fixture = { ...template, directory, ...Object.fromEntries(['remote', 'seed', 'resolverPath', 'recorder'].map((key) => [key, moved(template[key])])) };
  for (const repository of [fixture.seed, fixture.resolverPath, fixture.recorder]) git(repository, 'remote', 'set-url', 'origin', fixture.remote);
  return fixture;
}

function pushResolutions(fixture, message) {
  commit(fixture.resolverPath, message);
  git(fixture.resolverPath, 'push', '--quiet', '--force', 'origin', 'HEAD:refs/heads/rebase-resolutions');
}

function cloneFixture(fixture, name) {
  const clone = join(fixture.directory, name);
  git(fixture.directory, 'clone', '--quiet', '--branch', 'dev', fixture.remote, clone);
  return clone;
}

// Runs the prepare job's steps, handing over through a bundle as the workflow does.
async function prepareClone(clone) {
  const snapshots = await snapshotRefs({ repositoryPath: clone });
  const normalizedBase = await normalizeUpstream({ repositoryPath: clone, dev: snapshots.dev, upstream: snapshots.upstream, identity, runNormalizer: fakeNormalizer });
  const replayed = await replayDev({ repositoryPath: clone, snapshots, ...normalizedBase, identity });
  const directory = `${clone}.prepared`;
  const prepared = { ...snapshots, ...normalizedBase, head: replayed.head };
  await writePrepared({ repositoryPath: clone, directory, prepared });
  return { directory, prepared, replayed };
}

// Runs the publish job's signing step in `clone`, checked out at the captured dev.
async function signClone(clone, directory, options = {}) {
  const dev = JSON.parse(readFileSync(join(directory, 'prepared.json'), 'utf8')).dev;
  git(clone, 'checkout', '--quiet', '--detach', dev);
  const prepared = await loadPrepared({ repositoryPath: clone, directory, dev });
  return signPrepared({ repositoryPath: clone, prepared, signingKey, publicKey, identity, ...options });
}

// Both jobs, in one clone for brevity; the first fixture test uses a fresh clone to sign.
async function rebaseClone(clone) {
  const { directory, prepared } = await prepareClone(clone);
  return { prepared, result: await signClone(clone, directory) };
}

const remoteRefs = (clone) => git(clone, 'ls-remote', '--refs', 'origin');
const verify = (clone, sha) => git(clone, '-c', 'gpg.format=ssh', '-c', `gpg.ssh.allowedSignersFile=${allowedSigners}`, 'verify-commit', sha);
const rawCommit = (clone, sha) => {
  const raw = execFileSync('git', ['cat-file', 'commit', sha], { cwd: clone });
  const separator = raw.indexOf('\n\n');
  return { headers: raw.subarray(0, separator).toString('utf8').split('\n'), message: raw.subarray(separator + 2) };
};
const header = (commitObject, name) => commitObject.headers.find((line) => line.startsWith(`${name} `));

// A fake GitHub API answering from the recorded responses, rebound to another SHA and branch.
function recordedApi(sha, branch, mutate = (response) => response) {
  const rebind = (item) => ({
    ...item,
    head_sha: item.head_sha === RECORDED_SHA ? sha : item.head_sha,
    ...(item.head_branch === RECORDED_BRANCH ? { head_branch: branch } : {}),
  });
  const response = mutate({
    workflow_runs: recorded.workflow_runs.map(rebind),
    check_runs: recorded.check_runs.map(rebind),
  });
  const calls = [];
  const api = async (method, path, body) => {
    calls.push([method, path, body]);
    if (method === 'POST') return null;
    if (path.includes('/runs?')) return { workflow_runs: response.workflow_runs };
    return { check_runs: response.check_runs };
  };
  return { api, calls };
}

test('replay refuses git older than 2.55', () => {
  assert.doesNotThrow(() => assertGitVersion('git version 2.55.0\n'));
  assert.doesNotThrow(() => assertGitVersion('git version 3.0.1'));
  assert.throws(() => assertGitVersion('git version 2.54.0 (Apple Git-157)'), /cannot replay dev; use git 2\.55 or later/);
});

test('the signing identity comes from configuration and decides which committers a replay keeps', () => {
  assert.deepEqual([...identity.keepEmails], [SIGNER, OTHER_KEPT]);
  assert.throws(() => signingIdentity({ GITOXIDE_REBASE_SIGNING_EMAIL: SIGNER }), /SIGNING_NAME is required/);
  assert.throws(() => signingIdentity({ GITOXIDE_REBASE_SIGNING_NAME: 'Signer' }), /SIGNING_EMAIL is required/);
  assert.throws(
    () => signingIdentity({ GITOXIDE_REBASE_SIGNING_NAME: 'Signer', GITOXIDE_REBASE_SIGNING_EMAIL: SIGNER, GITOXIDE_REBASE_KEEP_COMMITTER_EMAILS: 'a b@c' }),
    /Invalid committer email/,
  );
  const committer = (email) => ({ committer: { line: `committer C <${email}> 1 +0000`, name: 'C', email, date: '1 +0000' } });
  assert.equal(replayCommitter(committer(OTHER_KEPT), 'sha', identity).email, OTHER_KEPT, 'a kept committer stays');
  assert.deepEqual(replayCommitter(committer('noreply@github.com'), 'sha', identity), {
    line: `committer Rebase Signer <${SIGNER}> 1 +0000`, name: 'Rebase Signer', email: SIGNER, date: '1 +0000',
  }, 'a web-flow committer becomes the signing identity at the original time');
  assert.throws(() => replayCommitter(committer('someone@example.invalid'), 'sha', identity), /unrecognized committer email/);
});

test('deletion records reject unsafe paths, modes, duplicate keys and non-deletion stages', () => {
  const record = {
    upstream: 'a'.repeat(40),
    commit: 'b'.repeat(40),
    path: 'patches/example.patch',
    stages: [{ mode: '100644', oid: 'c'.repeat(40), stage: 1 }, { mode: '100644', oid: 'd'.repeat(40), stage: 3 }],
  };
  assert.deepEqual(parseDeletionResolutions(JSON.stringify([record])), [record]);
  for (const path of ['../file', '/file', '.git/config', 'a/../file', 'a\nfile', 'a\\file']) {
    assert.throws(() => parseDeletionResolutions(JSON.stringify([{ ...record, path }])), /Invalid/);
  }
  for (const stages of [[], [...record.stages, { ...record.stages[1], stage: 2 }], record.stages.map((stage) => ({ ...stage, mode: '120000' }))]) {
    assert.throws(() => parseDeletionResolutions(JSON.stringify([{ ...record, stages }])), /Invalid/);
  }
  const [base, fork] = record.stages;
  for (const accepted of [{ ...record, stages: [base] }, { ...record, stages: [fork] }, { ...record, action: 'keep', stages: [fork] }]) {
    assert.deepEqual(parseDeletionResolutions(JSON.stringify([accepted])), [accepted]);
  }
  for (const rejected of [{ ...record, action: 'keep' }, { ...record, action: 'keep', stages: [base] }, { ...record, action: 'rename', stages: [fork] }]) {
    assert.throws(() => parseDeletionResolutions(JSON.stringify([rejected])), /Invalid/);
  }
  assert.throws(() => parseDeletionResolutions(JSON.stringify([record, record])), /Duplicate/);
});

test('release boundary maps the newest tagged or configured replay commit', () => {
  const sha = (character) => character.repeat(40);
  const tags = parseReleaseTags([
    `${sha('1')}\trefs/tags/gitoxide-fork-v0.1.0`,
    `${sha('2')}\trefs/tags/gitoxide-fork-v0.2.0`,
    `${sha('3')}\trefs/tags/gitoxide-fork-v0.2.0^{}`,
    `${sha('4')}\trefs/tags/gix-v0.90.0`,
    `${sha('5')}\trefs/tags/v9.9.9`,
  ].join('\n'));
  assert.deepEqual([...tags].sort(), [sha('1'), sha('3')], 'only fork tags count, peeled when annotated');
  const original = [sha('1'), sha('a'), sha('3'), sha('b')];
  const rebased = [sha('c'), sha('d'), sha('e'), sha('f')];
  assert.deepEqual(releaseBoundary(original, rebased, tags), { original: sha('3'), rewritten: sha('e') });
  assert.deepEqual(releaseBoundary(original, rebased, [sha('a'), undefined]), { original: sha('a'), rewritten: sha('d') },
    'a boundary an earlier rebase recorded moves again');
  assert.equal(releaseBoundary(original, rebased, [sha('9')]), null);
  assert.throws(() => releaseBoundary(original, rebased.slice(1), tags), /every replayed commit/);
});

test('release-please config moves bootstrap-sha before the first release and last-release-sha after it', () => {
  const sha = (character) => character.repeat(40);
  // The real file, so the in-place edit keeps its hand formatting.
  const text = readFileSync(new URL('../../release-please-config.json', import.meta.url), 'utf8');
  const config = JSON.parse(text);
  const replay = { originalCommits: [sha('1'), sha('2')], rewrittenCommits: [sha('a'), sha('b')], base: sha('n') };

  const bootstrap = releaseConfigUpdate({ config, releaseCommits: new Set(), ...replay });
  assert.deepEqual([bootstrap.key, bootstrap.sha], ['bootstrap-sha', sha('n')]);
  const moved = setConfigSha(text, bootstrap.key, bootstrap.sha);
  assert.equal(moved, text.replace(config['bootstrap-sha'], sha('n')), 'only the SHA changes');
  assert.equal(releaseConfigUpdate({ config: JSON.parse(moved), releaseCommits: new Set(), ...replay }), null);

  const released = releaseConfigUpdate({ config, releaseCommits: new Set([sha('1')]), ...replay });
  assert.deepEqual([released.key, released.sha], ['last-release-sha', sha('a')]);
  const added = setConfigSha(text, released.key, released.sha);
  assert.deepEqual(JSON.parse(added), { ...config, 'last-release-sha': sha('a') });
  assert.match(added, new RegExp(`"bootstrap-sha": "${config['bootstrap-sha']}",\n  "last-release-sha": "${sha('a')}",\n`));
  assert.equal(setConfigSha(added, 'last-release-sha', sha('b')), added.replace(sha('a'), sha('b')), 'an existing value is replaced in place');
  assert.equal(
    releaseConfigUpdate({ config: JSON.parse(added), releaseCommits: new Set([sha('9')]), ...replay, originalCommits: [sha('a'), sha('2')] }),
    null,
    'an unmoved boundary needs no commit',
  );
  assert.throws(() => releaseConfigUpdate({ config, releaseCommits: new Set([sha('9')]), ...replay }), /set last-release-sha by hand/,
    'a release outside the replay would let release-please walk into upstream history');
});

test('only a successful Fork CI required from the candidate dispatch run validates the exact SHA', () => {
  const evaluate = (mutate = () => {}, { sha = RECORDED_SHA, branch = RECORDED_BRANCH } = {}) => {
    const responses = structuredClone(recorded);
    mutate(responses);
    return evaluateCandidateChecks({ sha, branch, workflowRuns: responses.workflow_runs, checkRuns: responses.check_runs });
  };
  const dispatchRun = (responses) => responses.workflow_runs.find((run) => run.event === 'workflow_dispatch');
  const dispatchCheck = (responses) => responses.check_runs.find((check) => check.name === 'Fork CI required' && check.check_suite.id === dispatchRun(responses).check_suite_id);

  assert.deepEqual(evaluate(), { state: 'success', reason: 'Fork CI required passed in Fork CI run 41648484118', runId: 41648484118 });
  assert.equal(evaluate(() => {}, { branch: 'dev' }).state, 'pending', 'a push run on dev is not the candidate validation');
  assert.equal(evaluate(() => {}, { sha: 'f'.repeat(40) }).state, 'pending', 'runs for another SHA do not count');
  assert.equal(evaluate((r) => { r.workflow_runs = r.workflow_runs.filter((run) => run.event !== 'workflow_dispatch'); }).state, 'pending');
  assert.equal(evaluate((r) => { dispatchRun(r).path = '.github/workflows/other.yml'; }).state, 'pending', 'another workflow does not count');
  assert.equal(evaluate((r) => { Object.assign(dispatchCheck(r), { status: 'in_progress', conclusion: null }); }).state, 'pending');
  for (const conclusion of ['failure', 'skipped', 'cancelled', 'neutral']) {
    assert.deepEqual(evaluate((r) => { dispatchCheck(r).conclusion = conclusion; }), {
      state: 'failure', reason: `Fork CI required in run 41648484118 concluded ${conclusion}`,
    });
  }
  assert.match(evaluate((r) => { dispatchCheck(r).app.id = 1; }).reason, /completed without Fork CI required/, 'another app cannot satisfy the check');
  assert.match(evaluate((r) => { r.check_runs = r.check_runs.filter((check) => check !== dispatchCheck(r)); }).reason, /completed without/,
    "the push run's check is in another suite");
  assert.equal(evaluate((r) => { dispatchRun(r).status = 'in_progress'; r.check_runs = []; }).state, 'pending');
  assert.match(evaluate((r) => { dispatchCheck(r).head_sha = 'e'.repeat(40); }).reason, /is for e{40}/);
  assert.equal(evaluate((r) => { r.check_runs.push({ ...dispatchCheck(r), id: dispatchCheck(r).id + 1, conclusion: 'failure' }); }).state, 'failure',
    'the newest attempt decides');
});

test('validation dispatches Fork CI once and waits only while the result is pending', async () => {
  const sha = 'c'.repeat(40);
  const branch = 'rebase/candidate/1-1';
  let polls = 0;
  const { api, calls } = recordedApi(sha, branch);
  const pendingFirst = async (method, path, body) => {
    if (method === 'GET' && path.includes('/runs?') && polls++ === 0) return { workflow_runs: [] };
    return api(method, path, body);
  };
  const sleeps = [];
  const state = await validateCandidate({ api: pendingFirst, sha, branch, intervalMs: 5, sleep: async (ms) => sleeps.push(ms), now: () => 0 });
  assert.equal(state.state, 'success');
  assert.deepEqual(calls[0], ['POST', '/actions/workflows/fork-ci.yml/dispatches', { ref: branch }]);
  assert.equal(calls.filter(([method]) => method === 'POST').length, 1);
  assert.deepEqual(sleeps, [5]);

  const failing = recordedApi(sha, branch, (r) => {
    r.check_runs.find((check) => check.id === 125964032734).conclusion = 'failure';
    return r;
  });
  await assert.rejects(validateCandidate({ api: failing.api, sha, branch, sleep: async () => assert.fail('a failure must not wait') }), /concluded failure/);
  let clock = 0;
  await assert.rejects(
    validateCandidate({ api: async () => ({ workflow_runs: [], check_runs: [] }), sha, branch, timeoutMs: 10, intervalMs: 6, sleep: async (ms) => { clock += ms; }, now: () => clock }),
    /Timed out waiting for Fork CI/,
  );
});

test('the workflow keeps upstream-controlled code away from every credential', () => {
  const text = readFileSync(new URL('../workflows/rebase-dev.yml', import.meta.url), 'utf8');
  assert.match(text, /^on:\n  workflow_dispatch:\n\n/m);
  assert.match(text, /^permissions: \{\}$/m);
  assert.match(text, /github\.repository == 'iExalt\/gitoxide' && github\.ref == 'refs\/heads\/dev' && github\.actor == 'iExalt'/);
  const prepareJob = text.slice(text.indexOf('\n  prepare:\n'), text.indexOf('\n  publish:\n'));
  const publishJob = text.slice(text.indexOf('\n  publish:\n'));
  assert.match(prepareJob, /permissions:\n {6}contents: read\n {4}steps:/, 'the prepare job only reads');
  assert.doesNotMatch(prepareJob, /secrets\.|token/, 'the prepare job sees no secret');
  assert.match(publishJob, /needs: prepare\n/);
  const steps = text.split(/^ {6}- /m).slice(1);
  const index = (pattern) => {
    const matches = steps.flatMap((step, position) => (pattern.test(step) ? [position] : []));
    assert.equal(matches.length, 1, `exactly one step matches ${pattern}`);
    return matches[0];
  };
  const commands = ['snapshot', 'normalize', 'replay', 'check', 'bundle', 'sign', 'candidate', 'validate', 'publish'];
  const order = commands.map((command) => index(new RegExp(`rebase-dev\\.mjs"? ${command}'?$`, 'm')));
  assert.deepEqual(order, [...order].sort((a, b) => a - b), 'steps run in order');
  const sign = order[commands.indexOf('sign')];
  assert.equal(index(/secrets\.GITOXIDE_REBASE_SSH_SIGNING_KEY/), sign, 'only the sign step sees the signing key');
  assert.equal(index(/steps\.candidate-token\.outputs\.token/), order[commands.indexOf('candidate')]);
  assert.equal(index(/steps\.token\.outputs\.token/), order[commands.indexOf('publish')], 'only the publish step sees the publish token');
  const tokenIndex = index(/id: token$/m);
  assert.ok(order[commands.indexOf('validate')] < tokenIndex && tokenIndex < order[commands.indexOf('publish')], 'the publish token is minted after validation');
  for (const step of steps.slice(sign).filter((candidate) => /\brun: /.test(candidate))) {
    assert.doesNotMatch(step, /\bmise\b|\bcargo\b/, 'after checking out the replayed tree, nothing resolves tools through it');
    if (!/if: failure\(\)/.test(step)) assert.match(step, /"\$REBASE_NODE" "\$RUNNER_TEMP\/rebase-dev\.mjs"/);
  }
  for (const step of steps.filter((candidate) => /^uses: /.test(candidate))) assert.match(step, /^uses: [\w./-]+@[0-9a-f]{40} # v/, 'actions are pinned');
  for (const step of steps.filter((candidate) => /^uses: actions\/checkout@/.test(candidate))) assert.match(step, /persist-credentials: false/);
});

fixtureTest('known resolutions replay onto the signed normalized base with metadata, signatures and the release bootstrap', async () => {
  const fixture = await plainFixture('known');
  advanceUpstream(fixture, 'upstream far', 'upstream changes unrelated context');
  extendDev(fixture, (seed) => gitWithEnv(seed, devMetadata('Empty Author', 'empty@example.invalid', '1700000100 -0700', SIGNER, '1700000101 +0545'),
    'commit', '--quiet', '--allow-empty', '-m', 'originally empty dev commit\n\nkept as an original empty commit\n'));
  const preparer = cloneFixture(fixture, 'prepare');
  const before = remoteRefs(preparer);
  const { directory, prepared } = await prepareClone(preparer);
  assert.equal(prepared.base, fixture.baseSha);
  assert.match(git(preparer, 'log', '--format=%G?', `${prepared.upstream}..${prepared.head}`).replace(/\n/g, ''), /^N+$/, 'the prepare job signs nothing');

  // The publish job runs on a fresh clone, from the captured dev.
  const clone = cloneFixture(fixture, 'publish');
  for (const [options, error] of [
    [{ signingKey: undefined }, /SSH_SIGNING_KEY is required/],
    [{ signingKey: readFileSync(wrongKeyPath, 'utf8') }, /does not match the trusted public key/],
  ]) {
    await assert.rejects(signClone(clone, directory, options), error);
    assert.equal(git(clone, 'rev-parse', 'HEAD'), fixture.devSha, 'a bad key stops before signing');
  }
  const result = await signClone(clone, directory, { signingKey: signingKey.trimEnd() });
  const { normalized } = prepared;
  assert.equal(remoteRefs(clone), before, 'signing publishes nothing');
  assert.equal(git(clone, 'status', '--porcelain'), '');
  assert.equal(git(clone, 'show', 'HEAD:shared.txt'), content({ target: 'resolved target', far: 'upstream far' }).trimEnd());
  assert.equal(git(clone, 'show', 'HEAD:Cargo.toml'), manifest('0.0.0').trimEnd(), "upstream's version bump is normalized away");

  // The normalized base is signed, keeps its trailer and sits directly on the captured upstream.
  assert.notEqual(result.base, normalized);
  assert.equal(git(clone, 'log', '-1', '--format=%P', result.base), fixture.upstreamSha);
  assert.equal(git(clone, 'rev-parse', `${result.base}^{tree}`), git(clone, 'rev-parse', `${normalized}^{tree}`));
  assert.match(git(clone, 'log', '-1', '--format=%B', result.base), new RegExp(`^Normalized-upstream: ${fixture.upstreamSha}$`, 'm'));
  verify(clone, result.base);

  const originals = git(clone, 'rev-list', '--reverse', `${fixture.baseSha}..${fixture.devSha}`).split('\n');
  const rewritten = git(clone, 'rev-list', '--reverse', `${result.base}..HEAD~1`).split('\n');
  assert.equal(rewritten.length, originals.length);
  assert.equal(result.signedCommits, originals.length);
  for (const [position, sha] of rewritten.entries()) {
    const original = rawCommit(clone, originals[position]);
    const replayed = rawCommit(clone, sha);
    assert.equal(header(replayed, 'author'), header(original, 'author'));
    assert.deepEqual(replayed.message, original.message, 'message bytes are preserved');
    const webFlow = header(original, 'committer').includes('<noreply@github.com>');
    assert.equal(
      header(replayed, 'committer'),
      webFlow ? header(original, 'committer').replace(/Committer <noreply@github\.com>/, `Rebase Signer <${SIGNER}>`) : header(original, 'committer'),
    );
    verify(clone, sha);
  }
  assert.match(git(clone, 'log', '--format=%s', `${result.base}..HEAD`), /originally empty dev commit/);

  // No release exists yet, so release-please starts after the new base.
  assert.equal(result.boundaryCommits, 1);
  assert.equal(git(clone, 'log', '-1', '--format=%s'), 'chore(gix): start release notes at the rebased upstream base');
  assert.equal(git(clone, 'show', 'HEAD:release-please-config.json') + '\n', releaseConfig(result.base));
  verify(clone, 'HEAD');
});

fixtureTest('publication requires the validated check, then saves dev and moves it atomically', async () => {
  const fixture = await plainFixture('publish');
  // The last replayed commit carries a release, so the rebase records its rewritten copy.
  git(fixture.remote, 'tag', 'gitoxide-fork-v0.1.0', fixture.devSha);
  const clone = cloneFixture(fixture, 'checkout');
  const { result } = await rebaseClone(clone);
  assert.equal(git(clone, 'log', '-1', '--format=%s'), 'chore(gix): point release-please at the rebased release');
  assert.equal(JSON.parse(git(clone, 'show', 'HEAD:release-please-config.json'))['last-release-sha'], git(clone, 'rev-parse', 'HEAD~1'));
  const rebasedSha = result.head;

  const runIds = { runId: '90001', runAttempt: '1' };
  const branch = await pushCandidate({ repositoryPath: clone, rebasedSha, ...runIds });
  assert.equal(branch, 'rebase/candidate/90001-1');
  assert.equal(git(fixture.remote, 'rev-parse', `refs/heads/${branch}`), rebasedSha);
  const failed = recordedApi(rebasedSha, branch, (r) => {
    r.check_runs.find((check) => check.id === 125964032734).conclusion = 'skipped';
    return r;
  });
  const refsBefore = remoteRefs(clone);
  await assert.rejects(publishRebase({ repositoryPath: clone, api: failed.api, devSha: fixture.devSha, rebasedSha, ...runIds }), /not validated: .*concluded skipped/);
  await assert.rejects(publishRebase({ repositoryPath: clone, api: recordedApi(rebasedSha, 'other').api, devSha: fixture.devSha, rebasedSha, ...runIds }), /not validated/);
  assert.equal(remoteRefs(clone), refsBefore, 'nothing is pushed without a validated check');

  const validated = recordedApi(rebasedSha, branch).api;
  const published = await publishRebase({ repositoryPath: clone, api: validated, devSha: fixture.devSha, rebasedSha, ...runIds });
  assert.equal(git(fixture.remote, 'rev-parse', published.backupRef), fixture.devSha);
  assert.equal(git(fixture.remote, 'rev-parse', 'refs/heads/dev'), rebasedSha);
  assert.equal(git(clone, 'ls-remote', '--refs', 'origin', `refs/heads/${branch}`), '', 'the candidate branch is deleted');

  // dev moves after the capture, as another push would move it.
  git(fixture.remote, 'update-ref', 'refs/heads/dev', git(clone, 'rev-parse', 'HEAD~1'));
  const retry = { runId: '90002', runAttempt: '1' };
  git(clone, 'push', '--quiet', 'origin', `${rebasedSha}:refs/heads/rebase/candidate/90002-1`);
  await assert.rejects(
    publishRebase({ repositoryPath: clone, api: recordedApi(rebasedSha, 'rebase/candidate/90002-1').api, devSha: fixture.devSha, rebasedSha, ...retry }),
    /Atomic backup\/rebase push failed/,
    'a stale dev lease rejects the whole push',
  );
  assert.equal(git(clone, 'ls-remote', '--refs', 'origin', 'refs/heads/backup/rebase-dev/90002-1'), '');
});

test('mapping and tree checks compare commits, not their SHAs', async () => {
  const repository = join(root, 'mapping');
  initRepo(repository);
  const make = (file, message, metadata = {}) => {
    writeFileSync(join(repository, file), `${file} ${message}\n`);
    commit(repository, message, metadata);
    return git(repository, 'rev-parse', 'HEAD');
  };
  const author = devMetadata('Author', 'author@example.invalid', '1700000000 +0000');
  make('base.txt', 'base');
  const original = [make('a.txt', 'first', author), make('b.txt', 'second', author)];
  git(repository, 'checkout', '--quiet', '--detach', 'HEAD~2');
  const rewritten = [make('a.txt', 'first', author), make('c.txt', 'second', author)];
  await verifyMapping(repository, original, rewritten);
  await assert.rejects(verifyMapping(repository, original, rewritten.slice(1)), /count changed \(2 original, 1 rewritten\)/);
  const reworded = make('d.txt', 'reworded', author);
  await assert.rejects(verifyMapping(repository, original, [rewritten[0], reworded]), /changed the author or message while mapping/);
  const reauthored = make('e.txt', 'second', devMetadata('Someone Else', 'else@example.invalid', '1700000000 +0000'));
  await assert.rejects(verifyMapping(repository, original, [rewritten[0], reauthored]), /changed the author or message/);

  await assertSameTree(repository, rewritten[1], rewritten[1]);
  await assertSameTree(repository, rewritten[1], reworded, ['d.txt']);
  await assert.rejects(assertSameTree(repository, original[1], rewritten[1]), /differs from the prepared tree in b\.txt, c\.txt/);
});

fixtureTest('the publish job verifies the handed-over replay instead of trusting it', async () => {
  const fixture = await plainFixture('verify-prepared');
  const preparer = cloneFixture(fixture, 'prepare');
  const { prepared } = await prepareClone(preparer);
  const publisher = cloneFixture(fixture, 'publish');
  let variant = 0;
  // Hands over `prepared` with changes, as a prepare job running hostile code could.
  const handOver = async (changes) => {
    const directory = join(fixture.directory, `variant-${(variant += 1)}`);
    await writePrepared({ repositoryPath: preparer, directory, prepared: { ...prepared, ...changes } });
    return directory;
  };
  // A commit on the prepared head, made with plumbing in the prepare clone.
  const amendHead = (message, edit = () => {}) => {
    git(preparer, 'checkout', '--quiet', '--detach', prepared.head);
    edit();
    git(preparer, 'add', '-A');
    const tree = git(preparer, 'write-tree');
    const raw = rawCommit(preparer, prepared.head);
    const authorLine = header(raw, 'author').replace(/^author /, '');
    const [, name, email, date] = /^(.*) <(.*)> (.*)$/.exec(authorLine);
    return gitWithEnv(preparer, { GIT_AUTHOR_NAME: name, GIT_AUTHOR_EMAIL: email, GIT_AUTHOR_DATE: date },
      'commit-tree', tree, '-p', `${prepared.head}~1`, '-m', message ?? raw.message.toString('utf8'));
  };

  await assert.rejects(loadPrepared({ repositoryPath: publisher, directory: await handOver({}), dev: prepared.base }), /this job runs/);
  for (const [changes, error] of [
    [{ upstream: prepared.base }, /captured upstream .* is not part of refs\/remotes\/origin\/upstream\/main/],
    [{ base: prepared.normalized }, /normalized base .* is not part of/],
    [{ head: amendHead('reworded message') }, /changed the author or message/],
    [{ head: prepared.normalized }, /prepared replay has 0 commits for 2 in dev/],
    [{ head: amendHead(undefined, () => writeFileSync(join(preparer, '.github', 'workflows', 'fork.yml'), 'on: workflow_dispatch\n')) },
      /fork-owned paths: \.github\/workflows\/fork\.yml/],
  ]) {
    await assert.rejects(signClone(publisher, await handOver(changes)), error);
    git(publisher, 'reset', '--quiet', '--hard');
  }
  assert.equal((await signClone(publisher, await handOver({}))).signedCommits, 2, 'the untouched handover signs');
});

test('post-replay checks run on the exact clean rebased SHA', async () => {
  const repository = join(root, 'check');
  initRepo(repository);
  writeFileSync(join(repository, 'tracked.txt'), 'original\n');
  commit(repository, 'check fixture');
  const sha = git(repository, 'rev-parse', 'HEAD');
  let checked = 0;
  await checkRebase({ repositoryPath: repository, rebasedSha: sha, runChecks: async () => { checked += 1; } });
  assert.equal(checked, 1);
  for (const [runChecks, error] of [
    [async () => { throw new Error('qa failed'); }, /qa failed/],
    [async () => writeFileSync(join(repository, 'tracked.txt'), 'changed\n'), /clean tracked/],
    [async () => writeFileSync(join(repository, 'untracked.txt'), 'input\n'), /clean tracked/],
    [async () => git(repository, 'commit', '--quiet', '--allow-empty', '-m', 'moved'), /HEAD differs/],
  ]) {
    await assert.rejects(checkRebase({ repositoryPath: repository, rebasedSha: sha, runChecks }), error);
    git(repository, 'reset', '--quiet', '--hard', sha);
    rmSync(join(repository, 'untracked.txt'), { force: true });
  }
});

fixtureTest('normalizer output must be a manifest-only child of the captured upstream on a base in dev', async () => {
  const fixture = await plainFixture('normalize');
  const clone = cloneFixture(fixture, 'checkout');
  const snapshots = await snapshotRefs({ repositoryPath: clone });
  const normalize = (output) => normalizeUpstream({ repositoryPath: clone, dev: snapshots.dev, upstream: snapshots.upstream, identity, runNormalizer: async () => output(clone) });
  const good = normalizeCommit(clone, fixture.upstreamSha);
  assert.deepEqual(await normalize(() => `base=${fixture.baseSha}\nnormalized=${good}\n`), { base: fixture.baseSha, normalized: good });
  await assert.rejects(normalize(() => `base=${fixture.baseSha}\n`), /Unexpected normalizer output/);
  await assert.rejects(normalize(() => `base=${fixture.upstreamSha}\nnormalized=${good}\n`), /not part of the captured dev/);
  await assert.rejects(normalize(() => `base=${fixture.baseSha}\nnormalized=${fixture.baseSha}\n`), /not a normalized commit of/);
  const tree = git(clone, 'rev-parse', `${good}^{tree}`);
  const quoted = git(clone, 'commit-tree', tree, '-p', fixture.upstreamSha, '-m', `quote\n\nNormalized-upstream: ${'a'.repeat(40)}`);
  await assert.rejects(normalize(() => `base=${fixture.baseSha}\nnormalized=${quoted}\n`), /not a normalized commit/);
  const wide = git(clone, 'commit-tree', git(clone, 'rev-parse', `${fixture.devSha}^{tree}`), '-p', fixture.upstreamSha, '-m', `wide\n\nNormalized-upstream: ${fixture.upstreamSha}`);
  await assert.rejects(normalize(() => `base=${fixture.baseSha}\nnormalized=${wide}\n`), /changes more than manifests: .*shared\.txt/);
});

fixtureTest('exact rename/delete resolutions replay and mismatched blobs fail closed', async () => {
  const fixture = await makeFixture('deletion', { deletion: true });
  const clone = cloneFixture(fixture, 'checkout');
  await rebaseClone(clone);
  assert.equal(existsSync(join(clone, 'renamed.patch')), false);

  const record = structuredClone(fixture.deletionRecords[0]);
  record.stages[1].oid = 'a'.repeat(40);
  writeFileSync(join(fixture.resolverPath, 'deletions.json'), JSON.stringify([record]));
  pushResolutions(fixture, 'record mismatched deletion');
  const rejected = cloneFixture(fixture, 'mismatch');
  await assert.rejects(rebaseClone(rejected), /index mismatch/);
  assert.match(git(rejected, 'ls-files', '-u'), /renamed\.patch/, 'the conflict is left for diagnosis');
});

fixtureTest("rename/rename records keep the fork's name and delete the others", async () => {
  const fixture = await makeFixture('rename-rename', { renameRename: true });
  assert.deepEqual(fixture.deletionRecords.map(({ path, action, stages }) => [path, action, stages[0].stage]), [
    ['old.patch', 'delete', 1],
    ['upstream.patch', 'delete', 2],
    ['fork.patch', 'keep', 3],
  ]);
  const clone = cloneFixture(fixture, 'checkout');
  await rebaseClone(clone);
  assert.deepEqual(git(clone, 'ls-tree', '--name-only', 'HEAD').split('\n').filter((name) => name.endsWith('.patch')), ['fork.patch']);
  assert.equal(git(clone, 'show', 'HEAD:fork.patch'), content().trimEnd());
});

fixtureTest('a commit left empty by deletion records replays as an empty commit', async () => {
  const fixture = await makeFixture('deletion-only', { deletion: true, deletionOnly: true });
  const clone = cloneFixture(fixture, 'checkout');
  await rebaseClone(clone);
  const replayed = git(clone, 'log', '--format=%H', '--grep=dev patch-only edit');
  assert.match(replayed, /^[0-9a-f]{40}$/);
  assert.equal(git(clone, 'show', '--format=%an', '--name-only', replayed), 'Patch Author');
});

fixtureTest("a pick a reused resolution settles on upstream's side replays as an empty commit", async () => {
  const fixture = await plainFixture('resolved-empty');
  const { seed, recorder } = fixture;
  git(seed, 'checkout', '--quiet', 'upstream');
  writeFileSync(join(seed, 'other.txt'), 'upstream superset\n');
  commit(seed, 'upstream adds other');
  git(seed, 'push', '--quiet', 'origin', 'HEAD:refs/heads/upstream/main');
  git(seed, 'checkout', '--quiet', 'dev');
  writeFileSync(join(seed, 'other.txt'), 'fork subset\n');
  commit(seed, 'fork adds other', devMetadata('Other Author', 'other-author@example.invalid', '1700000900 +0000'));
  git(seed, 'push', '--quiet', 'origin', 'HEAD:refs/heads/dev');

  // Record that the fork's pick resolves to upstream's file.
  git(recorder, 'fetch', '--quiet', 'origin');
  git(recorder, 'checkout', '--quiet', '--detach', 'origin/upstream/main');
  assert.throws(() => git(recorder, '-c', 'rerere.enabled=true', 'cherry-pick', 'origin/dev'));
  writeFileSync(join(recorder, 'other.txt'), 'upstream superset\n');
  git(recorder, '-c', 'rerere.enabled=true', 'rerere');
  git(recorder, 'cherry-pick', '--abort');
  await exportResolutions({ repositoryPath: recorder, destinationPath: fixture.resolverPath });
  pushResolutions(fixture, 'record upstream-side resolution');

  const clone = cloneFixture(fixture, 'checkout');
  const { result } = await rebaseClone(clone);
  assert.equal(git(clone, 'show', 'HEAD:other.txt'), 'upstream superset');
  const emptied = git(clone, 'log', '--format=%H %s', `${result.base}..HEAD`).split('\n').find((line) => line.endsWith(' fork adds other'));
  assert.ok(emptied);
  assert.equal(git(clone, 'diff-tree', '--no-commit-id', '--name-only', emptied.split(' ')[0]), '');
});

fixtureTest('a newly empty replay stops with rebase state intact', async () => {
  const fixture = await plainFixture('newly-empty');
  advanceUpstream(fixture, 'same far', 'upstream makes the change dev makes next');
  extendDev(fixture, (seed) => {
    writeFileSync(join(seed, 'shared.txt'), content({ target: 'dev target', far: 'same far' }));
    commit(seed, 'dev patch now empty against upstream', devMetadata('Emptying Author', 'emptying@example.invalid', '1700000500 +0000'));
  });
  const clone = cloneFixture(fixture, 'checkout');
  const before = remoteRefs(clone);
  await assert.rejects(rebaseClone(clone), /without a safely reusable completed resolution/);
  assert.match(git(clone, 'status'), /rebase in progress/);
  assert.equal(remoteRefs(clone), before);
});

fixtureTest('a schema-only resolution branch is valid and an unknown conflict stops diagnosably', async () => {
  const fixture = await plainFixture('schema-only');
  git(fixture.resolverPath, 'rm', '-r', '--quiet', 'resolutions');
  pushResolutions(fixture, 'initialize empty resolution cache');
  assert.deepEqual(git(fixture.resolverPath, 'ls-tree', '-r', '--name-only', 'HEAD'), 'schema.json');
  const clone = cloneFixture(fixture, 'checkout');
  await assert.rejects(rebaseClone(clone), /without a safely reusable completed resolution/);
  assert.match(git(clone, 'ls-files', '-u'), /shared\.txt/);
  // Any failing step writes bounded diagnostics, including the conflict left in the checkout.
  const failed = spawnSync(process.execPath, [new URL('./rebase-dev.mjs', import.meta.url).pathname, 'check'], {
    cwd: clone,
    encoding: 'utf8',
    env: { ...process.env, RUNNER_TEMP: fixture.directory, REBASED_SHA: 'b'.repeat(40) },
  });
  assert.equal(failed.status, 1);
  const text = readFileSync(join(fixture.directory, 'rebase-dev-diagnostics.txt'), 'utf8');
  assert.match(text, /Captured refs: DEV_SHA=/);
  assert.match(text, /diff --cc shared\.txt/, 'diagnostics include the conflict');
});

fixtureTest('an upstream fixup applies exactly as one signed commit or stops the run', async () => {
  const fixture = await plainFixture('fixup');
  mkdirSync(join(fixture.resolverPath, 'fixups'));
  const attempt = (name, patch) => {
    writeFileSync(join(fixture.resolverPath, 'fixups', `${fixture.upstreamSha}.patch`), patch);
    pushResolutions(fixture, name);
    const clone = cloneFixture(fixture, name);
    return { clone, result: rebaseClone(clone) };
  };
  await assert.rejects(attempt('stale', '--- a/fixup.txt\n+++ b/fixup.txt\n@@ -1 +1 @@\n-stale\n+adapted\n').result, /does not apply/);
  const { clone, result } = attempt('applies', 'diff --git a/fixup.txt b/fixup.txt\nnew file mode 100644\n--- /dev/null\n+++ b/fixup.txt\n@@ -0,0 +1 @@\n+adapted\n');
  assert.equal((await result).result.fixupCommits, 1);
  assert.equal(git(clone, 'show', 'HEAD:fixup.txt'), 'adapted');
  assert.equal(git(clone, 'log', '-1', '--format=%s', 'HEAD~1'), 'chore(gix): adapt incorporated upstream to the fork');
  verify(clone, 'HEAD~1');
});

fixtureTest('a workflow added upstream stops the run until a fixup deletes it', async () => {
  const fixture = await plainFixture('inherited-workflow');
  const { seed } = fixture;
  git(seed, 'checkout', '--quiet', 'upstream');
  mkdirSync(join(seed, '.github', 'workflows'), { recursive: true });
  writeFileSync(join(seed, '.github', 'workflows', 'upstream.yml'), 'on: push\n');
  commit(seed, 'ci: add upstream workflow');
  git(seed, 'push', '--quiet', 'origin', 'HEAD:refs/heads/upstream/main');
  const upstreamSha = git(seed, 'rev-parse', 'HEAD');
  await assert.rejects(rebaseClone(cloneFixture(fixture, 'inherited')), /fork-owned paths: \.github\/workflows\/upstream\.yml/);

  mkdirSync(join(fixture.resolverPath, 'fixups'));
  writeFileSync(
    join(fixture.resolverPath, 'fixups', `${upstreamSha}.patch`),
    'diff --git a/.github/workflows/upstream.yml b/.github/workflows/upstream.yml\ndeleted file mode 100644\n--- a/.github/workflows/upstream.yml\n+++ /dev/null\n@@ -1 +0,0 @@\n-on: push\n',
  );
  pushResolutions(fixture, 'fixup: drop upstream workflow');
  const clone = cloneFixture(fixture, 'dropped');
  assert.equal((await rebaseClone(clone)).result.fixupCommits, 1);
  assert.equal(git(clone, 'ls-tree', '--name-only', 'HEAD', '--', '.github/workflows/'), '.github/workflows/fork.yml');
});

fixtureTest('a merge commit in dev stops before the replay', async () => {
  const fixture = await plainFixture('merge');
  extendDev(fixture, (seed) => {
    git(seed, 'checkout', '--quiet', '-b', 'side');
    writeFileSync(join(seed, 'side.txt'), 'side\n');
    commit(seed, 'side edit', devMetadata('Side Author', 'side@example.invalid', '1700000600 +0000'));
    git(seed, 'checkout', '--quiet', 'dev');
    gitWithEnv(seed, devMetadata('Side Author', 'side@example.invalid', '1700000800 +0000'), 'merge', '--quiet', '--no-ff', '-m', 'merge side', 'side');
  });
  const clone = cloneFixture(fixture, 'checkout');
  await assert.rejects(rebaseClone(clone), /contains merge commits; linearize dev by hand/);
  assert.equal(git(clone, 'rev-parse', 'HEAD'), fixture.devSha);
});

// A repository whose rr-cache holds one completed record, and an empty resolution checkout.
function resolutionRepos(name, preimage = 'preimage\n', postimage = 'postimage\n') {
  const source = join(root, name, 'source');
  const resolver = join(root, name, 'resolver');
  const id = 'b'.repeat(40);
  initRepo(source);
  const cache = join(source, '.git', 'rr-cache', id);
  mkdirSync(cache, { recursive: true });
  writeFileSync(join(cache, 'preimage'), preimage);
  writeFileSync(join(cache, 'postimage'), postimage);
  initRepo(resolver);
  git(resolver, 'checkout', '--quiet', '-b', 'rebase-resolutions');
  return { source, resolver, id, cache };
}

test('resolution export and import preserve arbitrary Git blob bytes', async () => {
  const preimage = Buffer.from([0xff, 0x00, 0x80, 0xc3, 0x28]);
  const postimage = Buffer.from([0xfe, 0x00, 0x81, 0xe2, 0x82]);
  const { source, resolver, id } = resolutionRepos('binary', preimage, postimage);
  assert.equal(await exportResolutions({ repositoryPath: source, destinationPath: resolver }), 1);
  commit(resolver, 'export binary resolution pair');
  const receiver = join(root, 'binary', 'receiver');
  initRepo(receiver);
  git(receiver, 'fetch', '--quiet', resolver, 'rebase-resolutions');
  assert.equal(await importResolutions(receiver, git(receiver, 'rev-parse', 'FETCH_HEAD')), 1);
  assert.deepEqual(readFileSync(join(receiver, '.git', 'rr-cache', id, 'preimage')), preimage);
  assert.deepEqual(readFileSync(join(receiver, '.git', 'rr-cache', id, 'postimage')), postimage);
});

test('export and import refuse symlinked rerere variants', async () => {
  const { source, resolver, id, cache } = resolutionRepos('symlink');
  writeFileSync(join(cache, 'preimage.saved'), readFileSync(join(cache, 'preimage')));
  unlinkSync(join(cache, 'preimage'));
  symlinkSync(join(cache, 'preimage.saved'), join(cache, 'preimage'));
  await assert.rejects(exportResolutions({ repositoryPath: source, destinationPath: resolver }), /not a regular file/);
  assert.equal(existsSync(join(resolver, 'schema.json')), false, 'nothing is written before validation');

  // A committed symlink has mode 120000 and must fail import before any rr-cache write.
  writeFileSync(join(resolver, 'schema.json'), '{"version":1,"format":"git-rerere-cache-v1"}\n');
  mkdirSync(join(resolver, 'resolutions', id), { recursive: true });
  writeFileSync(join(resolver, 'resolutions', id, 'postimage'), 'postimage\n');
  symlinkSync('postimage', join(resolver, 'resolutions', id, 'preimage'));
  commit(resolver, 'add invalid resolution symlink');
  await assert.rejects(importResolutions(resolver, git(resolver, 'rev-parse', 'HEAD')), /Unsupported path or file mode/);
  assert.equal(existsSync(join(resolver, '.git', 'rr-cache')), false);
});
