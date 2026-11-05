//! Repacking, with `git repack` as the oracle for what remains, and with interruptions at every step to see that nothing is lost.
//! See `make_repack_repo.sh` for the repository and what is special about each of its objects.
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::Path,
    sync::atomic::AtomicBool,
};

use gix::maintenance::repack::{Options, Phase, Scope, Unreachable};
use gix_error::ErrorExt as _;
use gix_hash::Kind;
use gix_testtools::{Creation, TestResult};

fn kinds() -> impl Iterator<Item = Kind> {
    [Kind::Sha1, Kind::Sha256]
        .into_iter()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
}

fn fixture(kind: Kind) -> TestResult<gix_testtools::tempfile::TempDir> {
    Ok(gix_testtools::scripted_fixture_writable_with_args(
        "make_repack_repo.sh",
        [kind.to_string()],
        Creation::Execute,
    )?)
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

/// What a repository has, in a form that can be compared with that of another one.
#[derive(Debug, PartialEq, Eq)]
struct State {
    /// All objects with their kinds, wherever they are.
    objects: BTreeSet<String>,
    /// The loose objects with the time they were modified at.
    loose: BTreeMap<String, u64>,
    /// The number of packs that were made by the repack and of those with a `.keep` file.
    packs: (usize, usize),
}

fn state(repo_dir: &Path) -> TestResult<State> {
    let objects = gix_testtools::git(
        repo_dir,
        "cat-file --batch-all-objects --unordered --batch-check=%(objectname)%x20%(objecttype)",
    )?
    .lines()
    .map(ToOwned::to_owned)
    .collect();
    let objects_dir = repo_dir.join(".git/objects");
    let mut loose = BTreeMap::new();
    for fan_out in std::fs::read_dir(&objects_dir)? {
        let fan_out = fan_out?;
        let name = fan_out.file_name().to_string_lossy().into_owned();
        if name.len() != 2 || !fan_out.path().is_dir() {
            continue;
        }
        for object in std::fs::read_dir(fan_out.path())? {
            let object = object?;
            let mtime = object
                .metadata()?
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            loose.insert(format!("{name}{}", object.file_name().to_string_lossy()), mtime);
        }
    }
    let mut packs = (0, 0);
    for entry in std::fs::read_dir(objects_dir.join("pack"))? {
        match entry?.path().extension().and_then(|ext| ext.to_str()) {
            Some("pack") => packs.0 += 1,
            Some("keep") => packs.1 += 1,
            _ => {}
        }
    }
    Ok(State { objects, loose, packs })
}

/// Compare two states, and say what differs instead of printing both of them.
#[track_caller]
fn assert_same_state(actual: &State, expected: &State, what: &str) {
    if actual == expected {
        return;
    }
    let loose_differences: Vec<_> = actual
        .loose
        .iter()
        .filter(|(id, time)| expected.loose.get(*id) != Some(time))
        .map(|(id, time)| format!("{id} actual {time} expected {:?}", expected.loose.get(id)))
        .chain(
            expected
                .loose
                .keys()
                .filter(|id| !actual.loose.contains_key(*id))
                .map(|id| format!("{id} is missing")),
        )
        .collect();
    panic!(
        "{what}\nonly in actual: {:?}\nonly in expected: {:?}\nloose differences: {loose_differences:?}\npacks and keeps actual {:?} expected {:?}",
        actual.objects.difference(&expected.objects).collect::<Vec<_>>(),
        expected.objects.difference(&actual.objects).collect::<Vec<_>>(),
        actual.packs,
        expected.packs
    );
}

/// Git can read everything, no object is missing, and nothing is left behind that it would complain about.
fn assert_healthy(repo_dir: &Path, what: &str) -> TestResult {
    gix_testtools::git(repo_dir, "fsck --full --strict")?;
    gix_testtools::git(repo_dir, "commit-graph verify")?;
    let count = gix_testtools::git(repo_dir, "count-objects -v")?;
    assert!(
        count.lines().any(|line| line == "garbage: 0"),
        "{what}: no garbage files are left in the object directory: {count}"
    );
    Ok(())
}

#[test]
fn what_remains_is_what_git_keeps() -> TestResult {
    for kind in kinds() {
        for (name, options, git_args) in [
            ("-A -d", Options::default(), "repack -A -d"),
            ("-a -d", options_dropping(), "repack -a -d"),
            ("-d", options_incremental(), "repack -d"),
        ] {
            let (git, native) = (fixture(kind)?, fixture(kind)?);
            let before = state(&native.path().join("repo"))?;
            gix_testtools::git(git.path().join("repo"), git_args)?;

            let repo = gix::open_opts(native.path().join("repo"), crate::restricted())?;
            let outcome = repo.repack(options, &never())?;
            assert!(outcome.failed.is_empty(), "{kind} {name}: {:?}", outcome.failed);

            let (expected, actual) = (state(&git.path().join("repo"))?, state(&native.path().join("repo"))?);
            assert_ne!(
                before, actual,
                "{kind} {name}: something changed, or the fixture is wrong"
            );
            assert_same_state(
                &actual,
                &expected,
                &format!(
                    "{kind} {name}: the same objects, loose objects with their times, and packs as with `git {git_args}`"
                ),
            );
            assert_healthy(&native.path().join("repo"), &format!("{kind} {name}"))?;
        }
    }
    Ok(())
}

fn options_dropping() -> Options {
    let mut options = Options::default();
    options.unreachable = Unreachable::Drop;
    options
}

fn options_incremental() -> Options {
    let mut options = Options::default();
    options.scope = Scope::Incremental;
    options
}

#[test]
fn a_pack_that_has_the_same_objects_is_not_removed_with_the_ones_it_replaces() -> TestResult {
    for kind in kinds() {
        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let repo = gix::open_opts(&repo_dir, crate::restricted())?;
        let mut last = None;
        for round in 0..3 {
            repo.repack(Options::default(), &never())?;
            assert_healthy(&repo_dir, &format!("{kind} round {round}"))?;
            let now = state(&repo_dir)?;
            assert_eq!(
                now.packs,
                (2, 1),
                "{kind} round {round}: the new pack and the one with a `.keep` file"
            );
            if let Some(last) = last.replace(now.objects.clone()) {
                assert_eq!(last, now.objects, "{kind} round {round}: no object is lost or made up");
            }
        }
    }
    Ok(())
}

#[test]
fn a_pack_with_a_keep_file_and_its_objects_are_left_alone() -> TestResult {
    for kind in kinds() {
        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let pack_dir = repo_dir.join(".git/objects/pack");
        let kept: Vec<_> = std::fs::read_dir(&pack_dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "keep"))
            .collect();
        assert_eq!(kept.len(), 1, "{kind}: the fixture has one");
        let kept_pack = kept[0].with_extension("pack");
        let before = std::fs::read(&kept_pack)?;

        let repo = gix::open_opts(&repo_dir, crate::restricted())?;
        let outcome = repo.repack(Options::default(), &never())?;
        assert_eq!(std::fs::read(&kept_pack)?, before, "{kind}: the kept pack is as it was");
        let new_pack = outcome.pack.expect("something was packed");
        let in_new = gix_testtools::git(
            &repo_dir,
            &format!("verify-pack -v {}", new_pack.with_extension("idx").display()),
        )?;
        let kept_commit = gix_testtools::git(&repo_dir, "rev-parse :/kept")?.trim().to_owned();
        assert!(
            !in_new.contains(&kept_commit),
            "{kind}: the objects of the kept pack aren't packed again"
        );
    }
    Ok(())
}

