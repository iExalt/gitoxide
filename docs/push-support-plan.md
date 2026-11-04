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
thorough-check budget is 12 minutes against a warm baseline of 530 seconds. No spending beyond
existing subscriptions is approved.

| Thread | Scope | Depends on | Human | State |
| --- | --- | --- | --- | --- |
| 1. Architecture assessment | Assessment deliverables 1–6 above. Read-only: no builds, mutating probes or fault injection. One docs PR, ending with an explicit stop and report | None | The owner accepts the report and decides storage, default format, conversion policy, revised scope and experiment authority | done: [assessment](push-architecture-assessment.md) delivered; owner decisions recorded below |
| 2. Client protocol and bounded packs | Plumbing that takes validated old/new/ref commands and a pack producer; strict command/status parsing; real `receive-pack` acceptance with Git as test oracle; complete object closure, remote-have exclusions, bounded streaming, both-hash fsck. Excludes the repository push API and lease policy | None | None known | done |
| 3. Hook runner | `core.hooksPath` resolution; argument, stdin, environment and working-directory parity with Git; veto versus notification exit handling; missing and non-executable hooks; Git 2.56 config-defined hooks, parallel jobs and per-hook stdin producers | None | None known | done |
| 4. Ref-store abstraction and interim reftable refusal | Probe P12 first; refuse every reference operation, reads and writes, on a repository whose `extensions.refStorage` is not `files`, with an `Unsupported` error, while opening and object access keep working; `gix init` follows Git's ref-format precedence and refuses a requested reftable; then the staged ref-store abstraction (assessment 5.1, additive handle first) and its call-site migration; probe P11 | None | None known | ready |
| 5. Durability plumbing and differential fault harness | Probes P1, P3, P5 and P7; `core.fsync` and `core.fsyncMethod` for objects, packs and refs, including the barrier flush; the files-store gaps in assessment 4.3; an uncertain-outcome class; a transaction observer; P1, P3 and P5 turned into differential tests against Git | None; rebases on thread 4 where both touch `gix-ref` | None known | ready |
| 6. Reftable codecs and stack layer | Probes P6 and P4; adopt the upstream reftable codecs after a parity review, both hashes and version 2; a new stack layer following Git's lock and publication protocol, with reload, segment compaction, cleanup and flush; interoperability tests where Git reads fork-written tables and stacks and the fork reads Git's. Integration as a ref backend waits for thread 4 | None for the standalone crate | None known | ready |

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
