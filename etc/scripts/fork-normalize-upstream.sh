#!/usr/bin/env bash
# Advance `upstream/normalized`: upstream's history with every manifest on the fork's lockstep
# version, so merging it into `dev` conflicts only on real changes.
#
# Usage: fork-normalize-upstream.sh [REVISION]   (default: origin/upstream/main)
#
# Each normalized commit has the previous normalized commit and REVISION as parents, and
# REVISION's tree with only its manifests and lockfile normalized. Merging one into `dev` then
# uses the previous normalized commit as merge base, so the version renumbering is never part
# of the merge: upstream's per-crate version bumps would otherwise conflict, or sit next to
# real changes and conflict, in most `Cargo.toml` files. Push the branch, then merge it into an
# update branch made from `dev`, as FORK.md describes.
set -euo pipefail

revision=${1:-origin/upstream/main}
branch=upstream/normalized
root=$(git rev-parse --show-toplevel)
upstream_commit_id=$(git -C "$root" rev-parse --verify "$revision^{commit}")
version=$(git -C "$root" show origin/dev:.release-please-manifest.json | jq -er '.["."]')
parents=()
if previous_commit_id=$(git -C "$root" rev-parse --verify --quiet "origin/$branch^{commit}"); then
    if git -C "$root" merge-base --is-ancestor "$upstream_commit_id" "$previous_commit_id"; then
        echo "$revision is already part of origin/$branch" >&2
        exit 1
    fi
    parents+=(-p "$previous_commit_id")
fi
parents+=(-p "$upstream_commit_id")

# Use this checkout's normalizer; upstream revisions don't have one.
cargo build --quiet --locked --manifest-path "$root/Cargo.toml" -p internal-tools --no-default-features --bin qa
qa=$(cargo metadata --format-version 1 --no-deps --manifest-path "$root/Cargo.toml" | jq -er .target_directory)/debug/qa

worktree=$(mktemp -d)
trap 'git -C "$root" worktree remove --force "$worktree"' EXIT
git -C "$root" worktree add --quiet --detach "$worktree" "$upstream_commit_id"
normalized_commit_id=$(
    cd "$worktree"
    "$qa" exec manifests --fix --version "$version" >/dev/null
    cargo update --quiet --workspace
    git add --all
    git commit-tree "$(git write-tree)" "${parents[@]}" \
        -m "chore(gix): normalize upstream manifests to the fork version $version" \
        -m "Upstream \`$upstream_commit_id\` with every crate on the fork's lockstep version and path-only dependencies between workspace crates."
)
git -C "$root" branch --force "$branch" "$normalized_commit_id"
echo "$branch is now $normalized_commit_id; push it with: git push origin $branch"
