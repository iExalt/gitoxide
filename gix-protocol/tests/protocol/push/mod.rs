use gix_hash::{Kind, ObjectId};
use gix_protocol::push::{Command, Options, Request, ServerCapabilities};

mod command;
#[cfg(feature = "blocking-client")]
mod driver;
#[cfg(feature = "blocking-client")]
mod real_peer;
mod report;
mod request;

/// An id consisting of `byte` repeated for every byte of `kind`, with `0` being the null id.
pub fn id(kind: Kind, byte: u8) -> ObjectId {
    ObjectId::from_bytes_or_panic(&vec![byte; kind.len_in_bytes()])
}

/// The classes of `err`, which tell callers what to do about it.
pub fn classes(err: &gix_error::Error) -> Vec<gix_error::Class> {
    err.classify().map(|class| class.class()).collect()
}

/// Server capabilities as advertised by receive-pack, with `extra` capabilities appended.
pub fn server(kind: Kind, extra: &str) -> ServerCapabilities {
    let advertised = format!(
        "report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta object-format={kind} agent=git/2.56.0 {extra}"
    );
    server_with(&advertised)
}

/// Server capabilities parsed from exactly `advertised`.
pub fn server_with(advertised: &str) -> ServerCapabilities {
    let capabilities = gix_transport::client::Capabilities::from_bytes(format!("\0{advertised}").as_bytes())
        .expect("valid capabilities")
        .0;
    ServerCapabilities::from_capabilities(&capabilities)
}

/// Commands from `(ref name, old byte, new byte)`, where byte `0` stands for the null id.
pub fn commands(kind: Kind, updates: &[(&str, u8, u8)]) -> Vec<Command> {
    updates
        .iter()
        .map(|(name, old, new)| Command::new(*name, id(kind, *old), id(kind, *new)).expect("valid command"))
        .collect()
}

pub fn request(kind: Kind, updates: &[(&str, u8, u8)], options: Options) -> Request {
    Request::new(commands(kind, updates), kind, &server(kind, "push-options"), options).expect("a valid request")
}
