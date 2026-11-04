#!/usr/bin/env bash
set -eu -o pipefail

# Repositories for checking if what a push brings is connected, each one a scenario with one new commit that no reference leads to.
#
# Every scenario has `main` with two commits, "c1" and "c2", where `sub/` and `t` come from "c1" and `t2` from "c2".
# The new commit "X" is on top of `main` (unless the scenario says otherwise) and adds `newdir/g`, keeping `sub/` as it is.
# What a scenario breaks is done by removing or damaging loose objects. The id of the tip to check is in `.git/tip`, and the
# references to hide, one per line, in `.git/hide` if there are any.
export GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@example.com GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@example.com
export GIT_AUTHOR_DATE="1700000000 +0000" GIT_COMMITTER_DATE="1700000000 +0000"

loose() { # the path of the loose object with the given id
  echo ".git/objects/${1:0:2}/${1:2}"
}
remove() { rm -f "$(loose "$1")"; }
damage() { chmod u+w "$(loose "$1")"; echo garbage >"$(loose "$1")"; }

scenario() { # name [parent-of-X]
  git init -q -b main "$1"
  cd "$1"
  mkdir sub && echo one >sub/f && echo top >t
  git add -A && git commit -q -m c1
  echo two >t2 && git add -A && git commit -q -m c2
  mkdir newdir && echo new >newdir/g && git add -A
  x_tree=$(git write-tree)
  x=$(git commit-tree "$x_tree" -p "${2:-main}" -m X)
  echo "$x" >.git/tip
  git reset -q --hard main && rm -rf newdir
  new_blob=$(git rev-parse "$x:newdir/g")
  new_subtree=$(git rev-parse "$x:newdir")
  kept_subtree=$(git rev-parse main:sub)
  kept_blob=$(git rev-parse main:sub/f)
}
end() { cd ..; }

# Nothing is wrong.
scenario ok; end

# What is new is missing.
scenario blob_missing; remove "$new_blob"; end
scenario subtree_missing; remove "$new_subtree"; end
scenario root_tree_missing; remove "$x_tree"; end
# A blob only has to exist, but a tree has to be valid.
scenario blob_damaged; damage "$new_blob"; end
scenario tree_damaged; damage "$new_subtree"; end

# What the parent that isn't new has in its trees is known to be there and isn't looked at, even if it's gone.
scenario reused_blob_missing; remove "$kept_blob"; end
scenario reused_subtree_missing; remove "$kept_subtree"; end
scenario parents_tree_missing; remove "$(git rev-parse main^{tree})"; end
scenario grandparent_tree_missing; remove "$(git rev-parse main~1^{tree})"; end
scenario grandparent_blob_missing; remove "$kept_blob"; end
scenario grandparent_missing; remove "$(git rev-parse main~1)"; end

# The history of a new commit has to be there, unless it is what a reference leads to.
scenario parent_missing main~1; remove "$(git rev-parse main~1)"; end
# The commit isn't referenced, so it's new but complete.
scenario parent_not_referenced; git update-ref refs/heads/side main~1; git update-ref -d refs/heads/main; git symbolic-ref HEAD refs/heads/unborn; end
# A reference that leads to nothing fails every push.
scenario reference_broken; remove "$(git rev-parse main)"; end

# Hidden references don't count, even if they lead to the only history there is.
hide() { # the scenario setup: `hidden` leads to `main`, which no other reference has, and `HEAD` doesn't lead anywhere
  git branch hidden main && git update-ref -d refs/heads/main && git symbolic-ref HEAD refs/heads/unborn
  echo refs/heads/hidden >.git/hide
}
scenario hidden_complete; hide; end
scenario hidden_blob_missing; hide; remove "$kept_blob"; end
scenario hidden_tree_missing; hide; remove "$(git rev-parse hidden^{tree})"; end
scenario hidden_blob_missing_but_not_hidden; git branch hidden main; git update-ref -d refs/heads/main; git symbolic-ref HEAD refs/heads/unborn; remove "$kept_blob"; end

# The tip is what a reference leads to already, so nothing is new, whatever else is wrong.
scenario tip_is_referenced; echo "$(git rev-parse main)" >.git/tip; remove "$(git rev-parse main^{tree})"; end

# Tags, and tips that aren't commits.
scenario tag_ok; git tag -a -m t v1 "$(cat .git/tip)"; echo "$(git rev-parse v1)" >.git/tip; git tag -d v1 >/dev/null; end
scenario tag_target_missing; git tag -a -m t v1 "$(cat .git/tip)"; echo "$(git rev-parse v1)" >.git/tip; git tag -d v1 >/dev/null; remove "$x"; end
scenario tip_is_blob; git rev-parse main:t >.git/tip; end
scenario tip_is_tree; git rev-parse main:sub >.git/tip; end

# A commit-graph that doesn't know the new commit, which is what is the case in a push, is no obstacle.
scenario commit_graph; git commit-graph write --reachable; end
scenario commit_graph_blob_missing; git commit-graph write --reachable; remove "$new_blob"; end

# The references of alternates cut history off like those of the repository do: `alternate_user` has no references and no objects
# of its own but those of `alternate_origin`, whose blob of `sub/f` is gone and whose new commit isn't referenced.
scenario alternate_origin; remove "$kept_blob"; end
git init -q -b main alternate_user
echo "$PWD/alternate_origin/.git/objects" >alternate_user/.git/objects/info/alternates
cp alternate_origin/.git/tip alternate_user/.git/tip
# The same, but what is new is what is missing.
scenario alternate_origin_new_blob_missing; remove "$new_blob"; end
git init -q -b main alternate_user_new_blob_missing
echo "$PWD/alternate_origin_new_blob_missing/.git/objects" >alternate_user_new_blob_missing/.git/objects/info/alternates
cp alternate_origin_new_blob_missing/.git/tip alternate_user_new_blob_missing/.git/tip

# The commit-graph knows a parent that is gone, and both Git and the fork trust it, as the parent is what a reference leads to.
scenario commit_graph_parent_missing main~1; git commit-graph write --reachable; remove "$(git rev-parse main~1)"; end

# The `HEAD` of an alternate isn't one of its references that Git uses: here it is detached at "X", which has lost a blob, and the tip to check is
# "Y" on top of it. "X" is new then, and the missing blob makes the push fail.
scenario alternate_detached_origin
echo "$x" >.git/HEAD
remove "$new_blob"
y=$(git commit-tree "$x_tree" -p "$x" -m Y)
end
git init -q -b main alternate_detached_user
echo "$PWD/alternate_detached_origin/.git/objects" >alternate_detached_user/.git/objects/info/alternates
echo "$y" >alternate_detached_user/.git/tip
