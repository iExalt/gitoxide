#!/usr/bin/env bash
set -eu -o pipefail

# A client with branches that each change the checked out state in one way, and the remotes with a work tree that they are pushed to,
# for comparing what `git receive-pack` and the native receiver do with `receive.denyCurrentBranch=updateInstead`.
# The hash is the one of the fixture environment. There is one directory for each ref storage format.
#
# `client` has `main` with the files below, and the branches on top of it that are named for what they do to them:
#
#   file             "base", a regular file
#   exec.sh          executable
#   link             a symbolic link to `file`
#   gone             a file that some branches remove or replace
#   dir/inner        a file in a directory
#   dir/deeper/leaf  a file in a directory below it, so that removing it leaves empty directories to be removed
#   tracked-dir/a    a file in a directory that some branches replace with a file
#   crlf.txt         stored with LF and checked out with CRLF as `.gitattributes` says, so that comparing it needs the filter
#
# * `edit`: changes `file` and `crlf.txt`, and adds `new` and `newdir/new`.
# * `delete`: removes `gone` and `dir/deeper/leaf`, which leaves `dir/deeper` empty.
# * `mode`: makes `file` executable and `exec.sh` not.
# * `file-to-dir`: replaces the file `gone` with a directory of that name.
# * `dir-to-file`: replaces the directory `tracked-dir` with a file of that name.
# * `link-to-file`: replaces the symbolic link `link` with a file, and `file-to-link` the file `gone` with a link.
# * `link-to-dir`: replaces the symbolic link `link` with a directory of that name, where the link points to a file.
# * `add-untracked-name`: adds `untracked-name`, which a test puts an untracked file at.
# * `add-in-new-dir`: adds `outside-link/new`, which a test puts an untracked symbolic link to somewhere else at.
# * `add-gitignore`: adds `.gitignore` that ignores `ignored-name`, and the file of that name.
# * `evil-*`: commits whose trees can't be made with `git add`, one for what Git's `verify_path()` refuses and what makes trees
#   invalid: `.git` as a file, below a directory, and in other case; `..`; `.gitmodules` as a symbolic link; `git~1` that NTFS
#   makes `.git`; the same name twice; a file and a directory of the same name; entries that are not in order. They are made with
#   `git hash-object --literally`, which writes what it is given.
#
# `remote.git` is bare with `main`. `work` is a clone of it with `main` checked out. `work-unborn` has a work tree whose `HEAD`
# points to `main`, which doesn't exist. `work-module` has `main` at `module-base`, which has a submodule `module` at a commit
# of the client, and `module-moved` and `module-removed` are on top of it, which move the submodule and remove it.
export GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@example.com GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@example.com

commit() { # name time: commit what is staged
  GIT_AUTHOR_DATE="$2 +0000" GIT_COMMITTER_DATE="$2 +0000" git commit -q --allow-empty -m "$1"
}

# Write a tree of the given "mode name id" entries, in the order given, whatever they are.
raw_tree() {
  perl -e 'for (@ARGV) { my ($m, $n, $h) = split / /, $_, 3; print "$m $n\0", pack("H*", $h) }' "$@" |
    git hash-object -w -t tree --literally --stdin
}

