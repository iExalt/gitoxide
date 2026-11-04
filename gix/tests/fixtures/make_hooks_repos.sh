#!/usr/bin/env bash
set -eu -o pipefail

# Git runs hooks in a few different places and environments, which `Repository::hook()` has to match. Each repository
# here has a hook that records where and how it ran, and Git's own run is recorded next to it as `expected-*.log`.
# The tests replay the hook with `gix` and compare the logs. The hook reports its name, its directory, the
# working directory, and where `GIT_DIR` points to if it is set at all, as a path that doesn't depend on how it's spelled.

root=$PWD

recorder() {
  cat >"$1" <<'HOOK'
#!/bin/sh
{
  echo "hook=$(basename "$0") dir=$(basename "$(dirname "$0")") args=$*"
  echo "cwd=$(pwd -P)"
  if [ -n "${GIT_DIR+x}" ]; then echo "git_dir=$(cd "$GIT_DIR" && pwd -P)"; else echo "git_dir=unset"; fi
  echo "git_dir_found=$(git rev-parse --absolute-git-dir 2>&1)"
  echo "location_variables=$(env | grep -c '^GIT_\(WORK_TREE\|COMMON_DIR\)=' || true)"
} >>"$RUN_LOG"
HOOK
  chmod +x "$1"
}

commit() { # <file> makes a commit from the current directory, which may be a subdirectory
  echo "$1" >"$1" && git add "$1" && git commit -q -m "$1"
}

# `plain` has a `pre-commit` hook in the default directory, run by `git commit` from a subdirectory.
# Hooks run at the top of the worktree without `GIT_DIR`.
git init -q plain
(cd plain
  commit first
  recorder .git/hooks/pre-commit
  mkdir sub && cd sub
  RUN_LOG="$root/expected-plain.log" commit second
)

# `relative-hooks-path` points `core.hooksPath` to a relative directory, which is relative to the top of the worktree,
# even if Git runs in a subdirectory. The default hook directory has a hook as well, which must not run.
git init -q relative-hooks-path
(cd relative-hooks-path
  commit first
  mkdir custom-hooks
  recorder custom-hooks/pre-commit
  recorder .git/hooks/pre-commit
  git config core.hooksPath custom-hooks
  mkdir sub && cd sub
  RUN_LOG="$root/expected-relative-hooks-path.log" commit second
)

# `absolute-hooks-path` is like that, with an absolute path, for which the working directory doesn't matter.
git init -q absolute-hooks-path
(cd absolute-hooks-path
  commit first
  mkdir ../absolute-hooks
  recorder ../absolute-hooks/pre-commit
  git config core.hooksPath "$(cd ../absolute-hooks && pwd)"
  RUN_LOG="$root/expected-absolute-hooks-path.log" commit second
)

# `linked` is a linked worktree of `linked-main` whose hooks are shared, but which runs them
# with its own `GIT_DIR`, at its own top-level directory.
git init -q linked-main
(cd linked-main
  commit first
  recorder .git/hooks/pre-commit
  git worktree add -q ../linked
  cd ../linked
  RUN_LOG="$root/expected-linked.log" commit second
)

# `bare.git` receives a push and runs `pre-receive` in the Git directory, with `GIT_DIR` set.
# It also has a `proc-receive` hook that isn't run, only looked up, and a `post-update` hook which reports the raw
# environment to `$PIN_LOG` instead, to pin the differences to Git: it spells `GIT_DIR` as `.` and prepends its exec-path to `PATH`.
git init -q --bare bare.git
recorder bare.git/hooks/pre-receive
recorder bare.git/hooks/proc-receive
cat >bare.git/hooks/post-update <<'HOOK'
#!/bin/sh
echo "git_dir_raw=$GIT_DIR" >>"$PIN_LOG"
echo "path=$PATH" >>"$PIN_LOG"
HOOK
chmod +x bare.git/hooks/post-update
git init -q source
(cd source
  commit first
  PIN_LOG="$root/expected-bare-pin.log" RUN_LOG="$root/expected-bare.log" git push -q ../bare.git HEAD:refs/heads/main
)

# `receiving` is a non-bare repository that is pushed to. Receiving hooks run in the Git directory with `GIT_DIR`
# there as well, unlike hooks of other events.
git init -q receiving
(cd receiving
  commit first
  git config receive.denyCurrentBranch ignore
  recorder .git/hooks/pre-receive
  cd ../source
  RUN_LOG="$root/expected-receiving.log" git push -q ../receiving HEAD:refs/heads/other
)

# `receiving-reftx` runs `reference-transaction` while receiving, which is in the Git directory with `GIT_DIR`, unlike when
# it runs locally. A relative `core.hooksPath` is then relative to the Git directory as well, and the decoy hook
# in the default directory must not run. Git runs the hook for each state of the transaction, so only the first
# invocation, `preparing`, matters.
git init -q receiving-reftx
(cd receiving-reftx
  commit first
  git config receive.denyCurrentBranch ignore
  mkdir .git/custom-hooks
  recorder .git/custom-hooks/reference-transaction
  recorder .git/hooks/reference-transaction
  git config core.hooksPath custom-hooks
  cd ../source
  RUN_LOG="$root/expected-receiving-reftx.log" git push -q ../receiving-reftx HEAD:refs/heads/other
)

# `separate` has its Git directory elsewhere, which its `.git` file points to, and `core-worktree` has a worktree that
# `core.worktree` points to from its Git directory. Git finds neither Git directory from the top of the worktree, so it
# passes `GIT_DIR` to hooks, which can then run `git`.
git init -q --separate-git-dir "$root/separate-git-dir" separate
recorder separate-git-dir/hooks/pre-commit
(cd separate
  RUN_LOG="$root/expected-separate.log" commit first
)
git init -q --bare core-worktree.git
git -C core-worktree.git config core.bare false
git -C core-worktree.git config core.worktree ../core-worktree
mkdir core-worktree
recorder core-worktree.git/hooks/pre-commit
(cd core-worktree
  export GIT_DIR=../core-worktree.git
  RUN_LOG="$root/expected-core-worktree.log" commit first
)

# `bare-relative.git` has a relative `core.hooksPath`, which is relative to the Git directory for bare repositories,
# and a decoy hook in the default directory.
git init -q --bare bare-relative.git
mkdir bare-relative.git/custom-hooks
recorder bare-relative.git/custom-hooks/pre-receive
recorder bare-relative.git/hooks/pre-receive
git -C bare-relative.git config core.hooksPath custom-hooks
(cd source
  RUN_LOG="$root/expected-bare-relative.log" git push -q ../bare-relative.git HEAD:refs/heads/main
)

# `not-executable` has a hook file without execute permission, and `not-executable-quiet` turns the advice about it off.
git init -q not-executable
recorder not-executable/.git/hooks/pre-commit
chmod -x not-executable/.git/hooks/pre-commit
git init -q not-executable-quiet
recorder not-executable-quiet/.git/hooks/pre-commit
chmod -x not-executable-quiet/.git/hooks/pre-commit
git -C not-executable-quiet config advice.ignoredHook false
