#!/usr/bin/env bash
set -eu -o pipefail

# An empty reftable repository with reflogs enabled for all references, which is the starting point of both sides of a
# replay: Git adds tables to its copy, and this crate adds identical records to its own copy, so that the tables that the two
# compact to can be compared. The size of the tables decides what is compacted, so what gets written needs to be the same.
#
# usage: make_replay_base.sh <object-format>
#
# * `base`:      the repository, whose stack has the table `git init` writes
# * `commit-id`: the id of a commit to point references at, which exists in `base`

object_format=${1:?the object format}
git init -q --ref-format=reftable --object-format="$object_format" -b main base
(
  cd base
  git config core.logAllRefUpdates always
  tree=$(git mktree </dev/null)
  git commit-tree "$tree" -m one >../commit-id
)
