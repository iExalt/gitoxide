# Push support and Git-free consumers

Status: confirmed planning direction; implementation and experiments have not started.
Confirming this plan approves no implementation phase, release, or migration.

The goal is to let a consumer replace its production Git CLI operations with fork APIs,
including synchronization with local repositories. SSH and configured credential helpers
may remain external programs. Network peers may run Git; local client and receiving
operations must work without a Git executable. User-provided hooks and helpers retain
their own executable dependencies.

## Requirements

The agreed scope includes:

- Blocking networking, with execution off asynchronous server worker threads.
- SHA-1 and SHA-256 throughout repository, pack, reference, and protocol operations.
- Atomic multi-reference push with explicit expected-old leases, including expected absence.
- Fast-forward-only log updates and separately authorized state-history rewrites.
- Leased deletion, strict status reporting, and reconciliation of uncertain outcomes.
- SSH, HTTPS, local paths, and file URLs; local transports must not launch Git servers.
- Full receive-pack compatibility, including hooks, quarantine, and receive policies.
- Reftable support, including reference-store integration and Git interoperability.
- Git-equivalent ref-transaction durability for each supported ref backend: the fork
  must fail, persist, and recover no worse than Git does under the same failures. Stronger
  crash atomicity than Git is out of scope.
- Preservation of pre-push hooks, credential-helper behavior, SSH configuration, and
  the consumer's HTTP low-speed policy: 1,000 bytes per second for 30 seconds.
- Native maintenance before complete cutover.

Reftable is required, but its use as the default store and conversion of existing
repositories are unresolved architecture decisions. Supporting a format for reads does
not establish stronger write guarantees for that format.

## Starting evidence

The inspected fork revision is `b0de3f8303aef229861ceb99582104e4608566e3`.
The following are source observations, not new runtime verification:

| Area | Existing building blocks | Remaining contract |
| --- | --- | --- |
| Push | Push refspecs, remote configuration, transport service selection, packet framing, pack generation | Command exchange, strict results, policy planning, and a supported repository push API |
| Fetch | `gix/src/remote/connection/fetch/` | Scoped pruning of stale destination refs after successful complete discovery and transfer |
| Local transport | Existing file transport | Native listing, fetch, and receiving without `upload-pack` or `receive-pack` subprocesses |
| Reference transactions | `gix-ref/src/store/file/transaction/` | Differential proof that failure behavior matches Git's; commit code documents possible partial application after I/O failure (`commit.rs:17`), which needs comparison with Git's own files-store behavior |
| Persistence | Lock files and tempfile renames in `gix-lock` and `gix-tempfile` | No `fsync`/`sync_all` calls exist in `gix-ref`, `gix-lock`, or `gix-tempfile`; honor `core.fsync` and `core.fsyncMethod` for objects, packs, refs, and reftable stacks as Git does |
| Hooks | None; `crate-status.md` lists client-side hooks as unimplemented | Shared hook runner: `core.hooksPath` resolution, arguments, stdin, environment, and exit handling, used by pre-push and every receive hook |
| Reftable | Private generic-store scaffolding in `gix-ref/src/lib.rs` | Format, stack, store, and public API integration; `gix::RefStore` currently aliases the files store |
| Configuration | `gix/src/config/file_mut.rs`, repository config APIs | Consumer persistence and precedence proofs; snapshot mutation alone is not a physical-file update |
| Credentials | Helper protocol and configuration support | Named helpers currently dispatch through Git; direct helper execution must preserve compatible behavior |
| Maintenance | Pack/index publication and object access | Safe compaction, retention, reclamation, and concurrent-reader/writer behavior |

A previous local push prototype demonstrated a single SHA-1 ref update against real
receive-pack. Its reported checks included stale-lease refusal, server rejection, and
required atomic capability. Those checks were not rerun for this document. They do not
prove multi-ref rollback, deletion, post-advertisement races, SSH/HTTPS, SHA-256, bounded
pack generation, or independence from local Git server executables.

## Consumer operation inventory

The migration must preserve behavior rather than translate commands individually.
Relevant consumer surfaces include the store, repository discovery, readiness checks,
initialization/configuration commands, and periodic server synchronization.

| Operation family | Required behavior |
| --- | --- |
| Opening and discovery | Ignore locating and injected-config environment variables while retaining supported user configuration and authentication; resolve linked-worktree common directories correctly |
| Reference lookup and listing | Exact and peeled commit resolution, absent/corrupt distinctions, prefix enumeration, symbolic refs, and both hash formats |
| Trees and object reads | Recursive and selected-path reads; exact bytes and modes; distinguish an absent requested path from a missing object referenced by a tree |
| Object and commit writes | Preserve identities, timestamps, parents, messages, and file modes; avoid modifying the user index or checkout; retain efficient batch packing |
| Local ref transactions | Expected absence, update/delete CAS, marker updates, lock/conflict diagnostics, and Git-equivalent failure and persistence behavior |
| Configuration | Replica identity, initialization markers, remote settings, push-after-write and maintenance settings; physical persistence without damaging unrelated configuration |
| Initialization | Reject nested destinations and duplicate remote identity before mutation; preserve relative-remote resolution and idempotent setup |
| Readiness and ancestry | Default-branch selection, unique commit abbreviations, cached traversal with ancestry fallback, and distinct unavailable/error/not-merged results; support mapped reftable clones |
| Fetch | Forced explicit mappings into private tracking namespaces; delete only stale destinations covered by those mappings after a complete successful fetch |
| Push and synchronization | State leases, log fast-forwards, safe deletes, duplicate-writer detection, join markers, and tracking/prune bookkeeping only after confirmed results |
| Squash and pruning | Preserve tree/timestamp semantics, pending deletion markers, and object lifetimes during publication |
| Maintenance | Honor disabled upkeep, bound pack/loose/table growth, and reclaim eligible storage without losing reachable or in-flight data |
| Server execution | Retain write/sync serialization, read responsiveness, bounded cancellation/shutdown, and the existing blocking-task boundary |

Tests and development fixtures may use real Git as an oracle. The production audit must
also inspect indirect library dispatch, credential programs, initialization probes,
configuration discovery, and file transports; searching for direct Git commands is
insufficient.

## Architecture investigation comes first

