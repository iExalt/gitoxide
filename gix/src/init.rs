use std::path::Path;

use gix_ref::{
    Category, FullName,
    store::WriteReflog,
    transaction::{PreviousValue, RefEdit},
};

use crate::error::{ResultExt, bail, message, validation};
use crate::{
    Result, ThreadSafeRepository,
    bstr::ByteSlice,
    config::tree::{Feature, Init},
};

/// The name of the branch to use if non is configured via git configuration.
///
/// # Deviation
///
/// We use `main` instead of `master`.
pub const DEFAULT_BRANCH_NAME: &str = "main";

impl ThreadSafeRepository {
    /// Create a repository with work-tree within `directory`, creating intermediate directories as needed.
    ///
    /// Fails without action if the destination directory isn't empty unless
    /// [`create::Options::destination_must_be_empty`][crate::create::Options::destination_must_be_empty] is `None`
    /// or `Some(false)`. Note that initialization still fails if a `.git` directory already exists in
    /// the destination.
    pub fn init(
        directory: impl AsRef<Path>,
        kind: crate::create::Kind,
        options: crate::create::Options,
    ) -> Result<Self> {
        use gix_sec::trust::DefaultForLevel;
        let open_options = crate::open::Options::default_for_level(gix_sec::Trust::Full);
        Self::init_opts(directory, kind, options, open_options)
    }

    /// Similar to [`init`][Self::init()], but allows to determine how exactly to open the newly created repository.
    /// Invalid `init.defaultBranch` values include their bytes as `input` [metadata](gix_error::Error::metadata()).
    ///
    /// References are stored in `files` unless something else is requested like Git does it, which is, in this order,
    /// by `GIT_REFERENCE_BACKEND`, by `init.defaultRefFormat` passed through `open_options`
    /// (the equivalent of `git init --ref-format`), by `GIT_DEFAULT_REF_FORMAT`, by `init.defaultRefFormat`, and by
    /// `feature.experimental` if `init.defaultRefFormat` isn't set. `reftable` is created if the `reftable` feature is enabled,
    /// and otherwise fails with an [unsupported](gix_error::Error::is_unsupported()) error before anything is created, as do
    /// formats at other locations like `files://<payload>`. A name that is not a format fails if it comes from
    /// `GIT_DEFAULT_REF_FORMAT` or `open_options`, and is ignored, with a trace, if it is configured.
    /// As with the other environment and system configuration read here, they are only honored as far as the permissions of
    /// `open_options` allow, so [`Options::isolated()`](crate::open::Options::isolated()) creates `files` regardless of
    /// `GIT_DEFAULT_REF_FORMAT`.
    ///
    /// # Deviation
    ///
    /// Instead of naming the default branch `master`, we name it `main` unless configured explicitly using the `init.defaultBranch`
    /// configuration key.
    pub fn init_opts(
        directory: impl AsRef<Path>,
        kind: crate::create::Kind,
        mut create_options: crate::create::Options,
        open_options: crate::open::Options,
    ) -> Result<Self> {
        let directory = directory.as_ref();
        let ref_format = select_ref_format(&open_options)?;
        if create_options.object_hash.is_none() {
            create_options.object_hash = default_object_format(&open_options)?;
        }
        Self::create_and_open(directory, kind, create_options, open_options, ref_format, true)
    }

