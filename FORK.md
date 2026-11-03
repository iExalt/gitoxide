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
Validate the rebased tip on a candidate branch with a deep Fork CI run, push a backup ref, and
update `dev` with `--force-with-lease`. Rebasing replaces published history, so announce it to
anyone with branches from `dev`.

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
| `mise run check:msrv` | Install Rust 1.88.0 through mise and build both declared gix MSRV configurations |
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
Rust 1.99.0 is the development and API-check toolchain. The gix MSRV remains 1.88,
and the root CLI's declared MSRV remains 1.91. Explicit task toolchains override
the inherited mise toolchain. Do not update the machine-wide `stable` alias to run QA.

Fork CI runs on Ubuntu 24.04 for every PR and push to `dev`, in one of two modes chosen
by `.github/scripts/qa-mode.cjs`:

- **Routine** (ordinary PRs): the planner's fast selection, meaning the packages a PR changes
  and every package that depends on them, with all of their declared feature and hash variants.
  It targets five minutes with warm caches. Global inputs such as `Cargo.toml` select the whole
  workspace.
- **Deep** (release PRs, pushes to `dev` and manual runs): the exhaustive `check:thorough` set.

Routine runs leave these to deep runs: the archived-fixture test pass, which detects stale
committed archives; variants, journeys and API checks of packages a PR doesn't affect; and the
exhaustive-only feature contracts. A failing deep run on a release PR blocks the release.

`mise run ci:plan` gives the selection to the QA planner. The planner turns each job group into a
lane and splits the groups named in `mise.toml`'s `ci_shards` into several lanes, never more than
the group has independent cases; `ci_combine` runs small groups as one lane. Each lane runs `mise run ci:job` with its own Rust cache, which
only `dev` pushes save. The plan job fetches two commits: a PR's merge ref has the base tip as
its first parent, fast selection compares against it, and `--require-base` fails the plan
instead of silently testing everything. The `Fork MSRV`
job runs in both modes. `Fork CI required` passes only when the planner, every lane and MSRV
succeeded; a skipped or cancelled job fails it. It is the only check the `dev` ruleset ("dev requires Fork CI") requires; the ruleset has no bypass actors.
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
through `just audit`; there is no scheduled advisory job. Tool pins mostly match
upstream QA metadata; cargo-deny uses 0.20.2 because the declared 0.20.0 release
was unavailable from its GitHub release endpoint.

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
`last-release-sha` to its rewritten copy. Conventional commit
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

### Consuming from acyclic

Use an immutable commit revision until an approved fork tag exists:

```toml
[dependencies]
gix = { git = "https://github.com/iExalt/gitoxide", rev = "<full-commit-sha>" }
```

Keep the features required by acyclic; this example does not select its backend
features. Cargo resolves the workspace's path dependencies from the same Git
checkout, all at the fork version. If another dependency pulls `gix`
or plumbing crates from crates.io, root-level patches can make it share fork types only
where its version requirement matches the fork version, which upstream's numbering
(`gix 0.90`, for example) does not:

```toml
[patch.crates-io]
gix = { git = "https://github.com/iExalt/gitoxide", rev = "<same-full-commit-sha>" }
gix-hash = { git = "https://github.com/iExalt/gitoxide", rev = "<same-full-commit-sha>" }
```

`gix-hash` is illustrative, not a complete patch list. Inspect `cargo tree -d` and
`cargo metadata` in the consumer, patch every registry crate that must share fork
types, and check that the fork version satisfies dependent semver requirements.
Use one revision consistently, update and commit the consumer lockfile, and run
its tests. A patch cannot satisfy an incompatible version requirement; adapt that
dependency deliberately. No acyclic dependency changes are part of fork setup.

## Setup checklist and evidence

- [x] Inspect reference forks and preserve the separate push spike.
- [x] Reuse the existing fork and add the upstream remote.
- [x] Replace inherited workflows and add pinned mise tasks.
- [x] Configure independent, PR-only fork versioning.
- [x] Preview version 0.1.0, tag `gitoxide-fork-v0.1.0`, and the three fork-only release files.
- [x] Install/configure the release App and verify its release PR receives CI (PR #2, run 41529329986).
- [ ] Install/configure the sync App and verify the scheduled mirror workflow.

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

Full QA, final runtime receipts, review, and the infrastructure merge are tracked
in [fork PR #1](https://github.com/iExalt/gitoxide/pull/1). At that time, the checks were
`Fork QA` and `Fork MSRV`; `Fork CI required` has since replaced them as the merge gate. At setup, the repository had no branch
protection or rulesets; no required human approval was bypassed. The App checklist
above remains separate from passing CI: installation-token workflow execution
cannot be verified until the repository's App variables and secrets are set.
