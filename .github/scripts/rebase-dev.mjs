// Rebase fork `dev` onto a normalized copy of the upstream mirror; see FORK.md for the workflow.
//
// Subcommands, each one workflow step. The prepare job runs upstream-controlled code (Cargo reads
// upstream's `.cargo/config.toml`), so it has no secrets:
//   snapshot   capture dev, upstream/main and rebase-resolutions, and check out the captured dev
//   normalize  normalize the captured upstream with the captured dev's normalizer
//   replay     replay dev onto it, unsigned, with known resolutions and the fixup
//   check      check the replayed manifests and lockfile
//   bundle     write the replayed commits and the captured SHAs for the publish job
// The publish job, on a fresh runner, runs only Git and Node against the replayed tree:
//   sign       verify the bundle against the captured refs, then sign it and add the release boundary
//   candidate  push the signed tip to rebase/candidate/<run>-<attempt>
//   validate   dispatch Fork CI on the candidate and wait for `Fork CI required`
//   publish    re-check that result, then push the backup ref and dev atomically
// And by hand:
//   import SHA / export DIR   copy rerere records between a checkout and the resolution branch
import { appendFile, chmod, lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { isDeepStrictEqual, promisify } from 'node:util';
import { execFile, spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';

const execFileAsync = promisify(execFile);
const DEV_REF = 'refs/heads/dev';
const UPSTREAM_REF = 'refs/heads/upstream/main';
const RESOLUTION_REF = 'refs/heads/rebase-resolutions';
const CANDIDATE_PREFIX = 'rebase/candidate/';
const CACHE_PREFIX = 'resolutions/';
const SCHEMA_PATH = 'schema.json';
const SCHEMA_TEXT = '{"version":1,"format":"git-rerere-cache-v1"}\n';
const DELETIONS_PATH = 'deletions.json';
const FIXUPS_PREFIX = 'fixups/';
const WORKFLOWS_DIR = '.github/workflows/';
const RELEASE_PLEASE_CONFIG_PATH = 'release-please-config.json';
// Upstream owns nothing here, and these decide what runs with the repository's credentials.
const FORK_OWNED_PATHS = ['.github/workflows', '.github/scripts'];
const PREPARED_REF = 'refs/rebase-dev/prepared';
const FIXUP_MESSAGE = 'chore(gix): adapt incorporated upstream to the fork';
const NORMALIZER = 'etc/scripts/fork-normalize-upstream.sh';
const TRAILER = 'Normalized-upstream';
const FORK_CI = 'fork-ci.yml';
const REQUIRED_CHECK = 'Fork CI required';
// The GitHub Actions app; another app or a commit status could report the same name.
const GITHUB_ACTIONS_APP_ID = 15368;
const VALIDATION_TIMEOUT_MS = 110 * 60 * 1000;
const POLL_INTERVAL_MS = 30 * 1000;
// GitHub's web merges commit as GitHub and carry GitHub's signature, which no replay can keep.
const WEB_FLOW_EMAIL = 'noreply@github.com';
export const SIGNING_PUBLIC_KEY =
  'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJaetmY9mmMk+vBc6ZPxUFWb79Qh6B55pwUT4lsDj2Vi gitoxide fork rebase signing';

// Commits and rebases start detached auto-maintenance, whose rerere-gc task takes
// MERGE_RR.lock and races the next pick's rerere, failing it with "Unable to create
// MERGE_RR.lock: File exists".
const NO_AUTO_MAINTENANCE = ['-c', 'maintenance.auto=false'];
const STAGING_IDENTITY = [
  '-c', 'user.name=Rebase staging',
  '-c', 'user.email=rebase-staging@users.noreply.github.com',
  '-c', 'commit.gpgsign=false',
];

async function git(repositoryPath, args, options = {}) {
  try {
    const result = await execFileAsync('git', [...NO_AUTO_MAINTENANCE, ...args], {
      cwd: repositoryPath,
      encoding: 'utf8',
      env: { ...process.env, LC_ALL: 'C', ...options.env },
      maxBuffer: 8 * 1024 * 1024,
    });
    return { code: 0, stdout: result.stdout, stderr: result.stderr };
  } catch (error) {
    return {
      code: error.code === 'ENOENT' ? 127 : (error.code ?? 1),
      stdout: error.stdout ?? '',
      stderr: error.stderr ?? error.message,
    };
  }
}

async function gitChecked(repositoryPath, args, options) {
  const result = await git(repositoryPath, args, options);
  if (result.code !== 0) {
    throw new Error(`git ${args.join(' ')} failed (${result.code}): ${result.stderr.trim()}`);
  }
  return result.stdout;
}

async function gitLine(repositoryPath, args, options) {
  return (await gitChecked(repositoryPath, args, options)).trim();
}

async function gitBlob(repositoryPath, oid, type = 'blob') {
  try {
    const result = await execFileAsync('git', ['cat-file', type, oid], {
      cwd: repositoryPath,
      encoding: 'buffer',
      env: { ...process.env, LC_ALL: 'C' },
      maxBuffer: 8 * 1024 * 1024,
    });
    return result.stdout;
  } catch (error) {
    const stderr = Buffer.isBuffer(error.stderr) ? error.stderr.toString('utf8') : error.stderr;
    throw new Error(`git cat-file ${type} ${oid} failed: ${(stderr ?? error.message).trim()}`, {
      cause: error,
    });
  }
}

// Runs a command with its output in the job log; returns stdout when captured.
function run(command, args, { cwd, env, captureStdout = false } = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, args, {
      cwd,
      env: { ...process.env, ...env },
      stdio: ['ignore', captureStdout ? 'pipe' : 'ignore', 'inherit'],
    });
    let stdout = '';
    child.stdout?.on('data', (chunk) => { stdout += chunk; });
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (code === 0) resolveRun(stdout);
      else reject(new Error(`${command} ${args.join(' ')} failed (${signal ?? code})`));
    });
  });
}

const isSha = (value) => /^[0-9a-f]{40}$/.test(value ?? '');

function requireSha(name, value) {
  if (!isSha(value)) throw new Error(`A ${name} SHA is required`);
  return value;
}

// Apple Git 2.54 fails partway through a replay: a pick that reuses a rerere resolution
// cannot create MERGE_RR.lock. 2.55 replays it; the ubuntu-24.04 runners ship 2.56.
const MIN_GIT_VERSION = [2, 55];

export function assertGitVersion(versionText) {
  const match = /^git version (\d+)\.(\d+)/.exec(versionText.trim());
  if (!match) throw new Error(`Unrecognized git version: ${versionText.trim()}`);
  const [major, minor] = [Number(match[1]), Number(match[2])];
  const [minMajor, minMinor] = MIN_GIT_VERSION;
  if (major < minMajor || (major === minMajor && minor < minMinor)) {
    throw new Error(`${versionText.trim()} cannot replay dev; use git ${minMajor}.${minMinor} or later`);
  }
}

const EMAIL = /^[^\s<>,@]+@[^\s<>,@]+$/;

// The identity of commits the run creates, and the committer emails a replay may keep. GitHub
// marks a signed commit verified only when its committer email is a verified email of the
// account that registered the key, so only those may be kept.
export function signingIdentity(env = process.env) {
  const name = env.GITOXIDE_REBASE_SIGNING_NAME?.trim();
  const email = env.GITOXIDE_REBASE_SIGNING_EMAIL?.trim();
  if (!name || /[<>\n]/.test(name)) throw new Error('GITOXIDE_REBASE_SIGNING_NAME is required');
  if (!EMAIL.test(email ?? '')) throw new Error('GITOXIDE_REBASE_SIGNING_EMAIL is required');
  const keep = (env.GITOXIDE_REBASE_KEEP_COMMITTER_EMAILS ?? '')
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean);
  for (const entry of keep) {
    if (!EMAIL.test(entry)) throw new Error(`Invalid committer email to keep: ${entry}`);
  }
  return { name, email, keepEmails: new Set([email, ...keep]) };
}

function identityEnvironment({ name, email }) {
  return {
    GIT_AUTHOR_NAME: name,
    GIT_AUTHOR_EMAIL: email,
    GIT_COMMITTER_NAME: name,
    GIT_COMMITTER_EMAIL: email,
  };
}