#[test]
fn stopping_after_any_step_loses_nothing_and_the_next_run_finishes_the_job() -> TestResult {
    let cruft = |year_2010: bool| {
        let mut options = Options::default();
        let expire = if year_2010 { 1_262_304_000 } else { 4_102_444_800 };
        options.unreachable = Unreachable::Cruft {
            expire: Some(gix_date::Time::new(expire, 0)),
        };
        options
    };
    for kind in kinds() {
        // what each way of repacking doesn't do: with the expiry in 2010, nothing is written loose, and with the one in 2100
        // nothing is kept, so that there is no cruft pack and unreachable objects of the packs are dropped
        for (how, options, absent) in [
            ("loosening", Options::default(), &["cruft written"][..]),
            ("with a cruft pack", cruft(true), &["loosened"][..]),
            (
                "dropping expired objects",
                cruft(false),
                &["loosened", "cruft written"][..],
            ),
        ] {
            let finished = fixture(kind)?;
            gix::open_opts(finished.path().join("repo"), crate::restricted())?.repack(options, &never())?;
            let expected = (
                state(&finished.path().join("repo"))?,
                cruft_times(&finished.path().join("repo"), kind)?,
            );

            for stop_at in [
                "written",
                "cruft written",
                "loosened",
                "derived files",
                "pack removed",
                "loose removed",
            ] {
                let dir = fixture(kind)?;
                let repo_dir = dir.path().join("repo");
                let before = state(&repo_dir)?;
                let repo = gix::open_opts(&repo_dir, crate::restricted())?;
                let mut stopped = false;
                let result = repo.repack_observed(options, &never(), &mut |phase| {
                    let name = match phase {
                        Phase::PackWritten(_) => "written",
                        Phase::CruftPackWritten(_) => "cruft written",
                        Phase::Loosened(_) => "loosened",
                        Phase::DerivedFilesRemoved => "derived files",
                        Phase::PackRemoved(_) => "pack removed",
                        Phase::LooseRemoved(_) => "loose removed",
                        _ => return Ok(()),
                    };
                    if name == stop_at && !stopped {
                        stopped = true;
                        return Err(gix_error::message("stopped for the test").raise());
                    }
                    Ok(())
                });
                if !stopped {
                    // a step that this way of repacking doesn't have
                    assert!(result.is_ok(), "{kind} {how} {stop_at}: {result:?}");
                    assert!(
                        absent.contains(&stop_at),
                        "{kind} {how}: the step '{stop_at}' was reached"
                    );
                    continue;
                }
                assert!(
                    result.is_err(),
                    "{kind} {how} {stop_at}: the observer's error is the result"
                );

                let after = state(&repo_dir)?;
                // what is dropped is only gone once its pack is, and nothing else is ever gone
                let lost: Vec<_> = before
                    .objects
                    .difference(&after.objects)
                    .filter(|object| expected.0.objects.contains(*object))
                    .collect();
                assert!(
                    lost.is_empty(),
                    "{kind} {how} {stop_at}: no object that stays is lost: {lost:?}"
                );
                assert!(
                    gix_testtools::git(&repo_dir, "fsck --full --strict").is_ok(),
                    "{kind} {how} {stop_at}: Git finds the repository to be complete"
                );

                // the process that was stopped here would be gone, which its marker tells by the time
                for entry in std::fs::read_dir(repo_dir.join(".git/objects/pack"))? {
                    let path = entry?.path();
                    if path.extension().is_some_and(|ext| ext == "keep")
                        && std::fs::read_to_string(&path)?.starts_with("gix repack in progress")
                    {
                        std::fs::write(&path, "gix repack in progress\npid 1\nhost nowhere\ntime 1\n")?;
                    }
                }
                gix::open_opts(&repo_dir, crate::restricted())?.repack(options, &never())?;
                let what =
                    format!("{kind} {how} {stop_at}: the run that follows ends with what an uninterrupted one has");
                assert_same_state(&state(&repo_dir)?, &expected.0, &what);
                assert_eq!(cruft_times(&repo_dir, kind)?, expected.1, "{what}, including the times");
                assert_healthy(&repo_dir, &format!("{kind} {how} {stop_at}"))?;
            }
        }
    }
    Ok(())
}