Propose a bounded initial assessment, with an explicit stop and report at its end. It was
first sized as 3–5 engineer-days; re-express that allowance in agent effort before starting. This is not a promise to complete prototypes or settle the full
implementation estimate within that allowance. Read-only investigation is distinct
from authorization for builds, mutating probes, or disruptive fault-injection experiments.
Client protocol and pack work does not wait for this assessment; see
[Work sequence and acceptance](#work-sequence-and-acceptance).

The assessment produces:

1. A pinned Git compatibility target and an exhaustive receiving behavior matrix.
2. Candidate reference-storage and coordination designs, with public API implications.
3. Git's failure, persistence, reader-snapshot, and external-writer behavior per ref backend,
   stated precisely enough to test the fork against it.
4. Repository-default and conversion options, including worktrees and concurrency.
5. Discriminating probe proposals with setup, build, execution, and analysis costs.
6. A revised implementation estimate in agent effort, with reftable and full receiver parity
   priced separately so either can be reconsidered on cost; or the specific remaining
   uncertainty and the smallest next investigation needed to resolve it.

Both backends follow Git's own coordination: files-store lock files and the
[reftable stack publication protocol](https://git-scm.com/docs/reftable), so fork and Git
writers interoperate. Git documents partial concurrent observation for files-store
transactions; see [update-ref transaction behavior](https://git-scm.com/docs/git-update-ref).
The fork matches that behavior rather than improving on it.

### Git-equivalent durability

Test each of these differentially against Git under the same injected failures, with the
Git version, platform, filesystem, and `core.fsync`/`core.fsyncMethod` configuration pinned
for both sides:

- Lease, lock-conflict, and validation failures reject the whole transaction before any ref changes.
- I/O failure during commit leaves the same classes of partial state Git would, and the
  fork reports the outcome as uncertain rather than rolled back.
- Objects and refs are flushed to disk as the pinned `core.fsync` configuration requires,
  and no earlier or later. Git allows hardening to be disabled, and its defaults can lose
  recent objects on power loss; the fork matches the configured behavior and promises no
  more.
- Reader snapshots and mixed Git/fork writers observe what they would with Git alone.

Process termination and power loss are separate failure models. Process-kill tests prove
only the first; power-loss claims need flush-ordering evidence for the pinned filesystem,
or are stated as unproven.

External hook effects, worktree changes, and cross-repository tracking updates are separate
from atomic ref publication. Post-commit hook failure must not be reported as a rolled-back
commit. No existing repository is converted without a selected and authorized migration policy.

### Receiving compatibility

The receiving matrix must cover effective configuration and includes, hooksPath,
connectivity/fsck, receive policies, status/capability negotiation, reflogs, and applicable
push-option and certificate behavior. Include pre-receive, update, proc-receive,
reference-transaction, post-receive/post-update, and push-to-checkout hooks; non-bare
repositories and updateInstead require explicit treatment.

Prove hook arguments, input, environment, working directory, ordering, veto versus
notification results, ref redirection, quarantine visibility, hook-created objects,
promotion, cancellation, and cleanup. A hook-free bare-repository subset does not satisfy
the chosen endpoint. Client features that are deferred must not silently remove required
receiver behavior. [Git's receive-pack documentation](https://git-scm.com/docs/git-receive-pack)
describes quarantine and hook behavior.

## Work sequence and acceptance

Client protocol and bounded pack work starts alongside the architecture assessment,
because it does not depend on the ref backend. Storage, receiver, and local-backend
implementation waits for acceptance of the assessment and revised scope. These are
provisional boundaries, to be refined from that evidence.

| Work | Dependencies | Completion evidence |
| --- | --- | --- |
| Architecture and initial dependency costs | None for read-only investigation; explicit authority for experiments | Compatibility matrix, candidate design tradeoffs, precise guarantees, migration choices, evidence gaps, and revised estimate |
| Native reftable store | Architecture decision and initial cost baseline | Git reads fork-written stacks and the fork reads Git-written stacks; both hashes, direct/symbolic refs, tombstones, reflogs, worktrees, snapshots, locks, reload, corruption handling, and compaction |
| Persistence and Git-equivalent durability | Architecture decision; reftable integration for its stacks | `core.fsync`/`core.fsyncMethod` support; differential fault-injection tests against Git for rejection, partial commit, object-before-ref ordering, and mixed writers |
| Hook runner | None | `core.hooksPath` resolution, argument/stdin/environment/working-directory parity with Git, veto versus notification exit handling, and missing or non-executable hooks; used by pre-push, local ref transactions, and the receiver |
| Local consumer backend | Accepted storage APIs, durability tests, and hook runner | Differential discovery/config/init/readiness/object/ref tests; exact bytes and errors; `reference-transaction` hook prepared/committed/aborted states and prepare-phase veto, matching `git update-ref`; no index/checkout/global-environment damage |
| Client protocol and bounded packs | None; runs in parallel with the assessment; protocol before wire proof | Strict command/status parsing and real receive-pack acceptance; complete object closure, remote-have exclusions, bounded streaming, and both-hash fsck |
| Repository push and fetch | Storage, protocol, and pack APIs | Multi-ref create/update/delete, expected absence, pre/post-advertisement lease races, log FF/state rewrite, server refusal, scoped fetch pruning, and ambiguous-result reconciliation |
| SSH/HTTPS/auth/client hooks | Repository network APIs and hook runner | Synthetic authenticated peers, direct helper get/store/erase and chaining/reset, SSH configuration, pre-push refusal, low-speed limits, cancellation, and no hidden local Git dispatch |
| Full native local receiver | Storage, packs, repository planning, and hook runner | Complete pinned receiver matrix; native local listing/fetch/push, both hashes, ref formats under selected write policy, quarantine, policies, hooks, worktrees, and crash behavior |
| Native maintenance | Local/store/pack integration; receiver for combined closure | Safe object and ref-stack compaction/retirement; grace/keep/reflog/quarantine/in-flight roots; concurrent access and interrupted reclamation; disabled-upkeep behavior |
| Cutover and release consumption | All preceding implementation and proofs | Complete no-Git CLI/server journeys, actual selected-feature cost measurements, platform/MSRV/QA checks, and consistent approved fork-tag consumption |

Protocol plumbing should accept validated old/new/ref commands and a pack producer.
Repository policy owns caller leases, ancestry, remote configuration, object selection,
and bookkeeping. A fresh advertisement must never replace a stale caller lease.
Uncertain remote results require reconciliation before retry or marker removal.

Generic protocol, storage, transport, and maintenance changes should be shaped for
possible upstream contribution. Consumer policy and wiring remain consumer-specific.
Upstream posting or acceptance is not a delivery dependency or currently authorized.

### Suggested execution phases

1. In parallel: architecture assessment with the initial dependency baseline, and client
   protocol with bounded pack generation.
2. Reftable, persistence, and Git-equivalent durability; the hook runner can proceed alongside.
3. Local consumer backend.
4. Network sync, authentication, and client hooks.
5. Full local receiving and native maintenance; split further if the assessment warrants it.
6. Final cutover, packaging, and complete verification.

Client protocol work is independent of storage, but network integration needs both. Local receiving joins the
storage, local-backend, and network work. Do not run concurrent mutations in a shared
checkout. Each implementation phase needs a concrete approved scope; confirmation of
this document does not approve those phases.

## Cost and verification gates

The earlier **8–15 engineer-week** estimate covered a smaller, unchosen scope. It is
superseded. The historical **3–6 week** push estimate concerns blocking client push,
not full receiving or reftable. Both are human-effort figures. There is no accepted
expanded total yet; do not sum old component estimates into a new commitment. Express
the revised estimate in agent effort.

Establish a stable consumer baseline early. Its warm routine check must stay under
60 seconds; a previously reported approximately 45-second result is historical and
has not been refreshed here. Measure comparable machine/load, cache, toolchain,
environment, and concurrency conditions, including representative edited-core runs,
cold builds, dependency duplication, release size, and CLI latency.

The early baseline **does not close the final cost gate**. Re-measure with the actual
receiver, storage, reftable, and HTTPS features before cutover. Final validation closes
that gate; it is not a prerequisite for starting the work that creates those features.
Apply this consumer budget separately from the fork's QA measurements in
[FORK.md](../FORK.md).

Use inexpensive unit tests for codecs, planning, and classification; public integration
tests for API contracts; and a small real-peer journey matrix for transport behavior.
Use isolated gix-testtools fixtures, read-only fixtures for reads, and disposable writable
fixtures for mutations. Cover distinct invariants without multiplying whole expensive
matrices across every transport/hash combination.

Run the fork's existing affected checks for coherent changes and its thorough feature,
test, documentation, API, journey, and relevant MSRV checks for final integration.
Preserve required verification rather than weakening checks to fit a budget. Pin
consumer dependencies consistently to an approved immutable fork tag and verify the
lockfile does not duplicate registry and fork plumbing unexpectedly.

Record actual evidence and limitations for every gate. Reopen architecture/scope when
durability, interoperability, migration, full receiver parity, safe reclamation, or
dependency costs contradict the selected route. Never silently substitute files-only
support, hook bypass, disabled maintenance, or weaker-than-Git durability to declare success.

## Next action

Prepare the bounded architecture assessment and its concrete probe proposals. Storage
selection, default format, conversion policy, and the expanded implementation estimate
remain decisions after that evidence. Client protocol and bounded pack work can start now,
within a separately approved scope. No source implementation,
benchmark, fault experiment, conversion, or runtime acceptance is claimed by this plan.

## Execution threads

An overseer session runs these threads as separate worker sessions, each using
`keep-me-in-the-loop`. Builds and tests run on a shared Linux host, never on the owner's
laptop. Each worker uses `cargo -j4`, and only one thorough check runs at a time. The
`mise run test` budget is 120 seconds against a warm baseline of 94–109 seconds for core
plumbing edits. `mise run check`, the affected-package gate before a PR, has no recorded
baseline yet; workers report its runtime and selection. The
thorough-check budget is 12 minutes against a warm baseline of 530 seconds, remeasured as 615 seconds
on 2026-11-04 after this work added about 450 native tests (`FORK_STATUS.md`, "Baseline"); a run
after a change that reaches `gix-ref` rebuilds most of the workspace and takes about 1,370 seconds,
which is not comparable with either figure. No spending beyond
existing subscriptions is approved.

| Thread | Scope | Depends on | Human | State |
| --- | --- | --- | --- | --- |
| 1. Architecture assessment | Assessment deliverables 1–6 above. Read-only: no builds, mutating probes or fault injection. One docs PR, ending with an explicit stop and report | None | The owner accepts the report and decides storage, default format, conversion policy, revised scope and experiment authority | done: [assessment](push-architecture-assessment.md) delivered; owner decisions recorded below |
| 2. Client protocol and bounded packs | Plumbing that takes validated old/new/ref commands and a pack producer; strict command/status parsing; real `receive-pack` acceptance with Git as test oracle; complete object closure, remote-have exclusions, bounded streaming, both-hash fsck. Excludes the repository push API and lease policy | None | None known | done |
| 3. Hook runner | `core.hooksPath` resolution; argument, stdin, environment and working-directory parity with Git; veto versus notification exit handling; missing and non-executable hooks; Git 2.56 config-defined hooks, parallel jobs and per-hook stdin producers | None | None known | done |
| 4. Ref-store abstraction and interim reftable refusal | Probe P12 first; refuse every reference operation, reads and writes, on a repository whose `extensions.refStorage` is not `files`, with an `Unsupported` error, while opening and object access keep working; `gix init` follows Git's ref-format precedence and refuses a requested reftable; then the staged ref-store abstraction (assessment 5.1, additive handle first) and its call-site migration; probe P11 | None | None known | done |
| 5. Durability plumbing and differential fault harness | Probes P1, P3, P5 and P7; `core.fsync` and `core.fsyncMethod` for objects, packs and refs, including the barrier flush; the files-store gaps in assessment 4.3; an uncertain-outcome class; a transaction observer; P1, P3 and P5 turned into differential tests against Git | None; rebases on thread 4 where both touch `gix-ref` | None known | done |
| 6. Reftable codecs and stack layer | Probes P6 and P4; adopt the upstream reftable codecs after a parity review, both hashes and version 2; a new stack layer following Git's lock and publication protocol, with reload, segment compaction, cleanup and flush; interoperability tests where Git reads fork-written tables and stacks and the fork reads Git's. Integration as a ref backend waits for thread 4 | None for the standalone crate | None known | done: standalone `gix-reftable` crate, estimated at 4–5.5 sessions and run in that range; see the revision log |
| 7. Reftable ref backend | A feature-gated `gix_ref::store::reftable` adapter over `gix-reftable` behind the neutral `gix_ref::Store`: reads, iteration, symrefs, peeled values, transactions with reflogs, the `HEAD` reflog and tombstones, reflog reading and expiry, worktree stacks; opening selects it for `extensions.refStorage=reftable` when the feature is built, and the interim refusal stays otherwise; `init` and clone create reftable only on an explicit or configured request, with `files` the default; outcome classes and the transaction observer through the neutral wrapper; a differential matrix against Git (selected `t0610`, `t0613`, `t0614` cases, mixed writers, both hashes). Excludes migration (decision 3) and the `reference-transaction` hook | Threads 4 and 6; rebases on thread 5 where both touch `gix-ref` | None known | done: the HEAD-log swap for thread 8's shared functions is the one open follow-up, see the revision log |
| 8. Local consumer backend | The plan's consumer operation inventory for local work, against Git 2.56 as the oracle: opening and discovery that ignores locating and injected-config environment variables, reference lookup and listing, tree and object reads, object and commit writes without touching the index or checkout, local ref transactions, configuration persistence, initialization, readiness and ancestry, for both hashes and both ref formats; the `reference-transaction` hook with prepared, committed and aborted states and a prepare-phase veto through the hook runner and the transaction observer, matching `git update-ref`; the files store's reflog divergences from Git that thread 7 records. Starts with an audit that replaces the placeholder estimate | Threads 3, 4 and 5; the hook part waits for thread 5's observer; rebases on thread 7 where both touch `gix-ref` | None known | done |
| 9. Repository push and fetch | Repository-level push on top of `gix_protocol::push` and the bounded pack writer: multi-ref create, update and delete, expected absence, caller leases checked before and after the advertisement (a fresh advertisement never replaces a stale caller lease), log fast-forward and state-rewrite policy, server refusal, the typed shallow-source refusal thread 2 left open, and reconciliation of ambiguous results before retry; fetch with forced explicit mappings into private tracking namespaces, pruning only stale destinations those mappings cover after a complete successful fetch. Differential tests against Git 2.56 over local transports, both hashes. Starts with an audit that replaces the placeholder estimate. Excludes SSH, HTTPS, authentication and client hooks (a later thread) | Threads 2, 4 and 5; rebases on threads 7 and 8 where they touch `gix-ref` or `gix` | None known | done |
| 10. SSH, HTTPS, authentication and client hooks | Network transports for thread 9's push and fetch: SSH with SSH configuration, HTTPS with a transport backend chosen from probe P11's cost data, authentication through direct credential helpers (get, store, erase, chaining and reset) without hidden local Git dispatch, the client `pre-push` hook through thread 9's seam and the hook runner, low-speed limits and cancellation. Synthetic authenticated peers as test oracles, both hashes. Starts with an audit that replaces the placeholder estimate | Threads 3 and 9 | The owner chooses the HTTPS backend if the audit finds the choice has lasting dependency cost | done |
| 11. Full native local receiver | The assessment's section 9.4 rows: the server-side protocol (advertisement, commands, certificates, options, report v1 and v2, sideband, keepalive); object ingestion (quarantine, unpack or keep, thin-pack fixing, migration with thread 5's barrier flush compared against its `receive-pack` flush golden, strict fsck with Git's message ids and skip list, size limits); policy and ref planning (deny rules, hidden refs, namespaces, aliased updates, atomic and batched execution with Git's rejection semantics, shallow updates, certificates through an external signer, reflogs); hooks (`pre-receive`, `update`, `post-receive`, `post-update`, `proc-receive`, `reference-transaction`); `updateInstead` and `push-to-checkout` without Git subprocesses; the native local transport for listing, fetch and receive in process, replacing the file transport's `git-*` spawning; and a receiver differential matrix against Git 2.56, both hashes and both ref formats. Runs in several phases | Threads 2, 3, 5, 7 and 9; the `reference-transaction` hook comes from thread 8 | None known | ready |
| 12. Native maintenance | Git-free maintenance for the consumer's "Maintenance" inventory row: honour disabled upkeep (`maintenance.auto`, `gc.auto` and their detach keys); bound pack, loose-object and reftable growth; reclaim eligible storage without losing reachable or in-flight data. Covers repacking and pack retirement, loose-object pruning with grace periods, `.keep` and cruft packs, reflog expiry and reflog roots, reftable stack compaction through `gix-reftable`, `packed-refs` packing, concurrent readers and writers, interrupted reclamation, and the `receive.autogc` seam thread 11 leaves. Quarantine and in-flight roots are closed with thread 11. Differential tests against Git 2.56's `gc`, `repack`, `prune` and `maintenance run`, both hashes and both ref formats. Starts with an audit that replaces the placeholder estimate | Threads 5, 6, 7 and 8; the combined closure with thread 11 | None known | done: see the revision log |
| 13. Pack-size follow-ups | Close the pack-size gap that thread 12 measured against `git repack -a -d -f --window=10 --depth=50`: make the delta encoder of `gix-pack` make the choices of Git's `diff-delta.c` (same-base uncompressed deltas within 0.5% of Git's, both hashes, encoder runtime within 10%), and document and measure `zlib-sirocco` for the consumer (pack size, compression and decompression time, build time, tests on both hashes, the default-feature `check:msrv` gate), with an assessment of making it the default once the workspace MSRV allows. Tells thread 12 what changes in `gix-pack`'s delta code | Thread 12 (its `gix-pack` delta encoder and repack integration) | None known; the owner decides whether `zlib-sirocco` becomes a default | ready |

Later threads (reftable ref backend, local consumer backend, repository push and fetch,
network, receiver, maintenance, cutover) follow the assessment's section 9 rows and
sequencing; the overseer proposes each as its dependencies finish.

### Revision log

- 2026-11-04: the owner agreed to run threads 1–3 in parallel ("Add the hook runner too")
  and granted the overseer high autonomy for this run ("Grant for this session").
  Releases and tags still need the owner's approval naming them.
- 2026-11-04: thread 1 delivered the [architecture assessment](push-architecture-assessment.md)
  and stopped for the owner's decisions listed in its section 10. It ran in about 17 minutes
  of wall-clock by the sandbox clock against a 5–8 hour allowance, and found one defect
  independent of this plan (the fork opens reftable repositories with the files store).
- 2026-11-04: thread 3 (hook runner) is done as `gix-hook` plus `gix::Repository::hook()`. The overseer added Git 2.56
  config-defined hooks, jobs and stdin producers to its scope. Two deliberate differences to Git are pinned by tests:
  `GIT_DIR` is an absolute path where Git passes `.`, and no exec-path is prepended to `PATH`.
  Callers still own `GIT_PREFIX`, `GIT_INDEX_FILE` and receive-side environment, and the `Side` for events such as
  `reference-transaction` that run both locally and while receiving. Not covered: the Windows `.exe` suffix lookup,
  warnings for invalid `hook.*` values, and byte-exact names and commands of configured hooks.
- 2026-11-04: thread 4 (ref-store abstraction and interim reftable refusal) is done, in four PRs. A repository whose
  `extensions.refStorage` is not `files` opens, but every reference operation fails with an `Unsupported` error and
  leaves the git directory untouched (#24); `gix::init()`, `init_opts()` and clone follow Git's ref-format precedence and
  refuse a requested reftable, and `ein init --ref-format` exists. Probe P12 confirmed the hazard first: a tag written into a reftable
  repository landed as a loose file that Git never reads, and reads reported existing references as missing
  (assessment 4.1). The abstraction came in three steps: the borrowed
  `gix_ref::StoreRef` view with `Snapshot` and neutral transactions (#25), the call sites moved onto
  `Repository::ref_store()` with `view::NotFound` as the neutral not-found marker (#27), and one breaking change that
  makes `Repository::refs` and `gix::RefStore` an owned `gix_ref::Store` (#29, break list in its commit body).
  The reftable ref backend (thread 7) adds a reftable arm to the private backend enums. Notes for it: `Store` is `Clone` and `with_locations()`
  can't fail, so a reftable store has to open lazily and clone cheaply, and missing references and failed transaction
  constraints must raise the `view::` error types. Deviations and limits:
  - `Store::from_backend` is not provided, as the reftable threads haven't asked for it. The unsupported-storage mark stays in
    `file::Store` and `Store` forwards to it.
  - `ein init` applies `init.defaultBranch` and `init.defaultObjectFormat`, which it ignored before, and an invalid
    `init.defaultRefFormat` is an error where Git warns. `GIT_DEFAULT_REF_FORMAT` is ignored by the isolated options
    of `init_opts()`. `feature.experimental=true` in the global configuration now makes init and clone fail, as
    they would create reftable. `gix clone` has no `--ref-format` flag.
  - Replace references and `onbranch` conditions are read while opening a repository and stay lenient: an unsupported
    store yields no replacements instead of an error. `GIT_REFERENCE_BACKEND=files` doesn't override a configured
    unsupported backend, which differs from Git.
  - `references().peeled()` iterates and peels from separate `packed-refs` snapshots. Negotiation in `gix-protocol`
    now uses one. `view::transaction` re-exports the files `PackedRefs` hint, which a neutral type should replace once
    the durability work has settled that module.
  - A failed write below `refs/notes/` leaves a reflog file behind (found by P12). The guard doesn't cause this, and it
    was forwarded to thread 5.
  - Probe P11 is in assessment section 8. Runtimes: `mise run check` took 26 min 11 s (exit 0) at station load 6 to 7 with
    other workers running, against no recorded baseline. The 104-case feature-matrix compile of its `check` stage
    dominates, while the main nextest run took 81 s. `mise run test` took 2 min 36 s and 2 min 59 s on the last two revisions at load 6, and 4 min 15 s
    at load 7 on an earlier one, against the 120 s budget; each includes compiling after an edit. Neither is comparable to the warm low-load baselines.
  - Edits were made in the anchor worktree on the laptop and copied to the station. Every build and test ran on the station.
- 2026-11-05: thread 2 (client protocol and bounded packs) is done as `gix_protocol::push` and `gix_pack::data::output::missing`.
  `push()` is blocking only, and needs a `Request` that was validated against what the server advertised. Its pack
  producer is a callback, and `missing::write_pack()` is the one to use. Tests run against real `git receive-pack` for SHA-1
  and SHA-256, with Git 2.43, 2.55 and 2.56. The limits are decisions for later threads:
  - Bounded memory means the object list and the metadata of every object, plus about three chunks of
    `missing::Options::chunk_size` objects with the default of one thread. A large blob is held whole, whereas Git streams
    blobs above `core.bigFileThreshold`, and streaming deflate is deferred. Memory isn't bounded with more than one thread,
    as the order of chunks is restored with an unbounded buffer and chunk sizes are chosen to distribute work. This isn't
    meant to match Git's memory use.
  - A shallow source isn't refused up front. Its missing history surfaces as a `NotFound` error naming the object. The
    repository push thread has to add the typed refusal.
  - Receivers without `ofs-delta` get whole objects instead of copied deltas, so their packs are bigger.
    Re-encoding deltas is a parking-lot item.
  - Tree diffs compare by path, so an object that moved to another path is sent again, which Git avoids by excluding
    everything reachable from the boundary trees. Pushes of renamed large files are bigger. Parking lot.
  - Without commit-graph generation numbers, clock skew can add commits the receiver has. The pack is bigger but valid.
  - Rejections are classified from Git's reason strings. `failed to update refs` and `transaction failed to start`
    for a non-atomic push can hide a deletion that was committed, so they are `Status::Uncertain`.
  - Not done: an async driver, signed pushes, thin packs, and a typed record of what the server redirected.
  - `Options::progress` is off by default, as the driver can't read while it writes the pack. A server that fills its
    output pipe could stall it. That can't be provoked with a pack that fits a test, which finished with and
    without progress.
  - A real peer found that push options were sent with a `push-option ` prefix that only signed pushes use.
  - `mise run check` takes about 12 minutes when a change reaches `gix-pack`, as the whole workspace depends on it.
  - Parking lot: bounded reordering for a multi-threaded `write_pack()`, an atomic push that fails part-way
    through finishing on the files backend, and the failure strings of `proc-receive` hooks.
- 2026-11-04: the owner decided the assessment's section 10 items. They accepted the
  storage, default-format and conversion recommendations ("Accept all three"), and kept
  the full scope as sequenced ("Full scope, as sequenced"). They authorized probes P1–P5
  and P6, P7, P11 and P12 within the resource limits; P9 and P10 stay deferred. On the
  reftable defect they asked: "Should we land support now? If not, we can refuse ref
  ops." Reftable support is priced at 6.5–10 sessions and needs the ref-store abstraction
  first, so the interim refusal lands now in thread 4. The overseer also started the
  standalone reftable codecs and stack layer (thread 6) now, rather than after the base
  work, so that support can land sooner.
- 2026-11-04: thread 6 (reftable codecs and stack layer) is done as the standalone `gix-reftable` crate, with no `gix-ref`
  dependency and no backend integration. Probe P6 found the upstream port worth adopting, so the record, block and table codecs follow
  its structure under `(MIT OR Apache-2.0) AND BSD-3-Clause`, the owner having chosen to add the latter for the code that
  derives from Git's reftable library. Its stack module wasn't adopted: it neither locks nor checks for concurrent writers,
  and flushes and compacts differently. The parity review changed the port: names are bytes, the writer streams and lays
  blocks, padding and indices out like Git, keys must ascend, seeking uses the index, lengths are bounded, and errors are
  `gix-error` errors. The stack layer reads, adds tables under the `tables.list` lock, compacts by Git's geometric policy
  and cleans up. Git reads what the fork writes and the fork reads what Git writes, for both hashes, with tombstones and
  logs, and the compaction choices and merged bytes match Git's `pack-refs`.
  - Differences from Git that are decisions: readers fail closed on a missing, damaged or foreign table, where Git's
    readers treat it as an empty stack and the same goes for a list that keeps changing past the deadline; names in
    `tables.list` must be plain file names; Git's stat-based staleness check is a content comparison; the object index is
    read but not written; compaction is kept out of `Addition::commit()`, so that its failures can't misreport a published
    change; and `Addition` reloads a stale view under the lock, like Git, leaving expected old values to the caller.
  - A failed replacement of the list is told apart: when the old list is in place the added tables are removed, but when
    the new list may be in place, which includes a list that can't be read, they stay and the error is a typed
    `UncertainAfterPublish`. Lock contention is retryable and a stale view in a compaction is a conflict.
  - P4 results behind this: a held lock fails after about the 100 ms `reftable.lockTimeout`; Git 2.56 readers swallow a
    missing or damaged table as no references while its writers fail; a transaction over several stacks commits the
    worktree stack first, so a failure on the main stack leaves the first published; `pack-refs` removes orphan tables;
    and there is no `reftable.autoCompaction` setting. Whether Git stays correct when it fails after publishing wasn't
    probed, as there is no deterministic trigger. The fork covers it with a fault seam in its own tests.
  - No `gix-reftable` has been merged upstream as of this date, and two draft upstream pull requests only add fixtures
    for one. They plan for `gix-ref` to consume the crate, which matches this layout.
  - Not covered: Windows, expiring reflog entries while compacting, writing the object index, and Git compacting
    tables below the fork's range inside its unlock window, which would contend for the same locks. The backend adapter
    in `gix-ref` and migration wait for thread 4's abstraction. `Stack` and `Snapshot` are `Send + Sync` and `Snapshot`
    is cheap to clone, so an adapter can open the stack lazily behind an `Arc`.
- 2026-11-04: thread 5 (durability plumbing and differential fault harness) is done, in four pull requests (#22, #32, #36 and #39). Git 2.56.0's
  flush behaviour was measured under `strace` (probes P1, P3 and P5) and is recorded in
  [push-durability-evidence.md](push-durability-evidence.md), and the fork follows it as follows:
  - `gix_fs::fsync` parses `core.fsync`, `core.fsyncMethod` and `core.fsyncObjectFiles` like Git and flushes by component,
    with the barrier file that batching needs, which nothing calls yet: the receiver thread wires it. The files store flushes reference and `packed-refs` values while a
    transaction is prepared. `Transaction::commit()` fails with `NothingApplied` or with `Uncertain`, which says how far
    each edit got. Loose objects and the bundle writer's pack and index are flushed as Git does, in the same order.
  - `gix_ref::transaction::observe` reports `Preparing`, `Prepared`, `Committed` and `Aborted` to an `Observer` that may
    refuse the transaction while preparing and once prepared, including the nested events of the `packed-refs`
    transaction. 27 rows compare its events with the `reference-transaction` hook of Git. Threads 7 and 8 take it
    through the neutral wrapper.
  - The differential tests run Git and the fork under `strace` and compare the order of writes, flushes and renames. The
    Linux lanes of Fork CI install `strace` and set `GIX_TEST_REQUIRE_STRACE`, so they fail on a runner that can't trace.
    The runners ship Git 2.56.0, every differential passed on the first run there, and the install step took 2 to 9
    seconds per lane. `golden/p5-git-2.56.0.txt` keeps Git's flush protocol for `receive-pack` as data. No test
    compares it with the fork, because the fork has no receiver; the thread that writes one adds that comparison.
  - Deviations from Git that remain: the files store fails without a committer, where Git derives an identity from the
    system, and a no-op update of the branch `HEAD` points at doesn't append to the `HEAD` reflog, where Git does; both are
    on the evidence page. A commit that fails before it changed anything is reported `Aborted`. The files store takes
    the lock on `packed-refs` before the others, where Git takes it last. An I/O error while reading the target of a
    symbolic reference fires no nested `Aborted`. Whether Git deletes an emptied `packed-refs` wasn't verified.
    Missing values of `core.fsync*` keys are read leniently. A reference whose reflog can't be deleted is deleted, like
    Git, and only deleting nothing but a reflog fails.
  - Not done: flushing the multi-pack index, `gix free pack explode` (no repository configuration) and pack output outside
    the bundle writer. `core.fsyncMethod=writeout-only` flushes fully on platforms without a write-out-only flush, which
    includes Windows, where Git has one. Directories are never flushed, as in Git. Evidence is from killing processes and from `strace`
    only, on Linux and ext4: what survives a power failure is unproven, and macOS and Windows were read from source.
  - Runtimes of `mise run check` on the dev station at the tips of the last two pull requests: 1,297 s with load 5 to 10
    (observer), and 1,815 s with load 5 to 10 and three checks running at once (objects and packs, where a workflow
    change selected the whole workspace). Neither is a baseline.
  - Changes to public fields without `!` on the way: `gix_ref::store::init::Options` gained `fsync` in the transaction
    change, under the 0.x exemption. `gix-odb` and `gix-pack` option structs gained `fsync` with `!`.

- 2026-11-04: thread 7 (reftable ref backend) is done, in PRs #38 (reading and opening), #40 (writing), #42 (reflog expiry
  and deletion), #43 (`init` and clone), #44 and #47 (fixes found on the way) and #48 (the default features). The reviewer accepted
  every one; the owner's decisions held: a store behind the neutral `gix_ref::Store`, Git's ref-format precedence with
  `files` the default for new repositories, and no conversion between the formats.
  - `gix_ref::reftable::Store` reads and edits references, symbolic references, peeled values and reflogs in the main
    repository and in linked worktrees, one table per touched stack and one update index per transaction, like Git. Stacks
    are locked before anything is written, so a failure maps to `NothingApplied` or `Uncertain`. Auto-compaction follows
    each commit. `StoreRef::reflog_expire()` and `reflog_delete()` exist for both formats; there is no `--rewrite` or
    `--updateref`. `init` and clone create reftable repositories on request (`--ref-format`, `GIT_DEFAULT_REF_FORMAT`,
    `init.defaultRefFormat`, `feature.experimental`) and Git reads them back; the first table matches Git's range.
    `reftable.*`, `core.fsync` and `core.sharedRepository` configure how stacks are written.
  - Default features: `reftable` is on in `gix` and in the `max`, `max-pure`, `lean` and `lean-async` presets of
    `gitoxide`, and off in `small`, which exists to stay minimal and can't clone; it stays available there as
    `--features reftable`. Cold release builds on the 8-core station, one sample per row, only the feature toggled:
    `gix` binary +1.5 to +2.9% (small +2.9%), `ein` binary +5.5 to +14.4% (small +14.4%, from 4.42 to 5.06 MB), cold
    build time within noise (load 6.5 to 12). `ein` grows by 0.65 to 0.70 MB in each preset. In the `.text` of `ein` with
    `small`, `gix-reftable` adds 90 KiB, `gix-ref` 72 KiB, `gix` 41 KiB, `std` 34 KiB and `crc32fast` 5 KiB: the
    functionality itself, not duplicated generics. The growth is above the 3% that was set as a limit for `ein` in all
    presets, and for `gix` in `small` it is just below it. The overseer chose including it in the full presets and leaving
    `small` out. With the feature off, operations on references of a reftable repository keep failing as unsupported.
  - Runtimes. `mise run check` on the station at the tip of #48: 14 tasks, exhaustive because `Cargo.toml` changed, 920 s;
    `mise run check:msrv` 936 s. CI at the tip of #48: `tests@linux-x86_64` 7m16s, features lanes 5m17 to 6m20. That lane
    was 5m36 to 7m08 on `dev` before this thread, so the lane the FORK.md 5-minute target applies to was already above it;
    #40 moved total CI from 7m05 to 7m54 with an identical set of compile units in that lane, which is runner variance. The
    new tests run in under a second each, both hashes (`git_cases.rs`: 14 cases in 0.4 s). `mise run check:thorough` on the
    station at the tests' tip (a960c728e; the next commit changed only documentation): all 100 QA cases passed, with 5,604
    tests in `native-tests`, in 1,263 s (21 m 03 s) with Git 2.56.0 first on `PATH` and no other check holding the lock. That is
    above the 12-minute budget and the 530 s baseline that the overseer gave for it. The load average was 2.9, 3.3 and 4.0 (1, 5 and 15 minutes) when the run started on the
    8-core machine, and 0.1, 2.7 and 4.9 right after it. The log has no per-stage timestamps, so the stages that took the
    longest can't be named from it; what it does show is that the 100 QA cases ran in the order `self-test`, `policy`,
    `housekeeping`, `lint`, `feature-builds`, `feature-contracts`, `native-tests`, `isolated-tests` and `feature-tests`,
    and that the test executions of `native-tests` took 66 s and 84 s in its two passes of 5,604 tests, so most of its time is
    likely compiling, which wasn't measured. A budget review needs the stage timings, and they weren't investigated here.
  - Differential coverage: Git 2.56 reads what the store writes (`for-each-ref`, `reflog show`, `refs verify`, `fsck`)
    and the store reads what Git writes, for both hashes. `git_cases.rs` mirrors the cases of `t0610`, `t0613` and
    `t0614` that apply to a store of references, and its module doc lists the rest by where they are covered or why
    they don't apply. Git 2.56 reads nothing from a stack with a corrupted table or a list naming a missing one
    (`for-each-ref` prints nothing and exits 0) and crashes in `git refs verify` on it, so those cases compare whether Git
    finds a reference, not a verdict.
  - Deviations from Git that remain: no object index is written, and tags are written as one value rather than a value
    and its peeled value, so table bytes differ from Git's though the content and `refs verify` agree; Git's `--rewrite`
    and `--updateref` of `reflog expire` don't exist; `reftable://<path>` payloads are refused; the reftable store has its
    own copy of the `HEAD` reflog rules and of reflog message normalisation, which thread 8's shared functions replace; Windows isn't covered by
    CI or by any test run here.
  - Files reflog divergences, for thread 8 to fix. The reviewer verified each with a probe against Git 2.56.0 on the same
    repository; Git lines are for v2.56.0.
    1. Updating the referent of `HEAD` directly writes no `HEAD` entry. Git adds a log-only update in
       `refs/files-backend.c:2499-2531` (`split_head_update`, called at :2709). The files arm logs `HEAD` only for edits
       through `HEAD` with `deref: true`. `git update-ref -m direct refs/heads/main <c1>` logs `direct` for `HEAD`; an
       `Update` of `refs/heads/main` with `deref: false` doesn't.
    2. A same-value update writes no `HEAD` entry. Git skips the branch entry (:2859-2864, :3355-3357) and still logs
       `HEAD`. The files arm returns no entry when previous equals new (`gix-ref/src/store/file/transaction/commit.rs`,
       `reflog_entry()`) and has no log-only `HEAD` update. `git update-ref -m same HEAD <c2>` with `main` at `c2` logs
       `same` for `HEAD`; the files arm logs neither.
    3. With `core.logAllRefUpdates=false`, Git still appends to reflogs that exist (`refs/files-backend.c:1912-1953`,
       `log_ref_setup`); `WriteReflog::Disable` never writes (`gix-ref/src/store/file/loose/reflog.rs`, `Disable => Ok(())`).
       `git -c core.logAllRefUpdates=false update-ref -m off refs/heads/side <c1>` logs `off` for a branch that has a reflog.
    4. Reflog messages are written as given, so a newline corrupts the file; Git normalises them (`refs.c:1030-1046`,
       `copy_reflog_msg`, applied at :1350: leading whitespace dropped, whitespace runs including newlines become one
       space, the end trimmed). `Update` with the message `"  a \t b\n\nc  "` writes three physical lines where Git writes
       `a b c`. This is a correctness bug, not only a divergence, and the reftable arm already normalises.
    Not divergences: updating through a dangling symbolic reference, and deleting then recreating a reference in separate
    transactions. Not probed against the files store: both in one transaction. The `HEAD` reflog after `worktree add` is
    `gix` behaviour and was compared only in a reftable repository, with Git 2.56: for an attached branch Git's log of the
    new worktree has an empty-message entry and a `reset: moving to HEAD` one (only the empty one with `--detach`), and
    `gix` writes the single empty-message entry.
  - Open follow-up: when thread 8's `HEAD`-log PR lands, the reftable arm's own `head_log_edit` and same-value rule are
    replaced by their shared functions and `head_log()` is wired in.
- 2026-11-04: thread 9 (repository push and fetch) is done: `gix::remote::push` (updates with leases, plan, send, outcome, reconcile,
  pre-push seam), `Prepare::with_prune()` for fetch, and the plumbing under them (a drain after the report without side-band
  and a typed marker for a producer failure after a complete pack in `gix-protocol`, `wait_for_server()` in `gix-transport`)
  and a `gix-refspec` fix that its review found: `expand_prefixes()` sent `refs/*` literally and nothing for `refs/*/main`,
  which narrowed the listing so far that pruning would have deleted destinations whose sources exist. The new push API isn't
  gated with `gix_macros::unstable` because `gix` has no `unstable` feature; the overseer accepted that.
  - Deliberate differences from Git: prune happens after the pack is received, and only a stale reference that blocks a new one
    (`a` against `a/b`) is deleted before, in its own transaction; a prune deletion expects the id it observed, so a racing local
    writer fails the update instead of being pruned over; the refusal of a shallow source is more conservative than Git's.
  - Known limits: a lease object that the server garbage-collects between the advertisement and a delete makes that delete
    unconditional; an advertisement can't tell "applied, then reverted by another writer" from "not applied"; Windows isn't covered.
  - Left open, found by the reviewer, none blocking:
    1. After a failed pack on `git://` a flush may be written while the transport is dropped.
    2. Packing can't be cancelled midway.
    3. The shallow walk runs once per pushed tip.
    4. Some shallow pushes that Git accepts are refused, as the docs of `ShallowSource` say.
    5. A delete can look applied at reconcile time if the reference has become hidden.
    6. Updates that a `proc-receive` hook redirects can't be reconciled by name.
    7. `Outcome::pruned()` has no test with packed references.
    8. The prune guard checks that a spec sent some prefix, not that the prefix covers its sources.
    9. `wait_for_server()` has no Windows or async coverage.
  - `mise run check` took 18:41 (exhaustive selection of 207 QA cases, 250% CPU) and `check:thorough` 10:05 (211% CPU) against
    the 530 s baseline, both on the shared host with load around 1.5 to 4.5.
- 2026-11-04: the owner chose the HTTP backend for thread 10: "Reqwest and rustls, apply the low-speed limits, proxy and CA
  settings." Thread 10 makes the `reqwest` backend with `rustls` the primary one for push and fetch, applies those
  settings on it, and keeps `curl` working. The backends that existing presets select don't change in this thread.
- 2026-11-04: thread 10 made the `reqwest` backend carry the same traffic as `curl`. It now performs requests with the
  asynchronous client on one thread of its own, which replaces `reqwest::blocking` and its 30 s limit per wait, and applies
  the connect timeout, the low-speed limits (`curl`'s rule: below the speed for the whole time), proxies with basic
  authentication, `no_proxy`, an empty `http.proxy`, `http.sslCAInfo`, `http.sslVerify` and `http.sslVersion`. Each is tested
  against a local peer with both backends. Decisions and deviations:
  - The hook types `ConfigureClientFn` and `ConfigureRequestFn` of the `reqwest` backend now take the asynchronous
    `ClientBuilder` and `Request`, which is `change(gix-transport)!`. The backend panics on a `tokio` runtime thread.
  - A new feature, `blocking-http-transport-reqwest-rust-tls-ring`, selects `rustls` with `ring`, as the owner preferred it
    if `reqwest` 0.13 supports it cleanly. It installs `ring` as the process default unless one is installed. No preset
    changed. Cold release builds of a scratch consumer at `-j4`, one run each at load 1.5 to 3.2: `curl` 39 s and 4.34 MB,
    `reqwest` with `aws-lc-rs` 63 s and 9.74 MB, `reqwest` with `ring` 50 s and 7.64 MB (P11: 45 s and 4.12 MB for `curl`,
    89 s and 9.45 MB for `reqwest`).
  - Open for the owner: `curl` reads the proxy variables of the environment itself when `gix` hands it no proxy, even where
    `gix`'s permissions deny them, and `reqwest` doesn't. Making `curl` strict would change precedence, so it stays.
  - The `curl` backend failed on the first `407` of a proxy with credentials and sent an empty password when the URL had a user
    and no password. Both are fixed. A push handshake now ends with Git's probe, a `POST` of `0000`, after `info/refs`.
  - Not done in this thread: Digest, Negotiate and NTLM proxy authentication on `reqwest`, `http.<url>.*` scoping, `http.sslCAPath`,
    and the lower bound of a TLS range on `rustls`, which has nothing older than 1.2.
- 2026-11-04: thread 10 made transfers cancellable and fixed two bugs of the `curl` backend that its push tests found.
  `TransportWithoutIO::interrupt_handle()` ends processes, sockets and both HTTP backends from another thread, and
  `InterruptHandle::watch()` ties a flag to it. `Prepare::receive()`, `Prepare::send()` and `gix free pack receive` use it, so a
  flag ends them while the server is silent. An interrupt before the pack is sent is a plain failure, later it is an uncertain
  outcome. Each fix has a test whose removal makes it fail or hang. Findings of the review: 2 material of 6 in the first round,
  0 of 3 in the second. Decisions and deviations:
  - The `curl` backend dropped a failure of a transfer when its pipe was full, so a reader saw a body that ended early and not
    the reason; this failed the CI of the previous chunk once and 11 of 24 parallel runs on the build host. It also kept the size
    of a buffered upload for the next, streamed one, so a push over HTTP with `curl` never finished. Both are fixed.
  - A consumer that reads late doesn't trip the low-speed limits on either backend, which the plan asked to pin. A silent
    `pre-receive` hook is not tested against the limits, as the peer relays the output of `git http-backend` once it is complete;
    the keep-alive packets that Git sends meanwhile are read past by both backends, which is tested.
  - A kernel quirk keeps the low-speed limit from ending a stalled upload to a loopback peer with `curl`, so that is pinned for
    `reqwest` only. An interrupt ends it with both.
  - Cancellation of `curl` takes up to a second. Not done: interrupting the opening of a connection, and a flag for the handshake.
- 2026-11-05: thread 10 (SSH, HTTPS, authentication and client hooks) is done. Completion evidence, each with tests against local
  synthetic peers (the `network` test target of `gix`):
  - Authenticated peers as oracles with both object formats: an HTTP peer in front of `git http-backend` (basic and proxy
    authentication, redirects, TLS with generated certificates), a program standing in for `ssh`, and a real `sshd` where installed.
  - Credentials without hidden dispatch: the native cascade (get, store, erase, chaining, reset) and a test that puts programs named
    like `git` and its helpers in front of `PATH` and runs authenticated fetches and pushes with both backends and over `ssh`.
    What still runs Git is listed in `crate-status.md`: `file://` remotes, `gix_credentials::builtin` in the two commands without a
    repository (`gix free remote refs` and `gix free pack receive`, which have no configuration to build a cascade from), the one-time
    probes of `gix-path` and the tools that compare with Git.
  - Client `pre-push`: `Prepare::with_pre_push_hook()` runs the hooks of `Repository::hook()` through thread 9's check before sending. It
    is compared with `git push` for a new branch, a fast-forward, a deletion, an update that is up to date and one that isn't a
    fast-forward: same arguments, directory, `GIT_PREFIX` and standard input, and Git runs the hook even if everything is up to date,
    so `gix` does too. A failing hook sends nothing; not calling it is `--no-verify`.
  - Pushes with leases and atomic updates over HTTP (both backends), `ssh` and a real `sshd`, for `sha1` and `sha256`, checked by
    looking at the repository that was pushed to.
  - Low-speed limits and cancellation as in the previous entries, plus: a silent server hook ends a push with the limits as it does for
    `git push` (compared), and an interrupt ends fetch and push while the server is silent.
  - Refusing local programs: `protocol.file.allow=never` and `protocol.allow=never` refuse `file://` before a program runs (tested).
  Decisions and deviations:
  - `with_pre_push_hook()` is opt-in and behind the `hook` feature, as running programs is.
  - The owner's call, taken by the overseer: `curl` keeps reading the proxy variables of the environment, `reqwest` doesn't.
  - Not verified: Git's behavior with the low-speed limits when a server streams keep-alive packets during a hook, as the test peer relays
    the output of `git http-backend` once it is complete.
  - Not done, parked: a flag for the handshake and for opening a connection, bounds on helpers, askpass, `ssh -G` and `pre-push`,
    Digest, NTLM and Negotiate proxy authentication, `http.<url>.*` scoping, async transports and `Expect: 100-continue`.
- 2026-11-04: thread 8 (local consumer backend), progress and what it left open so far. Merged: the files store's reflog
  messages are cleaned like Git's (#50), `HEAD` gets the reflog entry that Git writes when the branch it points to changes,
  in both stores, with `HeadLog::CallerDecides` as the explicit opt-in for the one consumer that edits `HEAD` and its branch
  in one transaction (#53, `gix-tix` uses it), and the reftable arm decides alike and refuses to create existing references (#58).
  This PR makes `core.logAllRefUpdates=false` and bare repositories append to existing reflogs instead of writing none.
  Each is pinned against Git 2.56 for both hashes; the reftable arm is covered by differential tests of its own.
  - Open items, deferred on purpose. The refusal for a transaction that edits `HEAD` and its referent lacks `reference` metadata,
    and Git's wording when the verification comes first is not matched. The observer's view of a refused transaction, `HEAD` logs
    in worktrees and namespaces, `WriteReflog::Always` and `Disable` with `HEAD` edits, a delete with `ExistingMustMatch` on an
    absent branch, and that `edit_references` defaults to Git's rules are untested. Deletions don't log an empty message for `HEAD`.
    A caller-supplied log-only edit of `HEAD` next to a branch edit is refused by the files store in Git's mode and accepted by
    reftables. A repository with leftover reflogs but no identity now fails with `NothingApplied` where it wrote none before.
    Reference lookup and listing still differ from Git in three ways: dangling and absent references are not told apart, a
    broken reference stops iteration where Git's policy differs, and `prefixed("refs/heads/m")` matches `main` and `mine`.
  - Runtimes on the 8-core station, one sample each, against the 530 s baseline and the 12-minute budget in FORK.md:
    `check:thorough` took 1,185 s at the tip of the commit that changes the default (load 5.3, 4.4 and 3.7), after 904 s
    for #41, 1,236 s for #53 and 1,190 s for #58. All are above both numbers; the stages weren't timed, and each run followed a
    rebase that rebuilt the dependents of `gix-ref`. `mise run test` takes 37 s when nothing changed and 121 to 147 s after a
    change to `gix-ref`, against a 120 s budget.
- 2026-11-04: thread 8 delivered the `reference-transaction` hook for local reference changes, and the owner's answer for how
  closely it follows Git is "Copy Git exactly". `Repository::edit_references_with_hooks()` runs the hook through the hook runner
  and the transaction observer of thread 5, with Git's arguments and input lines, a refusal in `preparing` and `prepared`, no effect
  of the exit code afterwards, and a refusal of any change while `GIT_QUARANTINE_PATH` is set. Every nested sequence is pinned by a
  differential test that runs `git update-ref --stdin` and this method with a recording hook, for the files store and for reftables:
  creating, updating with and without an expected value, deleting a loose, a packed and a doubly stored reference, updating through
  `HEAD`, a mixed transaction, refusals while preparing, once prepared and after the commit, and a stale expected value. This includes
  the transaction on `packed-refs` that Git opens inside the outer one, which a hook can't tell from it. `edit_references()` and its
  relatives don't run hooks.
  - Also merged in this thread since the last entry: `references().under()` with `git for-each-ref`'s pattern meaning (#62);
    `open::Options::ignore_repository_environment()`, `gix::init_or_open()`, `Repository::is_ancestor()` and byte comparisons of
    configuration edits with `git config` (#63); `set_raw_value()` replacing a value that an earlier section holds (#64); and
    comparisons of written blobs, commits, trees and tags, the empty tree, tree listings and short ids with Git, with `core.abbrev`
    values longer than a hash accepted as Git does (#65). The reference lookup differences listed above are answered as follows:
    dangling and absent references are told apart (a dangling symbolic reference is found and fails to peel), a broken reference is
    an error item of an iterator where Git skips it with a warning, and `under()` matches by whole components.
  - Still open from this thread. A batch write of objects into one pack was not examined: nothing in the repository API writes a set
    of objects as a pack, which the push path does for itself. Writing commits isn't compared for messages with a NUL byte or invalid
    UTF-8, `i18n.commitEncoding`, and names that Git cleans of `<`, `>` and surrounding spaces; re-encoding Git's commits with `gpgsig` or
    `mergetag` is untried; ambiguity extension of short ids was not compared with many objects; `core.abbrev=0` and `off` are accepted
    as "no abbreviation" where Git rejects `0`. Short ids count only packed objects for `auto`, where Git estimates loose ones too.
    `config`: formatting differences to `git config` are pinned in `git_bytes.rs`, none changes how Git reads the file.
    The hook runs on the local side only, so a receiving process that wants Git's directory and `GIT_DIR` for it needs a variant
    that takes the side; its events name references without a namespace, which is right for `update-ref`, while Git's receiving
    side passes namespaced names; and a refusal carries the marker `Vetoed` but not Git's wording.
- 2026-11-04: thread 8 is done. Its comparisons with Git also run on the reftable arm: the `reference-transaction` hook rows (#66), the
  HEAD-log rules for deleting the branch `HEAD` points at, appending to existing reflogs without creating one, and `under()`, which
  were files-only before, plus the transaction, expected-value and reflog rows that thread 7 and #58 already compared on reftables.
  `mise run check:thorough` passed all 100 QA cases at the tip of #66 (811ff5e84) on the 8-core station in 1,718 s, which isn't
  comparable with the 530 s baseline as it queued behind and ran next to another measurement on the same machine. The open items of
  the entries above stay open; the hook's `Side` variant and the namespaced event names of the receiving side go to thread 11.
- 2026-11-05: thread 12 (native maintenance) is done, in PRs #69 (start), #74, #76, #77, #78, #79, #81, #82, #83, #86, #88, #89 and #90, and the closing PR. Git-free maintenance
  now exists for the consumer's "Maintenance" row: `Repository::gc()` and `Repository::run_maintenance()` (with `--auto` conditions, the
  `pre-auto-gc` hook, `gc.pid` through `gc.pid.lock` and `objects/maintenance.lock`, `gc.log`, pack garbage and the tasks `gc`,
  `loose-objects`, `geometric-repack`, `pack-refs` and `reflog-expire`), on top of `repack` (cruft and geometric packs, `.keep` markers,
  recovery after a kill at every step, a delta search that follows Git's choices), `prune`, `expire_reflogs`, `StoreRef::pack_refs()` and
  `StoreRef::pack_refs_required()`, and `Repository::auto_maintenance()` for the `receive.autogc` seam. The native local transport
  of thread 11 now runs it after a push, compared with Git's `receive-pack` over both transports. Tests compare with Git 2.56's `gc`,
  `gc --auto`, `maintenance run`, `repack`, `prune`, `reflog expire` and `pack-refs --auto`, both hashes and both reference formats, and kill a repacking
  process at every step. The findings that changed designs: a `repack` must never remove a pack whose index is missing, so the index is
  renamed to `*.idx.removing` first; Git's `gc` takes `gc.pid.lock` and not `gc.pid` directly; and a geometric repack that would roll up all
  packs must be a full cruft repack, as it is the only place the default strategy expires unreachable objects. Git 2.56's code and documentation
  disagree on the default reflog ages (see [the evidence page](push-durability-evidence.md)).
  - Decisions made by the thread: `receive` implies `maintenance`; unsupported tasks and settings are reported and never fail a run; `gc()` holds both
    locks, so a stale `maintenance.lock` blocks it, and `force` never overrides that lock; `gc()` doesn't detach, and neither does the receiver's
    run; a promisor repository or one with precious objects is not touched and says why.
  - Deviations and limits, also in `crate-status.md`: no `prefetch`, `incremental-repack`, `commit-graph`, `worktree-prune` or `rerere-gc`; no
    `gc.bigPackThreshold`, `--keep-largest-pack`, `--aggressive`, `gc.repackFilter`, `gc.maxCruftSize` or `--expire-to`; no multi-pack-index, commit-graph or bitmap
    writing; no detaching and no `gc.log` written; on platforms other than Linux a `gc.pid` or marker is assumed to be alive; with an expiry, a second
    walk doubles the time that finding what a newer object needs takes; geometric and cruft packs get no delta search; `pack.depth` isn't clamped to 4095 and there is
    no `pack.deltaCacheSize`; a kill while loosening leaves one object dated now; the `reflog-expire` task can become due later than in Git; and the
    delta encoder makes packs a few percent larger than Git's, which thread 13 owns.
