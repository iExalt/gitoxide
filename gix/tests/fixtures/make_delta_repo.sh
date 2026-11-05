#!/usr/bin/env bash
set -eu -o pipefail

# Two repositories whose objects are all loose, for `gix`'s tests of how well repacking finds deltas, with
# `git repack -a -d -f --window=10 --depth=50` as the reference for the size of the pack. The argument is the object format
# (`sha1` or `sha256`). The text is made with `awk`'s random numbers that are seeded, so that it's the same every time.
#
# * `heavy` is the worst case for storing objects whole: 60 commits that each edit a few lines of two files of 3000 lines and
#   append some, so that every version of a file is nearly the same as the one before, and nothing but deltas keeps the pack small.
# * `ordinary` is a history like that of a small project: 36 files in six directories of between 40 and 200 lines, and 120
#   commits that each change one to four files a little, and add a file now and then. Most objects are small, and many of the
#   trees are tiny.
format=${1:?the object format to use}

line_text() { # seed, count: lines of text that can be edited
  awk -v seed="$1" -v n="$2" 'BEGIN { srand(seed); for (i = 0; i < n; i++) printf "line %d value %d and some more text to make the line long enough to matter\n", i, int(rand() * 100000) }'
}
edit_text() { # seed, rate: change about `rate` of the lines of what is read, and append a few
  awk -v seed="$1" -v rate="$2" 'BEGIN { srand(seed) } { if (rand() < rate) printf "edited line %d value %d, which replaced what was here before\n", NR, int(rand() * 100000); else print } END { for (i = 0; i < 3; i++) printf "appended in version %d number %d\n", seed, i }'
}

init() {
  git init -q --object-format="$format" -b main "$1"
  cd "$1"
  git config gc.auto 0
}

init heavy
line_text 1 3000 >one.txt
line_text 2 3000 >two.txt
git add -A && git commit -q -m "start"
for version in $(seq 1 59); do
  for file in one two; do
    edit_text "$version" 0.004 <"$file.txt" >"$file.new" && mv "$file.new" "$file.txt"
  done
  git add -A && git commit -q -m "version $version"
done
cd ..

init ordinary
dirs=(a b c d e f)
for dir in "${dirs[@]}"; do
  mkdir "$dir"
  for number in 1 2 3 4 5 6; do line_text "$number${#dir}$dir" "$((40 + (number * 29) % 160))" >"$dir/file$number.txt"; done
done
git add -A && git commit -q -m "start"
for version in $(seq 1 119); do
  touched=$((1 + version % 4))
  for step in $(seq 1 "$touched"); do
    dir=${dirs[$(((version * step) % 6))]}
    file="$dir/file$((1 + (version + step) % 6)).txt"
    edit_text "$version$step" 0.03 <"$file" >"$file.new" && mv "$file.new" "$file"
  done
  if [ $((version % 17)) -eq 0 ]; then line_text "$version" 80 >"a/added$version.txt"; fi
  git add -A && git commit -q -m "version $version"
done
