use gix_error::{ErrorExt, Result, bail, validation};
use gix_hash::ObjectId;
use gix_hashtable::HashSet;

use crate::{MessageId, Severity};

/// What to look for when checking objects, and how to treat what is found.
///
/// Use [`strict()`](Options::strict()) to check like `index-pack --strict` and `unpack-objects --strict` do, which
/// is what `receive-pack` and `fetch` use for `receive.fsckObjects` and `fetch.fsckObjects`.
#[derive(Debug, Clone)]
pub struct Options {
    strict: bool,
    /// The severity of each message after the first override was made, so the promotion of warnings in strict mode is
    /// part of it. `None` until then, to apply the defaults.
    severities: Option<Box<[Severity; MessageId::ALL.len()]>>,
    skip: HashSet,
    max_tree_entry_len: isize,
    big_file_threshold: u64,
}

/// An entry of the configuration like `receive.fsck.<msg-id>`, as understood by [`Options::parse_config_entry()`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigEntry {
    /// The message to configure.
    pub id: MessageId,
    /// The severity to give it.
    pub severity: Severity,
}

impl Options {
    /// Options to check as `git index-pack --strict` and `git unpack-objects --strict` do, which make errors from
    /// all warnings, as opposed to the [lenient](Options::lenient()) check of `git fsck`.
    pub fn strict() -> Self {
        Self::new(true)
    }

    /// Options to check as `git fsck` does by default, which leaves warnings as they are.
    pub fn lenient() -> Self {
        Self::new(false)
    }

    fn new(strict: bool) -> Self {
        Options {
            strict,
            severities: None,
            skip: HashSet::default(),
            max_tree_entry_len: 4096,
            big_file_threshold: 512 * 1024 * 1024,
        }
    }

    /// Return `true` if warnings are promoted to errors, unless they were configured explicitly.
    pub fn is_strict(&self) -> bool {
        self.strict
    }

    /// The names longer than this are reported as [`MessageId::LargePathname`], which is `4096` unless it was configured
    /// with [`set_severity()`](Options::set_severity()).
    pub fn max_tree_entry_len(&self) -> isize {
        self.max_tree_entry_len
    }

    /// The size in bytes from which a blob isn't loaded into memory to be checked, which is what `core.bigFileThreshold` configures
    /// and `512MiB` by default. A `.gitmodules` file that large is reported as [`MessageId::GitmodulesLarge`], while
    /// a `.gitattributes` file is limited to `100MiB`.
    pub fn big_file_threshold(&self) -> u64 {
        self.big_file_threshold
    }

    /// Set the [`big_file_threshold()`](Options::big_file_threshold()).
    pub fn set_big_file_threshold(&mut self, bytes: u64) -> &mut Self {
        self.big_file_threshold = bytes;
        self
    }

    /// The severity of `id` with all overrides, and the promotion of warnings in [strict](Options::strict()) mode, applied.
    pub fn severity(&self, id: MessageId) -> Severity {
        match &self.severities {
            Some(severities) => severities[id as usize],
            None => self.default_severity(id),
        }
    }

    fn default_severity(&self, id: MessageId) -> Severity {
        match id.default_severity() {
            Severity::Warn if self.strict => Severity::Error,
            other => other,
        }
    }

    /// Configure the `severity` of `id` just like `git index-pack --strict=<id>=<severity>` does.
    ///
    /// The first call bakes in the promotion of warnings, so configuring one message doesn't affect how the others are treated.
    /// Note that [`Severity::Fatal`] and [`Severity::Info`] aren't allowed, nor is a severity for a fatal message that is not
    /// [`Severity::Error`].
    pub fn set(&mut self, id: MessageId, severity: Severity) {
        if self.severities.is_none() {
            let mut severities = Box::new([Severity::Ignore; MessageId::ALL.len()]);
            for known in MessageId::ALL {
                severities[known as usize] = self.default_severity(known);
            }
            self.severities = Some(severities);
        }
        if let Some(severities) = self.severities.as_mut() {
            severities[id as usize] = severity;
        }
    }

