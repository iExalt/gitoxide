//! Bring the work tree of the checked out branch up to a pushed commit, which is what `receive.denyCurrentBranch=updateInstead` does
//! with `update-index --refresh`, `diff-files`, `diff-index --cached` and `read-tree -u -m`, but without running Git.
//!
//! The steps are those of Git, and so are the reasons that are reported when one of them refuses:
//!
//! 1. The `index.lock` of the work tree is taken and held until the new index is written, which is stricter than Git that takes
//!    it for each command. If it can't be taken, the update fails with "Up-to-date check failed".
//! 2. Tracked files that were changed in the work tree are "unstaged changes", and differences between the index and `HEAD`, or the
//!    empty tree if `HEAD` is unborn, are "staged changes". Submodules aren't looked at.
//! 3. The tree of the pushed commit is flattened and validated before anything is touched, as a pushed tree isn't trusted: it
//!    may have names that can't be checked out safely, entries that aren't in order, the same name twice, or a file and a directory
//!    of the same name.
//! 4. Everything that would be in the way is looked for before the first file changes: a file, ignored or not, where a new file is
//!    to be created, or in a directory that has to become a file, which `read-tree` calls untracked files that would be
//!    overwritten. Then files that are no longer tracked are removed along with the directories that became empty, new and changed
//!    files are created without replacing anything, and the new index is written.
//!
//! What isn't supported is refused with the reason "Could not update working tree to new HEAD", along with a note for whoever runs
//! the receiver: a split or sparse index, entries that are `skip-worktree` or `assume-unchanged`, and the case that the work tree
//! is in the middle of a rebase or bisect that involves the branch.
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use gix_hash::{ObjectId, oid};
use gix_index::entry::{Flags, Mode, Stat};
use gix_object::bstr::{BStr, BString, ByteSlice};

use crate::Repository;

/// The deepest nesting of trees that is accepted, which is far deeper than a path can be on any system.
const MAX_TREE_DEPTH: usize = 512;

/// Why the work tree wasn't updated, which tells what to report for the update, in the words of Git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The index couldn't be locked or read.
    UpToDate,
    /// A tracked file differs from the index.
    Unstaged,
    /// The index differs from `HEAD`.
    Staged,
    /// The new state couldn't be put in place, or the request was refused, which `notes` explain.
    Update,
    /// The `push-to-checkout` hook that does the update instead declined.
    Hook,
}

impl Refusal {
    pub(super) fn reason(self) -> &'static str {
        match self {
            Refusal::UpToDate => "Up-to-date check failed",
            Refusal::Unstaged => "Working directory has unstaged changes",
            Refusal::Staged => "Working directory has staged changes",
            Refusal::Update => "Could not update working tree to new HEAD",
            Refusal::Hook => "push-to-checkout hook declined",
        }
    }
}

/// An entry of the tree that the work tree is to match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Wanted {
    pub path: BString,
    pub mode: Mode,
    pub id: ObjectId,
}

/// Return what Git's `canon_mode()` makes of the mode of a tree entry, or `None` for those that can't be checked out.
fn canonical_mode(mode: u16) -> Option<Option<Mode>> {
    // `None` for a tree, and `Some(mode)` for what is an entry of the index.
    match mode & 0o170000 {
        0o040000 if mode == 0o040000 => Some(None),
        0o100000 => Some(Some(if mode & 0o100 != 0 {
            Mode::FILE_EXECUTABLE
        } else {
            Mode::FILE
        })),
        0o120000 if mode == 0o120000 => Some(Some(Mode::SYMLINK)),
        0o160000 if mode == 0o160000 => Some(Some(Mode::COMMIT)),
        _ => None,
    }
}

