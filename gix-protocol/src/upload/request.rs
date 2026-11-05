//! Reading the requests of a client, as `process_request()` of Git's `serve.c`, `ls_refs()` and `process_args()` of
//! `upload-pack` do.
use bstr::{BString, ByteSlice};
use gix_error::{Result, bail, validation};
use gix_hash::ObjectId;
use gix_packetline::PacketLineRef;

use super::Capabilities;

/// The number of `ref-prefix` arguments at which Git stops using them, as they are meant to be a comprehensive list.
const TOO_MANY_PREFIXES: usize = 65536;

/// The arguments of `command=ls-refs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LsRefs {
    /// Show the object a tag peels to, as `peeled:<id>`.
    pub peel: bool,
    /// Show where symbolic references point to, as `symref-target:<name>`.
    pub symrefs: bool,
    /// The client accepts to learn about a `HEAD` that points to a reference that doesn't exist yet. It needs `symrefs` as well.
    ///
    /// This is what the client sent. Drivers ignore it if `lsrefs.unborn` is `ignore`, as Git does, which is also when it isn't
    /// announced, while `allow` honors it without announcing it.
    pub unborn: bool,
    /// Only references whose name starts with one of these are shown, if there are any. Empty if there were too many to be used.
    pub ref_prefixes: Vec<BString>,
}

/// The arguments of `command=fetch`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fetch {
    /// The objects the client wants, without duplicates and in the order they were requested.
    pub wants: Vec<ObjectId>,
    /// The objects the client says it has, in the order of the request.
    pub haves: Vec<ObjectId>,
    /// The client doesn't want to negotiate any more, so the pack is to be sent now.
    pub done: bool,
    /// The pack may contain deltas against objects of the client, which it doesn't contain.
    pub thin_pack: bool,
    /// The pack may use offsets to refer to the base of a delta, instead of its id.
    pub ofs_delta: bool,
    /// The client wants no progress messages.
    pub no_progress: bool,
    /// Annotated tags that point at what is sent are to be sent as well.
    pub include_tag: bool,
    /// The client waits for the server to say that it has everything, before it sends `done`.
    pub wait_for_done: bool,
    /// The lines of `shallow`, `deepen`, `deepen-relative`, `deepen-since` and `deepen-not`, which are to refuse by drivers
    /// that don't implement shallow clones.
    pub shallow_lines: Vec<BString>,
}

/// The command of a request, with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `command=ls-refs`.
    LsRefs(LsRefs),
    /// `command=fetch`.
    Fetch(Fetch),
}

/// A complete request of a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// What the client announced as `agent`, which is only good for tracing.
    pub agent: Option<BString>,
    /// The values of all `server-option` lines, which are only good for tracing, as Git doesn't use them either.
    pub server_options: Vec<BString>,
    /// The identifier the client announced as `session-id`, which is only good for tracing.
    pub session_id: Option<BString>,
    /// What the client asks for.
    pub command: Command,
}

/// The result of [feeding](Parser::feed()) a packet line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// More lines are needed.
    More,
    /// The request is complete, and [`Parser::finish()`] can be called. It's also the end of the session if there was no request.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Reading the command and the capabilities of the client.
    Header,
    /// Reading the arguments of the command, which follow the delimiter packet.
    Arguments,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    LsRefs,
    Fetch,
}

/// Parses a request packet line by packet line.
#[derive(Debug)]
pub struct Parser {
    hash_kind: gix_hash::Kind,
    session_id_offered: bool,
    state: State,
    seen_anything: bool,
    kind: Option<Kind>,
    client_object_format: Option<&'static str>,
    agent: Option<BString>,
    server_options: Vec<BString>,
    session_id: Option<BString>,
    ls_refs: LsRefs,
    fetch: Fetch,
}

