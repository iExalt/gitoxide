//! The backend-neutral view must behave exactly like the files store it is created from, as that is what everything
//! is migrated onto before other kinds of storage exist.
use gix_lock::acquire::Fail;
use gix_ref::{StoreRef, view::Kind};

use crate::file::{
    store_at, store_with_packed_refs, store_writable,
    transaction::prepare_and_commit::{committer, create_at},
};

struct AnyObjectIsACommit;
impl gix_object::Find for AnyObjectIsACommit {
    fn try_find<'a>(
        &self,
        id: &gix_hash::oid,
        _buffer: &'a mut Vec<u8>,
    ) -> gix_error::Result<Option<gix_object::Data<'a>>> {
        Ok(Some(gix_object::Data {
            kind: gix_object::Kind::Commit,
            object_hash: id.kind(),
            data: &[],
        }))
    }
}

/// Items can fail individually, and the fixture has a broken reference, so compare results rather than only successes.
fn outcomes(
    iter: impl Iterator<Item = gix_error::Result<gix_ref::Reference>>,
) -> Vec<Result<gix_ref::Reference, String>> {
    iter.map(|res| res.map_err(|err| err.to_string())).collect()
}

#[test]
fn access_reports_what_the_files_store_has() -> crate::Result {
    let store = store_at("make_ref_repository.sh")?;
    let view = StoreRef::from(&store);
    assert_eq!(view.kind(), Kind::Files);
    assert_eq!(view.git_dir(), store.git_dir());
    assert_eq!(view.common_dir(), store.common_dir());
    assert_eq!(view.object_hash(), crate::fixture_hash_kind());
    assert!(view.namespace().is_none());
    assert_eq!(view.write_reflog(), store.write_reflog);
    assert_eq!(view.unsupported_backend(), None);
    view.assure_backend_supported()?;
    assert!(
        std::ptr::eq(view.as_file().expect("files"), &store),
        "the escape hatch yields the very store the view borrows"
    );
    Ok(())
}

#[test]
fn lookups_match_the_files_store_including_packed_refs() -> crate::Result {
    let store = store_with_packed_refs()?;
    let view = StoreRef::from(&store);
    for name in ["main", "HEAD", "refs/heads/main", "dt1", "tags/dt1", "missing"] {
        assert_eq!(
            view.try_find(name)?,
            store.try_find(name)?,
            "{name}: the view finds the same reference as the store"
        );
        assert_eq!(
            view.try_find_resolved(name)?,
            store.try_find_resolved(name)?,
            "{name}: resolving lookups agree"
        );
    }
    assert!(view.find("main").is_ok(), "existing references are found");
    assert!(
        view.find("missing").expect_err("missing").is_not_found(),
        "a missing reference is reported with the not-found class"
    );
    assert!(view.find_resolved("missing").expect_err("missing").is_not_found());
    assert!(view.try_find("missing")?.is_none());
    Ok(())
}

#[test]
fn iteration_matches_the_files_store_for_every_kind_of_traversal() -> crate::Result {
    let store = store_with_packed_refs()?;
    let view = StoreRef::from(&store);
    let platform = view.iter()?;
    let expected = store.iter()?;

    let all = outcomes(platform.all()?);
    assert_eq!(
        all,
        outcomes(expected.all()?),
        "all references come in the same order, with the same failures"
    );
    assert!(
        all.iter().any(Result::is_err),
        "a broken reference fails only its own item and doesn't end the iteration"
    );
    let prefix = <&gix_path::RelativePath>::try_from("refs/heads/")?;
    let prefixed = outcomes(platform.prefixed(prefix)?);
    assert_eq!(prefixed, outcomes(expected.prefixed(prefix)?));
    assert!(!prefixed.is_empty(), "the traversal isn't vacuous");
    assert_eq!(
        outcomes(platform.pseudo()?),
        outcomes(expected.pseudo()?),
        "pseudo references agree"
    );
    Ok(())
}

