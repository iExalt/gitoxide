//! Check the content of objects the way `git index-pack --strict` and `git unpack-objects --strict` do.
use std::fmt;

use gix_hash::{ObjectId, oid};
use gix_hashtable::{HashMap, HashSet};
use gix_object::{
    Kind,
    bstr::{BStr, BString},
};

use crate::{MessageId, Options, Severity};

mod blob;
mod commit;
mod ident;
mod links;
mod submodule;
mod tag;
mod tree;
mod url;

pub use links::LinkError;

/// What the checker found worth telling about an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// A problem that has a [message id](MessageId) and a severity, which is what Git prints as `error: object <id>: <message>`
    /// or `warning: object <id>: <message>`.
    Report(Report),
    /// A line Git prints as `error: <text>` regardless of the severity configured for any message or objects to skip,
    /// to explain how a tree couldn't be decoded. It always precedes the [report](Report) of `badTree`.
    Note(String),
}

/// A problem found in an object, with the [id](MessageId) that allows configuring its [severity](Severity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The object with the problem.
    pub oid: ObjectId,
    /// The kind of the object with the problem.
    pub kind: Kind,
    /// The problem.
    pub id: MessageId,
    /// How bad the problem is, which is either [`Severity::Error`] or [`Severity::Warn`] as the other severities are
    /// reported as one of them, or not at all.
    pub severity: Severity,
    /// The explanation as Git words it, without the name of `id`. It may contain parts of the object, like a name or a URL,
    /// which are bytes as nothing makes them UTF-8, and which Git prints without regard for control characters.
    pub message: BString,
}

impl fmt::Display for Report {
    /// Format like the text Git prints after `error: ` or `warning: `.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "object {}: {}: {}", self.oid, self.id, self.message)
    }
}

/// The reason why an object can't even be checked with the rules of Git's `fsck`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The object can't be parsed, which Git does before it checks an object during transfers. This applies even if the objects with
    /// the problem are [skipped](Options::skip()) or the message with the problem is configured to be [ignored](Severity::Ignore).
    ///
    /// Git reports [`detail()`](Refusal::detail()), if there is any, as an error and then `invalid commit` or `invalid tag`, which is the `Display` of this type.
    Unparsable {
        /// The object which couldn't be parsed.
        oid: ObjectId,
        /// Its kind, which is a commit or a tag.
        kind: Kind,
        /// What Git says about it, like `bogus commit object <oid>`.
        detail: Option<String>,
    },
    /// The links of the object can't be determined, even though checking the object itself passed, as can happen if some
    /// of the checks have been disabled. Git says `Not all child objects of <oid> are reachable` or `Error on reachable objects of <oid>`.
    BrokenLinks {
        /// The object whose links couldn't be followed.
        oid: ObjectId,
    },
    /// An object links to another object of an unexpected kind.
    Link(LinkError),
}

