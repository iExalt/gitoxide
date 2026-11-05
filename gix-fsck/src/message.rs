//! The identifiers and severities of the problems [`Checker`](crate::Checker) can report.
//!
//! The list mirrors `fsck.h` of Git 2.56, so that configuration written for Git has the same meaning here.

/// How a problem is treated when it is found.
///
/// These are the severities Git uses internally. [`Fatal`](Severity::Fatal) can only be configured as
/// [`Error`](Severity::Error), which is no change, and [`Info`](Severity::Info) can't be configured at all
/// as Git only accepts `error`, `warn` and `ignore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// The problem isn't reported.
    Ignore,
    /// The problem is reported as a warning and doesn't fail the check, not even in [strict](crate::Options::strict()) mode.
    Info,
    /// The problem is an error that can't be demoted, and checking of the object stops there.
    Fatal,
    /// The problem is reported as an error and fails the check.
    Error,
    /// The problem is reported as a warning and doesn't fail the check, unless the check is [strict](crate::Options::strict()).
    Warn,
}

/// The identifier of a problem that can be found in an object, as listed in Git's `fsck-msgids` documentation.
///
/// Each has a [default severity](MessageId::default_severity()) and a name in camel case that is used in messages and
/// configuration, see [`MessageId::name()`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MessageId {
    /// NUL byte exists in the object header.
    NulInHeader,
    /// Missing end-of-line in the object header.
    UnterminatedHeader,
    /// A continuation header (such as for `gpgsig`) is unexpectedly truncated.
    BadHeaderContinuation,
    /// Invalid date format in an author/committer line.
    BadDate,
    /// Invalid date value in an author/committer line.
    BadDateOverflow,
    /// Invalid email format in an author/committer line.
    BadEmail,
    /// A tag contains a bad (truncated) signature (e.g., `gpgsig`) header.
    BadGpgsig,
    /// The `HEAD` ref is a symref that does not refer to a branch.
    BadHeadTarget,
    /// An author/committer name is empty.
    BadName,
    /// An object has a bad sha1.
    BadObjectSha1,
    /// The "packed-refs" file contains an invalid entry.
    BadPackedRefEntry,
    /// The "packed-refs" file contains an invalid header.
    BadPackedRefHeader,
    /// A commit object has a bad parent sha1.
    BadParentSha1,
    /// The referent name of a symref is invalid.
    BadReferentName,
    /// A ref has bad content.
    BadRefContent,
    /// A ref has a bad file type.
    BadRefFiletype,
    /// A ref has an invalid format.
    BadRefName,
    /// A ref points to an invalid object ID.
    BadRefOid,
    /// Found an invalid time zone in an author/committer line.
    BadTimezone,
    /// A tree cannot be parsed.
    BadTree,
    /// A tree has an invalid format.
    BadTreeSha1,
    /// Found an invalid object type.
    BadType,
    /// A tree contains duplicate file entries.
    DuplicateEntries,
    /// A non-blob found at `.gitattributes`.
    GitattributesBlob,
    /// The `.gitattributes` blob is too large.
    GitattributesLarge,
    /// The `.gitattributes` blob contains too long lines.
    GitattributesLineLength,
    /// Unable to read `.gitattributes` blob.
    GitattributesMissing,
    /// A non-blob found at `.gitmodules`.
    GitmodulesBlob,
    /// The `.gitmodules` file is too large to parse.
    GitmodulesLarge,
    /// Unable to read `.gitmodules` blob.
    GitmodulesMissing,
    /// A submodule name is invalid.
    GitmodulesName,
    /// `.gitmodules` path is invalid.
    GitmodulesPath,
    /// `.gitmodules` is a symlink.
    GitmodulesSymlink,
    /// Found an invalid submodule update setting.
    GitmodulesUpdate,
    /// Found an invalid submodule url.
    GitmodulesUrl,
    /// Author is missing.
    MissingAuthor,
    /// Committer is missing.
    MissingCommitter,
    /// Email is missing in an author/committer line.
    MissingEmail,
    /// Missing name before an email in an author/committer line.
    MissingNameBeforeEmail,
    /// Missing `object` line in tag object.
    MissingObject,
    /// Missing space before date in an author/committer line.
    MissingSpaceBeforeDate,
    /// Missing space before the email in an author/committer line.
    MissingSpaceBeforeEmail,
    /// Unexpected end after `type` line in a tag object.
    MissingTag,
    /// Missing `tag` line in a tag object.
    MissingTagEntry,
    /// Missing `tree` line in a commit object.
    MissingTree,
    /// Invalid type value on the `type` line in a tag object.
    MissingType,
    /// Missing `type` line in a tag object.
    MissingTypeEntry,
    /// Multiple author lines found in a commit.
    MultipleAuthors,
    /// The "packed-refs" file contains an entry that is not terminated by a newline.
    PackedRefEntryNotTerminated,
    /// The "packed-refs" file is not sorted.
    PackedRefUnsorted,
    /// A tree is not properly sorted.
    TreeNotSorted,
    /// Found an unknown object type.
    UnknownType,
    /// Found a zero padded date in an author/committer line.
    ZeroPaddedDate,
    /// A reftable table has an invalid name.
    BadReftableTableName,
    /// A path contains an empty name.
    EmptyName,
    /// A path contains the full path starting with "/".
    FullPathname,
    /// A tree contains an entry named `.`.
    HasDot,
    /// A tree contains an entry named `..`.
    HasDotdot,
    /// A tree contains an entry named `.git`.
    HasDotgit,
    /// A tree contains an entry with a very long path name. If the value of `fsck.largePathname` contains a colon, that value is used as the maximum allowable length (e.g., "warn:10" would complain about any path component of 11 or more bytes). The default value is 4096.
    LargePathname,
    /// Tree contains entries pointing to a null sha1.
    NullSha1,
    /// Found a NUL byte in the commit object body.
    NulInCommit,
    /// Found a zero padded filemode in a tree.
    ZeroPaddedFilemode,
    /// A tree contains a bad filemode entry.
    BadFilemode,
    /// A tag has an invalid format.
    BadTagName,
    /// "packed-refs" file is empty. Report to the git@vger.kernel.org mailing list if you see this error. As only very early versions of Git would create such an empty "packed_refs" file, we might tighten this rule in the future.
    EmptyPackedRefsFile,
    /// `.gitattributes` is a symlink.
    GitattributesSymlink,
    /// `.gitignore` is a symlink.
    GitignoreSymlink,
    /// Could not parse `.gitmodules` blob.
    GitmodulesParse,
    /// `.mailmap` is a symlink.
    MailmapSymlink,
    /// Missing `tagger` line in a tag object.
    MissingTaggerEntry,
    /// A loose ref that does not end with newline(LF). As valid implementations of Git never created such a loose ref file, it may become an error in the future. Report to the git@vger.kernel.org mailing list if you see this error, as we need to know what tools created such a file.
    RefMissingNewline,
    /// A symbolic link is used as a symref. Report to the git@vger.kernel.org mailing list if you see this error, as we are assessing the feasibility of dropping the support to drop creating symbolic links as symrefs.
    SymlinkRef,
    /// The target of a symbolic reference points neither to a root reference nor to a reference starting with "refs/". Although we allow create a symref pointing to the referent which is outside the "ref" by using `git symbolic-ref`, we may tighten the rule in the future. Report to the git@vger.kernel.org mailing list if you see this error, as we need to know what tools created such a file.
    SymrefTargetIsNotARef,
    /// A loose ref has trailing content. As valid implementations of Git never created such a loose ref file, it may become an error in the future. Report to the git@vger.kernel.org mailing list if you see this error, as we need to know what tools created such a file.
    TrailingRefContent,
    /// Extra headers found after `tagger`.
    ExtraHeaderEntry,
}