fn chomp(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

fn hex_id(line: &[u8], hash_kind: gix_hash::Kind) -> Option<ObjectId> {
    ObjectId::from_hex(line.get(..hash_kind.len_in_hex())?).ok()
}

impl Parser {
    /// Create a parser for requests to a repository that announced `capabilities`, which determine what the client may use.
    pub fn new(capabilities: &Capabilities) -> Self {
        Parser {
            hash_kind: capabilities.object_format,
            session_id_offered: capabilities.session_id.is_some(),
            state: State::Header,
            seen_anything: false,
            kind: None,
            client_object_format: None,
            agent: None,
            server_options: Vec::new(),
            session_id: None,
            ls_refs: LsRefs::default(),
            fetch: Fetch::default(),
        }
    }

    /// Process the next packet `line` of the request.
    ///
    /// All errors are fatal protocol errors with the messages of Git's `upload-pack`, and
    /// classified as [`Validation`](gix_error::Class::Validation).
    pub fn feed(&mut self, line: PacketLineRef<'_>) -> Result<Step> {
        if let PacketLineRef::Data(data) = line
            && let Some(message) = chomp(data).strip_prefix(b"ERR ")
        {
            bail!(validation(format!("remote error: {}", message.as_bstr())));
        }
        match self.state {
            State::Header => self.feed_header(line),
            State::Arguments => self.feed_argument(line),
            State::Done => Ok(Step::Done),
        }
    }

    fn feed_header(&mut self, line: PacketLineRef<'_>) -> Result<Step> {
        let data = match line {
            PacketLineRef::Data(data) => chomp(data),
            PacketLineRef::Flush => {
                if !self.seen_anything {
                    self.state = State::Done;
                    return Ok(Step::Done);
                }
                self.check_complete()?;
                self.state = State::Done;
                return Ok(Step::Done);
            }
            PacketLineRef::Delimiter => {
                self.check_complete()?;
                self.state = State::Arguments;
                return Ok(Step::More);
            }
            PacketLineRef::ResponseEnd => bail!(validation("protocol error: unexpected response end packet")),
        };
        self.seen_anything = true;
        if let Some(name) = data.strip_prefix(b"command=") {
            if let Some(kind) = self.kind {
                bail!(validation(format!(
                    "command '{}' requested after already requesting command '{}'",
                    name.as_bstr(),
                    match kind {
                        Kind::LsRefs => "ls-refs",
                        Kind::Fetch => "fetch",
                    }
                )));
            }
            self.kind = Some(match name {
                b"ls-refs" => Kind::LsRefs,
                b"fetch" => Kind::Fetch,
                _ => bail!(validation(format!("invalid command '{}'", name.as_bstr()))),
            });
            return Ok(Step::More);
        }
        let (key, value) = match data.find_byte(b'=') {
            Some(pos) => (&data[..pos], Some(&data[pos + 1..])),
            None => (data, None),
        };
        match (key, value) {
            (b"agent", Some(value)) => self.agent = Some(value.into()),
            (b"agent", None) => {}
            (b"server-option", Some(value)) => self.server_options.push(value.into()),
            (b"server-option", None) => {}
            (b"session-id", value) if self.session_id_offered => self.session_id = value.map(Into::into),
            (b"object-format", None) => bail!(validation("object-format capability requires an argument")),
            (b"object-format", Some(value)) => {
                self.client_object_format = Some(match value {
                    b"sha1" => "sha1",
                    b"sha256" => "sha256",
                    _ => bail!(validation(format!("unknown object format '{}'", value.as_bstr()))),
                });
            }
            _ => bail!(validation(format!("unknown capability '{}'", data.as_bstr()))),
        }
        Ok(Step::More)
    }

    /// What Git checks once the header is complete, before it hands the request to the command.
    fn check_complete(&self) -> Result {
        if self.kind.is_none() {
            bail!(validation("no command requested"));
        }
        // A client that doesn't say which format it uses speaks `sha1`, as the protocol had no other when it started.
        let client = self.client_object_format.unwrap_or("sha1");
        if client != self.hash_kind.to_string() {
            bail!(validation(format!(
                "mismatched object format: server {}; client {client}",
                self.hash_kind
            )));
        }
        Ok(())
    }

    fn feed_argument(&mut self, line: PacketLineRef<'_>) -> Result<Step> {
        let data = match line {
            PacketLineRef::Data(data) => chomp(data),
            PacketLineRef::Flush => {
                self.state = State::Done;
                if self.ls_refs.ref_prefixes.len() >= TOO_MANY_PREFIXES {
                    self.ls_refs.ref_prefixes.clear();
                }
                return Ok(Step::Done);
            }
            PacketLineRef::Delimiter | PacketLineRef::ResponseEnd => bail!(validation(match self.kind {
                Some(Kind::Fetch) => "expected flush after fetch arguments",
                _ => "expected flush after ls-refs arguments",
            })),
        };
        match self.kind {
            Some(Kind::LsRefs) => self.ls_refs_argument(data)?,
            Some(Kind::Fetch) => self.fetch_argument(data)?,
            None => bail!(validation("no command requested")),
        }
        Ok(Step::More)
    }

    fn ls_refs_argument(&mut self, arg: &[u8]) -> Result {
        match arg {
            b"peel" => self.ls_refs.peel = true,
            b"symrefs" => self.ls_refs.symrefs = true,
            b"unborn" => self.ls_refs.unborn = true,
            _ => {
                let Some(prefix) = arg.strip_prefix(b"ref-prefix ") else {
                    bail!(validation(format!("unexpected line: '{}'", arg.as_bstr())));
                };
                if self.ls_refs.ref_prefixes.len() < TOO_MANY_PREFIXES {
                    self.ls_refs.ref_prefixes.push(prefix.into());
                }
            }
        }
        Ok(())
    }

    fn fetch_argument(&mut self, arg: &[u8]) -> Result {
        let fetch = &mut self.fetch;
        if let Some(hex) = arg.strip_prefix(b"want ") {
            let Some(id) = hex_id(hex, self.hash_kind) else {
                bail!(validation(format!(
                    "git upload-pack: protocol error, expected to get oid, not '{}'",
                    arg.as_bstr()
                )));
            };
            if !fetch.wants.contains(&id) {
                fetch.wants.push(id);
            }
        } else if let Some(hex) = arg.strip_prefix(b"have ") {
            let Some(id) = hex_id(hex, self.hash_kind) else {
                bail!(validation(format!(
                    "git upload-pack: expected SHA1 object, got '{}'",
                    hex.as_bstr()
                )));
            };
            fetch.haves.push(id);
        } else {
            match arg {
                b"thin-pack" => fetch.thin_pack = true,
                b"ofs-delta" => fetch.ofs_delta = true,
                b"no-progress" => fetch.no_progress = true,
                b"include-tag" => fetch.include_tag = true,
                b"done" => fetch.done = true,
                b"wait-for-done" => fetch.wait_for_done = true,
                b"deepen-relative" => fetch.shallow_lines.push(arg.into()),
                _ if [&b"shallow "[..], b"deepen ", b"deepen-since ", b"deepen-not "]
                    .iter()
                    .any(|prefix| arg.starts_with(prefix)) =>
                {
                    fetch.shallow_lines.push(arg.into());
                }
                _ => bail!(validation(format!("unexpected line: '{}'", arg.as_bstr()))),
            }
        }
        Ok(())
    }

    /// Return the request that was read, or `None` if the client ended the session instead of asking something.
    ///
    /// It is a mistake to call this before [`feed()`](Self::feed()) returned [`Step::Done`], which makes this return `None`.
    pub fn finish(self) -> Option<Request> {
        if self.state != State::Done {
            return None;
        }
        let command = match self.kind? {
            Kind::LsRefs => Command::LsRefs(self.ls_refs),
            Kind::Fetch => Command::Fetch(self.fetch),
        };
        Some(Request {
            agent: self.agent,
            server_options: self.server_options,
            session_id: self.session_id,
            command,
        })
    }
}
