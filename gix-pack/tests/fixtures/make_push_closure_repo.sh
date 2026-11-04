#!/usr/bin/env bash
set -eu -o pipefail

# A repository to test which objects a receiver needs, with history that is packed, and newer history that is loose.
#
# Its argument is the object format, `sha1` or `sha256`. The repository is `repo`.
#
# * `b1`…`b5` modify a large file a little each time, and `git repack` turns them into a pack with offset deltas,
#   which makes copying entries from packs interesting. The annotated tag `v-have` is on `b3`.
# * Everything after is loose: `c1` adds `dir/new` with the same content as `dir/y`, so one blob is in two trees,
#   `side` and the merge `m` of it with `main`, and `g` with a submodule entry that has no objects.
# * Tags: `v1` is annotated on `m`, `v1-outer` is a tag on `v1`, `tree-tag` is on the tree of `m`, `blob-tag` is on
#   the blob of `dir/new`, and `light` is a lightweight tag on `c1`.
format=${1:?the object format}

git init -q --object-format="$format" -b main repo
cd repo

seq 1 400 >big.txt
mkdir dir
echo y >dir/y
echo x >dir/x
git add .
git commit -qm b1
for n in 2 3 4 5; do
  sed -i.bak "$((n * 50))s/.*/changed in b$n/" big.txt
  rm big.txt.bak
  git commit -qam "b$n"
  git tag "b$n"
  if [ "$n" = 3 ]; then git tag -a -m "v-have" v-have; fi
done
git tag b1 main~4
git repack -q -adf --window=50 --depth=50

echo changed >dir/x
cp dir/y dir/new
git add .
git commit -qm c1
git tag light

git switch -q -c side b5
echo side >side.txt
git add side.txt
git commit -qm side
git switch -q main
git merge -q --no-ff -m m side
git tag -a -m v1 v1 main
git tag -a -m v1-outer v1-outer v1
git tag -a -m tree-tag tree-tag "$(git rev-parse 'main^{tree}')"
git tag -a -m blob-tag blob-tag "$(git rev-parse main:dir/new)"

git update-index --add --cacheinfo "160000,$(git rev-parse b5),sub"
git commit -qm g
git tag g
