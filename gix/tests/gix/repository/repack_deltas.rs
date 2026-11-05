//! How small repacked packs are, with `git repack -a -d -f` and `git repack -a -d` as the reference.
//! See `make_delta_repo.sh` for the repositories, which have all their objects loose.
use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool, time::Instant};

use gix::maintenance::repack::{Deltas, Options};
use gix_hash::Kind;
use gix_testtools::{Creation, TestResult};

fn kinds() -> impl Iterator<Item = Kind> {
    [Kind::Sha1, Kind::Sha256]
        .into_iter()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
}

fn fixture(kind: Kind) -> TestResult<gix_testtools::tempfile::TempDir> {
    Ok(gix_testtools::scripted_fixture_writable_with_args(
        "make_delta_repo.sh",
        [kind.to_string()],
        Creation::CopyFromReadOnly,
    )?)
}

/// The size of the packs of a repository, which are the only thing in it that Git's repack and ours leave behind besides
/// the refs.
fn pack_bytes(repo_dir: &Path) -> TestResult<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(repo_dir.join(".git/objects/pack"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "pack") {
            total += path.metadata()?.len();
        }
    }
    Ok(total)
}

/// The longest chain of deltas in the packs, as Git's `verify-pack` counts them, which also checks that every pack is valid.
fn longest_chain(repo_dir: &Path) -> TestResult<usize> {
    let mut longest = 0;
    for entry in std::fs::read_dir(repo_dir.join(".git/objects/pack"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "idx") {
            let listing = gix_testtools::git(repo_dir, &format!("verify-pack -v {}", path.display()))?;
            for line in listing.lines() {
                if let Some(length) = line.strip_prefix("chain length = ") {
                    longest = longest.max(length.split(':').next().and_then(|n| n.parse().ok()).unwrap_or(0));
                }
            }
        }
    }
    Ok(longest)
}

