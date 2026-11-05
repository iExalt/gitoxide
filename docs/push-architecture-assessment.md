# Push support: architecture assessment

Status: complete, awaiting the owner's decisions (section 10). This is the bounded, read-only assessment that
[the push support plan](push-support-plan.md) requires before storage, receiver and
local-backend implementation. It makes no implementation decision. It builds nothing,
runs no probe and measures nothing; every "not run" and "source observation" label below
is meant literally.

## Assessment budget and stop

The plan asked for the original 3–5 engineer-day allowance to be restated in agent
effort before starting. This assessment was allotted one worker session of three chunks,
about 5–8 hours wall-clock with the agent doing the reading and writing, plus one
persistent reviewer agent that verifies each claim against the pinned sources. Each
chunk gets two reviewer handoffs. Actual: about 17 minutes by the sandbox clock from
launch to the last review handoff (01:43 to 02:00 UTC on 2026-11-04), including the
reviewer rounds, not independently verified; read-only, no builds. The assessment ends
with a stop and a report to the owner, who decides what follows; nothing in this document
authorizes a build, probe, conversion or release.

## Summary

- **Receiving is larger than a hook-free receiver, and than the earlier estimates covered.** At Git 2.56.0, receive-pack involves
  config-defined multi-hook execution, a quarantine with an unconditional flush barrier,
  four `reference-transaction` states whose call sequence differs by backend, batched
  non-atomic updates whose failure handling can report applied deletions as failed, and
  `updateInstead` updating the worktree before the ref commit (section 2).
- **Git's own ref backends are not atomic or durable by default.** Files-backend multi-ref
  commits are per-ref renames with partial states (including a reflog entry for a move that
  never happened); reftable is atomic per stack but not across worktree stacks, can report a
  published transaction as failed, and neither backend flushes refs by default (section 3).
- **The fork has the files transaction's shape but not its durability.** It has no flush
  anywhere, no hooks, no quarantine, no server-side protocol and no reftable; and today it
  can open a reftable repository with the files store, an unsafe-write hazard to close
  first (section 4).
- **An upstream reftable port exists but covers only the format codecs.** Its stack layer
  has no lock, no stale-view check, no flush and a compact-all compactor, so the
  coordination layer must be built new (section 4.2).
- **Recommendations** (section 10): an enum store reached through an additive handle,
  reftable from adopted codecs with a new Git-protocol stack, Git's default format with
  explicit refusal of unsupported requests, no automatic conversion, and a two-tier probe
  authorization.
- **Estimate** (section 9): roughly 17 to 29 sessions for the Git-free client and local
  backend (the least reliable total), plus 6.5 to 10 for reftable and 12.5 to 18.5 for full
  receiver parity, each priced separately; hours per implementation session are unmeasured
  and should be calibrated from the client-protocol and hook-runner work the plan starts
  alongside this assessment.

## Method and labels

- **Git target.** Git v2.56.0 (tag object commit `a018953688f1b10bddf91bff8747068f5f4746a4`),
  the version the CI runners ship. Git citations are `path:line` at that commit.
- **Fork target.** Fork `dev` at `b592770fc`. Fork citations are `path:line` at that commit.
- **Labels.** Every statement is a *source observation* unless it is marked *ran* with its
  command. No statement here has been verified by running Git or the fork, except the
  grep-style searches named where they appear.
- **Coverage limit.** The matrix enumerates behavior by reading source. Enumeration is
  mechanical and meant to be closed: every configuration key read on the receive path,
  every hook call site, every capability string. Behavioral *proof* is a separate matter:
  no row below has been exercised, and each row is a candidate differential test.

## 1. Pinned compatibility target

| Item | Pin | Evidence |
| --- | --- | --- |
| Git version | 2.56.0 | `FORK.md` states the CI runners ship Git 2.56; tag `v2.56.0` resolves to `a018953688f1b10bddf91bff8747068f5f4746a4` |
| Default ref format of that build | `files` | `repository.h:26-28` selects `reftable` only under `WITH_BREAKING_CHANGES` |
| Default object format | SHA-1 | `Documentation/BreakingChanges.adoc` records no plan to deprecate SHA-1; SHA-256 is selected per repository (`object-format`) |
| Push protocol | v0/v1 only | `builtin/receive-pack.c:2534-2540`: a v2 request falls back to v0 |
| Reftable format | version 1 and version 2 (SHA-256) | `Documentation/technical/reftable.adoc:213-252` |
| Git 3.0 default | `reftable` for new repositories, conditional on other implementations supporting it | `Documentation/BreakingChanges.adoc:121-166` names Gitoxide explicitly as a prerequisite |

Compatibility rule for the fork: match this pin; stay backward compatible with older Git
as far as the plan requires; treat later Git as out of scope until the pin moves.

## 2. Receiving behavior matrix

Scope: everything a native local receiver must reproduce for `git receive-pack` at the
pin, so that it can replace the subprocess for local paths and file URLs, and so that a
network receiver can reuse it. Derivation: every `receive.*`, `transfer.*` and `core.*`
key read on the path, every capability string, every hook call site, and every call in
`execute_commands` were listed by reading `builtin/receive-pack.c` (2636 lines) and the
modules it calls (`object-file.c`, `tmp-objdir.c`, `hook.c`, `refs.c`).

### 2.1 Session, advertisement and request parsing

| Behavior | Source observation | Cite |
| --- | --- | --- |
| Invocation | `receive-pack [--quiet] <dir>`; hidden flags `--stateless-rpc`, `--http-backend-info-refs`/`--advertise-refs`, `--skip-connectivity-check` | `builtin/receive-pack.c:2504-2512` |
| Repository entry | `enter_repo()` tries the suffixes `/.git`, empty, `.git/.git`, `.git`, follows gitfiles, enforces the `safe.directory` ownership check, chdirs into the **git directory** (not the worktree) and exports `GIT_DIR`; failure is fatal | `builtin/receive-pack.c:2527`, `setup.c:1821-1892`, `:1102` |
| Config load | `receive_pack_config` runs over the repository config; unknown keys fall through to `git_default_config` | `builtin/receive-pack.c:2530`, `:136-259` |
| Protocol version | v0 and v1 only; v1 prints `version 1` unless stateless | `builtin/receive-pack.c:2534-2554` |
| Advertisement | Every visible ref as `<oid> <name>`; capabilities on the first line after NUL; `capabilities^{}` with the null id when the repository has no refs; shallow grafts; flush | `builtin/receive-pack.c:261-355` |
| Advertised capabilities | `report-status report-status-v2 delete-refs side-band-64k quiet`, plus `atomic` (`receive.advertiseAtomic`, default on), `ofs-delta` (`repack.useDeltaBaseOffset`, default on), `push-cert=<nonce>` (when `receive.certNonceSeed` is set), `push-options` (`receive.advertisePushOptions`, default off), `session-id=<id>` (`transfer.advertiseSID`), `object-format=<name>`, `agent=<ua>` | `builtin/receive-pack.c:268-281` |
| Hidden refs | `receive.hideRefs` and `transfer.hideRefs` suppress advertisement; hidden refs cannot be updated or deleted (`deny updating a hidden ref`, `deny deleting a hidden ref`) | `builtin/receive-pack.c:140`, `:289-311`, `:1824-1849` |
| Namespaces | `GIT_NAMESPACE`: refs outside the namespace are advertised as `.have`; updates are applied under the namespace prefix | `builtin/receive-pack.c:289-310`, `:1500-1502` |
| Alternates | Alternate object stores' refs are advertised as `.have`, as selected by `core.alternateRefsCommand` and `core.alternateRefsPrefixes` | `builtin/receive-pack.c:313-343`, `odb.c:391`, `:403` |
| Request commands | `<old> <new> <ref>`; malformed lines are fatal protocol errors | `builtin/receive-pack.c:2134-2157` |
| Request features | `report-status`, `report-status-v2`, `side-band-64k`, `quiet`, `atomic` (only if advertised), `push-options` (only if advertised), `object-format` (must equal the repository hash, default `sha1`; mismatch is fatal), `session-id` (trace only) | `builtin/receive-pack.c:2219-2245` |
| Shallow input | `shallow <oid>` lines in the command stream feed shallow-update handling | `builtin/receive-pack.c:2204-2211`, `:2578-2593` |
| Push certificate | A `push-cert` block replaces unsigned commands; mixing both is fatal; the certificate is stored as a blob, its signature checked, its nonce classified (`OK`, `SLOP`, `BAD`, `MISSING`, `UNSOLICITED`) | `builtin/receive-pack.c:2248-2271`, `:2171-2191`, `:774-828`, `:653-740` |
| Push options | Read after the commands when negotiated; inconsistent with a certificate is rejected | `builtin/receive-pack.c:2282-2291`, `:2570-2576`, `:746-772` |
| Status report | Sent only if the client asked for `report-status` or `report-status-v2`: `unpack ok|<msg>`, then `ok <ref>` or `ng <ref> <reason>` per command; v2 adds `option refname|old-oid|new-oid|forced-update` lines from `proc-receive` | `builtin/receive-pack.c:2412-2479`, `:2599-2602` |
| Sideband and keepalive | Hook and pack-processing stderr is muxed into sideband 2; keepalive packets after the first NUL (`receive.keepAlive`, default 5 s) | `builtin/receive-pack.c:481-584`, `:2315`, `:2594` |
| End of stream | A trailing flush is written when sideband is used | `builtin/receive-pack.c:2629-2630` |

Contract note for the client protocol work: the status report is written *after* refs are
updated and before `post-receive`. A connection loss between ref commit and report leaves
the outcome unknown to the client, which is why the plan requires reconciliation
(`builtin/receive-pack.c:2597-2606`).

### 2.2 Configuration read on the receive path

Closed list of keys, each with the behavior the receiver must reproduce.

