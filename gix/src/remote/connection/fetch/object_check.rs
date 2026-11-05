//! Check the objects of a pack that was received by a fetch before they become part of the repository, as `git fetch` does when
//! `fetch.fsckObjects` or `transfer.fsckObjects` is set.
//!
//! Git runs `index-pack --strict` on the pack it receives, which checks the objects before the pack is moved into place. The pack is
//! written to a directory of its own next to the other packs, `objects/pack/tmp_pack_dir_*`, to be checked there and moved into
//! `objects/pack` only if it is accepted. Until then nobody can find its objects, neither can anyone mistake it for something complete
//! when the process is killed meanwhile, and what is left behind then is what `Repository::prune()` removes once it is old enough,
//! like Git's `tmp_pack_*` files.
use std::{io::BufRead, path::Path, sync::atomic::AtomicBool};

use gix_error::{ResultExt, bail, message};
use gix_hash::ObjectId;
use gix_object::bstr::{BString, ByteSlice};
use gix_odb::inspect::{Inspection, RecordingFind};
use gix_utils::progress::DynNestedProgress;

use crate::{Repository, Result, scratch::Scratch};

/// The error of a fetch that was refused because an object that was received didn't pass the check, which can be found by
/// [downcasting](gix_error::Error::downcast_any_ref()) the error of the fetch.
///
/// Nothing of what was received is in the repository, and neither the references nor the shallow file have changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectsRefused {
    /// What Git prints about the objects, one line per problem, with the prefix `error: `, `warning: ` or `fatal: `
    /// and a newline at the end of each.
    pub report: BString,
}

impl std::fmt::Display for ObjectsRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the received objects were refused:\n{}",
            self.report.trim_end().as_bstr()
        )
    }
}

impl std::error::Error for ObjectsRefused {}

/// Return `true` if objects are fetched from a remote that is a promisor, which is where the objects that a pack needs may be left out,
/// and which Git doesn't check the links of as a result.
pub(super) fn is_promisor(repo: &Repository, remote_name: Option<&str>) -> bool {
    let config = &repo.config.resolved;
    remote_name.is_some_and(|name| {
        config
            .boolean(format!("remote.{name}.promisor").as_str())
            .is_ok_and(|value| value.unwrap_or(false))
    }) || config.string("extensions.partialClone").is_some()
}

/// What to check, and how.
pub(super) struct Checking<'a> {
    pub options: &'a gix_fsck::Options,
    /// The commits that the shallow file will have, the parents of which are not there.
    pub shallow_boundary: &'a [ObjectId],
    /// If `false`, what the objects link to doesn't have to be there, as the remote may have left objects out on purpose.
    pub links: bool,
}

/// Write the pack in `reader` into `repo` like `gix_pack::Bundle::write_to_directory()` does for its `objects/pack` directory, but
/// check its objects before that if `checking` says so, and return what Git would warn about.
///
/// The pack isn't there to find if the check fails. Files that exist already aren't replaced.
pub(super) fn write_pack(
    repo: &Repository,
    reader: &mut dyn BufRead,
    progress: &mut dyn DynNestedProgress,
    should_interrupt: &AtomicBool,
    options: gix_pack::bundle::write::Options,
    checking: Checking<'_>,
) -> Result<(gix_pack::bundle::write::Outcome, BString)> {
    let pack_dir = repo.objects.store_ref().path().join("pack");
    std::fs::create_dir_all(&pack_dir).or_raise(|| message!("Could not create {}", pack_dir.display()))?;
    let scratch = Scratch::new(&pack_dir)?;
    let object_hash = repo.object_hash();
    let thin = RecordingFind::new(repo.objects.clone());
    let mut written = gix_pack::Bundle::write_to_directory(
        reader,
        Some(scratch.path()),
        progress,
        should_interrupt,
        Some(thin.clone()),
        object_hash,
        options,
    )
    .or_raise(|| message("Failed to write the received pack"))?;

    let mut warnings = BString::default();
    if let Some(index_path) = &written.index_path {
        let bundle = gix_pack::Bundle::at(index_path, object_hash)
            .or_raise(|| message("Could not open the received pack to check its objects"))?;
        let thin_bases = thin.take_requested();
        let inspection = Inspection::new(&bundle, &thin_bases, &repo.objects, true);
        let outcome = crate::fsck::check(
            checking.options,
            object_hash,
            gix_fsck::Program::IndexPack,
            checking.shallow_boundary,
            checking.links,
            &inspection,
            Some(should_interrupt),
        )?;
        if !outcome.accepted {
            bail!(ObjectsRefused { report: outcome.report });
        }
        warnings = outcome.report;
    }
    move_into_place(&mut written, &pack_dir)?;
    Ok((written, warnings))
}

