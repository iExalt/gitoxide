//! Objects written as one pack, with Git as the oracle: it has to read what was written, find every object in a single pack
//! and no loose object, and the ids have to be those that Git computes.
use std::sync::atomic::AtomicBool;

use gix_hash::Kind;
use gix_object::{Kind as ObjectKind, WriteTo};
use gix_testtools::TestResult;

fn packs(repo: &gix::Repository) -> std::io::Result<Vec<String>> {
    let mut names: Vec<_> = std::fs::read_dir(repo.objects.store_ref().path().join("pack"))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| std::path::Path::new(name).extension().is_some_and(|ext| ext == "pack"))
        .collect();
    names.sort();
    Ok(names)
}

fn serialized(object: &dyn WriteTo) -> Vec<u8> {
    let mut out = Vec::new();
    object.write_to(&mut out).expect("serializing to memory works");
    out
}

#[test]
fn objects_of_every_kind_are_written_into_one_pack_that_git_reads() -> TestResult {
    for kind in [Kind::Sha1, Kind::Sha256]
        .into_iter()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
    {
        let dir = gix_testtools::tempfile::tempdir()?;
        gix_testtools::git(dir.path(), &format!("init -q --bare --object-format={kind} ."))?;
        let repo = gix::open_opts(dir.path(), crate::restricted())?;
        let interrupt = AtomicBool::new(false);

        let blob = b"{\"state\":1}\n".to_vec();
        let blob_id = gix_object::compute_hash(kind, ObjectKind::Blob, &blob)?;
        let tree = serialized(&gix_object::Tree {
            entries: vec![gix_object::tree::Entry {
                mode: gix_object::tree::EntryKind::Blob.into(),
                filename: "a.json".into(),
                oid: blob_id,
            }],
        });
        let tree_id = gix_object::compute_hash(kind, ObjectKind::Tree, &tree)?;
        let signature = gix_actor::Signature {
            name: "acyclic mac".into(),
            email: "mac@acyclic.invalid".into(),
            time: gix_date::Time::new(1_792_022_400, 0),
        };
        let commit = serialized(&gix_object::Commit {
            tree: tree_id,
            parents: Default::default(),
            author: signature.clone(),
            committer: signature,
            encoding: None,
            message: "batch".into(),
            extra_headers: Vec::new(),
        });

        let commit_id_for_tag = gix_object::compute_hash(kind, ObjectKind::Commit, &commit)?;
        let tag = serialized(&gix_object::Tag {
            target: commit_id_for_tag,
            target_kind: ObjectKind::Commit,
            name: "v1".into(),
            tagger: None,
            message: "a tag\n".into(),
            signature: None,
        });
        let objects = || {
            [
                (ObjectKind::Blob, blob.clone()),
                (ObjectKind::Tree, tree.clone()),
                (ObjectKind::Commit, commit.clone()),
                (ObjectKind::Blob, blob.clone()),
                (ObjectKind::Tag, tag.clone()),
            ]
        };
        let ids = repo.write_objects_as_pack(objects(), &interrupt)?;
        assert_eq!(ids.len(), 5, "{kind}: one id for each object that was given, in order");
        assert_eq!(
            (ids[0], ids[1]),
            (blob_id, tree_id),
            "{kind}: the ids are those of the objects"
        );
        assert_eq!(ids[3], blob_id, "{kind}: an object that is given twice has one id");

        let (blob_id, tree_id, commit_id) = (ids[0], ids[1], ids[2]);
        let blob_file = dir.path().join("blob.json");
        std::fs::write(&blob_file, b"{\"state\":1}\n")?;
        assert_eq!(
            gix_testtools::git(dir.path(), &format!("hash-object -t blob {}", blob_file.display()))?.trim(),
            blob_id.to_string(),
            "{kind}: the id of the blob is the one that Git computes"
        );
        assert_eq!(packs(&repo)?.len(), 1, "{kind}: the objects are in a single pack");
        assert!(
            std::fs::read_dir(repo.objects.store_ref().path().join("pack"))?
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".keep")),
            "{kind}: no `.keep` file protects the pack, as with `fast-import`"
        );
        let counts = gix_testtools::git(dir.path(), "count-objects -v")?;
        assert!(
            counts.contains("count: 0") && counts.contains("in-pack: 4") && counts.contains("packs: 1"),
            "{kind}: no loose object, four in the pack, which dropped the duplicate: {counts}"
        );
        assert_eq!(
            gix_testtools::git(dir.path(), &format!("cat-file -t {commit_id}"))?.trim(),
            "commit",
            "{kind}: Git finds the commit"
        );
        assert_eq!(
            gix_testtools::git(dir.path(), &format!("ls-tree -r {commit_id}"))?
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["100644", "blob", &blob_id.to_string(), "a.json"],
            "{kind}: Git reads the tree and the blob through the commit, with the tree {tree_id}"
        );
        gix_testtools::git(dir.path(), "fsck --strict --full")?;
        assert_eq!(
            repo.find_object(commit_id)?.kind,
            ObjectKind::Commit,
            "{kind}: the handle that wrote the pack finds the objects without being opened again"
        );
        #[cfg(unix)]
        for name in std::fs::read_dir(repo.objects.store_ref().path().join("pack"))? {
            use std::os::unix::fs::PermissionsExt;
            let mode = name?.metadata()?.permissions().mode() & 0o777;
            assert_eq!(
                mode & 0o222,
                0,
                "{kind}: pack files are read-only, as Git makes them, but one has mode {mode:o}"
            );
        }

        assert_eq!(
            repo.write_objects_as_pack(objects(), &interrupt)?,
            ids,
            "{kind}: the same objects in the same order give the same ids"
        );
        assert_eq!(
            packs(&repo)?.len(),
            1,
            "{kind}: and the pack that is there already, which isn't written twice"
        );
        assert_eq!(
            repo.write_objects_as_pack([(ObjectKind::Blob, b"{\"state\":1}\n".to_vec())], &interrupt)?,
            vec![blob_id],
            "{kind}: an object that is there already is written again, to be in this pack"
        );
        assert_eq!(packs(&repo)?.len(), 2, "{kind}: as a pack of its own");
        assert!(
            std::fs::read_dir(repo.objects.store_ref().path().join("pack"))?
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with("tmp_pack_dir_")),
            "{kind}: the directory in which the pack is made is gone"
        );
        let none: [(ObjectKind, Vec<u8>); 0] = [];
        assert!(repo.write_objects_as_pack(none, &interrupt)?.is_empty());
        assert_eq!(packs(&repo)?.len(), 2, "{kind}: nothing is written for no objects");
        gix_testtools::git(dir.path(), "fsck --strict --full")?;
    }
    Ok(())
}

