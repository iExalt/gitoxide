use gix_fsck::{ConfigEntry, MessageId, Options, Severity};
use gix_hash::{Kind, ObjectId};

fn message(err: gix_error::Error) -> String {
    err.to_string()
}

#[test]
fn strict_promotes_warnings_to_errors_and_nothing_else() {
    let (strict, lenient) = (Options::strict(), Options::lenient());
    for id in MessageId::ALL {
        let default = id.default_severity();
        assert_eq!(lenient.severity(id), default, "{id}");
        let expected = if default == Severity::Warn {
            Severity::Error
        } else {
            default
        };
        assert_eq!(strict.severity(id), expected, "{id}");
    }
    assert!(strict.is_strict() && !lenient.is_strict());
}

#[test]
fn the_first_override_keeps_the_promotion_of_the_others() {
    let mut options = Options::strict();
    options.set(MessageId::BadFilemode, Severity::Error);
    assert_eq!(options.severity(MessageId::BadFilemode), Severity::Error);
    assert_eq!(
        options.severity(MessageId::ZeroPaddedFilemode),
        Severity::Error,
        "still promoted"
    );
    assert_eq!(options.severity(MessageId::ExtraHeaderEntry), Severity::Ignore);

    let mut options = Options::lenient();
    options.set_severity("zeroPaddedFilemode", "ignore").expect("valid");
    assert_eq!(options.severity(MessageId::ZeroPaddedFilemode), Severity::Ignore);
    assert_eq!(options.severity(MessageId::HasDotgit), Severity::Warn, "not promoted");
}

#[test]
fn set_severity_follows_git() {
    let mut options = Options::strict();
    for (value, expected) in [
        ("error", Severity::Error),
        ("warn", Severity::Warn),
        ("ignore", Severity::Ignore),
    ] {
        options.set_severity("badDate", value).expect("valid");
        assert_eq!(options.severity(MessageId::BadDate), expected);
    }
    options
        .set_severity("BADDATE", "warn")
        .expect("ids are case-insensitive");
    options
        .set_severity("nulInHeader", "error")
        .expect("fatal ids can be errors");

    assert_eq!(
        message(options.set_severity("bogus", "warn").expect_err("unknown")),
        "Unhandled message id: bogus"
    );
    assert_eq!(
        message(
            options
                .set_severity("badDate", "info")
                .expect_err("info can't be configured")
        ),
        "Unknown fsck message type: 'info'"
    );
    assert_eq!(
        message(
            options
                .set_severity("badDate", "Warn")
                .expect_err("values are case-sensitive")
        ),
        "Unknown fsck message type: 'Warn'"
    );
    assert_eq!(
        message(options.set_severity("nulInHeader", "warn").expect_err("fatal")),
        "Cannot demote nulInHeader to warn"
    );
    assert_eq!(
        message(options.set_severity("unterminatedHeader", "ignore").expect_err("fatal")),
        "Cannot demote unterminatedHeader to ignore"
    );
}

#[test]
fn the_limit_for_large_path_names_is_part_of_the_value() {
    let mut options = Options::strict();
    assert_eq!(options.max_tree_entry_len(), 4096);
    options.set_severity("largePathname", "warn:100").expect("valid");
    assert_eq!(options.severity(MessageId::LargePathname), Severity::Warn);
    assert_eq!(options.max_tree_entry_len(), 100);
    for (value, len) in [
        ("error:1k", 1024),
        ("warn:0x10", 16),
        ("warn:010", 8),
        ("warn:-1", -1),
        ("warn:1m", 1 << 20),
    ] {
        options.set_severity("largePathname", value).expect("valid");
        assert_eq!(options.max_tree_entry_len(), len, "{value}");
    }
    assert_eq!(
        message(
            options
                .set_severity("largePathname", "warn:x")
                .expect_err("not a number")
        ),
        "unable to parse max tree entry len: x"
    );
    assert_eq!(
        message(
            options
                .set_severity("badDate", "warn:1")
                .expect_err("only for largePathname")
        ),
        "Unknown fsck message type: 'warn:1'"
    );
}

#[test]
fn config_entries_are_validated_like_git_does_when_it_reads_them() -> gix_error::TestResult {
    assert_eq!(
        Options::parse_config_entry("badDate", "ignore")?,
        Some(ConfigEntry {
            id: MessageId::BadDate,
            severity: Severity::Ignore
        })
    );
    assert_eq!(
        Options::parse_config_entry("baddate", "warn")?.map(|e| e.id),
        Some(MessageId::BadDate)
    );
    assert_eq!(
        Options::parse_config_entry("bogus", "warn")?,
        None,
        "unknown ids are skipped with a warning"
    );
    assert_eq!(
        Options::parse_config_entry("bogus", "bogus")?,
        None,
        "and aren't looked at any further"
    );
    assert_eq!(
        message(Options::parse_config_entry("badDate", "bogus").expect_err("invalid value")),
        "Unknown fsck message type: 'bogus'"
    );
    assert_eq!(
        message(Options::parse_config_entry("largePathname", "warn:100").expect_err("no limit in configuration")),
        "Unknown fsck message type: 'warn:100'"
    );
    assert!(
        Options::parse_config_entry("nulInHeader", "warn")?.is_some(),
        "demoting fatal ids is only found when the severity is applied"
    );
    Ok(())
}

#[test]
fn skip_lists_are_parsed_like_git_does() -> gix_error::TestResult {
    let (a, b) = (
        ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")?,
        ObjectId::from_hex(b"89abcdef0123456789abcdef0123456789abcdef")?,
    );
    let mut options = Options::strict();
    options.skip_list_from_bytes(
        format!(
            "# a comment\n\n   {a}  \n\t{b} # trailing comment\r\n   # indented comment\n{}\n",
            "ABCDEF0123456789ABCDEF0123456789ABCDEF01"
        )
        .as_bytes(),
        Kind::Sha1,
    )?;
    assert!(options.is_skipped(&a) && options.is_skipped(&b));
    assert!(
        options.is_skipped(&ObjectId::from_hex(b"abcdef0123456789abcdef0123456789abcdef01")?),
        "hex is case-insensitive"
    );
    assert!(!options.is_skipped(&ObjectId::null(Kind::Sha1)));

    let mut options = Options::strict();
    for bad in [
        "xyz\n",
        "0123456789abcdef\n",
        "0123456789abcdef0123456789abcdef01234567 x\n",
    ] {
        let err = options
            .skip_list_from_bytes(bad.as_bytes(), Kind::Sha1)
            .expect_err("invalid");
        assert_eq!(message(err), format!("invalid object name: {}", bad.trim_end()));
    }
    let err = options
        .skip_list_from_bytes(format!("{a}\nnope\n").as_bytes(), Kind::Sha1)
        .expect_err("invalid");
    assert_eq!(message(err), "invalid object name: nope");
    assert!(!options.is_skipped(&a), "nothing is added if there is an error");
    Ok(())
}

#[cfg(feature = "sha256")]
#[test]
fn skip_lists_have_the_width_of_the_hash() -> gix_error::TestResult {
    let sha256 = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    let mut options = Options::strict();
    options.skip_list_from_bytes(sha256.as_bytes(), Kind::Sha256)?;
    assert!(options.is_skipped(&ObjectId::from_hex(sha256.as_bytes())?));
    assert!(options.skip_list_from_bytes(sha256.as_bytes(), Kind::Sha1).is_err());
    assert!(
        options
            .skip_list_from_bytes(&sha256.as_bytes()[..40], Kind::Sha256)
            .is_err()
    );
    Ok(())
}
