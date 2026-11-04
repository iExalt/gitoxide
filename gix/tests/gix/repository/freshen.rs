//! Writing an object that exists must keep the next pruning from removing it, with `git gc --prune` as the oracle.
//!
//! Skipping the write of an object that is already there is only safe if the object doesn't expire before a reference makes it
//! reachable. See `make_unreachable_old_objects.sh` for the old and unreachable objects that each kind of storage has.
use gix_hash::Kind;
use gix_testtools::{Creation, TestResult};

fn exists_for_git(repo_dir: &std::path::Path, id: gix_hash::ObjectId) -> TestResult<bool> {
    Ok(gix_testtools::git_command(repo_dir)
        .args(["cat-file", "-e"])
        .arg(id.to_string())
        .status()?
        .success())
}

#[test]
fn writing_an_old_unreachable_object_again_keeps_it_from_being_pruned() -> TestResult {
    for kind in [Kind::Sha1, Kind::Sha256]
        .into_iter()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
    {
        // Run the script in place: copying a fixture would date every file now, and the age of the objects is the point.
        let fixtures = gix_testtools::scripted_fixture_writable_with_args(
            "make_unreachable_old_objects.sh",
            [kind.to_string()],
            Creation::Execute,
        )?;
        for storage in ["loose", "pack", "cruft"] {
            let repo_dir = fixtures.path().join(storage);
            let repo = gix::open_opts(&repo_dir, crate::restricted())?;
            let blob_id = |name: &str| {
                gix_object::compute_hash(kind, gix_object::Kind::Blob, format!("{storage}-{name}\n").as_bytes())
            };
            let (written, control) = (blob_id("written")?, blob_id("control")?);
            assert!(
                exists_for_git(&repo_dir, written)? && exists_for_git(&repo_dir, control)?,
                "{kind} {storage}: both blobs exist before, or the fixture is wrong"
            );

            let id = repo.write_blob(format!("{storage}-written\n"))?;
            assert_eq!(id, written, "{kind} {storage}: it writes the blob the fixture has");

            gix_testtools::git(&repo_dir, "gc --prune=1.day.ago")?;
            assert!(
                exists_for_git(&repo_dir, written)?,
                "{kind} {storage}: a blob that was written again survives pruning, as it is new now"
            );
            assert!(
                !exists_for_git(&repo_dir, control)?,
                "{kind} {storage}: its untouched twin is pruned, which shows that only its age protected the first"
            );
        }
    }
    Ok(())
}
