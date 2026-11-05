//! Check the objects that are received like `git receive-pack` and `git fetch` do when they are told to, and read what that
//! is told from the configuration.
use std::{borrow::Cow, path::PathBuf};

use gix_error::{ErrorExt, Result, ResultExt, message, validation};
use gix_object::bstr::ByteSlice;

use crate::Repository;

/// What `<program>.fsckObjects` and the keys that go with it say about checking the objects that are received.
pub(crate) struct Settings {
    /// `true` if received objects are to be checked.
    pub enabled: bool,
    /// How to check them, if they are. Everything about this is valid, but only if `enabled` are skip lists read and
    /// severities that can't be applied noticed, as Git does when it starts the program that checks.
    pub options: gix_fsck::Options,
}

impl Settings {
    /// Read the settings of `program`, which is `receive` or `fetch`, from the configuration of `repo`.
    ///
    /// These are `<program>.fsckObjects` or else `transfer.fsckObjects`, the severities `<program>.fsck.<msg-id>`, and the
    /// objects to skip in the files `<program>.fsck.skipList`. Fail if a value is invalid, which Git notices when it reads
    /// the configuration, or, if objects are checked, if a skip list can't be read or a severity can't be applied.
    /// Plain `fsck.<msg-id>` and `fsck.skipList` are for `git fsck`, so they don't take part.
    pub(crate) fn from_repository(repo: &Repository, program: &str) -> Result<Self> {
        let config = repo.config_snapshot();
        let boolean = |key: String| -> Result<Option<bool>> {
            config
                .try_boolean(key.as_str())
                .or_raise(|| message!("bad boolean config value for '{key}'"))
        };
        let enabled = match boolean(format!("{program}.fsckObjects"))? {
            Some(enabled) => enabled,
            None => boolean("transfer.fsckObjects".into())?.unwrap_or(false),
        };

        let mut severities = Vec::new();
        let mut skip_lists = Vec::new();
        for section in config.plumbing().sections() {
            let header = section.header();
            if !header.name().eq_ignore_ascii_case(program.as_bytes())
                || header.subsection_name().map(|name| name.as_bytes()) != Some(b"fsck".as_slice())
            {
                continue;
            }
            for name in section.body().value_names() {
                for value in section.body().values(&name) {
                    if name.eq_ignore_ascii_case("skipList") {
                        skip_lists.push(value);
                        continue;
                    }
                    let value = value.to_str_lossy();
                    if gix_fsck::Options::parse_config_entry(&name, &value)?.is_some() {
                        severities.push((name.clone(), value.into_owned()));
                    } else {
                        gix_trace::warn!("Skipping unknown fsck message id '{name}' in {program}.fsck.{name}");
                    }
                }
            }
        }

        let mut options = gix_fsck::Options::strict();
        options.set_big_file_threshold(repo.config.big_file_threshold()?);
        if enabled {
            for (id, value) in &severities {
                options.set_severity(id, value)?;
            }
            for list in skip_lists {
                let path = skip_list_path(repo, list.as_bstr())?;
                let data = std::fs::read(&path)
                    .or_raise(|| message!("could not read the skip list at \"{}\"", path.display()))?;
                options
                    .skip_list_from_bytes(&data, repo.object_hash())
                    .or_raise(|| message!("invalid skip list at \"{}\"", path.display()))?;
            }
        }
        Ok(Settings { enabled, options })
    }
}

/// Resolve the path of a skip list like `git_config_pathname()` does, and like Git, which runs in the git directory, take
/// a relative one from there.
fn skip_list_path(repo: &Repository, value: &gix_object::bstr::BStr) -> Result<PathBuf> {
    let install_dir = crate::path::install_dir().ok();
    let home = repo.config.home_dir();
    let context = crate::config::cache::interpolate_context(install_dir.as_deref(), home.as_deref());
    let path = gix_config::Path::from(Cow::Borrowed(value))
        .interpolate(context)
        .or_raise(|| validation(format!("bad path {value:?}")).raise())?;
    Ok(repo.git_dir().join(path))
}

/// Check the objects of the pack that `inspection` shows the way the program `program` of Git does with `options`, with `shallow` as the
/// commits whose parents are missing so that checking the links doesn't need them, and return what it says about them.
/// If `links` is `false`, what the objects link to isn't required to exist.
///
/// Fail if checking is interrupted through `should_interrupt`, which is something else than refusing the pack.
pub(crate) fn check(
    options: &gix_fsck::Options,
    object_hash: gix_hash::Kind,
    program: gix_fsck::Program,
    shallow: &[gix_hash::ObjectId],
    links: bool,
    inspection: &gix_odb::inspect::Inspection<'_>,
    should_interrupt: Option<&std::sync::atomic::AtomicBool>,
) -> Result<gix_fsck::Outcome> {
    let mut transfer =
        gix_fsck::Transfer::new(options.clone(), object_hash, program).with_shallow(shallow.iter().copied());
    if !links {
        transfer = transfer.without_links();
    }
    inspection.objects(&mut |object| {
        if should_interrupt.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            gix_error::bail!(gix_error::cancelled("Interrupted while checking the received objects"));
        }
        let (kind, size) = (object.kind(), object.size());
        let id = object.id().to_owned();
        let data = if transfer.needs_content(&id, kind, size) {
            Some(object.data()?)
        } else {
            None
        };
        Ok(transfer.object(&id, kind, size, data))
    })?;
    Ok(transfer.finish(&inspection.find()))
}

/// Return the options for the check of a pack that is received in `repo`, as `receive-pack` runs it, with `shallow`
/// as the commits whose parents are missing, so that checking the links doesn't need them.
#[cfg(feature = "receive")]
pub(crate) fn inspector(
    options: gix_fsck::Options,
    object_hash: gix_hash::Kind,
    shallow: Vec<gix_hash::ObjectId>,
) -> gix_odb::quarantine::Inspector {
    gix_odb::quarantine::Inspector::new(move |inspection| {
        let program = if inspection.kept() {
            gix_fsck::Program::IndexPack
        } else {
            gix_fsck::Program::UnpackObjects
        };
        let outcome = check(&options, object_hash, program, &shallow, true, inspection, None)?;
        Ok(gix_odb::quarantine::Inspected {
            accepted: outcome.accepted,
            report: outcome.report.into(),
        })
    })
}
