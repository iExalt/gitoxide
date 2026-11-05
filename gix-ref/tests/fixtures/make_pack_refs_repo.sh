#!/usr/bin/env bash
set -eu -o pipefail

# A repository with loose references of all kinds, for comparing `git pack-refs --all --prune` with packing references
# natively. What makes it special:
#
# * `refs/heads/main`, `refs/heads/feature/deep/er/branch` (which empties nested directories when it is pruned) and
#   `refs/remotes/origin/main` are loose and get packed.
# * `refs/heads/was-packed` is in `packed-refs` already, with a stale value that the loose reference overrides, and
#   `refs/heads/only-packed` is only there.
# * `refs/tags/light` is lightweight, `refs/tags/annotated` points to a tag object, `refs/tags/nested` is a tag of a tag, and the
#   tags `tree-tag` and `blob-tag` point to a tree and a blob. Packing writes peeled values for the tag objects.
# * `refs/remotes/origin/HEAD` is symbolic and stays as it is.
# * `refs/bisect/bad` and `refs/worktree/note` are private to a worktree and stay as they are.
# * `refs/heads/broken` points to an object that doesn't exist, and stays loose.
# * `refs/heads/locked` is locked by somebody else, so it can be packed, but not pruned.
# * `refs/stash` has a reflog, which isn't touched.
git init -q -b main repo
cd repo

git commit -q --allow-empty -m c1
git branch was-packed
git branch only-packed
git pack-refs --all --prune
git commit -q --allow-empty -m c2
git branch -f was-packed

git branch feature/deep/er/branch
git update-ref refs/remotes/origin/main HEAD
git symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

git tag light
git tag -a -m annotated annotated
git tag -a -m nested nested annotated
git tag tree-tag "$(git rev-parse 'HEAD^{tree}')"
git tag blob-tag "$(echo blob | git hash-object -w --stdin)"

git update-ref refs/bisect/bad HEAD
git update-ref refs/worktree/note HEAD
git update-ref --create-reflog -m stash refs/stash HEAD

printf '%s\n' "$(git rev-parse HEAD | tr '0-9a-f' 'e')" >.git/refs/heads/broken
git branch locked
touch .git/refs/heads/locked.lock