/// The time of each object of the cruft packs, which `.mtimes` files have in the order of the index of their pack. Their format
/// is checked as well: the header, and the checksum of the pack, which Git uses to see that the file belongs to the pack.
fn cruft_times(repo_dir: &Path, kind: Kind) -> TestResult<BTreeMap<String, u32>> {
    let mut times = BTreeMap::new();
    for entry in std::fs::read_dir(repo_dir.join(".git/objects/pack"))? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "mtimes") {
            continue;
        }
        let index = gix::odb::pack::index::File::at(path.with_extension("idx"), kind)?;
        let data = std::fs::read(&path)?;
        let id_len = kind.len_in_bytes();
        let count = index.num_objects() as usize;
        assert_eq!(
            data.len(),
            12 + count * 4 + 2 * id_len,
            "the size is that of the header, times, and two hashes"
        );
        assert_eq!(&data[..4], b"MTME", "{}: the signature", path.display());
        assert_eq!(&data[4..8], &1u32.to_be_bytes(), "{}: the version", path.display());
        let hash_version: u32 = if kind == Kind::Sha1 { 1 } else { 2 };
        assert_eq!(
            &data[8..12],
            &hash_version.to_be_bytes(),
            "{}: the hash",
            path.display()
        );
        let checksum_at = 12 + count * 4;
        assert_eq!(
            &data[checksum_at..checksum_at + id_len],
            index.pack_checksum().as_slice(),
            "{}: the file says which pack it belongs to",
            path.display()
        );
        for (idx, entry) in index.iter().enumerate() {
            let at = 12 + idx * 4;
            let time = u32::from_be_bytes(data[at..at + 4].try_into().expect("4 bytes"));
            times.insert(entry.oid.to_string(), time);
        }
    }
    Ok(times)
}

