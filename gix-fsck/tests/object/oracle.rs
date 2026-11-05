//! Run Git as the oracle for what the checker is supposed to do, and compare.
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use gix_error::TestResult;
use gix_fsck::{Checker, Diagnostic, Options, Refusal, Severity};
use gix_hash::ObjectId;
use gix_object::Kind;

/// The objects every case can refer to, and the repository that has them.
pub struct Base {
    pub objects: PathBuf,
    pub blob: ObjectId,
    pub tree: ObjectId,
    pub subtree: ObjectId,
    pub commit: ObjectId,
    pub parent: ObjectId,
    pub tag: ObjectId,
}

impl Base {
    pub fn new() -> TestResult<Base> {
        let repo = gix_testtools::scripted_fixture_read_only("make_oracle_repo.sh")?.join("base");
        let rev = |spec: &str| -> TestResult<ObjectId> {
            let out = gix_testtools::git(&repo, &format!("rev-parse {spec}"))?;
            Ok(ObjectId::from_hex(out.trim().as_bytes())?)
        };
        let objects = std::fs::canonicalize(repo.join(".git").join("objects"))?;
        Ok(Base {
            blob: rev("HEAD:a")?,
            tree: rev("HEAD^{tree}")?,
            subtree: rev("HEAD:d")?,
            commit: rev("HEAD")?,
            parent: rev("HEAD~1")?,
            tag: rev("refs/tags/v1")?,
            objects,
        })
    }

    /// A new handle to the objects, which isn't shareable between threads.
    pub fn odb(&self) -> TestResult<gix_odb::Handle> {
        let mut odb = gix_odb::at(&self.objects, gix_testtools::object_hash())?;
        odb.refresh_never();
        Ok(odb)
    }
}

/// An object to check, and how it came to be.
pub struct Case {
    pub name: String,
    pub kind: Kind,
    pub data: Vec<u8>,
    /// What to pass as `--strict=<id>=<severity>`.
    pub overrides: Vec<(&'static str, &'static str)>,
    /// Put the object itself on the skip list.
    pub skipped: bool,
}

impl Case {
    pub fn with(mut self, id: &'static str, severity: &'static str) -> Self {
        self.name = format!("{}-with-{id}-{severity}", self.name);
        self.overrides.push((id, severity));
        self
    }

    pub fn skipped(mut self) -> Self {
        self.name = format!("{}-skipped", self.name);
        self.skipped = true;
        self
    }

    pub fn id(&self) -> ObjectId {
        gix_object::compute_hash(gix_testtools::object_hash(), self.kind, &self.data).expect("hashable")
    }

    /// The options the checker has to be given to do what the arguments to Git say.
    pub fn options(&self) -> Options {
        let mut options = Options::strict();
        for (id, severity) in &self.overrides {
            options.set_severity(id, severity).expect("valid override");
        }
        if self.skipped {
            options.skip(self.id());
        }
        options
    }
}

pub fn case(name: impl Into<String>, kind: Kind, data: impl Into<Vec<u8>>) -> Case {
    Case {
        name: name.into(),
        kind,
        data: data.into(),
        overrides: Vec::new(),
        skipped: false,
    }
}

/// How an object was received.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Observed {
    pub accepted: bool,
    /// The lines Git prints with a severity, which is what is compared.
    pub lines: Vec<String>,
}

