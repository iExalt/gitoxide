use std::fmt::Formatter;
use std::path::PathBuf;

use gix::{
    Result,
    bstr::BString,
    error::{OptionExt, ResultExt, message},
};

#[cfg(feature = "archive")]
pub mod archive;
pub mod branch;
pub mod cat;
pub use cat::function::cat;
pub mod blame;
pub mod commit;
pub mod config;
mod credential;
pub use credential::function as credential;
pub mod attributes;
#[cfg(feature = "clean")]
pub mod clean;
#[cfg(feature = "clean")]
pub use clean::function::clean;
pub mod diff;
pub mod dirty;
pub mod dirwalk;
mod editor;
pub use editor::function as editor;
#[cfg(feature = "blocking-client")]
pub mod clone;
pub mod exclude;
#[cfg(feature = "blocking-client")]
pub mod fetch;
#[cfg(feature = "blocking-client")]
pub use clone::function::clone;
#[cfg(feature = "blocking-client")]
pub use fetch::function::fetch;

pub mod commitgraph;
mod fsck;
pub use fsck::function as fsck;
pub mod index;
pub mod log;
pub mod mailmap;
mod merge_base;
pub use merge_base::merge_base;
pub mod merge;
pub mod odb;
pub mod remote;
pub mod revision;
pub mod status;
pub mod submodule;
pub mod tag;
pub mod tree;
pub mod verify;
pub mod worktree;

/// Initialize a repository in `directory`, defaulting to the current directory, like `git init` would.
///
/// See [`init_with_ref_format()`] for how it selects the reference format.
pub fn init(directory: Option<PathBuf>) -> Result<gix::discover::repository::Path> {
    init_with_ref_format(directory, None)
}

/// Like [`init()`], but with `ref_format` as the equivalent of `git init --ref-format`.
///
/// `ref_format` is the equivalent of `git init --ref-format`. Like in Git, it takes precedence over
/// `GIT_DEFAULT_REF_FORMAT` and `init.defaultRefFormat`, but not over `GIT_REFERENCE_BACKEND`.
/// `reftable` is refused, before anything is created, if the `reftable` feature isn't enabled.
///
/// Apart from the reference format, it applies `init.defaultBranch` and `init.defaultObjectFormat` as well.
pub fn init_with_ref_format(
    directory: Option<PathBuf>,
    ref_format: Option<String>,
) -> Result<gix::discover::repository::Path> {
    use gix::sec::trust::DefaultForLevel;
    let mut open_options = gix::open::Options::default_for_level(gix::sec::Trust::Full);
    if let Some(ref_format) = ref_format {
        open_options = open_options.config_overrides([format!("init.defaultRefFormat={ref_format}")]);
    }
    let repo = gix::ThreadSafeRepository::init_opts(
        directory.unwrap_or_default(),
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        open_options,
    )
    .or_raise(|| message("Repository initialization failed"))?;
    let cwd = std::env::current_dir().or_raise(|| message("Could not obtain the current directory"))?;
    gix::discover::repository::Path::from_dot_git_dir(
        repo.git_dir().to_owned(),
        gix::discover::repository::Kind::WorkTree { linked_git_dir: None },
        &cwd,
    )
    .ok_or_raise(|| message("The new repository could not be located"))
}

pub enum PathsOrPatterns {
    Paths(Box<dyn std::iter::Iterator<Item = BString>>),
    Patterns(Vec<BString>),
}

struct HexId<'a>(gix::Id<'a>, bool);

impl<'a> HexId<'a> {
    pub fn new(id: gix::Id<'a>, long_hex: bool) -> Self {
        HexId(id, long_hex)
    }
}

impl std::fmt::Display for HexId<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let HexId(id, long_hex) = self;
        if *long_hex {
            id.fmt(f)
        } else {
            id.shorten_or_id().fmt(f)
        }
    }
}
