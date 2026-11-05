# Push durability: measured Git behaviour

This page records what Git did when run under the probes that the
[architecture assessment](push-architecture-assessment.md) proposed as P1, P3 and P5, and what the fork's
files store did under the P3 triggers (P7). It replaces the source-reading claims of assessment sections 3.2, 3.4
and 3.5 where the two differ. The fork's tests that follow from it live next to the code they test.

## Pins and limits

| Item | Pin |
| --- | --- |
| Git | 2.56.0 |
| Platform | Linux 6.11, x86_64, unprivileged user |
| File system | ext4 on a virtual disk |
| `strace` | 6.8, with `-f` so that child processes are followed |
| Isolation | no system or global configuration, no inherited `GIT_*` variables, `GIT_TEST_FSYNC` unset |
| Scripts | [`etc/scripts/durability/`](../etc/scripts/durability) (`p1.sh`, `p3.sh`, `p5.sh` take a scratch directory) |
| Golden | [`golden/p5-git-2.56.0.txt`](../etc/scripts/durability/golden/p5-git-2.56.0.txt) is the normalized output of `p5.sh` for the pinned Git, which a native receiver is to follow. Regenerate it as described in `p5.sh` |

Process-kill evidence comes from `SIGKILL` sent from a `reference-transaction` hook and by `strace` fault injection.
**Power loss is not tested and its behaviour is unproven**: no probe here can show what survives a power failure.
`strace` shows which flushes Git issues and in which order. It does not show what the storage does with them.
Only Linux on ext4 was observed. Statements marked *(source)* come from reading Git's source and were not observed; macOS and Windows behaviour is read from it, not measured.

## Flush behaviour (P5)

- A ref update writes the new value into `<ref>.lock`, flushes that file **only if** `core.fsync` selects `reference`,
  appends the reflog entry, and then renames the lock file. Reflogs are never flushed, and no directory is ever
  flushed. Symbolic references that `git symbolic-ref` writes are not flushed either, unlike `update-ref --no-deref`. Flushing happens while the transaction is *prepared*, so a failed flush would leave every ref unchanged *(source: `refs/files-backend.c:2069` reached from `:2867`, `refs/packed-backend.c:1570` from `:1744`; no flush failure was injected)*.
- `packed-refs.new` is flushed under the same condition, before its rename. For reftable repositories the new table
  and `tables.list.lock` are flushed under that condition as well.
- `core.fsyncMethod=writeout-only` turns each flush into `sync_file_range()` on Linux, and `batch` still flushes
  refs, packs and reftable files with a full `fsync()`.
- `core.fsync` adds to the defaults, which cover packs, their indices and the commit-graph, but neither loose objects
  nor references. A `core.fsync` entry replaces the value of earlier entries rather than adding to them *(source)*.
  The tests in `gix-fs` compare Git's flush counts for a table of values, quirks included, with what `gix_fs::fsync` selects.
- A loose object is flushed before it is linked into place only with the `loose-object` component or with
  `core.fsyncObjectFiles=true`. With `core.fsyncMethod=batch` *and* `loose-object`, each object is written out and
  one barrier flushes them all *(source: `write-or-die.h:76-79`; observed with `core.fsync=all`)*.
- `git receive-pack`, with the defaults, writes objects into the quarantine and flushes a scratch file
  `bulk_fsync_XXXXXX` in it *once* before it moves the objects into the object database. It does that regardless
  of `core.fsync`, but with the configured method. After the move, it flushes the ref lock file if `reference` is
  selected, and renames it. For a pack, the index, the reverse index and then the pack are flushed (defaults), the
  barrier follows, and `.keep`, `.pack`, `.rev` and `.idx` are moved in that order.
- `core.fsync=all` with `core.fsyncMethod=batch` makes `unpack-objects` write out every loose object with
  `sync_file_range()` into a nested directory `tmp_objdir-bulk-fsync-*`, flush a barrier file, and only then move
  them to the quarantine, where `receive-pack`'s own barrier follows.

## Reference transactions (P1)

Every successful transaction fires `preparing`, `prepared` and `committed`. Other sequences, for the files backend
unless noted:

