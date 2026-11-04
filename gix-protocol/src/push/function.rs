use std::{cell::RefCell, io::Write, ops::ControlFlow};

use bstr::{BString, ByteSlice};
use gix_error::{Result, ResultExt, bail, corruption, message};
use gix_transport::{
    client::{
        MessageKind, WriteMode,
        blocking_io::{ExtendedBufRead, Transport},
    },
    packetline::{PacketLineRef, blocking_io::StreamingPeekableIter},
};

use super::{Report, Request};

/// A function receiving the remote's messages as `(is_error, text)`.
type MessageHandler<'a> = &'a mut dyn FnMut(bool, &[u8]);

/// Collect the payloads of all packet lines returned by `$read`, until it returns `None`.
macro_rules! collect_lines {
    ($read:expr) => {{
        let mut lines: Vec<Vec<u8>> = Vec::new();
        while let Some(line) = $read {
            let line = line
                .or_raise(|| corruption("The status report could not be read, so the outcome of the push is unknown"))?
                .or_raise(|| corruption("The status report contained an invalid packet line"))?;
            match line {
                PacketLineRef::Data(data) => lines.push(data.to_vec()),
                _ => bail!(corruption(
                    "The status report contained an unexpected special packet line"
                )),
            }
        }
        lines
    }};
}

/// Send `request` over `transport`, which must have performed a handshake with
/// [`Service::ReceivePack`](gix_transport::Service::ReceivePack) already, and return the server's report.
///
/// `write_pack` is called with a writer for the raw pack data if the request [expects a pack](Request::expects_pack()),
/// and has to write a complete pack, which can be one without objects: the server keeps waiting for the rest
/// of an incomplete one, as the connection stays open. It must honor
/// [`Request::ofs_delta_allowed()`]. If it fails, the error is returned after the transport was left with an
/// incomplete pack that the server can't unpack, so nothing is applied. The transport has to be dropped
/// by the caller then, as it can't be used for anything else.
///
/// `on_remote_message` receives the remote's messages as `(is_error, text)`, which is progress unless `is_error` is set.
///
/// ## Progress
///
/// This function can't read the server's messages while it writes the pack, but a server that
/// relays progress while it receives the pack can block once its output isn't consumed. For that
/// reason, progress is only requested with [`Options::progress`](super::Options::progress),
/// which should be used with small packs only.
///
/// ## Outcome
///
/// See the [module documentation](super) for what a [`Report`] and an error mean. Errors from this function
/// are classified as [`Corruption`](gix_error::Class::Corruption) once the request was sent, which includes
/// the server closing the connection without a report, as happens if one of its hooks aborts. In these cases
/// commands may have been applied.
pub fn push(
    mut transport: impl Transport,
    request: &Request,
    write_pack: impl FnOnce(&mut dyn Write) -> Result<()>,
    mut on_remote_message: Option<MessageHandler<'_>>,
    trace: bool,
) -> Result<Report> {
    let remote_error = RefCell::new(None::<BString>);
    let mut writer = transport
        .request(WriteMode::Binary, MessageKind::Flush, trace)
        .or_raise(|| message("Could not prepare the push request"))?;
    let unknown = || corruption("The push was interrupted, so its outcome is unknown");
    for line in request.command_lines() {
        writer.write_all(&line).or_raise(unknown)?;
    }
    writer.write_message(MessageKind::Flush).or_raise(unknown)?;
    let option_lines = request.push_option_lines();
    if !option_lines.is_empty() {
        for line in option_lines {
            writer.write_all(&line).or_raise(unknown)?;
        }
        writer.write_message(MessageKind::Flush).or_raise(unknown)?;
    }
    let (mut raw_writer, mut reader) = writer.into_parts();
    if request.expects_pack() {
        write_pack(&mut raw_writer)?;
    }
    raw_writer.flush().or_raise(unknown)?;
    // The server may wait for the end of its input, and we must not read before it is closed or complete.
    drop(raw_writer);

    let lines = read_report(
        &mut *reader,
        request.uses_side_band(),
        &remote_error,
        on_remote_message
            .take()
            .map(|callback| -> MessageHandler<'_> { callback }),
        trace,
    );
    drop(reader);
    let lines = add_remote_error(lines, &remote_error)?;
    add_remote_error(request.parse_report(lines.iter().map(Vec::as_slice)), &remote_error)
}

/// If the remote sent an error message, add it as context to `result`, which is the reason for it.
fn add_remote_error<T>(result: Result<T>, remote_error: &RefCell<Option<BString>>) -> Result<T> {
    match remote_error.borrow().as_ref() {
        Some(error) => result.or_raise(|| message!("The remote sent an error: {}", error.trim_end().as_bstr())),
        None => result,
    }
}

/// Read the payloads of the lines of the status report, which follow the pack, from `reader`.
fn read_report<'a>(
    reader: &mut (dyn ExtendedBufRead<'a> + Unpin + 'a),
    side_band: bool,
    remote_error: &'a RefCell<Option<BString>>,
    mut on_remote_message: Option<MessageHandler<'a>>,
    trace: bool,
) -> Result<Vec<Vec<u8>>> {
    if side_band {
        reader.set_progress_handler(Some(Box::new(move |is_error, text| {
            if is_error {
                *remote_error.borrow_mut() = Some(text.into());
            }
            if let Some(callback) = on_remote_message.as_mut() {
                callback(is_error, text);
            }
            ControlFlow::Continue(())
        })));
        // The report is a stream of packet lines in band 1, which may split them arbitrarily.
        let lines = {
            let mut report = StreamingPeekableIter::new(&mut *reader, &[PacketLineRef::Flush], trace);
            let lines = collect_lines!(report.read_line());
            if report.stopped_at() != Some(PacketLineRef::Flush) && !lines.is_empty() {
                bail!(corruption("The status report ended without a flush packet"));
            }
            lines
        };
        // What follows are messages of hooks that ran after the report, which are not part of the outcome.
        std::io::copy(reader, &mut std::io::sink()).ok();
        Ok(lines)
    } else {
        let lines = collect_lines!(reader.readline());
        if reader.stopped_at() != Some(MessageKind::Flush) && !lines.is_empty() {
            bail!(corruption("The status report ended without a flush packet"));
        }
        Ok(lines)
    }
}
