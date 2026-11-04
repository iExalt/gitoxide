use gix_hash::Kind;
use gix_reftable::Stack;

use crate::{
    Result,
    util::{self, HASHES, git, git_ref_lines, git_written_tables, kind, ref_lines, reftable_dir},
};

#[test]
fn a_stack_written_by_git_lists_what_git_lists() -> Result {
    for object_format in HASHES {
        let Some(fixture) = git_written_tables(object_format)? else {
            return Ok(());
        };
        for repo in ["stacked", "compacted"] {
            let repo = fixture.join(repo);
            let stack = Stack::open(reftable_dir(&repo), kind(object_format))?;
            assert_eq!(
                stack.snapshot().tables().len(),
                util::table_names(&repo)?.len(),
                "{object_format} {repo:?}: every listed table is read"
            );
            assert_eq!(
                ref_lines(stack.snapshot().merged())?,
                git_ref_lines(&repo)?,
                "{object_format} {repo:?}: same references as Git, with tombstones applied"
            );
        }
    }
    Ok(())
}

#[test]
fn a_missing_list_or_directory_is_an_empty_stack_whose_first_update_index_is_one() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    for path in [dir.path().to_owned(), dir.path().join("does-not-exist")] {
        let stack = Stack::open(path, Kind::Sha1)?;
        assert!(stack.snapshot().tables().is_empty());
        assert_eq!(stack.snapshot().next_update_index(), 1);
        assert_eq!(stack.snapshot().merged().ref_iter()?.next_record()?, None);
    }
    Ok(())
}

#[test]
fn the_next_update_index_follows_the_newest_table() -> Result {
    let Some(fixture) = git_written_tables("sha1")? else {
        return Ok(());
    };
    let repo = fixture.join("stacked");
    let stack = Stack::open(reftable_dir(&repo), Kind::Sha1)?;
    let newest = stack.snapshot().tables().last().expect("a stack of several tables");
    assert_eq!(stack.snapshot().next_update_index(), newest.max_update_index() + 1);
    Ok(())
}

#[test]
fn tables_of_another_hash_fail_the_open() -> Result {
    let Some(fixture) = git_written_tables("sha256")? else {
        return Ok(());
    };
    let err =
        Stack::open(reftable_dir(&fixture.join("stacked")), Kind::Sha1).expect_err("sha256 tables in a sha1 stack");
    assert!(err.is_corrupted(), "{err:?}");
    Ok(())
}

#[test]
fn a_listed_table_that_does_not_exist_fails_instead_of_reading_as_empty() -> Result {
    let Some(repo) = util::writable_git_written_tables("sha1")? else {
        return Ok(());
    };
    let repo = repo.path().join("stacked");
    let dir = reftable_dir(&repo);
    let names = util::table_names(&repo)?;
    std::fs::remove_file(dir.join(&names[0]))?;
    let err = Stack::open(&dir, Kind::Sha1).expect_err("Git would silently show fewer references");
    assert!(err.is_not_found(), "{err:?}");
    Ok(())
}

#[test]
fn a_damaged_table_or_list_fails_with_corruption() -> Result {
    let Some(repo) = util::writable_git_written_tables("sha1")? else {
        return Ok(());
    };
    let repo = repo.path().join("stacked");
    let dir = reftable_dir(&repo);
    let names = util::table_names(&repo)?;

    let list = std::fs::read(dir.join("tables.list"))?;
    std::fs::write(dir.join("tables.list"), &list[..list.len() - 1])?;
    let err = Stack::open(&dir, Kind::Sha1).expect_err("the last line isn't terminated");
    assert!(err.is_corrupted(), "{err:?}");
    std::fs::write(dir.join("tables.list"), &list)?;

    let table = dir.join(&names[0]);
    let mut bytes = std::fs::read(&table)?;
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&table, bytes)?;
    let err = Stack::open(&dir, Kind::Sha1).expect_err("the checksum of the footer doesn't match");
    assert!(err.is_corrupted(), "{err:?}");
    Ok(())
}

#[test]
fn reloading_picks_up_what_git_published_and_snapshots_stay_as_they_were() -> Result {
    for object_format in HASHES {
        let Some(repo) = util::writable_git_written_tables(object_format)? else {
            return Ok(());
        };
        let repo = repo.path().join("stacked");
        let mut stack = Stack::open(reftable_dir(&repo), kind(object_format))?;
        assert!(!stack.reload()?, "nothing was published yet");

        let before = stack.snapshot().clone();
        let before_lines = ref_lines(before.merged())?;
        git(&repo, &["update-ref", "refs/heads/published-later", "refs/heads/main"])?;
        git(&repo, &["update-ref", "-d", "refs/heads/team01/sub1/feature-1"])?;

        assert!(stack.reload()?, "{object_format}: Git added tables or compacted them");
        assert_eq!(
            ref_lines(stack.snapshot().merged())?,
            git_ref_lines(&repo)?,
            "{object_format}: the reloaded stack is what Git shows now"
        );
        assert_eq!(
            ref_lines(before.merged())?,
            before_lines,
            "{object_format}: the earlier snapshot isn't affected by the reload"
        );
        assert!(!stack.reload()?, "a second reload finds nothing new");
    }
    Ok(())
}
