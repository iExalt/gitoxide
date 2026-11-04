#!/usr/bin/env bash
set -eu -o pipefail

# A source repository with a commit that is more than 1 MiB of data that doesn't compress much, and a remote that
# has everything but that commit, to push it to.
#
# * `source` has `main` at the commit "big", on top of the commit "base" that is tagged `base`. "big" adds five files
#   with 400,000 pseudo-random printable characters each, from a seeded generator, so a pack with them is about 1.6 MiB.
# * `remote.git` is bare, and has `main` at "base".
#
# The argument is the object format (`sha1` or `sha256`) to create all repositories with.
format=${1:?the object format to use}

git init -q --object-format="$format" -b main source
(
  cd source
  echo base >base.txt
  git add base.txt
  git commit -qm base
  git tag base
  mkdir blobs
  for n in 1 2 3 4 5; do
    awk -v seed="$n" 'BEGIN { srand(seed); for (i = 0; i < 400000; i++) printf "%c", 33 + int(rand() * 90) }' >"blobs/f$n"
  done
  git add blobs
  git commit -qm big
)

git init -q --bare --object-format="$format" remote.git
(
  cd source
  git push -q ../remote.git base^{commit}:refs/heads/main
)
