use bstr::{BString, ByteSlice};
use gix_error::{Result, bail, unsupported, validation};

use super::{Command, Report, ServerCapabilities};

/// The pkt-line payload limit, excluding the four length bytes.
const MAX_PACKET_DATA_LEN: usize = 65516;

/// The kind of status report to ask the server for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StatusFormat {
    /// `report-status`: every command is reported exactly once, as accepted (`ok`) or rejected (`ng`).
    #[default]
    V1,
    /// `report-status-v2`: like [`V1`](StatusFormat::V1), but an accepted command may be reported multiple times, each
    /// report carrying `option` lines about the update that was actually performed. This happens if the server redirected
    /// the update, for instance with the `proc-receive` hook, so callers must not assume the reported
    /// reference or ids equal the command.
    V2,
}

/// Choices a caller makes about how to push, validated against the server's capabilities by [`Request::new()`].
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The kind of status report to request. It is always requested, as without it the outcome cannot be known.
    pub status_format: StatusFormat,
    /// Ask the server to apply all commands or none. Refused if the server doesn't offer `atomic`.
    pub atomic: bool,
    /// Push options to pass to the server's hooks. Refused if the server doesn't offer `push-options`.
    pub push_options: Vec<BString>,
    /// Ask the server to report progress while it receives the pack, instead of suppressing it with `quiet`.
    ///
    /// Drivers that can't read the progress while they write the pack, like [`push()`](super::push()), risk a stall if the
    /// server produces more of it than the connection buffers, so leave this off unless the pack is small. The server
    /// is only asked to be quiet if it offers `quiet`.
    pub progress: bool,
    /// The value to announce as `agent`, which should look like `git/2.50.0-gitoxide`. It is omitted if `None`.
    pub agent: Option<String>,
}

/// A validated push request: the commands to send, and everything negotiated with the server.
#[derive(Debug, Clone)]
pub struct Request {
    commands: Vec<Command>,
    hash_kind: gix_hash::Kind,
    status_format: StatusFormat,
    atomic: bool,
    push_options: Vec<BString>,
    side_band: bool,
    quiet: bool,
    ofs_delta: bool,
    object_format: Option<String>,
    agent: Option<String>,
}

impl Request {
    /// Validate that `commands` can be pushed to a server with `server` capabilities using `options`,
    /// for a repository using `hash_kind`.
    ///
    /// Nothing is silently degraded: if a requested feature isn't offered, an error classified as
    /// [`Unsupported`](gix_error::Class::Unsupported) is returned. Invalid input is classified as
    /// [`Validation`](gix_error::Class::Validation). In both cases nothing was sent yet.
    pub fn new(
        commands: Vec<Command>,
        hash_kind: gix_hash::Kind,
        server: &ServerCapabilities,
        options: Options,
    ) -> Result<Self> {
        if commands.is_empty() {
            bail!(validation("There must be at least one command to push"));
        }
        let mut seen = std::collections::HashSet::with_capacity(commands.len());
        for command in &commands {
            if command.old_id().kind() != hash_kind {
                bail!(validation(format!(
                    "Command for '{}' uses {} ids, but the repository uses {hash_kind}",
                    command.ref_name(),
                    command.old_id().kind()
                )));
            }
            if !seen.insert(command.ref_name()) {
                bail!(validation(format!(
                    "Reference '{}' is the subject of more than one command",
                    command.ref_name()
                )));
            }
            if command.is_delete() && !server.delete_refs {
                bail!(unsupported(format!(
                    "The server doesn't support deleting references, but '{}' is to be deleted",
                    command.ref_name()
                )));
            }
        }

        let object_format = match server.object_format.as_ref() {
            Some(format) if format != hash_kind.to_string().as_bytes() => {
                bail!(unsupported(format!(
                    "The remote repository uses object format '{format}', the local repository uses {hash_kind}",
                )));
            }
            Some(format) => Some(format.to_string()),
            None if hash_kind.to_string() == "sha1" => None,
            None => bail!(unsupported(format!(
                "The server didn't announce an object format, so it is limited to sha1, but the local repository uses {hash_kind}"
            ))),
        };

        match options.status_format {
            StatusFormat::V1 if !server.report_status => {
                bail!(unsupported(
                    "The server doesn't offer 'report-status', so the outcome of the push could not be verified"
                ));
            }
            StatusFormat::V2 if !server.report_status_v2 => {
                bail!(unsupported("The server doesn't offer 'report-status-v2'"));
            }
            _ => {}
        }
        if options.atomic && !server.atomic {
            bail!(unsupported(
                "An atomic push was requested, but the server doesn't offer 'atomic'"
            ));
        }
        if !options.push_options.is_empty() {
            if !server.push_options {
                bail!(unsupported(
                    "Push options were provided, but the server doesn't offer 'push-options'"
                ));
            }
            if let Some(invalid) = options
                .push_options
                .iter()
                .find(|option| option.is_empty() || option.find_byteset(b"\0\n").is_some())
            {
                bail!(validation(format!(
                    "Push option {invalid:?} is empty or contains a NUL or newline byte"
                )));
            }
        }
        if let Some(agent) = options.agent.as_deref()
            && (agent.is_empty() || agent.contains(|c: char| c.is_whitespace() || c == '\0'))
        {
            bail!(validation(format!(
                "The agent {agent:?} must be non-empty and contain no whitespace or NUL"
            )));
        }

        let request = Request {
            commands,
            hash_kind,
            status_format: options.status_format,
            atomic: options.atomic,
            push_options: options.push_options,
            side_band: server.side_band_64k,
            quiet: !options.progress && server.quiet,
            ofs_delta: server.ofs_delta,
            object_format,
            agent: options.agent,
        };
        let too_long = request
            .command_lines()
            .into_iter()
            .chain(request.push_option_lines())
            .find(|line| line.len() > MAX_PACKET_DATA_LEN);
        if let Some(line) = too_long {
            bail!(validation(format!(
                "A request line of {} bytes exceeds the packet line limit of {MAX_PACKET_DATA_LEN}",
                line.len()
            )));
        }
        Ok(request)
    }

