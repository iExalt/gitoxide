#!/usr/bin/env bash
set -eu -o pipefail

# Repositories for editing references with `--batch-updates` semantics: some edits fail on their own and the others go on.
# The hash is the one of the fixture environment.
#
# * `files` and `reftable` have the same references, stored by the files and the reftable backend respectively:
#   `main` at commit "two", the branches `a`, `b`, `d` and `packed` at commit "one", where `a` and `packed` are also in
#   `packed-refs` if the backend has them, and a reference `deep/er` so that `deep` can't be created as it would be a directory.
export GIT_COMMITTER_NAME='A U Thor' GIT_COMMITTER_EMAIL='author@example.com'
export GIT_AUTHOR_NAME='A U Thor' GIT_AUTHOR_EMAIL='author@example.com'
for backend in files reftable; do
  git init -q --ref-format="$backend" -b main "$backend"
  (
    cd "$backend"
    GIT_COMMITTER_DATE="1700000060 +0000" GIT_AUTHOR_DATE="1700000060 +0000" git commit -q --allow-empty -m one
    c1=$(git rev-parse HEAD)
    GIT_COMMITTER_DATE="1700000120 +0000" GIT_AUTHOR_DATE="1700000120 +0000" git commit -q --allow-empty -m two
    for name in a b d packed; do git update-ref "refs/heads/$name" "$c1"; done
    git update-ref refs/heads/deep/er "$c1"
    if [ "$backend" = files ]; then git pack-refs --all; fi
  )
done