#[test]
fn a_cruft_pack_has_what_git_puts_in_it_with_the_times_of_the_objects() -> TestResult {
    for kind in kinds() {
        // the year 2010, which what the fixture has made is newer than, so that all of it stays, and the year 2100, which
        // nothing is newer than
        for (name, expire, git_args) in [
            ("no expiry", None, "repack -a -d --cruft"),
            (
                "2010",
                Some(gix_date::Time::new(1_262_304_000, 0)),
                "repack -a -d --cruft --cruft-expiration=2010-01-01T00:00:00Z",
            ),
            (
                "2100",
                Some(gix_date::Time::new(4_102_444_800, 0)),
                "repack -a -d --cruft --cruft-expiration=2100-01-01T00:00:00Z",
            ),
        ] {
            let (git, native) = (fixture(kind)?, fixture(kind)?);
            let (git_dir, native_dir) = (git.path().join("repo"), native.path().join("repo"));
            let before = state(&native_dir)?;
            gix_testtools::git(&git_dir, git_args)?;

            let mut options = Options::default();
            options.unreachable = Unreachable::Cruft { expire };
            let outcome = gix::open_opts(&native_dir, crate::restricted())?.repack(options, &never())?;
            assert!(outcome.failed.is_empty(), "{kind} {name}: {:?}", outcome.failed);

            let (expected, actual) = (state(&git_dir)?, state(&native_dir)?);
            assert_ne!(
                before, actual,
                "{kind} {name}: something changed, or the fixture is wrong"
            );
            assert_same_state(
                &actual,
                &expected,
                &format!(
                    "{kind} {name}: the same objects, loose objects with their times, and packs as with `git {git_args}`"
                ),
            );
            assert_eq!(
                cruft_times(&native_dir, kind)?,
                cruft_times(&git_dir, kind)?,
                "{kind} {name}: each object of a cruft pack has the time that Git gives it"
            );
            assert_healthy(&native_dir, &format!("{kind} {name}"))?;
            assert_eq!(
                native_dir.join(".git/objects/info/commit-graph").exists(),
                name != "2100",
                "{kind} {name}: the commit-graph that has the commit of `scratch` is gone if that commit was dropped, and is kept otherwise"
            );

            // Git reads what was written, and has nothing to change when it repeats what was done
            gix_testtools::git(&native_dir, git_args)?;
            assert_eq!(
                (state(&native_dir)?, cruft_times(&native_dir, kind)?),
                (expected, cruft_times(&git_dir, kind)?),
                "{kind} {name}: Git accepts the cruft pack and its times, and repeating the repack with it changes nothing"
            );
        }
    }
    Ok(())
}

#[test]
fn a_loose_object_that_was_modified_while_the_cruft_pack_was_written_stays_as_the_pack_has_an_older_time() -> TestResult
{
    for kind in kinds() {
        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let dangling = blob_id(kind, "dangling\n");
        let dangling_path = repo_dir
            .join(".git/objects")
            .join(&dangling.to_hex().to_string()[..2])
            .join(&dangling.to_hex().to_string()[2..]);
        assert!(dangling_path.is_file(), "{kind}: the fixture has the blob loose");

        let mut options = Options::default();
        options.unreachable = Unreachable::Cruft { expire: None };
        let mut touched = false;
        gix::open_opts(&repo_dir, crate::restricted())?.repack_observed(options, &never(), &mut |phase| {
            if matches!(phase, Phase::CruftPackWritten(_)) {
                // somebody freshens the object, as a writer of the same content does
                std::fs::File::open(&dangling_path)
                    .and_then(|file| file.set_modified(std::time::SystemTime::now()))
                    .map_err(|err| err.and_raise(gix_error::message("Could not touch the loose object")))?;
                touched = true;
            }
            Ok(())
        })?;
        assert!(touched, "{kind}: the cruft pack was written, with the blob in it");
        assert!(
            dangling_path.is_file(),
            "{kind}: the loose copy is newer than the time in the cruft pack, so it isn't removed"
        );
        assert!(
            cruft_times(&repo_dir, kind)?.contains_key(&dangling.to_string()),
            "{kind}: the cruft pack has the object, with its older time"
        );
    }
    Ok(())
}