/// Collect all entries of the tree `tree_id` that go into an index, in the order of paths, and refuse the tree if it can't be
/// checked out safely for what the message says.
///
/// This doesn't trust the tree: names are validated as for a checkout, names are unique and in the order of Git, which also means
/// that no file has the name of a directory, and trees are not nested without bounds.
pub(super) fn flatten(
    repo: &Repository,
    tree_id: &oid,
    validate: gix_validate::path::component::Options,
) -> Result<Vec<Wanted>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(BString::default(), tree_id.to_owned(), 0_usize)];
    while let Some((prefix, id, depth)) = stack.pop() {
        if depth > MAX_TREE_DEPTH {
            return Err(format!("the tree is nested more deeply than {MAX_TREE_DEPTH} levels"));
        }
        let object = repo
            .find_object(id)
            .map_err(|err| format!("could not read the tree {id}: {err}"))?;
        if object.kind != gix_object::Kind::Tree {
            return Err(format!("{id} is not a tree"));
        }
        let mut names = HashSet::new();
        let mut previous: Option<(BString, bool)> = None;
        for entry in gix_object::TreeRefIter::from_bytes(&object.data, repo.object_hash()) {
            let entry = entry.map_err(|err| format!("could not decode the tree {id}: {err}"))?;
            let name = entry.filename;
            let full = if prefix.is_empty() {
                BString::from(name)
            } else {
                let mut full = prefix.clone();
                full.push(b'/');
                full.extend_from_slice(name);
                full
            };
            if name.contains(&b'/') {
                return Err(format!("the path '{full}' has a name that contains a slash"));
            }
            let Some(mode) = canonical_mode(entry.mode.value()) else {
                return Err(format!(
                    "the path '{full}' has the mode {:o}, which can't be checked out",
                    entry.mode.value()
                ));
            };
            let component_mode = (mode == Some(Mode::SYMLINK)).then_some(gix_validate::path::component::Mode::Symlink);
            gix_validate::path::component(name, component_mode, validate)
                .map_err(|err| format!("the path '{full}' isn't valid: {err}"))?;
            let is_tree = mode.is_none();
            // Entries are sorted as if directories had a trailing slash, which is what makes the order the one of paths.
            if let Some((before, before_is_tree)) = &previous {
                let order = |name: &[u8], is_tree: bool| {
                    let mut key = name.to_vec();
                    if is_tree {
                        key.push(b'/');
                    }
                    key
                };
                if order(before.as_slice(), *before_is_tree) >= order(name, is_tree) {
                    return Err(format!("the entries of the tree {id} aren't in order at '{full}'"));
                }
            }
            if !names.insert(BString::from(name)) {
                return Err(format!(
                    "the tree {id} has the name '{full}' as both file and directory"
                ));
            }
            previous = Some((name.to_owned(), is_tree));
            match mode {
                None => stack.push((full, entry.oid.to_owned(), depth + 1)),
                Some(mode) => out.push(Wanted {
                    path: full,
                    mode,
                    id: entry.oid.to_owned(),
                }),
            }
        }
    }
    out.sort_by(|a, b| gix_index::Entry::cmp_filepaths(a.path.as_bstr(), b.path.as_bstr()));
    Ok(out)
}

/// What is to happen to the tracked entries, found by comparing the index with what is wanted.
#[derive(Debug, Default)]
struct Plan {
    /// The positions of the entries of the index that are removed from the work tree.
    remove: Vec<usize>,
    /// What stays as it is: the position in what is wanted and the one in the index.
    keep: Vec<(usize, usize)>,
    /// The positions in what is wanted that need a file, or a directory for a submodule, which wasn't there or differs.
    create: Vec<usize>,
    /// The positions in what is wanted that replace a submodule with another, which only changes the index.
    index_only: Vec<usize>,
}

fn plan(old: &gix_index::State, wanted: &[Wanted]) -> Plan {
    use std::cmp::Ordering;
    let mut plan = Plan::default();
    let old_entries = old.entries();
    let (mut o, mut w) = (0, 0);
    while o < old_entries.len() || w < wanted.len() {
        let order = match (old_entries.get(o), wanted.get(w)) {
            (Some(old_entry), Some(new)) => gix_index::Entry::cmp_filepaths(old_entry.path(old), new.path.as_bstr()),
            (Some(_), None) => Ordering::Less,
            (None, _) => Ordering::Greater,
        };
        match order {
            Ordering::Less => {
                plan.remove.push(o);
                o += 1;
            }
            Ordering::Greater => {
                plan.create.push(w);
                w += 1;
            }
            Ordering::Equal => {
                let (old_entry, new) = (&old_entries[o], &wanted[w]);
                if old_entry.mode == new.mode && old_entry.id == new.id {
                    plan.keep.push((w, o));
                } else if old_entry.mode == Mode::COMMIT && new.mode == Mode::COMMIT {
                    plan.index_only.push(w);
                } else {
                    plan.remove.push(o);
                    plan.create.push(w);
                }
                o += 1;
                w += 1;
            }
        }
    }
    plan
}

/// Whether anything exists at `path` without following a symbolic link.
fn lstat(path: &Path) -> std::io::Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn to_path(root: &Path, relative: &BStr) -> Result<PathBuf, String> {
    gix_path::from_bstr(relative)
        .map(|relative| root.join(relative))
        .map_err(|err| format!("the path '{relative}' can't be used here: {err}"))
}

/// Tells if a path, a directory if the flag is set, is ignored, which makes it something that may be overwritten.
type Ignored<'a> = dyn FnMut(&BStr, bool) -> bool + 'a;

/// Check that `directory`, at `relative` to the work tree, has nothing but tracked files that are removed, ignored files and
/// directories, which are all that may be lost with it.
fn directory_is_clear(
    directory: &Path,
    relative: &BStr,
    removed: &HashSet<&[u8]>,
    ignored: &mut Ignored<'_>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory).map_err(|err| format!("could not read '{relative}': {err}"))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("could not read '{relative}': {err}"))?;
        let mut name = relative.to_owned();
        name.push(b'/');
        name.extend_from_slice(&gix_path::os_string_into_bstring(entry.file_name()).unwrap_or_default());
        let kind = entry
            .file_type()
            .map_err(|err| format!("could not read '{name}': {err}"))?;
        if kind.is_dir() {
            if !ignored(name.as_bstr(), true) {
                directory_is_clear(&entry.path(), name.as_bstr(), removed, ignored)?;
            }
        } else if !removed.contains(name.as_slice()) && !ignored(name.as_bstr(), false) {
            return Err(format!("Updating '{relative}' would lose untracked files in it"));
        }
    }
    Ok(())
}

