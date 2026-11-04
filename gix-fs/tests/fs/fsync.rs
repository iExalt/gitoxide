use bstr::ByteSlice;
use gix_fs::fsync::{Components, Method, Options};
use gix_testtools::TestResult;

#[test]
fn defaults_match_git() {
    let default = Components::default();
    assert!(
        default.contains(Components::PACK) && default.contains(Components::DERIVED_METADATA),
        "packs and their metadata are flushed by default"
    );
    assert!(
        !default.contains(Components::REFERENCE),
        "references are not flushed by default, so the default flush count of a ref update is zero"
    );
    assert_eq!(
        default.contains(Components::LOOSE_OBJECT),
        cfg!(windows),
        "loose objects are flushed by default on Windows only"
    );
}

#[test]
fn unknown_tokens_are_reported_and_ignored() {
    // Tokens end at ',' only and are compared in full: trailing white space is part of the token, as in Git.
    let (components, ignored) = Components::parse("reference, pack ,bogus, index".into());
    assert_eq!(
        components,
        Components::DEFAULT | Components::REFERENCE | Components::INDEX,
        "known tokens still apply next to unknown ones"
    );
    assert_eq!(ignored, ["pack ", "bogus"]);
}

#[test]
fn method_values() {
    assert_eq!(Method::parse("fsync".into()), Some(Method::Fsync));
    assert_eq!(Method::parse("writeout-only".into()), Some(Method::WriteoutOnly));
    assert_eq!(Method::parse("batch".into()), Some(Method::Batch));
    assert_eq!(
        Method::parse("Batch".into()),
        None,
        "values are case-sensitive and unknown ones are ignored by Git"
    );
}

#[test]
fn flush_is_gated_by_component_and_batching_needs_both_method_and_component() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = std::fs::File::create(dir.path().join("f"))?;
    let mut options = Options {
        components: Components::NONE,
        ..Options::default()
    };
    assert!(
        !options.flush(&file, Components::REFERENCE)?,
        "nothing is flushed for a disabled component"
    );
    options.components = Components::REFERENCE;
    assert!(
        options.flush(&file, Components::REFERENCE)?,
        "an enabled component is flushed"
    );

    options.fsync_object_files = true;
    assert!(
        options.flush(&file, Components::LOOSE_OBJECT)?,
        "the deprecated core.fsyncObjectFiles enables loose objects only"
    );
    assert!(!options.flush(&file, Components::PACK)?);
    assert!(
        options.loose_object_batch().is_none(),
        "without batch method there is no batching, even with fsyncObjectFiles"
    );

    options.method = Method::Batch;
    assert!(
        options.loose_object_batch().is_none(),
        "batching also needs the loose-object component"
    );
    options.components = Components::LOOSE_OBJECT;
    let batch = options.loose_object_batch().expect("method and component are set");
    batch.flush_object(&file)?;
    batch.barrier(dir.path())?;
    options.barrier(dir.path())?;
    assert_eq!(
        std::fs::read_dir(dir.path())?.count(),
        1,
        "barriers remove their scratch file again, only our own file remains"
    );
    Ok(())
}

#[test]
fn barrier_fails_in_missing_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(Options::default().barrier(&dir.path().join("missing")).is_err());
}

/// Compare the flushes that Git performs with the flushes the configuration selects, for configurations that cover
/// the parsing rules of `core.fsync`, `core.fsyncMethod` and the deprecated `core.fsyncObjectFiles`.
///
/// Git reports each flush in its `trace2` event stream as the `fsync` counters `hardware-flush` and `writeout-only`,
/// which tells how often and how data was flushed, but not *which* file it was. That is the subject of the
/// flush-order tests. One command per component keeps the counts attributable.
#[cfg(unix)]
mod git_differential {
    use std::process::Stdio;

    use super::*;

    /// `(hardware flushes, write-outs)`
    type Counts = (u32, u32);

    /// Run Git in `dir`. `git_command()` removes all `GIT_*` variables of the environment, including `GIT_TEST_FSYNC`,
    /// which would otherwise silence Git's flushes and its counters.
    fn run_git(dir: &std::path::Path, config: &[String], args: &[&str], stdin: Option<&[u8]>) -> TestResult<Counts> {
        let trace = dir.join("trace2.json");
        std::fs::remove_file(&trace).ok();
        let mut cmd = gix_testtools::git_command(dir);
        for c in config {
            cmd.args(["-c", c]);
        }
        cmd.args(args).env("GIT_TRACE2_EVENT", &trace);
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        if let Some(input) = stdin {
            use std::io::Write;
            child.stdin.take().expect("piped").write_all(input)?;
        }
        let out = child.wait_with_output()?;
        assert!(out.status.success(), "git {args:?} failed: {}", out.stderr.as_bstr());
        let events = std::fs::read(&trace)?;
        let count = |name: &str| -> u32 {
            events
                .lines()
                .filter(|l| l.contains_str("\"category\":\"fsync\"") && l.contains_str(format!("\"name\":\"{name}\"")))
                .map(|l| {
                    let digits = l.rsplit_once_str("\"count\":").expect("counter has a count").1;
                    digits
                        .iter()
                        .take_while(|b| b.is_ascii_digit())
                        .fold(0, |n, b| n * 10 + u32::from(b - b'0'))
                })
                .sum()
        };
        Ok((count("hardware-flush"), count("writeout-only")))
    }

    /// The flushes `options` selects for `times` files of the given `component`, written individually.
    fn expected(options: &Options, component: Components, times: u32) -> Counts {
        if !options.is_enabled(component) {
            return (0, 0);
        }
        match options.method {
            Method::WriteoutOnly => (0, times),
            Method::Fsync | Method::Batch => (times, 0),
        }
    }

