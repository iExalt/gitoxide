#!/bin/bash
# P1: reference-transaction hook events per backend (Git only, disposable dirs).
# Usage: p1.sh <scratch-root>   Output: golden event table on stdout.
set -u
PROBE_ROOT=$1; mkdir -p "$PROBE_ROOT"; PROBE_ROOT=$(cd "$PROBE_ROOT" && pwd)
. "$(dirname "$0")/env.sh"
git --version

mkhook() { # <gitdir> logfile
  cat > "$1/hooks/reference-transaction" <<EOH
#!/bin/bash
state=\$1
stdin=\$(cat | tr '\n' '|')
echo "  hook \$state | \$stdin" >> "$2"
if [ "\${VETO:-}" = "\$state" ]; then exit 1; fi
EOH
  chmod +x "$1/hooks/reference-transaction"
}

run() { # label cmd...   (prints events produced by the command)
  label=$1; shift
  : > "$LOG"
  out=$( "$@" 2>&1 ); rc=$?
  echo "--- $label (rc=$rc)"
  sed 's/^/  out: /' <<<"$out" | head -4
  cat "$LOG"
}

for fmt in files reftable; do
  echo "=================== backend=$fmt"
  R=$PROBE_ROOT/$fmt; rm -rf "$R"; mkdir -p "$R"; cd "$R"
  git init -q --ref-format=$fmt -b main repo; cd repo
  LOG=$R/events.log
  git commit -q --allow-empty -m one; C1=$(git rev-parse HEAD)
  git commit -q --allow-empty -m two; C2=$(git rev-parse HEAD)
  git reset -q --hard "$C1"
  mkhook "$(git rev-parse --git-dir)" "$LOG"
  Z=0000000000000000000000000000000000000000
  echo "C1=$C1 C2=$C2"
  run "create refs/heads/new" git update-ref refs/heads/new $C1
  run "update main C1->C2" git update-ref refs/heads/main $C2 $C1
  run "update no-op (same value)" git update-ref refs/heads/main $C2
  run "delete refs/heads/new (loose)" git update-ref -d refs/heads/new
  git update-ref refs/heads/packed $C1; git pack-refs --all 2>/dev/null
  run "delete packed ref (after pack-refs --all)" git update-ref -d refs/heads/packed
  run "stale old value" git update-ref refs/heads/main $C1 $C1
  run "create when exists (expected absent)" git update-ref refs/heads/main $C1 $Z
  run "delete nonexistent" git update-ref -d refs/heads/nope
  run "veto at preparing" env VETO=preparing git update-ref refs/heads/main $C1
  run "veto at prepared" env VETO=prepared git update-ref refs/heads/main $C1
  run "veto at committed (post-hook exit ignored?)" env VETO=committed git update-ref refs/heads/main $C1
  git update-ref refs/heads/main $C2
  run "stdin multi: update main + create a + create b" bash -c "printf 'start\nupdate refs/heads/main $C1 $C2\ncreate refs/heads/a $C1\ncreate refs/heads/b $C2\nprepare\ncommit\n' | git update-ref --stdin"
  run "stdin multi abort after prepare" bash -c "printf 'start\ncreate refs/heads/c $C1\nprepare\nabort\n' | git update-ref --stdin"
  run "stdin multi, one stale" bash -c "printf 'start\ncreate refs/heads/d $C1\nupdate refs/heads/main $C2 $C2\nprepare\ncommit\n' | git update-ref --stdin"
  # lock conflict
  if [ $fmt = files ]; then
    touch "$(git rev-parse --git-dir)/refs/heads/main.lock"
    run "lock conflict on main (files .lock held)" git -c core.filesRefLockTimeout=0 update-ref refs/heads/main $C1
    rm "$(git rev-parse --git-dir)/refs/heads/main.lock"
  else
    touch "$(git rev-parse --git-dir)/reftable/tables.list.lock"
    run "lock conflict (tables.list.lock held)" git update-ref refs/heads/main $C1
    rm "$(git rev-parse --git-dir)/reftable/tables.list.lock"
  fi
  # symref / HEAD update
  run "update through HEAD (symref target main)" git update-ref HEAD $C1
  run "symbolic-ref HEAD -> refs/heads/a" git symbolic-ref HEAD refs/heads/a
  git symbolic-ref HEAD refs/heads/main

  # receive-pack: bare target of same backend
  for mode in plain atomic; do
    B=$R/bare-$mode.git; git init -q --bare --ref-format=$fmt "$B"
    mkhook "$B" "$LOG"
    git -C "$B" config receive.advertiseatomic true
    # seed
    git push -q "$B" main:refs/heads/main main:refs/heads/old 2>/dev/null
    git commit -q --allow-empty -m three; C3=$(git rev-parse HEAD)
    args=(push "$B" "$C3:refs/heads/main" ":refs/heads/old" "$C3:refs/heads/fresh")
    [ $mode = atomic ] && args+=(--atomic)
    run "receive-pack $mode: update+delete+create" git "${args[@]}"
    # stale: push with bad lease-equivalent via direct old-value not possible; use a rejected non-ff
    run "receive-pack $mode: non-ff rejected (denyNonFastForwards)" bash -c "git -C '$B' config receive.denyNonFastForwards true; git push -f '$B' $C1:refs/heads/main"
    run "receive-pack $mode: veto at preparing" env VETO=preparing git push "$B" "$C3:refs/heads/v1"
    run "receive-pack $mode: veto at prepared" env VETO=prepared git push "$B" "$C3:refs/heads/v2"
    # stale old value inside receive-pack: a pre-receive hook moves the target ref after the client computed its command
    cat > "$B/hooks/pre-receive" <<EOH
#!/bin/bash
env -u GIT_QUARANTINE_PATH -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES git update-ref refs/heads/race $C1 2>>$R/prerecv.err
EOH
    chmod +x "$B/hooks/pre-receive"
    args=(push "$B" "$C3:refs/heads/race" "$C3:refs/heads/ok")
    [ $mode = atomic ] && args+=(--atomic)
    run "receive-pack $mode: ref moved by pre-receive hook (stale old value)" git "${args[@]}"
    rm "$B/hooks/pre-receive"
    git reset -q --hard "$C1"
  done
done