impl MessageId {
    /// All message ids in the order Git lists them: fatal errors, errors, warnings, infos and then ignored ones.
    pub const ALL: [MessageId; 76] = [
        MessageId::NulInHeader,
        MessageId::UnterminatedHeader,
        MessageId::BadHeaderContinuation,
        MessageId::BadDate,
        MessageId::BadDateOverflow,
        MessageId::BadEmail,
        MessageId::BadGpgsig,
        MessageId::BadHeadTarget,
        MessageId::BadName,
        MessageId::BadObjectSha1,
        MessageId::BadPackedRefEntry,
        MessageId::BadPackedRefHeader,
        MessageId::BadParentSha1,
        MessageId::BadReferentName,
        MessageId::BadRefContent,
        MessageId::BadRefFiletype,
        MessageId::BadRefName,
        MessageId::BadRefOid,
        MessageId::BadTimezone,
        MessageId::BadTree,
        MessageId::BadTreeSha1,
        MessageId::BadType,
        MessageId::DuplicateEntries,
        MessageId::GitattributesBlob,
        MessageId::GitattributesLarge,
        MessageId::GitattributesLineLength,
        MessageId::GitattributesMissing,
        MessageId::GitmodulesBlob,
        MessageId::GitmodulesLarge,
        MessageId::GitmodulesMissing,
        MessageId::GitmodulesName,
        MessageId::GitmodulesPath,
        MessageId::GitmodulesSymlink,
        MessageId::GitmodulesUpdate,
        MessageId::GitmodulesUrl,
        MessageId::MissingAuthor,
        MessageId::MissingCommitter,
        MessageId::MissingEmail,
        MessageId::MissingNameBeforeEmail,
        MessageId::MissingObject,
        MessageId::MissingSpaceBeforeDate,
        MessageId::MissingSpaceBeforeEmail,
        MessageId::MissingTag,
        MessageId::MissingTagEntry,
        MessageId::MissingTree,
        MessageId::MissingType,
        MessageId::MissingTypeEntry,
        MessageId::MultipleAuthors,
        MessageId::PackedRefEntryNotTerminated,
        MessageId::PackedRefUnsorted,
        MessageId::TreeNotSorted,
        MessageId::UnknownType,
        MessageId::ZeroPaddedDate,
        MessageId::BadReftableTableName,
        MessageId::EmptyName,
        MessageId::FullPathname,
        MessageId::HasDot,
        MessageId::HasDotdot,
        MessageId::HasDotgit,
        MessageId::LargePathname,
        MessageId::NullSha1,
        MessageId::NulInCommit,
        MessageId::ZeroPaddedFilemode,
        MessageId::BadFilemode,
        MessageId::BadTagName,
        MessageId::EmptyPackedRefsFile,
        MessageId::GitattributesSymlink,
        MessageId::GitignoreSymlink,
        MessageId::GitmodulesParse,
        MessageId::MailmapSymlink,
        MessageId::MissingTaggerEntry,
        MessageId::RefMissingNewline,
        MessageId::SymlinkRef,
        MessageId::SymrefTargetIsNotARef,
        MessageId::TrailingRefContent,
        MessageId::ExtraHeaderEntry,
    ];