/// Check what is in the way of `wanted`, an entry that is to be created, given the tracked paths in `removed` that are gone by the
/// time it is created. Untracked files that are ignored aren't in the way, like they aren't for Git, and the path of what has to
/// be removed for that is returned.
fn verify_absent(
    root: &Path,
    wanted: &Wanted,
    removed: &HashSet<&[u8]>,
    ignored: &mut Ignored<'_>,
) -> Result<Option<PathBuf>, String> {
    let path = wanted.path.as_bstr();
    let in_the_way = |what: &BStr| format!("Untracked working tree file '{what}' would be overwritten by merge.");
    // No leading component may be a file or a link that stays, as creating below it would write wherever it leads.
    let mut end = 0;
    while let Some(slash) = path[end..].find_byte(b'/') {
        end += slash;
        let leading = path[..end].as_bstr();
        let leading_path = to_path(root, leading)?;
        match lstat(&leading_path).map_err(|err| format!("could not look at '{leading}': {err}"))? {
            None => return Ok(None),
            Some(meta) if meta.is_dir() => {}
            Some(_) if removed.contains(leading.as_bytes()) => return Ok(None),
            Some(_) if ignored(leading, false) => return Ok(Some(leading_path)),
            Some(_) => return Err(in_the_way(leading)),
        }
        end += 1;
    }
    let full_path = to_path(root, path)?;
    match lstat(&full_path).map_err(|err| format!("could not look at '{path}': {err}"))? {
        None => Ok(None),
        Some(meta) if meta.is_dir() => {
            if !ignored(path, true) {
                directory_is_clear(&full_path, path, removed, ignored)?;
            }
            Ok(Some(full_path))
        }
        Some(_) if removed.contains(path.as_bytes()) => Ok(None),
        Some(_) if ignored(path, false) => Ok(Some(full_path)),
        Some(_) => Err(in_the_way(path)),
    }
}

/// Remove the file or link at `relative`, and the directories above it that this leaves empty, which `rmdir` leaves alone if they
/// aren't.
fn remove_with_empty_parents(root: &Path, relative: &BStr, is_submodule: bool) -> Result<(), String> {
    let path = to_path(root, relative)?;
    let removed = if is_submodule {
        std::fs::remove_dir(&path)
    } else {
        std::fs::remove_file(&path)
    };
    match removed {
        Ok(()) => {}
        // A submodule that was checked out stays, like in Git, which only warns about it.
        Err(_) if is_submodule => return Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.to_string()),
    }
    let mut parent = path.parent();
    while let Some(dir) = parent {
        if dir == root || std::fs::remove_dir(dir).is_err() {
            break;
        }
        parent = dir.parent();
    }
    Ok(())
}

/// Return the position of the first entry that needs more than the index to be rewritten, which is what this doesn't support.
fn unsupported_entry(index: &gix_index::State) -> Option<&'static str> {
    if index.link().is_some() {
        return Some("the index is split");
    }
    if index.is_sparse() {
        return Some("the index is sparse");
    }
    index.entries().iter().find_map(|entry| {
        if entry.flags.contains(Flags::SKIP_WORKTREE) {
            Some("the work tree has entries that are marked skip-worktree")
        } else if entry.flags.contains(Flags::ASSUME_VALID) {
            Some("the work tree has entries that are marked assume-unchanged")
        } else {
            None
        }
    })
}

/// Run `update-index --refresh` and `diff-files` for the work tree of `repo`, which fail if a tracked file differs from the index.
///
/// Entries that are only different in their file system data are unchanged, and the index that a refresh would write isn't.
fn up_to_date(repo: &Repository) -> Result<(), Refusal> {
    use gix_status::index_as_worktree::EntryStatus;

    use crate::status::{Submodule, index_worktree::Item};
    let iter = repo
        .status(gix_utils::progress::Discard)
        .map_err(|_| Refusal::UpToDate)?
        .index_worktree_rewrites(None)
        .index_worktree_submodules(Submodule::Given {
            ignore: crate::submodule::config::Ignore::All,
            check_dirty: false,
        })
        .index_worktree_options_mut(|options| options.dirwalk_options = None)
        .into_index_worktree_iter(Vec::new())
        .map_err(|_| Refusal::UpToDate)?;
    for item in iter {
        match item.map_err(|_| Refusal::UpToDate)? {
            Item::Modification {
                status: EntryStatus::NeedsUpdate(_),
                ..
            } => {}
            _ => return Err(Refusal::Unstaged),
        }
    }
    Ok(())
}

