# Push support: architecture assessment

Status: in progress; awaiting the owner's decisions (section 10). This is the bounded, read-only assessment that
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
- Migration order: an unconditional flush of a scratch file as a durability barrier (not
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

