#!/bin/bash
# Normalize the output of p5.sh so that two runs of the same Git produce the same text: random name suffixes, object
# and pack ids and fan-out directories are replaced. Usage: p5.sh <scratch-root> | normalize.sh > golden/p5-git-<version>.txt
# Lines that only show scheduling or reading are left out: calls that strace splits when processes run at the same
# time, opening /dev/null, and listing directories, which background maintenance does. The sizes of writes to `.keep`
# files are replaced, as their text names the process and the host that wrote them.
exec sed -E \
  -e '/<unfinished \.\.\.>|<\.\.\. [a-z_0-9]+ resumed>|"\/dev\/null"|O_RDONLY\|O_NONBLOCK\|O_CLOEXEC\|O_DIRECTORY/d' \
  -e 's/tmp_obj_[A-Za-z0-9]{6}/tmp_obj/g' \
  -e 's/(tmp_(pack|idx|rev))_[A-Za-z0-9]{6}/\1/g' \
  -e 's/bulk_fsync_[A-Za-z0-9]{6}/bulk_fsync_X/g' \
  -e 's/tmp_objdir-incoming-[A-Za-z0-9]{6}/INCOMING/g' \
  -e 's/tmp_objdir-bulk-fsync-[A-Za-z0-9]{6}/BULK/g' \
  -e 's/-[0-9a-f]{8}\.temp\.[A-Za-z0-9]{6}/-H.temp.X/g' \
  -e 's/(0x[0-9a-f]{12}-0x[0-9a-f]{12})-[0-9a-f]{8}\.ref/\1-H.ref/g' \
  -e 's/pack-[0-9a-f]{40,64}/pack-H/g' \
  -e 's#/[0-9a-f]{2}/[0-9a-f]{38,62}#/xx/OID#g' \
  -e 's#objects/[0-9a-f]{2}/#objects/xx/#g' \
  -e 's#(\.git/objects|INCOMING|/objects)/[0-9a-f]{2}/#\1/xx/#g' \
  -e 's#(\.keep[^ ]*> ->)[0-9]+#\1N#'
