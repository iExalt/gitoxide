#!/usr/bin/env bash
set -eu -o pipefail

# Repositories to receive pushes, for comparing what the fork's receiving side does with what `git receive-pack` does.
#
# * `source`        has `main` at commit "two" with the annotated tag `v1` and the tag `victim` on "one", and the branch
#                   `other` with commit "three" that no remote has. Its objects are what tests send.
# * `remote.git`    is bare, with `main`, `victim` and `v1` of `source`, and without `other`.
# * `empty.git`     is bare and has no objects or references at all, so it advertises `capabilities^{}`.
# * `shallow.git`   is a bare clone of `source` with a depth of one, so it has a shallow boundary which it advertises.
# * `shallow-client` is a non-bare clone of `source` with a depth of one. Pushing from it sends `shallow` lines.
#
# The argument is the object format (`sha1` or `sha256`) to create all repositories with.
format=${1:?the object format to use}

git init -q --object-format="$format" -b main source
(
  cd source
  echo one >file
  git add file
  git commit -qm one
  git tag victim
  echo two >file
  git commit -qam two
  git tag -a -m annotated v1
  git switch -q -c other
  echo three >other
  git add other
  git commit -qm three
  git switch -q main
)

git init -q --bare --object-format="$format" remote.git
(
  cd source
  git push -q ../remote.git main victim v1
)
git init -q --bare --object-format="$format" empty.git
git clone -q --bare --depth 1 "file://$PWD/source" shallow.git
git clone -q --depth 1 "file://$PWD/source" shallow-client
