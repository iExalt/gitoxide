use std::path::Path;

use gix_lock::acquire::Fail;
use gix_ref::bstr::ByteSlice;

use crate::file::{store, store_writable, transaction::prepare_and_commit::create_at};

const BACKEND: &str = "reftable";

fn paths_below(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    fn recurse(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            out.push(path.clone());
            if path.is_dir() {
                recurse(&path, out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    recurse(dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn assert_unsupported<T>(what: &str, res: gix_error::Result<T>) {
    let err = res.err().unwrap_or_else(|| panic!("{what} must be refused"));
    assert!(
        err.is_unsupported(),
        "{what}: refusal is classified as unsupported, so callers can switch strategy: {err:?}"
    );
}

fn assert_unsupported_io<T>(what: &str, res: std::io::Result<T>) {
    let err = res.err().unwrap_or_else(|| panic!("{what} must be refused"));
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::Unsupported,
        "{what}: refusal maps to the unsupported class"
    );
}

#[test]
fn the_marked_store_refuses_every_operation_on_references_instead_of_reporting_them_as_missing() -> crate::Result {
    let mut store = store()?;
    assert_eq!(store.unsupported_backend(), None, "stores are usable by default");
    let main_exists = store.try_find("main")?.is_some();
    assert!(
        main_exists,
        "the fixture has the reference that must not appear to be missing"
    );

    store.set_unsupported_backend(Some(BACKEND.into()));
    assert_eq!(store.unsupported_backend(), Some(BACKEND));
    assert_eq!(
        store.clone().unsupported_backend(),
        Some(BACKEND),
        "clones, which is how thread-local copies are made, are refused as well"
    );

    assert_unsupported("try_find", store.try_find("main"));
    assert_unsupported("try_find_loose", store.try_find_loose("main"));
    assert_unsupported("try_find_packed", store.try_find_packed("main", None));
    assert_unsupported("try_find_resolved", store.try_find_resolved("main"));
    assert_unsupported("find", store.find("main"));
    assert_unsupported("find_loose", store.find_loose("main"));
    assert_unsupported("find_packed", store.find_packed("main", None));
    assert_unsupported("find_resolved", store.find_resolved("main"));
    assert_unsupported("find of HEAD", store.find("HEAD"));

    assert_unsupported("open_packed_buffer", store.open_packed_buffer());
    assert_unsupported("cached_packed_buffer", store.cached_packed_buffer());
    assert_unsupported("force_refresh_packed_buffer", store.force_refresh_packed_buffer());

    assert_unsupported("iter", store.iter());
    assert_unsupported_io("loose_iter", store.loose_iter());
    assert_unsupported_io(
        "loose_iter_prefixed",
        store.loose_iter_prefixed(b"refs/heads/".as_bstr().try_into().expect("valid")),
    );
    assert_unsupported_io("iter_packed", store.iter_packed(None));
    assert_unsupported_io(
        "iter_prefixed_packed",
        store.iter_prefixed_packed(b"refs/".as_bstr().try_into().expect("valid"), None),
    );
    assert_unsupported_io("iter_pseudo", store.iter_pseudo());

    let mut buf = Vec::new();
    assert_unsupported("reflog_exists", store.reflog_exists("HEAD"));
    assert_unsupported("reflog_iter", store.reflog_iter("HEAD", &mut buf).map(|_| ()));
    assert_unsupported(
        "reflog_iter_rev",
        store.reflog_iter_rev("HEAD", &mut buf[..]).map(|_| ()),
    );

    assert_eq!(
        store.is_pristine("refs/heads/main".try_into()?),
        None,
        "the store can't tell what it contains, which is `None` as when HEAD can't be read"
    );

    store.set_unsupported_backend(None);
    assert!(
        store.try_find("main")?.is_some(),
        "the mark can be removed, restoring all operations"
    );
    Ok(())
}

#[test]
fn a_refused_transaction_leaves_no_trace_in_the_repository() -> crate::Result {
    let (_dir, mut store) = store_writable("make_ref_repository.sh")?;
    store.set_unsupported_backend(Some(BACKEND.into()));
    let before = paths_below(store.git_dir())?;

    let res = store
        .transaction()
        .prepare([create_at("refs/heads/new")], Fail::Immediately, Fail::Immediately);
    assert_unsupported("prepare", res.map(|_| ()));

    assert_eq!(
        paths_below(store.git_dir())?,
        before,
        "no lock file, reference or reflog is created for a transaction that was refused"
    );
    Ok(())
}