function parseLsRemote(text) {
  const refs = new Map();
  for (const line of text.trim().split('\n')) {
    if (!line) continue;
    const [sha, ref] = line.split('\t');
    if (!isSha(sha) || refs.has(ref)) throw new Error(`Invalid or duplicate ls-remote row: ${line}`);
    refs.set(ref, sha);
  }
  return refs;
}

// Captures the three refs once, so every later step works from the same SHAs, and checks out
// the captured dev.
export async function snapshotRefs({ repositoryPath, remote = 'origin', expectedDev }) {
  assertGitVersion(await gitChecked(repositoryPath, ['--version']));
  if (await gitChecked(repositoryPath, ['status', '--porcelain'])) {
    throw new Error('Refusing to rebase a worktree with local changes');
  }
  const refs = parseLsRemote(
    await gitChecked(repositoryPath, ['ls-remote', '--refs', remote, DEV_REF, UPSTREAM_REF, RESOLUTION_REF]),
  );
  const snapshots = {
    dev: refs.get(DEV_REF),
    upstream: refs.get(UPSTREAM_REF),
    resolutions: refs.get(RESOLUTION_REF),
  };
  for (const [name, sha] of Object.entries(snapshots)) {
    if (!sha) throw new Error(`Required ${name} ref is missing on ${remote}`);
  }
  // The publish job runs the workflow's own commit, so it must be the dev that is rebased.
  if (expectedDev !== undefined && snapshots.dev !== expectedDev) {
    throw new Error(`dev moved to ${snapshots.dev} after this run started at ${expectedDev}; dispatch it again`);
  }
  await gitChecked(repositoryPath, [
    'fetch', '--no-tags', remote,
    `+${DEV_REF}:refs/rebase-dev/snapshot/dev`,
    `+${UPSTREAM_REF}:refs/rebase-dev/snapshot/upstream`,
    `+${RESOLUTION_REF}:refs/rebase-dev/snapshot/resolutions`,
  ]);
  for (const name of Object.keys(snapshots)) {
    if ((await gitLine(repositoryPath, ['rev-parse', `refs/rebase-dev/snapshot/${name}`])) !== snapshots[name]) {
      throw new Error(`${name} ref moved while snapshots were fetched; rerun from current refs`);
    }
  }
  await gitChecked(repositoryPath, ['checkout', '--quiet', '--detach', snapshots.dev]);
  return snapshots;
}

// A normalized commit has the upstream revision its trailer names as its only parent, and
// differs from it only in manifests and the lockfile.
async function assertNormalizedCommit(repositoryPath, sha, upstream) {
  const parents = await gitLine(repositoryPath, ['log', '-1', '--format=%P', sha]);
  const named = await gitLine(repositoryPath, ['log', '-1', `--format=%(trailers:key=${TRAILER},valueonly)`, sha]);
  if (parents !== named || (upstream !== undefined && parents !== upstream)) {
    throw new Error(`${sha} is not a normalized commit of ${upstream ?? 'its parent'}`);
  }
  const changed = (await gitChecked(repositoryPath, ['diff-tree', '-r', '-z', '--name-only', '--no-commit-id', parents, sha]))
    .split('\0')
    .filter(Boolean);
  const unexpected = changed.filter((path) => !/(^|\/)Cargo\.(toml|lock)$/.test(path));
  if (unexpected.length > 0) {
    throw new Error(`Normalized commit ${sha} changes more than manifests: ${unexpected.join(', ')}`);
  }
}

function runNormalizerScript(repositoryPath, upstream, dev, env) {
  return run('bash', [NORMALIZER, upstream, dev], { cwd: repositoryPath, env, captureStdout: true });
}

// The normalizer builds its `qa` binary from the checkout, so it must run on the captured dev.
// Its commit uses the signing identity; the rebase step signs it.
export async function normalizeUpstream({
  repositoryPath,
  dev,
  upstream,
  identity,
  runNormalizer = runNormalizerScript,
}) {
  requireSha('captured dev', dev);
  requireSha('captured upstream', upstream);
  if ((await gitLine(repositoryPath, ['rev-parse', 'HEAD'])) !== dev) {
    throw new Error('Normalize from a checkout of the captured dev');
  }
  const output = await runNormalizer(repositoryPath, upstream, dev, identityEnvironment(identity));
  const base = /^base=([0-9a-f]{40})$/m.exec(output)?.[1];
  const normalized = /^normalized=([0-9a-f]{40})$/m.exec(output)?.[1];
  if (!base || !normalized) throw new Error(`Unexpected normalizer output:\n${output}`);
  if ((await git(repositoryPath, ['merge-base', '--is-ancestor', base, dev])).code !== 0) {
    throw new Error(`Normalized base ${base} is not part of the captured dev`);
  }
  await assertNormalizedCommit(repositoryPath, base);
  await assertNormalizedCommit(repositoryPath, normalized, upstream);
  return { base, normalized };
}

function parseTree(text) {
  const entries = [];
  for (const row of text.split('\0')) {
    if (!row) continue;
    const match = /^(\d{6}) (blob) ([0-9a-f]{40})\t(.+)$/.exec(row);
    if (!match) throw new Error(`Unexpected resolution tree entry: ${JSON.stringify(row)}`);
    entries.push({ mode: match[1], oid: match[3], path: match[4] });
  }
  return entries;
}

export async function importResolutions(repositoryPath, resolutionSha) {
  requireSha('captured resolution-branch', resolutionSha);
  const entries = parseTree(await gitChecked(repositoryPath, ['ls-tree', '-r', '-z', '--full-tree', resolutionSha]));
  const schema = entries.find((entry) => entry.path === SCHEMA_PATH);
  if (!schema || schema.mode !== '100644') {
    throw new Error('Resolution branch must contain the regular schema.json file');
  }
  if ((await gitChecked(repositoryPath, ['cat-file', 'blob', schema.oid])) !== SCHEMA_TEXT) {
    throw new Error(`Unsupported resolution schema in ${SCHEMA_PATH}`);
  }
  const deletions = entries.find((entry) => entry.path === DELETIONS_PATH);
  if (deletions) {
    if (deletions.mode !== '100644') throw new Error('Unsupported path or file mode: deletions.json');
    parseDeletionResolutions(await gitChecked(repositoryPath, ['cat-file', 'blob', deletions.oid]));
  }
  for (const entry of entries.filter(({ path }) => path.startsWith(FIXUPS_PREFIX))) {
    if (!/^fixups\/[0-9a-f]{40}\.patch$/.test(entry.path) || entry.mode !== '100644') {
      throw new Error(`Unsupported path or file mode in resolution branch: ${entry.path}`);
    }
  }
  const cacheEntries = entries.filter(
    (entry) => ![SCHEMA_PATH, DELETIONS_PATH].includes(entry.path) && !entry.path.startsWith(FIXUPS_PREFIX),
  );
  const pairs = new Map();
  for (const entry of cacheEntries) {
    const match = /^resolutions\/([0-9a-f]{40})\/(preimage|postimage)(\.\d+)?$/.exec(entry.path);
    if (!match || entry.mode !== '100644') {
      throw new Error(`Unsupported path or file mode in resolution branch: ${entry.path}`);
    }
    const [, id, side, suffix = ''] = match;
    const key = `${id}/${suffix}`;
    const pair = pairs.get(key) ?? new Set();
    if (pair.has(side)) throw new Error(`Duplicate ${side} for ${key}`);
    pair.add(side);
    pairs.set(key, pair);
  }
  for (const [key, sides] of pairs) {
    if (sides.size !== 2) throw new Error(`Incomplete resolution pair: ${key}`);
  }

  const cacheRoot = resolve(repositoryPath, await gitLine(repositoryPath, ['rev-parse', '--git-path', 'rr-cache']));
  await mkdir(cacheRoot, { recursive: true });
  for (const entry of cacheEntries) {
    // Validated above: <40 hex>/<side>[.N], never a path from the branch used as-is.
    const destination = join(cacheRoot, entry.path.slice(CACHE_PREFIX.length));
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(destination, await gitBlob(repositoryPath, entry.oid), { flag: 'wx', mode: 0o600 });
  }
  return pairs.size;
}