| Key | Default | Effect | Cite |
| --- | --- | --- | --- |
| `receive.denyDeletes` | off | Refuses deleting `refs/heads/*` (`deletion prohibited`) | `builtin/receive-pack.c:145`, `:1534-1539` |
| `receive.denyNonFastForwards` | off | Refuses non-fast-forward updates of `refs/heads/*` only (merge-base check on old and new commits) | `builtin/receive-pack.c:150`, `:1563-1591` |
| `receive.denyCurrentBranch` | unset (= refuse) | `ignore`, `warn`, `refuse`, `updateInstead`; applies to any worktree that has the ref checked out (`find_shared_symref`), non-bare only | `builtin/receive-pack.c:188`, `:119-134`, `:1487-1523` |
| `receive.denyDeleteCurrent` | unset (= refuse) | Same actions for deleting the current branch | `builtin/receive-pack.c:193`, `:1541-1561` |
| `receive.fsckObjects`, `transfer.fsckObjects` | off | Pass `--strict` to the pack processors; `receive.` overrides `transfer.` | `builtin/receive-pack.c:178-186`, `:2297-2306` |
| `receive.fsck.<msg-id>`, `receive.fsck.skipList` | none | Severity overrides and skip list for strict checking | `builtin/receive-pack.c:155-176` |
| `receive.unpackLimit`, `transfer.unpackLimit` | 100 | Below the limit the pack is exploded into loose objects (`unpack-objects`); at or above it is kept as a pack (`index-pack --stdin --keep --fix-thin`) | `object-file.c:1325-1337`, `:1369-1420` |
| `receive.maxInputSize` | 0 (unlimited) | Aborts oversized packs | `builtin/receive-pack.c:241`, `:2297-2306` |
| `receive.shallowUpdate` | off | Allows pushes that change the shallow boundary | `builtin/receive-pack.c:213`, `:2578-2593` |
| `receive.procReceiveRefs` | none | Prefix list (with `a`, `d`, `m`, `!` modifiers) routing commands to the `proc-receive` hook | `builtin/receive-pack.c:246`, `:373-444` |
| `receive.certNonceSeed`, `receive.certNonceSlop` | unset, 0 | Enables signed pushes; nonce issue and tolerated skew | `builtin/receive-pack.c:218-224`, `:625-740` |
| `receive.advertiseAtomic`, `receive.advertisePushOptions` | on, off | Capability advertisement | `builtin/receive-pack.c:226-234` |
| `receive.keepAlive` | 5 | Seconds between keepalive packets | `builtin/receive-pack.c:236`, `:481-542` |
| `receive.autogc` | on | Runs auto maintenance after the push | `builtin/receive-pack.c:208`, `:2609-2623` |
| `receive.updateServerInfo` | off | Runs `update_server_info` after the push | `builtin/receive-pack.c:203`, `:2624-2625` |
| `transfer.advertiseSID` | off | Advertises `session-id` | `builtin/receive-pack.c:253` |
| `repack.useDeltaBaseOffset` | on | Advertises `ofs-delta` | `builtin/receive-pack.c:198`, `:272` |
| `receive.hideRefs`, `transfer.hideRefs` | none | Hidden refs (see 2.1) | `builtin/receive-pack.c:140` |
| `core.hooksPath` | unset | Where traditional hooks are found | `hook.c:26-60` (path via `hooks/<name>`) |
| `hook.<name>.command`, `.event`, `.enabled`, `.parallel`; `hook.<event>.enabled`, `.jobs`; `hook.jobs` | none, serial | Config-defined hooks, run before the traditional hook, optionally parallel (see 2.5) | `hook.c:153-300`, `:535-550`, `:719-780`; `Documentation/config/hook.adoc:8-107` |
| `core.fsync`, `core.fsyncMethod`, `core.fsyncObjectFiles` (deprecated) | components: pack and derived metadata only; method: platform-dependent, `writeout-only` on macOS (no hardware flush), `batch` on Windows, `fsync` elsewhere | Which components are flushed, and how | `environment.c:50-51`, `:257-268`, `:504-540`; `write-or-die.h:20-60`; `git-compat-util.h:1044-1050`, `compat/mingw-posix.h:318` |
| `core.filesRefLockTimeout`, `core.packedRefsTimeout` | 100 ms, 1000 ms | Lock-wait timeouts for the files backend (loose refs, `packed-refs`) | `refs.c:992-1006`, `refs/packed-backend.c:1236-1250` |
| `core.logAllRefUpdates`, `core.preferSymlinkRefs` | repository-dependent | Reflog creation; symlink symrefs | `refs.c:1059`, `refs/files-backend.c:136-138` |
| `reftable.blockSize`, `.restartInterval`, `.indexObjects`, `.geometricFactor`, `.lockTimeout` | 4096, library, on, library, 100 ms | Reftable write options | `refs/reftable-backend.c:323-392` |
| `safe.directory` | unset | Receiving into a repository owned by another user fails unless allowed | `setup.c:1869-1873`, `:1884-1885` |
| `core.alternateRefsCommand`, `core.alternateRefsPrefixes` | unset | Which alternate refs are advertised as `.have` | `odb.c:391`, `:403` |
| `core.lockfilePid` | off | Writes a `<lock>~pid.lock` sibling holding the owner pid | `lockfile.c:105-135`, `lockfile.h:136` |
| `core.sharedRepository` | unset | Permissions on migrated directories and new tables | `tmp-objdir.c:244`, `refs/reftable-backend.c:372` |
| `pack.indexVersion`, `pack.threads`, `pack.writeReverseIndex`, `core.deltaBaseCacheLimit` | library defaults | Read by `index-pack` when the pack is kept | `builtin/index-pack.c:1670-1693` |
| `maintenance.auto`, `gc.auto`, `maintenance.autoDetach`, `gc.autoDetach` | on | Decide whether post-push auto maintenance runs and detaches | `run-command.c:1961-1975` |

### 2.3 Execution order for one push

1. Parse commands, certificate, push options (`builtin/receive-pack.c:2566-2576`).
2. Unless the push only deletes refs, begin an object transaction in receive mode and
   write the pack into it; a delete-only push creates no quarantine
   (`:2581-2591`).
3. Connectivity check of every new tip, with the quarantine environment; failures are
   marked `missing necessary objects` per command (`:2034-2061`, `:1773-1804`).
4. Reject updates to hidden refs (`:2063`, `:1824-1849`).
5. Mark commands matching `receive.procReceiveRefs` for `proc-receive` (`:2069-2079`).
6. Run `pre-receive`; any failing hook marks every unfinished command
   `pre-receive hook declined` (`:2081-2087`). The quarantine directory is removed at
   process exit, after `post-receive`, `post-update` and maintenance have run
   (`tmp-objdir.c:165-168`); finalization only deletes pack lock files
   (`object-file.c:1453-1464`).
7. If every command already failed, return without touching refs (`:2093-2096`).
8. Migrate the quarantine into the main object store: the objects become visible
   before any ref is written (`:2102-2108`).
9. Reject aliased updates (a symref and its target updated inconsistently in one push)
   (`:2110`, `:1685-1760`), then resolve `HEAD` (`:2112-2115`).
10. Run `proc-receive` for routed commands (`:2117-2123`).
11. Update refs: `update()` per command and one atomic transaction (`atomic`), or two
    batched transactions (deletions first, then everything else) that tolerate
    per-ref rejection (`:2125-2128`, `:1886-2013`).
12. Finish the object transaction, send the report, run `post-receive`, then
    `post-update` with successful ref names, then auto maintenance and optional
    server-info update (`:2597-2625`).

Corrections to the plan's wording follow from this order. The non-atomic path is not one
transaction per ref: it is up to two batched transactions with
`REF_TRANSACTION_ALLOW_FAILURE`, so the files backend can reject some updates and apply
others inside one batch (`:1924-1951`; `refs.c:1276-1300`). Only user-input rejection
classes (stale old value, name conflict) become per-ref `ng`; a generic error, including
I/O failure, a lock-infrastructure failure or a duplicate ref name in one push
(`refs.c:1290-1295`, `:2708-2709`), is never converted, so the commit fails the whole
batch and the `failure:` loop overwrites **every** command's status with
`failed to update refs`, including deletions already committed in the first phase
(`:1955-1960`). Git can therefore report applied deletions as failed, the same
reported-failed-but-applied class as the reftable case in section 3.3, and a client's
reconciliation must not trust `ng` after a generic failure. Objects reach their final
location before any ref changes, so a failed ref update leaves migrated objects behind,
and a hook that vetoes at `update` time cannot restore the quarantine.

### 2.4 Per-command policy in `update()`

In order (`builtin/receive-pack.c:1478-1656`): refuse names outside `refs/` or failing
refname validation (`funny refname`); `denyCurrentBranch` for each worktree with the
ref checked out; require the new object to exist (`bad pack`); `denyDeletes`;
`denyDeleteCurrent`; `denyNonFastForwards`; run the `update` hook; for `updateInstead`
update the worktree *before* the ref transaction commits; queue the ref update or delete
(with the expected old id, so a stale `old` fails the lease). Between the `update` hook and
queueing, a shallow-boundary change runs `update_shallow_ref` (`shallow error`;
`:1629-1633`). Deleting a ref whose old object cannot be parsed sets the expected old id
to none, so that delete is queued **without a lease check**; deleting a ref that does not
exist sets `did_not_exist`, which drops it from `post-receive` and `post-update`
(`:1604-1623`, `:929`, `:1671`).

Persistence consequence of `updateInstead`: the worktree update, including its
`read-tree -u -m` index and checkout changes, happens in `update()` (`:1598-1602`) and the
ref commit later (`:1942`, `:1998`). A failed commit, or a later failing command in an
atomic push, therefore leaves an updated worktree and index with an unmoved ref. This is a
state the fork must reproduce or explicitly refuse.