impl Refusal {
    /// The line Git prints as `error: <line>` before it gives up, if there is one.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Refusal::Unparsable { detail, .. } => detail.as_deref(),
            _ => None,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unparsable { kind, .. } => write!(f, "invalid {kind}"),
            Refusal::BrokenLinks { oid } => write!(f, "Not all child objects of {oid} are reachable"),
            Refusal::Link(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Checks the objects of a pack or a transfer as Git's `fsck` does for objects it receives, with a lazy check of the
/// links between them and of blobs that need a look at their contents.
///
/// Feed each object to [`check()`](Checker::check()) in any order and once all are known, call [`finish()`](Checker::finish()).
///
/// ### Memory
///
/// The checker remembers the kind of each object it has seen and each object that is linked to but not seen yet, which
/// is a few dozen bytes for each, like Git does.
#[derive(Debug)]
pub struct Checker {
    options: Options,
    hash_kind: gix_hash::Kind,
    /// Commits that have no parents as far as the links are concerned.
    shallow: HashSet,
    /// What was seen.
    seen: HashMap<ObjectId, Kind>,
    /// Objects that are linked to by what was seen, along with the kind they must have, but weren't seen yet.
    pending: HashMap<ObjectId, Kind>,
    blobs: blob::Found,
    /// Look at blobs only when the check is [finished](Checker::finish()).
    defer_blobs: bool,
    /// Don't require what is linked to in [`finish()`](Checker::finish()) to be there.
    without_links: bool,
}

impl Checker {
    /// Create a checker for objects of `hash_kind` which applies `options`.
    pub fn new(options: Options, hash_kind: gix_hash::Kind) -> Self {
        Checker {
            options,
            hash_kind,
            shallow: HashSet::default(),
            seen: HashMap::default(),
            pending: HashMap::default(),
            blobs: Default::default(),
            defer_blobs: false,
            without_links: false,
        }
    }

    /// Check the blobs that need a look at their contents only in [`finish()`](Checker::finish()), by reading them from there,
    /// even if they are passed to [`check()`](Checker::check()), which is what `git unpack-objects` does.
    pub fn with_deferred_blob_checks(mut self) -> Self {
        self.defer_blobs = true;
        self
    }

    /// The options the checker was created with.
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Don't look at what the objects link to when the check is [finished](Checker::finish()), which is what Git does when it
    /// receives objects from a remote that may leave objects out on purpose, and checks the objects one by one.
    pub fn without_links(mut self) -> Self {
        self.without_links = true;
        self
    }

    /// Treat the commits in `boundary` as if they had no parents when the links are checked, which is what Git does for
    /// the commits in its `shallow` file. Their `parent` lines still have to be valid.
    pub fn with_shallow(mut self, boundary: impl IntoIterator<Item = ObjectId>) -> Self {
        self.shallow.extend(boundary);
        self
    }

    /// Return `true` if the content of the blob `id` with `size` bytes must be passed to [`check()`](Checker::check()), as a tree that was
    /// seen already says it is a `.gitmodules` or `.gitattributes` file and it isn't too large to be loaded. For all other blobs, passing no data is fine
    /// as nothing is checked about them, and also for too-large `.gitattributes` which will be reported without it.
    ///
    /// If a tree that mentions the blob comes after it, it will be [checked in `finish()`](Checker::finish()).
    pub fn needs_blob_content(&self, id: &oid, size: u64) -> bool {
        !self.defer_blobs && self.blobs.needs_content(&self.options, id, size)
    }

    /// Check the object `id` of `kind` with `size` bytes whose content is `data`, and append everything noteworthy to `out`
    /// in the order Git prints it.
    ///
    /// `data` can be `None` only for a blob with no need for [its content](Checker::needs_blob_content()) or if it's too large to load,
    /// that is larger than [`Options::big_file_threshold()`]. Every other object needs data.
    ///
    /// Return `Ok(true)` if the check failed and Git would refuse the object. This is the return value of Git's `fsck_object()`,
    /// which isn't the same as there being an [error](Severity::Error) in `out`: a tag with an error in the `tagger` line and an extra header
    /// is accepted as Git 2.56's `fsck_tag_standalone()` overwrites the result of checking the line. A tree reports all of its problems,
    /// while a commit or tag stops at the first one that fails.
    ///
    /// Return `Err` if the object can't be parsed to check its links, see [`Refusal`].
    pub fn check(
        &mut self,
        id: &oid,
        kind: Kind,
        size: u64,
        data: Option<&[u8]>,
        out: &mut Vec<Diagnostic>,
    ) -> Result<bool, Refusal> {
        let id = id.to_owned();
        let strict = self.options.is_strict();
        let mut targets: Vec<(ObjectId, Kind)> = Vec::new();
        let failed = match kind {
            Kind::Blob if self.defer_blobs => false,
            Kind::Blob => self.blobs.check(&self.options, &id, size, data, out),
            Kind::Commit => {
                let data = data.unwrap_or_default();
                if strict {
                    let parsed = commit::parse(&id, data, self.hash_kind)?;
                    targets.push((parsed.tree, Kind::Tree));
                    if !self.shallow.contains(&id) {
                        targets.extend(parsed.parents.into_iter().map(|parent| (parent, Kind::Commit)));
                    }
                }
                commit::check(&mut Reporter::new(&self.options, &id, kind, out), data, self.hash_kind)
            }
            Kind::Tag => {
                let data = data.unwrap_or_default();
                if strict {
                    targets.push(tag::parse(&id, data, self.hash_kind)?);
                }
                tag::check(&mut Reporter::new(&self.options, &id, kind, out), data, self.hash_kind)
            }
            Kind::Tree => {
                let data = data.unwrap_or_default();
                let failed = tree::check(
                    &mut Reporter::new(&self.options, &id, kind, out),
                    data,
                    self.hash_kind,
                    &mut self.blobs,
                );
                if strict && !failed {
                    match tree::links(data, self.hash_kind) {
                        Some(links) => targets = links,
                        None => return Err(Refusal::BrokenLinks { oid: id }),
                    }
                }
                failed
            }
        };
        if strict && !failed {
            self.arrive(&id, kind)?;
            for (target, expected) in targets {
                self.link(target, expected)?;
            }
        }
        Ok(failed)
    }

    /// Complete the check once all objects have been passed to [`check()`](Checker::check()).
    ///
    /// First all objects that were linked to but haven't been seen are looked up in `find`, which is expected to see what the objects
    /// that were checked have been added to. They must exist and be of the expected kind, or the first problem is returned.
    /// Then the blobs `.gitmodules` and `.gitattributes` that couldn't be checked yet are read from `find` and checked.
    ///
    /// Return `Ok(true)` if the check of the blobs failed.
    pub fn finish<F>(&mut self, find: &F, out: &mut Vec<Diagnostic>) -> Result<bool, LinkError>
    where
        F: gix_object::Find + gix_object::FindHeader + ?Sized,
    {
        if self.options.is_strict() && !self.without_links {
            self.finish_links(find)?;
        }
        Ok(self.blobs.finish(&self.options, find, out))
    }
}

/// Create the [`Report`]s for the object `oid`, if they aren't suppressed, and tell if they fail the check.
pub(crate) struct Reporter<'a> {
    options: &'a Options,
    oid: &'a oid,
    kind: Kind,
    out: &'a mut Vec<Diagnostic>,
}

impl<'a> Reporter<'a> {
    fn new(options: &'a Options, oid: &'a oid, kind: Kind, out: &'a mut Vec<Diagnostic>) -> Self {
        Reporter {
            options,
            oid,
            kind,
            out,
        }
    }

