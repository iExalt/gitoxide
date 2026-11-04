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
equals the current value writes nothing and gets no reflog entry (`:2857-2866`).

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
| Objects from a push | Barrier flush of a scratch file before migration, unconditional (`object-file.c:1259-1294`); packs and derived metadata flushed by default; the default component set excludes loose objects, but pushes under the unpack limit run `unpack-objects`, which starts its own object transaction inside the quarantine (`builtin/unpack-objects.c:605`); that interaction is not checked | Same | Barrier yes; loose objects: unverified |

The plan's statement that objects are flushed "no earlier or later" than `core.fsync`
requires does not hold for receive-pack: the migration barrier is unconditional, and a
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

