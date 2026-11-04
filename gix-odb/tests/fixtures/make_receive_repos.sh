#!/usr/bin/env bash
set -eu -o pipefail

# Repositories for receiving packs into a quarantine, so the result can be compared with that of `git receive-pack`.
# The object format comes from `GIT_DEFAULT_HASH`.
#
# * `source`         has `main` with the commits "one", "two" and "three". Each changes `data`, which has many similar
#                    lines so that Git stores later versions as deltas of earlier ones. Tests pack what `target.git` lacks.
# * `target.git`    is bare and has "one" only, as a pack. Packing "two" and "three" with `--thin` produces ref-deltas
#                    against objects that only this repository has.
# * `base-alternate.git` is bare and has "one" only. `thin-alternate.git` is bare and has nothing but an
#                    alternates file pointing to it with a relative path, so its base objects live in an alternate only.
#                    The path is relative to survive copies of the fixture.
# * `target-loose.git` is like `target.git`, but with the objects of "one" being loose objects.
git init -q -b main source
(
  cd source
  for line in $(seq 1 200); do echo "line $line of a file that changes a little with each commit"; done >data
  git add data
  git commit -qm one
  git tag one
  sed -i 's/^line 17 .*/line 17 was changed in two/' data
  echo two >two
  git add data two
  git commit -qm two
  git tag two
  sed -i 's/^line 99 .*/line 99 was changed in three/' data
  git commit -qam three
  git tag three
)

git init -q --bare target.git
(cd source && git push -q ../target.git one:refs/heads/main && cd ../target.git && git repack -adq && git prune-packed)

git init -q --bare base-alternate.git
(cd source && git push -q ../base-alternate.git one:refs/heads/main)
git init -q --bare thin-alternate.git
echo ../../base-alternate.git/objects >thin-alternate.git/objects/info/alternates

git init -q --bare target-loose.git
(cd source && git push -q ../target-loose.git one:refs/heads/main)
