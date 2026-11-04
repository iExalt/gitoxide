use bstr::{BString, ByteSlice};
use gix_transport::client::Capabilities;

/// The capabilities `git receive-pack` advertised, narrowed to what a push depends on.
///
/// Unknown capabilities are ignored. Whether a missing capability is acceptable is decided
/// by [`Request::new()`](super::Request::new()), which refuses to use anything that wasn't offered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerCapabilities {
    /// The server can report the status of each command.
    pub report_status: bool,
    /// The server can report extended status, including per-update options, see [`StatusFormat::V2`](super::StatusFormat::V2).
    pub report_status_v2: bool,
    /// The server can multiplex progress, errors and the status report over `side-band-64k`.
    pub side_band_64k: bool,
    /// The server can suppress progress output.
    pub quiet: bool,
    /// The server can apply all commands or none.
    pub atomic: bool,
    /// The server accepts packs containing offset deltas.
    pub ofs_delta: bool,
    /// The server accepts commands which delete references.
    pub delete_refs: bool,
    /// The server accepts push options.
    pub push_options: bool,
    /// The server offers to receive signed pushes. They are never requested.
    pub push_cert: bool,
    /// The server announced a session id.
    pub session_id: bool,
    /// The hash used by the remote repository, like `sha256`, which is absent when it uses SHA-1 and predates the capability.
    pub object_format: Option<BString>,
    /// The server's user agent, if it announced one.
    pub agent: Option<BString>,
}

impl ServerCapabilities {
    /// Extract the receive-pack capabilities from `capabilities` as received in a handshake with
    /// [`Service::ReceivePack`](gix_transport::Service::ReceivePack).
    pub fn from_capabilities(capabilities: &Capabilities) -> Self {
        let mut out = ServerCapabilities::default();
        for capability in capabilities.iter() {
            let value = capability.value().map(ToOwned::to_owned);
            match capability.name().as_bytes() {
                b"report-status" => out.report_status = true,
                b"report-status-v2" => out.report_status_v2 = true,
                b"side-band-64k" => out.side_band_64k = true,
                b"quiet" => out.quiet = true,
                b"atomic" => out.atomic = true,
                b"ofs-delta" => out.ofs_delta = true,
                b"delete-refs" => out.delete_refs = true,
                b"push-options" => out.push_options = true,
                b"push-cert" => out.push_cert = true,
                b"session-id" => out.session_id = true,
                b"object-format" => out.object_format = value,
                b"agent" => out.agent = value,
                _ => {}
            }
        }
        out
    }
}