export function parseDeletionResolutions(text) {
  const records = JSON.parse(text);
  if (!Array.isArray(records)) throw new Error('Deletion resolutions must be an array');
  const keys = new Set();
  for (const record of records) {
    const action = record?.action ?? 'delete';
    const shape = record?.stages?.map?.((entry) => entry?.stage).join();
    // A delete/modify conflict leaves stage 1 and one side. A rename/rename conflict leaves
    // each name at a single stage: delete the base and the losing name, keep the winner.
    const validShape = action === 'keep' ? ['2', '3'].includes(shape) : ['1,2', '1,3', '1', '2', '3'].includes(shape);
    if (
      !record ||
      !['commit,path,stages,upstream', 'action,commit,path,stages,upstream'].includes(Object.keys(record).sort().join()) ||
      !['delete', 'keep'].includes(action) ||
      !validShape ||
      !isSha(record.commit) ||
      !isSha(record.upstream) ||
      typeof record.path !== 'string' ||
      !record.path
        .split('/')
        .every((part) => /^[A-Za-z0-9_@+.-]+$/.test(part) && !['.', '..', '.git'].includes(part.toLowerCase())) ||
      record.stages.some(
        (entry) =>
          !entry || Object.keys(entry).sort().join() !== 'mode,oid,stage' || entry.mode !== '100644' || !isSha(entry.oid),
      )
    ) {
      throw new Error('Invalid exact deletion resolution');
    }
    const key = `${record.upstream}:${record.commit}:${record.path}`;
    if (keys.has(key)) throw new Error('Duplicate deletion resolution');
    keys.add(key);
  }
  return records;
}

async function loadDeletionResolutions(repositoryPath, resolutionSha) {
  if (!(await gitChecked(repositoryPath, ['ls-tree', resolutionSha, '--', DELETIONS_PATH]))) return [];
  return parseDeletionResolutions(await gitChecked(repositoryPath, ['show', `${resolutionSha}:${DELETIONS_PATH}`]));
}

// Records are keyed to the captured upstream, so a record made for another upstream never applies.
async function applyDeletionResolutions(repositoryPath, records, upstream) {
  const commit = await gitLine(repositoryPath, ['rev-parse', 'REBASE_HEAD']);
  const matching = records.filter((record) => record.commit === commit && record.upstream === upstream);
  for (const record of matching) {
    const actual = await gitChecked(repositoryPath, ['ls-files', '-u', '-z', '--', record.path]);
    const expected = record.stages.map(({ mode, oid, stage }) => `${mode} ${oid} ${stage}\t${record.path}\0`).join('');
    if (actual !== expected) throw new Error(`Deletion resolution index mismatch: ${record.path}`);
  }
  for (const { action, path, stages } of matching) {
    if (action === 'keep') {
      const [{ mode, oid }] = stages;
      await gitChecked(repositoryPath, ['update-index', '--cacheinfo', `${mode},${oid},${path}`]);
      await gitChecked(repositoryPath, ['checkout', '--', path]);
    } else {
      await gitChecked(repositoryPath, ['rm', '--quiet', '--', path]);
    }
  }
  return matching.length;
}

// The fork deleted upstream's workflows, and upstream keeps editing them. Such a modify/delete
// conflict leaves the base (stage 1) and upstream's edit (stage 2, the side being rebased onto),
// while the replayed fork commit deleted it (no stage 3). Resolve it by deletion when the path is
// a workflow the captured dev doesn't have, as `assertForkOwnedPathsUnchanged` requires anyway.
async function deleteInheritedWorkflows(repositoryPath, dev) {
  const stages = new Map();
  for (const entry of await listZ(repositoryPath, ['ls-files', '-u', '-z'])) {
    const separator = entry.indexOf('\t');
    const path = entry.slice(separator + 1);
    stages.set(path, [...(stages.get(path) ?? []), entry.slice(0, separator).split(' ')[2]]);
  }
  let deleted = 0;
  for (const [path, pathStages] of stages) {
    if (!path.startsWith(WORKFLOWS_DIR) || pathStages.sort().join() !== '1,2') continue;
    if (await gitChecked(repositoryPath, ['ls-tree', dev, '--', path])) continue;
    await gitChecked(repositoryPath, ['rm', '--quiet', '--', path]);
    deleted += 1;
  }
  return deleted;
}

async function listZ(repositoryPath, args) {
  return (await gitChecked(repositoryPath, args)).split('\0').filter(Boolean);
}

async function rebaseProgress(repositoryPath) {
  const head = await gitLine(repositoryPath, ['rev-parse', 'HEAD']);
  const current = await git(repositoryPath, ['rev-parse', '-q', '--verify', 'REBASE_HEAD']);
  const staged = await gitLine(repositoryPath, ['diff', '--cached', '--raw']);
  const unmerged = await gitLine(repositoryPath, ['ls-files', '-u']);
  return `${head}:${current.code === 0 ? current.stdout.trim() : 'none'}:${staged}:${unmerged}`;
}

function reusedRerereRecords(result) {
  return (`${result.stdout}\n${result.stderr}`.match(/^Staged '.+' using previous resolution\.$/gm) ?? []).length;
}