fn blob_id(kind: Kind, content: &str) -> gix_hash::ObjectId {
    gix_object::compute_hash(kind, gix_object::Kind::Blob, content.as_bytes()).expect("valid object hash")
}

/// The blob that `scratch` had and nothing wants any more, which is in the pack that is replaced and is the base of a delta.
fn scratch_blob(kind: Kind) -> gix_hash::ObjectId {
    let mut content = String::new();
    for n in 1..=510 {
        writeln!(content, "{n}").expect("writing to a string works");
    }
    blob_id(kind, &content)
}

fn has_object(repo_dir: &Path, id: &gix_hash::oid) -> bool {
    gix_testtools::git(repo_dir, &format!("cat-file -e {id}")).is_ok()
}

#[test]
fn a_handle_that_has_read_a_pack_which_is_removed_doesnt_take_objects_to_be_in_a_pack_that_aren_t() -> TestResult {
    for kind in kinds() {
        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let repo = gix::open_opts(&repo_dir, crate::restricted())?;
        let unreachable = scratch_blob(kind);
        assert!(
            repo.find_object(unreachable).is_ok(),
            "{kind}: the object is read from the pack that is about to be replaced, so the handle has it mapped"
        );

        repo.repack(Options::default(), &never())?;
        assert!(
            has_object(&repo_dir, &unreachable),
            "{kind}: the unreachable object was written loose and stays, and isn't taken to be in a pack that is gone"
        );
        assert!(
            repo.find_object(unreachable).is_ok(),
            "{kind}: the handle that had the old pack mapped finds the loose object"
        );
        assert!(
            repo_dir
                .join(".git/objects")
                .join(&unreachable.to_string()[..2])
                .join(&unreachable.to_string()[2..])
                .is_file(),
            "{kind}: and it's a loose object"
        );
    }
    Ok(())
}

