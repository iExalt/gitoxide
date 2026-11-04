#!/usr/bin/env bash
set -eu -o pipefail

# A repository whose references are stored in reftables written by Git, for reading them with the reftable store and
# comparing what it finds with what Git reports. The hash is the one of the fixture environment, so that the same script
# provides SHA-1 and SHA-256 repositories.
#
# What makes it special:
# * `repo` is the main worktree, and `w1` a linked worktree, so that references private to a worktree (`HEAD`,
#   `refs/bisect/*`, `refs/worktree/*`) are in a stack of their own. Both have private references that the other doesn't.
# * Tags: a lightweight one, an annotated one that has a peeled value, and one that points to another tag.
# * Symbolic references: one that resolves, one without a target, and two that point to each other.
# * `refs/heads/gone` was deleted, which leaves a tombstone in an older table than the one that deleted it.
# * Pseudo references: `ORIG_HEAD` is a root reference in the stack, while Git keeps `FETCH_HEAD` and `MERGE_HEAD` in files.
# * Reflogs: `refs/heads/main` has several entries, `refs/heads/emptied` has none but the marker that it exists (which is
#   what expiring a log leaves behind), and `HEAD` has its own log.
# * A reference in a namespace.
#
# `expected/` has what Git reports about `repo` and `w1`, to compare with.

export GIT_COMMITTER_NAME='A U Thor' GIT_COMMITTER_EMAIL='author@example.com'
export GIT_AUTHOR_NAME='A U Thor' GIT_AUTHOR_EMAIL='author@example.com'
tick=1700000000
commit() {
  tick=$((tick + 60))
  GIT_COMMITTER_DATE="$tick +0000" GIT_AUTHOR_DATE="$tick +0000" git commit -q --allow-empty -m "$1"
}

git init -q --ref-format=reftable -b main repo
cd repo
git config core.logAllRefUpdates always

commit c1
commit c2
commit c3
c1=$(git rev-parse :/c1)
c2=$(git rev-parse :/c2)
c3=$(git rev-parse :/c3)

GIT_COMMITTER_DATE="1700001000 -0130" GIT_COMMITTER_NAME='Zoë Ünï' git update-ref -m "advance with a committer in a different time zone" refs/heads/side "$c2"
git update-ref refs/heads/side "$c3" "$c2"
git branch feature "$c1"
git tag light "$c2"
GIT_COMMITTER_DATE="1700002000 +0000" git tag -a -m "annotated" annotated "$c3"
GIT_COMMITTER_DATE="1700003000 +0000" git tag -a -m "a tag of a tag" tag-of-tag annotated
git update-ref refs/remotes/origin/main "$c2"
git symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main
git symbolic-ref refs/heads/alias refs/heads/main
git symbolic-ref refs/heads/dangling refs/heads/does-not-exist
git symbolic-ref refs/heads/cycle-a refs/heads/cycle-b
git symbolic-ref refs/heads/cycle-b refs/heads/cycle-a
git update-ref refs/heads/gone "$c1"
git update-ref -d refs/heads/gone
git update-ref ORIG_HEAD "$c2"
git update-ref refs/namespaces/ns/refs/heads/main "$c1"
git update-ref refs/heads/emptied "$c2"
git update-ref refs/heads/emptied "$c3"
git reflog expire --expire=all refs/heads/emptied
git update-ref refs/worktree/main-private "$c1"
git bisect start >/dev/null && git bisect bad "$c3" >/dev/null && git bisect good "$c1" >/dev/null
git bisect reset >/dev/null 2>&1 || true
git bisect start >/dev/null && git bisect bad "$c3" >/dev/null

printf '%s\t\tbranch '"'"'main'"'"' of an-url\n' "$c2" >.git/FETCH_HEAD
printf '%s\n' "$c3" >.git/MERGE_HEAD

git worktree add -q ../w1 -b w1-branch "$c2"
(
  cd ../w1
  git config core.logAllRefUpdates always
  git update-ref refs/worktree/w1-private "$c3"
  git bisect start >/dev/null && git bisect bad "$c2" >/dev/null
  git update-ref ORIG_HEAD "$c1"
  GIT_COMMITTER_DATE="1700004000 +0000" git commit -q --allow-empty -m w1-commit
)

mkdir -p ../expected
dump() { # <dir> <name>
  (
    cd "$1"
    git for-each-ref --format='%(refname) %(objectname) %(symref) %(*objectname)' >"../expected/$2.refs"
    git for-each-ref --include-root-refs --format='%(refname)' | grep -v '^refs/' >"../expected/$2.root-refs" || true
    for ref in refs/heads/main refs/heads/side refs/heads/emptied HEAD; do
      git reflog show --date=raw --format='%H%x09%gd%x09%gn%x09%ge%x09%gs' "$ref" >"../expected/$2.reflog.$(echo "$ref" | tr / _)" || true
    done
  )
}
dump . repo
dump ../w1 w1

# The same questions for revision parsing, answered by Git.
resolve() { # <dir> <name> <rev>...
  (
    cd "$1"
    shift; out=$1; shift
    for rev in "$@"; do
      printf '%s %s\n' "$rev" "$(git rev-parse --verify -q "$rev" || echo missing)"
    done >"../expected/$out.resolved"
  )
}
resolve . repo main alias light annotated "annotated^{}" tag-of-tag "tag-of-tag^{}" feature origin origin/main dangling cycle-a gone HEAD ORIG_HEAD FETCH_HEAD MERGE_HEAD "refs/bisect/bad" "main-worktree/HEAD" "worktrees/w1/HEAD" "worktrees/w1/refs/bisect/bad" does-not-exist
resolve ../w1 w1 main w1-branch HEAD ORIG_HEAD FETCH_HEAD MERGE_HEAD "refs/bisect/bad" "main-worktree/HEAD" "main-worktree/ORIG_HEAD" "worktrees/w1/HEAD" "refs/worktree/w1-private" "refs/worktree/main-private"
