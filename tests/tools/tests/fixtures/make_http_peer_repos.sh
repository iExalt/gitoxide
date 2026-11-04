#!/usr/bin/env bash
set -eu -o pipefail

# Repositories for serving them with the HTTP peer of `gix-testtools`, which has the object format as first argument.
#
# * `remote.git` is a bare repository with `main` at the second of two commits, and the tag `v1` on the first.
# * `work` is a repository that has a third commit on `main` that is not in `remote.git`, so pushing
#   `main` to `remote.git` is a fast-forward. Its `origin` is `remote.git`.
object_format="${1:-sha1}"

git init -q --bare --object-format="$object_format" remote.git
git init -q --object-format="$object_format" work
(
  cd work
  git checkout -q -b main
  echo one >file
  git add file
  git commit -q -m one
  git tag v1
  echo two >file
  git commit -q -am two
  git push -q ../remote.git main v1
  echo three >file
  git commit -q -am three
)
