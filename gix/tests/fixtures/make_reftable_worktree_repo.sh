#!/usr/bin/env bash
set -eu -o pipefail

# A repository whose references are stored in reftables written by Git, with a linked worktree that has references
# of its own, which makes Git use a second reftable stack for it: `repo` is the main worktree and `wt` a linked one
# that has its own `HEAD` (checked out to `wt-branch`) and `refs/worktree/private`.
# It only exists for versions of Git that can create reftables.

git init -q --ref-format=reftable -b main repo
cd repo
git commit -q --allow-empty -m c1
git commit -q --allow-empty -m c2
git worktree add -q ../wt -b wt-branch HEAD~1
(
  cd ../wt
  git update-ref refs/worktree/private "$(git rev-parse HEAD)"
)