/// A pack of one object, which has as many entries as the `unpack-objects` limit of receive-pack needs to explode it
/// and as little compression as is needed to not need a compressor.
fn pack_of_one(kind: Kind, data: &[u8]) -> Vec<u8> {
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&1u32.to_be_bytes());
    let type_bits = match kind {
        Kind::Commit => 1,
        Kind::Tree => 2,
        Kind::Blob => 3,
        Kind::Tag => 4,
    };
    let mut size = data.len();
    let mut first = (type_bits << 4) | (size & 0xf) as u8;
    size >>= 4;
    while size != 0 {
        pack.push(first | 0x80);
        first = (size & 0x7f) as u8;
        size >>= 7;
    }
    pack.push(first);
    // A zlib stream of stored blocks.
    pack.extend_from_slice(&[0x78, 0x01]);
    let mut chunks = data.chunks(0xffff).peekable();
    if chunks.peek().is_none() {
        pack.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        let len = chunk.len() as u16;
        pack.push(u8::from(chunks.peek().is_none()));
        pack.extend_from_slice(&len.to_le_bytes());
        pack.extend_from_slice(&(!len).to_le_bytes());
        pack.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    pack.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut hasher = gix_hash::hasher(gix_testtools::object_hash());
    hasher.update(&pack);
    let trailer = hasher.try_finalize().expect("no collision");
    pack.extend_from_slice(trailer.as_slice());
    pack
}

/// How much of what is printed has to be the same.
#[derive(Clone, Copy)]
pub enum Compare {
    /// Only if the object is accepted.
    Verdict,
    /// Also the errors and warnings, but not what is said when giving up.
    WithoutFatal,
    /// Everything.
    Full,
}

impl Compare {
    pub fn is_verdict(self) -> bool {
        matches!(self, Compare::Verdict)
    }
}

#[derive(Clone, Copy)]
pub enum Driver {
    UnpackObjects,
    IndexPack,
}

/// Let Git receive the object `case` as part of a pack, with `receive-pack`'s options for fsck, and see what happens.
pub fn git_observed(base: &Base, worker: &Path, scratch: &Path, case: &Case, driver: Driver) -> TestResult<Observed> {
    let objects = scratch.join("objects");
    std::fs::create_dir_all(objects.join("pack"))?;
    let mut cmd = gix_testtools::git_command(worker);
    let mut strict = String::from("--strict");
    let mut messages: Vec<String> = case
        .overrides
        .iter()
        .map(|(id, severity)| format!("{id}={severity}"))
        .collect();
    if case.skipped {
        let skip_list = scratch.join("skip-list");
        std::fs::write(&skip_list, format!("{}\n", case.id()))?;
        messages.push(format!("skiplist={}", skip_list.display()));
    }
    if !messages.is_empty() {
        strict = format!("{strict}={}", messages.join(","));
    }
    match driver {
        Driver::UnpackObjects => cmd.args(["unpack-objects", "-q"]).arg(&strict),
        Driver::IndexPack => cmd.args(["index-pack", "--stdin"]).arg(&strict),
    };
    let mut child = cmd
        .env("GIT_OBJECT_DIRECTORY", &objects)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &base.objects)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let pack = pack_of_one(case.kind, &case.data);
    // Git may stop reading as soon as it found a problem.
    child.stdin.take().expect("piped").write_all(&pack).ok();
    let output = child.wait_with_output()?;
    let lines = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter(|line| {
            ["error: ", "warning: ", "fatal: "]
                .iter()
                .any(|prefix| line.starts_with(prefix))
        })
        .map(ToOwned::to_owned)
        .collect();
    std::fs::remove_dir_all(scratch).ok();
    Ok(Observed {
        accepted: output.status.success(),
        lines,
    })
}

