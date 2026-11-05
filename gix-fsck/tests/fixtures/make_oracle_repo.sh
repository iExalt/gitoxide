#!/usr/bin/env bash
set -eu -o pipefail

# The objects that the objects of the differential tests in `tests/object` refer to.
# What makes this repository special is that it contains one of each kind of object, with a history
# of two commits, so checking links finds what it expects to find, and a subtree for `d`.
git init -q base
(
  cd base
  echo a > a
  mkdir d
  echo b > d/b
  git add -A
  git commit -q -m one
  echo c >> a
  git commit -q -a -m two
  git tag -a -m tag v1
)
