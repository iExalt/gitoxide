#!/usr/bin/env bash
set -eu -o pipefail

# Repositories to fetch from and push to over a synthetic network peer. The object format is the first argument.
#
# * `remote.git` is a bare repository with `main` at the second of two commits, and the tag `v1` on the first.
# * `work` has a third commit on `main` that `remote.git` doesn't have, so pushing `main` is a fast-forward.
#   It is the repository that clients operate in, with no remote configured: tests name the URL of the peer.
# * `dest.git` is an empty bare repository to fetch into, with the same object format.
object_format="${1:-sha1}"

git init -q --bare --object-format="$object_format" remote.git
git init -q --bare --object-format="$object_format" dest.git
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