    /// Like [`init_opts()`][Self::init_opts()], but without applying `init.defaultObjectFormat` and with the format of references
    /// decided by the caller. Reftables are only created if `create_references` is set, which clones don't do before they
    /// know the object hash, see [`create_reference_database()`].
    pub(crate) fn create_and_open(
        directory: &Path,
        kind: crate::create::Kind,
        create_options: crate::create::Options,
        mut open_options: crate::open::Options,
        ref_format: RefFormat,
        create_references: bool,
    ) -> Result<Self> {
        let (path, capabilities) = crate::create::into_with_ref_format(directory, kind, create_options, ref_format)?;
        if !capabilities.symlink {
            open_options.api_config_overrides.push("core.symlinks=false".into());
        }
        let (git_dir, worktree_dir) = path.into_repository_and_work_tree_directories();
        open_options.git_dir_trust = Some(gix_sec::Trust::Full);
        // The repo will use `core.precomposeUnicode` to adjust the value as needed.
        open_options.current_dir = gix_fs::current_dir(false)
            .or_raise(|| message("Could not obtain the current directory"))?
            .into();
        let repo = ThreadSafeRepository::open_from_paths(git_dir, worktree_dir, open_options, None)?;

        // The template of `files` has `HEAD` point to the default branch, and only a different name needs to be written.
        // For reftables, `HEAD` is written along with the first table.
        if ref_format == RefFormat::Files || create_references {
            write_default_branch(&mut repo.to_thread_local(), ref_format == RefFormat::Reftable)?;
        }

        Ok(repo)
    }
}

/// Create the stack of tables of a repository that stores its references as reftables, and make `HEAD` point to the
/// default branch, which is `init.defaultBranch` or `main`. Do nothing if `repo` stores its references otherwise or already has
/// a stack.
///
/// A stack is created as late as the object hash of the repository is known, as the tables that make it up depend on it.
#[cfg_attr(
    not(any(feature = "blocking-network-client", feature = "async-network-client")),
    allow(dead_code)
)]
pub(crate) fn create_reference_database(repo: &mut crate::Repository) -> Result<()> {
    #[cfg(feature = "reftable")]
    if repo.refs.as_reftable().is_some() && !repo.git_dir().join("reftable").is_dir() {
        write_default_branch(repo, true)?;
    }
    #[cfg(not(feature = "reftable"))]
    let _ = repo;
    Ok(())
}

/// Point `HEAD` to the default branch without writing a reflog, but only if `always` is set or the branch isn't the one the
/// template of `files` uses already. If `always` is set, also create the directory that holds reftables.
fn write_default_branch(repo: &mut crate::Repository, always: bool) -> Result<()> {
    let branch_name = repo
        .config
        .resolved
        .string(Init::DEFAULT_BRANCH)
        .unwrap_or_else(|| DEFAULT_BRANCH_NAME.into());
    if always || branch_name.as_bstr() != DEFAULT_BRANCH_NAME {
        let configured_branch_name = branch_name;
        let sym_ref: FullName = Category::LocalBranch
            .to_full_name(configured_branch_name.as_bstr())
            .or_raise(|| validation("Invalid default branch name").with_input(configured_branch_name.clone()))?;
        gix_validate::reference::branch_name(sym_ref.as_bstr())
            .or_raise(|| validation("Invalid default branch name").with_input(configured_branch_name))?;
        if always {
            std::fs::create_dir_all(repo.git_dir().join("reftable"))
                .or_raise(|| message("Could not create the directory for reftables"))?;
        }
        let prev_write_reflog = repo.refs.set_write_reflog(WriteReflog::Disable);
        let res = repo.edit_reference(RefEdit::update(
            "HEAD".try_into().expect("valid"),
            sym_ref,
            PreviousValue::Any,
            "",
        ));
        repo.refs.set_write_reflog(prev_write_reflog);
        res.or_raise(|| message("Could not edit HEAD reference with new default name"))?;
    }
    Ok(())
}

/// How the references of a repository that is about to be created are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefFormat {
    Files,
    Reftable,
}

