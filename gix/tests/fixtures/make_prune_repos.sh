#!/usr/bin/env bash
set -eu -o pipefail

# Repositories to fetch from and into, for `gix`'s tests of pruning, which compare the fork's `fetch` with `git fetch --prune`.
# The argument is the object format (`sha1` or `sha256`) of all repositories.
#
# * `upstream` has the branches `main` at "three", `b1` and `b2` at "one", and `x/y` at "one", and the tags `t1` (lightweight)
#              and `v1` (annotated), both on "one".
# * `client` has fetched `refs/heads/*` of `upstream` as it was before "three" was added, into the private namespace
#            `refs/p/heads/*`, so it needs a pack to catch up, and tags into `refs/tags/*`. It doesn't have `refs/p/heads/x/y`
#            though, so that creating it is free to be in the way of a file at `refs/p/heads/x`. On top of that, it has what a
#            prune has to tell apart:
#            - `refs/p/heads/gone` and `refs/p/heads/sub/dir`, which the remote never had, inside the namespace,
#            - `refs/p/other/keep` and the tag `refs/tags/local-only`, outside of anything the specs map to,
#            - `refs/p/heads/alias`, a symbolic reference to `refs/p/heads/main`, and `refs/p/heads/dangling`, a symbolic
#              reference to the branch `refs/p/heads/b2`, which tests delete from `upstream` to make it stale.
format=${1:?the object format to use}

git init -q --object-format="$format" -b main upstream
(
  cd upstream
  commit() {
    echo "$1" >"$1"
    git add "$1"
    git commit -qm "$1"
  }
  commit one
  git branch b1
  git branch b2
  git branch x/y
  git tag t1
  git tag -a -m "annotated" v1
  commit two
  git checkout -q --detach
  git checkout -q main
)

git init -q --object-format="$format" -b main client
(
  cd client
  git fetch -q ../upstream '+refs/heads/*:refs/p/heads/*' '+refs/tags/*:refs/tags/*'
  git update-ref -d refs/p/heads/x/y
  git update-ref refs/p/heads/gone refs/p/heads/main
  git update-ref refs/p/heads/sub/dir refs/p/heads/main
  git update-ref refs/p/other/keep refs/p/heads/main
  git update-ref refs/tags/local-only refs/p/heads/main
  git symbolic-ref refs/p/heads/alias refs/p/heads/main
  git symbolic-ref refs/p/heads/dangling refs/p/heads/b2
)

(
  cd upstream
  echo three >three
  git add three
  git commit -qm three
)
