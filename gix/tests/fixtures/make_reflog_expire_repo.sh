#!/usr/bin/env bash
set -eu -o pipefail

# A repository with reflog entries of every age that matters for expiring them. The argument is the reference storage format,
# `files` or `reftable`. Run it in place: dates are relative to when it ran, and copying doesn't change them, but the
# ages matter more than a few hours. The ages are 10, 45, 100 and 400 days.
#
# * `main` is a line of commits that are 400, 100, 45 and 10 days old, so everything its reflog mentions is reachable. The reflog of
#   `HEAD` has the same entries, and in addition is the one that keeps the entries of the branch `rewritten`.
# * `rewritten` was created on a commit that is 50 days old, and then reset to a sibling of it, so that it points to a commit
#   that doesn't have the first one as an ancestor. Its entry from creation is 50 days old, and is about an unreachable commit once
#   it was reset, which makes it expire with the documented defaults, and keeps it with Git's code, which isn't the point.
# * `refs/stash` has entries that are 400 days old, which only expire if somebody asks for that.
# * `pinned` and `pinned-total-only` have old entries as well, and the config pins them: `gc.refs/heads/pinned.reflogExpire` is
#   `never` and `gc.refs/heads/pinned-total-only.reflogExpireUnreachable` is the only one set for its pattern, which leaves
#   the other one at zero, so nothing expires, as in Git.
# * `w1` is a linked worktree with a reflog entry of `HEAD` that is 100 days old.
#
# The config sets `gc.reflogExpire` and `gc.reflogExpireUnreachable` explicitly to the documented values, so that
# Git and the native implementation are compared with the same ages. Without them, what Git does differs from what it documents.
format=${1:?the reference format to use}

git init -q --ref-format="$format" -b main repo
cd repo

now=$(date +%s)
commit_at() { # days, message
  local t=$((now - $1 * 86400))
  GIT_AUTHOR_DATE="$t +0000" GIT_COMMITTER_DATE="$t +0000" git commit -q --allow-empty -m "$2"
}
update_ref_at() { # days, ref, value, message
  local t=$((now - $1 * 86400))
  GIT_COMMITTER_DATE="$t +0000" git update-ref --create-reflog -m "$4" "$2" "$3"
}

commit_at 400 c400
commit_at 100 c100
commit_at 45 c45
commit_at 10 c10

# `rewritten` starts on a commit that is then left behind
git checkout -q -b rewritten main~2
commit_at 50 sibling-a
sibling_a=$(git rev-parse HEAD)
git checkout -q main
git checkout -q -b "scratch" main~3
commit_at 49 sibling-b
sibling_b=$(git rev-parse HEAD)
git checkout -q main
update_ref_at 50 refs/heads/rewritten "$sibling_a" "create rewritten"
update_ref_at 49 refs/heads/rewritten "$sibling_b" "reset rewritten"
git branch -q -D scratch

update_ref_at 400 refs/stash main~3 "old stash"
update_ref_at 400 refs/stash main~2 "older stash"

update_ref_at 100 refs/heads/pinned main~2 "pinned 100d"
update_ref_at 45 refs/heads/pinned main "pinned 45d"
update_ref_at 100 refs/heads/pinned-total-only main~2 "pinned-total-only 100d"
update_ref_at 45 refs/heads/pinned-total-only main "pinned-total-only 45d"

git config gc.reflogExpire 90.days.ago
git config gc.reflogExpireUnreachable 30.days.ago
git config "gc.refs/heads/pinned.reflogExpire" never
git config "gc.refs/heads/pinned.reflogExpireUnreachable" never
git config "gc.refs/heads/pinned-total-only.reflogExpireUnreachable" 30.days.ago

git worktree add -q ../w1 -b w1-branch main~1
(
  cd ../w1
  t=$((now - 100 * 86400))
  GIT_COMMITTER_DATE="$t +0000" git checkout -q --detach main~3
  git checkout -q --detach main~1
)
