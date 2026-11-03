# Fork status

Last updated: 2026-11-03

This file records where the fork's iteration-speed and release work stands, backed by evidence.
[FORK.md](FORK.md) explains how to develop and release the fork; this file tracks the work that
changes it.

## Current snapshot

| Field | Status |
| --- | --- |
| Baseline | `f37e5297f` on `dev` |
| Current phase | 1: iteration speed. A, B and C are done; D is next |
| Overall state | Fork CI runs the full QA set in parallel lanes: 8m56s with cold caches, against 40m53s as one job. A warm local run takes 9–10 min on an 8-vCPU Linux host |
| Immediate focus | Routine/deep split and the required gate (D) |
| Release readiness | No fork release or tag exists. Release PR #2 predates the renumbering decision |

## Goals

| Goal | Workflow | Evidence | Exit condition |
| --- | --- | --- | --- |
| A: baseline and cheaper builds | complete | verified | Comparable baseline recorded; build cost cut without dropping checks |
| B: parallel CI lanes | complete | verified | The full task set runs in parallel lanes; deep-run wall time is measured against A |
| C: local fast loop | complete | verified; owner accepted 94–109 s for core plumbing edits | `mise run test` takes 60 s or less warm for typical edits; the worst case is reported |
| D: routine/deep split and required gate | planned | — | Two warm PR runs finish within 5 min on the critical path; a deep run passes; a `dev` ruleset requires the gate |
| E: renumber to 0.1.0 in lockstep | planned | — | Every crate shares one release-please-managed version |
| F: tagged releases | planned | — | Merging a release PR tags `gitoxide-fork-vX` and creates its GitHub release |

Decisions from the owner (2026-11-02, plan approval):
- PR gate: "Routine/deep split".
- Versions: "Lockstep, all crates".
- Ruleset: "Yes, require the gate".

## Baseline

Host class: shared Linux x86_64 host with 4 cores (8 threads) and 31 GB of RAM. Each run's log
header records Rust 1.99.0 through mise, `GIX_TEST_IGNORE_ARCHIVES=1`, git 2.56.0 and umask 022. Its
command is `mise exec -- bash etc/scripts/ci-check-local.sh --thorough --stage
self-test,policy,housekeeping,check,tests,doctests,docs,api,journey`, which is the
`check:thorough` stage set without workflow lint.

| Run | Wall | CPU user + sys | Result |
| --- | --- | --- | --- |
| CI run 41528920875 (`dev`, 4 vCPU) | 40m53s | not recorded | pass |
| Host, empty `target/` (environment not yet matched; not a valid cold baseline) | 1694 s | 4588 + 1376 s | 3 tasks failed (see below) |
| Host, warm, fixtures regenerated | 603 s | 576 + 620 s | pass |
| Host, warm | 530 s | 549 + 589 s | pass |

Warm task durations (seconds):

| Task | Seconds |
| --- | --- |
| lint | 18 |
| feature-builds | 14 |
| feature-contracts | 33 |
| native-tests (two fixture passes) | 111 |
| isolated-tests | 9 |
| feature-tests | 187 |
| doctests | 22 |
| journey | 108 |
| api | 26 |

On CI, every feature-build case recompiles its workspace crates (15–38 s each). `Swatinem/rust-cache`
leaves workspace members out of the cache, and fresh checkout mtimes would make them stale anyway.
Compilation, not test execution, dominates CI time.

## Accomplishments

### A: cheaper debug builds

Evidence state: verified. Reviewed; committed as `a152ba41f`.

`[profile.dev] debug = "line-tables-only"` in `Cargo.toml`. Panic backtraces still show
`file:line`, which was checked with a temporary probe. `#[track_caller]` error locations don't
depend on debuginfo. Cold `cargo nextest list --workspace` in fresh target directories, run in
ABAB order:

| Setting | Wall | CPU user + sys | Target size |
| --- | --- | --- | --- |
| full debuginfo | 136 s, 172 s | 959 s, 971 s | 22 GB |
| line tables only | 104 s, 105 s | 757 s, 761 s | 9.7 GB |

Warm full-suite runs are unchanged within noise: 586 s and 571 s wall, 1165 and 1164 CPU-s.

