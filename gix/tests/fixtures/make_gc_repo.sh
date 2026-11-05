#!/usr/bin/env bash
set -eu -o pipefail

# A repository for `gix`'s tests of `gc` and `maintenance run`, with `git gc` and `git maintenance run` 2.56 as the oracle. The
# arguments are the object format (`sha1` or `sha256`) and the reference format (`files` or `reftable`). Dates are fixed to the
# year 2001 or 2002, which is old, and the time of creation where it has to be recent, which only is true for the blob `recent`.
#
# * `main` has three commits that were packed into three packs, the first with 42 objects and the others with 3 each, so that a
#   geometric repack rolls up the small ones and leaves the large one. Their reflog entries of `HEAD` are from 2001 and so expire, followed by
#   commits that are loose, among them one with several blobs whose ids start with `17`, so that Git's estimate of the number of
#   loose objects, which only looks into the directory `objects/17`, finds more than the smallest limit that `gc.auto` can be.
#   Branches are loose references.
# * `scratch` was a branch that is gone with its reflog, but `HEAD` still mentions it until that expires. Its commit, tree and blob are loose and are from 2001, so that
#   they are unreachable and expired, and go with any `gc` that doesn't keep what is unreachable.
# * `recent` is a loose blob that nothing refers to, which is created now and so stays with the default `gc.pruneExpire`.
# * The config has `gc.reflogExpire` and `gc.reflogExpireUnreachable` set to the documented values, so that Git and native code
#   use the same ages.
format=${1:?the object format to use}
ref_format=${2:?the reference format to use}

git init -q --object-format="$format" --ref-format="$ref_format" -b main repo
cd repo
git config gc.reflogExpire 90.days.ago
git config gc.reflogExpireUnreachable 30.days.ago

# Fixed dates make the ids the same each time, so that what Git did can be compared with what native code did.
export GIT_AUTHOR_DATE="2001-01-01T00:00:00+0000" GIT_COMMITTER_DATE="2001-01-01T00:00:00+0000"
for n in 1 2 3; do
  seq 1 $((n * 40)) >file.txt
  if [ "$n" = 1 ]; then
    for i in $(seq 1 40); do echo "file $i" >"f$i.txt"; done
  fi
  git add -A
  git commit -q -m "old $n"
  git repack -q -d
done
git prune-packed

git checkout -q -b scratch
echo "only on scratch" >gone.txt
git add -A
git commit -q -m scratch
git rev-list --objects scratch ^main | cut -d' ' -f1 >../scratch-ids
git checkout -q main
git branch -q -D scratch
while read -r id; do
  loose=".git/objects/${id:0:2}/${id:2}"
  if [ -e "$loose" ]; then touch -t 200101010000 "$loose"; fi
done <../scratch-ids

# blobs whose ids start with `17`, found without writing the others
mkdir ../blobs
for i in $(seq 1 2600); do echo "blob $i" >"../blobs/$i"; done
paste -d' ' <(ls -d ../blobs/* | git hash-object --stdin-paths) <(ls -d ../blobs/*) |
  awk '$1 ~ /^17/ { print $2 }' | head -4 >../blobs-17
[ "$(wc -l <../blobs-17)" -ge 3 ] || {
  echo "the fixture needs at least 3 blobs in objects/17" >&2
  exit 1
}
mkdir seventeen
n=0
while read -r path; do
  n=$((n + 1))
  cp "$path" "seventeen/$n"
done <../blobs-17
rm -rf ../blobs ../blobs-17 ../scratch-ids
export GIT_AUTHOR_DATE="2002-01-01T00:00:00+0000" GIT_COMMITTER_DATE="2002-01-01T00:00:00+0000"
git add -A
git commit -q -m "loose, with blobs in objects/17"
echo more >>file.txt
git add -A
git commit -q -m "loose 2"
git branch -q b1
git branch -q b2
git branch -q b3

echo "recent and unreachable" | git hash-object -w --stdin >/dev/null
