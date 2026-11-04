#!/usr/bin/env bash
set -eu -o pipefail

# A repository with everything that keeps an object alive, and some that doesn't, for `gix`'s tests of finding reachable objects,
# with `git rev-list --objects --all --reflog --indexed-objects` as the oracle. The argument is the object format (`sha1`
# or `sha256`), and `repo` is the main worktree.
#
# The file that only the named way reaches:
#
# * `branch.txt` and `tagged.txt`, on the branches `main` and `feature`, where `feature` also has an annotated tag `v1`, which a
#   walk has to follow from the tag to its commit. The tags `tree-tag` and `blob-tag` point to a tree with `tree-tag.txt`
#   and to a blob with the contents of `blob-tag.txt`.
# * `reflog.txt`, in a commit on a branch that was deleted, which only the reflog of `HEAD` remembers.
# * `stash.txt`, in `refs/stash`.
# * `staged.txt`, which is only in the index, and `cache-tree.txt`, whose tree only the cache-tree extension of the index of
#   the main worktree remembers, as it was written without a commit and the file was unstaged again.
# * The blob that `conflict.txt` had on their side of the conflict that was resolved, which only the resolve-undo extension of
#   the index remembers. The commit it came from is `MERGE_HEAD`, which isn't a root, as the branch that had it was
#   deleted before any reflog could mention it. `their-side.txt` was merged without conflict, so it's in the index.
# * `w1-branch.txt` on the branch of the linked worktree `w1`, `w1-staged.txt` in its index, and `w2-head.txt` in a commit only
#   the detached `HEAD` of the linked worktree `w2` points to.
# * The tip of `feature` is replaced by a commit that only has `replaced.txt`. With the replacement in effect, the files of
#   `feature` are out of reach, so it must not be followed.
#
# And what doesn't keep an object: `dangling.txt` is in a blob and a commit that nothing refers to.
format=${1:?the object format to use}

git init -q --object-format="$format" -b main repo
cd repo

commit() {
  git add -A
  git commit -q -m "$1"
}

echo base >shared.txt
echo "conflict base" >conflict.txt
commit base
echo ours >conflict.txt
echo branch >branch.txt
commit "main one"

git checkout -q -b feature HEAD~1
echo tagged >tagged.txt
commit "feature one"
git tag -a -m "annotated" v1
git checkout -q main

echo tree-tag >tree-tag.txt
git add tree-tag.txt
git tag tree-tag "$(git write-tree)"
git reset -q
rm tree-tag.txt
echo blob-tag >blob-tag.txt
git tag blob-tag "$(git hash-object -w blob-tag.txt)"
rm blob-tag.txt

# only the reflog of `HEAD` knows about this commit
git checkout -q -b gone
echo reflog >reflog.txt
commit "reflog only"
git checkout -q main
git branch -q -D gone

echo stash >stash.txt
git add stash.txt
git stash -q

echo dangling >dangling.txt
git hash-object -w dangling.txt >/dev/null
git commit-tree -m dangling "$(git rev-parse 'HEAD^{tree}')" >/dev/null
rm dangling.txt

# a conflict that was resolved, but is remembered as resolve-undo, and a merge that is still going on
theirs_index=$(mktemp)
GIT_INDEX_FILE=$theirs_index git read-tree HEAD~1
GIT_INDEX_FILE=$theirs_index git update-index --add --cacheinfo "100644,$(echo theirs | git hash-object -w --stdin),conflict.txt"
GIT_INDEX_FILE=$theirs_index git update-index --add --cacheinfo "100644,$(echo their-side | git hash-object -w --stdin),their-side.txt"
theirs_commit=$(git commit-tree -p "$(git rev-parse HEAD~1)" -m theirs "$(GIT_INDEX_FILE=$theirs_index git write-tree)")
rm "$theirs_index"
git update-ref refs/heads/side "$theirs_commit"
git merge side >/dev/null 2>&1 || true
echo resolved >conflict.txt
git add conflict.txt
git update-ref -d refs/heads/side

# `staged.txt` is only in the index, and the cache-tree remembers a tree with `cache-tree.txt` that no commit has
echo staged >staged.txt
git add staged.txt
echo "never committed" >cache-tree.txt
git add cache-tree.txt
git write-tree >/dev/null
git reset -q cache-tree.txt
git write-tree >/dev/null
rm cache-tree.txt

git worktree add -q ../w1 -b w1-branch
(
  cd ../w1
  echo w1 >w1-branch.txt
  git add w1-branch.txt
  git commit -q -m "w1 branch"
  echo w1-staged >w1-staged.txt
  git add w1-staged.txt
)
git worktree add -q --detach ../w2 main
(
  cd ../w2
  echo w2 >w2-head.txt
  git add w2-head.txt
  git commit -q -m "w2 head"
)

# the replacement of the tip of `feature` comes last as it affects everything that follows it
index=$(mktemp)
blob=$(echo replaced | git hash-object -w --stdin)
GIT_INDEX_FILE=$index git read-tree main
GIT_INDEX_FILE=$index git update-index --add --cacheinfo "100644,$blob,replaced.txt"
replacement=$(git commit-tree -m "replaces the tip of feature" "$(GIT_INDEX_FILE=$index git write-tree)")
rm "$index"
git update-ref "refs/replace/$(git rev-parse feature)" "$replacement"