| Situation | Events |
| --- | --- |
| Deleting a loose ref | `preparing`, `aborted`, `prepared`, `committed` (the `aborted` is the nested `packed-refs` transaction finding nothing to delete) |
| Deleting a packed ref | each of `preparing`, `prepared` and `committed` twice |
| Stale old value, lock held by someone else, creating a ref that exists | `preparing` only, no `aborted` |
| Veto at `preparing` or `prepared` | `aborted` follows, with `prepared` in between for a veto at `prepared` |
| Exit status of `committed` | ignored |
| Updating through `HEAD` | `preparing` names `HEAD`, `prepared` and `committed` name the branch |
| Updating `HEAD` and a branch `a` in one transaction | `preparing` lists `HEAD`, then `a`. `prepared` and `committed` list `a` first and then the branch `HEAD` points to, as Git appends the split-off update at the end |
| A deletion is locked, then a later edit fails to lock | `preparing`, then `aborted` for the deletions locked so far. With the failing edit first, only `preparing` |
| `packed-refs.lock` is held by somebody else | `preparing`, then `aborted` for all deletions, as `packed-refs` is locked after the loose references |
| A name is also the target of a symbolic reference that is edited | `preparing` lists the edits as requested, then the transaction fails, with `aborted` for the deletions before the symbolic reference |
| Deleting `HEAD` | `preparing` names `HEAD`, `aborted` and the later states name the branch |
| Veto while the transaction on `packed-refs` is prepared | that transaction's `aborted`, and Git exits without an `aborted` of the outer transaction |
| The same reference edited twice | rejected without any event |
| Reftable | one set of events, without nested ones |

The files store reports all of these through `gix_ref::transaction::observe`. The tests in
`gix-ref/tests/refs/file/transaction/prepare_and_commit/observe.rs` run 27 rows with Git, with a hook that logs its
arguments and input, and with an observer of the files store on a copy of the repository, and compare the events. The
transaction Git opens on `packed-refs` appears in them, indistinguishable from the others as in a hook. Only the files
store with Git's default of deleting from `packed-refs` is compared. Updates that pack references have no Git
counterpart.

A push through `receive-pack` runs one transaction for all deletions first and a second for the other commands,
unless `--atomic` is used, in which case one transaction holds all commands in order. Rejecting every command still
runs all three events with no updates. A ref that a `pre-receive` hook moved before the transaction is rejected on its own without `--atomic`, while the
rest of its batch is applied, yet it still appears in the `committed` event of that batch. With `--atomic`, all
commands fail and only `preparing` fires.

## Failure while committing (P3, files backend)

All injections happen at `prepared`, when every lock is held and nothing was renamed, or with `strace`.

| Injected failure | Result |
| --- | --- |
| Directory with content at the second ref's reflog path | The first ref is updated, the second is not, `commit` fails |
| Directory of the second ref made read-only | The first ref is updated, the second's reflog has an entry for a change that never happened, a stale `.lock` file remains because it cannot be unlinked |
| Loose and packed ref deleted, directory made read-only | The packed entry and the reflog are gone, the loose file remains and the ref still resolves, a stale lock remains |
| Second reflog file made read-only | The first ref is updated, the second is not, and its reflog is unchanged |
| `SIGKILL` at `prepared` | Every `.lock` file remains with the new value, no ref changed |
| `SIGKILL` at the second of three `rename()` calls | The first ref is updated, the second has its reflog entry but kept its value, the lock files of the second and third remain |
| `EIO` at the second of three `rename()` calls | As above but without stale locks, the process lived to clean up |
| Reflog of a deleted ref cannot be unlinked | Git ignores it: the ref is deleted, `rc=0`, an orphaned reflog remains |
| No committer identity in the environment | The update succeeds, with an identity Git derives from the system |

The `strace` injections reproduced in five of five runs. Updating the branch `HEAD` points at without changing its
value still appends an entry to the `HEAD` reflog, but none to the branch's reflog.

## The fork's files store (P7)

Run with the same triggers, between `prepare()` and `commit()`, before the changes described below. It matched Git for
the first four rows of the failure table, and deviated in three ways, which are now closed:

| Deviation | Now |
| --- | --- |
| Nothing was flushed | `gix_ref::file::Store::fsync` selects what is flushed. The value of a loose reference and the new `packed-refs` file are flushed while *preparing*, if `reference` is selected, as in Git. Reflogs are not flushed. |
| A reflog that could not be deleted aborted the commit and left the reference in place | The reference is deleted and the reflog remains, like Git. Only deleting nothing but the reflog still fails. |
| A missing committer failed after creating an empty reflog file | It fails before anything is written, with the marker `NothingApplied`. Git succeeds with an identity derived from the system, which the fork deliberately does not copy. |

