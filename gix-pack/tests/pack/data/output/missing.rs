//! Which objects a receiver needs, with Git as oracle for what has to be there and whether the pack is valid.
use std::{
    collections::HashSet,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use gix_hash::{Kind, ObjectId};
use gix_pack::data::output::missing::{self, Options};
use gix_testtools::TestResult;

struct Repo {
    kind: Kind,
    dir: PathBuf,
}

impl Repo {
    fn git(&self, args: &str) -> TestResult<String> {
        Ok(gix_testtools::git(self.dir.join("repo"), args)?)
    }

    fn id(&self, rev: &str) -> TestResult<ObjectId> {
        Ok(ObjectId::from_hex(
            self.git(&format!("rev-parse {rev}"))?.trim().as_bytes(),
        )?)
    }

    fn db(&self) -> TestResult<gix_odb::HandleArc> {
        let store = gix_odb::Store::at_opts(
            self.dir.join("repo/.git/objects"),
            self.kind,
            &mut None.into_iter(),
            gix_odb::store::init::Options::default(),
        )?;
        let mut cache = Arc::new(store).to_cache_arc();
        cache.prevent_pack_unload();
        Ok(cache)
    }
}

fn repos() -> TestResult<Vec<Repo>> {
    repos_from("make_push_closure_repo.sh")
}

fn repos_from(script: &str) -> TestResult<Vec<Repo>> {
    Kind::all()
        .iter()
        .copied()
        .map(|kind| {
            let dir = gix_testtools::scripted_fixture_read_only_with_args(script, [kind.to_string()])?;
            Ok(Repo { kind, dir })
        })
        .collect()
}

fn no_interrupt() -> AtomicBool {
    AtomicBool::new(false)
}

/// Count and write the pack for `tips` and `haves` of `repo`, the way a caller would.
fn write_pack(
    repo: &Repo,
    tips: &[ObjectId],
    haves: &[ObjectId],
    out: &mut dyn Write,
    options: Options,
) -> gix_error::Result<missing::Outcome> {
    let db = repo.db().expect("the fixture's object database can be opened");
    let counts = missing::counts(
        db.clone(),
        tips,
        haves,
        &gix_utils::progress::Discard,
        &no_interrupt(),
        options,
    )?;
    missing::write_pack(db, counts, out, repo.kind, options)
}

fn lines_to_ids(output: &str) -> TestResult<HashSet<ObjectId>> {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| Ok(ObjectId::from_hex(line.split(' ').next().expect("an id").as_bytes())?))
        .collect()
}

fn ids(repo: &Repo, revs: &[&str]) -> TestResult<Vec<ObjectId>> {
    revs.iter().map(|rev| repo.id(rev)).collect()
}

/// A scenario to send `tips` to a receiver who has `haves`, given as revisions of the repository.
struct Case {
    name: &'static str,
    tips: &'static [&'static str],
    haves: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        name: "new history on top of what the receiver has",
        tips: &["main"],
        haves: &["b5"],
    },
    Case {
        name: "everything, with history copied from packs",
        tips: &["main"],
        haves: &[],
    },
    Case {
        name: "tags to commits, trees and blobs, and tags of tags",
        tips: &["v1-outer", "tree-tag", "blob-tag", "light"],
        haves: &["b5"],
    },
    Case {
        name: "the receiver has an annotated tag, which has to be peeled",
        tips: &["main"],
        haves: &["v-have"],
    },
    Case {
        name: "the receiver has everything already",
        tips: &["main"],
        haves: &["main"],
    },
    Case {
        name: "a new tag on a commit the receiver has",
        tips: &["v1"],
        haves: &["main~1"],
    },
    Case {
        name: "a merge of which one parent is known",
        tips: &["main~1"],
        haves: &["side"],
    },
    Case {
        name: "a submodule entry has no objects to send",
        tips: &["g"],
        haves: &["main"],
    },
];

