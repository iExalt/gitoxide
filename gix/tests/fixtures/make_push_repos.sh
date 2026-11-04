#!/usr/bin/env bash
set -eu -o pipefail

# Repositories to push from and to, for `gix`'s push tests, which compare the fork's push with `git push`.
# The argument is the object format (`sha1` or `sha256`) of all repositories.
#
# * `source` has `main` at "three", on top of "two" on top of "one"; the lightweight tag `light` on "one"; the
#             annotated tag `v1` on "one"; and the branch `side` with commit "side" on top of "one", which
#             is a history `main` doesn't contain, so moving `main` there isn't a fast-forward.
# * `remote.git` is bare, and has `main` at "two", `victim` and `hidden` at "one", `ahead` at "three" and the tag `light`
#             on "one". It doesn't have the commit "side". Tests mutate copies of it, or tell it to hide `hidden`.
# * `empty.git` is bare and has no objects at all.
# * `shallow-three` is a shallow clone of `main` with depth 1: its only commit, "three", is the shallow boundary. The remote's
#             `ahead` has it, so history on top of it can be sent as the boundary is hidden by what the remote advertises.
# * `shallow-side` is a shallow clone of `side` with depth 1: "side" is the boundary, and `remote.git` has no branch pointing to
#             it, so the missing history behind it can't be told to be present. Git doesn't send such history either.
format=${1:?the object format to use}

git init -q --object-format="$format" -b main source
(
  cd source
  commit() {
    echo "$1" >"$1"
    git add "$1"
    git commit -qm "$1"
  }
  commit one
  git tag light
  git tag -a -m "annotated" v1
  git branch victim
  git branch hidden
  commit two
  git branch behind
  commit three
  git branch ahead
  git switch -q -c side light
  commit side
  git switch -q main
)

git init -q --bare --object-format="$format" remote.git
(
  cd source
  git push -q ../remote.git behind:refs/heads/main victim:refs/heads/victim hidden:refs/heads/hidden ahead:refs/heads/ahead refs/tags/light
)
git init -q --bare --object-format="$format" empty.git

git clone -q --depth=1 --no-local --branch main "file://$PWD/source" shallow-three
git clone -q --depth=1 --no-local --branch side "file://$PWD/source" shallow-side