The fork also wrote the whole `packed-refs` file after the loose files were renamed, where Git writes and flushes it
while preparing. It does the same as Git now. Failures of `commit()` carry a marker, `NothingApplied` or `Uncertain`, with
how far each edit got, which Git can't tell its caller. The tests that follow are in
`gix-ref/tests/refs/file/transaction/prepare_and_commit/commit_failure.rs`. Two differences remain by design. Git derives
an identity when the committer is missing. And updating the branch `HEAD` points at without changing it still appends to
the `HEAD` reflog in Git, which the fork doesn't do.

## Objects and packs in the fork

Git's flushes of objects and packs are followed by the stores that write them. The tests run Git and the fork under `strace`,
on the same input and for a table of `core.fsync` and `core.fsyncMethod` values, and compare the abstract sequence of what
happened to the files:

* **Loose objects** (`gix-odb/tests/odb/durability.rs`): the temporary file is written, flushed if `loose-object` or
  `core.fsyncObjectFiles` select it, and made visible. Git links where the fork renames, which the comparison abstracts from.
  With `core.fsyncMethod=batch`, Git batches flushes only within a multi-object operation, so a single object is flushed
  fully by both. The fork has no such operation yet, and `gix_fs::fsync::Options::barrier()` and `loose_object_batch()` wait
  for it. Git dies when the flush fails and leaves its temporary file, where the fork returns an error and removes it.
* **Packs** (`gix-pack/tests/pack/durability.rs`): the index is flushed as `pack-metadata`, then the pack as `pack`, then the
  unflushed `.keep` file is written, and the pack becomes visible before its index, like `git index-pack --stdin --keep`. Git
  also writes and flushes a reverse index, which the fork doesn't write at all. This is not a durability gap.
* **Configuration**: `gix` passes `core.fsync`, `core.fsyncMethod` and `core.fsyncObjectFiles` to the object database when it
  opens a repository, which makes changing them require reopening it, and to the writer of fetched packs.

`receive-pack` flushes a barrier file once before it moves quarantined objects into place, and `gix::receive::Receiver`
does the same. `gix/tests/gix/receive/receive_pack.rs` compares the phases of a whole push under `strace` with Git for the
default and the `core.fsync=all` configurations, loose and kept: objects are flushed in the quarantine, a barrier makes them
durable, they are moved into place, and only then are references locked, renamed and released. The comparison is of phases,
because how the quarantine writes its files is what the tests of the stores above compare.

## The fork's receiver

`gix/tests/gix/receive/receive_pack.rs` runs the same pushes against Git 2.56 `receive-pack` and the receiver, for both hash
kinds and both reference formats, with Git in the C locale, and compares the report, the messages sent on the side band,
the references, the objects, what is left behind and the advertisement. The receiver differs on purpose in these ways,
each pinned by a test:

| Case | Git | The receiver |
| --- | --- | --- |
| Deleting a reference with a null old value, which a client that sends no old value does | Skips `receive.denyDeletes` and `receive.denyDeleteCurrent` | Applies them like any deletion |
| An update whose old value is stale, through a symbolic reference | Reports `ok` for a reference that didn't change | Reports `ng … incorrect old value provided` |
| Updating the branch `HEAD` points at without changing it | Appends to the `HEAD` reflog and locks `HEAD` | Does neither |
| A request that `receive.autogc` would act on | Runs `git gc --auto` | Reports `Outcome::maintenance_due` and leaves it to the caller. `NativeLocal` doesn't attach `Receiver::with_auto_maintenance()`, so a push through it runs no maintenance at all, which Git's `receive-pack` would run |
| A reverse index next to a kept pack | Writes one | Writes none, see above |
| An update that is rejected while it is queued, for the branch `HEAD` points at | Appends to the reflog of `HEAD` anyway | Doesn't |
| Deleting or updating the current branch through a symbolic reference | Protects the name that is pushed to only, so `receive.denyDeleteCurrent` and `receive.denyCurrentBranch` are bypassed by an alias | Protects what the reference leads to as well, and refuses |
| No identity is configured | Derives one from the machine | Uses the fixed identity `no name configured <noEmailAvailable@example.com>` for reflogs |

The fixed identity differs on purpose from the rule of the reference store, that a missing committer fails the update. A
receiver must not fail a push for lack of an identity, which servers often don't have, so it supplies one itself.
Deleting a branch through a symbolic reference deletes both the reference and the branch in Git and in the receiver.

