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
| Reftable | one set of events, without nested ones |

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
