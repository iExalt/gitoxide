#!/usr/bin/env bash
set -eu -o pipefail

# Repositories that only differ in how `extensions.refStorage` selects the reference storage backend.
# They all store their references in plain files, which Git itself would read for none of the unsupported variants,
# but the selection is all that matters to opening them, and nothing here writes to them.
# Setting the configuration works with every Git version, unlike creating a reftable repository.
# Version 0 variants exist only for SHA-1, as other object formats are an extension that needs version 1.

git init -q base
(cd base
  git checkout -q -b main
  git commit -q --allow-empty -m c1
)

# $1: name, $2: repositoryformatversion, then key and value pairs to add to the configuration, in order.
make_variant() {
  local name="$1" version="$2"
  shift 2
  if [ "$version" = 0 ] && [ "$(git -C base rev-parse --show-object-format)" != sha1 ]; then
    return
  fi
  cp -R base "$name"
  # The `-f` form doesn't need a valid repository, which might not be the case while it is being configured.
  git config -f "$name/.git/config" core.repositoryformatversion "$version"
  while [ $# -gt 0 ]; do
    git config -f "$name/.git/config" --add "$1" "$2"
    shift 2
  done
}

make_variant v0 0
make_variant v1-without-extension 1
make_variant v1-files 1 extensions.refStorage files
make_variant v1-reftable 1 extensions.refStorage reftable
# The last value wins, like in Git.
make_variant v1-reftable-then-files 1 extensions.refStorage reftable extensions.refStorage files
make_variant v1-files-then-reftable 1 extensions.refStorage files extensions.refStorage reftable
# The key is case-insensitive, whereas values are not.
make_variant v1-key-in-uppercase 1 extensions.REFSTORAGE reftable
make_variant v1-value-in-uppercase 1 extensions.refStorage Files
make_variant v1-unknown 1 extensions.refStorage bogus
make_variant v1-empty 1 extensions.refStorage ""
# These put the references elsewhere, as `<format>://<payload>`, which is as unsupported as reftable.
make_variant v1-files-with-payload 1 extensions.refStorage files://elsewhere
make_variant v1-files-with-empty-payload 1 extensions.refStorage files://
make_variant v1-reftable-with-payload 1 extensions.refStorage reftable://elsewhere
make_variant v1-unknown-with-payload 1 extensions.refStorage bogus://elsewhere
# Git refuses to read refStorage from a repository with version 0, even if the value is `files`.
make_variant v0-files 0 extensions.refStorage files
make_variant v0-reftable 0 extensions.refStorage reftable

make_variant v1-without-value 1
printf '[extensions]\n\trefStorage\n' >> v1-without-value/.git/config
