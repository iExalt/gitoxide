# Developing the gitoxide fork

This fork supports acyclic's Git storage and synchronization backend. It lets us
develop in-process push without depending on upstream review capacity. Fork setup
does not implement push. The existing investigation and prototype remain separate
in the sibling `gitoxide-upstream` checkout. [FORK_STATUS.md](FORK_STATUS.md) tracks
iteration-speed and release work in progress, with its measurements.

## Branches and upstream updates

`origin` is <https://github.com/iExalt/gitoxide>; `upstream` is
<https://github.com/GitoxideLabs/gitoxide>. `dev` is the fork's default and integration branch.
Open development PRs against `iExalt/gitoxide:dev`, and rebase-merge them after checks
and review; the repository allows no other merge method, so `dev` stays linear. Preserve upstream history, copyright notices, and licenses.

Acyclic uses `main`; this fork follows Paseo's `dev` and `upstream/main` convention.
The latter is an upstream mirror, not a development target. The original `main`
branch remains at the fork's starting revision for reference; it is not maintained.
Upstream updates rebase `dev`, as Paseo's fork does, rather than merging. `dev` is a
normalized upstream commit with the fork's commits on top. Upstream versions every crate
separately, while the fork keeps them in lockstep (see [Fork releases](#fork-releases)), so the
base is not raw upstream but a commit whose only parent is the upstream revision and whose tree
changes only manifests and `Cargo.lock`, with a `Normalized-upstream:` trailer.
`mise run upstream:normalize [revision]` prints both `dev`'s current base and a newly normalized
commit for the revision (default `origin/upstream/main`); rebase with
`git rebase --onto <normalized> <base> dev`. Normalizing first keeps version bumps out of the
rebase: replaying the fork's renumbering onto raw upstream would conflict in most manifests on
every update. The normalizer runs `cargo update` inside the upstream revision, so upstream's
`.cargo/config.toml` applies, unlike the sync workflow, which never executes the mirrored tip.
Rebasing replaces published history, so announce it to anyone with branches from `dev`.

The `dev` ruleset lets the admin role and the sync App bypass it, including its required check.
Never merge a PR using that bypass: it exists only for the `rebase-dev` workflow's push and for
owner emergencies.

### The rebase workflow

`rebase-dev.yml` ("Rebase dev onto upstream") performs the update. Dispatch it from `dev` as
`iExalt`. Its two jobs run `.github/scripts/rebase-dev.mjs`, one subcommand per step, and nothing
is published unless every step succeeds.

Cargo reads upstream's `.cargo/config.toml`, which can run upstream-controlled programs (a
`build.rustc-wrapper`, for example). Such code could leave something behind on the runner for
later steps, so it runs only in the **prepare** job, which has no secrets and only reads the
repository:

1. **Capture.** It reads `dev`, `upstream/main` and `rebase-resolutions` once and works only from
   those SHAs. The captured `dev` must be the commit the run started from.
2. **Normalize.** The captured `dev`'s normalizer, `fork-normalize-upstream.sh <upstream> <dev>`,
   builds `qa` from that checkout and prints the current base and a new normalized commit. The
   script checks that both have the upstream commit their trailer names as their only parent and
   change only `Cargo.toml` files and `Cargo.lock`.
3. **Replay.** It imports the recorded resolutions and runs
   `git rebase --merge --keep-empty --reapply-cherry-picks --empty=stop --onto <normalized> <base>`.
   A stop is continued only when every conflict was resolved by a recorded resolution: Git's
   output says it staged a previous resolution, or a deletion record applied; nothing is
   unmerged; and the rebase moves on. Upstream keeps editing the workflows the fork deleted, so a
   modify/delete conflict resolves by deletion without a record when upstream modified the file,
   the replayed fork commit deleted it, it is under `.github/workflows/`, and the captured `dev`
   doesn't have it. When a reused resolution settles on upstream's side, the
   pick replays as an empty commit with its own author and message, keeping the replay one to
   one. A pick that becomes empty without a conflict, an unknown conflict or a merge commit in
   `dev` stops the run. A reviewed fixup patch (below) then becomes `chore(gix): adapt
   incorporated upstream to the fork`. The fork-owned `.github/workflows` and `.github/scripts`
   must end up exactly as in the captured `dev`, so a workflow upstream added stops the run.
4. **Check.** `target/debug/qa exec manifests` and `cargo metadata --locked` must pass on the
   result. `--no-deps` would skip dependency resolution, so it would miss a stale lockfile.
5. **Bundle.** A `git bundle` of the replayed commits and a file of the captured SHAs become the
   `rebase-dev-prepared` artifact.

The **publish** job starts on a fresh runner from the workflow's own commit, which is the captured
`dev`, and runs only Git and Node against the replayed tree. It installs Node with a pinned
`actions/setup-node` and a literal version, never mise, because no check protects `mise.toml` and
mise tool options can run commands; it then calls that Node binary by absolute path.
It alone holds the signing key, `actions: write`, `checks: read` and the App tokens:

6. **Sign.** It takes nothing from the prepare job on trust. The captured upstream and resolution
   commits must be on the remote branches. Both bases must be normalized commits, the old one in
   `dev`. The replay must map one to one onto `dev`'s commits with the same authors and messages.
   It then rebuilds the normalized commit and every replayed commit with their original author,
   timestamps and exact message bytes, signs them with the SSH key in
   `GITOXIDE_REBASE_SSH_SIGNING_KEY`, and verifies each signature against the public key pinned in
   the script. It applies the fixup again itself and requires the prepared tree, so the prepare
   job's checks still hold. It then adds the signed release-boundary commit, which changes only
   `release-please-config.json` (see [Fork releases](#fork-releases)). GitHub marks a commit
   verified only when its committer email is a verified email of the account that registered the
   key. A committer in `GITOXIDE_REBASE_KEEP_COMMITTER_EMAILS`, or the signing email, is kept
   exactly. GitHub's own web-flow committer (rebase merges made on GitHub) becomes the signing
   identity (`GITOXIDE_REBASE_SIGNING_NAME`, `GITOXIDE_REBASE_SIGNING_EMAIL`) at the original
   time. Any other committer stops the run.
7. **Validate.** It pushes the result to `rebase/candidate/<run-id>-<attempt>`, dispatches Fork CI
   there (dispatched runs are deep) and waits up to 110 minutes for its result. It does not run
   tests itself.
8. **Publish.** Because the ruleset bypass would accept the push either way, the script queries
   the candidate's check runs again and requires a completed, successful `Fork CI required` from
   the GitHub Actions app, in the check suite of the `fork-ci.yml` run dispatched on the candidate
   branch, for the exact candidate SHA. Missing, pending, failed, skipped or cancelled results
   stop the run. One atomic push then creates `backup/rebase-dev/<run-id>-<attempt>` at the
   captured `dev`, moves `dev` from exactly the captured SHA to the candidate SHA, and deletes the
   candidate branch.

A failed run writes its captured SHAs, Git status and conflict diff to the step summary and the
`rebase-dev-prepare-diagnostics` or `rebase-dev-publish-diagnostics` artifact. A candidate branch
left by a failed validation names the failed Fork CI run; delete it after diagnosis.

Credentials: the signing key is only in the sign step's environment, and the script deletes it
from its own environment before running Git. The publish job's `GITHUB_TOKEN` has
`contents: read`, `actions: write` (to dispatch Fork CI and read its runs; a dispatch is the one
event a `GITHUB_TOKEN` can trigger) and `checks: read`. `GITHUB_TOKEN` cannot push commits that
change workflow files, which every rebase does, so the sync App pushes: one token minted after
signing for the candidate push, and another minted after validation for the publish push.

### Resolution branch format

The `rebase-resolutions` branch holds reviewed resolutions; the run never writes to it. It may
contain only `schema.json`, a valid empty cache with which any conflict stops the run. That file
must contain exactly these bytes, followed by a newline:

```text
{"version":1,"format":"git-rerere-cache-v1"}
```

- `resolutions/<40-hex-id>/preimage` and `postimage`: completed Git `rerere` records, plus
  complete numbered pairs such as `preimage.1` and `postimage.1`. Only regular, non-executable
  files are allowed; no `thisimage`, unpaired variants or symlinks. The importer validates modes,
  paths and pairs before writing anything to `.git/rr-cache`. A record adapted to changed context
  applies when native `rerere` accepts it.
- `fixups/<captured-upstream-sha>.patch` (optional): a patch over the replayed tree that
  `git apply --index` must apply exactly, for upstream changes that break the fork without a
  textual conflict, or to delete a workflow upstream added. Only the patch named for the captured
  upstream applies, so a newer upstream needs a new patch. Build it in the reproduction checkout
  after the rebase finishes and save `git diff --no-ext-diff --binary HEAD`.
- `deletions.json` (optional): `rerere` cannot record modify/delete or rename/delete resolutions.
  Each record has `upstream` (captured upstream SHA), `commit` (the original `REBASE_HEAD`),
  `path` and `stages`, captured from `git ls-files -u -- <path>` before resolving as `mode`
  (only `100644`), `oid` and numeric `stage`. A modify/delete conflict leaves stage 1 and one of
  stages 2 or 3; the record deletes the path. A rename/rename conflict leaves each name at a single
  stage: record the base and the losing name for deletion, and the winner with
  `"action": "keep"`, which stages its exact blob. Every index entry must match, or the run stops.
  A commit whose only changes were to deleted paths replays as an empty commit.

### Reproducing a run and recording resolutions

Copy the captured SHAs from the failed run's artifact. Use Git 2.55 or later, as the runners do
(they ship 2.56); Apple Git 2.54 fails at the first pick that reuses a resolution with
`Unable to create '.git/MERGE_RR.lock': File exists`, and the script refuses older versions. The
normalizer needs the Rust toolchain, since it builds `qa`; its commit differs from the run's only
in its committer and date, so its tree and every conflict are the same.

```sh
git clone https://github.com/iExalt/gitoxide.git gitoxide-repro
cd gitoxide-repro
git fetch origin "$DEV_SHA" "$UPSTREAM_SHA" "$RESOLUTIONS_SHA"
git checkout --detach "$DEV_SHA"
eval "$(mise exec -- bash etc/scripts/fork-normalize-upstream.sh "$UPSTREAM_SHA" "$DEV_SHA")"
mise exec -- node .github/scripts/rebase-dev.mjs import "$RESOLUTIONS_SHA"
git -c rerere.enabled=true -c rerere.autoupdate=true rebase --merge --keep-empty \
    --reapply-cherry-picks --empty=stop --onto "$normalized" "$base"
```

Resolve each conflict by hand, stage only the resolved paths, record the resolution with
`git rerere`, and continue with `git -c core.editor=true rebase --continue`. Regenerate a
conflicted `Cargo.lock` rather than merging it by hand. Then export the completed records into a
separate checkout of the resolution branch, review the diff, and push it:

```sh
git clone --branch rebase-resolutions https://github.com/iExalt/gitoxide.git ../gitoxide-resolutions
mise exec -- node .github/scripts/rebase-dev.mjs export ../gitoxide-resolutions
git -C ../gitoxide-resolutions add schema.json resolutions
git -C ../gitoxide-resolutions commit -m "chore(gix): record rebase resolutions"
git -C ../gitoxide-resolutions push origin HEAD:refs/heads/rebase-resolutions
```

The exporter writes only complete regular-file pairs, refuses to overwrite different content, and
leaves `deletions.json` and fixups alone; add those by hand. Then dispatch the workflow again
from the current `dev`.

If the workflow cannot run, rebase by hand from a `dev` checkout: run
`mise run upstream:normalize [revision]`, `git rebase --onto <normalized> <base> dev`, push the
result to a candidate branch, dispatch Fork CI on it, check that `Fork CI required` passed for that
exact SHA, and then push a backup ref and `dev` with `--force-with-lease=dev:<old dev>`. Update
the release boundary as the workflow would.

`upstream-sync.yml` runs daily at 07:23 UTC (02:23 Toronto standard time;
03:23 daylight time) and on manual dispatch from `dev`. It updates only
`upstream/main`, using GitHub's atomic non-forced ref update. A diverged mirror
fails; the workflow never force-pushes, merges, or changes `dev`. The mirror must
exist before the first run and is initialized from upstream during fork setup.
The job checks out `dev` only; it never executes scripts from the mirrored tip.

## Tools and checks

Run `mise trust` and `mise install` after reviewing `mise.toml`.

| Command | Purpose |
| --- | --- |
| `mise run test` | Developer loop: nextest for crates changed since `origin/dev` and their dependents, plus affected isolated packages |
| `mise run check` | Affected-package QA, including workflow lint; global inputs select the workspace |
| `mise run check:thorough` | Exhaustive host checks, feature tests, docs, API tests and CLI journeys |
| `mise run check:msrv` | Install Rust 1.99.0 through mise and build both declared gix MSRV configurations |
| `mise run build` | Default CLI build, without publishing |
| `mise run lint:workflows` | Actionlint, embedded-shell ShellCheck and workflow script tests |
| `mise run release:preview` | Read-only release-please PR preview with `GH_TOKEN` |

`mise run test` is for quick iteration, not a substitute for `check`. It shares build
artifacts with the exhaustive native tests by unifying features across the whole workspace,
so it can hide a crate's missing feature declaration. It also skips doctests, feature and
hash variants, the archived-fixture pass and the guard against tests modifying source files.
`check` and CI keep all of those. QA commands set `CI=true`; plain `cargo test` without
it builds separate artifacts, so stick to the mise tasks to reuse their builds.

The tasks delegate to the existing `justfile` and QA planner. Feature selection
comes from Cargo QA metadata: small, lean, lean-async, max, max-pure and declared
crate-specific variants are distinct checks; `--all-features` is not a substitute.
Rust 1.99.1 is the development and API-check toolchain. Every crate declares MSRV 1.99, the root CLI included. Explicit task toolchains override
the inherited mise toolchain. Do not update the machine-wide `stable` alias to run QA.

Fork CI runs on Ubuntu 24.04 for every PR and push to `dev`, in one of two modes chosen
by `.github/scripts/qa-mode.cjs`:

- **Routine** (ordinary PRs): the planner's fast selection, meaning the packages a PR changes
  and every package that depends on them, with all of their declared feature and hash variants.
  It targets five minutes with warm caches. Global inputs such as `Cargo.toml` select the whole
  workspace.
- **Deep** (release PRs, pushes to `dev` and manual runs): the exhaustive `check:thorough` set.

The Linux lanes install `strace` and set `GIX_TEST_REQUIRE_STRACE`, so that the tests which compare the flushes of the
fork with those of Git (`gix_testtools::strace`) fail on a runner that can't trace instead of skipping. Elsewhere they skip.

Routine runs leave these to deep runs: the archived-fixture test pass, which detects stale
committed archives; variants, journeys and API checks of packages a PR doesn't affect; and the
exhaustive-only feature contracts. A failing deep run on a release PR blocks the release.

`mise run ci:plan` gives the selection to the QA planner. The planner turns each job group into a
lane and splits the groups named in `mise.toml`'s `ci_shards` into several lanes, never more than
the group has independent cases; `ci_combine` runs small groups as one lane. Workflow lint (`mise run lint:workflows`) runs in the lane that holds housekeeping rather than in the plan job, because planning doesn't depend on it. Each lane runs `mise run ci:job` with its own Rust cache, which
only `dev` pushes save. The plan job fetches two commits: a PR's merge ref has the base tip as
its first parent, fast selection compares against it, and `--require-base` fails the plan
instead of silently testing everything. The `Fork MSRV`
job runs in both modes. `Fork CI required` passes only when the planner, every lane and MSRV
succeeded; a skipped or cancelled job fails it. It is the only check the `dev` ruleset ("dev requires Fork CI") requires; only the admin role and the sync App may bypass the ruleset (see [Branches and upstream updates](#branches-and-upstream-updates)).
Workflow actions are pinned to commit IDs; checkout credentials are
not persisted. All six inherited workflows were removed from development, including upstream
publishing, schedules, CodeQL uploads, fuzzing, and security-analysis jobs.
Their GitHub workflow registrations are also disabled, so copies on historical
branches and the mirror cannot run accidentally. Only fork-owned workflows are
enabled; revisit this if upstream adds a new workflow filename.

This deliberately smaller CI does not reproduce upstream's cross-platform and
32-bit runners, downstream consumer builds, Docker pure-Rust check, package-size
audit, cargo-auditable build, or nightly minimal-dependency MSRV resolution.
The existing `just`/QA entry points remain available for those investigations.
The fork MSRV job checks the committed dependency lockfile. Dependency policy
(bans, licenses and sources) remains blocking. Advisory scanning remains available
through `just audit`; there is no scheduled advisory job. `mise.toml` is the fork's installation authority. Shared tools in
`workspace.metadata.qa.tool-versions` must match those exact pins; housekeeping
checks them before running workflow lint. The catalog includes workflow lint's
Node, actionlint and ShellCheck requirements, including empty fast selections and
combined lanes. The MSRV task keeps Rust 1.99.0 to test the declared minimum.

Catalog tool requirements describe the commands in each QA lane, rather than a
complete runner bootstrap: Mise, Git and the development Rust toolchain are
provided separately, as are OS packages such as strace. The planner and required
jobs also invoke Node outside the catalog. QA-only tools such as cargo-auditable
and cargo-diet retain their own metadata pins. CI still installs the complete Mise
tool set; no selective installation is derived from this metadata.

## Fork releases

Every crate shares one fork version, restarted from upstream's numbering because the fork
is permanent. `[workspace.package] version` in the root `Cargo.toml` holds it; every member
inherits it with `version.workspace = true`, and dependencies between workspace crates are
path-only. `qa exec manifests` (run by the housekeeping stage) fails when a manifest breaks
that rule or when the workspace version differs from `.release-please-manifest.json`; add
`--fix` to repair the manifests. All crates are `publish = false`: their names belong to
upstream on crates.io.

Release-please uses its `simple` strategy with `fork-version.txt` and
`.release-please-manifest.json`, and its TOML updaters bump the workspace version in
`Cargo.toml` and every workspace package in `Cargo.lock`. The initial unreleased version is
`0.0.0`; the first feature release is `0.1.0`, whose release PR is also the first proof that
those updaters work. Tags use `gitoxide-fork-v0.1.0`. Generated notes go to
`CHANGELOG.fork.md`; upstream `CHANGELOG.md` files remain untouched. There is no crates.io
publication and there are no binary packages. The user agent (`git/oxide-<version>`) and
`gix --version` report the fork version.

The bootstrap boundary is `5c38bd661867c542298d8cbe9b9ffae67da6ecac`, the normalized
fork point `dev` sits on, so upstream commits are excluded from the first fork release.
Because `dev` is linear with upstream below the fork's commits, release-please never walks into
upstream history. A rebase that moves `dev` onto a newer base before the first release must move
`bootstrap-sha` to the new base, and one that rewrites a released commit must set
`last-release-sha` to its rewritten copy. `rebase-dev` does both in a signed commit: without a
`gitoxide-fork-v*` tag it sets `bootstrap-sha` to the new signed base; otherwise it finds the
newest replayed commit that a release tag or `last-release-sha` names and points
`last-release-sha` at its copy, and stops if there is none, since release-please would then walk
into upstream history. Until the first release, every rebase therefore adds one signed
`bootstrap-sha` commit; they accumulate on `dev`, which is harmless, and stop once 0.1.0 is
tagged. Conventional commit
titles describe fork changes; breaking changes require explicit review even while
the fork version is below 1.0.

The workflow maintains a release PR and, once one is merged, tags the merge commit
`gitoxide-fork-v<version>` and publishes its GitHub release on the next `dev` run. Merging a
release PR is therefore the publishing act. Do not enable release-PR auto-merge. Before merging
a release PR, obtain explicit owner approval naming the version, and review the PR's deep CI run
and changelog. The manifest alone does not mean a release has been published; the tag and
GitHub release do. If the release run fails, the merged PR keeps its `autorelease: pending` label
and release-please opens no new release PRs until it is tagged: fix the cause and re-run the
workflow by dispatch. Remove that label by hand only if the tag already exists.

### GitHub App setup

Install a release GitHub App on **iExalt/gitoxide** with repository Contents,
Pull requests, and Issues read/write permissions. Set repository variable
`GITOXIDE_RELEASE_APP_CLIENT_ID` and repository Actions secret
`GITOXIDE_RELEASE_APP_PRIVATE_KEY`. The workflow scopes its installation token to
this repository and those permissions. No credentials are copied from acyclic or
Paseo. The job is skipped until the client ID is configured; a missing or invalid
private key then fails visibly. Repository workflow permissions remain read-only.

The App token matters: PRs opened with the default `GITHUB_TOKEN` do not normally
trigger PR CI. Once configured, verify an App-created release PR actually gets
both fork checks before declaring release automation live. See the
[release-please action documentation](https://github.com/googleapis/release-please-action#github-credentials).

The mirror uses a separate sync App, following Paseo's pattern. Install it on
**iExalt/gitoxide** with Contents and Workflows read/write permissions; upstream
commits can change workflow files, which the default `GITHUB_TOKEN` cannot update.
Set variable `GITOXIDE_SYNC_APP_CLIENT_ID` and secret
`GITOXIDE_SYNC_APP_PRIVATE_KEY`. Its token is scoped to this repository and those
permissions. Without the client ID, sync is skipped; an invalid key fails visibly.
The mirror necessarily contains upstream workflows as data; keep their push
filters under review during updates. Scheduled workflows run from default `dev`.
Do not reuse another repository's App credentials without explicitly configuring
and authorizing this installation.

`rebase-dev.yml` pushes with the same sync App, which the `dev` ruleset may bypass. It also needs
secret `GITOXIDE_REBASE_SSH_SIGNING_KEY`, an ed25519 private key whose public key is pinned in
`.github/scripts/rebase-dev.mjs` and registered as a signing key on the owner's GitHub account,
and variables `GITOXIDE_REBASE_SIGNING_NAME`, `GITOXIDE_REBASE_SIGNING_EMAIL` and
`GITOXIDE_REBASE_KEEP_COMMITTER_EMAILS` (comma-separated). Every email in them must be a verified
email of that account, or GitHub won't mark the replayed commits verified. The
`rebase-resolutions` branch must exist, holding at least `schema.json`.

### Consuming from acyclic

Pin a fork release tag such as `gitoxide-fork-v0.1.0`, or an immutable commit revision. Tags keep
pointing at their release commit after `dev` is rebased:

```toml
[dependencies]
gix = { git = "https://github.com/iExalt/gitoxide", tag = "gitoxide-fork-v0.1.0" }
```

Keep the features required by acyclic; this example does not select its backend
features. Cargo resolves the workspace's path dependencies from the same Git
checkout, all at the fork version. If another dependency pulls `gix`
or plumbing crates from crates.io, root-level patches can make it share fork types only
where its version requirement matches the fork version, which upstream's numbering
(`gix 0.90`, for example) does not:

```toml
[patch.crates-io]
gix = { git = "https://github.com/iExalt/gitoxide", tag = "gitoxide-fork-v0.1.0" }
gix-hash = { git = "https://github.com/iExalt/gitoxide", tag = "gitoxide-fork-v0.1.0" }
```

`gix-hash` is illustrative, not a complete patch list. Inspect `cargo tree -d` and
`cargo metadata` in the consumer, patch every registry crate that must share fork
types, and check that the fork version satisfies dependent semver requirements.
Use one revision consistently, update and commit the consumer lockfile, and run
its tests. A patch cannot satisfy an incompatible version requirement; adapt that
dependency deliberately. No acyclic dependency changes are part of fork setup.

### Compression backend

`gix` compresses and decompresses objects with [`sirocco`](https://docs.rs/sirocco), through its `zlib-sirocco` feature, which is
one of the default features. A consumer that sets `default-features = false` gets `zlib-rs` unless it adds `zlib-sirocco` to the
features it lists. Both are pure Rust, and `gix-zlib` builds for `wasm32-unknown-unknown` with either.

Measured with `gix-zlib-bench` (one thread, level 6, best of three, SHA-1 fixtures) and `repack_deltas` on the synthetic delta fixtures
of `gix/tests/fixtures/make_delta_repo.sh`, on the shared build host at a load of about 3, one run each:

| | `zlib-rs` | `sirocco` |
| --- | --- | --- |
| compressing the 240 objects of `heavy` (28.5 MB of large text files) | 0.161 s, 9.23% of the input | 0.103 s, 8.91% |
| compressing the 816 objects of `ordinary` (3.3 MB, mostly small) | 25.4 ms, 14.33% | 17.7 ms, 14.44% |
| decompressing `heavy` | 10.9 ms | 8.8 ms |
| decompressing `ordinary` | 2.72 ms | 1.87 ms |
| pack of `heavy` (SHA-1) repacked with `Repository::repack()`, against Git 2.56 `repack -a -d -f --window=10 --depth=50` | +2.9% | −0.2% |
| pack of `ordinary` (SHA-1) the same way | +0.9% | +0.3% |

The size depends on the data and the level, not simply on the size of the objects: at level 6 `sirocco` compresses `heavy` to 3.4% less
and `ordinary` to 0.8% more. At level 1, which is not the default, it compresses both to less (9.40% against 13.46% of the input for `heavy`,
15.00% against 18.92% for `ordinary`), but `heavy` took 0.035 s against 0.032 s, so it was 10% slower there, and `ordinary` 6% faster.
Decompression was 1.2 to 1.5 times as fast across both levels and fixtures.

## Setup checklist and evidence

- [x] Inspect reference forks and preserve the separate push spike.
- [x] Reuse the existing fork and add the upstream remote.
- [x] Replace inherited workflows and add pinned mise tasks.
- [x] Configure independent, PR-only fork versioning.
- [x] Preview version 0.1.0, tag `gitoxide-fork-v0.1.0`, and the three fork-only release files.
- [x] Install/configure the release App and verify its release PR receives CI (PR #2, run 41529329986).
- [ ] Install/configure the sync App and verify the scheduled mirror workflow.
- [ ] Configure the rebase signing key and variables, push `rebase-resolutions`, and run `rebase-dev.yml`.

Initial pre-change QA planner baseline: 39 tests passed in 1.79 seconds, with a
15.13-second initial QA build. After the toolchain fix, the focused planner run
took 6.58 seconds overall (39 tests, 1.88 seconds in the test binary). Workflow
lint took 0.77 seconds before adding the two sync tests, which took 0.11 seconds.
Both local gix MSRV configurations passed in 95.18 seconds including build-lock
waiting (individual builds: 62 seconds and 30.54 seconds). The archived-fixture
native suite passed 5,296 tests in 287.46 seconds, with eight expected skips.

These are initial observations on macOS arm64, not budgets or comparable warm
benchmark samples. Tool downloads, initial compilation, fixture generation, and
concurrent machine load affect them. No prior equivalent full-fork baseline was
available and no check was removed to meet a time target. The 120-minute CI
timeout is a runaway guard, not a performance budget. Review growth against the
same task, tool versions, platform, and cache state.

The `network` test target of `gix` (35 tests, `curl` and `reqwest` backends, both hashes, real `sshd` where installed) ran in 9.1 s,
and 7.1 s with `reqwest` and `rustls` only, on the shared build host at a load of 4 to 5, one run each. It generates its TLS
certificates with `rcgen` and serves them with `rustls`, both unix-only development dependencies that use `ring`, so they add
no C code to the build. The longest tests wait for a stall to be given up on, about three seconds each, and run in parallel.

Full QA, final runtime receipts, review, and the infrastructure merge are tracked
in [fork PR #1](https://github.com/iExalt/gitoxide/pull/1). At that time, the checks were
`Fork QA` and `Fork MSRV`; `Fork CI required` has since replaced them as the merge gate. At setup, the repository had no branch
protection or rulesets; no required human approval was bypassed. The App checklist
above remains separate from passing CI: installation-token workflow execution
cannot be verified until the repository's App variables and secrets are set.