/// `rev-list` is the oracle for the objects that have to be sent. The pack may contain more, but only
/// objects of the parents of new commits, which the receiver has unless they are new themselves: tree entries are compared by path,
/// so an object that is at a different path than in the parent is sent again.
fn assert_closure(repo: &Repo, case: &Case, counts: &[gix_pack::data::output::Count]) -> TestResult {
    let kind = repo.kind;
    let tips = case.tips.join(" ");
    let haves = case
        .haves
        .iter()
        .map(|have| format!(" ^{have}"))
        .collect::<Vec<_>>()
        .join("");
    let required = lines_to_ids(&repo.git(&format!("rev-list --objects {tips}{haves}"))?)?;
    let produced: HashSet<_> = counts.iter().map(|count| count.id).collect();
    assert_eq!(
        produced.len(),
        counts.len(),
        "{kind} {}: no object is sent twice",
        case.name
    );
    let missing: Vec<_> = required.difference(&produced).collect();
    assert!(
        missing.is_empty(),
        "{kind} {}: the receiver can't have these, they are missing from the pack: {missing:?}",
        case.name
    );

    let mut allowed = required.clone();
    for line in repo.git(&format!("rev-list --parents {tips}{haves}"))?.lines() {
        for parent in line.split(' ').skip(1) {
            allowed.insert(ObjectId::from_hex(parent.as_bytes())?);
            let tree_objects = repo.git(&format!("ls-tree -r -t {parent}"))?;
            allowed.insert(repo.id(&format!("{parent}^{{tree}}"))?);
            for object in tree_objects.lines() {
                let (_mode_and_type, rest) = object.split_once(' ').expect("mode and type");
                let (_type, rest) = rest.split_once(' ').expect("type and id");
                let (id, _path) = rest.split_once('\t').expect("id and path");
                allowed.insert(ObjectId::from_hex(id.as_bytes())?);
            }
        }
    }
    let extra: Vec<_> = produced.difference(&allowed).collect();
    assert!(
        extra.is_empty(),
        "{kind} {}: these objects aren't needed, nor are they in the parents of new commits: {extra:?}",
        case.name
    );
    Ok(())
}

#[test]
fn the_objects_to_send_are_what_rev_list_says_plus_boundaries() -> TestResult {
    for repo in repos()? {
        for case in CASES {
            let counts = missing::counts(
                repo.db()?,
                &ids(&repo, case.tips)?,
                &ids(&repo, case.haves)?,
                &gix_utils::progress::Discard,
                &no_interrupt(),
                Options::default(),
            )?;
            assert_closure(&repo, case, &counts)?;
        }
    }
    Ok(())
}

#[test]
fn haves_that_the_database_lacks_or_that_are_no_commits_are_ignored() -> TestResult {
    for repo in repos()? {
        let unknown = ObjectId::from_hex("1".repeat(repo.kind.len_in_hex()).as_bytes())?;
        let tree = repo.id("main^{tree}")?;
        let case = &CASES[0];
        let mut haves = ids(&repo, case.haves)?;
        haves.extend([unknown, tree, repo.id("main:dir/new")?]);
        let counts = missing::counts(
            repo.db()?,
            &ids(&repo, case.tips)?,
            &haves,
            &gix_utils::progress::Discard,
            &no_interrupt(),
            Options::default(),
        )?;
        assert_closure(&repo, case, &counts)?;
    }
    Ok(())
}

#[test]
fn a_new_tag_for_a_known_commit_sends_only_the_tag() -> TestResult {
    for repo in repos()? {
        let counts = missing::counts(
            repo.db()?,
            &ids(&repo, &["v1"])?,
            &ids(&repo, &["main~1"])?,
            &gix_utils::progress::Discard,
            &no_interrupt(),
            Options::default(),
        )?;
        assert_eq!(
            counts.iter().map(|c| c.id).collect::<Vec<_>>(),
            [repo.id("v1")?],
            "{}: the commit the tag points to is not walked, as the receiver has it",
            repo.kind
        );
    }
    Ok(())
}

#[test]
fn missing_tips_are_not_found_errors() -> TestResult {
    for repo in repos()? {
        let unknown = ObjectId::from_hex("2".repeat(repo.kind.len_in_hex()).as_bytes())?;
        let err = missing::counts(
            repo.db()?,
            &[unknown],
            &[],
            &gix_utils::progress::Discard,
            &no_interrupt(),
            Options::default(),
        )
        .expect_err("a tip that isn't there can't be sent");
        assert!(err.is_not_found(), "{}: {err:#}", repo.kind);
    }
    Ok(())
}