    /// Configure the severity of the message named `id` with `value`, which is `error`, `warn` or `ignore`, just like
    /// `git index-pack --strict=<id>=<value>` does. The value for `largePathname` may be followed by a colon and the
    /// longest name that is allowed, like `warn:100`, which is what [`max_tree_entry_len()`](Options::max_tree_entry_len()) returns.
    ///
    /// Fail if the id is unknown, if the value is not one of the three, if the colon is followed by something that isn't a number,
    /// or if the message is fatal and `value` isn't `error`.
    pub fn set_severity(&mut self, id: &str, value: &str) -> Result {
        let Some(message_id) = MessageId::from_name(id) else {
            bail!(validation(format!("Unhandled message id: {id}")));
        };
        let mut severity_value = value;
        if message_id == MessageId::LargePathname
            && let Some((severity, len)) = value.split_once(':')
        {
            severity_value = severity;
            self.max_tree_entry_len = parse_ssize(len)?;
        }
        let severity = parse_severity(severity_value)?;
        if severity != Severity::Error && message_id.default_severity() == Severity::Fatal {
            bail!(validation(format!("Cannot demote {id} to {value}")));
        }
        self.set(message_id, severity);
        Ok(())
    }

    /// Validate the entry `receive.fsck.<id>=<value>` or `fetch.fsck.<id>=<value>` of the configuration like Git does,
    /// and return what it asks for, or `None` if Git would skip it as it doesn't know `id` (in which case it warns that
    /// the message id is unknown).
    ///
    /// Fail if `value` isn't `error`, `warn` or `ignore`. Unlike [`set_severity()`](Options::set_severity()), the value for
    /// `largePathname` can't carry a length, and demoting a fatal message isn't noticed until the entry is applied with it.
    pub fn parse_config_entry(id: &str, value: &str) -> Result<Option<ConfigEntry>> {
        let Some(message_id) = MessageId::from_name(id) else {
            return Ok(None);
        };
        let severity = parse_severity(value)?;
        Ok(Some(ConfigEntry {
            id: message_id,
            severity,
        }))
    }

    /// Never report anything about the objects in the `skip` list, whatever the message.
    ///
    /// Use [`skip_list_from_bytes()`](Options::skip_list_from_bytes()) to read them from a file like `receive.fsck.skipList` names.
    pub fn skip(&mut self, id: ObjectId) -> &mut Self {
        self.skip.insert(id);
        self
    }

    /// Return `true` if nothing is to be reported about the object `id`.
    pub fn is_skipped(&self, id: &gix_hash::oid) -> bool {
        !self.skip.is_empty() && self.skip.contains(id)
    }

    /// Add the objects listed in `data` to those [skipped](Options::skip()), which is `hash_kind`-wide hex ids, one per line.
    /// Parsing is the same as Git's, so `#` starts a comment, whitespace around the id is ignored just as empty lines are.
    ///
    /// Fail if a line has something else, in which case nothing was added.
    pub fn skip_list_from_bytes(&mut self, data: &[u8], hash_kind: gix_hash::Kind) -> Result {
        let mut ids = Vec::new();
        for line in data.split(|b| *b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = line.split(|b| *b == b'#').next().unwrap_or_default();
            let line = line.trim_ascii();
            if line.is_empty() {
                continue;
            }
            match ObjectId::from_hex(line) {
                Ok(id) if id.kind() == hash_kind => ids.push(id),
                _ => bail!(validation(format!(
                    "invalid object name: {}",
                    String::from_utf8_lossy(line)
                ))),
            }
        }
        self.skip.extend(ids);
        Ok(())
    }
}

fn parse_severity(value: &str) -> Result<Severity> {
    Ok(match value {
        "error" => Severity::Error,
        "warn" => Severity::Warn,
        "ignore" => Severity::Ignore,
        other => bail!(validation(format!("Unknown fsck message type: '{other}'"))),
    })
}

/// Parse a number like `git_parse_ssize_t()` does, which accepts the suffixes `k`, `m` and `g` and any base `strtoimax()` does.
/// A negative value is smaller than any name, so every name is too long.
fn parse_ssize(value: &str) -> Result<isize> {
    let bad = || validation(format!("unable to parse max tree entry len: {value}")).raise();
    let trimmed = value.trim_start();
    let (negative, unsigned) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let lower = unsigned.to_ascii_lowercase();
    let (digits, factor) = match lower.as_bytes().last() {
        Some(b'k') => (&lower[..lower.len() - 1], 1024_u64),
        Some(b'm') => (&lower[..lower.len() - 1], 1024 * 1024),
        Some(b'g') => (&lower[..lower.len() - 1], 1024 * 1024 * 1024),
        _ => (lower.as_str(), 1),
    };
    let number = if let Some(hex) = digits.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else if digits.len() > 1 && digits.starts_with('0') {
        u64::from_str_radix(&digits[1..], 8)
    } else {
        digits.parse()
    }
    .map_err(|_| bad())?;
    let number = number
        .checked_mul(factor)
        .and_then(|n| isize::try_from(n).ok())
        .ok_or_else(bad)?;
    Ok(if negative { -number } else { number })
}