Hooks are compared as well, with scripts that record their arguments, input, environment and position in the sequence
(`gix/tests/gix/receive/hooks.rs`). `GIT_DIR` is the absolute path where Git passes `.`, which the comparison treats as the
same. These differences remain, for the network phase to decide about:

* What a hook prints is captured and relayed on the side band when it has exited, where Git streams it. A hook that prints
  without bound makes the receiver's memory grow without bound.
* Keep-alive packets are sent every `receive.keepAlive` seconds from the start of each hook, where Git sends one after that
  many seconds of silence. None are sent while connectivity is checked or deltas are resolved, which Git does as well, and
  which matters to clients with timeouts for large pushes.
* A hook that a signal ended, other than an interrupt, quit or broken pipe, declines like in Git, and the client is told
  `error: <program> died of signal <n>` after what the hook said. The program is named relative to the Git directory like
  Git does, which differs only for a `core.hooksPath` that is an absolute path inside the Git directory.
* Hooks that are configured to run at the same time say what they say in the order in which they finish, in Git and in the
  receiver, so the tests of that case use hooks that say nothing. Everything else, including the input each hook gets and
  that one failing hook declines the push, is compared.
* The receiver works in the namespace that the repository it was created from has, which is what `GIT_NAMESPACE` is for Git,
  and which nothing in `gix` reads from the environment, so callers set it with `Repository::set_namespace()`. Clients, hooks
  and reports see names without the namespace, and `GIT_NAMESPACE` is exported to hooks, which also see the names with it in
  the `reference-transaction` hook. Everything outside the namespace is advertised as `.have`, in the order of the
  references, with `^` patterns of `receive.hideRefs` matching names with the namespace and all other patterns those
  without, which hides nothing outside of it. The rules of Git that look at the name without the namespace do so in the
  receiver as well: worktrees, `denyDeletes` and the existence of a corrupt reference. The current branch is that of the
  global `HEAD`, which is only affected by a namespace that contains it. Differences:
  * Git crashes with a segmentation fault when it is to update a symbolic reference of the namespace that leads out of it, by
    looking up a name that isn't there. The receiver refuses as for a broken symbolic reference.
  * Git compares what the reference store rejected with the commands by the name with the namespace, which no command has,
    so it reports `ok` for an update that was rejected as stale or for deleting what isn't there, in a push that isn't atomic.
    The reference doesn't change in either, and the message for people is the same. The receiver reports `ng` with the reason.
    Because Git thinks that these commands went well, it also runs `post-receive` and `post-update` for them, which the
    receiver doesn't. The tests compare everything else by taking Git's `ok` for these, in rows that have no hooks.
* A shallow repository is received into by advertising every entry of its `shallow` file, once and in order, whether it has the
  commit or not, as Git does. Only those commits count as having no parents: a client that leaves out history because the
  receiver advertised a reference that reaches it is told `missing necessary objects` when the receiver doesn't have what is
  below its shallow commits, which is what Git does as well, and the commit-graph file isn't believed, which tells of parents
  that a shallow repository doesn't have.
* A client that is shallow sends the commits at its boundary, and without `receive.shallowUpdate` the commands whose history
  leads to one of them are rejected with `shallow update not allowed`, while the others go on, in atomic pushes as well, as
  in Git. Reached means reachable from the new tip, without going past the boundary and without counting what any reference
  reaches: `HEAD`, hidden references and those of other namespaces too. A boundary commit that the receiver has isn't needed if
  a reference reaches it, and one that it doesn't have is when the pack brings it. A rejected command is told
  `missing necessary objects` instead if it doesn't connect on its own, as in Git. Differences:
  * Lines of the client that aren't commits are ignored. Git ignores blobs, and crashes for a tree that the receiver has, and
    rejects a push with `invalid new value provided` for a tree that the pack brings.
  * Trailing characters after the hex id of a `shallow` line are refused, as in the advertisement, where Git ignores them.
