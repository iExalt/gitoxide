#!/usr/bin/env bash
set -eu -o pipefail

# A repository with every kind of object that `git prune` has to treat differently, and every temporary file it removes, for
# `gix`'s tests of pruning, with `git prune --expire 2010-01-01T00:00:00Z` as the oracle. The argument is the object format
# (`sha1` or `sha256`), `repo` is the repository, and `shallow` a shallow clone of it.
#
# Everything is made old, a time in the year 2000, before the exceptions are made recent again. The unreachable things are:
#
# * `old-dangling`, a blob that nothing refers to. It's removed.
# * `new-dangling`, the same, but recent, so it stays.
# * `chain-parent`, a commit with its tree and blob, which is old, and the parent of `chain-tip`, a commit that is recent.
#   Nothing refers to either of them, but the old objects stay as they are needed by something recent.
# * `pack-tip`, a commit in a pack that is recent, whose parent `pack-parent` is loose and old, with its tree and blob. A pack
#   that is young keeps what its objects lead to.
# * `old-packed`, a blob in a pack that is old, whose objects are never removed.
# * `doomed`, a commit that a deleted branch had, which a commit-graph still has. Pruning removes the
#   commit, and must not leave a commit-graph behind that names it.
# * `empty/`, a directory for loose objects without any.
#
# * `cruft-tree`, a tree in a cruft pack that is old itself, but remembers that the tree is recent. Its blob `cruft-blob` is
#   loose and old, and remains.
#
# * `replaced`, a commit that only exists loose, which a replacement ref has replaced with `replacement` in a pack. It is
#   removed by `prune`, which doesn't follow replacements, and must be left by `prune-packed`, as no pack has it.
#
# And the things that stay as they are reachable: `kept` and the files of `main`, the commit of a branch that only the reflog of
# `HEAD` remembers (`reflog-only`), a file of the index that was never committed (`index-only`), and `packed-and-loose`,
# a blob of `main` that is in a pack and loose as well, where the loose copy is removed by pruning.
#
# The temporary files are `tmp_obj_old` and `tmp_obj_new` in a directory for loose objects, `tmp_dir_old/` with a file, `tmp_new`
# in `objects/`, and `tmp_pack_old` and `tmp_pack_new` in `objects/pack/`, where the old ones are removed.
format=${1:?the object format to use}

git init -q --object-format="$format" -b main repo
cd repo

commit() {
  git add -A
  git commit -q -m "$1"
}
loose_path() { echo ".git/objects/${1:0:2}/${1:2}"; }
blob() { printf '%s\n' "$1" | git hash-object -w --stdin; }

echo kept >kept.txt
commit kept
echo packed-and-loose >packed-and-loose.txt
commit packed-and-loose
git repack -q # packs what is loose, and leaves the loose copies
echo index-only >index-only.txt
git add index-only.txt

git checkout -q -b scratch
echo reflog-only >reflog-only.txt
commit reflog-only
git checkout -q main
git branch -q -D scratch

old_dangling=$(blob old-dangling)
new_dangling=$(blob new-dangling)

# a commit that is made of nothing but what is passed, for the chains
orphan_commit() { # message, parents...
  local message=$1 tree blob_id parents=()
  shift
  blob_id=$(blob "$message")
  tree=$(printf '100644 blob %s\t%s.txt\n' "$blob_id" "$message" | git mktree)
  for parent in "$@"; do parents+=(-p "$parent"); done
  git commit-tree "$tree" ${parents[@]+"${parents[@]}"} -m "$message"
}
chain_parent=$(orphan_commit chain-parent)
chain_tip=$(orphan_commit chain-tip "$chain_parent")
cruft_blob=$(blob cruft-blob)
cruft_tree=$(printf '100644 blob %s\tcruft.txt\n' "$cruft_blob" | git mktree)
rm "$(loose_path "$cruft_tree")" # it's only in the pack that is made below
pack_parent=$(orphan_commit pack-parent)
pack_tip=$(orphan_commit pack-tip "$pack_parent")

old_packed=$(blob old-packed)

# `replaced` is only loose, and a replacement ref has it replaced with `replacement`, which is only in a pack, so that looking
# it up the way a reader does finds it in a pack, which it isn't
replaced=$(orphan_commit replaced)
replacement=$(orphan_commit replacement)
git update-ref "refs/replace/$replaced" "$replacement"

# `doomed` is in a commit-graph while a branch has it, which no reflog of `HEAD` remembers
doomed_tree=$(git mktree </dev/null)
doomed=$(git commit-tree "$doomed_tree" -m doomed)
git update-ref refs/heads/doomed-branch "$doomed"
git commit-graph write --reachable
git branch -q -D doomed-branch

# the packs: one with the recent commit of `pack-tip`, and one with the old blob. Both are removed from the loose objects.
recent_pack=$(echo "$pack_tip" | git pack-objects -q .git/objects/pack/pack)
rm "$(loose_path "$pack_tip")"
old_pack=$(echo "$old_packed" | git pack-objects -q .git/objects/pack/pack)
rm "$(loose_path "$old_packed")"
echo "$replacement" | git pack-objects -q .git/objects/pack/pack >/dev/null
rm "$(loose_path "$replacement")"
# a cruft pack that remembers that `cruft-tree` is recent. It's made in a repository of its own, as a cruft pack has all
# unreachable objects that are loose, and the tree is all it should have. It is the age of the pack that is the old one, and the
# `.mtimes` file that has the time of the tree, which is the time of its creation.
git init -q --object-format="$format" ../cruft-source
(
  cd ../cruft-source
  tree=$(printf '100644 blob %s\tcruft.txt\n' "$cruft_blob" | git mktree --missing)
  source_pack=$(echo "$tree" | git pack-objects -q .git/objects/pack/pack)
  rm ".git/objects/${tree:0:2}/${tree:2}"
  cruft_pack=$(echo "-pack-$source_pack.pack" | git pack-objects -q --cruft .git/objects/pack/pack)
  # the cruft pack has the objects of the pack it replaces, and so possibly its name
  if [ "$cruft_pack" != "$source_pack" ]; then rm .git/objects/pack/pack-"$source_pack".*; fi
)
cp ../cruft-source/.git/objects/pack/pack-* .git/objects/pack/
rm -rf ../cruft-source

# everything is old, and what must be recent is made so again
find .git/objects -type f \( -path '*/??/*' -o -path '*/pack/*' \) -exec touch -t 200001010000 {} +
touch "$(loose_path "$new_dangling")" "$(loose_path "$chain_tip")" .git/objects/pack/pack-"$recent_pack".*

mkdir .git/objects/empty .git/objects/tmp_dir_old
echo x >.git/objects/tmp_dir_old/content
echo x >.git/objects/tmp_new
echo x >.git/objects/pack/tmp_pack_old
echo x >.git/objects/pack/tmp_pack_new
fan_out=$(dirname "$(loose_path "$old_dangling")")
echo x >"$fan_out/tmp_obj_old"
echo x >"$fan_out/tmp_obj_new"
touch -t 200001010000 .git/objects/tmp_dir_old/content .git/objects/tmp_dir_old .git/objects/pack/tmp_pack_old "$fan_out/tmp_obj_old"

# `shallow` is a shallow clone whose branches are gone, so that nothing reaches its shallow commit any more, and all of it is old
cd ..
git clone -q --depth 1 "file://$PWD/repo" shallow
cd shallow
git update-ref -d refs/heads/main
git update-ref -d refs/remotes/origin/main
git reflog expire --expire=now --all
find .git/objects -type f \( -path '*/??/*' -o -path '*/pack/*' \) -exec touch -t 200001010000 {} +