#[test]
fn snapshots_find_iterate_and_follow_like_the_store() -> crate::Result {
    let store = store_with_packed_refs()?;
    let snapshot = StoreRef::from(&store).snapshot()?;
    assert_eq!(snapshot.store().git_dir(), store.git_dir());

    for name in ["main", "dt1", "missing"] {
        assert_eq!(snapshot.try_find(name)?, store.try_find(name)?, "{name}");
    }
    assert!(snapshot.find("missing").expect_err("missing").is_not_found());
    assert_eq!(
        outcomes(snapshot.iter().all()?),
        outcomes(store.iter()?.all()?),
        "iterating a snapshot is the same as iterating the store"
    );

    let head = snapshot.find("HEAD")?;
    let followed = snapshot
        .follow(&head)
        .expect("HEAD is symbolic")
        .expect("its target exists");
    assert_eq!(followed.name.as_bstr(), "refs/heads/main");
    assert!(snapshot.follow(&followed).is_none(), "a direct reference is a leaf");

    let mut head = snapshot.find("HEAD")?;
    let id = snapshot.follow_to_object(&mut head)?;
    assert_eq!(
        head.target.try_id(),
        Some(id.as_ref()),
        "the reference is altered in place"
    );
    assert_eq!(id, store.find("main")?.target.into_id());

    let mut head = snapshot.find("HEAD")?;
    assert_eq!(
        snapshot.peel_to_id(&mut head, &AnyObjectIsACommit)?,
        id,
        "peeling a commit yields the commit"
    );
    Ok(())
}

#[test]
fn reflogs_match_the_files_store_in_both_directions() -> crate::Result {
    let store = store_at("make_repo_for_reflog.sh")?;
    let view = StoreRef::from(&store);
    let name: &gix_ref::FullNameRef = "refs/heads/old".try_into()?;

    let mut buf = Vec::new();
    let expected_all = store
        .reflog_iter(name, &mut buf)?
        .expect("exists")
        .map(|line| line.map(gix_ref::log::Line::from))
        .collect::<Result<Vec<_>, _>>()?;
    let expected_rev = store
        .reflog_iter_rev(name, &mut vec![0; 4096])?
        .expect("exists")
        .collect::<Result<Vec<_>, _>>()?;

    let mut platform = view.reflog(name);
    let all = platform.all()?.expect("exists").collect::<Result<Vec<_>, _>>()?;
    assert_eq!(all, expected_all, "oldest first");
    assert!(all.len() > 1, "the log has multiple lines so that the order matters");

    let rev = platform.rev()?.expect("exists").collect::<Result<Vec<_>, _>>()?;
    assert_eq!(rev, expected_rev, "newest first, and the same lines as the store");
    assert_eq!(
        rev.iter().rev().cloned().collect::<Vec<_>>(),
        all,
        "both directions list the same lines in opposite order"
    );

    assert!(view.reflog_exists(name)?);
    let missing: &gix_ref::FullNameRef = "refs/heads/does-not-exist".try_into()?;
    assert!(!view.reflog_exists(missing)?);
    assert!(
        view.reflog(missing).rev()?.is_none(),
        "a missing reflog is None, not an error"
    );
    assert!(view.reflog(missing).all()?.is_none());
    Ok(())
}

#[test]
fn transactions_create_and_roll_back_through_the_view() -> crate::Result {
    let (_dir, store) = store_writable("make_ref_repository.sh")?;
    let view = StoreRef::from(&store);
    let lock = gix_ref::view::transaction::LockOptions::new(Fail::Immediately, Fail::Immediately);

    let name = "refs/heads/created-through-the-view";
    let rolled_back = view.transaction().prepare([create_at(name)], lock)?.rollback();
    assert_eq!(rolled_back.len(), 1, "the prepared edits are handed back");
    assert!(view.try_find(name)?.is_none(), "a rollback changes nothing");

    let outcome = view
        .transaction()
        .packed_refs(gix_ref::view::transaction::PackedRefs::DeletionsOnly)
        .prepare([create_at(name)], lock)?
        .commit(committer().to_ref(&mut Default::default()))?;
    assert_eq!(outcome.edits.len(), 1);
    let created = view.find(name)?;
    assert_eq!(
        created.target.try_id().map(ToOwned::to_owned),
        Some(crate::hex_to_id("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")),
    );
    assert!(
        view.reflog_exists(name)?,
        "the edit's reflog was written with the committer"
    );
    assert_eq!(
        store.try_find(name)?,
        Some(created),
        "what the view committed is what the store reads"
    );
    Ok(())
}