/// Move the files of the pack that were written to the directory of `written` into `pack_dir`, in the order that makes the pack
/// safe at each point: the `.keep` file that keeps it from being collected, the pack, and then the index which makes it findable.
/// A pack that is there already is left alone and so is its index, as it has the same objects, and has no `.keep` file made for it.
fn move_into_place(written: &mut gix_pack::bundle::write::Outcome, pack_dir: &Path) -> Result<()> {
    let (Some(data_path), Some(index_path)) = (written.data_path.clone(), written.index_path.clone()) else {
        return Ok(());
    };
    let destination = |path: &Path| pack_dir.join(path.file_name().expect("files of a pack have a name"));
    let (data_destination, index_destination) = (destination(&data_path), destination(&index_path));
    if data_destination.is_file() {
        written.keep_path = None;
    } else {
        if let Some(keep_path) = &written.keep_path {
            let keep_destination = destination(keep_path);
            std::fs::rename(keep_path, &keep_destination)
                .or_raise(|| message!("Could not move the keep file to {}", keep_destination.display()))?;
            written.keep_path = Some(keep_destination);
        }
        std::fs::rename(&data_path, &data_destination)
            .or_raise(|| message!("Could not move the received pack to {}", data_destination.display()))?;
    }
    if !index_destination.is_file() {
        std::fs::rename(&index_path, &index_destination).or_raise(|| {
            message!(
                "Could not move the index of the received pack to {}",
                index_destination.display()
            )
        })?;
    }
    written.data_path = Some(data_destination);
    written.index_path = Some(index_destination);
    Ok(())
}

#[cfg(test)]
mod tests {
    use gix_testtools::TestResult;

    use super::move_into_place;

    fn written(dir: &std::path::Path) -> gix_pack::bundle::write::Outcome {
        let data_path = dir.join("pack-x.pack");
        let index_path = dir.join("pack-x.idx");
        let keep_path = dir.join("pack-x.keep");
        for path in [&data_path, &index_path, &keep_path] {
            std::fs::write(path, path.extension().and_then(|e| e.to_str()).unwrap_or_default()).expect("writable");
        }
        gix_pack::bundle::write::Outcome {
            index: gix_pack::index::write::Outcome {
                index_version: gix_pack::index::Version::V2,
                index_hash: gix_hash::Kind::Sha1.null(),
                data_hash: gix_hash::Kind::Sha1.null(),
                num_objects: 1,
            },
            pack_version: gix_pack::data::Version::V2,
            object_hash: gix_hash::Kind::Sha1,
            index_path: Some(index_path),
            data_path: Some(data_path),
            keep_path: Some(keep_path),
        }
    }

    #[test]
    fn a_checked_pack_is_moved_with_its_keep_file() -> TestResult {
        let tmp = gix_testtools::tempfile::tempdir()?;
        let (staging, pack_dir) = (tmp.path().join("staging"), tmp.path().join("pack"));
        std::fs::create_dir_all(&staging)?;
        std::fs::create_dir_all(&pack_dir)?;
        let mut outcome = written(&staging);
        move_into_place(&mut outcome, &pack_dir)?;
        assert_eq!(
            outcome.keep_path,
            Some(pack_dir.join("pack-x.keep")),
            "the keep file stays with the pack"
        );
        assert_eq!(outcome.data_path, Some(pack_dir.join("pack-x.pack")));
        assert_eq!(outcome.index_path, Some(pack_dir.join("pack-x.idx")));
        for name in ["pack-x.keep", "pack-x.pack", "pack-x.idx"] {
            assert!(pack_dir.join(name).is_file(), "{name} was moved");
            assert!(!staging.join(name).exists(), "{name} isn't left behind");
        }
        Ok(())
    }

    #[test]
    fn a_pack_that_is_there_already_is_not_replaced_and_gets_no_keep_file() -> TestResult {
        let tmp = gix_testtools::tempfile::tempdir()?;
        let (staging, pack_dir) = (tmp.path().join("staging"), tmp.path().join("pack"));
        std::fs::create_dir_all(&staging)?;
        std::fs::create_dir_all(&pack_dir)?;
        std::fs::write(pack_dir.join("pack-x.pack"), "existing pack")?;
        std::fs::write(pack_dir.join("pack-x.idx"), "existing index")?;
        let mut outcome = written(&staging);
        move_into_place(&mut outcome, &pack_dir)?;
        assert_eq!(
            outcome.keep_path, None,
            "a pack that was there needs no protection of ours"
        );
        assert_eq!(std::fs::read_to_string(pack_dir.join("pack-x.pack"))?, "existing pack");
        assert_eq!(std::fs::read_to_string(pack_dir.join("pack-x.idx"))?, "existing index");
        assert!(
            !pack_dir.join("pack-x.keep").exists(),
            "no keep file is made for what was there"
        );
        Ok(())
    }
}