* With `receive.shallowUpdate=true` the commits of the boundary that a command needs are added to the `shallow` file when that
  command is updated, after the `update` hook and before its reference is queued: the file is locked, written with what it had
  sorted and what the command needs after it in the order the client said them, and kept if the command is connected with it, or
  else the command is told `shallow error` and the file stays as it was. What references reach is looked at once, when the first
  command is updated, after the deletions of a push that isn't atomic. Hooks, including `reference-transaction`, are told
  about a temporary `shallow_XXXXXX` file in the Git directory with `GIT_SHALLOW_FILE` when something that the client said is
  there, which has what the receiver has sorted and then every line of the client as it was sent, and which stays until the
  hooks that follow the report ran. Differences:
  * Git 2.56 passes the command where a pointer to it is expected in `update_shallow_ref()`, so that it checks the command that
    follows, or none, and drops all that follow. A push of two new branches where the first needs the boundary ends with
    `remote failed to report status` for the second, and a single command whose pack lacks a tree of the boundary is accepted
    and leaves a corrupt repository. Each command is checked and told here. That is not what Git does, which is why these rows
    assert the outcome of the receiver and the others compare what Git does for the cases it handles as meant: the command that
    needs the boundary is the last one, or the only one.
  * Git writes an empty `shallow` file where a command that reaches the boundary through a path that references reach as well takes
    nothing into it, which makes the repository shallow without a commit. The receiver doesn't write the file then, and doesn't
    rewrite one that was there to sort it.
  * Git dies if the file has changed since it read it, or is locked, without telling the client what happened. The command is
    rejected with `shallow error` and a message here.
  * The new file doesn't have the permissions of `core.sharedRepository`.
* The post-receive and post-update hooks are skipped for reference names that can't be represented on Windows, as in Git.
* The `reference-transaction` hook runs in the Git directory with `GIT_DIR` set, like the other receive-side hooks, for the
  one transaction of an atomic push or the two of a push without atomicity, where deletions come first. A refusal in the
  `preparing` or `prepared` state ends the receiver without a report, as `die()` does in Git: the client gets what was said
  before, and nothing after, and no hook that follows runs. What the hook prints isn't relayed to the client in either:
  Git runs it outside of the side band, so its output goes to the standard error of the receiver. That is deliberate.
  The receiver releases the `.keep` file of the pack on the way out, which Git leaves behind.
* Observers learn of rejected updates in the order of Git, with updates that go through symbolic references last. The
  lookup of symbolic references doesn't hold locks, so under a concurrent change of such a reference the order may differ.

It refuses configurations and repositories it can't handle yet, before advertising anything, and classifies the refusal as
unsupported: `updateInstead` for a work tree that needs more than the index to be rewritten, signed pushes, `proc-receive`, partial repositories,
`core.alternateRefs*`, `transfer.advertiseSID=true` and `receive.updateServerInfo=true`, repositories that aren't fully
trusted, as their hooks wouldn't be seen, and every `extensions.*` other than the object format, the reference storage,
`worktreeConfig`, `noop` and `preciousObjects`.

### `updateInstead`

`gix/tests/gix/receive/update_instead.rs` pushes to a branch that a work tree has checked out, with `receive.denyCurrentBranch=updateInstead`,
and compares the report, the references, the index and every file of the work tree with what Git 2.56 does, in both hash kinds
and both reference formats. Each row says what Git tells the client, so that none passes by failing for another reason. Git runs
`update-index --refresh`, `diff-files`, `diff-index --cached` and `read-tree -u -m` for it, after the `update` hook and before the
reference is queued. The receiver does what they do without running them, and has the same outcome for what is tested: changed,
staged, intent-to-add, deleted and unmerged paths, untracked files in the way, a branch that doesn't exist yet, linked work
trees, a locked index, submodules, `core.filemode`, `core.symlinks`, line endings, a version 4 index, and a work tree that is
updated although a later command of an atomic push fails. What differs:

