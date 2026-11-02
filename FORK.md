# Developing the gitoxide fork

This fork supports acyclic's Git storage and synchronization backend. It lets us
develop in-process push without depending on upstream review capacity. Fork setup
does not implement push. The existing investigation and prototype remain separate
in the sibling `gitoxide-upstream` checkout.

## Branches and upstream updates

`origin` is <https://github.com/iExalt/gitoxide>; `upstream` is
<https://github.com/GitoxideLabs/gitoxide>. `dev` is the fork's default and integration branch.
Open development PRs against `iExalt/gitoxide:dev`, and squash-merge after checks
and review. Preserve upstream history, copyright notices, and licenses.

Acyclic uses `main`; this fork follows Paseo's `dev` and `upstream/main` convention.
The latter is an upstream mirror, not a development target. The original `main`
branch remains at the fork's starting revision for reference; it is not maintained.
For an update, fetch origin, create an update branch from fork `dev`, merge the
chosen `origin/upstream/main` revision, resolve conflicts, run thorough QA, and
review a PR into fork `dev`. Preserve the merge ancestry for
upstream updates (use a merge commit rather than squashing that PR). Do not rebase
published fork history. There are no automated merges or rebases into `dev`.

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
| `mise run check` | Affected-package QA, including workflow lint; global inputs select the workspace |
| `mise run check:thorough` | Exhaustive host checks, feature tests, docs, API tests and CLI journeys |
| `mise run check:msrv` | Install Rust 1.88.0 through mise and build both declared gix MSRV configurations |
| `mise run build` | Default CLI build, without publishing |
| `mise run lint:workflows` | Actionlint and embedded-shell ShellCheck |
| `mise run release:preview` | Read-only release-please PR preview with `GH_TOKEN` |

The tasks delegate to the existing `justfile` and QA planner. Feature selection
comes from Cargo QA metadata: small, lean, lean-async, max, max-pure and declared
crate-specific variants are distinct checks; `--all-features` is not a substitute.
Rust 1.99.0 is the development and API-check toolchain. The gix MSRV remains 1.88,
and the root CLI's declared MSRV remains 1.91. Explicit task toolchains override
the inherited mise toolchain. Do not update the machine-wide `stable` alias to run QA.

The fork CI runs exhaustive host QA and MSRV builds on Ubuntu 24.04 for every PR
and push to `dev`. Check names are `Fork QA` and `Fork MSRV`. Release PRs use the
same trigger. Workflow actions are pinned to commit IDs; checkout credentials are
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

Release-please uses its `simple` strategy with `fork-version.txt` and
`.release-please-manifest.json`. The initial unreleased version is `0.0.0`;
the first feature release is `0.1.0`. Tags use `gitoxide-fork-v0.1.0`, independently
of every Rust crate's version. Generated notes go to `CHANGELOG.fork.md`.
Upstream `CHANGELOG.md` files and all Cargo versions remain untouched. There are
no workspace version replacements, crates.io publication, or binary packages.

The bootstrap boundary is upstream commit
`9dd78c800d554c18fc99816bf922c93d71402881`, the fork's starting point. Earlier
upstream commits are excluded from the first fork release. Conventional commit
titles describe fork changes; breaking changes require explicit review even while
the fork version is below 1.0.

The workflow uses `skip-github-release: true`: it only maintains a release PR.
It neither tags nor creates a GitHub release, even after a release PR is merged.
Do not enable release-PR auto-merge. Before merging a release PR or creating its
tag/release, obtain explicit owner approval naming the version. After approval,
review the preview, merge the checked release PR, then separately create the exact
fork tag and release at that merge revision. No release-creation dispatch is
configured. The manifest alone does not mean a release has been published.

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
checkout, retaining their upstream versions. If another dependency pulls `gix`
or plumbing crates from crates.io, add root-level patches for the affected crates
to avoid mixing registry and Git types:

```toml
[patch.crates-io]
gix = { git = "https://github.com/iExalt/gitoxide", rev = "<same-full-commit-sha>" }
gix-hash = { git = "https://github.com/iExalt/gitoxide", rev = "<same-full-commit-sha>" }
```

`gix-hash` is illustrative, not a complete patch list. Inspect `cargo tree -d` and
`cargo metadata` in the consumer, patch every registry crate that must share fork
types, and ensure the preserved versions satisfy dependent semver requirements.
Use one revision consistently, update and commit the consumer lockfile, and run
its tests. A patch cannot satisfy an incompatible version requirement; adapt that
dependency deliberately. No acyclic dependency changes are part of fork setup.

## Setup checklist and evidence

- [x] Inspect reference forks and preserve the separate push spike.
- [x] Reuse the existing fork and add the upstream remote.
- [x] Replace inherited workflows and add pinned mise tasks.
- [x] Configure independent, PR-only fork versioning.
- [x] Preview version 0.1.0, tag `gitoxide-fork-v0.1.0`, and the three fork-only release files.
- [ ] Install/configure the release App and verify its release PR receives CI.
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
in [fork PR #1](https://github.com/iExalt/gitoxide/pull/1). Require both `Fork QA`
and `Fork MSRV` to pass before merging. At setup, the repository had no branch
protection or rulesets; no required human approval was bypassed. The App checklist
above remains separate from passing CI: installation-token workflow execution
cannot be verified until the repository's App variables and secrets are set.
