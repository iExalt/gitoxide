#!/usr/bin/env bash
set -eu -o pipefail

# A history that tells ancestry questions apart: a line of commits, a branch that
# merges back, a criss-cross merge with two merge bases, a root that shares nothing
# with the rest, and a tag so that the commits can be named.
#
#   a - b - c ---- m1 ---- x1 - x2
#        \        /   \   /
#         d - e --+     y1
#   (root2, shares nothing)
git init -q
git config user.name committer
git config user.email committer@example.com

commit() {
  echo "$1" > "file-$1"
  git add "file-$1"
  GIT_COMMITTER_DATE="$((1700000000 + $(git rev-list --all --count) * 60)) +0000" \
  GIT_AUTHOR_DATE="$((1700000000 + $(git rev-list --all --count) * 60)) +0000" \
    git commit -q -m "$1"
  git tag "$1"
}

commit a
commit b
commit c
git checkout -q -b side b
commit d
commit e
git checkout -q main
git merge -q --no-ff -m m1 e
git tag m1

git checkout -q -b left m1
commit x1
git checkout -q -b right m1
commit y1
git checkout -q left
git merge -q --no-ff -m x2 y1
git tag x2
git checkout -q right
git merge -q --no-ff -m y2 x1
git tag y2

git checkout -q --orphan unrelated
git rm -q -rf . >/dev/null
commit root2
git checkout -q main
