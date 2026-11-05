use gix_fsck::{MessageId, Severity};

#[test]
fn all_ids_of_git_are_known() {
    assert_eq!(MessageId::ALL.len(), 76, "Git 2.56 has this many");
    let mut names: Vec<_> = MessageId::ALL.iter().map(|id| id.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 76, "all names are distinct");
}

#[test]
fn names_are_camel_case() {
    for (id, name) in [
        (MessageId::NulInHeader, "nulInHeader"),
        (MessageId::BadTreeSha1, "badTreeSha1"),
        (MessageId::NullSha1, "nullSha1"),
        (MessageId::NulInCommit, "nulInCommit"),
        (MessageId::GitmodulesUrl, "gitmodulesUrl"),
        (MessageId::BadGpgsig, "badGpgsig"),
        (MessageId::SymrefTargetIsNotARef, "symrefTargetIsNotARef"),
        (MessageId::ZeroPaddedFilemode, "zeroPaddedFilemode"),
    ] {
        assert_eq!(id.name(), name);
        assert_eq!(id.to_string(), name);
    }
}

#[test]
fn names_are_parsed_without_regard_for_case_but_underscores_are_not_removed() {
    for id in MessageId::ALL {
        assert_eq!(MessageId::from_name(id.name()), Some(id));
        assert_eq!(MessageId::from_name(&id.name().to_lowercase()), Some(id));
        assert_eq!(MessageId::from_name(&id.name().to_uppercase()), Some(id));
    }
    assert_eq!(MessageId::from_name("bad_timezone"), None);
    assert_eq!(MessageId::from_name("BAD_TIMEZONE"), None);
    assert_eq!(MessageId::from_name(""), None);
    assert_eq!(MessageId::from_name("badTimezone "), None);
}

#[test]
fn default_severities_are_those_of_git() {
    let count = |severity| {
        MessageId::ALL
            .iter()
            .filter(|id| id.default_severity() == severity)
            .count()
    };
    assert_eq!(
        [
            Severity::Fatal,
            Severity::Error,
            Severity::Warn,
            Severity::Info,
            Severity::Ignore
        ]
        .map(count),
        [2, 51, 10, 12, 1]
    );
    for (id, severity) in [
        (MessageId::NulInHeader, Severity::Fatal),
        (MessageId::UnterminatedHeader, Severity::Fatal),
        (MessageId::BadTimezone, Severity::Error),
        (MessageId::MissingTree, Severity::Error),
        (MessageId::TreeNotSorted, Severity::Error),
        (MessageId::GitmodulesUrl, Severity::Error),
        (MessageId::ZeroPaddedFilemode, Severity::Warn),
        (MessageId::HasDotgit, Severity::Warn),
        (MessageId::NullSha1, Severity::Warn),
        (MessageId::LargePathname, Severity::Warn),
        (MessageId::BadFilemode, Severity::Info),
        (MessageId::BadTagName, Severity::Info),
        (MessageId::MissingTaggerEntry, Severity::Info),
        (MessageId::GitmodulesParse, Severity::Info),
        (MessageId::ExtraHeaderEntry, Severity::Ignore),
    ] {
        assert_eq!(id.default_severity(), severity, "{id}");
    }
}
