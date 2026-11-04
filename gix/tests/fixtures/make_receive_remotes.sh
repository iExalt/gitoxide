#!/usr/bin/env bash
set -eu -o pipefail

# Bare remotes and a client with what to push to them, for comparing what `git receive-pack` and the native receiver do with the same push.
# The hash is the one of the fixture environment. There is one directory for each ref storage format:
#
# * `remote.git` is bare with `main` at "c2" on top of "c1", and `side` at "c1".
# * `client` has those as well, and more that the remote doesn't have:
#   `next` at "c4" on top of "c3" on top of "c2", `diverge` at "d1" on top of "c1" which isn't a fast-forward of `main`,
#   and the annotated tag `v1` at "c2".
# * `work` is a clone of `remote.git` with a worktree where `main` is checked out.
export GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@example.com GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@example.com

commit() { # name time: an empty commit
  GIT_AUTHOR_DATE="$2 +0000" GIT_COMMITTER_DATE="$2 +0000" git commit -q --allow-empty -m "$1"
}

for format in files reftable; do
  mkdir "$format"
  (
    cd "$format"
    git init -q --bare --ref-format="$format" -b main remote.git
    git init -q --ref-format="$format" -b main client
    cd client
    echo one >file && git add file && commit c1 1700000100
    echo two >file && git add file && commit c2 1700000200
    git branch side main~1
    git push -q ../remote.git main side
    git tag -a -m v1 v1 main
    git checkout -q -b next
    echo three >file3 && git add file3 && commit c3 1700000300
    commit c4 1700000400
    git checkout -q -b diverge main~1
    echo diverge >other && git add other && commit d1 1700000500
    git checkout -q main
  )
  # `work` has a worktree with `main` checked out, which makes it special for what can be pushed to `main`.
  git clone -q --no-hardlinks --ref-format="$format" "$format/remote.git" "$format/work" 2>/dev/null
done
