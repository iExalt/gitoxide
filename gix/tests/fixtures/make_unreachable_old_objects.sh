#!/usr/bin/env bash
set -eu -o pipefail

# Bare repositories whose only objects are unreachable and old, for `gix`'s tests of freshening objects that are written
# again, with `git gc --prune` as the oracle: a writer that relies on such an object without freshening it loses it to the
# next pruning, before a reference makes it reachable. The argument is the object format (`sha1` or `sha256`).
#
# Each repository has two blobs of the same kind of storage, `<name>-written` for the test to write and `<name>-control` for it
# to leave alone, which has to be pruned to show that the age of the object, and not something else, is what the test sees.
# Nothing refers to any blob, and every object file is dated 2001, far beyond any expiry.
#
# * `loose` stores the blobs as loose objects.
# * `pack` has them in two normal packs, one for each, so that touching a pack affects only one blob.
# * `cruft` has them in one cruft pack with a `.mtimes` file, which is where Git keeps unreachable objects, each with its own
#   expiry time that touching the pack can't change.
format=${1:?the object format to use}

long_ago=200109090000

age_everything() {
  find objects -type f -exec touch -t "$long_ago" {} +
}

blob_in_pack() {
  local content=$1 id
  id=$(echo "$content" | git hash-object -w --stdin)
  git update-ref refs/tmp/keep "$id"
  git repack -q -d
  git update-ref -d refs/tmp/keep
}

make_repo() {
  git init -q --bare --object-format="$format" "$1"
}

make_repo loose
(
  cd loose
  for name in loose-written loose-control; do echo "$name" | git hash-object -w --stdin >/dev/null; done
  age_everything
)

make_repo pack
(
  cd pack
  for name in pack-written pack-control; do blob_in_pack "$name"; done
  age_everything
)

make_repo cruft
(
  cd cruft
  for name in cruft-written cruft-control; do blob_in_pack "$name"; done
  age_everything
  git repack -q -a -d --cruft --cruft-expiration=never
  # the new pack files are new
  age_everything
)