/// Decide how Git would store the references of a new repository, and fail if that isn't something we can create.
///
/// This follows Git's precedence, which is `GIT_REFERENCE_BACKEND`, then a format requested explicitly by the caller as
/// `init.defaultRefFormat` using `open_options` (like Git's `--ref-format`), then `GIT_DEFAULT_REF_FORMAT`,
/// then `init.defaultRefFormat`, and then `feature.experimental`, which selects `reftable` unless
/// `init.defaultRefFormat` is set. Like in Git, only the system, global and environment configuration is used.
/// A value of `init.defaultRefFormat` that names no format is ignored, as Git only warns about it, whereas one of
/// `GIT_DEFAULT_REF_FORMAT` is an error.
///
/// It must be called before anything is created, so that requesting something that can't be created doesn't silently
/// create a repository that stores its references in files.
pub(crate) fn select_ref_format(options: &crate::open::Options) -> Result<RefFormat> {
    let from_name = |format: &str| -> Result<RefFormat> {
        match format {
            "files" => Ok(RefFormat::Files),
            #[cfg(feature = "reftable")]
            "reftable" => Ok(RefFormat::Reftable),
            other => bail!(
                gix_error::unsupported(
                    "Creating repositories that store references in this format is not supported by this build"
                )
                .with("format", other.to_owned())
            ),
        }
    };
    let from_name_in = |value: &crate::bstr::BStr| -> Result<RefFormat> {
        match value.as_bytes() {
            b"files" | b"reftable" => from_name(&value.to_str_lossy()),
            _ => Err(
                crate::config::key::error(&Init::DEFAULT_REF_FORMAT, "Unknown reference format")
                    .with_input(value)
                    .validation_error(),
            ),
        }
    };
    if let Some(backend) = crate::open::ref_backend_from_environment(&options.permissions.env)? {
        return backend.as_deref().map_or(Ok(RefFormat::Files), from_name);
    }
    let config = crate::config(None, options)?;
    let filter = options
        .filter_config_section
        .unwrap_or(crate::config::section::is_trusted);
    let key = &Init::DEFAULT_REF_FORMAT;
    // The values that Git is strict about are those that are requested explicitly, which are a format passed through the API
    // like `--ref-format`, and the environment. The configuration is lenient.
    for source in [gix_config::Source::Api, gix_config::Source::EnvOverride] {
        if let Some(value) = config.string_implicit_filter(key, |meta| meta.source == source && filter(meta)) {
            let value = value.ok_or_else(|| missing_value(key))?;
            return from_name_in(value.as_bstr());
        }
    }
    let value = config.string_implicit_filter(key, |meta| {
        !matches!(meta.source, gix_config::Source::Api | gix_config::Source::EnvOverride) && filter(meta)
    });
    if let Some(value) = value {
        let value = value.ok_or_else(|| missing_value(key))?;
        match value.as_bytes() {
            b"files" | b"reftable" => return from_name(&value.to_str_lossy()),
            // Git only warns about this. As there is no channel for warnings other than traces, a trace it is, and the value
            // is ignored as if it wasn't set, which lets `feature.experimental` decide.
            _ => gix_trace::warn!("ignoring unknown ref storage format {value:?} in `init.defaultRefFormat`"),
        }
    }
    let experimental = Feature::EXPERIMENTAL
        .enrich_error(config.boolean_filter(Feature::EXPERIMENTAL, filter))?
        .unwrap_or_default();
    from_name(if experimental { "reftable" } else { "files" })
}

fn missing_value(key: &impl crate::config::tree::Key) -> gix_error::Error {
    crate::config::key::error(key, "Missing configuration value").validation_error()
}

/// Read `init.defaultObjectFormat` without repository context, like Git's format selection.
fn default_object_format(options: &crate::open::Options) -> Result<Option<gix_hash::Kind>> {
    let config = crate::config(None, options)?;
    let key = &Init::DEFAULT_OBJECT_FORMAT;
    let filter = options
        .filter_config_section
        .unwrap_or(crate::config::section::is_trusted);
    let value = config.string_implicit_filter(key, filter);
    value
        .map(|value| {
            let value = value
                .ok_or_else(|| crate::config::key::error(key, "Missing configuration value").validation_error())?;
            key.try_into_object_format(value.as_bstr())
        })
        .transpose()
}
