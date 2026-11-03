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
trailer=Normalized-upstream

# `dev` is the authoritative record of the chain: every merged normalized commit is in its
# history. The branch may be ahead of it with a normalized commit not yet merged.
# Other commits can quote the trailer (a squash merge copies messages), so accept only a commit
# whose last parent is the upstream revision its trailer names.
merged_commit_id=
while read -r candidate_commit_id; do
    named_upstream_id=$(git -C "$root" log -1 --format="%(trailers:key=$trailer,valueonly)" "$candidate_commit_id")
    parent_ids=$(git -C "$root" rev-list --parents -n1 "$candidate_commit_id")
    if [[ ${named_upstream_id//[[:space:]]/} == "${parent_ids##* }" ]]; then
        merged_commit_id=$candidate_commit_id
        break
    fi
done < <(git -C "$root" log origin/dev --topo-order --format=%H --grep="^$trailer: [0-9a-f]\{40\}$")
previous_commit_id=$merged_commit_id
if branch_commit_id=$(git -C "$root" rev-parse --verify --quiet "origin/$branch^{commit}"); then
    if [[ -n $merged_commit_id ]] && ! git -C "$root" merge-base --is-ancestor "$merged_commit_id" "$branch_commit_id"; then
        echo "origin/$branch doesn't contain $merged_commit_id, the last normalized commit merged into dev" >&2
        exit 1
    fi
    previous_commit_id=$branch_commit_id
fi
parents=()
if [[ -n $previous_commit_id ]]; then
    if git -C "$root" merge-base --is-ancestor "$upstream_commit_id" "$previous_commit_id"; then
        echo "$revision is already normalized in $previous_commit_id" >&2
        exit 1
    fi
    parents+=(-p "$previous_commit_id")
else
    echo "No normalized upstream commit in origin/dev or origin/$branch; starting the chain" >&2
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
    git add --update
    git commit-tree "$(git write-tree)" "${parents[@]}" \
        -m "chore(gix): normalize upstream manifests to the fork version $version" \
        -m "Upstream \`$upstream_commit_id\` with every crate on the fork's lockstep version and path-only dependencies between workspace crates." \
        -m "$trailer: $upstream_commit_id"
)
git -C "$root" branch --force "$branch" "$normalized_commit_id"
echo "$branch is now $normalized_commit_id; push it with: git push origin $branch"