/// For each object in the packs the object it is a delta of, as `git verify-pack` lists them.
fn bases(repo_dir: &Path) -> TestResult<BTreeMap<String, Option<String>>> {
    let mut bases = BTreeMap::new();
    for entry in std::fs::read_dir(repo_dir.join(".git/objects/pack"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "idx") {
            let listing = gix_testtools::git(repo_dir, &format!("verify-pack -v {}", path.display()))?;
            for line in listing.lines() {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() >= 5 && ["commit", "tree", "blob", "tag"].contains(&fields[1]) {
                    bases.insert(fields[0].to_owned(), fields.get(6).map(|base| (*base).to_owned()));
                }
            }
        }
    }
    Ok(bases)
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

fn deltas(window: Option<usize>, depth: Option<u32>, reuse_deltas: bool) -> Deltas {
    let mut deltas = Deltas::default();
    deltas.window = window;
    deltas.depth = depth;
    deltas.reuse_deltas = reuse_deltas;
    deltas
}

fn options(deltas: Deltas) -> Options {
    let mut options = Options::default();
    options.deltas = Some(deltas);
    options
}

#[test]
fn packs_made_without_reusing_deltas_are_about_as_small_as_gits() -> TestResult {
    for kind in kinds() {
        for (name, limit) in [("heavy", 1.25), ("ordinary", 1.10)] {
            let (git, native) = (fixture(kind)?, fixture(kind)?);
            let (git_dir, native_dir) = (git.path().join(name), native.path().join(name));
            let started = Instant::now();
            gix_testtools::git(&git_dir, "-c pack.threads=1 repack -a -d -f --window=10 --depth=50 -q")?;
            let git_time = started.elapsed();

            let started = Instant::now();
            let outcome = gix::open_opts(&native_dir, crate::restricted())?
                .repack(options(deltas(Some(10), Some(50), false)), &never())?;
            let native_time = started.elapsed();

            let (expected, actual) = (pack_bytes(&git_dir)?, pack_bytes(&native_dir)?);
            let ratio = actual as f64 / expected as f64;
            eprintln!(
                "{kind} {name}: {actual} bytes against Git's {expected}, a ratio of {ratio:.3}, in {native_time:?} against {git_time:?}, with {} of {} objects as deltas",
                outcome.deltas, outcome.packed
            );
            assert!(outcome.deltas > 0, "{kind} {name}: deltas were found");
            assert!(
                ratio <= limit,
                "{kind} {name}: the pack is at most {limit} times as large as Git's, but is {ratio:.3} times, {actual} against {expected} bytes"
            );
            assert!(
                longest_chain(&native_dir)? <= 50,
                "{kind} {name}: no chain is longer than the depth that was asked for"
            );
            gix_testtools::git(&native_dir, "fsck --full --strict")?;
        }
    }
    Ok(())
}

#[test]
fn deltas_that_exist_are_kept_and_what_is_new_is_searched_for() -> TestResult {
    for kind in kinds() {
        let (git, native) = (fixture(kind)?, fixture(kind)?);
        let (git_dir, native_dir) = (git.path().join("heavy"), native.path().join("heavy"));
        // deltas exist, and then some versions are added that are loose, which have to find the deltas of the pack
        for dir in [&git_dir, &native_dir] {
            gix_testtools::git(dir, "repack -a -d -q")?;
            for version in 0..6 {
                let text = std::fs::read_to_string(dir.join("one.txt"))?;
                std::fs::write(
                    dir.join("one.txt"),
                    format!("{}added later {version}\n", text.replacen("line 7 ", "line seven ", 1)),
                )?;
                gix_testtools::git(dir, "commit -q -a -m later")?;
            }
        }
        gix_testtools::git(&git_dir, "-c pack.threads=1 repack -a -d -q --window=10 --depth=50")?;
        let before = bases(&native_dir)?;
        let outcome = gix::open_opts(&native_dir, crate::restricted())?.repack(Options::default(), &never())?;

        let (expected, actual) = (pack_bytes(&git_dir)?, pack_bytes(&native_dir)?);
        let ratio = actual as f64 / expected as f64;
        eprintln!(
            "{kind} reusing deltas: {actual} bytes against Git's {expected}, a ratio of {ratio:.3}, with {} of {} objects as deltas",
            outcome.deltas, outcome.packed
        );
        assert!(
            ratio <= 1.10,
            "{kind}: with deltas reused and the new objects searched, the pack is at most 1.10 times as large as Git's: {actual} against {expected} bytes"
        );
        let after = bases(&native_dir)?;
        let changed = before
            .iter()
            .filter(|(id, base)| base.is_some() && after.get(*id) != Some(base))
            .count();
        assert_eq!(
            changed, 0,
            "{kind}: deltas that exist, with their bases in the new pack, are kept as they are"
        );
        assert!(
            after.values().filter(|base| base.is_some()).count()
                > before.values().filter(|base| base.is_some()).count(),
            "{kind}: what is new is searched for deltas, too"
        );
        assert!(
            longest_chain(&native_dir)? <= 50,
            "{kind}: chains stay within the depth"
        );
        gix_testtools::git(&native_dir, "fsck --full --strict")?;
    }
    Ok(())
}

#[test]
fn the_configuration_decides_the_window_and_the_depth_unless_options_say_otherwise() -> TestResult {
    for kind in kinds() {
        let run = |settings: &[(&str, &str)], deltas: Option<Deltas>| -> TestResult<(u64, usize, usize)> {
            let dir = fixture(kind)?;
            let repo_dir = dir.path().join("ordinary");
            for (key, value) in settings {
                gix_testtools::git(&repo_dir, &format!("config {key} {value}"))?;
            }
            let mut repack_options = Options::default();
            if let Some(deltas) = deltas {
                repack_options = options(deltas);
            }
            let outcome = gix::open_opts(&repo_dir, crate::restricted())?.repack(repack_options, &never())?;
            Ok((pack_bytes(&repo_dir)?, outcome.deltas, longest_chain(&repo_dir)?))
        };
        let (default_bytes, default_deltas, default_chain) = run(&[], None)?;
        let (no_window_bytes, no_window_deltas, _) = run(&[("pack.window", "0")], None)?;
        let (_, _, shallow_chain) = run(&[("pack.depth", "1")], None)?;
        let (_, overridden_deltas, _) = run(&[("pack.window", "0")], Some(deltas(Some(10), None, true)))?;
        assert!(default_deltas > 0, "{kind}: the default window finds deltas");
        assert!(
            default_chain > 1,
            "{kind}: and chains of them, which the next case limits"
        );
        assert_eq!(no_window_deltas, 0, "{kind}: `pack.window=0` finds none");
        assert!(
            no_window_bytes > default_bytes,
            "{kind}: {no_window_bytes} bytes without deltas are more than {default_bytes} with them"
        );
        assert_eq!(
            shallow_chain, 1,
            "{kind}: `pack.depth=1` makes no chain longer than one delta"
        );
        assert!(
            overridden_deltas > 0,
            "{kind}: what the options say wins over the configuration"
        );
    }
    Ok(())
}
