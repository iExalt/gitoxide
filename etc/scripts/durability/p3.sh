#!/bin/bash
# P3: files-backend failure classes in Git. Failures are injected from the
# `reference-transaction` hook at state `prepared` (all locks held, nothing renamed yet),
# so the same injection can be reproduced by a fork-side transaction observer.
# Usage: p3.sh <scratch-root>
set -u
PROBE_ROOT=$1; mkdir -p "$PROBE_ROOT"; PROBE_ROOT=$(cd "$PROBE_ROOT" && pwd)
. "$(dirname "$0")/env.sh"
git --version; id -u; df -T "$PROBE_ROOT" | tail -1 | awk '{print "fs=" $2}'

fresh() { # name  -> creates repo with main=C1, sets cwd
  R=$PROBE_ROOT/$1; rm -rf "$R"; mkdir -p "$R"; cd "$R"
  git init -q -b main repo; cd repo
  git config core.logAllRefUpdates always
  git commit -q --allow-empty -m one; C1=$(git rev-parse HEAD)
  git commit -q --allow-empty -m two; C2=$(git rev-parse HEAD)
  git reset -q --hard "$C1"
  G=$(git rev-parse --absolute-git-dir)
}
inject() { # <state> <shell-snippet run in hook at that state>
  cat > "$G/hooks/reference-transaction" <<EOH
#!/bin/bash
if [ "\$1" = "$1" ] && [ ! -e "$G/.injected" ]; then touch "$G/.injected"; $2; fi
exit 0
EOH
  chmod +x "$G/hooks/reference-transaction"; rm -f "$G/.injected"
}
dump() {
  echo "  state:"
  git for-each-ref --format='    ref %(refname) %(objectname:short)' 2>&1 | sed -e "s/${C1:0:7}/C1/;s/${C2:0:7}/C2/"
  echo "    packed-refs: $( [ -f "$G/packed-refs" ] && grep -v '^#' "$G/packed-refs" | sed -e "s/$C1/C1/;s/$C2/C2/" | tr '\n' ';' || echo none)"
  ( cd "$G"; for f in $(find refs -type f 2>/dev/null | sort); do echo "    loose $f = $(sed -e "s/$C1/C1/;s/$C2/C2/" $f)"; done
    for f in $(find logs -type f 2>/dev/null | sort); do echo "    log $f: $(awk '{print $1" "$2}' $f | sed -e "s/$C1/C1/g;s/$C2/C2/g;s/0\{40\}/Z/g" | tr '\n' ';')"; done
    for f in $(find . -name '*.lock' 2>/dev/null | sort); do echo "    LOCKFILE $f"; done
    find logs -type d -empty 2>/dev/null | sed 's/^/    emptydir /' )
}
case_() { echo "=== $1"; }
restore() { chmod -R u+rwx "$G" 2>/dev/null; find "$G" -name '*.lock' -delete 2>/dev/null; }

# S1: two creates in one `update-ref --stdin` transaction; reflog path of 2nd ref is a non-empty directory
fresh s1; case_ "S1 files: second ref's reflog path is a non-empty directory (fails in commit after first rename)"
mkdir -p "$G/logs/refs/notes/b"; echo x > "$G/logs/refs/notes/b/x"
out=$(printf 'start\ncreate refs/heads/a %s\ncreate refs/notes/b %s\nprepare\ncommit\n' $C1 $C1 | git update-ref --stdin 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-300)"; dump

# S2: hook at prepared makes refs/notes read-only: first rename ok, reflog append for 2nd ok?, rename of 2nd fails
fresh s2; case_ "S2 files: refs/notes made read-only at prepared (a renamed; b's log appended? b rename fails)"
mkdir -p "$G/refs/notes"; inject prepared "chmod 555 '$G/refs/notes'"
out=$(printf 'start\ncreate refs/heads/a %s\ncreate refs/notes/b %s\nprepare\ncommit\n' $C1 $C1 | git update-ref --stdin 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-300)"; dump; restore

# S3: deletion of ref that is both loose and packed; loose unlink fails after packed-refs rewrite
fresh s3; case_ "S3 files: delete ref present loose+packed; refs/heads read-only at prepared"
git update-ref refs/heads/p $C1; git update-ref refs/heads/q $C2; git pack-refs --all --no-prune
echo "  before:"; dump
inject prepared "chmod 555 '$G/refs/heads'"
out=$(git update-ref -d refs/heads/p 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-300)"; dump; restore

