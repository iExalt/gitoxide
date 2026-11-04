#!/usr/bin/env bash
set -eu -o pipefail

# A repository with a linked worktree, for tests of listing the references that have a reflog. The argument is the
# reference storage format, `files` or `reftable`.
#
# * `repo` has the branches `main`, `feature/x` and `deleted-branch`, the latter deleted so that its reflog is gone,
#   and reflogs of its own for `refs/stash`, and for the references `refs/bisect/main-side` and `refs/worktree/main-note`,
#   which are private to the main worktree. The reflog of `HEAD` is there as well.
# * `w1` is a linked worktree with its own `HEAD` reflog, and reflogs for `refs/bisect/w1-side` and `refs/worktree/w1-note`,
#   which only it has. It shares the branches of the main worktree.
format=${1:?the reference format to use}

git init -q --ref-format="$format" -b main repo
(
  cd repo
  git commit -q --allow-empty -m c1
  git commit -q --allow-empty -m c2
  git branch feature/x
  git branch deleted-branch
  git branch -q -D deleted-branch
  git update-ref --create-reflog -m stash refs/stash HEAD
  git update-ref --create-reflog -m side refs/bisect/main-side HEAD
  git update-ref --create-reflog -m note refs/worktree/main-note HEAD
  git worktree add -q ../w1 -b w1-branch
)
(
  cd w1
  git commit -q --allow-empty -m w1-c1
  git update-ref --create-reflog -m side refs/bisect/w1-side HEAD
  git update-ref --create-reflog -m note refs/worktree/w1-note HEAD
)