| Case | Git | The receiver |
| --- | --- | --- |
| An index entry marked skip-worktree or assume-unchanged, `core.sparseCheckout`, `core.ignoreCase`, `core.precomposeUnicode`, `submodule.recurse`, a file system monitor, or a smudge or process filter | Takes them into account | Declines the push, or refuses to receive if the setting is known up front |
| A tree with one path twice, as a file and as a directory, or out of order | Goes ahead | Declines, as an index can't represent it. Paths that `verify_path()` refuses, like `.git` in any case or `..`, are declined by both |
| A `push-to-checkout` hook | Takes the place of the four commands, and is called with the new commit in `GIT_DIR` and `GIT_WORK_TREE` of the work tree, from the Git directory of the receiving repository, without input | The same, down to the `/.` at the end of `GIT_DIR` of the main work tree. A hook that can't be started, or isn't executable, is no hook, and the checks run, as in Git, and one that can't be started ends the run so that the hooks after it aren't started, and declines if a hook ran before it. A configured hook and a hook file both run, and either one declines. What it prints is a diagnostic, like the output of the four commands |
| A namespace | Looks the work tree up by the name without the namespace | Refuses to receive |
| A push to a symbolic reference that leads to the checked out branch | Looks for the work tree by the pushed name, finds none, and leaves the index and files behind the moved branch | Follows the reference, as its deny rules do, and updates the work tree |
| A checked out branch while a rebase or bisect is in progress | Checks the new commit out into the work tree in the middle of it | Refuses with `Could not update working tree to new HEAD`, as `HEAD` isn't at the branch. No test covers this |
| What Git prints on the standard error of the receiver, like the file that is in the way | Reaches the pushing user's terminal over `file://` and `ssh` | Is a diagnostic, which `NativeLocal` doesn't forward to anything |
| What the four commands print | Standard error of the receiver | Diagnostics of the receiver only, the client sees the reason of `ng` as with Git |
| An index that was refreshed before the work tree was found to differ | Writes the refreshed index | Leaves the index alone |
| The cache tree of the index | Primed by `read-tree` | Not written, as no extensions are |
| Version 4 index | Keeps the version | Writes version 2 or 3 |
| A filter that expands `$Id$` | `$Id: <hex> $` | `$Id: <hex>$`, as `gix-filter` does everywhere |

The work tree is not rolled back if a later command of the push fails, which is what Git does, as it updates the work tree when
its command is processed. Untracked files that are ignored don't stop the update and are replaced, like in Git.

## Maintenance

What Git 2.56 does that the fork's `gc()` and `run_maintenance()` follow or deliberately differ from. The fork's tests
compare `git gc`, `git gc --auto` and `git maintenance run` with the native code in repositories of both hashes and both
reference formats (`gix/tests/gix/repository/gc.rs`). Statements marked *(source)* come from reading Git's `builtin/gc.c`
and `odb/source-files.c` and were not run.

| Case | Git | The fork |
| --- | --- | --- |
| Reflog ages with nothing configured | The documentation says 90 days, or 30 if unreachable. The code sets 30 and 90, so everything older than 30 days expires | Removes an entry only if the documented and the actual rule both would, which is the documented one, and says so in the module documentation. Configuring `gc.reflogExpire` and `gc.reflogExpireUnreachable` removes the difference |
| `maintenance.auto=false` and `maintenance run --auto` | Caller commands like `receive-pack` and `commit` consult `maintenance.auto`, falling back to `gc.auto > 0`, before they start it. `maintenance run --auto` itself doesn't *(source)* | `Repository::auto_maintenance()` answers for the callers, `run_maintenance()` ignores it |
| Taking `gc.pid` | Creates `gc.pid.lock`, reads `gc.pid` under it (ignored after 12 hours, or if the process on this host is gone), writes the lock, renames it. Dies if `gc.pid.lock` exists *(source)* | The same sequence, and an existing `gc.pid.lock` skips an automatic run and fails an explicit one. The process check is only made on Linux |
| `objects/maintenance.lock` | Has no age limit. A run finding it skips quietly with `--auto` *(source)* | The same, and `gc()` takes it too, which Git's `gc` doesn't, so a lock that a killed `git maintenance` left also blocks `gc()`. `force` doesn't override it |
| Which tasks a manual `maintenance run` has | `maintenance.strategy`, `geometric` by default: `commit-graph`, `geometric-repack`, `pack-refs`, `rerere-gc`, `reflog-expire`, `worktree-prune`, then `maintenance.<task>.enabled` *(source)* | The same selection. `commit-graph`, `rerere-gc` and `worktree-prune`, and `prefetch` and `incremental-repack`, are reported as unsupported |
| Are there too many loose objects | Counts those in `objects/17` and compares with `gc.auto` divided by 256, rounded up *(source)* | The same estimate |
| `geometric-repack` when all packs would be rolled up | Does a full `repack -a -d --cruft` with `gc.pruneExpire` instead, which is the only place where the default strategy expires unreachable objects *(source, and ran)* | The same, with `now` meaning that they are dropped |
| `pre-auto-gc` | Runs it for `gc --auto`, and a second time inside the `gc` that `maintenance run --auto` starts *(source)* | Runs it once |
| Index files without a pack | Removes all files of that name, including `.keep` *(source)*. Names that are not known, like the fork's `*.idx.removing`, stay | The same |
| Maintenance after a push | `receive-pack` starts `maintenance run --auto` after the report, and with `maintenance.autoDetach` or `gc.autoDetach` at the default `true` it detaches, so the client doesn't wait *(source)* | The native local transport runs it in the server thread, after the report and the post hooks, and never detaches. The client of a push reads the server's output until it ends, so it waits for the whole run, as it does with `autoDetach=false` in Git (measured: a `pre-auto-gc` hook of 12 seconds delayed the push by 12 seconds). The reason is that honouring the setting would need the receiver to end its output before it starts maintenance, which changes how the receiver and the transport are structured, and the run is kept in the server's thread so that a dropped transport can interrupt it and its locks are released. A caller that wants Git's detaching sets `receive.autogc=false` and runs `Repository::auto_maintenance()` and `run_maintenance()` itself |