    /// Return `true` if nothing is to be reported for the object.
    fn is_skipped(&self) -> bool {
        self.options.is_skipped(self.oid)
    }

    /// The severity to report `id` with, or `None` if nothing is to be reported for it.
    fn severity_of(&self, id: MessageId) -> Option<Severity> {
        if self.is_skipped() {
            return None;
        }
        match self.options.severity(id) {
            Severity::Ignore => None,
            Severity::Fatal | Severity::Error => Some(Severity::Error),
            Severity::Info | Severity::Warn => Some(Severity::Warn),
        }
    }

    fn push(&mut self, id: MessageId, severity: Severity, message: BString) -> bool {
        self.out.push(Diagnostic::Report(Report {
            oid: self.oid.to_owned(),
            kind: self.kind,
            id,
            severity,
            message,
        }));
        severity == Severity::Error
    }

    /// Report `id` with `message` unless it's configured to be ignored or the object is on the skip list.
    /// Return `true` if the report is an error, which is Git's non-zero return value of `report()`.
    fn report(&mut self, id: MessageId, message: fmt::Arguments<'_>) -> bool {
        match self.severity_of(id) {
            Some(severity) => self.push(id, severity, message.to_string().into()),
            None => false,
        }
    }

    /// Like [`report()`](Self::report()), but with `text` followed by `value`, which can be any bytes.
    fn report_value(&mut self, id: MessageId, text: &str, value: &BStr) -> bool {
        match self.severity_of(id) {
            Some(severity) => {
                let mut message = BString::from(text);
                message.extend_from_slice(value);
                self.push(id, severity, message)
            }
            None => false,
        }
    }

    fn note(&mut self, text: &str) {
        self.out.push(Diagnostic::Note(text.to_owned()));
    }
}
