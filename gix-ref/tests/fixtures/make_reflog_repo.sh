#!/usr/bin/env bash
set -eu -o pipefail

# A repository for expiring and deleting reflogs with the store of either reference format, which is the single argument,
# `files` or `reftable`. The hash is the one of the fixture environment.
#
# What makes it special:
# * `refs/heads/topic` has four reflog lines, one per minute, with the messages `one` to `four`, so that a time can tell
#   the older half from the newer one.
# * `refs/tags/logged` has a reflog, although a tag usually doesn't, which only exists because of `core.logAllRefUpdates=always`.
#   Whether a reflog exists is what decides if updating a reference outside of branches adds a line to it.
# * `refs/tags/unlogged` has none, as it was made while reflogs were turned off.

format=${1:?the reference format is the first argument}
export GIT_COMMITTER_NAME='A U Thor' GIT_COMMITTER_EMAIL='author@example.com'
export GIT_AUTHOR_NAME='A U Thor' GIT_AUTHOR_EMAIL='author@example.com'
tick=1700000000
commit() {
  tick=$((tick + 60))
  GIT_COMMITTER_DATE="$tick +0000" GIT_AUTHOR_DATE="$tick +0000" git commit -q --allow-empty -m "$1"
}

git init -q --ref-format="$format" -b main repo
cd repo
commit c1
commit c2
commit c3
c1=$(git rev-parse :/c1)
c2=$(git rev-parse :/c2)
c3=$(git rev-parse :/c3)

git config core.logAllRefUpdates always
update() { GIT_COMMITTER_DATE="$1 +0000" git update-ref -m "$2" "$3" "$4"; }
update 1700000100 one refs/heads/topic "$c1"
update 1700000200 two refs/heads/topic "$c2"
update 1700000300 three refs/heads/topic "$c3"
update 1700000400 four refs/heads/topic "$c1"
update 1700000500 tagged refs/tags/logged "$c1"

git config core.logAllRefUpdates false
git update-ref refs/tags/unlogged "$c2"
git config core.logAllRefUpdates always
