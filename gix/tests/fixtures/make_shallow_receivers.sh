#!/usr/bin/env bash
set -eu -o pipefail

# A client with a full history and the receivers that are shallow, for comparing what `git receive-pack` and the native receiver do
# when they are pushed to. The hash is the one of the fixture environment. There is one directory for each ref storage format.
#
# `client` has `main` with the commits `c1` to `c4`, in a line, and branches of it:
#
# * `ff`: `c5`, on top of `c4`.
# * `orphan`: a root commit, which is not related to anything else.
# * `side`: `s1`, on top of `c2`, so that it forks from `main` below `c4`.
# * `below-merge`: a merge of `side` into `main`, which therefore has history that is below where the receivers are shallow.
#
# `remote.git` and `remote-graph.git` are bare with `main` and `extra` at `c4`. The first is shallow there as made by `git clone --depth 1`, so that `c4` has the
# `shallow` file's entry and nothing of `c1` to `c3`. The other is made differently: it was a full clone with a
# commit-graph file that remembers all commits, then its `shallow` file was written and what that cuts off was pruned, so that
# the commit-graph file claims parents that are no longer there, as it does if a repository becomes shallow after it had one.
export GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@example.com GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@example.com

commit() { # name time: commit what is staged
  GIT_AUTHOR_DATE="$2 +0000" GIT_COMMITTER_DATE="$2 +0000" git commit -q --allow-empty -m "$1"
}

for format in files reftable; do
  mkdir "$format"
  (
    cd "$format"
    git init -q --ref-format="$format" -b main client
    (
      cd client
      for n in 1 2 3 4; do
        echo "$n" >file && git add file && commit "c$n" "170000010$n"
      done
      git checkout -q -b ff main && echo 5 >file && git add file && commit c5 1700000105
      git checkout -q --orphan orphan && echo other >other && git add -A && commit orphan 1700000106
      git checkout -q -b side main~2 && echo side >side && git add side && commit s1 1700000107
      git checkout -q -b below-merge main
      GIT_AUTHOR_DATE="1700000108 +0000" GIT_COMMITTER_DATE="1700000108 +0000" git merge -q --no-edit --no-ff side
      git checkout -q main
    )

    git clone -q --bare --depth 1 --single-branch --branch main --ref-format="$format" "file://$PWD/client" remote.git 2>/dev/null
    git -C remote.git branch extra main

    git clone -q --bare --branch main --single-branch --ref-format="$format" "file://$PWD/client" remote-graph.git 2>/dev/null
    (
      cd remote-graph.git
      git branch extra main
      git commit-graph write --reachable
      parent=$(git rev-parse main~1)
      git rev-parse main >shallow
      git reflog expire --expire=now --all
      git repack -q -a -d
      git prune --expire=now
      # What the pruning is to have done, so that the fixture shows what it is about.
      test -f objects/info/commit-graph
      ! git cat-file -e "$parent" 2>/dev/null
    )
  )
done