for format in files reftable; do
  mkdir "$format"
  (
    cd "$format"
    git init -q --ref-format="$format" -b main client
    git init -q --bare --ref-format="$format" -b main remote.git
    cd client
    git config core.symlinks true
    printf '*.txt text eol=crlf\n' >.gitattributes
    echo base >file
    printf '#!/bin/sh\n' >exec.sh && chmod +x exec.sh
    ln -s file link
    echo "to remove" >gone
    mkdir -p dir/deeper tracked-dir
    echo inner >dir/inner
    echo leaf >dir/deeper/leaf
    echo a >tracked-dir/a
    printf 'line one\nline two\n' >crlf.txt
    git add -A && commit base 1700000100
    base=$(git rev-parse HEAD)

    branch() { # name: start a branch on top of main
      git checkout -q -b "$1" main
    }
    done_with() { # time: commit what is staged and go back to main
      commit "$1" "$2"
      git checkout -q main
    }

    branch edit
    echo changed >file && printf 'line one\nline two changed\n' >crlf.txt
    echo new >new && mkdir newdir && echo new >newdir/new
    git add -A && done_with edit 1700000200

    branch delete
    git rm -q gone dir/deeper/leaf
    done_with delete 1700000200

    branch mode
    chmod +x file && chmod -x exec.sh
    git add -A && done_with mode 1700000200

    branch file-to-dir
    git rm -q gone && mkdir gone && echo below >gone/x && git add -A
    done_with file-to-dir 1700000200

    branch dir-to-file
    git rm -q -r tracked-dir && echo now a file >tracked-dir && git add -A
    done_with dir-to-file 1700000200

    branch link-to-file
    git rm -q link && echo now a file >link && git add -A
    done_with link-to-file 1700000200

    branch file-to-link
    git rm -q gone && ln -s file gone && git add -A
    done_with file-to-link 1700000200

    branch link-to-dir
    git rm -q link && mkdir link && echo inside >link/inside && git add -A
    done_with link-to-dir 1700000200

    branch add-untracked-name
    echo tracked >untracked-name && git add -A
    done_with add-untracked-name 1700000200

    branch add-in-new-dir
    mkdir outside-link && echo escaped >outside-link/new && git add -A
    done_with add-in-new-dir 1700000200

    branch add-gitignore
    echo ignored-name >.gitignore && echo tracked >ignored-name && git add -f -A
    done_with add-gitignore 1700000200

    blob=$(git rev-parse main:file)
    nested=$(raw_tree "100644 config $blob")
    link_blob=$(printf 'file' | git hash-object -w --stdin)
    evil() { # name tree: a commit on top of main with that tree
      c=$(GIT_AUTHOR_DATE="1700000200 +0000" GIT_COMMITTER_DATE="1700000200 +0000" git commit-tree -p main -m "$1" "$2")
      git branch "$1" "$c"
    }
    evil evil-dotgit-file "$(raw_tree "100644 .git $blob")"
    evil evil-dotgit-dir "$(raw_tree "40000 .git $nested")"
    evil evil-dotgit-below "$(raw_tree "40000 dir $(raw_tree "40000 .git $nested")")"
    evil evil-dotgit-case "$(raw_tree "100644 .GIT $blob")"
    evil evil-dotdot "$(raw_tree "100644 .. $blob")"
    evil evil-gitmodules-link "$(raw_tree "120000 .gitmodules $link_blob")"
    evil evil-ntfs-short-name "$(raw_tree "100644 git~1 $blob")"
    evil evil-same-name-twice "$(raw_tree "100644 a $blob" "100644 a $blob")"
    evil evil-file-and-directory "$(raw_tree "100644 a $blob" "40000 a $nested")"
    evil evil-unsorted "$(raw_tree "100644 b $blob" "100644 a $blob")"

    git push -q ../remote.git main
    cd ..
    git clone -q --no-hardlinks -c core.symlinks=true --ref-format="$format" remote.git work 2>/dev/null

    # A work tree whose branch doesn't exist yet.
    git init -q --ref-format="$format" -b main work-unborn && git -C work-unborn config core.symlinks true

    # A submodule is a commit of the client that `module` points to, which no repository of its own backs here.
    cd client
    git checkout -q -b module-base main
    git update-index --add --cacheinfo "160000,$base,module"
    printf '[submodule "module"]\n\tpath = module\n\turl = ./module\n' >.gitmodules && git add .gitmodules
    commit module-base 1700000200
    git branch module-moved
    git checkout -q module-moved
    other=$(GIT_AUTHOR_DATE="1700000300 +0000" GIT_COMMITTER_DATE="1700000300 +0000" git commit-tree -m other "$(git rev-parse main^{tree})")
    git update-index --cacheinfo "160000,$other,module"
    commit module-moved 1700000300
    git checkout -q -b module-removed module-base
    git rm -q --cached module && git rm -q -f .gitmodules && commit module-removed 1700000300
    git checkout -q main
    cd ..
    git init -q --bare --ref-format="$format" -b main remote-module.git
    (cd client && git push -q ../remote-module.git module-base:main module-moved module-removed)
    git clone -q --no-hardlinks -c core.symlinks=true --ref-format="$format" remote-module.git work-module 2>/dev/null
  )
done
