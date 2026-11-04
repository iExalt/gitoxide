//! Reading the request of a client, as `read_head_info()` and `read_push_options()` of Git's `receive-pack` do.
use bstr::{BString, ByteSlice};
use gix_error::{Result, bail, unsupported, validation};
use gix_hash::ObjectId;
use gix_packetline::PacketLineRef;

use super::{Capabilities, Features, capabilities::feature_value};

/// A command as it was sent, which updates a reference from `old_id` to `new_id`.
///
/// Nothing but the hex of both ids is validated, as the server decides what to do with a name it doesn't like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommand {
    /// The value the reference is expected to have, or the null id if it is to be created.
    pub old_id: ObjectId,
    /// The value the reference is to have, or the null id if it is to be deleted.
    pub new_id: ObjectId,
    /// The name of the reference, which is any sequence of bytes up to a NUL byte.
    pub ref_name: BString,
}

impl RawCommand {
    /// Return `true` if the reference is to be deleted.
    pub fn is_delete(&self) -> bool {
        self.new_id.is_null()
    }
}

/// A complete request of a client, up to but not including the pack.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Request {
    /// The commands in the order they were sent. It is empty if the client had nothing to push.
    pub commands: Vec<RawCommand>,
    /// What the client asked for.
    pub features: Features,
    /// The shallow boundary of the client, if it is shallow.
    pub shallow: Vec<ObjectId>,
    /// The push options, which are only read if the client [sends them](Features::push_options).
    pub push_options: Vec<BString>,
}

impl Request {
    /// Return `true` if a pack follows the request, which is the case unless all commands delete.
    ///
    /// Like Git, this is `true` for a request without commands, even though nothing is read then.
    pub fn expects_pack(&self) -> bool {
        !self.commands.iter().all(RawCommand::is_delete)
    }
}

/// The result of [feeding](Parser::feed()) a packet line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// More lines are needed.
    More,
    /// The commands are complete, and [`Parser::finish()`] can be called.
    Done,
}

/// Parses the commands of a request packet line by packet line.
#[derive(Debug)]
pub struct Parser {
    hash_kind: gix_hash::Kind,
    atomic_offered: bool,
    push_options_offered: bool,
    request: Request,
}

/// Remove the trailing newline of `line`, as Git does for every line it reads.
fn chomp(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

/// The message of an error packet, which Git finds in the line after chomping it and reads like a C string.
fn error_message(line: &[u8]) -> Option<&[u8]> {
    let line = &line[..line.find_byte(0).unwrap_or(line.len())];
    line.strip_prefix(b"ERR ")
}

fn hex_id(line: &[u8], hash_kind: gix_hash::Kind) -> Option<ObjectId> {
    let hex = line.get(..hash_kind.len_in_hex())?;
    ObjectId::from_hex(hex).ok()
}

impl Parser {
    /// Create a parser for requests to a repository that offers `capabilities`, which determine what the client may use.
    pub fn new(capabilities: &Capabilities) -> Self {
        Parser {
            hash_kind: capabilities.object_format,
            atomic_offered: capabilities.atomic,
            push_options_offered: capabilities.push_options,
            request: Request::default(),
        }
    }

    /// Process the next packet `line` of the request.
    ///
    /// All errors are fatal protocol errors, with the messages of Git's. They are classified as
    /// [`Validation`](gix_error::Class::Validation) except for push certificates, which are
    /// [`Unsupported`](gix_error::Class::Unsupported).
    pub fn feed(&mut self, line: PacketLineRef<'_>) -> Result<Step> {
        let data = match line {
            PacketLineRef::Data(data) => data,
            PacketLineRef::Flush | PacketLineRef::Delimiter | PacketLineRef::ResponseEnd => return Ok(Step::Done),
        };
        let line = chomp(data);
        if let Some(error) = error_message(line) {
            bail!(validation(format!("remote error: {}", error.as_bstr())));
        }
        // Everything is C strings in Git, which stop at a NUL byte, so features follow after it.
        let (command, features) = match line.find_byte(0) {
            Some(nul) => (&line[..nul], Some(&line[nul + 1..])),
            None => (line, None),
        };
        if line.len() > 8 && line.starts_with(b"shallow ") {
            let id = hex_id(&command[8..], self.hash_kind).ok_or_else(|| {
                validation(format!(
                    "protocol error: expected shallow sha, got '{}'",
                    command[8..].as_bstr()
                ))
            })?;
            self.request.shallow.push(id);
            return Ok(Step::More);
        }
        if let Some(features) = features {
            self.parse_features(features)?;
        }
        if command == b"push-cert" {
            bail!(unsupported(
                "Push certificates are not supported, and they are never offered"
            ));
        }
        self.request.commands.push(self.parse_command(command)?);
        Ok(Step::More)
    }

    fn parse_features(&mut self, list: &[u8]) -> Result {
        // The feature list is a C string in Git, which ends at the next NUL byte.
        let list = list.split(|&byte| byte == 0).next().unwrap_or_default();
        let features = &mut self.request.features;
        let has = |name: &str| feature_value(list, name).is_some();
        features.report_status |= has("report-status");
        features.report_status_v2 |= has("report-status-v2");
        features.side_band_64k |= has("side-band-64k");
        features.quiet |= has("quiet");
        features.atomic |= self.atomic_offered && has("atomic");
        features.push_options |= self.push_options_offered && has("push-options");
        let format = feature_value(list, "object-format");
        let (value, shown) = match &format {
            Some(format) => (format.value, format.rest),
            None => (&b"sha1"[..], &b"sha1"[..]),
        };
        if value != self.hash_kind.to_string().as_bytes() {
            bail!(validation(format!(
                "error: unsupported object format '{}'",
                shown.as_bstr()
            )));
        }
        if let Some(session) = feature_value(list, "session-id") {
            features.session_id = Some(session.value.into());
        }
        Ok(())
    }

    fn parse_command(&self, line: &[u8]) -> Result<RawCommand> {
        let invalid = || {
            validation(format!(
                "protocol error: expected old/new/ref, got '{}'",
                line.as_bstr()
            ))
        };
        let hex_len = self.hash_kind.len_in_hex();
        let old_id = hex_id(line, self.hash_kind).ok_or_else(invalid)?;
        let rest = &line[hex_len..];
        let rest = rest.strip_prefix(b" ").ok_or_else(invalid)?;
        let new_id = hex_id(rest, self.hash_kind).ok_or_else(invalid)?;
        let rest = &rest[hex_len..];
        let ref_name = rest.strip_prefix(b" ").ok_or_else(invalid)?;
        Ok(RawCommand {
            old_id,
            new_id,
            ref_name: ref_name.into(),
        })
    }

    /// Return the request, which is only complete once [`Step::Done`] was returned by [`feed()`](Self::feed()).
    pub fn finish(self) -> Request {
        self.request
    }
}

/// Process the next packet `line` of the push options, and return the option it carries, or `None` if the options are complete.
///
/// Like Git, every line is an option and nothing is validated, except that error packets are fatal.
pub fn parse_push_option(line: PacketLineRef<'_>) -> Result<Option<BString>> {
    let PacketLineRef::Data(data) = line else {
        return Ok(None);
    };
    let line = chomp(data);
    if let Some(error) = error_message(line) {
        bail!(validation(format!("remote error: {}", error.as_bstr())));
    }
    Ok(Some(line.into()))
}