/// Git replaces control characters other than tabs and newlines by question marks when it prints.
fn printable(line: String) -> String {
    line.chars()
        .map(|c| {
            if c.is_control() && c != '\t' && c != '\n' {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// What the checker has to say about `case`, phrased as `unpack-objects` does.
pub fn checker_observed(odb: &gix_odb::Handle, options: Options, case: &Case, driver: Driver) -> Observed {
    let mut checker = Checker::new(options, gix_testtools::object_hash());
    let id = gix_object::compute_hash(gix_testtools::object_hash(), case.kind, &case.data).expect("hashable");
    let mut out = Vec::new();
    let result = checker.check(&id, case.kind, case.data.len() as u64, Some(&case.data), &mut out);
    let mut lines: Vec<String> = out
        .iter()
        .map(|diagnostic| match diagnostic {
            Diagnostic::Note(text) => format!("error: {text}"),
            Diagnostic::Report(report) => format!(
                "{}: {report}",
                if report.severity == Severity::Error {
                    "error"
                } else {
                    "warning"
                }
            ),
        })
        .collect();
    let (link_error, check_failed) = match driver {
        Driver::UnpackObjects => ("fatal: object of unexpected type", "fatal: fsck error in packed object"),
        Driver::IndexPack => (
            "fatal: did not receive expected object",
            "fatal: fsck error in packed object",
        ),
    };
    let accepted = match result {
        Err(Refusal::Unparsable { kind, detail, .. }) => {
            lines.extend(detail.map(|detail| format!("error: {detail}")));
            lines.push(format!("fatal: invalid {kind}"));
            false
        }
        Err(refusal) => {
            lines.push(format!("fatal: {refusal}"));
            false
        }
        Ok(true) => {
            lines.push(check_failed.into());
            false
        }
        Ok(false) => {
            let mut deferred = Vec::new();
            match checker.finish(odb, &mut deferred) {
                Err(_) => {
                    lines.push(link_error.into());
                    false
                }
                Ok(failed) => {
                    lines.extend(deferred.iter().map(|d| match d {
                        Diagnostic::Report(report) => format!(
                            "{}: {report}",
                            if report.severity == Severity::Error {
                                "error"
                            } else {
                                "warning"
                            }
                        ),
                        Diagnostic::Note(text) => format!("error: {text}"),
                    }));
                    if failed {
                        lines.push("fatal: fsck error in pack objects".into());
                    }
                    !failed
                }
            }
        }
    };
    Observed {
        accepted,
        lines: lines.into_iter().map(printable).collect(),
    }
}

/// Run all `cases` against Git and the checker with `threads` workers, and return a description of every case where they differ.
///
/// What of the output is compared depends on `compare`. `driver` selects what Git does.
pub fn differences(
    base: &Base,
    cases: &[Case],
    driver: Driver,
    compare: &(dyn Fn(&Case) -> Compare + Sync),
) -> TestResult<Vec<String>> {
    let next = AtomicUsize::new(0);
    let problems = Mutex::new(Vec::new());
    let workers = 4;
    let root = gix_testtools::tempfile::tempdir()?;
    std::thread::scope(|scope| -> TestResult {
        let mut handles = Vec::new();
        for worker_idx in 0..workers {
            let (next, problems, root) = (&next, &problems, root.path());
            handles.push(scope.spawn(move || -> TestResult {
                let worker = root.join(format!("worker-{worker_idx}"));
                gix_testtools::git(
                    root,
                    &format!(
                        "init -q --bare --object-format={} {}",
                        match gix_testtools::object_hash() {
                            gix_hash::Kind::Sha1 => "sha1",
                            _ => "sha256",
                        },
                        worker.display()
                    ),
                )?;
                let odb = base.odb()?;
                loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    let Some(case) = cases.get(idx) else { return Ok(()) };
                    let scratch = root.join(format!("scratch-{worker_idx}"));
                    let theirs = git_observed(base, &worker, &scratch, case, driver)?;
                    let mut ours = checker_observed(&odb, case.options(), case, driver);
                    let mut expected = theirs.clone();
                    match compare(case) {
                        Compare::Verdict => {
                            ours.lines.clear();
                            expected.lines.clear();
                        }
                        Compare::WithoutFatal => {
                            ours.lines.retain(|line| !line.starts_with("fatal: "));
                            expected.lines.retain(|line| !line.starts_with("fatal: "));
                        }
                        Compare::Full => {}
                    }
                    if ours != expected {
                        problems.lock().expect("no panic while held").push(format!(
                            "{}:\n  git:  accepted={} {:#?}\n  ours: accepted={} {:#?}",
                            case.name, theirs.accepted, theirs.lines, ours.accepted, ours.lines
                        ));
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker doesn't panic")?;
        }
        Ok(())
    })?;
    let mut problems = problems.into_inner().expect("no panic while held");
    problems.sort();
    Ok(problems)
}
