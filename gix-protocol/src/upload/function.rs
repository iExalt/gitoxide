use std::io;

use gix_error::{Result, ResultExt, message};

use super::{
    Capabilities, Request,
    request::{Parser, Step},
};
use crate::receive::function::LineReader;

/// Write the advertisement of `capabilities` to `out` as `git upload-pack` does for a client that asked for protocol version 2,
/// and flush `out`.
pub fn write_advertisement(out: &mut dyn io::Write, capabilities: &Capabilities) -> Result {
    let bytes = capabilities.encode()?;
    out.write_all(&bytes)
        .and_then(|()| out.flush())
        .or_raise(|| message("Could not write the advertisement"))
}

/// Read the next request of a client from `reader`, or return `None` if the client ended the session by closing its end or by
/// sending a flush packet where a request would start.
///
/// Nothing beyond the last packet line of the request is read. Errors are the fatal protocol errors of Git's `upload-pack`,
/// among them an unexpected end of `reader` in the middle of a request.
pub fn read_request(reader: &mut dyn io::Read, capabilities: &Capabilities) -> Result<Option<Request>> {
    let mut lines = LineReader::new(reader);
    let mut parser = Parser::new(capabilities);
    // A client that is done closes the connection before it starts another request.
    let Some(first) = lines.next_line_or_end()? else {
        return Ok(None);
    };
    let mut step = parser.feed(first)?;
    while step == Step::More {
        step = parser.feed(lines.next_line()?)?;
    }
    Ok(parser.finish())
}
