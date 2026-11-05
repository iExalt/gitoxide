#!/usr/bin/env bash
set -eu -o pipefail

# A bare remote and a client with objects that `receive.fsckObjects` has opinions about, for comparing what `git receive-pack` and the native
# receiver do with the same push. The directory `files` has the hash of the fixture environment:
#
# * `remote.git` is bare with `main` at "c2" on top of "c1".
# * `client` has the same, and branches that each add one commit on top of `main` and have a single flaw, which is what its name says.
#   Their objects are written as they are, without anything checking them, and are made to be a pack of one commit, its tree and what
#   it needs.
# * `remote-empty.git` has nothing, which makes the history that a shallow client leaves out of its pack missing.
# * `remote-shallow.git` is shallow at `main`, whose parent it doesn't have, as a repository is that was cloned with a depth.
# * `remote-legacy.git` has a `main` whose tree has a flaw that predates checking, as repositories that were not checked do.
#   `legacy-next` of the client is a commit on top of it whose tree is that of the flawed one without its flaw and with one more
#   file, which a thin pack makes a delta of the flawed tree, and `legacy-resend` has the flawed
#   tree under a commit that doesn't have `legacy-main` as parent, so that a pack for it has that tree in full.
export GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@example.com GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@example.com

commit() { # name time: an empty commit
  GIT_AUTHOR_DATE="$2 +0000" GIT_COMMITTER_DATE="$2 +0000" git commit -q --allow-empty -m "$1"
}

hex_to_bin() { perl -e 'print pack("H*", $ARGV[0])' "$1"; }

# A tree of entries "mode name id", whose lines are written as they are, in the order given and with whatever mode.
tree() {
  while read -r mode name id; do
    printf '%s %s\0' "$mode" "$name"
    hex_to_bin "$id"
  done | git hash-object -w --literally -t tree --stdin
}

# A commit with `tree` and `parent` and the lines given, which can be anything.
mkcommit() { # tree parent author committer extra
  {
    printf 'tree %s\n' "$1"
    [ -z "$2" ] || printf 'parent %s\n' "$2"
    [ -z "$3" ] || printf '%s\n' "$3"
    [ -z "$4" ] || printf '%s\n' "$4"
    [ -z "${5-}" ] || printf '%s\n' "$5"
    printf '\nmessage\n'
  } | git hash-object -w --literally -t commit --stdin
}

good_author='author a <a@example.com> 1700000600 +0000'
good_committer='committer a <a@example.com> 1700000600 +0000'

mkdir files
cd files
git init -q --bare -b main remote.git
git init -q -b main client
cd client
echo one >file && git add file && commit c1 1700000100
echo two >file && git add file && commit c2 1700000200
git push -q ../remote.git main
base=$(git rev-parse main)
base_tree=$(git rev-parse 'main^{tree}')
blob=$(echo hello | git hash-object -w --stdin)
blob2=$(echo world | git hash-object -w --stdin)

branch() { git update-ref "refs/heads/$1" "$2"; }

# Commits with flaws in their headers. All have the tree of `main`, so that the pack is the commit alone.
branch ok "$(mkcommit "$base_tree" "$base" "$good_author" "$good_committer")"
branch missing-committer "$(mkcommit "$base_tree" "$base" "$good_author" "")"
branch bad-email "$(mkcommit "$base_tree" "$base" 'author a <a@example.com 1700000600 +0000' "$good_committer")"
branch bad-timezone "$(mkcommit "$base_tree" "$base" 'author a <a@example.com> 1700000600 +00x0' "$good_committer")"
branch zero-padded-date "$(mkcommit "$base_tree" "$base" 'author a <a@example.com> 01700000600 +0000' "$good_committer")"
branch extra-header "$(mkcommit "$base_tree" "$base" "$good_author" "$good_committer" 'x-extra: header')"
# A commit that has what is wrong with it twice: the first problem is what's said.
branch two-flaws "$(mkcommit "$base_tree" "$base" 'author a <a@example.com 01700000600 +00x0' "$good_committer")"