Reflogs (named explicitly in the plan's receiving list): every update and delete is
recorded with the message `push` (`:1617-1638`); the identity is `git_committer_info(0)`
with no strictness, so it comes from the server-side environment and configuration, never
from the pusher (`refs/reftable-backend.c:1473`, and its files-backend counterpart).
Autocreation follows `core.logAllRefUpdates`: unset means off in a bare repository, and
`git init` sets it on in a non-bare one (`setup.c:2634-2636`); `normal` logs only
`refs/heads/`, `refs/remotes/`, `refs/notes/` and `HEAD`, `always` logs everything, and an
existing reflog is appended to regardless (`refs.c:1062-1076`;
`refs/reftable-backend.c:1443-1461`). In the files backend, updating the branch `HEAD`
points at also writes `HEAD`'s reflog (`refs/files-backend.c:2499`, `:2709`); the reftable
equivalent is a section 3 item.

### 2.5 Hooks

| Hook | When | Arguments | Stdin | Environment | Failure effect | Cite |
| --- | --- | --- | --- | --- | --- | --- |
| `pre-receive` | After connectivity check, before migration | none | `<old> <new> <ref>` per command | Quarantine variables (`GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_QUARANTINE_PATH`) when a quarantine exists, `GIT_PUSH_OPTION_<n>`, `GIT_PUSH_OPTION_COUNT`, `GIT_PUSH_CERT*` | Veto: all unfinished commands fail | `builtin/receive-pack.c:907-962`, `:2081-2087` |
| `proc-receive` | After migration, before ref updates, for routed commands | none; long-lived bidirectional process (`version=1` negotiation, then commands, optional push options, then a per-command report) | Packet-line protocol | No quarantine; only the traditional hook file is looked up (`find_hook`), not config-defined hooks | Spawn failure or protocol error fails every routed command, and every command in atomic mode | `builtin/receive-pack.c:1133-1309`, `:1145` |
| `update` | Per command inside `update()` | `<ref> <old> <new>` | none | Quarantine already migrated, so none | Veto: that command fails (`hook declined`); in atomic mode the whole push fails | `builtin/receive-pack.c:964-989`, `:1592-1596` |
| `push-to-checkout` | `updateInstead`, inside `update()` | `<new-oid>` | none | `GIT_DIR` (absolute git directory of *that* worktree; linked worktrees use `.git/worktrees/<id>`) and `GIT_WORK_TREE` (absolute); cwd stays the git directory; always serial | Veto fails the command (`push-to-checkout hook declined`). If no such hook exists, Git runs `update-index`, `diff-files`, `diff-index` and `read-tree -u -m` as child Git processes and refuses with `Up-to-date check failed`, `Working directory has unstaged changes`, `Working directory has staged changes` or `Could not update working tree to new HEAD`; a bare target yields `denyCurrentBranch = updateInstead needs a worktree` | `builtin/receive-pack.c:1369-1476` |
| `reference-transaction` | Four states around every ref transaction, in both backends: `preparing` (queued, nothing locked), `prepared`, `committed`, `aborted` | state name | Per-update lines | Quarantine disables ref updates (`ref updates forbidden inside quarantine environment`) | A veto at `preparing` or `prepared` aborts and calls `die()`, killing receive-pack before it reports; `committed` is skipped for initial transactions and when the backend's finish step fails. Update lines skip log-only updates and print symref targets as `ref:<target>`; the transaction sees the namespaced name, not the wire name | `refs.c:2617-2634`, `:2663-2786`, `:2702-2706`, `setup.c:1069`; `Documentation/githooks.adoc:486-519`; `builtin/receive-pack.c:1617-1638` |
| `post-receive` | After the report | none | `<old> <new> <ref>`, only successful commands | Push options and certificate variables | Cannot change the reported status | `builtin/receive-pack.c:2604-2605`, `:929` |
| `post-update` | After `post-receive` | Names of successfully updated refs | none | none | Cannot change the reported status | `builtin/receive-pack.c:1658-1683`, `:2606` |

Backend dependence of the `reference-transaction` hook: the files backend runs a nested
transaction on the `packed-refs` store through the generic API, which always runs the
hook (`refs/files-backend.c:3093`, `:3116`, `:3296`, `:3421`; `refs.c:2713-2785`; no
skip-hook flag exists at the pin). A deletion therefore emits extra hook invocations: an
`aborted` with zero ids when `packed-refs` needs no rewrite, or a full inner
`preparing`/`prepared`/`committed` sequence when it does. Git pins this as files-only
expected output (`t/t1416-ref-transaction-hooks.sh:186-203`). When the files backend
fails its own prepare step it cleans up and closes the outer transaction without calling
the generic abort (`refs/files-backend.c:2911-2945`, `:3127-3128`), so no outer `aborted`
event is emitted for lock conflicts or stale old values; the reftable equivalent is
unchecked (`refs/reftable-backend.c:1401-1424`). The fork must either reproduce these
call sequences or name the difference as a deliberate divergence; a probe (lock conflict
with a logging hook, per backend) would settle the unchecked cases.

Cross-cutting hook behavior observed in `hook.c`: configured hooks run first, then the
traditional hook, and every hook runs; the combined status is the bitwise OR of exit codes
and a failure to start counts as failure (`:535-550`, `:636-660`). With more than one job,
output is merged into stderr (`:677-681`). Hook stderr is relayed over sideband while the
client is connected (`builtin/receive-pack.c:549-584`). Push hooks always run with the **git directory** as working
directory, even in a non-bare repository (`Documentation/githooks.adoc:24-28`): no `dir`
is set for receive hooks (`hook.c:609`) and `enter_repo` chdirs there (`setup.c:1879-1889`),
and every hook inherits `GIT_DIR` (`setup.c:1102`).

### 2.6 Quarantine and object ingestion

- Created eagerly under the object directory as `tmp_objdir-incoming-XXXXXX` when the
  push has at least one non-delete command (`object-file.c:1481-1517`,
  `tmp-objdir.c:140-182`).
- The quarantine environment (`GIT_OBJECT_DIRECTORY` pointing at the temporary directory,
  the real object directory appended to `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
  `GIT_QUARANTINE_PATH`) reaches the connectivity check, `pre-receive` and the pack
  processors (`tmp-objdir.c:176-180`; `builtin/receive-pack.c:945`, `:2050`).
- Migration order: an unconditional flush of a scratch file in the quarantine directory as a durability barrier (not
  gated by `core.fsync` components; `object-file.c:1259-1294`, `write-or-die.c:58-76`),
  then per-file rename or link: loose objects, then packs in the order `.keep`, `.pack`,
  `.rev`, `.idx` (`tmp-objdir.c:192-205`, `:256-312`). Migration is not atomic; a crash
  midway leaves some objects in each place. Stale quarantine directories are pruned later
  (`tmp-objdir.c:149-151`, `:167`).
- `index-pack --keep` creates `.keep` files released at finalization, which happens before
  the report (`object-file.c:1453-1464`; `builtin/receive-pack.c:2597`).

### 2.7 After the push

Order is report, `post-receive`, `post-update`, auto maintenance, optional
`update_server_info` (`builtin/receive-pack.c:2597-2625`). Auto maintenance honors the
`maintenance.*`/`gc.*` keys listed above; it is a separate process in Git. For a Git-free
local receiver this step becomes native maintenance, which the plan prices separately.

### 2.8 Coverage gaps in this matrix

Stated so they are not mistaken for completeness:

- Internals of `fsck` message-type handling, `check_connected` and the shallow-update
  computation are named but not enumerated beyond their call sites.
- Signature verification of push certificates delegates to the GPG layer; its
  configuration is not enumerated.
- No row has been exercised against a running Git; each is a differential-test candidate.

## 3. Git reference-backend behavior

Scope: what Git itself does at the pin when a ref transaction fails, persists, is read
concurrently, or races an external writer, per backend. It is written to be turned into
differential tests; every row is a source observation, none was run.

### 3.1 Contract shared by both backends

- A transaction moves through `OPEN` → `PREPARED` → `CLOSED`. `prepare` runs the
  `reference-transaction` hook at `preparing`, then the backend's prepare, then `prepared`
  (`refs.c:2681-2730`). `commit` runs the backend's finish and then the `committed` hook
  only on success (`refs.c:2759-2787`). Duplicate ref names in one transaction are rejected
  (`refs.c:2709`, via `ref_update_reject_duplicates`).
- Ref updates are refused while `GIT_QUARANTINE_PATH` is set (`refs.c:2702-2706`).
- `REF_TRANSACTION_ALLOW_FAILURE` lets a backend reject individual updates (not generic
  I/O errors) at prepare time and apply the rest (`refs.c:1276-1300`; used by non-atomic
  receive-pack, `builtin/receive-pack.c:1924-1951`). Without the flag a single rejected
  update fails the whole prepare.
- An expected old value is verified against the state read *under the backend's lock*:
  existing value, expected absence (null id) and expected symref target are distinguished,
  with distinct error classes `CREATE_EXISTS`, `NONEXISTENT_REF` and
  `INCORRECT_OLD_VALUE` (`refs/files-backend.c:2615-2666`). Git documents that on a
  complete lock set with matching old values all modifications are performed, otherwise
  none, but that a concurrent reader may see a subset
  (`Documentation/git-update-ref.adoc:158-163`). Git's own list of files-backend
  disadvantages says the same (`Documentation/BreakingChanges.adoc:146-149`).

### 3.2 Files backend

**Locking.** One `<ref>.lock` per updated ref, taken while preparing, with a wait of
`core.filesRefLockTimeout` (100 ms default; `refs/files-backend.c:846-848`, `refs.c:992-1006`).
`packed-refs.lock` is taken only if the transaction deletes refs, with
`core.packedRefsTimeout` (1000 ms default; `refs/files-backend.c:3084-3090`,
`refs/packed-backend.c:1236-1250`). No repository-wide lock exists
(`refs/files-backend.c:3063-3076`). A symref update is split into a log-only update of the
symref plus an update of the referent; an update of the branch `HEAD` points at also gets
a `HEAD` reflog entry, guarded by a "HEAD has been racily updated" check that Git itself
says is incomplete (`:2499-2613`, `:2735-2757`). Lock-file cleanup runs from `atexit` and
signal handlers (`tempfile.c:66-118`); a killed process (for example `SIGKILL`) therefore
leaves stale `*.lock` files, optionally with a `~pid.lock` sibling under
`core.lockfilePid` (`lockfile.c:105-135`).

**Prepare, all or nothing.** Prepare locks every ref, verifies old values under the lock,
and writes each new value into its lock file (`refs/files-backend.c:3020-3061`,
`:2857-2887`). Any failure releases all locks and changes no ref (`:3123-3132`,
`:2911-2947`), except per-ref rejections under `ALLOW_FAILURE`. An update whose new value
equals the current value writes nothing and gets no entry in the ref's own reflog (`:2857-2866`). When the ref is the branch `HEAD` points at, the `HEAD` reflog still gets an entry (ran, see [measured behaviour](push-durability-evidence.md)).

**Commit order and partial application.** Finish performs, in this order
(`refs/files-backend.c:3321-3475`):

1. For every non-rejected update: append the reflog entry, then rename the lock file over
   the ref (`:3347-3387`).
2. For deletions: delete the reflog first (a ref without a log is "less bad" than a log
   without a ref, per the code comment), ignoring unlink failures (`:3389-3412`).
3. Commit the `packed-refs` transaction that removes the deleted refs, while still
   holding its lock (`:3414-3427`).
4. Unlink the loose files of deleted refs, then remove empty parent directories
   (`:3429-3471`).

An error at any step jumps to cleanup, so earlier steps stay applied; no rollback is
attempted (`:3455-3456`). The gitoxide analogue documents the same property
(`gix-ref/src/store/file/transaction/commit.rs:17`). Reachable partial states:
some updates renamed and others not; a reflog entry recording a move the ref never made,
because step 1 appends the reflog before the rename and a failed rename jumps to cleanup
(`:3355-3381`; this includes the log-only `HEAD` entry); updates applied with deletions
not applied; a packed entry deleted while the loose file remains (the deletion is then
not visible: the loose file holds the pre-transaction value and takes precedence over any
packed entry, and the reflog is already gone); stale lock files after a kill.

**Name-conflict race.** The directory/file conflict check against `packed-refs` runs
before `packed-refs` is locked, and Git states that it accepts the race
(`refs/files-backend.c:3063-3080`), so a concurrent `packed-refs` writer can create a
conflict that goes undetected.

**Worktrees.** Per-worktree refs (`HEAD`, `refs/bisect/*`, `refs/worktree/*`,
`refs/rewritten/*`) are routed to the worktree's git directory, `worktrees/<id>/...` to
`<commondir>/worktrees/<id>/`, and shared refs to the common directory
(`refs/files-backend.c:266-290`). A transaction spanning directories is still per-ref
lock and rename, so it adds no new partial-state class. Both backends consult only the
`HEAD` of the ref store's own git directory when deciding to write a `HEAD` reflog entry
(`refs/files-backend.c:3004-3010`, `refs/reftable-backend.c:1104-1128`); a push to a
branch checked out in a *linked* worktree is therefore inferred, not checked, to write no
entry to that worktree's `HEAD` reflog.

**Persistence.** The new loose value is flushed before the lock file closes only if
`core.fsync` includes the `reference` component, which the default does not
(`refs/files-backend.c:2058-2073`, `write-or-die.h:34-39`). `packed-refs` is flushed the
same way (`refs/packed-backend.c:1570`). Reflog appends are `O_APPEND` writes with no flush
(`refs/files-backend.c:1905-2056`). No directory flush follows any rename: a search for
`fsync` in `lockfile.c`, `tempfile.c` and `refs/files-backend.c` finds exactly one call, the
component-gated loose-ref flush at `refs/files-backend.c:2069`; the `packed-refs` call is in
`refs/packed-backend.c:1570` (`rg -n fsync lockfile.c tempfile.c refs/files-backend.c`, ran
at the pin). In the fork, `rg` finds no `fsync` or `sync_all` in `gix-ref`, `gix-lock` or
`gix-tempfile` at `b592770fc` (ran by the reviewer).

**Reader view and snapshots.** A loose ref read is one file read with a retry loop for
file/directory/symlink races (`refs/files-backend.c:537-560`). Iteration reads all loose
refs of the subtree before taking or refreshing the `packed-refs` snapshot, so a
concurrent move of a loose ref into `packed-refs` is never missed (`:1080-1094`). The
`packed-refs` file is read as a reference-counted snapshot (memory-mapped where the
platform allows) that is revalidated against file status (`refs/packed-backend.c:22-120`,
`:174-190`). There is no snapshot across loose refs: a reader racing a multi-ref commit
can see some refs updated and others not. A single iteration is not a snapshot either:
loose refs are primed into a cache before `packed-refs` is checked, so a loose ref changed
mid-iteration is stale, but consistently so for that ref.

**External writers.** Fork and Git writers interoperate only if they use the same lock
file names and exclusive-create semantics, the same directory/file conflict handling
(`raceproof_create_file`, `try_remove_empty_parents`; `refs/files-backend.c:1128-1200`,
`:2925`), the same `packed-refs` protocol (lock, rewrite via tempfile, rename), and tolerate
`~pid.lock` files.

### 3.3 Reftable backend

**Storage.** Tables live in `$GIT_DIR/reftable/`, ordered by `tables.list`. Every
transaction is one new immutable table (refs and log records, one `update_index`) appended
to the list (`Documentation/technical/reftable.adoc:884-960`). A repository selects the
format with `extensions.refStorage = reftable` and `repositoryformatversion = 1`, and keeps
dummy files (`HEAD` containing `ref: refs/heads/.invalid`, `refs/`, a regular file
`refs/heads`) so older clients still recognize it (`:858-912`). SHA-256 repositories
use table version 2 (`:235-252`). Linked worktrees have their own stack
(`<commondir>/worktrees/<name>/reftable`); per-worktree refs (`HEAD`, `refs/bisect/*`
and similar) go to that stack and everything else to the main stack
(`refs/reftable-backend.c:185-317`).

**Publication.** The single writer lock is `tables.list.lock`, waited on for
`reftable.lockTimeout` (100 ms default; `refs/reftable-backend.c:347-353`, `:375`). A
writer holding it reloads a stale in-memory view instead of failing, so old values are
verified against the locked state (`reftable/stack.c:677-718`). It writes the new table
and renames it to its final name, writes the new list to `tables.list.lock`, flushes that
file, and renames it over `tables.list` (`reftable/stack.c:775-813`, `:952`); that rename
is the publication point. The flush is gated like refs: the library's `fsync` is redefined
to `fsync_component(REFERENCE)` (`reftable/reftable-system.h:16`, `reftable/system.c:137-139`),
so the default `core.fsync` skips it. No directory flush follows.

**Atomicity.** Within one stack a transaction is atomic against kill and against
readers: it either appears in `tables.list` or not. A transaction touching more than one
stack (main plus a worktree stack) commits one stack at a time
(`refs/reftable-backend.c:1673-1684`), so a failure on the second stack leaves the first
published. Validation failures reject the whole transaction before publication
(`refs/reftable-backend.c:1340-1415`), except per-ref rejections under `ALLOW_FAILURE`.

**Reported failure after publication.** After the rename, commit reloads the stack and
runs auto-compaction (`reftable/stack.c:823-843`). A reload error or a compaction error
other than lock contention or an outdated stack returns failure although the refs are
already published, so the backend reports `transaction failure`
(`refs/reftable-backend.c:1681-1695`) and the `committed` hook is skipped
(`refs.c:2784`). Git itself can therefore report a committed reftable transaction as
failed; the fork must treat that class as uncertain, never as rolled back.

**Readers.** A reader opens `tables.list`, opens every table it lists, and restarts if a
table is missing (`reftable.adoc:915-925`). The implementation
(`reftable_stack_reload_maybe_reuse`, `reftable/stack.c:369-440`) retries only when a
listed table is missing **and** `tables.list` changed between the read and a re-read
(`:410-426`); with growing random sleeps, and the 3-second deadline is consulted only after
three attempts (`:384-386`, `:433-434`). If the list is unchanged and a table is
missing, the load fails immediately with a not-exist error: corruption, not a race.
Up-to-date detection compares the device and inode of `tables.list`, which assumes writers
always replace the file by rename (`reftable/stack.c:576-604`). The fork must never rewrite
`tables.list` in place.

Per-operation snapshot behavior (`refs/reftable-backend.c`): single-ref and symref reads
reload the stack on every call (`:895`, `:921`, reload at `:292-293`); a ref iterator
reloads once at start and then iterates over the tables it opened, a consistent snapshot
for that iteration (`:828`); reflog iteration does **not** reload, which Git marks as a TODO
(`:2209-2212`, `:2260-2263`), so reflog reads can be stale relative to refs; and prepare
reads the `HEAD` stack without reload or lock when the transaction does not touch it, which
Git calls "dubious" (`:1357-1367`). That an open iterator survives a concurrent
compaction's unlink on POSIX, and fails to delete on Windows leaving it to the cleanup pass
(`reftable/stack.c:1787-1863`), is inferred, not checked.

**Compaction.** Auto-compaction runs after every commit unless disabled, keeps table sizes
in a geometric sequence (factor `reftable.geometricFactor`) and ignores lock or
outdated-stack contention (`reftable/stack.c:828-843`, `:1550-1650`). The explicit maintenance entry point is
`refs/reftable-backend.c:1699-1730`: `auto` runs the same segment logic, otherwise it
compacts everything, then cleans obsolete tables, which is the hook for native maintenance.
The compactor takes `tables.list.lock`, locks each table in its range with a zero-wait
lock, **releases** `tables.list.lock` while it merges, then re-acquires it, verifies the
range is unchanged and publishes the result (`reftable/stack.c:1166-1520`;
`reftable.adoc:960-1000`). Obsolete tables are deleted afterwards or by a cleanup pass
(`stack.c:1818-1863`). A fork compactor that held `tables.list.lock` throughout would stay
safe but could starve Git writers past their 100 ms wait.

**Reflogs and deletions.** Refs and log records of one transaction are written into the same
table and published by one rename, so a ref and its reflog entry are atomic together,
unlike the files backend (`refs/reftable-backend.c:1463-1660`). Log records carry the update
index of their transaction. Deleting a ref writes a deletion record plus a log tombstone
for every existing log entry of that ref, in the same table, "to match the files backend"
(`:1505-1545`); reflog expiry writes a new table of tombstones and existence dummies
through a normal addition, followed by auto-compaction (`:2575-2735`); tombstones are
dropped only by a compaction that includes the bottom of the stack (`reftable.adoc:958-1000`).
Updating the branch `HEAD` points at enqueues a log-only `HEAD` update; if `HEAD` itself is
also in the transaction it fails as a name conflict (`refs/reftable-backend.c:1104-1128`).
Whether a reflog is created follows `core.logAllRefUpdates` through the same rule in both
backends: unset means off in bare and on in non-bare repositories
(`refs/reftable-backend.c:1443-1461`, `refs.c:1062-1076`).

### 3.4 Durability comparison

| Component | Files backend | Reftable backend | Flushed by default? |
| --- | --- | --- | --- |
| New ref value | Lock file contents, before rename | New table file, before it is listed | No; `reference` is outside the default `core.fsync` set (`write-or-die.h:34-39`) |
| Ref publication | Rename of lock file; no directory flush | Rename of `tables.list.lock`; no directory flush | No |
| Reflog | Append, never flushed | Part of the table; flushed only as above | No |
| `packed-refs` / list file | Tempfile flushed if `reference` enabled, then renamed | `tables.list.lock` flushed if `reference` enabled, then renamed | No |
| Objects from a push | Barrier flush of a scratch file in the quarantine before migration, unconditional (`object-file.c:1259-1294`); packs and derived metadata flushed by default; the default component set excludes loose objects, but pushes under the unpack limit run `unpack-objects`, which starts its own object transaction inside the quarantine (`builtin/unpack-objects.c:605`); with the defaults, no per-object flush happens, only the barrier (ran: [measured behaviour](push-durability-evidence.md)) | Same | Barrier yes; loose objects: no, except with the `loose-object` component or `core.fsyncObjectFiles`; batching needs `loose-object` as well |

The plan's statement that objects are flushed "no earlier or later" than `core.fsync`
requires does not hold for receive-pack: the migration barrier ignores the selected components (it still follows `core.fsyncMethod`), and a
fork receiver must match it.

### 3.5 What differential tests must pin per backend

| Failure class | Files backend expectation | Reftable expectation |
| --- | --- | --- |
| Stale lease, expected absence, lock conflict, validation failure | No ref changes, locks released; outcome known | No table published; outcome known |
| Failure while writing lock files or table | Nothing visible; known | Nothing visible; known |
| I/O failure between per-ref renames | Some refs updated, some not; a reflog entry may exist for a move the ref never made; uncertain | Not applicable within one stack |
| Listed table missing while `tables.list` is unchanged | Not applicable | Hard error (corruption), not retried |
| Reflog write failure | Aborts the commit after earlier refs were renamed | Not applicable (same table) |
| I/O failure after the first stack of a multi-stack transaction | Not applicable | First stack published, second not; uncertain |
| Failure after publication (reload or compaction) | Not applicable | Refs published but failure reported; uncertain |
| Deletion interrupted | Reflog gone, packed entry gone, loose file possibly present | Tombstone published atomically |
| Process kill | Stale `*.lock` files; partial multi-ref state | Possibly an orphan table file, cleaned later; no partial state |
| Power loss | Depends on `core.fsync` and filesystem; refs and logs not flushed by default | Same dependence; unproven here |
| Reader during commit | May see a subset of a multi-ref transaction | Sees all or none of a single-stack transaction |
| `reference-transaction` hook | Extra nested `packed-refs` events on deletion; no outer `aborted` on prepare failure | Single transaction; prepare-failure events unchecked |

Process kill and power loss remain separate models, as the plan states. No row of this
table has been exercised; the cheapest discriminating probes are proposed in section 7.

## 4. Fork state at `b592770fc`

Source audit of what the fork already has against sections 2 and 3. Nothing here was
built or run; the counts below come from `rg`/`wc` searches over the tree at that commit
and are labelled where they are searches.

### 4.1 Reference storage

| Area | Observation | Gap against Git at the pin |
| --- | --- | --- |
| Store type | `gix::RefStore` is a type alias for `gix_ref::file::Store` (`gix/src/lib.rs:194`) and `Repository.refs` is a **public field** of that concrete type (`gix/src/types.rs:161`, `:202`). The concrete file store has 53 `pub fn` items (search: `rg -c 'pub fn' gix-ref/src/store/file`) | Any second backend changes a public type or adds a parallel one |
| Coupling | About 80 references to `file::Store` or `.refs` across 19 files in `gix/src` (search: `rg -c 'file::Store\|\.refs\b' gix/src`), plus `gix-protocol/src/fetch/{negotiate,types}.rs`, `gix-discover/src/is.rs`, `gitoxide-core/src/remote.rs` and test tooling | Mechanical but wide |
| Backend-neutral surface | `RefEdit`, `Change`, `PreviousValue`, `LogChange` live in `gix_ref::transaction` and `Repository::edit_references` already takes them (`gix/src/repository/reference.rs:127-150`) | Callers of the porcelain edit API are insulated from the store type |
| Generic store scaffolding | `gix-ref/src/lib.rs:100-131` declares a `Handle`/`State`/`Store` whose only variant wraps the file store, marked `dead_code` "for planned ref-table support" | Starting point exists; no second variant |
| Reftable on open | A reftable clone opens, and reading `HEAD` fails with "This reference uses an unsupported storage backend, such as reftable" (`gix/tests/gix/repository/open.rs:137-149`, `gix-ref/src/store/file/loose/reference/decode.rs:26`) | No reftable read or write |
| Unsafe-write hazard today | `gix` opens a repository without reading `extensions.refStorage` (search: `rg -i refstorage gix/src` finds only a documentation comment at `gix/src/repository/mod.rs:46`; version-0 rules ignore unknown extension keys, `:32`), so it instantiates the **files** store on a reftable repository. The only guard is decoding the stub `HEAD`, which names `refs/heads/.invalid` and fails as "unsupported storage backend" (`gix-ref/src/store/file/loose/reference/decode.rs:22-27`). Any other lookup under a stub file is treated as absent (`gix-ref/src/store/file/find.rs:246`, `NotADirectory` returns `Ok(None)`), so a reference that exists in the reftable reads as missing without an error. Inferred, not run: `refs/heads` is a stub regular file in such a repository, so branch writes probably fail, but a write such as `refs/tags/x` would create a loose file that Git never sees. Upstream `main` (`689fe4578042fafe9ded904351eb08fb5fffcf04`, 2026-11-03) has the same code: `git grep` for `refstorage` finds only the documentation comment and a test list, and the same decode guard (ran) | Git refuses unknown extensions in a version-1 repository and treats `refStorage` as known; the fork must refuse writes (or the open) whenever the storage format is not `files`, before any receiver or consumer work builds on the store. Probe P12 |
| New-repository format inputs | `gix init` reads none of Git's format inputs (search: `rg -i 'defaultRefFormat\|DEFAULT_REF_FORMAT\|feature.experimental' gix/src gix-discover/src` finds nothing), so it creates a files repository where Git would create reftable (6.1) | Silent fallback to `files` must become an explicit refusal when configuration asks for reftable |
| Clone ordering | The clone path notes that a reftable store must not be initialized before the remote hash is known (`gix/src/clone/fetch/util.rs:30-41`) | Init order constraint for SHA-256 reftable clones |
| Upstream port | See 4.2 | Codecs only; stack layer would need rewriting |

**Probe P12 result (ran 2026-11-04, fork `dev` at `fb71d5805`, Git 2.56.0).** Opening the reftable fixture
of `gix/tests/fixtures/make_reftable_repo.sh` with `gix` and using its public API confirmed the inference
above, and found that reads were wrong as well:

- `Repository::try_find_reference()` returned `Ok(None)` for `refs/heads/main` and `refs/remotes/origin/main`,
  which `git for-each-ref` lists. `references().all()` yielded a single item.
- `Repository::reference("refs/tags/…")` succeeded and wrote a loose file that `git for-each-ref` and `git fsck`
  don't see. A write below `refs/heads/` failed, as `refs/heads` is a stub file there, and a write below
  `refs/notes/` failed after leaving a reflog file behind.
- Independently, a loose `refs/tags/ghost` written into a reftable repository is invisible to `git show-ref` and
  `git rev-parse` (ran by the thread's reviewer).

The interim refusal of decision 7, option (c), closes both cases: see the guard in `gix_ref::file::Store`
(`set_unsupported_backend()`), set by `gix` when opening a repository.

### 4.2 The unmerged upstream `gix-reftable` port

A single library commit `94793bb6fb932212ce763dc5e531051911f86386` on the upstream branch
`codex/reftable-port-sequence` (branch tip `7335b332e9ba4a840d37338d5eaf4f0cbe535748`, which
only adds a planning file; neither commit is merged into upstream `main` at the time of
reading, `git merge-base --is-ancestor` ran and reported not merged) adds a standalone
`gix-reftable` crate. Measured with `git show` and `wc -l` (ran): about 3.4k lines of
library code (3,359) and 0.6k lines of tests (642). The branch touches nothing in `gix-ref`
or `gix` (the file list of the commit shows only `gix-reftable/`, the workspace manifest
and the lockfile), so no backend integration exists. The crate depends on `crc32fast`,
`flate2`, `gix-hash` (both hashes) and `thiserror`, so it does not use the `gix-error`
convention that `AGENTS.md` prefers for plumbing crates being migrated. It contains
record, block, table, merged-iterator, writer and stack modules, and tests ported from
Git's `u-reftable-*` and `t061x` suites.

What the stack layer at that commit does not do (read in `gix-reftable/src/stack.rs`,
`rg` for `sync_all|sync_data|create_new|OpenOptions` finds no match):

- No exclusive lock: `write_tables_list` writes `tables.list.lock` with `fs::write`, which
  truncates and creates without exclusive-create semantics, and renames it
  (`stack.rs:172-183`). It would overwrite a lock a Git writer holds, and two writers can
  both succeed with the later one silently dropping the earlier one's table.
- No stale-view check, no reload-on-missing-table retry, no per-table locks.
- No flush of tables or of `tables.list`, so `core.fsync` cannot be honored.
- Compaction rewrites everything into one table once the table count reaches the
  factor, then deletes the old tables immediately; it is not Git's geometric
  segment compaction and does not release `tables.list.lock` while merging.

It is therefore a candidate source for the format codecs (the hardest part to get
byte-compatible) and not for the coordination protocol, which is where the plan's
Git-equivalence requirement lives. Upstream's September 2026 report says no reftable
work had started yet and that contributors are expected to join (`etc/reports/26-09.md`,
section "Git3 support: reftable"); the fork does not control that timeline. The fork's `crate-status.md` lists reftable under Git 3.0 compatibility for
`gix-hash`, `gix-ref`, `gix-reftable`, `gix-protocol` and `gix-odb` (`crate-status.md:56`,
`:1050-1057`), and Git's own breaking-changes list makes support in Gitoxide a
prerequisite for reftable becoming Git's default (`Documentation/BreakingChanges.adoc:164-166`).

### 4.3 Files-store transaction against Git

| Property | Fork | Git | Cite |
| --- | --- | --- | --- |
| Lock all, verify old values under lock, then change | Yes, per ref: lock, re-read loose then packed under the lock, check expected value | Same shape | `gix-ref/src/store/file/transaction/prepare.rs:40-190`; `refs/files-backend.c:2687-2905` |
| Lock timeouts | `core.filesRefLockTimeout` 100 ms, `core.packedRefsTimeout` 1000 ms | Same defaults | `gix/src/config/cache/access.rs:227-241`; `refs.c:992-1006`, `refs/packed-backend.c:1236-1250` |
| Commit order | Reflog then rename per update; reflog deletion; packed-refs commit; loose deletion | Same order | `gix-ref/src/store/file/transaction/commit.rs:60-176`; `refs/files-backend.c:3321-3475` |
| Rollback on failure | None; documented | None | `commit.rs:17`; `refs/files-backend.c:3455-3456` |
| Failed reflog deletion | Aborts the commit with an error | Ignored (`unlink_or_warn`) | `commit.rs:121-137`; `refs/files-backend.c:3403-3412` |
| Missing committer when a reflog is needed | Aborts late, after some refs committed | Falls back to the environment identity | `commit.rs:11-15`; `refs/files-backend.c:1988-1994`, `refs/reftable-backend.c:1473` (Git) |
| No-op update | Skips the reflog | Skips write and reflog | `commit.rs:81-96`; `refs/files-backend.c:2857-2866` |
| `fsync` of refs, reflogs, `packed-refs`, objects, packs | None anywhere in non-test source (search: `rg 'sync_all\|sync_data\|fsync'` outside tests finds only a worktree-checkout comment, an example, and a "Planned" entry for `core.fsync` in `src/plumbing/progress.rs:203-208`) | Component-gated | see 3.4 |
| Stale lock cleanup | Signal-handler cleanup exists in `gix-tempfile` but is opt-in (`setup(mode)`, default `Mode::None`) | Always installed | `gix-tempfile/src/signal.rs:12-14`; `tempfile.c:66-118` |
| `core.lockfilePid` sibling files | Not supported (search finds no hit) | Supported | `lockfile.c:105-135` |
| `reference-transaction` hook | None; client and receive hooks unimplemented | Four states, backend-dependent calls | `crate-status.md:170-175`; section 2.5 |
| Linked-worktree stores | `RefStore::for_linked_worktree_opts` | Per-worktree stack or directory | `gix/src/repository/location.rs:58` |

The first three rows mean the fork's files transaction already has the right shape;
the differences that a differential test would expose are the reflog-deletion and
missing-committer behaviors, the absent flushes, the optional cleanup handlers, the
absent hook states and the absent `~pid.lock` handling.

### 4.4 Receiving building blocks

| Need | Fork state | Cite |
| --- | --- | --- |
| Advertisement, command parsing, report writing | No server-side protocol code. `gix-protocol` holds `command`, `fetch`, `handshake`, `ls_refs`, `remote_progress` (client side); `gix-packetline` provides line encoding and decoding | `ls gix-protocol/src`, `gix-packetline/src/encode.rs` |
| Local transport | Spawns `git upload-pack` or `git receive-pack` as a subprocess | `gix-transport/src/client/blocking_io/file.rs:41`, `:132`, `:291`, `:349` |
| Pack ingestion | `Bundle::write_to_directory` writes a pack and index, completing thin packs through a caller-supplied lookup; no flush | `gix-pack/src/bundle/write/mod.rs:55-107` |
| Quarantine | None. `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES` and `GIT_QUARANTINE_PATH` are not honored by the fork's object store; they are only stripped from child Git environments, and a TODO records quarantine support as missing | `gix-path/src/env/git/mod.rs:406-407`, `gix/src/open/repository.rs:142` |
| Connectivity and fsck | `gix-fsck` is a 107-line connectivity walker (`Connectivity::check_commit`); no strict object checks, no `receive.fsck.*` | `gix-fsck/src/lib.rs:1-107` |
| Unpack limit | Not implemented (`fetch.unpackLimit` is listed as planned) | `src/plumbing/progress.rs:253` |
| Hooks | None | `crate-status.md:170-175` |
| Credential helpers | Named helpers run as `git credential-<name>` through the Git executable | `gix-credentials/src/program/mod.rs:13-19`, `:95-103` |
| Maintenance | No object or ref maintenance engine found; promisor-aware maintenance is listed as unchecked (search of `crate-status.md`; not audited in source) | `crate-status.md:314`, `:351` |

Reading: a native receiver reuses pack writing, object lookup, connectivity walking and
the ref transaction machinery, and needs new server-side protocol, quarantine and
object-migration code, policy checks, strict fsck, the unpack-or-keep decision,
`updateInstead`, `proc-receive` and the hook-runner integration. The matrix in section 2
is the specification for that new code.

## 5. Candidate reference-storage and coordination designs

Both backends must follow Git's own coordination (lock files for the files store, the
`tables.list.lock` publication protocol for reftable), so that fork and Git writers
interoperate; that part is fixed by the plan. What is open is the shape of the types, the
source of the reftable implementation, and how hooks, durability and uncertain outcomes
surface in the API. Each choice below lists what it commits the public API to.

### 5.1 Where the backend choice lives

| Option | Description | Public API consequence | Assessment |
| --- | --- | --- | --- |
| A. Enum store | `gix_ref::Store` becomes `File(file::Store) \| Reftable(reftable::Store)`, completing the dead-code scaffolding in `gix-ref/src/lib.rs:100-131`; `gix::RefStore` aliases the enum; transactions and iteration platforms become backend-neutral types | Breaking: `Repository.refs` and `ThreadSafeRepository.refs` are public fields (`gix/src/types.rs:161`, `:202`) and `file::Store` has public fields that callers mutate (`write_reflog`, `namespace`, `prohibit_windows_device_names`; `gix-ref/src/store/file/mod.rs:24-29`). Anything outside `gix` that touches them breaks; the downstream advisory QA list in `gix/Cargo.toml` (`[package.metadata.qa]`, `:509-520`, includes large consumers) would measure it, and it is not checked here. Inside `gix`, about 80 call sites (4.1) | Static dispatch, no lifetime problems, matches the existing scaffolding |
| B. Trait-object core | An object-safe `dyn RefBackend` core taking `&PartialNameRef`/`&FullNameRef` and returning `gix_error::Result`, behind a concrete handle that keeps the generic convenience methods (today's `find` is generic over name and error types, `gix-ref/src/store/file/find.rs:448-455`) | `Repository.refs` type changes anyway | Feasible. Costs: dynamic dispatch, boxing of the borrowed platforms and iterators, and a rewrite of the public platform types. Gains: extensibility for in-memory stores in tests, alternate-location storage (6.1) and consumer-provided stores. A wins on cost and on matching the existing scaffolding, not on feasibility |
| C. Additive, parallel type | Keep `gix::RefStore = file::Store` and `Repository.refs` unchanged for files repositories; add a backend-neutral `Repository::ref_store()` handle and route new APIs through it; for a reftable repository the old field keeps today's "unsupported storage backend" behavior | Non-breaking first step; the old field is deprecated and migrated away in a later breaking release | Lowest churn, but leaves two access paths for one release train |

Recommendation: A, staged through C (introduce the neutral handle first without
breaking anything, then flip the alias in one breaking commit per the `AGENTS.md`
rule that breaking changes and their adaptations land together). Option B stays open if
alternate-location or consumer-provided stores become requirements; it is not needed
for the plan's list. **Precondition for every option:** option C's "old field keeps
today's behavior" is not safe as it stands (4.1, unsafe-write hazard); the fork must
first refuse writes, or the open, for any repository whose `extensions.refStorage` is
not `files`, until a reftable store exists.

### 5.2 Source of the reftable implementation

| Option | Description | Risks | Assessment |
| --- | --- | --- | --- |
| R1. Adopt the codecs, build the stack new | Take the upstream branch's record, block, table, merged-iterator and writer modules (4.2) after a parity review against Git's `reftable/` directory (7,113 lines of C plus 2,123 of headers at the pin) and its 9 unit-test files (4,832 lines); write the stack layer to Git's protocol | Dependence on an unmerged upstream branch; a later upstream `gix-reftable` could conflict with the fork's rebases; `thiserror` versus `gix-error` migration; no new third-party crates (`crc32fast`, `flate2`, `thiserror` are already in `Cargo.lock`) | Best cost, moderate coordination risk; codecs are the largest byte-compatibility surface |
| R2. Write from the specification | A fork-owned `gix-reftable` from Git's specification and C sources | Highest effort; the C library's edge cases must be rediscovered | Only if R1's parity review fails |
| R3. Wait for upstream | Track upstream's announced reftable work and consume it | Timing outside the fork's control; the plan says upstream acceptance is not a delivery dependency | Not viable as the primary route; worth checking again before the reftable thread starts |

Recommendation: R1, with the stack layer, locking, compaction, fsync and `ref-store`
adapter written new, and a one-session parity review of the codecs as the first gate.
Name the crate so that a later upstream landing can be reconciled: the fork should keep
the upstream crate name and public module layout where the review finds them sound.

### 5.3 Coordination design

- **Files store.** Keep the existing lock-file approach (it already matches Git's shape,
  4.3), and close the gaps found in 4.3: reflog-deletion failure, missing-committer
  handling, `~pid.lock` tolerance, default signal-handler cleanup in server contexts,
  nested `packed-refs` hook events, and an uncertain-outcome classification for commit
  failures after the first rename.
- **Reftable.** Implement Git's publication protocol exactly: exclusive `tables.list.lock`
  with the same wait (100 ms default, `reftable.lockTimeout`), reload under the lock,
  new table renamed into place, list written, flushed per `core.fsync`, renamed over
  `tables.list`; segment compaction with per-table locks and the lock released while
  merging; readers retry only on a missing table with a changed list; `tables.list` is
  never rewritten in place. One transaction per stack, committed stack by stack, with
  Git's documented non-atomicity across stacks.
- **Corruption and verification.** Reftable tables carry checksums and Git exposes fsck
  checks for both backends (`t/t0614-reftable-fsck.sh`, 102 lines); the fork needs an
  equivalent and a defined result for a damaged table, distinct from the retryable
  missing-table case (3.3). Tombstones follow section 3.3: a deletion record plus log
  tombstones, dropped only by a compaction that includes the bottom of the stack.
- **In-process layering (additive).** A consumer that already serializes writes can add an
  in-process queue in front of either backend; it does not replace the on-disk protocol,
  and Git's own documentation suggests exactly this for servers
  (`Documentation/technical/reftable.adoc:947-953`). The assessment recommends no
  library-level global lock, since it would diverge from Git's contention profile.
- **Compaction contention.** A fork compactor must release `tables.list.lock` while merging
  (3.3). Holding it throughout is safe but can starve Git writers past their 100 ms wait.

### 5.4 Public API implications beyond the store type

| Concern | Implication |
| --- | --- |
| Uncertain outcomes | `gix_error::Class` has no "outcome uncertain" class (`gix-error/src/error.rs:268-304`); it is `#[non_exhaustive]`, so adding one is non-breaking. Commit failures after a partial or published change need it, and so does the client's reconciliation rule. Reftable "failed after publication" and files "some refs renamed" are the two producers |
| Lock options | `Transaction::prepare` takes file-specific `Fail` modes for loose refs and `packed-refs` (`gix/src/repository/reference.rs:139-149`); a neutral options value is needed, with reftable's `lockTimeout` |
| Hooks | `reference-transaction` is raised in Git's generic layer, above the backends. The fork needs a transaction observer in `gix-ref` (plumbing must not spawn processes) that `gix` implements with the hook runner; the files backend must raise the nested `packed-refs` events (section 2.5). The observer must support Git's two veto points, `preparing` (before locks) and `prepared` (locks held); Git aborts and calls `die()` at either (`refs.c:2713-2727`), so the client of a receiving process sees no report. The fork must choose between returning an error after releasing locks and matching process death in the CLI and receiver; that choice is a documented divergence point. Exit codes of `committed` and `aborted` are ignored (`refs.c:2753`, `:2785`), which is the Git behavior behind the plan's rule that a post-commit hook failure is not a rolled-back commit |
| Quarantine | Honoring `GIT_QUARANTINE_PATH` means ref updates are refused (`refs.c:2702-2706`); the fork records this as a TODO needing `gix-sec` and `gix-ref` support (`gix/src/open/repository.rs:142`). In the opposite direction, a `gix`-based tool run as a hook inside *Git's* receive-pack cannot see quarantined objects and would not refuse ref updates, because the fork ignores `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES` and `GIT_QUARANTINE_PATH` (4.4); that is an interoperability item for consumers independent of a native receiver |
| Durability | `core.fsync` and `core.fsyncMethod` need one options value passed to `gix-lock`, `gix-tempfile`, `gix-ref`, loose and pack object writing, and the reftable stack. Today none of them flushes (4.3); the plumbing progress table lists the config as planned |
| Hashes | Both hashes already flow into the store constructors (`gix/src/repository/location.rs:58`); reftable needs version-2 tables for SHA-256 and the clone-ordering rule in `gix/src/clone/fetch/util.rs:30-41` |
| Reflog | Backend-neutral reflog reading with reftable's non-reloading iteration semantics stated (3.3), and `HEAD`-split behavior per backend |
| Errors | Plumbing crates still on `thiserror` keep it; the new reftable crate migrates to `gix-error` per `AGENTS.md` |
| File-specific public surface | The file store also exposes packed-buffer accessors (`cached_packed_buffer`, `open_packed_buffer`, `packed_refs_path`, used by `gix` and by `gix-protocol` negotiation), iteration platforms and loose iterators, reflog path and iterator APIs that take caller buffers, `git_dir()`, `common_dir()`, `namespace` and `write_reflog`. Each needs a neutral equivalent or a files-only marker |
| Stability policy | New store, handle, observer or flush-options types exposed from `gix` must be gated with `gix_macros::unstable(since = ...)` or listed as allowed unstable types, per `AGENTS.md` and `STABILITY.md:21-53`; the `api-check` block in `gix/Cargo.toml` is still a commented sample (`:477-499`), so this is policy rather than a CI gate today |
| Feature flags | Reftable log blocks are zlib-compressed; the upstream crate depends on `flate2` directly. The crate must work with the small, lean and max-pure builds that `AGENTS.md` requires, so the zlib backend selection and whether reftable is a cargo feature need a decision before the crate lands (not examined) |

## 6. Repository default, conversion, worktrees and concurrency

### 6.1 Default format for new repositories

| Option | Effect | Assessment |
| --- | --- | --- |
| Follow Git, refusing what is unsupported | New-repository format follows Git's precedence at the pin: `--ref-format`, `GIT_DEFAULT_REF_FORMAT` (`setup.c:2803`), `init.defaultRefFormat`, then `feature.experimental` (`setup.c:2747-2757`), then the compiled default `files` (`repository.h:25-29`); it moves when the pin moves. The fork reads none of these today (4.1), so until reftable exists a request for it must be **refused explicitly, never silently replaced by `files`**. The alternate-location forms (`GIT_REFERENCE_BACKEND`, the `<format>://<payload>` value of `extensions.refStorage`; `setup.c:690-705`, `:2824-2835`, `Documentation/config/extensions.adoc:59-71`) must likewise be refused or priced | Recommended. Matches the compatibility target, and keeps new repositories readable by every Git |
| Fork default `reftable` | Newly created repositories use reftable | Not recommended before the differential matrix passes; the consumer can opt in per repository |
| `files` only, refuse reftable | No reftable work | Conflicts with the plan's requirement list; only valid as an owner-approved scope cut, priced in section 9 |

### 6.2 Conversion of existing repositories

Git's own conversion is `git refs migrate`: it creates a new store in a scratch
directory, writes all refs and (by default) reflogs, then deletes the old store and moves
the new files into place, and finally rewrites the repository format. It refuses
repositories that have worktrees and assumes no concurrent writers; the destructive steps
are not atomic, and an interrupted run leaves the migrated refs in the scratch directory
for manual recovery (`refs.c:3344-3470`, `Documentation/git-refs.adoc:40-48`).

| Policy | Description | Assessment |
| --- | --- | --- |
| Never convert | Both formats are read and written in place; format is a property of each repository | Recommended default; nothing to authorize or lose |
| Explicit, consumer-invoked migration | A native equivalent of `refs migrate`, with a dry run, matching Git's refusal of worktrees and its no-concurrent-writer assumption. A check that no writer holds locks would go beyond Git, which has none and treats concurrent writes as the caller's problem (`Documentation/git-refs.adoc:44-48`); it would be fork policy, not parity | Offer only after the reftable store passes differential tests; priced separately in section 9; requires an authorized migration policy per the plan |
| Conversion on clone or init | Create reftable directly when the remote hash is known | Cheap once reftable exists; the init-order constraint is in 4.1 |
| Automatic conversion on open | Convert when a repository is opened | Rejected: surprises other readers and destroys the "no worse than Git" property |

### 6.3 Worktrees and concurrency

- Reftable keeps one stack per worktree for per-worktree refs and one shared stack;
  multi-stack transactions are not atomic (3.3). The consumer inventory requires correct
  linked-worktree common-directory resolution, so worktree stacks must be supported for
  reading and writing; migration with worktrees stays out of scope, as in Git.
- Contention: reftable has one writer lock per stack with a 100 ms default wait, so bursts
  of writers fail with `cannot lock references` unless callers queue; the files store
  locks per ref. A consumer with write serialization is already shaped for this; a
  second-process Git writer is not, and mixed-writer tests must run with Git's defaults.
- Reader behavior differs by operation (3.3); consumers that need a consistent view
  across several lookups should hold a ref iterator or a reftable snapshot rather than
  repeating single-ref reads; reflog iteration does not reload, so a held reflog
  iterator is not a fresh view.

## 7. Discriminating probe proposals

None of these has been run. Each is proposed with its setup, build, execution and analysis
cost, in agent effort (a fraction of one worker session) and machine time. The resource
limits in the plan's "Execution threads" section apply (`cargo -j4`, one thorough check at
a time, no spending beyond existing subscriptions). P1 to P5 are temporary investigative
probes, not retained tests: plain shell scripts in disposable directories with an isolated
Git environment (`GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `HOME` and
`XDG_CONFIG_HOME` pointed into the scratch directory, no inherited `GIT_DIR` or similar
variables). They need no cargo build, only a Git 2.56 binary on the host (the CI runners ship
it; the host's version has not been checked). Their outputs become retained tests later,
at which point `AGENTS.md` requires `gix-testtools` fixtures and its isolated Git helpers.

| ID | Question it settles | Setup | Build | Execution | Analysis | Authority needed |
| --- | --- | --- | --- | --- | --- | --- |
| P1 | Which `reference-transaction` events fire, in which order, per backend, for create, update, delete, lock conflict, stale old value and a veto at `preparing`/`prepared`, including inside receive-pack (2.5, 3.5 unchecked cells) | 0.25 session: a scripted fixture with a logging hook, both backends | None | Seconds | 0.25 session: a golden event table | Run Git in disposable fixtures |
| P2 | Golden receive-pack transcripts: advertisement, status, ordering, quarantine environment, hook arguments and stdin, for the hook-free and hooked cases, atomic and not, delete-only, stale lease, hidden refs, namespaces | 0.5–1 session: fixtures that drive `git receive-pack` over pipes and record packet lines and hook captures | None | Minutes | 0.5 session | Same |
| P3 | Files backend failure classes (3.2, 3.5): partial state when a ref rename fails after an earlier rename, reflog entry without the move, deletion interrupted between `packed-refs` and loose removal | 0.5 session: unprivileged triggers, relabeled by the phase they should hit and unvalidated until run: a directory at the **reflog** path of the second ref should fail in the commit phase after the first rename (`refs/files-backend.c:3355-3381`), giving the "first applied, rest not" state; a non-empty directory at a ref path most likely fails earlier, in prepare's availability check, giving no partial state; a pre-held `*.lock` file tests lock conflict in prepare, not partial state | None | Seconds | 0.25 session | Run Git in disposable fixtures |
| P4 | Reftable failure classes (3.3, 3.5): multi-stack partial commit, listed table missing with unchanged and changed `tables.list`, lock held beyond 100 ms, corrupt `tables.list` | 0.5 session: linked-worktree fixture, permission changes on the worktree stack (ineffective when run as root, so the probe must not run as root), hand-edited list | None | Seconds | 0.25 session | Same |
| P5 | Flush ordering: which `fsync` calls Git issues, in which order, for `update-ref` and receive-pack under `core.fsync` and `core.fsyncMethod` settings | 0.25 session: `strace -f` on the Linux host so index-pack, unpack-objects and hook children are followed (not available on macOS; the host's `strace` and ptrace policy are unchecked) | None | Seconds | 0.5 session: ordering table to diff the fork against later | Run Git under `strace` in disposable fixtures |
| P6 | Are the upstream reftable codecs byte-compatible and sound? Build the crate at `94793bb6f` in a scratch worktree, run its tests, have Git read tables it writes and it read tables Git writes, SHA-1 and SHA-256 | 0.25 session | A cold build of one crate and its dependencies: minutes at `-j4` (not measured) | Minutes | 0.5–0.75 session | Cargo build on the shared host; resolves R1 versus R2 and the reftable codec price |
| P7 | Fork files-store failure classes under the P3 triggers, and the reflog-deletion, missing-committer and `~pid.lock` deltas in 4.3 | 0.5 session | `gix-ref` tests: incremental builds | Seconds | 0.25 session | Cargo build on the shared host |
| P8 | Mixed Git and fork reftable writers racing, including compaction versus writer | Needs the new stack layer | Needs the implementation | Minutes | 0.5 session | Not proposed now; first gate of the reftable thread |
| P9 | Power-loss flush correctness | A device-level fault-injection environment (a privileged virtual machine) | n/a | n/a | n/a | Not proposed: likely needs spending or a disruptive environment. Without it, power-loss claims stay "unproven" and P5 supplies ordering evidence only |
| P10 | Consumer warm routine check (under 60 s) and cold, edited-core, duplication, size and latency measurements | Needs the consumer's repository, which the fork cannot read | Consumer builds | Hours of machine time | 0.5 session | Owner or consumer-side action; measured under comparable load |
| P12 | The unsafe-write hazard (4.1): open a reftable clone with `gix` and attempt `refs/heads/x` and `refs/tags/x` updates; does anything land as a loose file Git never reads? | 0.1 session: reuse the existing reftable fixture script (`gix/tests/fixtures/make_reftable_repo.sh`) | One incremental `gix` test build at `-j4` | Seconds | 0.1 session | Cargo build on the shared host; cheapest first cargo probe |
| P11 | Fork-side cost of the candidate features: cold build, release size and duplication for the plain, `sha256` and each HTTP transport feature set | 0.25 session | Several cold builds at `-j4`; `AGENTS.md` documents about 2.5, 1.5 and 0.8 minutes for the default, lean and small release variants (documented, not measured here) | Minutes each | 0.5 session | Cargo builds on the shared host |

Recommended order and tiers: authorize P1 to P5 first (Git only, disposable fixtures,
about 2 to 3 sessions of effort in total, no cargo); then P6, P7 and P11 (cargo builds,
about 1.5 to 2 sessions); keep P8 as the entry gate of the reftable thread; P10 needs the
owner; P9 is not recommended.

## 8. Initial dependency and cost baseline (source observations)

Established so far, with the measurement that is still missing named beside each.

| Item | Observation | Not measured |
| --- | --- | --- |
| Fork routine and thorough budgets | The plan's "Execution threads" section records a 120 s routine-check budget against a warm 94–109 s baseline and a 12 minute thorough-check budget against 530 s; `FORK.md:224` separately targets five minutes with warm caches for its planner-selected run | Whether new code keeps within them; to be tracked per thread |
| Consumer budget | The plan requires the consumer's warm routine check to stay under 60 s; its historical value of about 45 s has not been refreshed | Everything, until P10 |
| Release variants | `AGENTS.md` documents about 2.5, 1.5 and 0.8 minutes for the default, lean and small release builds. Measured on the shared host (P11 below): 151 s, 147 s and 65 s | Load-free values; the lean and small builds ran slower than documented |
| Reftable dependency surface | A reftable crate adds a workspace crate (the upstream branch's is about 3.4k lines) whose third-party dependencies `crc32fast`, `flate2` and `thiserror` already appear in `Cargo.lock`; no new third-party crate is implied | Compatible versions; compile time of the new first-party code |
| Receiver dependency surface | Pack writing, object access, connectivity and hooks exist in the tree; the new code is first-party (server protocol, quarantine, policy, strict fsck). Signed pushes and hooks run external programs the consumer already provides | Compile-time and binary-size impact |
| Network features | `blocking-network-client` and several `blocking-http-transport-*` variants already exist in `gix/Cargo.toml` (`:220-261`); the HTTPS choice, not the receiver, decides the third-party cost. P11 below measured build time, binary size and duplication per variant | Latency |
| Hash support | The `sha1` and `sha256` features exist (`gix/Cargo.toml:102-110`). Enabling both costs about 7 more crates and 6 KB in a consumer binary, with no measurable build time (P11 below) | The consumer's own figures (P10) |
| First-party growth | Planning sizes below imply roughly 10–14k new lines for reftable and 10–15k for the receiver, including tests (inferred from Git's own sizes, section 9) | Compile time per added kloc |

**Probe P11 result (ran 2026-11-04 between 13:57 and 14:23 UTC, fork `dev` at `1bc6fd610`, Rust 1.99, `CARGO_BUILD_JOBS=4`).**
A scratch consumer crate outside the repository depends on `gix` by path with `default-features = false` and
`sha1` plus one feature set. It opens a repository and lists references, and with the network features it also
calls `remote_at(..).connect(Fetch)` and `ref_map()`, so that the transports are linked and not discarded.
Each row is one cold `cargo build --release` into an empty target directory (`strip = true`, crates already in the
cargo cache, so no download time), run under the station's shared build lock. The 1-minute load average before each
build was 2.5 to 4.0 because other workers were running, so these are not load-free figures. One run per row; the
`reqwest-rustls` figure includes a load spike to 9.8 and is probably high, as `reqwest-native-tls` has about the same
number of crates and took half as long.

| Feature set | Cold build | Crates compiled | Binary | Crates with several versions |
| --- | --- | --- | --- | --- |
| `sha1` only | 27 s | 90 | 1.97 MB | `hashbrown` |
| `sha1` and `sha256` | 26 s | 97 | 1.98 MB | `hashbrown` |
| `blocking-network-client` | 36 s | 110 | 3.08 MB | `hashbrown` |
| `blocking-http-transport-curl` | 45 s | 121 | 4.12 MB | `hashbrown` |
| `blocking-http-transport-curl-rustls` | 68 s | 141 | 7.97 MB | `hashbrown` |
| `blocking-http-transport-curl-openssl` | 44 s | 123 | 4.22 MB | `hashbrown` |
| `blocking-http-transport-reqwest-rust-tls` | 89 s | 195 | 9.45 MB | `base64`, `hashbrown`, `syn` |
| `blocking-http-transport-reqwest-native-tls` | 45 s | 190 | 5.74 MB | `base64`, `hashbrown`, `syn` |

The `gix` binary built from the repository (`cargo build --release`, cold, load 2.5 to 3.9): `small` 65 s and
16.6 MB, `lean` 147 s and 37.9 MB, default 151 s and 38.5 MB.

What this says for the plan:

- Pushing needs `blocking-network-client` only, which costs about 9 s, 20 crates and 1.1 MB over the plain consumer.
  `sha256` is practically free.
- The HTTPS choice decides the third-party cost. The curl variants add 11 to 31 crates and 1 to 5 MB over the
  network client. The reqwest variants add about 80 crates and 2.7 to 6.4 MB, and bring `base64` and `syn` in two
  versions. `curl` with the system TLS library is the cheapest; `curl-rustls` avoids a system library for about 3.9 MB more.
- The documented `AGENTS.md` figure matches for the default build. The measured `lean` build took about 1.6 times,
  and the `small` build about 1.4 times, the documented time, at a load of 2.5 to 3.9.
- Not measured: incremental and edited-core builds, the reftable crate and the receiver, as neither is linked into `gix`
  yet, and the consumer's own figures (P10).

The early baseline does not close the plan's final cost gate; the actual receiver,
storage, reftable and HTTPS features are re-measured before cutover.

## 9. Revised implementation estimate

### 9.1 Units and basis

- A **session** is one worker session of two to four reviewer-checked chunks, with one
  persistent reviewer agent, as this assessment was run. A **review round** is one
  author-to-reviewer handoff with its findings fixed; this assessment needed two per
  chunk. Both are estimates, not measurements, for implementation work.
- **Wall-clock, calibrated once.** This assessment was budgeted at 5 to 8 hours and ran about
  17 minutes by the sandbox clock (01:43 to 02:00 UTC on 2026-11-04, from the recorded
  worker launch time and the commit timestamps), including two review rounds per chunk;
  not independently verified. It was read-only and built nothing, so that number
  calibrates reading and writing work only. Implementation sessions add builds, the
  repository's checks (the plan's "Execution threads" section records a 120 s routine
  and a 12 minute thorough budget) and real review loops, none observed yet. The planning
  assumption is therefore 0.5 to 2 hours of wall-clock per implementation session,
  unmeasured. The client-protocol and hook-runner work that the plan starts alongside
  this assessment supplies the first observed implementation figures at no extra cost.
- Sizes come from Git's own code, which the Rust work must match behavior for, counted with
  `wc -l` at the pin: the reftable library (7,113 C lines and 2,123 header lines), its unit
  tests (9 files, 4,832 lines), the reftable ref backend (2,891 lines), the files backend
  (4,114), `receive-pack` (2,636), `fsck.c` and `fsck.h` (1,488 and 292 lines, with 76
  message ids), Git's maintenance code (`builtin/gc.c`, `builtin/repack.c`, `repack*.c`,
  `builtin/prune.c` and `reachable.c`: 6,622 lines), and 10,828 lines of oracle shell tests
  (`t5410`, `t5411` and `t5411/*`, `t5534`, `t5546`, `t5516`, `t5504`, `t5571`, `t1400`,
  `t1416`, `t0610`, `t0613`, `t0614`; a representative set, not a complete one). The
  assumption is 1,000 to 1,500 reviewed lines of Rust including tests per session. That
  assumption is the largest single driver and is not measured.
- Estimates assume the plan's structure: up to three workers on disjoint crates, one
  thorough check at a time.

### 9.2 Base: Git-free client and local backend (excluding reftable and full receiver)

Rows marked "Low, placeholder" have no source audit and no size basis beyond the number of
operation families in the consumer inventory; they are order-of-magnitude placeholders, so
the base total is the least reliable total here.

| Package | Sessions | Review rounds | Confidence |
| --- | --- | --- | --- |
| Client protocol and bounded packs (started alongside this assessment) | 1–2 | 3–6 | Medium |
| Hook runner including config-defined hooks (started alongside this assessment) | 1–1.5 | 2–4 | Medium |
| Durability plumbing: `core.fsync`, `core.fsyncMethod`, barrier flush, files-store gaps (4.3), uncertain-outcome class, transaction observer | 2–3 | 5–8 | Medium |
| Ref-store abstraction (5.1, staged) and call-site migration, including the interim refusal of non-`files` repositories (decision 7) | 1–2.5 | 3–6 | Medium |
| Differential durability and fault harness against Git (P3–P5 as tests) | 1–2 | 3–5 | Medium |
| Local consumer backend: discovery, configuration, init, readiness, objects, refs, `reference-transaction` | 3–4 | 8–12 | Low, placeholder |
| Repository push API, leases, fetch pruning, reconciliation | 2–3 | 5–8 | Low, placeholder |
| SSH, HTTPS, authentication, direct credential helpers, client hooks, low-speed policy | 2–3 | 5–8 | Low, placeholder |
| Native maintenance (Git's comparable code is 6.6k lines, about 4.4 to 6.6 sessions at the stated throughput before tests; reduced because scheduling and cron code is not needed; plus grace, keep, in-flight-root and interrupted-reclamation safety) | 3–6 | 7–12 | Low |
| Cutover, packaging, verification | 1–2 | 3–5 | Low, placeholder |
| **Base total** | **17–29** | **44–74** | |

### 9.3 Reftable (priced separately)

| Package | Sessions | Review rounds | Confidence |
| --- | --- | --- | --- |
| Codec parity review and adoption, both hashes, version 2 | 1–1.5 | 3–4 | Medium (P6 narrows it) |
| Stack layer: lock protocol, reload, publication, segment compaction, cleanup, flush | 2–3 | 6–9 | Low |
| Ref backend: transactions with logs, symrefs, `HEAD` reflog, tombstones, worktree stacks, iteration, reflog read and expiry | 2–3 | 6–9 | Low |
| Interoperability and differential tests: Git reads fork, fork reads Git, mixed writers, corruption, both hashes | 1.5–2.5 | 4–7 | Low |
| **Reftable total** | **6.5–10** | **19–29** | |
| Optional: explicit migration tool (6.2) | 0.5–1 | 2–3 | Medium |

Derivation: about 10 to 14k new lines of Rust including tests (the Rust equivalent of Git's library, backend and the tests that matter), minus about 3.4k lines adopted from the upstream codecs under R1, at 1.0 to 1.5k reviewed lines per session gives roughly 4.4 to 10.6 sessions of writing, and the 6.5 to 10 priced here puts the adoption review and Git interoperability testing on top of the low end and trims the high end where codec reuse also removes test-porting. The row sizes are judgment, not measurement.

If the codec parity review (P6) fails and the codecs are rewritten (R2), add 1.5 to 2.5
sessions. If upstream lands a usable store first (R3), the first and second rows shrink;
this is not planned for.

### 9.4 Full receiver parity (priced separately)

| Package | Sessions | Review rounds | Confidence |
| --- | --- | --- | --- |
| Server-side protocol: advertisement, command, certificate and option parsing, report v1/v2, sideband and keepalive | 1–1.5 | 3–4 | Medium |
| Object ingestion: quarantine, unpack-or-keep, thin-pack fixing, migration with the barrier flush, strict fsck (76 message ids, skip list), size limits | 2.5–3.5 | 7–10 | Low |
| Policy and ref planning: deny rules, hidden refs, namespaces, aliased updates, atomic and batched execution with Git's rejection semantics, shallow updates, certificates through an external signer, reflogs | 2–3 | 6–8 | Low |
| Hook integration: `pre-receive`, `update`, `post-*`, `proc-receive`, `reference-transaction` states | 1.5–2 | 4–6 | Medium |
| `updateInstead` and `push-to-checkout` without Git subprocesses (see decision 4) | 1.5–2.5 | 4–7 | Low |
| Native local transport: listing, fetch and receive in process | 2–3 | 5–8 | Low |
| Receiver differential matrix against Git (selected oracle tests, golden transcripts from P2) | 2–3 | 5–8 | Medium |
| **Receiver total** | **12.5–18.5** | **34–51** | |

Derivation: about 10 to 15k new lines including tests at 1.0 to 1.5k per session is 6.7 to 15 sessions; the 12.5 to 18.5 priced here adds integration with the hook runner, the ref store and the transport, and the differential matrix against Git, which are the larger part of receiver risk. The row sizes are judgment, not measurement.

Hook-free bare-repository receiving alone (server protocol, ingestion, policy subset,
native local transport, differential matrix) is about 8 to 12 sessions; the plan states
that this subset does not satisfy the chosen endpoint, so it is a sequencing step and not
a scope cut.

### 9.5 Totals and sequencing

| Scope | Sessions | Review rounds |
| --- | --- | --- |
| Base | 17–29 | 44–74 |
| Base plus reftable | 23.5–39 | 63–103 |
| Base plus full receiver | 29.5–47.5 | 78–125 |
| Everything | 36–57.5 | 97–154 |

At the planning assumption of 0.5 to 2 hours per implementation session (9.1), the totals
are roughly 9 to 58 agent-hours for the base, 3 to 20 for reftable and 6 to 37 for the
receiver, 18 to 115 for everything in series. With up to three lanes on disjoint crates the
elapsed span is roughly a half to a third of that, before the owner's decision gates, review
stalls and rework, which the figures do not include. These hours are derived from an
unmeasured assumption and should be replaced by the observed figures from the running
threads. The earlier human-effort estimates (8 to 15 engineer-weeks for a smaller scope,
3 to 6 engineer-weeks for blocking client push) are superseded and are not converted.

Sequencing implied by the dependencies: durability, abstraction and local backend first;
then reftable and the receiver on separate lanes once the abstraction lands (both need
it); maintenance after both; cutover last. The reftable lane's first gate is P6 and P8.

### 9.6 Remaining uncertainty and the smallest next investigation

| Uncertainty | Smallest next investigation | Cost |
| --- | --- | --- |
| Sessions per kloc and hours per implementation session | Read the elapsed times and sizes of the client-protocol and hook-runner work when it ends | None |
| Reftable codec quality and R1 versus R2 | P6 | About 1 session, one small cold build |
| Files and reftable failure classes in Git | P3 and P4 | About 1 session, no build |
| `reference-transaction` call sequences per backend | P1 | About 0.5 session |
| Whether any consumer receiver is non-bare or uses `updateInstead` | Ask the consumer; no investigation | None |
| Real consumer cost impact | P10 | Owner action |

## 10. Decisions for the owner

| # | Decision | Recommendation | Why |
| --- | --- | --- | --- |
| 1 | Storage selection | An enum store reached through an additive handle first (5.1 A staged through C), reftable from adopted codecs with a new stack layer (5.2 R1), Git's lock and publication protocols for both backends | Fits the existing scaffolding, keeps the first step non-breaking, and puts the new effort where the interoperability risk is |
| 2 | Default ref format | Follow Git's precedence (`--ref-format`, `GIT_DEFAULT_REF_FORMAT`, `init.defaultRefFormat`, `feature.experimental`, default `files` at the pin); until reftable exists, refuse an explicit or configured reftable request rather than fall back to `files`; reftable opt-in per repository until the differential matrix passes. Separately, **now**: make the fork refuse writes to any repository whose `extensions.refStorage` is not `files` (4.1), a small independent fix | Matches the compatibility target; Git itself makes Gitoxide support a prerequisite for its own reftable default |
| 3 | Conversion policy | Never convert by default; offer an explicit, consumer-invoked migration only after reftable passes its differential tests, refusing worktrees as Git does; no automatic conversion | The plan forbids conversion without an authorized policy; Git's own conversion is non-atomic and assumes no concurrent writers |
| 4 | Revised scope and estimate | Accept the ranges in section 9 as planning inputs, not commitments, and decide: (a) `updateInstead`/non-bare receiving native versus an explicit refusal (recommend asking the consumer first; if every receiver is bare, refuse explicitly and price it later); (b) reproduce Git's backend-dependent `reference-transaction` call sequences versus a named divergence (recommend reproducing only if the consumer's hooks depend on it); (c) sequencing: base first, then reftable and receiver on separate lanes | These are the scope items that move the estimate most |
| 5 | Experiment authority | Authorize P1 to P5 now (Git only, disposable directories, no cargo); authorize P12, P6, P7 and P11 as a second tier within the plan's resource limits; keep P8 as a thread gate; take P10 as an owner or consumer action; do not authorize P9 | The first tier answers most open Git-behavior cells for no build cost; P9 likely needs spending or disruption |
| 6 | Upstream coordination | None proposed; reading the upstream branch is enough for now, and no posting or contact is authorized | The plan makes upstream acceptance a non-dependency; recheck upstream status before the reftable thread starts |
| 7 | Reftable repositories opened by the fork today (4.1) | Refuse every reference operation, reads and writes, on a repository whose `extensions.refStorage` is not `files`, with an `Unsupported` error, while leaving the open and object access working; and make `gix init` refuse a requested reftable format instead of creating `files`. Options: (a) refuse the open, which is safest but also blocks object-only use that works today and conflicts with the plan's readiness requirement to "support mapped reftable clones" (`docs/push-support-plan.md`, consumer operation inventory); (b) refuse writes only, which prevents a loose-file write Git never sees but still lets reference reads return "missing" for references that exist; (c) refuse all reference operations, recommended, an interim state that meets the mapped-reftable-clone requirement only once the reftable store lands; (d) warn only, which leaves both hazards. Effort: about 0.5 to 0.75 session and 2 to 3 review rounds (a guard set at open from the configuration, the init refusal, tests on the existing reftable fixtures; probe P12 confirms the write case first). Upstream `main` has the same behavior, so this is not fork-specific and could be offered upstream, which is not authorized | The reads-return-missing case is silent wrong data, and the write case is probably repository corruption as seen by Git; both exist now, independent of this plan |

## 11. Evidence limits

- Every statement about Git is a source or documentation reading at tag
  `v2.56.0` (`a018953688f1b10bddf91bff8747068f5f4746a4`); none was exercised against a
  running Git. Every statement about the fork is a source reading at `b592770fc`. The only
  commands run were `rg`, `wc`, `git show`, `git log`, `git merge-base` and `date`, all
  read-only, and `git clone --depth 1` of the Git tag into an ignored scratch directory.
- The receiving matrix is a closed enumeration of the code paths and keys read, derived
  by reading; behavioral proof is absent for every row. Internals of `fsck` message
  handling, shallow-update computation, signature verification and `unpack-objects`
  flushing inside the quarantine are named but not traced.
- The estimates rest on size analogies and an unmeasured throughput assumption; hours
  per implementation session are unmeasured (9.1). The base rows marked placeholder were
  not audited in source. Credentials, SSH, HTTP low-speed handling, repository discovery,
  configuration persistence and the consumer's readiness and ancestry operations were
  not audited at all; they appear only as estimate rows.
- Inferred hazards (the reftable write case, the P3 triggers, linked-worktree `HEAD`
  reflogs) were not run, and whether the shared host has Git 2.56 or `strace` was not
  checked. Downstream consumers of the public `gix` fields were not read.
- The unmerged upstream branch was inspected only at the commit named in 4.2; its status
  can change, and a newer upstream state was not examined.
- Power-loss behavior is unproven for both backends; only ordering evidence (P5) is
  proposed.

## 12. Stop

This is the end of the assessment. It selects nothing, builds nothing and authorizes
nothing. The next step is the owner's decision on section 10.
