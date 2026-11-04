#!/usr/bin/env bash
set -eu -o pipefail

# Configured hooks need Git 2.54 and parallel ones 2.55, and older versions silently ignore `hook.*` configuration,
# which would record misleading expectations.
IFS=. read -r git_major git_minor _ <<<"$(git version | sed -e 's/^git version //')"
if ((git_major < 2 || (git_major == 2 && git_minor < 55))); then
  echo "this fixture needs Git 2.55 or newer to record hook behavior, got $(git version)" >&2
  exit 1
fi

# Each repository exercises one aspect of how Git resolves and runs the hooks of the `pre-commit` event with
# `git hook run`, and records Git's observable behaviour next to it as `expected-*` files.
# The tests replay the same invocation with `gix-hook` and compare. Hooks append what they see to `$RUN_LOG`.

# An executable hook file that logs its name, arguments and standard input, and exits with `$HOOK_EXIT` (or 0).
# It logs only the file name as Git passes a relative path while `gix-hook` passes an absolute one.
file_hook() {
  cat >"$1" <<'HOOK'
#!/bin/sh
echo "file $(basename "$0") args=$* stdin=$(cat)" >>"$RUN_LOG"
exit ${HOOK_EXIT:-0}
HOOK
  chmod +x "$1"
}

printf 'line one\nline two\n' >stdin.txt

# `order` is special for having two configured hooks followed by the hook file. Configured hooks run first, in the
# order they declare the event, and via the shell as they contain shell syntax, which makes `$0` the command itself
# and passes arguments twice, once explicitly and once through the implicit `"$@"`.
git init -q order
(cd order
  git config hook.a.event pre-commit
  git config hook.a.command 'echo cfg-a $0 "$@" >>"$RUN_LOG"'
  git config hook.b.event pre-commit
  git config hook.b.command 'echo cfg-b "$@" >>"$RUN_LOG"'
  file_hook .git/hooks/pre-commit
  RUN_LOG=expected-order.log git hook run --to-stdin=../stdin.txt pre-commit -- x y
)

# `event-disabled` disables the event for configured hooks only, so the hook file still runs.
git init -q event-disabled
(cd event-disabled
  git config hook.a.event pre-commit
  git config hook.a.command 'echo cfg-a >>"$RUN_LOG"'
  git config hook.pre-commit.enabled false
  file_hook .git/hooks/pre-commit
  RUN_LOG=expected-event-disabled.log git hook run pre-commit
)

# `hook-disabled` disables just one hook by its friendly-name.
git init -q hook-disabled
(cd hook-disabled
  git config hook.a.event pre-commit
  git config hook.a.command 'echo cfg-a >>"$RUN_LOG"'
  git config hook.b.event pre-commit
  git config hook.b.command 'echo cfg-b >>"$RUN_LOG"'
  git config hook.a.enabled false
  RUN_LOG=expected-hook-disabled.log git hook run pre-commit
)

# `exit-codes` has hooks fail with different codes, all of which run and whose codes are OR'ed.
git init -q exit-codes
(cd exit-codes
  git config hook.a.event pre-commit
  git config hook.a.command 'echo cfg-a >>"$RUN_LOG"; exit 1'
  git config hook.b.event pre-commit
  git config hook.b.command 'echo cfg-b >>"$RUN_LOG"; exit 2'
  file_hook .git/hooks/pre-commit
  code=0
  RUN_LOG=expected-exit-codes.log HOOK_EXIT=4 git hook run pre-commit || code=$?
  echo "$code" >expected-exit-codes.code
)

# `parallel` runs three configured hooks that are allowed to run in parallel with `hook.jobs`. All of them run and
# fail, so the combined exit code is Git's. The order of their log lines is not defined.
git init -q parallel
(cd parallel
  git config hook.jobs 3
  for name in a b c; do
    git config "hook.$name.event" pre-commit
    git config "hook.$name.parallel" true
    git config "hook.$name.command" "echo cfg-$name >>\"\$RUN_LOG\"; exit 1"
  done
  code=0
  RUN_LOG=expected-parallel.log git hook run pre-commit || code=$?
  echo "$code" >expected-parallel.code
)

# `not-executable` has a hook file without execute permission, which is ignored with advice that Git prints.
git init -q not-executable
(cd not-executable
  file_hook .git/hooks/pre-commit
  chmod -x .git/hooks/pre-commit
  git hook run --ignore-missing pre-commit 2>expected-not-executable.stderr
)