/// What is needed to bring the work tree to a commit, which doesn't depend on its state.
struct Prepared {
    options: gix_worktree_state::checkout::Options,
    wanted: Vec<Wanted>,
    plan: Plan,
    /// What the files that are to be removed looked like before the work tree was checked, which they must still look like when
    /// they are removed, or they have been changed since the checks that decide if that is allowed.
    seen: Vec<(usize, Option<Stat>)>,
}

/// Read what `new_commit` has and decide what to do about it, with everything that can't be done as a message.
fn prepare(repo: &Repository, old: &gix_index::State, new_commit: &oid, work_dir: &Path) -> Result<Prepared, String> {
    if new_commit.is_null() {
        return Err(format!("not a valid tree-ish: {new_commit}"));
    }
    let options = repo
        .checkout_options(gix_worktree::stack::state::attributes::Source::IdMappingThenWorktree)
        .map_err(|err| format!("could not configure the checkout: {err}"))?;
    let tree_id = repo
        .find_commit(new_commit)
        .map_err(|err| format!("could not read the commit {new_commit}: {err}"))
        .and_then(|commit| {
            commit
                .tree_id()
                .map(crate::Id::detach)
                .map_err(|err| format!("could not read the tree: {err}"))
        })?;
    let wanted = flatten(repo, &tree_id, options.validate).map_err(|err| format!("refusing the tree: {err}"))?;
    let plan = plan(old, &wanted);
    let mut seen = Vec::new();
    for &position in &plan.remove {
        let entry = &old.entries()[position];
        if entry.mode == Mode::COMMIT {
            continue;
        }
        let path = to_path(work_dir, entry.path(old))?;
        seen.push((
            position,
            gix_index::fs::Metadata::from_path_no_follow(&path)
                .ok()
                .and_then(|meta| Stat::from_fs(&meta).ok()),
        ));
    }
    Ok(Prepared {
        options,
        wanted,
        plan,
        seen,
    })
}

/// Run `diff-index --cached` of `HEAD`, or of the empty tree if it is unborn, against the index.
fn index_matches_head(repo: &Repository, index: &gix_index::State) -> Result<bool, Refusal> {
    let head_tree_id = repo.head_tree_id_or_empty().map_err(|_| Refusal::Staged)?;
    let mut differs = false;
    repo.tree_index_status(
        &head_tree_id,
        index,
        None,
        crate::status::tree_index::TrackRenames::Disabled,
        |change, _, _| {
            // Submodules aren't compared, like with `--ignore-submodules`.
            let is_submodule = match &change {
                gix_diff::index::ChangeRef::Addition { entry_mode, .. }
                | gix_diff::index::ChangeRef::Deletion { entry_mode, .. } => *entry_mode == Mode::COMMIT,
                gix_diff::index::ChangeRef::Modification {
                    previous_entry_mode,
                    entry_mode,
                    ..
                } => *previous_entry_mode == Mode::COMMIT && *entry_mode == Mode::COMMIT,
                gix_diff::index::ChangeRef::Rewrite { .. } => false,
            };
            if !is_submodule {
                differs = true;
                return Ok(std::ops::ControlFlow::Break(()));
            }
            Ok(std::ops::ControlFlow::Continue(()))
        },
    )
    .map_err(|_| Refusal::Staged)?;
    Ok(!differs)
}