    /// The commands to send, in the order they will be sent.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// The hash kind of the repository this push originates from.
    pub fn hash_kind(&self) -> gix_hash::Kind {
        self.hash_kind
    }

    /// The kind of status report that was requested.
    pub fn status_format(&self) -> StatusFormat {
        self.status_format
    }

    /// Return `true` if the server was asked to apply all commands or none.
    pub fn is_atomic(&self) -> bool {
        self.atomic
    }

    /// Return `true` if a pack has to follow the [header](Self::encode_header()), which is the case unless all commands delete.
    ///
    /// The pack may contain no objects if the server already has everything, but it must be present.
    /// Sending one when only deleting, or none otherwise, makes the server misinterpret the request.
    pub fn expects_pack(&self) -> bool {
        self.commands.iter().any(|command| !command.is_delete())
    }

    /// Return `true` if the response is multiplexed with `side-band-64k`: the status report is then carried
    /// as packet lines inside band 1, and band 3 carries a fatal error. Otherwise, it is a plain sequence of packet lines.
    pub fn uses_side_band(&self) -> bool {
        self.side_band
    }

    /// Return `true` if the server announced it can handle packs with offset deltas.
    ///
    /// A pack producer must not emit them otherwise.
    pub fn ofs_delta_allowed(&self) -> bool {
        self.ofs_delta
    }

    /// The capabilities announced to the server along with the first command.
    pub fn capabilities(&self) -> String {
        let mut out = String::from(match self.status_format {
            StatusFormat::V1 => "report-status",
            StatusFormat::V2 => "report-status-v2",
        });
        if self.side_band {
            out.push_str(" side-band-64k");
        }
        if self.quiet {
            out.push_str(" quiet");
        }
        if self.atomic {
            out.push_str(" atomic");
        }
        if !self.push_options.is_empty() {
            out.push_str(" push-options");
        }
        if let Some(format) = &self.object_format {
            out.push_str(" object-format=");
            out.push_str(format);
        }
        if let Some(agent) = &self.agent {
            out.push_str(" agent=");
            out.push_str(agent);
        }
        out
    }

    /// The command lines without packet framing or trailing newline: `<old> <new> <ref>`,
    /// the first one followed by a NUL byte and [the capabilities](Self::capabilities()).
    pub fn command_lines(&self) -> Vec<BString> {
        self.commands
            .iter()
            .enumerate()
            .map(|(index, command)| {
                let mut line = BString::from(format!(
                    "{} {} {}",
                    command.old_id(),
                    command.new_id(),
                    command.ref_name()
                ));
                if index == 0 {
                    line.push(0);
                    line.extend_from_slice(self.capabilities().as_bytes());
                }
                line
            })
            .collect()
    }

    /// The push option lines without packet framing, which are empty if no options were provided. Each is the option as it was
    /// provided, as `receive-pack` hands every line to its hooks as an option. The `push-option ` prefix belongs to signed pushes only.
    pub fn push_option_lines(&self) -> Vec<BString> {
        self.push_options.to_vec()
    }

    /// Encode everything the server expects before the pack: the commands and a flush packet,
    /// followed by the push options and another flush packet if there are any.
    ///
    /// The pack, if [expected](Self::expects_pack()), follows without framing.
    pub fn encode_header(&self) -> Vec<u8> {
        fn write_line(out: &mut Vec<u8>, line: &[u8]) {
            out.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
            out.extend_from_slice(line);
        }
        let mut out = Vec::new();
        for line in self.command_lines() {
            write_line(&mut out, &line);
        }
        out.extend_from_slice(b"0000");
        let option_lines = self.push_option_lines();
        if !option_lines.is_empty() {
            for line in option_lines {
                write_line(&mut out, &line);
            }
            out.extend_from_slice(b"0000");
        }
        out
    }

    /// Decode the status report from `lines`, the payloads of the packet lines up to but not including the final
    /// flush packet, with or without trailing newlines.
    ///
    /// Every command must be accounted for, and the report must be consistent with the request. If it isn't,
    /// the error says why, and the outcome of the push must be treated as uncertain. See the [module documentation](super).
    pub fn parse_report<'a>(&self, lines: impl IntoIterator<Item = &'a [u8]>) -> Result<Report> {
        super::report::parse(self, lines)
    }
}
