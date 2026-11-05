#!/usr/bin/env bash
set -eu -o pipefail

# A repository whose objects are loose and in a pack at the same time, which is what `git repack` without `-d` leaves behind
# until `git prune-packed` removes the loose copies.
git init -q
git checkout -q -b main
echo hello >this
git add this
git commit -q -m c1
git repack -q