    fn sum(a: Counts, b: Counts) -> Counts {
        (a.0 + b.0, a.1 + b.1)
    }

    fn init_repo(dir: &std::path::Path) -> TestResult {
        gix_testtools::git(dir, "init -q")?;
        Ok(())
    }

    #[test]
    fn flush_counts_match_git_for_each_component() -> TestResult {
        let work = tempfile::tempdir()?;

        // A pack of one blob, which a receiving repository needs to index.
        let source = work.path().join("source");
        std::fs::create_dir(&source)?;
        init_repo(&source)?;
        std::fs::write(source.join("blob"), "pack me\n")?;
        let blob_id = gix_testtools::git(&source, "hash-object -w blob")?.trim().to_owned();
        let pack = {
            let mut cmd = gix_testtools::git_command(&source);
            cmd.args(["pack-objects", "--stdout"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
            let mut child = cmd.spawn()?;
            {
                use std::io::Write;
                writeln!(child.stdin.take().expect("piped"), "{blob_id}")?;
            }
            child.wait_with_output()?.stdout
        };

        // (core.fsync, core.fsyncMethod, core.fsyncObjectFiles); `None` leaves a key unset.
        let configs: &[(Option<&str>, Option<&str>, Option<&str>)] = &[
            (None, None, None),
            (Some("none"), None, None),
            (Some("all"), None, None),
            (Some("reference"), None, None),
            (Some("loose-object"), None, None),
            (Some("-pack"), None, None),
            (Some("committed,-reference"), None, None),
            (Some("pack,-pack"), None, None),
            (Some("none,pack"), None, None),
            (Some("pack,none"), None, None),
            (Some("pack ,index"), None, None),
            (Some("pack-metadata"), None, None),
            (Some("derived-metadata,reference"), None, None),
            (Some("objects"), None, None),
            (Some("added"), None, None),
            (Some("-"), None, None),
            (Some("-,all"), None, None),
            (Some("bogus,reference"), None, None),
            (Some(",\t reference"), None, None),
            (Some("ALL"), None, None),
            (None, None, Some("true")),
            (Some("none"), None, Some("true")),
            (Some("all"), Some("writeout-only"), None),
            (Some("all"), Some("fsync"), None),
            (Some("all"), Some("bogus"), None),
            (Some("loose-object"), Some("batch"), None),
            (Some("reference"), Some("batch"), None),
            (None, Some("batch"), Some("true")),
        ];

        for (core_fsync, method, object_files) in configs.iter().copied() {
            let mut options = Options::default();
            let mut config = Vec::new();
            if let Some(value) = core_fsync {
                options.components = Components::parse(value.into()).0;
                config.push(format!("core.fsync={value}"));
            }
            if let Some(value) = method {
                options.method = Method::parse(value.into()).unwrap_or(options.method);
                config.push(format!("core.fsyncMethod={value}"));
            }
            if let Some(value) = object_files {
                options.fsync_object_files = value == "true";
                config.push(format!("core.fsyncObjectFiles={value}"));
            }
            let context = format!("{core_fsync:?} {method:?} {object_files:?}");

            let dir = tempfile::tempdir_in(work.path())?;
            let repo = dir.path();
            init_repo(repo)?;

            std::fs::write(repo.join("blob"), "loose\n")?;
            let got = run_git(repo, &config, &["hash-object", "-w", "blob"], None)?;
            assert_eq!(
                got,
                expected(&options, Components::LOOSE_OBJECT, 1),
                "loose object, {context}"
            );

            let tag_target = gix_testtools::git(repo, "hash-object -w blob")?.trim().to_owned();
            let got = run_git(repo, &config, &["update-ref", "refs/tags/u", &tag_target], None)?;
            assert_eq!(
                got,
                expected(&options, Components::REFERENCE, 1),
                "loose reference lock file, {context}"
            );

            let got = run_git(repo, &config, &["index-pack", "--stdin"], Some(&pack))?;
            assert_eq!(
                got,
                sum(
                    expected(&options, Components::PACK, 1),
                    expected(&options, Components::PACK_METADATA, 2)
                ),
                "pack, its index and its reverse index, {context}"
            );

            let got = run_git(repo, &config, &["read-tree", "--empty"], None)?;
            assert_eq!(got, expected(&options, Components::INDEX, 1), "index, {context}");

            let tree_id = gix_testtools::git(repo, "write-tree")?.trim().to_owned();
            let commit_id = gix_testtools::git(repo, &format!("commit-tree {tree_id} -m c"))?
                .trim()
                .to_owned();
            gix_testtools::git(repo, &format!("update-ref refs/heads/main {commit_id}"))?;
            let got = run_git(repo, &config, &["commit-graph", "write", "--reachable"], None)?;
            assert_eq!(
                got,
                expected(&options, Components::COMMIT_GRAPH, 1),
                "commit-graph, {context}"
            );

            // Two new loose objects are written in one operation. With batching, they are written out one by one, followed
            // by one full flush, which is the barrier.
            std::fs::write(repo.join("one"), "one\n")?;
            std::fs::write(repo.join("two"), "two\n")?;
            let got = run_git(repo, &config, &["add", "one", "two"], None)?;
            let want = if options.loose_object_batch().is_some() {
                (1, 2)
            } else {
                sum(
                    expected(&options, Components::LOOSE_OBJECT, 2),
                    expected(&options, Components::INDEX, 1),
                )
            };
            assert_eq!(got, want, "two loose objects added in one operation, {context}");
        }
        Ok(())
    }
}
