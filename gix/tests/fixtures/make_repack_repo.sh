#!/usr/bin/env bash
set -eu -o pipefail

# A repository with the packs and loose objects that `git repack` treats differently, for `gix`'s tests of repacking, with
# `git repack -A -d`, `-a -d` and `-d` 2.56 as the oracle. The argument is the object format (`sha1` or `sha256`), and
# `repo` is the repository. Dates are fixed, and the packs that are old have the time of the year 2001.
#
# * `main` has commits that were packed into the pack `old`, which has deltas, then more commits that are loose, and then
#   `kept` has commits that are in a pack with a `.keep` file, whose objects are not loose any more. `main` ends with commits that are loose.
# * `scratch` was a branch that is gone, and with it all reflogs. Its commit, tree and blobs are in the pack `old` and aren't
#   reachable. One blob, `delta.txt` with 510 lines, is the base of the delta of the blob of `delta.txt` of `main`, which has
#   500 lines, so that a copy of that entry can't keep the delta as the base isn't wanted.
# * `dangling` is a loose blob that nothing refers to, old, and stays as it is.
# * A `multi-pack-index` names `old` and `kept`.
format=${1:?the object format to use}

git init -q --object-format="$format" -b main repo
cd repo

commit() {
  git add -A
  git commit -q -m "$1"
}
loose_path() { echo ".git/objects/${1:0:2}/${1:2}"; }

seq 1 400 >big.txt
commit big
seq 1 500 >delta.txt
commit delta
sed -i.bak 's/^200$/two hundred/' big.txt && rm big.txt.bak
commit "edit big"

git checkout -q -b scratch
seq 1 510 >delta.txt
echo "only on scratch" >gone.txt
commit scratch
scratch_blob=$(git rev-parse HEAD:delta.txt)
git checkout -q main

git repack -q -a -d --window=250 --depth=50
old_pack=$(basename .git/objects/pack/pack-*.idx .idx)

# the base of a delta is not wanted any more
main_blob=$(git rev-parse HEAD:delta.txt)
pack_listing=$(git verify-pack -v ".git/objects/pack/$old_pack.idx")
if ! grep -q "^$main_blob .* $scratch_blob\$" <<<"$pack_listing"; then
  echo "the fixture needs the blob of main to be a delta of the blob of scratch" >&2
  exit 1
fi

git branch -q -D scratch
git reflog expire --expire=now --all

echo "more" >>big.txt
commit "loose 1"
echo "kept" >kept.txt
commit "kept"
git repack -q # packs what is loose and reachable, and leaves the loose copies
kept_pack=$(ls -t .git/objects/pack/pack-*.idx | head -1 | xargs basename | sed 's/\.idx$//')
touch ".git/objects/pack/$kept_pack.keep"
git prune-packed
echo "loose 2" >>big.txt
commit "loose 2"
echo "loose 3" >>big.txt
commit "loose 3"

dangling=$(echo dangling | git hash-object -w --stdin)

touch -t 200101010000 .git/objects/pack/pack-*
touch -t 200001010000 "$(loose_path "$dangling")"
git multi-pack-index write