#[test]
fn an_interrupt_writes_nothing() -> TestResult {
    let dir = gix_testtools::tempfile::tempdir()?;
    gix_testtools::git(dir.path(), "init -q --bare .")?;
    let repo = gix::open_opts(dir.path(), crate::restricted())?;
    let interrupt = AtomicBool::new(true);
    let res = repo.write_objects_as_pack([(ObjectKind::Blob, b"x".to_vec())], &interrupt);
    assert!(res.is_err(), "the interrupt is an error");
    assert!(
        packs(&repo)?.is_empty(),
        "and leaves no pack, and no temporary file behind"
    );
    assert_eq!(
        std::fs::read_dir(repo.objects.store_ref().path().join("pack"))?.count(),
        0,
        "the pack directory is empty"
    );
    Ok(())
}

/// What a writer that was killed left, and what a pack made by an earlier run is worth: nothing here may be lost to maintenance.
#[test]
fn a_pack_of_unreachable_objects_survives_gc_and_what_a_kill_leaves_is_pruned() -> TestResult {
    let dir = gix_testtools::tempfile::tempdir()?;
    gix_testtools::git(dir.path(), "init -q --bare .")?;
    let repo = gix::open_opts(dir.path(), crate::restricted())?;
    let ids = repo.write_objects_as_pack(
        [(ObjectKind::Blob, b"not reachable yet".to_vec())],
        &AtomicBool::new(false),
    )?;

    // The caller makes its objects reachable after this, so for a while no reference leads to them.
    repo.gc(gix::maintenance::gc::Options::default(), &AtomicBool::new(false))?;
    gix_testtools::git(dir.path(), &format!("cat-file -e {}", ids[0]))?;

    // A kill during the write leaves its directory behind, which pruning removes once it is old enough.
    let left = repo.objects.store_ref().path().join("pack/tmp_pack_dir_1_2_3");
    std::fs::create_dir(&left)?;
    std::fs::write(left.join(".tmpAbCdEf"), b"half a pack")?;
    let mut options = gix::maintenance::prune::Options::default();
    options.expire = Some(gix_date::Time::new(4_000_000_000, 0));
    let outcome = repo.prune(options)?;
    assert!(
        !left.exists(),
        "the leftover is removed ({} temporary files and directories)",
        outcome.temporary
    );
    gix_testtools::git(dir.path(), &format!("cat-file -e {}", ids[0]))?;
    Ok(())
}
