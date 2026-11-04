use std::io;

use bstr::ByteSlice;
use gix_error::{ErrorExt, Result, ResultExt, bail, message, validation};
use gix_packetline::PacketLineRef;
use gix_transport::Protocol;

use super::{
    Advertisement, Capabilities, Report, Request, request,
    request::{Parser, Step},
    side_band,
};
use crate::push::StatusFormat;

/// The number of bytes `receive-pack` can hold in a packet line, which includes a trailing newline and a NUL byte that
/// is added by Git. Longer lines are a fatal error.
const MAX_BUFFER_LEN: usize = 65520;

/// Write the `advertisement` to `out` as `git receive-pack` does for a client that requested `version`, and flush `out`.
pub fn write_advertisement(out: &mut dyn io::Write, advertisement: &Advertisement, version: Protocol) -> Result {
    let bytes = advertisement.encode(version)?;
    out.write_all(&bytes)
        .and_then(|()| out.flush())
        .or_raise(|| message("Could not write the advertisement"))
}

/// Read the request of a client from `reader`, which consists of its commands and, if it was
/// [asked for](crate::receive::Features::push_options), its push options.
///
/// Nothing beyond the last packet line is read, so the pack of the client is next in `reader` if it
/// [is expected](Request::expects_pack()) and there are commands. Callers must thus not buffer `reader` and then drop the buffer.
/// If there are no commands, nothing else is to be read, and no report is to be written.
///
/// `capabilities` are what was offered to the client. All errors are fatal protocol errors with the messages of Git's `receive-pack`,
/// among them an unexpected end of `reader`. Protocol errors have the class of the errors of [`Parser::feed()`], while errors of `reader` are as reported by it.
/// Packet lines are decoded like Git does, which accepts a data packet without payload and longer packets than `gix-packetline` does.
pub fn read_request(reader: &mut dyn io::Read, capabilities: &Capabilities) -> Result<Request> {
    let mut lines = LineReader {
        inner: reader,
        buf: Vec::new(),
    };
    let mut parser = Parser::new(capabilities);
    while parser.feed(lines.next_line()?)? == Step::More {}
    let mut request = parser.finish();
    if request.features.push_options {
        while let Some(option) = request::parse_push_option(lines.next_line()?)? {
            request.push_options.push(option);
        }
    }
    Ok(request)
}

struct LineReader<'a> {
    inner: &'a mut dyn io::Read,
    buf: Vec<u8>,
}

impl LineReader<'_> {
    fn read_exact(&mut self, len: usize) -> Result<()> {
        self.buf.resize(len, 0);
        self.inner.read_exact(&mut self.buf).map_err(|err| {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                validation("the remote end hung up unexpectedly").raise()
            } else {
                err.and_raise(message("Could not read the request"))
            }
        })
    }

    /// Read the next packet line, decoded like `packet_read_with_status()` of Git does.
    fn next_line(&mut self) -> Result<PacketLineRef<'_>> {
        self.read_exact(4)?;
        let header = self.buf.clone();
        let len = header
            .iter()
            .try_fold(0usize, |len, &byte| {
                (byte as char).to_digit(16).map(|digit| len * 16 + digit as usize)
            })
            .ok_or_else(|| {
                validation(format!(
                    "protocol error: bad line length character: {}",
                    header.as_bstr()
                ))
                .raise()
            })?;
        match len {
            0 => Ok(PacketLineRef::Flush),
            1 => Ok(PacketLineRef::Delimiter),
            2 => Ok(PacketLineRef::ResponseEnd),
            3 => bail!(validation("protocol error: bad line length 3")),
            len => {
                let data_len = len - 4;
                if data_len >= MAX_BUFFER_LEN {
                    bail!(validation(format!("protocol error: bad line length {data_len}")));
                }
                self.read_exact(data_len)?;
                Ok(PacketLineRef::Data(&self.buf))
            }
        }
    }
}

/// Write `report` to `out` in the way the client asked for, with `format`, in a side band if `side_band` is set,
/// and flush `out`.
///
/// If `side_band` is set, the packet that ends the response isn't written, which is the task of the caller once everything
/// else was written, like `receive-pack` does with a flush packet.
pub fn write_report(out: &mut dyn io::Write, report: &Report, format: StatusFormat, side_band: bool) -> Result {
    let bytes = report.encode(format)?;
    let bytes = if side_band {
        side_band::encode(side_band::Channel::Data, &bytes)
    } else {
        bytes
    };
    out.write_all(&bytes)
        .and_then(|()| out.flush())
        .or_raise(|| message("Could not write the report"))
}