#[test]
fn a_pack_that_a_writer_freshened_while_the_repack_ran_stays_with_what_it_keeps_alive() -> TestResult {
    for kind in kinds() {
        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let repo = gix::open_opts(&repo_dir, crate::restricted())?;
        let old_pack = std::fs::read_dir(repo_dir.join(".git/objects/pack"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
            .find(|path| !path.with_extension("keep").exists())
            .expect("the pack that is replaced");

        let outcome = repo.repack_observed(Options::default(), &never(), &mut |phase| {
            if matches!(phase, Phase::Loosened(_)) {
                // what a writer does to keep the objects of a pack alive that it is going to refer to
                std::fs::File::open(&old_pack)
                    .and_then(|file| file.set_modified(std::time::SystemTime::now()))
                    .expect("a writer can freshen the pack");
            }
            Ok(())
        })?;
        assert!(
            old_pack.is_file() && outcome.changed_packs == [old_pack.clone()],
            "{kind}: the pack is not removed as it changed after it was listed: {outcome:?}"
        );

        let unreachable = scratch_blob(kind);
        let expire = gix_date::Time::new(
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
            )? - 3600,
            0,
        );
        let mut prune = gix::maintenance::prune::Options::default();
        prune.expire = Some(expire);
        repo.prune(prune)?;
        assert!(
            has_object(&repo_dir, &unreachable),
            "{kind}: the object that the freshened pack has isn't pruned, although its loose copy is as old as the pack was"
        );
        assert_healthy(&repo_dir, &format!("{kind}"))?;
    }
    Ok(())
}

#[test]
fn a_pack_that_is_received_and_not_referenced_yet_survives_unless_unreachable_objects_are_dropped() -> TestResult {
    for kind in kinds() {
        for (unreachable, survives) in [(Unreachable::Loosen, true), (Unreachable::Drop, false)] {
            let dir = fixture(kind)?;
            let repo_dir = dir.path().join("repo");
            let id = gix::open_opts(&repo_dir, crate::restricted())?
                .write_blob("received\n")?
                .detach();
            let pack_dir = repo_dir.join(".git/objects/pack");
            let mut command = gix_testtools::git_command(&repo_dir);
            command
                .arg("pack-objects")
                .arg(pack_dir.join("pack"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null());
            let mut child = command.spawn()?;
            {
                use std::io::Write;
                writeln!(child.stdin.take().expect("piped"), "{id}")?;
            }
            assert!(child.wait()?.success());
            let hex = id.to_string();
            std::fs::remove_file(repo_dir.join(".git/objects").join(&hex[..2]).join(&hex[2..]))?;
            assert!(
                has_object(&repo_dir, &id),
                "{kind}: the object is in a pack that has no `.keep` file"
            );

            let mut options = Options::default();
            options.unreachable = unreachable;
            gix::open_opts(&repo_dir, crate::restricted())?.repack(options, &never())?;
            assert_eq!(
                has_object(&repo_dir, &id),
                survives,
                "{kind} {unreachable:?}: an object nothing refers to yet survives by being loosened, and only a drop loses it"
            );
        }
    }
    Ok(())
}

#[test]
fn what_an_interrupted_run_left_to_mark_its_pack_is_removed_by_the_next_one_and_other_keep_files_stay() -> TestResult {
    for kind in kinds() {
        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let pack_dir = repo_dir.join(".git/objects/pack");
        let marker = pack_dir.join("pack-left-behind.keep");
        let received = pack_dir.join("pack-received.keep");
        // the process that wrote it is long gone, which is told by its time on all platforms
        std::fs::write(&marker, "gix repack in progress\npid 1\nhost nowhere\ntime 1\n")?;
        std::fs::write(&received, "")?;

        gix::open_opts(&repo_dir, crate::restricted())?.repack(Options::default(), &never())?;
        assert!(!marker.exists(), "{kind}: the marker of an interrupted run is gone");
        assert!(
            received.exists(),
            "{kind}: a `.keep` file of somebody else is not touched"
        );
    }
    Ok(())
}

#[test]
fn a_marker_of_a_run_that_may_still_be_going_stops_a_second_run_before_it_changes_anything() -> TestResult {
    for kind in kinds() {
        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let marker = repo_dir.join(".git/objects/pack/pack-in-progress.keep");
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        std::fs::write(
            &marker,
            format!(
                "gix repack in progress\npid {}\nhost {}\ntime {now}\n",
                std::process::id(),
                host.trim()
            ),
        )?;
        let before = state(&repo_dir)?;

        let err = gix::open_opts(&repo_dir, crate::restricted())?
            .repack(Options::default(), &never())
            .expect_err("somebody else may be repacking");
        assert!(
            format!("{err:?}").contains("Another repack"),
            "{kind}: the reason is given: {err:?}"
        );
        assert!(marker.exists(), "{kind}: the marker of the other run is left alone");
        assert_eq!(state(&repo_dir)?, before, "{kind}: nothing was changed");
    }
    Ok(())
}

#[test]
fn files_that_belong_to_a_pack_of_the_same_name_but_not_to_this_one_are_removed_before_it_is_used() -> TestResult {
    for kind in kinds() {
        let probe = fixture(kind)?;
        let outcome =
            gix::open_opts(probe.path().join("repo"), crate::restricted())?.repack(Options::default(), &never())?;
        let new_pack = outcome.pack.expect("something was packed");
        let name = new_pack.file_name().expect("a file name").to_owned();

        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let strays: Vec<_> = ["mtimes", "rev", "bitmap"]
            .iter()
            .map(|extension| repo_dir.join(".git/objects/pack").join(&name).with_extension(extension))
            .collect();
        for stray in &strays {
            std::fs::write(stray, "left by a crash")?;
        }
        gix::open_opts(&repo_dir, crate::restricted())?.repack(Options::default(), &never())?;
        for stray in &strays {
            assert!(
                !stray.exists(),
                "{kind}: {} isn't kept next to a pack it doesn't belong to",
                stray.display()
            );
        }
        assert_healthy(&repo_dir, &format!("{kind}"))?;
    }
    Ok(())
}

#[test]
fn the_keep_file_of_somebody_who_is_installing_the_same_pack_is_not_replaced_or_removed() -> TestResult {
    for kind in kinds() {
        let probe = fixture(kind)?;
        let outcome =
            gix::open_opts(probe.path().join("repo"), crate::restricted())?.repack(Options::default(), &never())?;
        let new_pack = outcome.pack.expect("something was packed");
        let name = new_pack.file_name().expect("a file name").to_owned();

        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        // A receiver that has written its `.keep` file, and not its pack yet, to protect it until its references are updated.
        let keep = repo_dir.join(".git/objects/pack").join(&name).with_extension("keep");
        std::fs::write(&keep, "received\n")?;
        let outcome = gix::open_opts(&repo_dir, crate::restricted())?.repack(Options::default(), &never())?;
        assert_eq!(
            outcome.pack.and_then(|pack| pack.file_name().map(ToOwned::to_owned)),
            Some(name),
            "{kind}: the same pack is made"
        );
        assert_eq!(
            std::fs::read_to_string(&keep)?,
            "received\n",
            "{kind}: the `.keep` file stays as it was, so the pack stays protected for whoever wrote it"
        );
        assert_healthy(&repo_dir, &format!("{kind}"))?;
    }
    Ok(())
}

#[test]
fn a_multi_pack_index_with_all_that_belongs_to_it_is_removed_with_the_packs_it_names() -> TestResult {
    for kind in kinds() {
        for (what, write_args) in [
            ("with a bitmap", "multi-pack-index write --bitmap"),
            ("an incremental chain", "multi-pack-index write --incremental"),
        ] {
            let dir = fixture(kind)?;
            let repo_dir = dir.path().join("repo");
            if gix_testtools::git(&repo_dir, "multi-pack-index write").is_err() {
                continue;
            }
            std::fs::remove_file(repo_dir.join(".git/objects/pack/multi-pack-index")).ok();
            if gix_testtools::git(&repo_dir, write_args).is_err() {
                continue;
            }
            gix::open_opts(&repo_dir, crate::restricted())?.repack(Options::default(), &never())?;
            let leftovers: Vec<_> = std::fs::read_dir(repo_dir.join(".git/objects/pack"))?
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("multi-pack-index"))
                .collect();
            assert!(
                leftovers.is_empty(),
                "{kind} {what}: nothing that names removed packs remains: {leftovers:?}"
            );
            assert_healthy(&repo_dir, &format!("{kind} {what}"))?;
        }
    }
    Ok(())
}

#[test]
fn readers_and_writers_never_fail_while_packs_are_replaced_over_and_over() -> TestResult {
    for kind in kinds() {
        let dir = fixture(kind)?;
        let repo_dir = dir.path().join("repo");
        let ids: Vec<gix_hash::ObjectId> = gix_testtools::git(
            &repo_dir,
            "cat-file --batch-all-objects --unordered --batch-check=%(objectname)",
        )?
        .lines()
        .map(str::parse)
        .collect::<Result<_, _>>()?;
        let reachable: BTreeSet<String> = gix_testtools::git(&repo_dir, "rev-list --objects --all --reflog")?
            .lines()
            .map(|line| line.split(' ').next().expect("an id").to_owned())
            .collect();
        let shared = gix::open_opts(&repo_dir, crate::restricted())?.into_sync();
        let done = AtomicBool::new(false);
        let failures = std::sync::Mutex::new(Vec::<String>::new());

        std::thread::scope(|scope| -> TestResult {
            for _ in 0..2 {
                scope.spawn(|| {
                    // one handle for the whole time, which has to notice that the packs it has are gone
                    let repo = shared.to_thread_local();
                    while !done.load(std::sync::atomic::Ordering::Relaxed) {
                        for id in &ids {
                            if let Err(err) = repo.find_object(*id) {
                                failures.lock().expect("no panic").push(format!(
                                    "{id}: {err} (reachable: {})",
                                    reachable.contains(&id.to_string())
                                ));
                            }
                        }
                    }
                });
            }
            scope.spawn(|| {
                let repo = shared.to_thread_local();
                let mut written = Vec::new();
                for n in 0..200 {
                    match repo.write_blob(format!("written while repacking {n}\n")) {
                        Ok(id) => written.push(id.detach()),
                        Err(err) => failures.lock().expect("no panic").push(format!("write {n}: {err:?}")),
                    }
                }
                for id in written {
                    if repo.find_object(id).is_err() {
                        failures
                            .lock()
                            .expect("no panic")
                            .push(format!("{id} was written and then lost"));
                    }
                }
            });
            let repo = shared.to_thread_local();
            for _ in 0..15 {
                repo.repack(Options::default(), &never())?;
            }
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })?;
        let failures = failures.into_inner()?;
        assert!(
            failures.is_empty(),
            "{kind}: readers and writers saw no error: {failures:?}"
        );
        assert_healthy(&repo_dir, &format!("{kind}"))?;
    }
    Ok(())
}
