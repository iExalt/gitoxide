use bstr::{BString, ByteSlice};
use gix_error::{Result, bail, validation};

use crate::push::StatusFormat;

/// What a receiving side announces to clients, which is what `git receive-pack` announces in the same order.
///
/// `delete-refs`, `report-status`, `report-status-v2`, `side-band-64k` and `quiet` are always offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Offer `atomic`, which is controlled by `receive.advertiseAtomic` and on by default.
    pub atomic: bool,
    /// Offer `ofs-delta`, which is controlled by `repack.useDeltaBaseOffset` and on by default.
    pub ofs_delta: bool,
    /// The nonce to offer as `push-cert=<nonce>`, if signed pushes are enabled.
    ///
    /// Note that signed pushes aren't supported by [`Parser`](super::Parser) yet, so this should stay `None`.
    pub push_cert_nonce: Option<String>,
    /// Offer `push-options`, which is controlled by `receive.advertisePushOptions` and off by default.
    pub push_options: bool,
    /// The identifier to offer as `session-id=<id>`, which is controlled by `transfer.advertiseSID`.
    pub session_id: Option<String>,
    /// The hash of the repository, announced as `object-format`.
    pub object_format: gix_hash::Kind,
    /// The user agent to announce as `agent=<agent>`, which looks like `git/2.56.0-gitoxide`.
    pub agent: String,
}

impl Capabilities {
    /// Create capabilities for a repository using `object_format` which are announced by `agent`, and all that Git
    /// offers by default.
    pub fn new(object_format: gix_hash::Kind, agent: impl Into<String>) -> Self {
        Capabilities {
            atomic: true,
            ofs_delta: true,
            push_cert_nonce: None,
            push_options: false,
            session_id: None,
            object_format,
            agent: agent.into(),
        }
    }

    /// Return the capabilities as they are written after the first reference, separated by a single space.
    ///
    /// Fail if a value can't be represented, which is the case if it is empty or contains whitespace or a NUL byte.
    pub fn to_line(&self) -> Result<String> {
        fn check(name: &str, value: &str) -> Result {
            if value.is_empty() || value.contains(|c: char| c.is_whitespace() || c == '\0') {
                bail!(validation(format!(
                    "The value {value:?} of capability '{name}' must be non-empty and contain no whitespace or NUL"
                )));
            }
            Ok(())
        }
        let mut out = String::from("report-status report-status-v2 delete-refs side-band-64k quiet");
        if self.atomic {
            out.push_str(" atomic");
        }
        if self.ofs_delta {
            out.push_str(" ofs-delta");
        }
        if let Some(nonce) = &self.push_cert_nonce {
            check("push-cert", nonce)?;
            out.push_str(" push-cert=");
            out.push_str(nonce);
        }
        if self.push_options {
            out.push_str(" push-options");
        }
        if let Some(id) = &self.session_id {
            check("session-id", id)?;
            out.push_str(" session-id=");
            out.push_str(id);
        }
        out.push_str(" object-format=");
        out.push_str(&self.object_format.to_string());
        check("agent", &self.agent)?;
        out.push_str(" agent=");
        out.push_str(&self.agent);
        Ok(out)
    }
}

/// What a client asked for in the feature list that follows its first command.
///
/// Features that weren't offered are ignored, as `receive-pack` does, with the exception of the object format which
/// has to match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features {
    /// The client wants a status report as with [`StatusFormat::V1`].
    pub report_status: bool,
    /// The client wants a status report as with [`StatusFormat::V2`].
    pub report_status_v2: bool,
    /// The client wants the response multiplexed over `side-band-64k`, see [`side_band`](super::side_band).
    pub side_band_64k: bool,
    /// The client wants no progress output.
    pub quiet: bool,
    /// The client wants all commands applied or none. Only set if it was offered.
    pub atomic: bool,
    /// The client sends push options after the commands. Only set if it was offered.
    pub push_options: bool,
    /// The identifier the client announced as `session-id`, which is only good for tracing.
    pub session_id: Option<BString>,
}

impl Features {
    /// The format of the status report to send, or `None` if the client didn't ask for one.
    ///
    /// Like `receive-pack`, prefer [`StatusFormat::V2`] if the client asked for both.
    pub fn status_format(&self) -> Option<StatusFormat> {
        if self.report_status_v2 {
            Some(StatusFormat::V2)
        } else if self.report_status {
            Some(StatusFormat::V1)
        } else {
            None
        }
    }
}

/// The value of a feature and what follows it.
pub(crate) struct FeatureValue<'a> {
    /// The value, which is empty for a feature without one.
    pub value: &'a [u8],
    /// Everything from the start of the value to the end of the feature list, which is what Git prints in some errors.
    pub rest: &'a [u8],
}

/// Whether `byte` is whitespace according to Git's `isspace()`, which is not the one of C: it knows
/// neither vertical tabs nor form feeds.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// Find `feature` in `list` like `parse_feature_value()` of Git does, which includes its handling of
/// features that contain other features as a substring. `list` must not contain a NUL byte.
pub(crate) fn feature_value<'a>(list: &'a [u8], feature: &str) -> Option<FeatureValue<'a>> {
    let len = feature.len();
    let mut start = 0;
    while start < list.len() {
        let found = start + list[start..].find(feature.as_bytes())?;
        // Like Git, a match at the start of the remaining list counts even though a feature may precede it
        // in the whole list, which can only happen when the feature overlaps itself.
        if found == start || is_space(list[found - 1]) {
            let value_at = found + len;
            match list.get(value_at) {
                None => {
                    return Some(FeatureValue {
                        value: &[],
                        rest: &list[value_at..],
                    });
                }
                Some(&byte) if is_space(byte) => {
                    return Some(FeatureValue {
                        value: &[],
                        rest: &list[value_at..],
                    });
                }
                Some(b'=') => {
                    let value_start = value_at + 1;
                    let end = list[value_start..]
                        .iter()
                        .position(|byte| matches!(byte, b' ' | b'\t' | b'\n'))
                        .map_or(list.len(), |pos| value_start + pos);
                    return Some(FeatureValue {
                        value: &list[value_start..end],
                        rest: &list[value_start..],
                    });
                }
                // We matched the start of another feature, so keep looking.
                Some(_) => {}
            }
        }
        start = found + 1;
    }
    None
}
