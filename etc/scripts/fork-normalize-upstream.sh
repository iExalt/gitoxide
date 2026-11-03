#!/usr/bin/env bash
# Print a commit that is an upstream revision with its manifests moved to the fork's lockstep
# versioning, and the normalized base `dev` currently sits on.
#
# Usage: fork-normalize-upstream.sh [REVISION]   (default: origin/upstream/main)
#
# `dev` is a normalized upstream commit with the fork's commits on top. To update, rebase those
# commits from the current base onto a newly normalized revision:
#
#     git rebase --onto <normalized> <base> dev
#
# Upstream versions every crate separately, so rebasing onto raw upstream would conflict in most
# manifests whenever upstream bumps versions. The normalized commit has REVISION as its only
# parent, and REVISION's tree with only manifests and `Cargo.lock` changed. It always uses the
# version `0.0.0`; release commits on top carry the fork version. It runs `cargo update` inside
# REVISION, so upstream's `.cargo/config.toml` applies there.
set -euo pipefail

revision=${1:-origin/upstream/main}
root=$(git rev-parse --show-toplevel)
upstream_commit_id=$(git -C "$root" rev-parse --verify "$revision^{commit}")
trailer=Normalized-upstream

# Other commits can quote the trailer, so accept only a commit whose single parent is the
# upstream revision its trailer names.
base_commit_id=
while read -r candidate_commit_id; do
    named_upstream_id=$(git -C "$root" log -1 --format="%(trailers:key=$trailer,valueonly)" "$candidate_commit_id")
    parent_ids=$(git -C "$root" log -1 --format=%P "$candidate_commit_id")
    if [[ ${named_upstream_id//[[:space:]]/} == "$parent_ids" ]]; then
        base_commit_id=$candidate_commit_id
        break
    fi
done < <(git -C "$root" log origin/dev --first-parent --format=%H --grep="^$trailer: [0-9a-f]\{40\}$")
if [[ -z $base_commit_id ]]; then
    echo "origin/dev has no normalized upstream base" >&2
    exit 1
fi
if git -C "$root" merge-base --is-ancestor "$upstream_commit_id" "$base_commit_id"; then
    echo "$revision is already part of dev's base $base_commit_id" >&2
    exit 1
fi

# Use this checkout's normalizer; upstream revisions don't have one.
cargo build --quiet --locked --manifest-path "$root/Cargo.toml" -p internal-tools --no-default-features --bin qa
qa=$(cargo metadata --format-version 1 --no-deps --manifest-path "$root/Cargo.toml" | jq -er .target_directory)/debug/qa

worktree=$(mktemp -d)
trap 'git -C "$root" worktree remove --force "$worktree"' EXIT
git -C "$root" worktree add --quiet --detach "$worktree" "$upstream_commit_id"
normalized_commit_id=$(
    cd "$worktree"
    "$qa" exec manifests --fix --version 0.0.0 >/dev/null
    cargo update --quiet --workspace
    git add --update
    git commit-tree "$(git write-tree)" -p "$upstream_commit_id" \
        -m "chore(gix): normalize upstream manifests to the fork's lockstep version" \
        -m "Upstream \`$upstream_commit_id\` with every crate on the fork's lockstep version and path-only dependencies between workspace crates." \
        -m "$trailer: $upstream_commit_id"
)
echo "base=$base_commit_id"
echo "normalized=$normalized_commit_id"