#[test]
fn a_marked_store_is_refused_through_every_part_of_the_view() -> crate::Result {
    let (_dir, mut store) = store_writable("make_ref_repository.sh")?;
    store.set_unsupported_backend(Some("reftable".into()));
    let view = StoreRef::from(&store);
    assert_eq!(view.unsupported_backend(), Some("reftable"));

    let refused = |what: &str, err: gix_error::Error| assert!(err.is_unsupported(), "{what}: {err:?}");
    refused("assure", view.assure_backend_supported().expect_err("refused"));
    refused("try_find", view.try_find("main").expect_err("refused"));
    refused("find", view.find("main").expect_err("refused"));
    refused(
        "try_find_resolved",
        view.try_find_resolved("main").expect_err("refused"),
    );
    refused("snapshot", view.snapshot().err().expect("refused"));
    assert_eq!(
        view.iter().err().map(|err| err.is_unsupported()),
        Some(true),
        "iteration is refused when creating the platform"
    );
    let name: &gix_ref::FullNameRef = "refs/heads/main".try_into()?;
    refused("reflog", view.reflog(name).rev().err().expect("refused"));
    refused(
        "transaction",
        view.transaction()
            .prepare(
                [create_at("refs/heads/new")],
                gix_ref::view::transaction::LockOptions::new(Fail::Immediately, Fail::Immediately),
            )
            .err()
            .expect("refused"),
    );
    Ok(())
}

mod owned_store {
    use gix_ref::{Store, store::WriteReflog};

    use crate::file::store_at;

    #[test]
    fn it_is_the_files_store_in_an_owned_neutral_wrapper() -> crate::Result {
        let files = store_at("make_ref_repository.sh")?;
        let mut store = Store::from(files.clone());
        assert_eq!(store.git_dir(), files.git_dir());
        assert_eq!(store.common_dir(), files.common_dir());
        assert_eq!(store.object_hash(), files.object_hash());
        assert_eq!(
            store.as_ref().try_find("main")?,
            files.try_find("main")?,
            "the borrowed view reads what the wrapped store reads"
        );
        assert!(store.as_file().is_some(), "files are what this is");

        assert_eq!(store.set_write_reflog(WriteReflog::Always), files.write_reflog);
        assert_eq!(store.write_reflog(), WriteReflog::Always);

        assert!(store.set_namespace(Some(gix_ref::namespace::expand("ns")?)).is_none());
        assert_eq!(
            store.namespace().map(|ns| ns.as_bstr().to_owned()),
            Some("refs/namespaces/ns/".into())
        );
        assert!(
            store.as_ref().try_find("main")?.is_none(),
            "the namespace limits what is found, like it does for the files store"
        );
        assert!(
            store.set_namespace(None).is_some(),
            "the previous namespace is returned"
        );
        assert!(store.as_ref().try_find("main")?.is_some());

        store.set_unsupported_backend(Some("reftable".into()));
        assert_eq!(store.unsupported_backend(), Some("reftable"));
        assert!(
            store.as_ref().try_find("main").is_err(),
            "the mark applies to the view of the owned store"
        );
        Ok(())
    }

    #[test]
    fn with_locations_keeps_everything_but_the_places() -> crate::Result {
        let files = store_at("make_ref_repository.sh")?;
        let mut store = Store::from(files.clone());
        store.set_write_reflog(WriteReflog::Disable);
        store.set_namespace(Some(gix_ref::namespace::expand("ns")?));
        store.set_unsupported_backend(Some("reftable".into()));

        let moved = store.with_locations(files.git_dir().to_owned(), None);
        assert_eq!(moved.git_dir(), files.git_dir());
        assert_eq!(
            moved.as_file().expect("files").precompose_unicode,
            files.precompose_unicode,
            "options of the files store are retained"
        );
        assert_eq!(moved.write_reflog(), WriteReflog::Disable);
        assert_eq!(moved.namespace(), store.namespace());
        assert_eq!(moved.unsupported_backend(), Some("reftable"));

        let linked = store.with_locations("linked".into(), Some("main".into()));
        assert_eq!(linked.git_dir(), std::path::Path::new("linked"));
        assert_eq!(linked.common_dir(), Some(std::path::Path::new("main")));
        Ok(())
    }
}