function isConflictStop(result) {
  const diagnostics = `${result.stdout}\n${result.stderr}`;
  return result.code !== 0 && /CONFLICT \(/.test(diagnostics) && /could not apply [0-9a-f]+/.test(diagnostics);
}

// Continues only conflict stops that known records resolved completely: Git reported staging a
// previous resolution (or a deletion record applied), nothing is unmerged, and the rebase moves.
async function continueKnownResolutions(repositoryPath, firstResult, maxContinuations, { deletionRecords, upstream, dev }) {
  let result = firstResult;
  for (let attempt = 0; attempt < maxContinuations; attempt += 1) {
    const inRebase = (await git(repositoryPath, ['rev-parse', '-q', '--verify', 'REBASE_HEAD'])).code === 0;
    const conflictStop = isConflictStop(result);
    const deletions = inRebase && conflictStop
      ? (await applyDeletionResolutions(repositoryPath, deletionRecords, upstream)) +
        (await deleteInheritedWorkflows(repositoryPath, dev))
      : 0;
    const conflicts = await listZ(repositoryPath, ['ls-files', '-u', '-z']);
    const staged = await listZ(repositoryPath, ['diff', '--cached', '--name-only', '-z']);
    // A commit that only changed paths upstream deleted is empty once they are deleted, and
    // a reviewed resolution can settle on upstream's side when upstream already made the
    // change. Keep it empty, with its own author and message, so the replay still maps 1:1.
    const resolvedByRecords = deletions > 0 || (conflictStop && reusedRerereRecords(result) > 0);
    const emptied = resolvedByRecords && conflicts.length === 0 && staged.length === 0;
    if (emptied) {
      await gitChecked(repositoryPath, [...STAGING_IDENTITY, 'commit', '--allow-empty', '--no-verify', '--cleanup=verbatim', '-C', 'REBASE_HEAD']);
    }
    if (!inRebase || !resolvedByRecords || conflicts.length > 0 || !(staged.length > 0 || emptied)) {
      throw new Error(`Rebase stopped without a safely reusable completed resolution:\n${result.stdout}\n${result.stderr}`);
    }
    if (!emptied) {
      // Commit the resolved pick ourselves: `rebase --continue` would commit it through the
      // editor path, whose message cleanup drops every line starting with `#`, such as the
      // Markdown headings our commit bodies use.
      await gitChecked(repositoryPath, [...STAGING_IDENTITY, 'commit', '--no-verify', '--cleanup=verbatim', '-C', 'REBASE_HEAD']);
    }
    const before = await rebaseProgress(repositoryPath);
    result = await git(repositoryPath, [
      '-c', 'rerere.enabled=true', '-c', 'rerere.autoupdate=true', ...STAGING_IDENTITY,
      '-c', 'core.editor=true', 'rebase', '--continue',
    ]);
    if (result.code === 0) return;
    if (before === (await rebaseProgress(repositoryPath))) {
      throw new Error('Rebase continuation made no progress; stopping safely');
    }
  }
  throw new Error(`Rebase exceeded the captured replay bound of ${maxContinuations} automatic continuations`);
}

// `dev` is linear: PRs land by rebase merge and upstream sits below the normalized base.
async function replayCommitIds(repositoryPath, base, head) {
  const range = `${base}..${head}`;
  if (await gitLine(repositoryPath, ['rev-list', '--merges', range])) {
    throw new Error(`${range} contains merge commits; linearize dev by hand`);
  }
  const output = await gitLine(repositoryPath, ['rev-list', '--reverse', '--topo-order', range]);
  return output ? output.split('\n') : [];
}

export function replayCommitter(original, sha, identity) {
  if (identity.keepEmails.has(original.committer.email)) return original.committer;
  if (original.committer.email !== WEB_FLOW_EMAIL) {
    throw new Error(`Cannot sign replay of ${sha} from unrecognized committer email ${original.committer.email}`);
  }
  const { date } = original.committer;
  const { name, email } = identity;
  return { line: `committer ${name} <${email}> ${date}`, name, email, date };
}

function parseCommitObject(raw, sha) {
  const separator = raw.indexOf(Buffer.from('\n\n'));
  if (separator < 0) throw new Error(`Commit ${sha} has no header/message separator`);
  let headerText;
  try {
    headerText = new TextDecoder('utf-8', { fatal: true }).decode(raw.subarray(0, separator));
  } catch {
    throw new Error(`Commit ${sha} has unsupported non-UTF-8 metadata`);
  }
  const headers = new Map();
  let previous = '';
  for (const line of headerText.split('\n')) {
    if (line.startsWith(' ')) {
      if (previous !== 'gpgsig') throw new Error(`Commit ${sha} has an unsupported continued header`);
      continue;
    }
    const separatorIndex = line.indexOf(' ');
    if (separatorIndex < 1) throw new Error(`Commit ${sha} has an invalid header`);
    const name = line.slice(0, separatorIndex);
    if (!['tree', 'parent', 'author', 'committer', 'encoding', 'gpgsig'].includes(name)) {
      throw new Error(`Commit ${sha} has an unsupported ${name} header`);
    }
    headers.set(name, [...(headers.get(name) ?? []), line.slice(separatorIndex + 1)]);
    previous = name;
  }
  const one = (name, required = true) => {
    const values = headers.get(name) ?? [];
    if (values.length > 1 || (required && values.length !== 1)) {
      throw new Error(`Commit ${sha} has an unsupported ${name} header count`);
    }
    return values[0];
  };
  if ((headers.get('parent') ?? []).length !== 1) throw new Error(`Commit ${sha} is not a single-parent commit`);
  const encoding = one('encoding', false);
  if (encoding && !/^[A-Za-z0-9._-]+$/.test(encoding)) throw new Error(`Commit ${sha} has an unsupported encoding header`);
  const parseIdentity = (field) => {
    const value = one(field);
    const match = /^(.*) <([^>]*)> (-?\d+ [+-]\d{4})$/.exec(value);
    if (!match) throw new Error(`Commit ${sha} has an unsupported ${field} identity`);
    return { line: `${field} ${value}`, name: match[1], email: match[2], date: match[3] };
  };
  return {
    author: parseIdentity('author'),
    committer: parseIdentity('committer'),
    encoding,
    message: raw.subarray(separator + 2),
  };
}

// A pick that stopped on a conflict is committed with message cleanup, which ends the
// message with a newline; GitHub web merges have none. Signing writes the original bytes.
function sameMessage(original, rewritten) {
  const trim = (message) => {
    let end = message.length;
    while (end > 0 && message[end - 1] === 0x0a) end -= 1;
    return message.subarray(0, end);
  };
  return trim(original).equals(trim(rewritten));
}

const trustedKey = (publicKey) => publicKey.trim().split(/\s+/).slice(0, 2).join(' ');

// Holds the private key in a 0600 temporary file for the duration of `use` only.
async function withSigner({ signingKey, publicKey, identity }, use) {
  if (!signingKey) throw new Error('GITOXIDE_REBASE_SSH_SIGNING_KEY is required');
  if (!signingKey.endsWith('\n')) signingKey += '\n';
  const trusted = trustedKey(publicKey);
  if (!/^ssh-ed25519 [A-Za-z0-9+/=]+$/.test(trusted)) throw new Error('Configured rebase SSH public key is invalid');
  const directory = await mkdtemp(join(process.env.RUNNER_TEMP ?? tmpdir(), 'gitoxide-rebase-sign-'));
  const keyPath = join(directory, 'signing-key');
  const allowedSignersPath = join(directory, 'allowed-signers');
  try {
    await writeFile(keyPath, signingKey, { flag: 'wx', mode: 0o600 });
    await chmod(keyPath, 0o600);
    let derived;
    try {
      derived = await execFileAsync('ssh-keygen', ['-y', '-f', keyPath], { encoding: 'utf8', env: { ...process.env, LC_ALL: 'C' } });
    } catch {
      throw new Error('GITOXIDE_REBASE_SSH_SIGNING_KEY is not a readable SSH private key');
    }
    if (trustedKey(derived.stdout) !== trusted) {
      throw new Error('GITOXIDE_REBASE_SSH_SIGNING_KEY does not match the trusted public key');
    }
    await writeFile(allowedSignersPath, `${[...identity.keepEmails].join(',')} ${trusted}\n`, { flag: 'wx', mode: 0o600 });
    return await use({ keyPath, allowedSignersPath, identity, messagePath: join(directory, 'commit-message') });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

// Rebuilds each rewritten commit on `parent` with its original author, committer and exact
// message, signs it and verifies the signature against the pinned key. Returns the new SHAs.
async function signChain(repositoryPath, signer, parent, pairs) {
  const signedShas = [];
  for (const { original: originalSha, rewritten: rewrittenSha } of pairs) {
    const original = parseCommitObject(await gitBlob(repositoryPath, originalSha, 'commit'), originalSha);
    const rewritten = parseCommitObject(await gitBlob(repositoryPath, rewrittenSha, 'commit'), rewrittenSha);
    if (original.author.line !== rewritten.author.line || !sameMessage(original.message, rewritten.message)) {
      throw new Error(`Rebase changed the author or message while mapping ${originalSha}`);
    }
    const committer = replayCommitter(original, originalSha, signer.identity);
    const tree = await gitLine(repositoryPath, ['rev-parse', `${rewrittenSha}^{tree}`]);
    await writeFile(signer.messagePath, original.message, { mode: 0o600 });
    const args = ['-c', 'gpg.format=ssh', '-c', `user.signingkey=${signer.keyPath}`];
    if (original.encoding) args.push('-c', `i18n.commitEncoding=${original.encoding}`);
    args.push('commit-tree', '-S', tree, '-p', parent, '-F', signer.messagePath);
    const signedSha = (await gitChecked(repositoryPath, args, {
      env: {
        GIT_AUTHOR_NAME: original.author.name,
        GIT_AUTHOR_EMAIL: original.author.email,
        GIT_AUTHOR_DATE: original.author.date,
        GIT_COMMITTER_NAME: committer.name,
        GIT_COMMITTER_EMAIL: committer.email,
        GIT_COMMITTER_DATE: committer.date,
      },
    })).trim();
    const signed = parseCommitObject(await gitBlob(repositoryPath, signedSha, 'commit'), signedSha);
    if (
      signed.author.line !== original.author.line ||
      signed.committer.line !== committer.line ||
      signed.encoding !== original.encoding ||
      !signed.message.equals(original.message)
    ) {
      throw new Error(`Signing changed replayed metadata or message for ${originalSha}`);
    }
    await gitChecked(repositoryPath, ['-c', 'gpg.format=ssh', '-c', `gpg.ssh.allowedSignersFile=${signer.allowedSignersPath}`, 'verify-commit', signedSha]);
    signedShas.push(signedSha);
    parent = signedSha;
  }
  return signedShas;
}

// Commits the index on HEAD as the signing identity, unsigned, and moves HEAD to it.
async function commitIndex(repositoryPath, identity, message) {
  const parent = await gitLine(repositoryPath, ['rev-parse', 'HEAD']);
  const tree = await gitLine(repositoryPath, ['write-tree']);
  const commit = await gitLine(repositoryPath, ['commit-tree', tree, '-p', parent, '-m', message], {
    env: identityEnvironment(identity),
  });
  await gitChecked(repositoryPath, ['reset', '--quiet', '--hard', commit]);
  return { parent, commit };
}

// Adds one signed commit on HEAD from the index, authored by the signing identity.
async function commitSignedIndex(repositoryPath, signer, message) {
  const { parent, commit } = await commitIndex(repositoryPath, signer.identity, message);
  const [signed] = await signChain(repositoryPath, signer, parent, [{ original: commit, rewritten: commit }]);
  await gitChecked(repositoryPath, ['reset', '--quiet', '--hard', signed]);
  return 1;
}

// Upstream owns nothing under the fork-owned paths, so a rebase must leave them exactly as the
// captured dev has them. Upstream edits to workflows dev deleted surface as conflicts, but a
// workflow upstream adds would replay silently and start running in the fork.
async function assertForkOwnedPathsUnchanged(repositoryPath, dev, head) {
  const changed = await listZ(repositoryPath, ['diff-tree', '-r', '-z', '--name-only', '--no-commit-id', dev, head, '--', ...FORK_OWNED_PATHS]);
  if (changed.length > 0) {
    throw new Error(
      `Rebase would change fork-owned paths: ${changed.join(', ')}. Delete upstream additions in ` +
        `${FIXUPS_PREFIX}<upstream sha>.patch on the resolution branch; change fork files in a PR.`,
    );
  }
}

async function hasUpstreamFixup(repositoryPath, { resolutions, upstream }) {
  return Boolean(await gitChecked(repositoryPath, ['ls-tree', resolutions, '--', `${FIXUPS_PREFIX}${upstream}.patch`]));
}

// Upstream code can break the fork without any textual conflict. A reviewed patch keyed to the
// exact captured upstream adapts it in one commit; a patch that does not apply stops the run.
// Both jobs apply it, so the publish job never takes the prepared fixup commit on trust.
async function stageUpstreamFixup(repositoryPath, { resolutions, upstream }) {
  if (!(await hasUpstreamFixup(repositoryPath, { resolutions, upstream }))) return false;
  const path = `${FIXUPS_PREFIX}${upstream}.patch`;
  const directory = await mkdtemp(join(process.env.RUNNER_TEMP ?? tmpdir(), 'gitoxide-fixup-'));
  try {
    const patchPath = join(directory, 'fixup.patch');
    await writeFile(patchPath, await gitBlob(repositoryPath, `${resolutions}:${path}`));
    const applied = await git(repositoryPath, ['apply', '--index', patchPath]);
    if (applied.code !== 0) throw new Error(`Upstream fixup ${path} does not apply: ${applied.stderr.trim()}`);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
  return true;
}

// The newest replayed commit that release-please may treat as the last release
// (a release tag or the configured last-release-sha) and its rewritten SHA.
export function releaseBoundary(originalCommits, rewrittenCommits, boundaryShas) {
  if (originalCommits.length !== rewrittenCommits.length) throw new Error('Release boundary needs every replayed commit mapped');
  const candidates = new Set(boundaryShas);
  for (let index = originalCommits.length - 1; index >= 0; index -= 1) {
    if (candidates.has(originalCommits[index])) return { original: originalCommits[index], rewritten: rewrittenCommits[index] };
  }
  return null;
}

export function parseReleaseTags(text) {
  const commits = new Map();
  for (const line of text.trim().split('\n')) {
    const match = /^([0-9a-f]{40})\trefs\/tags\/(gitoxide-fork-v\d+\.\d+\.\d+)(\^\{\})?$/.exec(line);
    // An annotated tag's peeled row names its commit; prefer it over the tag object.
    if (match && (match[3] || !commits.has(match[2]))) commits.set(match[2], match[1]);
  }
  return new Set(commits.values());
}

// release-please walks dev back to the last release, or to `bootstrap-sha` before the first
// one, and includes every commit it passes. The rebase rewrites both, so point the config at
// their new copies; otherwise the walk would continue into upstream history.
export function releaseConfigUpdate({ config, releaseCommits, originalCommits, rewrittenCommits, base }) {
  if (releaseCommits.size === 0) {
    if (config['bootstrap-sha'] === base) return null;
    return { key: 'bootstrap-sha', sha: base, message: 'chore(gix): start release notes at the rebased upstream base' };
  }
  const boundary = releaseBoundary(originalCommits, rewrittenCommits, [...releaseCommits, config['last-release-sha']]);
  if (!boundary) {
    throw new Error('No release tag or last-release-sha is among the replayed commits; set last-release-sha by hand');
  }
  if (boundary.rewritten === boundary.original || boundary.rewritten === config['last-release-sha']) return null;
  return { key: 'last-release-sha', sha: boundary.rewritten, message: 'chore(gix): point release-please at the rebased release' };
}

// Edits one SHA value in place, keeping the file's hand formatting; a missing key goes after
// `bootstrap-sha`.
export function setConfigSha(text, key, sha) {
  const existing = new RegExp(`^(\\s*"${key}": )"[0-9a-f]{40}"`, 'm');
  const next = existing.test(text)
    ? text.replace(existing, `$1"${sha}"`)
    : text.replace(/^(\s*)("bootstrap-sha": "[0-9a-f]{40}",\n)/m, `$1$2$1"${key}": "${sha}",\n`);
  if (!isDeepStrictEqual(JSON.parse(next), { ...JSON.parse(text), [key]: sha })) {
    throw new Error(`Could not set ${key} in ${RELEASE_PLEASE_CONFIG_PATH}`);
  }
  return next;
}

async function releaseTagCommits(repositoryPath, remote) {
  return parseReleaseTags(await gitChecked(repositoryPath, ['ls-remote', '--tags', remote, 'refs/tags/gitoxide-fork-v*']));
}

// The replay maps commit for commit, keeping each author and exact message.
export async function verifyMapping(repositoryPath, originalCommits, rewrittenCommits) {
  if (rewrittenCommits.length !== originalCommits.length) {
    throw new Error(`Replayed commit count changed (${originalCommits.length} original, ${rewrittenCommits.length} rewritten)`);
  }
  for (const [index, originalSha] of originalCommits.entries()) {
    const original = parseCommitObject(await gitBlob(repositoryPath, originalSha, 'commit'), originalSha);
    const rewritten = parseCommitObject(await gitBlob(repositoryPath, rewrittenCommits[index], 'commit'), rewrittenCommits[index]);
    if (original.author.line !== rewritten.author.line || !sameMessage(original.message, rewritten.message)) {
      throw new Error(`Rebase changed the author or message while mapping ${originalSha}`);
    }
  }
}

// Fails unless the two commits' trees differ at most in `allowedPaths`.
export async function assertSameTree(repositoryPath, expected, actual, allowedPaths = []) {
  const changed = await listZ(repositoryPath, ['diff-tree', '-r', '-z', '--name-only', '--no-commit-id', expected, actual]);
  const unexpected = changed.filter((path) => !allowedPaths.includes(path));
  if (unexpected.length > 0) throw new Error(`The signed tree differs from the prepared tree in ${unexpected.join(', ')}`);
}

async function assertAncestor(repositoryPath, ancestor, descendant, name) {
  if ((await git(repositoryPath, ['merge-base', '--is-ancestor', ancestor, descendant])).code !== 0) {
    throw new Error(`The ${name} ${ancestor} is not part of ${descendant}`);
  }
}

// Prepare job: replays dev onto the normalized upstream without signing.
export async function replayDev({ repositoryPath, snapshots, base, normalized, identity }) {
  const { dev, upstream, resolutions } = snapshots;
  for (const [name, sha] of Object.entries({ dev, upstream, resolutions, base, normalized })) requireSha(name, sha);
  if ((await gitLine(repositoryPath, ['rev-parse', 'HEAD'])) !== dev) throw new Error('Rebase from a checkout of the captured dev');
  if (await gitChecked(repositoryPath, ['status', '--porcelain'])) throw new Error('Refusing to rebase a worktree with local changes');
  const originalCommits = await replayCommitIds(repositoryPath, base, dev);
  await importResolutions(repositoryPath, resolutions);
  const deletionRecords = await loadDeletionResolutions(repositoryPath, resolutions);
  const rebaseResult = await git(repositoryPath, [
    '-c', 'rerere.enabled=true', '-c', 'rerere.autoupdate=true', ...STAGING_IDENTITY,
    'rebase', '--merge', '--keep-empty', '--reapply-cherry-picks', '--empty=stop', '--onto', normalized, base,
  ]);
  if (rebaseResult.code !== 0) {
    await continueKnownResolutions(repositoryPath, rebaseResult, originalCommits.length, { deletionRecords, upstream, dev });
  }
  await verifyMapping(repositoryPath, originalCommits, await replayCommitIds(repositoryPath, normalized, 'HEAD'));
  let fixupCommits = 0;
  if (await stageUpstreamFixup(repositoryPath, { resolutions, upstream })) {
    await commitIndex(repositoryPath, identity, FIXUP_MESSAGE);
    fixupCommits = 1;
  }
  await assertForkOwnedPathsUnchanged(repositoryPath, dev, 'HEAD');
  return { head: await gitLine(repositoryPath, ['rev-parse', 'HEAD']), fixupCommits };
}

const PREPARED_KEYS = ['base', 'dev', 'head', 'normalized', 'resolutions', 'upstream'];

// Hands the replayed commits and the captured SHAs to the publish job.
export async function writePrepared({ repositoryPath, directory, prepared }) {
  for (const key of PREPARED_KEYS) requireSha(key, prepared[key]);
  await gitChecked(repositoryPath, ['update-ref', PREPARED_REF, prepared.head]);
  await mkdir(directory, { recursive: true });
  await gitChecked(repositoryPath, ['bundle', 'create', '--quiet', join(directory, 'prepared.bundle'), PREPARED_REF, `^${prepared.upstream}`]);
  await writeFile(join(directory, 'prepared.json'), `${JSON.stringify(prepared, null, 2)}\n`);
}

// Publish job: reads what the prepare job handed over. `dev` is the commit this job checked out.
export async function loadPrepared({ repositoryPath, directory, dev }) {
  const prepared = JSON.parse(await readFile(join(directory, 'prepared.json'), 'utf8'));
  if (Object.keys(prepared).sort().join() !== PREPARED_KEYS.join()) throw new Error('Unexpected prepared.json fields');
  for (const key of PREPARED_KEYS) requireSha(key, prepared[key]);
  if (prepared.dev !== dev) throw new Error(`The prepared replay is of ${prepared.dev}, but this job runs ${dev}`);
  const bundle = join(directory, 'prepared.bundle');
  await gitChecked(repositoryPath, ['bundle', 'verify', '--quiet', bundle]);
  await gitChecked(repositoryPath, ['fetch', '--quiet', '--no-tags', bundle, `+${PREPARED_REF}:${PREPARED_REF}`]);
  if ((await gitLine(repositoryPath, ['rev-parse', PREPARED_REF])) !== prepared.head) {
    throw new Error('The bundle does not hold the prepared head');
  }
  return prepared;
}

// Publish job: nothing the prepare job reports is taken on trust, since it ran upstream code.
// The captured refs must be on the remote, the bases must be normalized commits, and the replay
// must map 1:1 to dev's commits. Signing rebuilds every commit; the fixup is applied again, and
// the result must have the prepared tree, which the prepare job checked.
export async function signPrepared({ repositoryPath, remote = 'origin', prepared, signingKey, publicKey = SIGNING_PUBLIC_KEY, identity }) {
  const { dev, upstream, resolutions, base, normalized, head } = prepared;
  for (const key of PREPARED_KEYS) requireSha(key, prepared[key]);
  return withSigner({ signingKey, publicKey, identity }, async (signer) => {
    if ((await gitLine(repositoryPath, ['rev-parse', 'HEAD'])) !== dev) throw new Error('Sign from a checkout of the captured dev');
    if (await gitChecked(repositoryPath, ['status', '--porcelain'])) throw new Error('Refusing to sign in a worktree with local changes');
    await assertAncestor(repositoryPath, upstream, `refs/remotes/${remote}/upstream/main`, 'captured upstream');
    await assertAncestor(repositoryPath, resolutions, `refs/remotes/${remote}/rebase-resolutions`, 'captured resolution commit');
    await assertAncestor(repositoryPath, base, dev, 'normalized base');
    await assertNormalizedCommit(repositoryPath, base);
    await assertNormalizedCommit(repositoryPath, normalized, upstream);
    const originalCommits = await replayCommitIds(repositoryPath, base, dev);
    const preparedCommits = await replayCommitIds(repositoryPath, normalized, head);
    const fixup = await hasUpstreamFixup(repositoryPath, { resolutions, upstream });
    if (preparedCommits.length !== originalCommits.length + (fixup ? 1 : 0)) {
      throw new Error(`The prepared replay has ${preparedCommits.length} commits for ${originalCommits.length} in dev`);
    }
    const replayedCommits = preparedCommits.slice(0, originalCommits.length);
    await verifyMapping(repositoryPath, originalCommits, replayedCommits);

    // The normalized base is signed too; its committer is the signing identity.
    const [signedBase] = await signChain(repositoryPath, signer, upstream, [{ original: normalized, rewritten: normalized }]);
    const signedCommits = await signChain(
      repositoryPath,
      signer,
      signedBase,
      originalCommits.map((original, index) => ({ original, rewritten: replayedCommits[index] })),
    );
    await gitChecked(repositoryPath, ['reset', '--quiet', '--hard', signedCommits.at(-1) ?? signedBase]);
    let fixupCommits = 0;
    if (fixup) {
      await stageUpstreamFixup(repositoryPath, { resolutions, upstream });
      fixupCommits = await commitSignedIndex(repositoryPath, signer, FIXUP_MESSAGE);
    }
    await assertSameTree(repositoryPath, head, 'HEAD');
    await assertForkOwnedPathsUnchanged(repositoryPath, dev, 'HEAD');

    const configText = await readFile(join(repositoryPath, RELEASE_PLEASE_CONFIG_PATH), 'utf8');
    const update = releaseConfigUpdate({
      config: JSON.parse(configText),
      releaseCommits: await releaseTagCommits(repositoryPath, remote),
      originalCommits,
      rewrittenCommits: signedCommits,
      base: signedBase,
    });
    let boundaryCommits = 0;
    if (update) {
      await writeFile(join(repositoryPath, RELEASE_PLEASE_CONFIG_PATH), setConfigSha(configText, update.key, update.sha));
      await gitChecked(repositoryPath, ['add', '--', RELEASE_PLEASE_CONFIG_PATH]);
      boundaryCommits = await commitSignedIndex(repositoryPath, signer, update.message);
    }
    await assertSameTree(repositoryPath, head, 'HEAD', boundaryCommits ? [RELEASE_PLEASE_CONFIG_PATH] : []);
    return {
      head: await gitLine(repositoryPath, ['rev-parse', 'HEAD']),
      base: signedBase,
      signedCommits: signedCommits.length,
      fixupCommits,
      boundaryCommits,
    };
  });
}

// Requires HEAD at the expected SHA with no tracked changes or untracked files.
async function assertCleanHead(repositoryPath, expectedSha) {
  if ((await gitLine(repositoryPath, ['rev-parse', 'HEAD'])) !== expectedSha) throw new Error('HEAD differs from the rebased SHA');
  if (await gitChecked(repositoryPath, ['status', '--porcelain=v1', '-z', '--untracked-files=all'])) {
    throw new Error('The rebased checkout must have a clean tracked and nonignored working tree');
  }
}

// The normalizer built `qa` from the captured dev. A stale lockfile fails `cargo metadata`
// only when it resolves dependencies, so `--no-deps` would not catch it.
async function runPostReplayChecks(repositoryPath) {
  await run(join(repositoryPath, 'target/debug/qa'), ['exec', 'manifests'], { cwd: repositoryPath });
  await run('cargo', ['metadata', '--locked', '--format-version', '1'], { cwd: repositoryPath });
}

export async function checkRebase({ repositoryPath, rebasedSha, runChecks = runPostReplayChecks }) {
  requireSha('rebased', rebasedSha);
  await assertCleanHead(repositoryPath, rebasedSha);
  await runChecks(repositoryPath);
  await assertCleanHead(repositoryPath, rebasedSha);
}

function runRefs(runId, runAttempt) {
  if (!/^\d+$/.test(runId ?? '') || !/^\d+$/.test(runAttempt ?? '')) throw new Error('Run id and attempt must be numeric');
  return {
    candidateBranch: `${CANDIDATE_PREFIX}${runId}-${runAttempt}`,
    backupRef: `refs/heads/backup/rebase-dev/${runId}-${runAttempt}`,
  };
}

export async function pushCandidate({ repositoryPath, remote = 'origin', rebasedSha, runId, runAttempt }) {
  const { candidateBranch } = runRefs(runId, runAttempt);
  await assertCleanHead(repositoryPath, rebasedSha);
  await gitChecked(repositoryPath, ['push', `--force-with-lease=refs/heads/${candidateBranch}:`, remote, `${rebasedSha}:refs/heads/${candidateBranch}`]);
  return candidateBranch;
}

// Decides from the API's workflow runs and check runs whether Fork CI validated exactly `sha`:
// the newest dispatched fork-ci.yml run on the candidate branch at that SHA must have a
// completed, successful `Fork CI required` from GitHub Actions in its own check suite.
export function evaluateCandidateChecks({ sha, branch, workflowRuns, checkRuns }) {
  const runs = workflowRuns.filter(
    (run) =>
      run.path === `${WORKFLOWS_DIR}${FORK_CI}` &&
      run.event === 'workflow_dispatch' &&
      run.head_branch === branch &&
      run.head_sha === sha,
  );
  if (runs.length === 0) return { state: 'pending', reason: `no dispatched Fork CI run for ${sha} on ${branch}` };
  const run = runs.reduce((newest, candidate) => (candidate.id > newest.id ? candidate : newest));
  const checks = checkRuns.filter(
    (check) =>
      check.name === REQUIRED_CHECK &&
      check.app?.id === GITHUB_ACTIONS_APP_ID &&
      check.check_suite?.id === run.check_suite_id,
  );
  if (checks.length === 0) {
    return run.status === 'completed'
      ? { state: 'failure', reason: `Fork CI run ${run.id} completed without ${REQUIRED_CHECK}` }
      : { state: 'pending', reason: `Fork CI run ${run.id} has not reported ${REQUIRED_CHECK}` };
  }
  const check = checks.reduce((newest, candidate) => (candidate.id > newest.id ? candidate : newest));
  if (check.head_sha !== sha) return { state: 'failure', reason: `${REQUIRED_CHECK} ${check.id} is for ${check.head_sha}, not ${sha}` };
  if (check.status !== 'completed') return { state: 'pending', reason: `${REQUIRED_CHECK} in run ${run.id} is ${check.status}` };
  if (check.conclusion !== 'success') {
    return { state: 'failure', reason: `${REQUIRED_CHECK} in run ${run.id} concluded ${check.conclusion}` };
  }
  return { state: 'success', reason: `${REQUIRED_CHECK} passed in Fork CI run ${run.id}`, runId: run.id };
}

export function githubApi({ token, repository, apiUrl = 'https://api.github.com', fetchImpl = fetch }) {
  if (!token) throw new Error('GITHUB_TOKEN is required');
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository ?? '')) throw new Error('GITHUB_REPOSITORY is required');
  return async (method, path, body) => {
    const response = await fetchImpl(`${apiUrl}/repos/${repository}${path}`, {
      method,
      headers: {
        accept: 'application/vnd.github+json',
        authorization: `Bearer ${token}`,
        'x-github-api-version': '2022-11-28',
        ...(body ? { 'content-type': 'application/json' } : {}),
      },
      body: body ? JSON.stringify(body) : undefined,
    });
    if (!response.ok) throw new Error(`${method} ${path} failed (${response.status}): ${await response.text()}`);
    return response.status === 204 ? null : response.json();
  };
}

export async function candidateState(api, sha, branch) {
  const runs = await api('GET', `/actions/workflows/${FORK_CI}/runs?branch=${encodeURIComponent(branch)}&event=workflow_dispatch&head_sha=${sha}&per_page=100`);
  const checks = await api('GET', `/commits/${sha}/check-runs?check_name=${encodeURIComponent(REQUIRED_CHECK)}&filter=all&per_page=100`);
  return evaluateCandidateChecks({ sha, branch, workflowRuns: runs.workflow_runs, checkRuns: checks.check_runs });
}

// Workflow-dispatch is the one event a GITHUB_TOKEN can trigger; dispatched runs are deep.
export async function validateCandidate({
  api,
  sha,
  branch,
  timeoutMs = VALIDATION_TIMEOUT_MS,
  intervalMs = POLL_INTERVAL_MS,
  sleep = (ms) => new Promise((done) => setTimeout(done, ms)),
  now = Date.now,
}) {
  requireSha('candidate', sha);
  await api('POST', `/actions/workflows/${FORK_CI}/dispatches`, { ref: branch });
  const deadline = now() + timeoutMs;
  for (;;) {
    const state = await candidateState(api, sha, branch);
    console.log(state.reason);
    if (state.state === 'success') return state;
    if (state.state === 'failure') throw new Error(state.reason);
    if (now() >= deadline) throw new Error(`Timed out waiting for Fork CI: ${state.reason}`);
    await sleep(intervalMs);
  }
}

// The dev ruleset's bypass lets this push land without the required check, so the check is
// enforced here, immediately before pushing exactly the validated SHA.
export async function publishRebase({ repositoryPath, remote = 'origin', api, devSha, rebasedSha, runId, runAttempt }) {
  requireSha('captured dev', devSha);
  requireSha('rebased', rebasedSha);
  const { candidateBranch, backupRef } = runRefs(runId, runAttempt);
  const state = await candidateState(api, rebasedSha, candidateBranch);
  if (state.state !== 'success') throw new Error(`Candidate is not validated: ${state.reason}`);
  await assertCleanHead(repositoryPath, rebasedSha);
  const candidateRef = `refs/heads/${candidateBranch}`;
  const command = await git(repositoryPath, [
    'push', '--atomic',
    `--force-with-lease=${DEV_REF}:${devSha}`,
    `--force-with-lease=${backupRef}:`,
    `--force-with-lease=${candidateRef}:${rebasedSha}`,
    remote,
    `${devSha}:${backupRef}`,
    `${rebasedSha}:${DEV_REF}`,
    `:${candidateRef}`,
  ]);
  if (command.code !== 0) throw new Error(`Atomic backup/rebase push failed (${command.code}): ${command.stderr.trim()}`);
  return { backupRef, rebasedSha };
}

async function ensureDirectory(path) {
  try {
    const entry = await lstat(path);
    if (!entry.isDirectory() || entry.isSymbolicLink()) throw new Error(`Export destination is not a regular directory: ${path}`);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await mkdir(path, { recursive: true });
  }
}

async function writeExportFile(path, contents) {
  try {
    const entry = await lstat(path);
    if (!entry.isFile() || entry.isSymbolicLink()) throw new Error(`Export target is not a regular file: ${path}`);
    if (!(await readFile(path)).equals(contents)) throw new Error(`Export target already exists with different content: ${path}`);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await writeFile(path, contents, { flag: 'wx', mode: 0o644 });
  }
}

async function readResolutionVariants(idDirectory, id) {
  const variants = new Map();
  for (const entry of await readdir(idDirectory, { withFileTypes: true })) {
    const match = /^(preimage|postimage)(\.\d+)?$/.exec(entry.name);
    if (!match) continue;
    if (entry.isSymbolicLink() || !entry.isFile()) throw new Error(`Rerere variant is not a regular file: ${id}/${entry.name}`);
    const [, side, suffix = ''] = match;
    const pair = variants.get(suffix) ?? {};
    pair[side] = await readFile(join(idDirectory, entry.name));
    variants.set(suffix, pair);
  }
  return variants;
}

export async function exportResolutions({ repositoryPath, destinationPath }) {
  if (!repositoryPath || !destinationPath) throw new Error('repositoryPath and destinationPath are required');
  if ((await gitLine(destinationPath, ['branch', '--show-current'])) !== 'rebase-resolutions') {
    throw new Error('Export destination must be checked out on rebase-resolutions');
  }
  if (await gitLine(destinationPath, ['status', '--porcelain'])) throw new Error('Export destination must have a clean worktree');
  const destination = resolve(destinationPath);
  const cacheRoot = resolve(repositoryPath, await gitLine(repositoryPath, ['rev-parse', '--git-path', 'rr-cache']));
  let cacheEntries = [];
  try {
    const rootStat = await lstat(cacheRoot);
    if (!rootStat.isDirectory() || rootStat.isSymbolicLink()) throw new Error('Git rr-cache must be a regular directory');
    cacheEntries = await readdir(cacheRoot, { withFileTypes: true });
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }

  const completed = [];
  for (const idEntry of cacheEntries) {
    if (!isSha(idEntry.name)) continue;
    if (idEntry.isSymbolicLink() || !idEntry.isDirectory()) throw new Error(`Rerere ID entry is not a regular directory: ${idEntry.name}`);
    for (const [suffix, pair] of await readResolutionVariants(join(cacheRoot, idEntry.name), idEntry.name)) {
      // Unfinished rerere records lack one side; export completed pairs only.
      if (pair.preimage && pair.postimage) completed.push({ id: idEntry.name, suffix, ...pair });
    }
  }

  await writeExportFile(join(destination, SCHEMA_PATH), Buffer.from(SCHEMA_TEXT));
  await ensureDirectory(join(destination, 'resolutions'));
  for (const pair of completed) {
    const idDirectory = join(destination, 'resolutions', pair.id);
    await ensureDirectory(idDirectory);
    for (const side of ['preimage', 'postimage']) await writeExportFile(join(idDirectory, `${side}${pair.suffix}`), pair[side]);
  }
  return completed.length;
}

async function writeDiagnostics(path, message, repositoryPath) {
  if (!path) return;
  const shas = ['DEV_SHA', 'UPSTREAM_SHA', 'RESOLUTIONS_SHA', 'BASE_SHA', 'NORMALIZED_SHA', 'PREPARED_SHA', 'REBASED_SHA']
    .map((name) => `${name}=${process.env[name] ?? ''}`);
  const status = await git(repositoryPath, ['status', '--short', '--branch']);
  const conflicts = await git(repositoryPath, ['ls-files', '-u']);
  const conflictDiff = await git(repositoryPath, ['diff', '--cc']);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(
    path,
    [
      `Captured refs: ${shas.join(' ')}`,
      `Status: ${message}`,
      'Git status:',
      status.stdout,
      'Unmerged index entries:',
      conflicts.stdout,
      'Conflict diff (truncated at 40000 characters):',
      conflictDiff.stdout.slice(0, 40_000),
    ].join('\n'),
  );
}

async function setEnvironment(values) {
  for (const [name, value] of Object.entries(values)) console.log(`${name}=${value}`);
  if (process.env.GITHUB_ENV) {
    await appendFile(process.env.GITHUB_ENV, Object.entries(values).map(([name, value]) => `${name}=${value}\n`).join(''));
  }
}

async function main() {
  const repositoryPath = process.cwd();
  const env = process.env;
  const [command, argument] = process.argv.slice(2);
  const preparedDirectory = join(env.RUNNER_TEMP ?? tmpdir(), 'rebase-dev-prepared');
  const api = () => githubApi({ token: env.GITHUB_TOKEN, repository: env.GITHUB_REPOSITORY, apiUrl: env.GITHUB_API_URL });
  switch (command) {
    case 'import':
      console.log(`Imported ${await importResolutions(repositoryPath, argument)} completed rerere pair(s)`);
      return;
    case 'export':
      console.log(`Exported ${await exportResolutions({ repositoryPath, destinationPath: argument })} completed rerere pair(s)`);
      return;
    case 'snapshot': {
      const { dev, upstream, resolutions } = await snapshotRefs({ repositoryPath, expectedDev: env.GITHUB_SHA });
      await setEnvironment({ DEV_SHA: dev, UPSTREAM_SHA: upstream, RESOLUTIONS_SHA: resolutions });
      return;
    }
    case 'normalize': {
      const { base, normalized } = await normalizeUpstream({ repositoryPath, dev: env.DEV_SHA, upstream: env.UPSTREAM_SHA, identity: signingIdentity() });
      await setEnvironment({ BASE_SHA: base, NORMALIZED_SHA: normalized });
      return;
    }
    case 'replay': {
      const result = await replayDev({
        repositoryPath,
        snapshots: { dev: env.DEV_SHA, upstream: env.UPSTREAM_SHA, resolutions: env.RESOLUTIONS_SHA },
        base: env.BASE_SHA,
        normalized: env.NORMALIZED_SHA,
        identity: signingIdentity(),
      });
      await setEnvironment({ PREPARED_SHA: result.head });
      return;
    }
    case 'check':
      await checkRebase({ repositoryPath, rebasedSha: env.PREPARED_SHA });
      return;
    case 'bundle':
      await writePrepared({
        repositoryPath,
        directory: preparedDirectory,
        prepared: {
          base: env.BASE_SHA,
          dev: env.DEV_SHA,
          head: env.PREPARED_SHA,
          normalized: env.NORMALIZED_SHA,
          resolutions: env.RESOLUTIONS_SHA,
          upstream: env.UPSTREAM_SHA,
        },
      });
      return;
    case 'sign': {
      const signingKey = env.GITOXIDE_REBASE_SSH_SIGNING_KEY;
      delete env.GITOXIDE_REBASE_SSH_SIGNING_KEY;
      const prepared = await loadPrepared({ repositoryPath, directory: preparedDirectory, dev: env.GITHUB_SHA });
      await setEnvironment({
        DEV_SHA: prepared.dev,
        UPSTREAM_SHA: prepared.upstream,
        RESOLUTIONS_SHA: prepared.resolutions,
        BASE_SHA: prepared.base,
        NORMALIZED_SHA: prepared.normalized,
        PREPARED_SHA: prepared.head,
      });
      const result = await signPrepared({ repositoryPath, prepared, signingKey, identity: signingIdentity() });
      console.log(`Signed ${result.signedCommits} replayed commit(s) on ${result.base}; ${result.fixupCommits} fixup and ${result.boundaryCommits} release-boundary commit(s)`);
      await setEnvironment({ REBASED_SHA: result.head });
      return;
    }
    case 'candidate': {
      const branch = await pushCandidate({ repositoryPath, rebasedSha: env.REBASED_SHA, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT });
      await setEnvironment({ CANDIDATE_BRANCH: branch });
      return;
    }
    case 'validate':
      await validateCandidate({ api: api(), sha: env.REBASED_SHA, branch: env.CANDIDATE_BRANCH });
      return;
    case 'publish': {
      const result = await publishRebase({
        repositoryPath,
        api: api(),
        devSha: env.DEV_SHA,
        rebasedSha: env.REBASED_SHA,
        runId: env.GITHUB_RUN_ID,
        runAttempt: env.GITHUB_RUN_ATTEMPT,
      });
      console.log(`Published backup ref ${result.backupRef} and rebased dev ${result.rebasedSha}`);
      return;
    }
    default:
      throw new Error(`Unknown subcommand: ${command}`);
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch(async (error) => {
    const diagnostics = process.env.RUNNER_TEMP && join(process.env.RUNNER_TEMP, 'rebase-dev-diagnostics.txt');
    if (!['import', 'export'].includes(process.argv[2])) {
      await writeDiagnostics(diagnostics, error.message, process.cwd()).catch(() => {});
    }
    console.error(error.stack ?? error.message);
    process.exitCode = 1;
  });
}
