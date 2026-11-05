//! What is sent in answer to requests, as `ls_refs()` and `upload_pack_v2()` of Git write it.
use bstr::{BString, ByteSlice};
use gix_error::{Result, bail, validation};
use gix_hash::ObjectId;

/// The pkt-line payload limit, excluding the four length bytes.
const MAX_PACKET_DATA_LEN: usize = 65516;

pub(super) fn write_line(out: &mut Vec<u8>, line: &[u8]) -> Result {
    if line.len() > MAX_PACKET_DATA_LEN {
        bail!(validation(format!(
            "A line of {} bytes exceeds the packet line limit of {MAX_PACKET_DATA_LEN}",
            line.len()
        )));
    }
    out.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    out.extend_from_slice(line);
    Ok(())
}

/// A line of the answer to `ls-refs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The object the reference points to, or `None` if the reference is `HEAD` and points to a reference that doesn't exist yet.
    /// Such a line is sent as `unborn`.
    pub id: Option<ObjectId>,
    /// The name of the reference, without the namespace.
    pub name: BString,
    /// The name of the reference that this one points to, if it is symbolic and the client asked for `symrefs`.
    pub symref_target: Option<BString>,
    /// The object that a tag peels to, if the client asked for `peel`.
    pub peeled: Option<ObjectId>,
}

/// Encode the answer to `ls-refs`, which is one line for each of `references` and a flush packet.
pub fn ls_refs(references: &[Reference]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for reference in references {
        let mut line = match reference.id {
            Some(id) => BString::from(format!("{id} ")),
            None => BString::from("unborn "),
        };
        line.extend_from_slice(&reference.name);
        if let Some(target) = &reference.symref_target {
            line.extend_from_slice(b" symref-target:");
            line.extend_from_slice(target);
        }
        if let Some(peeled) = reference.peeled {
            line.extend_from_slice(format!(" peeled:{peeled}").as_bytes());
        }
        line.push(b'\n');
        write_line(&mut out, line.as_bstr())?;
    }
    out.extend_from_slice(b"0000");
    Ok(out)
}

/// Encode the `acknowledgments` section of a `fetch` response, which tells the client which of its `have` lines are known: one `ACK`
/// for each of `acknowledged`, or `NAK` if there are none.
///
/// If the server is `ready` to send the pack because what is common is enough, that is said as well, and the section is ended
/// with a delimiter packet, after which the pack follows. Otherwise it is ended with a flush packet, which is the end of the
/// response, and the client sends more `have` lines in a new request.
pub fn acknowledgments(acknowledged: &[ObjectId], ready: bool) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_line(&mut out, b"acknowledgments\n")?;
    if acknowledged.is_empty() {
        write_line(&mut out, b"NAK\n")?;
    }
    for id in acknowledged {
        write_line(&mut out, format!("ACK {id}\n").as_bytes())?;
    }
    if ready {
        write_line(&mut out, b"ready\n")?;
    }
    out.extend_from_slice(if ready { b"0001" } else { b"0000" });
    Ok(out)
}

/// The line that starts the section of a `fetch` response with the pack. Everything that follows in the response is the
/// pack in packets of [`side_band`](crate::receive::side_band), ended with a flush packet.
pub fn packfile() -> &'static [u8] {
    b"000dpackfile\n"
}

/// The packet that tells the client why the request fails, after which the connection ends.
///
/// Newlines in `message` are replaced by spaces to keep the packet a single line, and a message that is too long is cut.
pub fn error(message: &str) -> Vec<u8> {
    let mut line = format!("ERR {}", message.replace('\n', " ")).into_bytes();
    line.truncate(MAX_PACKET_DATA_LEN);
    let mut out = Vec::with_capacity(line.len() + 4);
    out.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    out.extend_from_slice(&line);
    out
}
