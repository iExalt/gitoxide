#!/usr/bin/env bash
set -eu -o pipefail

# Repositories to serve from, for the tests of `gix::upload` which compare what the fork serves with `git upload-pack`.
# The arguments are the object format (`sha1` or `sha256`) and the format of references (`files` or `reftable`) of all repositories.
#
# * `remote.git` is bare. `main` has "one" and "two" on top of it, `side` has "side" on top of "one", and `merged` has a merge of
#   `main` and `side` on top of both, so that a client which has `side` and wants `merged` shows how merges are handled.
#   `light` is a lightweight tag on "one", `v1` an annotated tag on "two", and `refs/remotes/origin/HEAD` is a symbolic
#   reference to `refs/remotes/origin/main`. `HEAD` points to `main`.
#   `orphan` is a root commit that has nothing in common with the rest, which negotiation can't find a common commit for.
# * `old.git` is a bare clone of `remote.git` that was made when it had `main` at "one" only, which is what a client that
#   fetches incrementally has.
# * `unborn.git` is bare and has no references and no objects, so its `HEAD` points to `main` that doesn't exist.
# * `unborn-head.git` has references, but `HEAD` points to `trunk` that doesn't exist.
# * `detached.git` has a `HEAD` that points to a commit directly.
# * `namespaced.git` has the references of `remote.git` in the namespace `team` with a `HEAD` for it, and `team-unborn`
#   with a `HEAD` that points to a branch that doesn't exist, and `refs/heads/outside` that is in no namespace. It has the
#   annotated tag `global-tag` outside of all namespaces and `ns-tag` in `team`, both on `main`, which show that clients that
#   ask for `include-tag` get what the tags of the repository as a whole point to, as Git does.
# * `broken-blob.git` is a copy of `remote.git` in which a blob that `main` needs was removed.
object_format=${1:?the object format to use}
ref_format=${2:?the format of references to use}

init() {
  git init -q --bare --object-format="$object_format" --ref-format="$ref_format" "$1"
}

git init -q --object-format="$object_format" --ref-format="$ref_format" -b main source
(
  cd source
  # Every commit is a day younger than the one before, so that the date cutoff of negotiation has something to cut off.
  tick=0
  commit() {
    tick=$((tick + 1))
    export GIT_AUTHOR_DATE="@$((1700000000 + tick * 86400)) +0000" GIT_COMMITTER_DATE="@$((1700000000 + tick * 86400)) +0000"
    echo "$1" >"$1"
    git add "$1"
    git commit -qm "$1"
  }
  commit one
  git tag light
  one=$(git rev-parse HEAD)
  git branch -q old-main
  commit two
  git tag -a -m annotated v1
  git tag -a -m global global-tag
  git tag -a -m namespaced ns-tag
  git switch -q -c side "$one"
  commit side
  git switch -q -c merged main
  git merge -q --no-ff -m merged side
  git switch -q --orphan orphan
  commit orphan
  git switch -q main
)

init remote.git
init old.git
(
  cd source
  git push -q ../old.git old-main:refs/heads/main
  git push -q ../remote.git main side merged orphan light v1
)
git -C remote.git symbolic-ref HEAD refs/heads/main
git -C old.git symbolic-ref HEAD refs/heads/main
git -C remote.git update-ref refs/remotes/origin/main "$(git -C remote.git rev-parse main)"
git -C remote.git symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

init unborn.git

init unborn-head.git
(cd source && git push -q ../unborn-head.git main)
git -C unborn-head.git symbolic-ref HEAD refs/heads/trunk

init detached.git
(cd source && git push -q ../detached.git main)
git -C detached.git update-ref --no-deref HEAD "$(git -C detached.git rev-parse main)"

init namespaced.git
(
  cd source
  git push -q ../namespaced.git main:refs/namespaces/team/refs/heads/main side:refs/namespaces/team/refs/heads/side \
    v1:refs/namespaces/team/refs/tags/v1 main:refs/heads/outside main:refs/namespaces/team-unborn/refs/heads/main \
    global-tag:refs/tags/global-tag ns-tag:refs/namespaces/team/refs/tags/ns-tag
)
git -C namespaced.git symbolic-ref refs/namespaces/team/HEAD refs/namespaces/team/refs/heads/main
git -C namespaced.git symbolic-ref refs/namespaces/team-unborn/HEAD refs/namespaces/team-unborn/refs/heads/nothing
git -C namespaced.git symbolic-ref HEAD refs/heads/outside

init broken-blob.git
(cd source && git push -q ../broken-blob.git main)
# The push was unpacked into loose objects, so the blob of "two" can be removed on its own.
blob=$(git -C source rev-parse main:two)
rm "broken-blob.git/objects/${blob:0:2}/${blob:2}"