/// Put the work tree of `repo`, which has the branch that is pushed to checked out, in the state of the commit `new_commit`.
///
/// Whatever can't be done is refused, with the reason to report for it, before a file has changed if it can be known by then.
/// `notes` collects what Git would say on its standard error, which isn't for the client.
pub(super) fn update_instead(repo: &Repository, new_commit: &oid, notes: &mut Vec<String>) -> Result<(), Refusal> {
    let Some(work_dir) = repo.workdir().map(Path::to_owned) else {
        notes.push("the repository has no work tree".into());
        return Err(Refusal::Update);
    };
    let index_path = repo.index_path();
    let mut lock =
        gix_lock::File::acquire_to_update_resource(&index_path, gix_lock::acquire::Fail::Immediately, None, 0)
            .map_err(|err| {
                notes.push(format!("Unable to create '{}.lock': {err}", index_path.display()));
                Refusal::UpToDate
            })?;
    let index = repo.index_or_empty().map_err(|err| {
        notes.push(format!("could not read the index: {err}"));
        Refusal::UpToDate
    })?;
    let old: &gix_index::State = &index;
    // `update-index --refresh` fails for paths that need a merge.
    if old
        .entries()
        .iter()
        .any(|entry| entry.stage() != gix_index::entry::Stage::Unconflicted)
    {
        return Err(Refusal::UpToDate);
    }
    // What the files looked like is noted before they are checked, which is what makes a change after that noticeable. Whether
    // the commit can be used is only told once the work tree has passed its checks, as it is by Git.
    let prepared = prepare(repo, old, new_commit, &work_dir);
    up_to_date(repo)?;
    if !index_matches_head(repo, old)? {
        return Err(Refusal::Staged);
    }

    if let Some(reason) = unsupported_entry(old) {
        notes.push(format!("not supported: {reason}"));
        return Err(Refusal::Update);
    }
    let Prepared {
        options,
        wanted,
        plan,
        seen,
    } = prepared.map_err(|err| {
        notes.push(err);
        Refusal::Update
    })?;
    let removed_files: HashSet<&[u8]> = plan
        .remove
        .iter()
        .filter(|&&position| old.entries()[position].mode != Mode::COMMIT)
        .map(|&position| old.entries()[position].path(old).as_bytes())
        .collect();
    // Untracked files that are ignored are overwritten, like Git does it, using the same rules that tell what is ignored.
    let mut excludes = repo
        .excludes(
            old,
            None,
            gix_worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
        )
        .map_err(|err| {
            notes.push(format!("could not set up the ignore rules: {err}"));
            Refusal::Update
        })?;
    let mut ignored = |path: &BStr, is_dir: bool| {
        let mode = if is_dir { Mode::DIR } else { Mode::FILE };
        excludes
            .at_entry(path, Some(mode))
            .is_ok_and(|platform| platform.is_excluded())
    };
    let mut in_the_way = Vec::new();
    for &position in &plan.create {
        let obstacle = verify_absent(&work_dir, &wanted[position], &removed_files, &mut ignored).map_err(|err| {
            notes.push(err);
            Refusal::Update
        })?;
        in_the_way.extend(obstacle);
    }

    // What is removed must still be what it was when it was checked, as it could have changed since.
    for (position, before) in seen {
        let entry = &old.entries()[position];
        let path = to_path(&work_dir, entry.path(old)).map_err(|err| {
            notes.push(err);
            Refusal::Update
        })?;
        let current = gix_index::fs::Metadata::from_path_no_follow(&path)
            .ok()
            .and_then(|meta| Stat::from_fs(&meta).ok());
        if !before
            .zip(current)
            .is_some_and(|(before, current)| before.matches(&current, options.stat_options))
        {
            notes.push(format!("Entry '{}' not uptodate. Cannot merge.", entry.path(old)));
            return Err(Refusal::Update);
        }
    }

    for &position in &plan.remove {
        let entry = &old.entries()[position];
        remove_with_empty_parents(&work_dir, entry.path(old), entry.mode == Mode::COMMIT).map_err(|err| {
            notes.push(format!("could not remove '{}': {err}", entry.path(old)));
            Refusal::Update
        })?;
    }

    // What is ignored and in the way goes now, as nothing that can't be replaced is lost, and what is gone already is no obstacle.
    for obstacle in in_the_way {
        let removed = match std::fs::symlink_metadata(&obstacle) {
            // Like Git, this goes by path, so a local process that replaces a parent directory with a link between the checks
            // and now could redirect it. The pusher can't do that, only somebody who can already write to the work tree.
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&obstacle),
            Ok(_) => std::fs::remove_file(&obstacle),
            Err(err) => Err(err),
        };
        match removed {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                notes.push(format!("could not remove '{}': {err}", obstacle.display()));
                return Err(Refusal::Update);
            }
        }
    }

    // The checkout looks at the entries of the new index only to find attributes, so what stays as it is is skipped.
    let mut new_index = gix_index::State::new(repo.object_hash());
    let mut staying = HashSet::new();
    staying.extend(plan.keep.iter().map(|&(wanted_position, _)| wanted_position));
    staying.extend(plan.index_only.iter().copied());
    let kept: HashMap<usize, usize> = plan.keep.iter().copied().collect();
    for (position, entry) in wanted.iter().enumerate() {
        // What is kept has the stat information it had, which the next status refreshes if it was out of date.
        let stat = kept
            .get(&position)
            .map_or_else(Stat::default, |&old_position| old.entries()[old_position].stat);
        let flags = if staying.contains(&position) {
            Flags::SKIP_WORKTREE
        } else {
            Flags::empty()
        };
        new_index.dangerously_push_entry(stat, entry.id, flags, entry.mode, entry.path.as_bstr());
    }
    let objects = repo.objects.clone().into_arc().map_err(|err| {
        notes.push(format!("could not share the object database: {err}"));
        Refusal::Update
    })?;
    let mut options = options;
    // Git makes files executable when they are executable in the tree, whether or not it trusts the file system to keep that.
    options.fs.executable_bit = true;
    options.destination_is_initially_empty = true;
    options.overwrite_existing = false;
    options.keep_going = false;
    let outcome = gix_worktree_state::checkout(
        &mut new_index,
        &work_dir,
        objects,
        &gix_utils::progress::Discard,
        &gix_utils::progress::Discard,
        &AtomicBool::new(false),
        options,
    )
    .map_err(|err| {
        notes.push(format!("could not check out: {err}"));
        Refusal::Update
    })?;
    if !outcome.collisions.is_empty() || !outcome.errors.is_empty() {
        for collision in &outcome.collisions {
            notes.push(format!(
                "Untracked working tree file '{}' would be overwritten by merge.",
                collision.path
            ));
        }
        for error in &outcome.errors {
            notes.push(format!("could not check out '{}': {}", error.path, error.error));
        }
        return Err(Refusal::Update);
    }
    for entry in new_index.entries_mut() {
        entry.flags.remove(Flags::SKIP_WORKTREE);
    }

    let skip_hash = crate::config::tree::Index::SKIP_HASH
        .enrich_error(repo.config.resolved.boolean(crate::config::tree::Index::SKIP_HASH))
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut writer = std::io::BufWriter::new(&mut lock);
    new_index
        .write_to(
            &mut writer,
            gix_index::write::Options {
                extensions: gix_index::write::Extensions::None,
                skip_hash,
            },
        )
        .map_err(|err| {
            notes.push(format!("could not write the index: {err}"));
            Refusal::Update
        })?;
    std::io::Write::flush(&mut writer).map_err(|err| {
        notes.push(format!("could not write the index: {err}"));
        Refusal::Update
    })?;
    drop(writer);
    lock.commit().map_err(|err| {
        notes.push(format!("could not commit the index: {}", err.error));
        Refusal::Update
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use gix_index::entry::Mode;
    use gix_object::bstr::ByteSlice;

    use super::*;

    const ALL_PROTECTIONS: gix_validate::path::component::Options = gix_validate::path::component::Options {
        protect_windows: true,
        protect_hfs: true,
        protect_ntfs: true,
    };

    fn repo() -> gix_error::TestResult<(gix_testtools::tempfile::TempDir, Repository)> {
        let dir = gix_testtools::tempfile::tempdir()?;
        let repo = crate::init_bare(dir.path())?;
        Ok((dir, repo))
    }

    /// An entry of a tree, which is written as it is, in any order and with any mode, as nobody checks what is pushed.
    struct Entry(u16, &'static str, ObjectId);

    fn entry(mode: u16, name: &'static str, id: ObjectId) -> Entry {
        Entry(mode, name, id)
    }

    fn write_tree(repo: &Repository, entries: Vec<Entry>) -> gix_error::TestResult<ObjectId> {
        let mut data = Vec::new();
        for Entry(mode, name, id) in entries {
            data.extend_from_slice(format!("{mode:o} {name}").as_bytes());
            data.push(0);
            data.extend_from_slice(id.as_bytes());
        }
        Ok(gix_object::Write::write_buf(
            &repo.objects,
            gix_object::Kind::Tree,
            &data,
        )?)
    }

    #[test]
    fn a_tree_is_flattened_in_the_order_of_paths_with_modes_that_git_makes_of_them() -> gix_error::TestResult {
        let (_dir, repo) = repo()?;
        let blob = repo.write_blob(b"content")?.detach();
        let nested = write_tree(&repo, vec![entry(0o100644, "b", blob), entry(0o100755, "run", blob)])?;
        let root = write_tree(
            &repo,
            vec![
                entry(0o100664, "a.legacy", blob),
                entry(0o040000, "dir", nested),
                entry(0o120000, "link", blob),
                entry(0o160000, "module", blob),
            ],
        )?;
        let flat = flatten(&repo, &root, ALL_PROTECTIONS).map_err(|err| gix_error::message!("{err}"))?;
        let summary: Vec<_> = flat
            .iter()
            .map(|wanted| (wanted.path.to_string(), wanted.mode))
            .collect();
        assert_eq!(
            summary,
            [
                ("a.legacy".to_owned(), Mode::FILE),
                ("dir/b".to_owned(), Mode::FILE),
                ("dir/run".to_owned(), Mode::FILE_EXECUTABLE),
                ("link".to_owned(), Mode::SYMLINK),
                ("module".to_owned(), Mode::COMMIT),
            ],
            "the mode of the legacy file is canonical, and directories are replaced by their files, in the order of paths"
        );
        Ok(())
    }

    #[test]
    fn trees_that_cannot_be_checked_out_safely_are_refused_before_anything_is_touched() -> gix_error::TestResult {
        let (_dir, repo) = repo()?;
        let blob = repo.write_blob(b"content")?.detach();
        let empty = write_tree(&repo, vec![])?;
        let dotgit = write_tree(&repo, vec![entry(0o100644, ".git", blob)])?;
        let hfs_dotgit = write_tree(&repo, vec![entry(0o100644, ".GIT", blob)])?;
        let ntfs_dotgit = write_tree(&repo, vec![entry(0o100644, "git~1", blob)])?;
        let dotdot = write_tree(&repo, vec![entry(0o100644, "..", blob)])?;
        let slash = write_tree(&repo, vec![entry(0o100644, "a/b", blob)])?;
        let gitmodules_link = write_tree(&repo, vec![entry(0o120000, ".gitmodules", blob)])?;
        let unsorted = write_tree(&repo, vec![entry(0o100644, "b", blob), entry(0o100644, "a", blob)])?;
        let twice = write_tree(&repo, vec![entry(0o100644, "a", blob), entry(0o100644, "a", blob)])?;
        let nested = write_tree(&repo, vec![entry(0o100644, "x", blob)])?;
        let file_and_directory = write_tree(&repo, vec![entry(0o100644, "a", blob), entry(0o040000, "a", nested)])?;
        let strange_mode = write_tree(&repo, vec![entry(0o020000, "device", blob)])?;
        let in_a_directory = write_tree(&repo, vec![entry(0o040000, "dir", dotgit)])?;
        let not_a_tree = repo.write_blob(b"not a tree")?.detach();
        let missing_tree = write_tree(
            &repo,
            vec![entry(0o040000, "missing", ObjectId::empty_tree(repo.object_hash()))],
        );
        drop(missing_tree);
        let mut too_deep = empty;
        for _ in 0..=MAX_TREE_DEPTH + 1 {
            too_deep = write_tree(&repo, vec![entry(0o040000, "d", too_deep)])?;
        }
        for (what, tree_id, expected) in [
            ("a .git file", dotgit, "isn't valid"),
            ("a .GIT file that HFS+ would make into .git", hfs_dotgit, "isn't valid"),
            (
                "a git~1 file that NTFS would make into .git",
                ntfs_dotgit,
                "isn't valid",
            ),
            ("a .git directory below another", in_a_directory, "isn't valid"),
            ("a name that goes up", dotdot, "isn't valid"),
            ("a name with a slash", slash, "contains a slash"),
            ("a symbolic link as .gitmodules", gitmodules_link, "isn't valid"),
            ("entries in the wrong order", unsorted, "aren't in order"),
            ("the same name twice", twice, "aren't in order"),
            (
                "a file and a directory of the same name",
                file_and_directory,
                "both file and directory",
            ),
            ("a mode that can't be checked out", strange_mode, "can't be checked out"),
            ("a blob where there is a tree", not_a_tree, "is not a tree"),
            ("trees that are nested too deeply", too_deep, "nested more deeply"),
        ] {
            let err = flatten(&repo, &tree_id, ALL_PROTECTIONS).expect_err(what);
            assert!(
                err.contains(expected),
                "{what} is refused for the right reason, but: {err}"
            );
        }
        Ok(())
    }

    fn state_of(entries: &[(&str, Mode, u8)]) -> gix_index::State {
        let mut state = gix_index::State::new(gix_hash::Kind::Sha1);
        for (path, mode, byte) in entries {
            state.dangerously_push_entry(
                Stat::default(),
                ObjectId::from_bytes_or_panic(&[*byte; 20]),
                Flags::empty(),
                *mode,
                path.as_bytes().as_bstr(),
            );
        }
        state.sort_entries();
        state
    }

    fn wanted(entries: &[(&str, Mode, u8)]) -> Vec<Wanted> {
        entries
            .iter()
            .map(|(path, mode, byte)| Wanted {
                path: (*path).into(),
                mode: *mode,
                id: ObjectId::from_bytes_or_panic(&[*byte; 20]),
            })
            .collect()
    }

    #[test]
    fn the_index_and_the_new_tree_are_compared_like_a_one_way_merge() {
        let old = state_of(&[
            ("changed", Mode::FILE, 1),
            ("gone", Mode::FILE, 2),
            ("module", Mode::COMMIT, 3),
            ("same", Mode::FILE, 4),
            ("typed", Mode::FILE, 5),
        ]);
        let new = wanted(&[
            ("added", Mode::FILE, 9),
            ("changed", Mode::FILE, 8),
            ("module", Mode::COMMIT, 7),
            ("same", Mode::FILE, 4),
            ("typed", Mode::SYMLINK, 5),
        ]);
        let plan = plan(&old, &new);
        let paths =
            |positions: &[usize], of: &dyn Fn(usize) -> String| positions.iter().map(|&p| of(p)).collect::<Vec<_>>();
        let old_path = |position: usize| old.entries()[position].path(&old).to_string();
        let new_path = |position: usize| new[position].path.to_string();
        assert_eq!(
            paths(&plan.remove, &old_path),
            ["changed", "gone", "typed"],
            "changed and removed files go before they are created"
        );
        assert_eq!(
            paths(&plan.create, &new_path),
            ["added", "changed", "typed"],
            "what is new or differs is created"
        );
        assert_eq!(
            paths(&plan.index_only, &new_path),
            ["module"],
            "a submodule that moves only changes the index"
        );
        assert_eq!(plan.keep.len(), 1, "what is the same stays, with its stat information");
    }

    fn touch(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory can be made");
        std::fs::write(path, content).expect("a file can be written");
    }

    /// Nothing is ignored, unless a test says so.
    fn verify_absent(root: &Path, wanted: &Wanted, removed: &HashSet<&[u8]>) -> Result<Option<PathBuf>, String> {
        super::verify_absent(root, wanted, removed, &mut |_, _| false)
    }

    fn create(path: &str) -> Wanted {
        Wanted {
            path: path.into(),
            mode: Mode::FILE,
            id: ObjectId::null(gix_hash::Kind::Sha1),
        }
    }

    #[test]
    fn what_is_in_the_way_of_a_new_file_is_found_before_anything_changes() -> gix_error::TestResult {
        let dir = gix_testtools::tempfile::tempdir()?;
        let root = dir.path();
        let nothing = HashSet::new();
        assert!(
            verify_absent(root, &create("new/file"), &nothing).is_ok(),
            "nothing is in the way of a new file in a new directory"
        );

        touch(root, "untracked", "mine");
        let err = verify_absent(root, &create("untracked"), &nothing).expect_err("an untracked file is overwritten");
        assert_eq!(
            err,
            "Untracked working tree file 'untracked' would be overwritten by merge."
        );
        let removed: HashSet<&[u8]> = [b"untracked".as_slice()].into();
        assert!(
            verify_absent(root, &create("untracked"), &removed).is_ok(),
            "a tracked file that is removed first is not in the way"
        );

        std::fs::create_dir(root.join("empty"))?;
        assert!(
            verify_absent(root, &create("empty"), &nothing).is_ok(),
            "an empty directory is removed to make way for a file, like Git does"
        );
        touch(root, "occupied/untracked", "mine");
        touch(root, "occupied/tracked", "theirs");
        let err = verify_absent(root, &create("occupied"), &nothing).expect_err("untracked files would be lost");
        assert_eq!(err, "Updating 'occupied' would lose untracked files in it");
        let removed: HashSet<&[u8]> = [b"occupied/untracked".as_slice(), b"occupied/tracked".as_slice()].into();
        assert!(
            verify_absent(root, &create("occupied"), &removed).is_ok(),
            "a directory whose files are all tracked and removed is no obstacle"
        );

        touch(root, "file", "mine");
        let err = verify_absent(root, &create("file/below"), &nothing).expect_err("a file is not a directory");
        assert_eq!(err, "Untracked working tree file 'file' would be overwritten by merge.");
        Ok(())
    }

    #[test]
    fn ignored_files_are_in_the_way_of_nothing_but_have_to_go() -> gix_error::TestResult {
        let dir = gix_testtools::tempfile::tempdir()?;
        let root = dir.path();
        let nothing = HashSet::new();
        let ignores =
            |names: &'static [&'static str]| move |path: &BStr, _is_dir: bool| names.iter().any(|name| path == *name);

        touch(root, "ignored", "mine");
        assert_eq!(
            super::verify_absent(root, &create("ignored"), &nothing, &mut ignores(&["ignored"]))?,
            Some(root.join("ignored")),
            "an ignored file is overwritten, so it is to be removed first"
        );
        assert!(
            super::verify_absent(root, &create("ignored"), &nothing, &mut ignores(&[])).is_err(),
            "without the rule that ignores it, it is an untracked file that is in the way"
        );

        touch(root, "dir/ignored", "mine");
        touch(root, "dir/untracked", "mine");
        assert!(
            super::verify_absent(root, &create("dir"), &nothing, &mut ignores(&["dir/ignored"])).is_err(),
            "one untracked file that isn't ignored keeps the directory"
        );
        assert_eq!(
            super::verify_absent(root, &create("dir"), &nothing, &mut ignores(&["dir"]))?,
            Some(root.join("dir")),
            "a directory that is ignored as a whole is lost with all it has"
        );
        assert_eq!(
            super::verify_absent(
                root,
                &create("dir"),
                &nothing,
                &mut ignores(&["dir/ignored", "dir/untracked"])
            )?,
            Some(root.join("dir")),
            "a directory with ignored files only is lost too"
        );

        touch(root, "leading", "mine");
        assert_eq!(
            super::verify_absent(root, &create("leading/below"), &nothing, &mut ignores(&["leading"]))?,
            Some(root.join("leading")),
            "an ignored file where a directory is to be is replaced by it"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_in_the_way_is_never_followed_out_of_the_work_tree() -> gix_error::TestResult {
        let dir = gix_testtools::tempfile::tempdir()?;
        let root = dir.path().join("work");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&outside)?;
        std::os::unix::fs::symlink(&outside, root.join("link"))?;
        let nothing = HashSet::new();
        let err = verify_absent(&root, &create("link/escaped"), &nothing)
            .expect_err("an untracked link to a directory is in the way of a file below it");
        assert_eq!(err, "Untracked working tree file 'link' would be overwritten by merge.");
        assert!(
            std::fs::read_dir(&outside)?.next().is_none(),
            "the verification doesn't write, and certainly not outside"
        );
        let removed: HashSet<&[u8]> = [b"link".as_slice()].into();
        assert!(
            verify_absent(&root, &create("link/inside"), &removed).is_ok(),
            "a tracked link that is removed first, as the push turns it into a directory, is not in the way"
        );
        Ok(())
    }
}
