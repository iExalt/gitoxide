#!/usr/bin/env bash
set -eu -o pipefail

# A repository for tests that watch a pack being streamed, which needs few objects with a lot of data.
#
# * `base` is the one commit a receiver is assumed to have.
# * `main` adds eight blobs of 1 MiB each to it, which is what a writer that holds more than a few objects
#   at a time shows. The blobs differ in their first line only.
format=${1:?the object format}

git init -q --object-format="$format" -b main repo
cd repo

echo base >base.txt
git add base.txt
git commit -qm base
git tag base

mkdir bulk
for n in $(seq 1 8); do
  { echo "$n"; head -c 1048576 /dev/zero; } >"bulk/f$n"
done
git add bulk
git commit -qm bulk