# S4: update (not delete) of an existing loose ref: reflog dir unwritable at prepared
fresh s4; case_ "S4 files: update existing ref; logs/ read-only at prepared (reflog append fails before rename)"
git update-ref refs/heads/main $C1
inject prepared "chmod -R 555 '$G/logs'"
out=$(git update-ref refs/heads/main $C2 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-300)"; dump; restore

# S5: two updates, logs of second unwritable: first applied
fresh s5; case_ "S5 files: two updates a,b (b's reflog file read-only at prepared)"
git update-ref refs/heads/a $C1; git update-ref refs/heads/b $C1
inject prepared "chmod 444 '$G/logs/refs/heads/b'"
out=$(printf 'start\nupdate refs/heads/a %s %s\nupdate refs/heads/b %s %s\nprepare\ncommit\n' $C2 $C1 $C2 $C1 | git update-ref --stdin 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-300)"; dump; restore

# S6: SIGKILL of git at prepared
fresh s6; case_ "S6 files: SIGKILL of git at prepared (locks held, nothing renamed)"
inject prepared 'kill -9 $PPID'
out=$(printf 'start\ncreate refs/heads/a %s\ncreate refs/heads/b %s\nprepare\ncommit\n' $C1 $C1 | git update-ref --stdin 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-200)"; dump; restore

# S7: SIGKILL between per-ref renames using strace fault injection (rename number 2 => kill)
fresh s7; case_ "S7 files: SIGKILL injected by strace at the 2nd rename(2) of a 3-ref update (between per-ref renames)"
rm -f "$G/hooks/reference-transaction"
for r in a b c; do git update-ref refs/heads/$r $C1; done
out=$(printf 'start\nupdate refs/heads/a %s %s\nupdate refs/heads/b %s %s\nupdate refs/heads/c %s %s\nprepare\ncommit\n' $C2 $C1 $C2 $C1 $C2 $C1 | strace -f -qq -e trace=rename,renameat,renameat2 -e inject=rename,renameat,renameat2:signal=KILL:when=2 -o "$R/strace7.txt" git update-ref --stdin 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-200)"; grep -c . "$R/strace7.txt" | sed 's/^/  strace lines: /'; dump; restore

# S8: EIO injected at the 2nd rename
fresh s8; case_ "S8 files: EIO injected by strace at the 2nd rename(2) of a 3-ref update"
rm -f "$G/hooks/reference-transaction"
for r in a b c; do git update-ref refs/heads/$r $C1; done
out=$(printf 'start\nupdate refs/heads/a %s %s\nupdate refs/heads/b %s %s\nupdate refs/heads/c %s %s\nprepare\ncommit\n' $C2 $C1 $C2 $C1 $C2 $C1 | strace -f -qq -e trace=rename,renameat,renameat2 -e inject=rename,renameat,renameat2:error=EIO:when=2 -o "$R/strace8.txt" git update-ref --stdin 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-300)"; dump; restore

# S9: reflog of a deleted ref cannot be unlinked (Git ignores the failure; fork aborts)
fresh s9; case_ "S9 files: delete ref whose reflog cannot be unlinked (logs/refs/heads read-only at prepared)"
git update-ref refs/heads/x $C1
inject prepared "chmod 555 '$G/logs/refs/heads'"
out=$(git update-ref -d refs/heads/x 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-300)"; dump; restore

# S10: no committer identity available in environment/config
fresh s10; case_ "S10 files: update needing a reflog without GIT_COMMITTER_* set"
rm -f "$G/hooks/reference-transaction"
out=$(env -u GIT_COMMITTER_NAME -u GIT_COMMITTER_EMAIL -u GIT_COMMITTER_DATE -u GIT_AUTHOR_NAME -u GIT_AUTHOR_EMAIL git update-ref refs/heads/n $C1 2>&1); echo "  rc=$? out=$(echo "$out"|tr '\n' '~'|cut -c1-300)"
# Never print the derived identity: it can contain the host name. Only report that an entry exists and its identity is non-empty.
ident=$(cut -d'	' -f1 "$G/logs/refs/heads/n" 2>/dev/null | sed -E 's/^[0-9a-f]{40} [0-9a-f]{40} //; s/ [0-9]+ [+-][0-9]{4}$//')
echo "  reflog entry exists: $([ -n "$ident" ] && echo yes || echo no); identity non-empty: $([ -n "$ident" ] && echo yes || echo no)"