#[test]
fn a_missing_blob_fails_before_a_single_byte_is_written() -> TestResult {
    for kind in Kind::all().iter().copied() {
        let dir = gix_testtools::scripted_fixture_writable_with_args(
            "make_push_closure_repo.sh",
            [kind.to_string()],
            gix_testtools::Creation::CopyFromReadOnly,
        )?;
        let repo = Repo {
            kind,
            dir: dir.path().to_owned(),
        };
        let blob = repo.id("main:side.txt")?;
        let hex = blob.to_string();
        let loose = repo.dir.join("repo/.git/objects").join(&hex[..2]).join(&hex[2..]);
        assert!(
            loose.is_file(),
            "{kind}: the blob is loose, as it was added after packing"
        );
        std::fs::remove_file(&loose)?;

        let mut out = Vec::new();
        let err = write_pack(
            &repo,
            &ids(&repo, &["main"])?,
            &ids(&repo, &["b5"])?,
            &mut out,
            Options::default(),
        )
        .expect_err("a pack that lacks an object it promises is invalid");
        assert!(err.is_not_found(), "{kind}: {err:#}");
        assert!(
            out.is_empty(),
            "{kind}: nothing was written, as the header would promise the object"
        );
    }
    Ok(())
}

/// Receive `pack` in a fresh repository that has `haves` and check what Git thinks of it, then make `tips` reachable
/// and let `fsck` verify there is nothing missing.
fn assert_git_accepts(repo: &Repo, pack: &[u8], tips: &[&str], haves: &[&str], context: &str) -> TestResult {
    let kind = repo.kind;
    let recipient = gix_testtools::tempfile::tempdir()?;
    gix_testtools::git(
        recipient.path(),
        &format!("init -q --bare --object-format={kind} recipient.git"),
    )?;
    let recipient_dir = recipient.path().join("recipient.git");
    for (index, have) in haves.iter().enumerate() {
        repo.git(&format!(
            "push -q {} {have}^{{commit}}:refs/heads/have{index}",
            recipient_dir.display()
        ))?;
    }
    let pack_path = recipient.path().join("incoming.pack");
    std::fs::write(&pack_path, pack)?;
    let output = gix_testtools::git_command(&recipient_dir)
        .args(["index-pack", "--strict", "--stdin"])
        .stdin(std::fs::File::open(&pack_path)?)
        .output()?;
    assert!(
        output.status.success(),
        "{kind} {context}: git rejects the pack: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (index, tip) in tips.iter().enumerate() {
        let id = repo.id(tip)?;
        gix_testtools::git(&recipient_dir, &format!("update-ref refs/tags/tip{index} {id}"))?;
    }
    gix_testtools::git(&recipient_dir, "fsck --full --strict --no-dangling")?;
    Ok(())
}

#[test]
fn packs_are_accepted_by_git_for_both_hashes() -> TestResult {
    for repo in repos()? {
        for case in CASES {
            let mut pack = Vec::new();
            let outcome = write_pack(
                &repo,
                &ids(&repo, case.tips)?,
                &ids(&repo, case.haves)?,
                &mut pack,
                Options::default(),
            )?;
            assert_eq!(
                outcome.bytes_written as usize,
                pack.len(),
                "{} {}",
                repo.kind,
                case.name
            );
            assert_eq!(
                &pack[pack.len() - repo.kind.len_in_bytes()..],
                outcome.trailer.as_slice(),
                "{} {}: the trailer is the pack's hash",
                repo.kind,
                case.name
            );
            assert_git_accepts(&repo, &pack, case.tips, case.haves, case.name)?;
        }
    }
    Ok(())
}

/// The headers of all entries of `pack`, which tell if they are deltas.
fn entry_headers(pack: &[u8], kind: Kind) -> TestResult<Vec<gix_pack::data::entry::Header>> {
    gix_pack::data::input::BytesToEntriesIter::new_from_header(
        pack,
        gix_pack::data::input::Mode::AsIs,
        gix_pack::data::input::EntryDataMode::Ignore,
        kind,
    )?
    .map(|entry| Ok(entry?.header))
    .collect()
}

#[test]
fn deltas_with_offsets_are_only_copied_if_the_receiver_supports_them() -> TestResult {
    for repo in repos()? {
        let tips = ids(&repo, &["b5"])?;
        let mut with_deltas = Vec::new();
        write_pack(&repo, &tips, &[], &mut with_deltas, Options::default())?;
        assert!(
            entry_headers(&with_deltas, repo.kind)?
                .iter()
                .any(|header| matches!(header, gix_pack::data::entry::Header::OfsDelta { .. })),
            "{}: the packed history of the fixture has deltas that are copied as they are",
            repo.kind
        );
        assert_git_accepts(&repo, &with_deltas, &["b5"], &[], "with offset deltas")?;

        let mut without_deltas = Vec::new();
        let options = Options {
            allow_ofs_delta: false,
            ..Default::default()
        };
        write_pack(&repo, &tips, &[], &mut without_deltas, options)?;
        assert!(
            !entry_headers(&without_deltas, repo.kind)?
                .iter()
                .any(gix_pack::data::entry::Header::is_delta),
            "{}: objects are written whole if the receiver can't resolve offsets",
            repo.kind
        );
        assert_git_accepts(&repo, &without_deltas, &["b5"], &[], "without deltas")?;
    }
    Ok(())
}

