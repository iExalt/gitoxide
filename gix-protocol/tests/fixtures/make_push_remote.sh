#!/usr/bin/env bash
set -eu -o pipefail

# Remotes for push tests, to be mutated by pushing to them, and the repository they were seeded from.
#
# * `source`  has `main` at commit "two", the tag `victim` on commit "one", and the branch `other` with
#             commit "three" that no remote has. Its objects are what tests send.
# * `remote.git` is bare, with `main` at "two" and `refs/tags/victim` on "one", but without `other`.
#             Pushing to it requires `refs/heads/` to be a plain directory to be made read-only.
# * `empty.git` is bare and has no objects or references at all, which makes it advertise
#             `capabilities^{}` instead of references.
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
  git switch -q -c other
  echo three >other
  git add other
  git commit -qm three
  git switch -q main
)

git init -q --bare --object-format="$format" remote.git
(
  cd source
  git push -q ../remote.git main refs/tags/victim
)
git init -q --bare --object-format="$format" empty.git
