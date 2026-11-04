#!/bin/bash
# P5: which flush calls Git issues, and in which order, under core.fsync/core.fsyncMethod.
# Linux only (strace). Usage: p5.sh <scratch-root>
set -u
PROBE_ROOT=$1; mkdir -p "$PROBE_ROOT"; PROBE_ROOT=$(cd "$PROBE_ROOT" && pwd)
. "$(dirname "$0")/env.sh"
git --version; strace -V | head -1; df -T "$PROBE_ROOT" | tail -1 | awk '{print "fs=" $2}'

SYS=openat,open,creat,write,pwrite64,fsync,fdatasync,sync_file_range,rename,renameat,renameat2,link,linkat,unlink,unlinkat,mkdir,mkdirat,ftruncate,fcntl,close
# trace <label> <cmd...>: run under strace -f, print the persistence-relevant call sequence
trace() {
  label=$1; shift
  T=$PROBE_ROOT/trace.raw; rm -f "$T"*
  strace -f -y -qq -s 0 -e trace=$SYS -o "$T" "$@" >"$PROBE_ROOT/last.out" 2>&1; rc=$?
  echo "--- $label (rc=$rc)"
  # keep calls touching the scratch tree; compress writes into one line per fd run
  awk -v root="$PROBE_ROOT" '
    { pid=$1; $1=""; line=$0 }
    index(line, root)==0 && line !~ /"\.\// { next }
    line ~ /ENOENT|EEXIST|ENOTDIR/ && line !~ /^ (unlink|rename)/ { next }
    { if (!(pid in idx)) idx[pid]=++n; sub(/^ /,"",line); print "p" idx[pid] " " line }' "$T" \
   | sed -e "s#$PROBE_ROOT/##g" -e 's/O_RDONLY|O_CLOEXEC/RD/' \
   | awk '
      /^p[0-9]+ (openat|open)\(.*O_(WRONLY|RDWR|CREAT)/ { print; next }
      /^p[0-9]+ (openat|open)\(.*O_DIRECTORY/ { print; next }
      /^p[0-9]+ (write|pwrite64)\(/ { w=$0; sub(/,.*\) += */," ->",w); if (w!=prev) print w; prev=w; next }
      /^p[0-9]+ (fsync|fdatasync|sync_file_range|rename|renameat|renameat2|link|linkat|unlink|unlinkat|mkdir|mkdirat|ftruncate|fcntl)\(/ { print; next }' \
   | awk '!/close\(/' | uniq
}
mkrepo() { # name backend [config...]
  R=$PROBE_ROOT/$1; rm -rf "$R"; mkdir -p "$R"; cd "$R"
  git init -q -b main --ref-format=${2:-files} repo; cd repo; shift 2
  for c in "$@"; do git config "${c%%=*}" "${c#*=}"; done
  git config core.logAllRefUpdates always
  git commit -q --allow-empty -m one; C1=$(git rev-parse HEAD); G=$(git rev-parse --absolute-git-dir)
}

for cfg in "" "core.fsync=reference" "core.fsync=all" "core.fsync=none" "core.fsync=all core.fsyncMethod=writeout-only" "core.fsync=all core.fsyncMethod=batch"; do
  echo; echo "################ files backend, update-ref create, config: ${cfg:-<defaults>}"
  mkrepo u1 files $cfg
  trace "update-ref create refs/heads/new" git update-ref refs/heads/new $C1
done

for cfg in "" "core.fsync=reference" "core.fsync=all"; do
  echo; echo "################ files backend, pack-refs --all, config: ${cfg:-<defaults>}"
  mkrepo u2 files $cfg; git update-ref refs/heads/new $C1
  trace "pack-refs --all" git pack-refs --all
  echo "################ files backend, delete (packed), config: ${cfg:-<defaults>}"
  trace "update-ref -d refs/heads/new" git update-ref -d refs/heads/new
done

for cfg in "" "core.fsync=reference" "core.fsync=all"; do
  echo; echo "################ reftable backend, update-ref create + delete, config: ${cfg:-<defaults>}"
  mkrepo u3 reftable $cfg
  trace "update-ref create refs/heads/new" git update-ref refs/heads/new $C1
  trace "update-ref -d refs/heads/new" git update-ref -d refs/heads/new
done

for cfg in "" "core.fsync=loose-object" "core.fsync=all" "core.fsync=none" "core.fsync=all core.fsyncMethod=batch" "core.fsync=all core.fsyncMethod=writeout-only"; do
  echo; echo "################ hash-object -w x1 loose object, config: ${cfg:-<defaults>}"
  mkrepo u4 files $cfg; echo hi > "$PROBE_ROOT/blob.txt"
  trace "hash-object -w" git hash-object -w "$PROBE_ROOT/blob.txt"
done

# receive-pack: sender repo with N objects, bare target
for cfg in "" "core.fsync=all" "core.fsync=none" "core.fsync=reference" "core.fsync=loose-object" "core.fsyncObjectFiles=true" "core.fsync=all core.fsyncMethod=batch" "core.fsync=loose-object core.fsyncMethod=batch" "core.fsync=all core.fsyncMethod=writeout-only"; do
  for lim in unpack index; do
    echo; echo "################ receive-pack via push ($lim-objects path), files backend, config: ${cfg:-<defaults>}"
    S=$PROBE_ROOT/send; rm -rf "$S"; git init -q -b main "$S"; (cd "$S"; echo a>a; echo b>b; git add a b; git commit -q -m x)
    B=$PROBE_ROOT/recv-$lim.git; rm -rf "$B"; git init -q --bare -b main "$B"
    for c in $cfg; do git -C "$B" config "${c%%=*}" "${c#*=}"; done
    [ $lim = index ] && git -C "$B" config receive.unpackLimit 1
    rm -f "$PROBE_ROOT/trace.raw"*
    git -C "$S" push --receive-pack="strace -f -y -qq -s 0 -e trace=$SYS -o $PROBE_ROOT/trace.raw git-receive-pack" "$B" main:refs/heads/main >"$PROBE_ROOT/last.out" 2>&1; echo "--- receive-pack ($lim) push rc=$?"
    echo "  (receive-side strace, all pids)"
    awk -v root="$PROBE_ROOT" '{pid=$1;$1="";line=$0} index(line,root)==0 && line !~ /"\.\// {next} line ~ /ENOENT|EEXIST/ && line !~ /^ (unlink|rename)/ {next} {if(!(pid in i))i[pid]=++n; sub(/^ /,"",line); print "p" i[pid] " " line}' "$PROBE_ROOT/trace.raw" \
     | sed -e "s#$PROBE_ROOT/##g" \
     | awk '/^p[0-9]+ (openat|open)\(.*O_(WRONLY|RDWR|CREAT|DIRECTORY)/ {print;next} /^p[0-9]+ (write|pwrite64)\(/ {w=$0; sub(/,.*\) += */," ->",w); if(w!=prev)print w; prev=w; next} /^p[0-9]+ (fsync|fdatasync|sync_file_range|rename|renameat|renameat2|link|linkat|unlink|unlinkat|mkdir|mkdirat|ftruncate|fcntl)\(/ {print;next}' | uniq
  done
done