# Trees with flaws. Each is under a commit, and has a blob that the pack brings.
tree_commit() { branch "$1" "$(mkcommit "$2" "$base" "$good_author" "$good_committer")"; }
tree_commit zero-padded-mode "$(printf '0100644 f %s\n' "$blob" | tree)"
tree_commit unsorted "$(printf '100644 b %s\n100644 a %s\n' "$blob" "$blob2" | tree)"
tree_commit duplicate "$(printf '100644 a %s\n100644 a %s\n' "$blob" "$blob2" | tree)"
tree_commit dotgit "$(printf '100644 .git %s\n' "$blob" | tree)"
tree_commit dotgit-ntfs "$(printf '100644 GIT~1 %s\n' "$blob" | tree)"
tree_commit full-pathname "$(printf '100644 a/b %s\n' "$blob" | tree)"
tree_commit bad-mode-info "$(printf '100664 f %s\n' "$blob" | tree)"
tree_commit dot-entry "$(printf '100644 . %s\n' "$blob" | tree)"
tree_commit dotdot-entry "$(printf '100644 .. %s\n' "$blob" | tree)"
tree_commit symlink-gitmodules "$(printf '120000 .gitmodules %s\n' "$blob" | tree)"
gm_bad=$(printf '[submodule "a"]\n\tpath = a\n\turl = -oProxyCommand=x\n' | git hash-object -w --stdin)
tree_commit gitmodules-bad-url "$(printf '100644 .gitmodules %s\n' "$gm_bad" | tree)"
gm_ok=$(printf '[submodule "a"]\n\tpath = a\n\turl = https://example.com/a.git\n' | git hash-object -w --stdin)
tree_commit gitmodules-ok "$(printf '100644 .gitmodules %s\n' "$gm_ok" | tree)"
gm_name=$(printf '[submodule "../x"]\n\tpath = a\n\turl = https://example.com/a.git\n' | git hash-object -w --stdin)
tree_commit gitmodules-bad-name "$(printf '100644 .gitmodules %s\n' "$gm_name" | tree)"

# A tag without a tagger, which is accepted with a warning, and one with a bad name.
git update-ref refs/tags/no-tagger "$(printf 'object %s\ntype commit\ntag no-tagger\n\nmessage\n' "$base" | git hash-object -w --literally -t tag --stdin)"
git update-ref refs/tags/bad-tag-name "$(printf 'object %s\ntype commit\ntag bad..name\ntagger a <a@example.com> 1700000600 +0000\n\nmessage\n' "$base" | git hash-object -w --literally -t tag --stdin)"

# A push of more than a hundred objects, which is kept as a pack, with and without a flaw somewhere in the middle.
many() { # name extra-entry-or-empty
  local entries="" n
  for n in $(seq 1 120); do
    entries+="$(printf 'file-%03d\n' "$n" | git hash-object -w --stdin) "
  done
  local lines="" i=0
  for id in $entries; do
    i=$((i + 1))
    lines+="$(printf '100644 file-%03d %s\n' "$i" "$id")"$'\n'
  done
  [ -z "$2" ] || lines+="$2"$'\n'
  tree_commit "$1" "$(printf '%s' "$lines" | tree)"
}
many many-ok ""
many many-flawed "0100644 zzz $blob"
many many-unsorted "100644 aaa $blob"

# A repository that has a flawed tree already, and commits on top of it.
many_lines=""
for n in $(seq 1 40); do
  many_lines+="$(printf '100644 f-%02d %s\n' "$n" "$(printf 'content-%d\n' "$n" | git hash-object -w --stdin)")"$'\n'
done
legacy_tree=$(printf '0100644 flawed %s\n%s' "$blob" "$(printf '%s' "$many_lines" | sed 's/^/&/')" | tree)
legacy_main=$(mkcommit "$legacy_tree" "" "$good_author" "$good_committer")
branch legacy-main "$legacy_main"
git init -q --bare -b main ../remote-legacy.git
git push -q ../remote-legacy.git legacy-main:main
next_tree=$(printf '%s100644 f-new %s\n100644 flawed %s\n' "$many_lines" "$blob2" "$blob" | tree)
branch legacy-next "$(mkcommit "$next_tree" "$legacy_main" "$good_author" "$good_committer")"
resend_tree=$legacy_tree
branch legacy-resend "$(mkcommit "$resend_tree" "$base" "$good_author" "$good_committer")"
git init -q --bare -b main ../remote-empty.git
git clone -q --bare --depth 1 "file://$PWD/../remote.git" ../remote-shallow.git
git checkout -q main