### B: parallel CI lanes

Evidence state: verified. Reviewed through `4d17ad086`.

The planner splits the `host_stages` set into lanes (`mise run ci:plan`). `features` is split
six ways, `feature-contracts` has its own lane, and `Fork QA` requires every lane. Both measured
runs used cold Rust caches: the cache keys are new, and branch runs don't save caches.

| Run | Wall | Runner time | Slowest lane |
| --- | --- | --- | --- |
| Single job, run 41528920875 | 40m53s | 41 min | — |
| 15 lanes, run 41566998163 | 10m22s | 72 min | features#2of6, 563 s (it held `feature-contracts`) |
| 16 lanes, run 41568167308 | 8m56s | 75 min | features#3of6, 490 s |

In the rebalanced run, lanes took 376–490 s for features, tests, journey and contracts. Other
lanes took 99–268 s, and the plan job 29 s. In the tests lane, the build took 220 s and the two
fixture passes 117 s and 104 s, so sharding it would repeat the build.

Runner time is up 80%, because each lane installs tools and compiles separately. Public-repository
runners are free, but the account's limit of about 20 concurrent jobs is shared with other
repositories. With 18 jobs per run, this run's parallelism depends on their CI being quiet. Queue
time was 2–13 s per lane in both runs. These figures are cold. Re-measure once a `dev` push has
saved the per-lane caches.

sccache, measured on the station, saved 42% of cold-build wall time but no CPU, because test
harnesses, binaries and proc-macros can't be cached. It stays deferred unless warm lanes stay
compile-bound.

### C: local fast loop

Evidence state: verified. The 60 s target is met for upper-layer crates. For core plumbing edits the owner chose "Accept as is" (2026-11-03), keeping the 94–109 s timings recorded below.

`mise run test` runs nextest for the planner's affected packages and their dependents. It builds
exactly the exhaustive native-test package set, so it shares artifacts with `check` (shown by
counting compiled units: after a loop run, the exhaustive build with QA's environment compiled
nothing). The old fast path, `--stage default-tests`, took 133–150 s for a `gix-url` edit
because each selection changed feature unification.

Measured with one appended comment per run, warm, at load 0.6–9 on the shared host (the
load rose during the set, so later rows are slower than an idle host):

| Edited | Wall | CPU | Tests run |
| --- | --- | --- | --- |
| `gix-url` (leaf) | 48 s, 48 s | 226 s | 2,042 in 14 s |
| `gix` (top) | 47 s warm (68 s after a wider rebuild) | 203 s | 1,624 in 13 s |
| `gix-ref` (core plumbing) | 94 s, 104 s | 585–631 s | 5,326 in 30–36 s |
| `gix-hash` (worst case) | 107 s | 654 s | 5,326 in 35 s |
| `Cargo.toml` (global input) | 109 s | 676 s | 5,333 in 37 s |

The planner and launcher add about 0.3 s. Core plumbing crates such as `gix-ref`, `gix-hash` and
`gix-object` reach nearly every test, and `Cargo.toml` builds them with `opt-level = 3`. An edit
there therefore relinks about 60 test binaries and runs the whole suite. That's the reason for the
~100 s cases; trimming them would mean changing upstream's test-speed profile choices.

QA commands run with `CI=true`. Plain `cargo test` or `cargo nextest` without it builds a separate
set of artifacts, so switching between the two rebuilds changed crates once more. A bisection on
the host showed `CI` is the only variable among QA's settings that has this effect.

## Deferred findings

- The `gix-pack` multi-index checksums and the `gix free pack receive` journey depend on how git's
  zlib compresses (Nix git fails them). The `gix-tix` undo/redo tests assert mode 0644 and fail
  under umask 0002. These are test-isolation defects, not regressions.
- `gix-command` `spawn::with_shell::command_in_path_with_args` failed once out of six runs under load
  (`ls -l` in the test snapshot exited non-zero). The cause is unconfirmed; a race with a concurrent tempdir deletion is suspected.
- The exhaustive archived-fixture pass rewrites tracked `generated-archives/*.tar` files in the
  working tree. Never commit those rewritten archives.