## The fork's upload server

`gix/tests/gix/upload/` runs the same requests against Git 2.56 `upload-pack` and `gix::upload`, for both hash kinds and both
reference formats, with Git in the C locale. `ls-refs` is compared line by line, `fetch` is compared by its acknowledgments
byte for byte and by what the pack lets a client do: a clone matches the objects Git sends exactly, and an incremental fetch
contains everything `rev-list --objects` lists, plus the edge commits and their root trees and nothing else, after which
`fsck --connectivity-only` of the client passes. The server differs from Git in these ways:

| Case | Git | The server |
| --- | --- | --- |
| A repository that isn't fully trusted | Serves it, as nothing it has is run | Refuses it as unsupported, as the receiver does |
| A shallow repository, a shallow client, `deepen*`, `filter`, `wait-for-done`, `want-ref`, `sideband-all`, bundle URIs, `packObjectsHook` | Serves them, some only when configured | Doesn't advertise them, refuses the configuration that enables them, and answers a request that uses them with `ERR` |
| Protocol versions 0 and 1 | Serves them | Refuses them when started in the process |
| Packs that depend on objects only the client has | Sends thin packs when asked | Sends none, so a pack is always self-contained and may be larger |
| The `symref-target` of a symbolic reference that leads outside the namespace | Sends `(null)` | Sends the full name |
| The order of `ERR` messages for several missing wants | Reports them in the order of the wants | Reports the first one it finds |
| An object that can't be read once the pack has started | Not compared | Sends an error on band 3 and ends the stream |

`include-tag` takes the annotated tags from `refs/tags/` of the whole repository, ignoring the namespace and `hideRefs`,
because Git does, and the server does the same on purpose, with a test that pins it.

## Object checking on receive

`receive.fsckObjects` or else `transfer.fsckObjects` makes the receiver check every object of the pack it receives, as
`unpack-objects --strict` does for packs with fewer entries than the unpack limit and `index-pack --strict` does for the others,
and refuse the push as `receive-pack` does: the status says `unpack unpack-objects abnormal exit` or `unpack index-pack abnormal exit`
and `ng <ref> unpacker error`, after the lines Git prints about the objects. `receive.fsck.<msg-id>` and `receive.fsck.skipList`
configure severities and objects to skip. The plain `fsck.<msg-id>` and `fsck.skipList` are for `git fsck` and don't apply, as in Git.
The checks are those of `gix-fsck`, which has Git's 76 message ids. Everything is checked before the quarantine is migrated, a reference
changes, a loose object is written or a `.keep` file is made.

Ran, with Git 2.56 `receive-pack` as the oracle (`gix/tests/gix/receive/fsck.rs`, SHA-1 and SHA-256): the report to the client, the lines
people see, the references, the objects and what is left behind are the same, in 10 tests: flaws in commits, trees,
tags and `.gitmodules`, on both paths and with unpack limits of 0, 1, 2 and 100, packs of more than 100 objects, `receive.fsckObjects`
against `transfer.fsckObjects`, overrides and skip lists, a thin pack on a base with an old flaw (accepted), the flawed tree sent again
(refused), warnings of accepted pushes, commits with flawed identities that are accepted when nothing checks them or the flaw is ignored, and a client and a receiver that are shallow. The checker itself is compared on thousands of objects in `gix-fsck`.

Measured (one 8-core host, load about 2, release build, `receive.fsckObjects` toggled): a push of the whole history of this
repository (222,226 objects, 379 MB pack) into an empty repository took:

| | Git 2.56 `receive-pack` | the receiver |
| --- | --- | --- |
| without checking | 8.67 s | 5.62 s |
| with checking | 8.98 s (+3.6%) | 6.16 s (+9.7%) |

The receiver decodes the objects a second time to check them, and only commits, trees and tags, as nothing is checked about other
blobs, while Git checks them as `index-pack` resolves them. Decoding every object without a cache added 3.9 s, and with a delta cache 2.2 s.

What a new tree links to that isn't new is looked at by its header only, which matters for pushes that add to a repository
as most do. The same host, a push that changes one file of a directory of 600 files of 200 KB (a 154 KB request) into a repository
that has the rest took 0.01 s without checking and 0.01 s with it, where Git needs 0.02 s and 0.03 s. Reading what is linked
completely, as it first did, took 0.27 s, which is what to look out for. A push of the last 10 commits of this repository onto what
precedes them took 0.03 s and 0.03 s (Git: 0.05 s and 0.05 s).

Differences from Git:

* The check isn't run in a child process, so a failure to check, like a skip list that can't be read, is a failure to receive, including for a push that only deletes references, which Git accepts as it has no objects to run its program on.
* A configuration that Git rejects when it reads it (an invalid severity, `receive.fsckObjects` that isn't a boolean) fails every push in
  both, also if objects aren't checked; a skip list that can't be read and a severity for a message that can't be lowered only matter
  when objects are checked, in both.
* `.gitmodules` is parsed the way the checked-out file is read.

## Object checking on fetch and clone

`fetch.fsckObjects` or else `transfer.fsckObjects` makes a fetch or clone check every object of the pack it receives, as `git fetch`
does by running `index-pack --strict`, whatever the number of objects, and refuse it if one fails. `fetch.fsck.<msg-id>` and
`fetch.fsck.skipList` configure severities and objects to skip. The plain `fsck.<msg-id>`, `receive.fsck.<msg-id>` and the others don't
apply. The pack is written to `objects/pack/tmp_pack_dir_*`, which is named like what `Repository::prune()` removes once it is older than
the expiry (`gc.pruneExpire`, two weeks by default), checked there and moved into `objects/pack` as `.keep` file, pack and index only if it
is accepted. The links are checked with the commits that the `shallow` file will have as the boundary, which the protocol computes once
and the check and the file both use. Refusing a fetch fails it before the references and the `shallow` file are touched, with an error that
carries what Git prints (`gix::remote::fetch::ObjectsRefused`). Warnings of an accepted pack are in `Outcome::object_warnings` and printed by the
`gix` command.

Ran, with Git 2.56 `git fetch` and `git clone` as the oracle (`gix/tests/gix/remote/fetch_fsck.rs`, SHA-1 and SHA-256): verdict, references,
`shallow` file, objects, and the lines about objects are the same for flaws in commits, trees, tags and `.gitmodules` (checked and not);
the settings and their order, overrides, warnings; a thin pack on a base with an old flaw (accepted); the same flawed tree sent again
(refused); shallow fetches at depths 1 and 2; partial clones; and clones, which leave no directory if refused. The native fetch
leaves no temporary file where Git leaves `tmp_pack_*`.

Measured (one 8-core host, load about 2, release build, `transfer.fsckObjects` toggled): a bare clone of this repository over `file://`
(222,226 objects, 369 MB):

| | Git 2.56 `clone` | the native clone |
| --- | --- | --- |
| without checking | 10.3 s | 5.5 s |
| with checking | 10.4 s (+1%) | 6.1 s (+10%) |

The ratio is worse than Git's for the reason it is for receiving: the objects are decoded a second time to check them, where Git checks
them as `index-pack` resolves them.

Differences from Git:

* A skip list that can't be read, and a severity that can't be applied, fail the fetch before the server is contacted, also when nothing
  would be received. Git reads the skip list when it starts to check.
* A clone of a local path is checked, where Git's local copy shortcut isn't. `file://` and `--no-local` are checked in both.
* In a repository that is a partial clone (`remote.<name>.promisor` or `extensions.partialClone`) the objects are checked one by one,
  with the same severities, and the objects they link to aren't required to be there, whichever remote is fetched from. Git does that
  only for a fetch that sends a filter, which `gix` doesn't, and requires them for a `git fetch <url>` without one.
* Where Git leaves the unchecked pack in `objects/pack/tmp_pack_*` when it refuses, nothing is left.