    /// The name in camel case as used in messages and configuration, like `badTimezone`.
    pub fn name(self) -> &'static str {
        match self {
            MessageId::NulInHeader => "nulInHeader",
            MessageId::UnterminatedHeader => "unterminatedHeader",
            MessageId::BadHeaderContinuation => "badHeaderContinuation",
            MessageId::BadDate => "badDate",
            MessageId::BadDateOverflow => "badDateOverflow",
            MessageId::BadEmail => "badEmail",
            MessageId::BadGpgsig => "badGpgsig",
            MessageId::BadHeadTarget => "badHeadTarget",
            MessageId::BadName => "badName",
            MessageId::BadObjectSha1 => "badObjectSha1",
            MessageId::BadPackedRefEntry => "badPackedRefEntry",
            MessageId::BadPackedRefHeader => "badPackedRefHeader",
            MessageId::BadParentSha1 => "badParentSha1",
            MessageId::BadReferentName => "badReferentName",
            MessageId::BadRefContent => "badRefContent",
            MessageId::BadRefFiletype => "badRefFiletype",
            MessageId::BadRefName => "badRefName",
            MessageId::BadRefOid => "badRefOid",
            MessageId::BadTimezone => "badTimezone",
            MessageId::BadTree => "badTree",
            MessageId::BadTreeSha1 => "badTreeSha1",
            MessageId::BadType => "badType",
            MessageId::DuplicateEntries => "duplicateEntries",
            MessageId::GitattributesBlob => "gitattributesBlob",
            MessageId::GitattributesLarge => "gitattributesLarge",
            MessageId::GitattributesLineLength => "gitattributesLineLength",
            MessageId::GitattributesMissing => "gitattributesMissing",
            MessageId::GitmodulesBlob => "gitmodulesBlob",
            MessageId::GitmodulesLarge => "gitmodulesLarge",
            MessageId::GitmodulesMissing => "gitmodulesMissing",
            MessageId::GitmodulesName => "gitmodulesName",
            MessageId::GitmodulesPath => "gitmodulesPath",
            MessageId::GitmodulesSymlink => "gitmodulesSymlink",
            MessageId::GitmodulesUpdate => "gitmodulesUpdate",
            MessageId::GitmodulesUrl => "gitmodulesUrl",
            MessageId::MissingAuthor => "missingAuthor",
            MessageId::MissingCommitter => "missingCommitter",
            MessageId::MissingEmail => "missingEmail",
            MessageId::MissingNameBeforeEmail => "missingNameBeforeEmail",
            MessageId::MissingObject => "missingObject",
            MessageId::MissingSpaceBeforeDate => "missingSpaceBeforeDate",
            MessageId::MissingSpaceBeforeEmail => "missingSpaceBeforeEmail",
            MessageId::MissingTag => "missingTag",
            MessageId::MissingTagEntry => "missingTagEntry",
            MessageId::MissingTree => "missingTree",
            MessageId::MissingType => "missingType",
            MessageId::MissingTypeEntry => "missingTypeEntry",
            MessageId::MultipleAuthors => "multipleAuthors",
            MessageId::PackedRefEntryNotTerminated => "packedRefEntryNotTerminated",
            MessageId::PackedRefUnsorted => "packedRefUnsorted",
            MessageId::TreeNotSorted => "treeNotSorted",
            MessageId::UnknownType => "unknownType",
            MessageId::ZeroPaddedDate => "zeroPaddedDate",
            MessageId::BadReftableTableName => "badReftableTableName",
            MessageId::EmptyName => "emptyName",
            MessageId::FullPathname => "fullPathname",
            MessageId::HasDot => "hasDot",
            MessageId::HasDotdot => "hasDotdot",
            MessageId::HasDotgit => "hasDotgit",
            MessageId::LargePathname => "largePathname",
            MessageId::NullSha1 => "nullSha1",
            MessageId::NulInCommit => "nulInCommit",
            MessageId::ZeroPaddedFilemode => "zeroPaddedFilemode",
            MessageId::BadFilemode => "badFilemode",
            MessageId::BadTagName => "badTagName",
            MessageId::EmptyPackedRefsFile => "emptyPackedRefsFile",
            MessageId::GitattributesSymlink => "gitattributesSymlink",
            MessageId::GitignoreSymlink => "gitignoreSymlink",
            MessageId::GitmodulesParse => "gitmodulesParse",
            MessageId::MailmapSymlink => "mailmapSymlink",
            MessageId::MissingTaggerEntry => "missingTaggerEntry",
            MessageId::RefMissingNewline => "refMissingNewline",
            MessageId::SymlinkRef => "symlinkRef",
            MessageId::SymrefTargetIsNotARef => "symrefTargetIsNotARef",
            MessageId::TrailingRefContent => "trailingRefContent",
            MessageId::ExtraHeaderEntry => "extraHeaderEntry",
        }
    }

    /// The severity this message has unless it is configured otherwise, without the promotion of warnings to errors
    /// that [strict](crate::Options::strict()) checking applies.
    pub fn default_severity(self) -> Severity {
        match self {
            MessageId::NulInHeader => Severity::Fatal,
            MessageId::UnterminatedHeader => Severity::Fatal,
            MessageId::BadHeaderContinuation => Severity::Error,
            MessageId::BadDate => Severity::Error,
            MessageId::BadDateOverflow => Severity::Error,
            MessageId::BadEmail => Severity::Error,
            MessageId::BadGpgsig => Severity::Error,
            MessageId::BadHeadTarget => Severity::Error,
            MessageId::BadName => Severity::Error,
            MessageId::BadObjectSha1 => Severity::Error,
            MessageId::BadPackedRefEntry => Severity::Error,
            MessageId::BadPackedRefHeader => Severity::Error,
            MessageId::BadParentSha1 => Severity::Error,
            MessageId::BadReferentName => Severity::Error,
            MessageId::BadRefContent => Severity::Error,
            MessageId::BadRefFiletype => Severity::Error,
            MessageId::BadRefName => Severity::Error,
            MessageId::BadRefOid => Severity::Error,
            MessageId::BadTimezone => Severity::Error,
            MessageId::BadTree => Severity::Error,
            MessageId::BadTreeSha1 => Severity::Error,
            MessageId::BadType => Severity::Error,
            MessageId::DuplicateEntries => Severity::Error,
            MessageId::GitattributesBlob => Severity::Error,
            MessageId::GitattributesLarge => Severity::Error,
            MessageId::GitattributesLineLength => Severity::Error,
            MessageId::GitattributesMissing => Severity::Error,
            MessageId::GitmodulesBlob => Severity::Error,
            MessageId::GitmodulesLarge => Severity::Error,
            MessageId::GitmodulesMissing => Severity::Error,
            MessageId::GitmodulesName => Severity::Error,
            MessageId::GitmodulesPath => Severity::Error,
            MessageId::GitmodulesSymlink => Severity::Error,
            MessageId::GitmodulesUpdate => Severity::Error,
            MessageId::GitmodulesUrl => Severity::Error,
            MessageId::MissingAuthor => Severity::Error,
            MessageId::MissingCommitter => Severity::Error,
            MessageId::MissingEmail => Severity::Error,
            MessageId::MissingNameBeforeEmail => Severity::Error,
            MessageId::MissingObject => Severity::Error,
            MessageId::MissingSpaceBeforeDate => Severity::Error,
            MessageId::MissingSpaceBeforeEmail => Severity::Error,
            MessageId::MissingTag => Severity::Error,
            MessageId::MissingTagEntry => Severity::Error,
            MessageId::MissingTree => Severity::Error,
            MessageId::MissingType => Severity::Error,
            MessageId::MissingTypeEntry => Severity::Error,
            MessageId::MultipleAuthors => Severity::Error,
            MessageId::PackedRefEntryNotTerminated => Severity::Error,
            MessageId::PackedRefUnsorted => Severity::Error,
            MessageId::TreeNotSorted => Severity::Error,
            MessageId::UnknownType => Severity::Error,
            MessageId::ZeroPaddedDate => Severity::Error,
            MessageId::BadReftableTableName => Severity::Warn,
            MessageId::EmptyName => Severity::Warn,
            MessageId::FullPathname => Severity::Warn,
            MessageId::HasDot => Severity::Warn,
            MessageId::HasDotdot => Severity::Warn,
            MessageId::HasDotgit => Severity::Warn,
            MessageId::LargePathname => Severity::Warn,
            MessageId::NullSha1 => Severity::Warn,
            MessageId::NulInCommit => Severity::Warn,
            MessageId::ZeroPaddedFilemode => Severity::Warn,
            MessageId::BadFilemode => Severity::Info,
            MessageId::BadTagName => Severity::Info,
            MessageId::EmptyPackedRefsFile => Severity::Info,
            MessageId::GitattributesSymlink => Severity::Info,
            MessageId::GitignoreSymlink => Severity::Info,
            MessageId::GitmodulesParse => Severity::Info,
            MessageId::MailmapSymlink => Severity::Info,
            MessageId::MissingTaggerEntry => Severity::Info,
            MessageId::RefMissingNewline => Severity::Info,
            MessageId::SymlinkRef => Severity::Info,
            MessageId::SymrefTargetIsNotARef => Severity::Info,
            MessageId::TrailingRefContent => Severity::Info,
            MessageId::ExtraHeaderEntry => Severity::Ignore,
        }
    }
}

impl MessageId {
    /// Parse the name of a message like Git does, which ignores the case of `name` and compares it to the camel-case
    /// [`name()`](MessageId::name()) with its case ignored. Underscores aren't removed from `name`.
    ///
    /// That makes `badTimezone` and `badtimezone` the same, while `bad_timezone` isn't known.
    pub fn from_name(name: &str) -> Option<MessageId> {
        MessageId::ALL
            .into_iter()
            .find(|id| id.name().eq_ignore_ascii_case(name))
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