/// Counts the reads of objects, which tells how much of the database was consumed at any point in time.
#[derive(Clone)]
struct Spy {
    inner: gix_odb::HandleArc,
    reads: Arc<AtomicUsize>,
}

impl gix_object::Find for Spy {
    fn try_find<'a>(
        &self,
        id: &gix_hash::oid,
        buffer: &'a mut Vec<u8>,
    ) -> gix_error::Result<Option<gix_object::Data<'a>>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        gix_object::Find::try_find(&self.inner, id, buffer)
    }
}

impl gix_pack::Find for Spy {
    fn contains(&self, id: &gix_hash::oid) -> bool {
        gix_pack::Find::contains(&self.inner, id)
    }

    fn try_find_cached<'a>(
        &self,
        id: &gix_hash::oid,
        buffer: &'a mut Vec<u8>,
        pack_cache: &mut dyn gix_pack::cache::DecodeEntry,
    ) -> gix_error::Result<Option<(gix_object::Data<'a>, Option<gix_pack::data::entry::Location>)>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        gix_pack::Find::try_find_cached(&self.inner, id, buffer, pack_cache)
    }

    fn location_by_oid(
        &self,
        id: &gix_hash::oid,
        buf: &mut Vec<u8>,
    ) -> gix_error::Result<Option<gix_pack::data::entry::Location>> {
        gix_pack::Find::location_by_oid(&self.inner, id, buf)
    }

    fn pack_offsets_and_oid(&self, pack_id: u32) -> gix_error::Result<Option<Vec<(gix_pack::data::Offset, ObjectId)>>> {
        gix_pack::Find::pack_offsets_and_oid(&self.inner, pack_id)
    }

    fn entry_by_location(&self, location: &gix_pack::data::entry::Location) -> Option<gix_pack::find::Entry> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        gix_pack::Find::entry_by_location(&self.inner, location)
    }
}

/// Notes how many reads were done by the time the first byte of an object is written.
struct FirstObjectSpy {
    written: usize,
    reads: Arc<AtomicUsize>,
    reads_at_first_object: Option<usize>,
}

impl Write for FirstObjectSpy {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let header = gix_pack::data::header::SIZE;
        if self.written + buf.len() > header && self.reads_at_first_object.is_none() {
            // Whatever is able to run ahead of the writer does so while it is held up here, which finds the most that is read
            // before the first object is written.
            std::thread::sleep(std::time::Duration::from_millis(300));
            self.reads_at_first_object = Some(self.reads.load(Ordering::Relaxed));
        }
        self.written += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn objects_are_written_while_they_are_still_being_read_in_bounded_chunks() -> TestResult {
    for repo in repos_from("make_push_bulk_repo.sh")? {
        let reads = Arc::new(AtomicUsize::new(0));
        let spy = Spy {
            inner: repo.db()?,
            reads: reads.clone(),
        };
        let (tips, haves) = (ids(&repo, &["main"])?, ids(&repo, &["base"])?);
        let options = Options {
            chunk_size: 1,
            ..Default::default()
        };

        let counts = missing::counts(
            spy.clone(),
            &tips,
            &haves,
            &gix_utils::progress::Discard,
            &no_interrupt(),
            options,
        )?;
        assert!(
            counts.len() >= 10,
            "{}: eight blobs, their tree and the commit are sent",
            repo.kind
        );
        reads.store(0, Ordering::Relaxed);

        let mut out = FirstObjectSpy {
            written: 0,
            reads: reads.clone(),
            reads_at_first_object: None,
        };
        let outcome = missing::write_pack(spy, counts, &mut out, repo.kind, options)?;
        let total_reads = reads.load(Ordering::Relaxed);
        let reads_at_first_object = out.reads_at_first_object.expect("objects were written");
        assert!(
            reads_at_first_object <= 4,
            "{}: with chunks of one object, at most a few are held while the first is written, which is a constant and not a share of the pack: {reads_at_first_object} of {total_reads}",
            repo.kind,
        );
        assert!(
            total_reads >= outcome.objects,
            "{}: but all objects were read in the end",
            repo.kind
        );
    }
    Ok(())
}
