#!/usr/bin/env bash
set -eu -o pipefail

# Reftable repositories whose tables were written by Git, for reading them and for comparing what is written
# against what Git writes.
#
# usage: make_git_written_tables.sh <object-format> <branches>
#
# All repositories hold the same references: <branches> branches with shared and (for every 97th) very long names,
# annotated tags which have a peeled value, a symbolic reference, a reference with a name that is not valid UTF-8,
# and references which were deleted or re-created. Their reflogs have entries by a committer with non-ASCII bytes
# in the name, at a time zone with a non-zero minute offset.
#
# * `stacked`:   default settings, which includes the object index. As the references were written by many
#                transactions the stack has several tables, and the newest ones hold tombstones that hide
#                references of the oldest table.
# * `compacted`: `stacked` after `git pack-refs --all`, which merges everything into a single table.
# * `plain`:     one table written without the object index, which this crate doesn't write. It can be
#                reproduced byte for byte.
# * `deep`:      like `plain`, but with a block size small enough for a multi-level index of ref blocks.
#                Git refuses records that don't fit a block, so it can't be smaller than the longest name.

object_format=${1:?the object format}
branches=${2:?the number of branches to create}

populate() {
  local repo=$1 c1 c2 c3 tree i
  git init -q --ref-format=reftable --object-format="$object_format" -b main "$repo"
  (
    cd "$repo"
    git config core.logAllRefUpdates always
    shift
    for setting in "$@"; do git config "${setting%%=*}" "${setting#*=}"; done

    tree=$(git mktree </dev/null)
    c1=$(git commit-tree "$tree" -m one)
    c2=$(git commit-tree "$tree" -p "$c1" -m two)
    c3=$(git commit-tree "$tree" -p "$c2" -m three)

    git update-ref -m "initial" refs/heads/main "$c1"
    awk -v n="$branches" -v c="$c2" 'BEGIN {
      print "start"
      for (i = 1; i <= n; i++) {
        pad = (i % 97 == 0) ? "-long-component-" sprintf("%0180d", i) : ""
        printf "create refs/heads/team%02d/sub%d/feature-%d%s %s\n", i % 37, i % 5, i, pad, c
      }
      print "prepare"
      print "commit"
    }' | git update-ref -m "bulk create" --stdin >/dev/null
    git update-ref -m "advance" refs/heads/main "$c2" "$c1"
    GIT_COMMITTER_NAME='Zoë Ünï' GIT_COMMITTER_EMAIL='zoe@example.com' GIT_COMMITTER_DATE='1700001234 -0830' \
      git update-ref -m "advance again, with a committer who is not ASCII" refs/heads/main "$c3" "$c2"
    for i in 1 2 3; do git tag -a -m "tag $i" "v$i" "$c3"; done
    git update-ref refs/remotes/origin/main "$c3"
    git symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main
    git symbolic-ref refs/heads/alias refs/heads/main
    printf 'create refs/heads/caf\351\0%s\0' "$c1" | git update-ref -z --stdin

    # Deletions come last, so that their tombstones are in the newest tables of the stack.
    git update-ref -d refs/tags/v2
    git update-ref -m "re-create after delete" refs/tags/v2 "$c2"
    git update-ref -d refs/heads/team01/sub1/feature-1
    git update-ref refs/heads/scratch "$c3"
    git update-ref -d refs/heads/scratch
  )
}

populate stacked
if [ "$(wc -l <stacked/.git/reftable/tables.list)" -lt 3 ]; then
  echo "the fixture needs a stack of several tables, but has $(wc -l <stacked/.git/reftable/tables.list)" >&2
  exit 1
fi

cp -R stacked compacted
git -C compacted pack-refs --all

populate plain reftable.indexObjects=false
git -C plain pack-refs --all

populate deep reftable.indexObjects=false reftable.blockSize=512
git -C deep pack-refs --all
