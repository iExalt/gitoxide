//! Multiplexing of a response over `side-band-64k`, as `send_sideband()` of Git does.
pub use gix_packetline::Channel;

/// The maximum number of bytes of data a single packet of a band carries, which is `LARGE_PACKET_MAX` of Git minus
/// the four bytes of the length and the byte that names the band.
pub const MAX_DATA_LEN: usize = 65515;

/// Encode `data` for `channel` as one or more packets, each with at most [`MAX_DATA_LEN`] bytes of data.
///
/// Nothing is written for empty `data`.
pub fn encode(channel: Channel, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / MAX_DATA_LEN * 5 + 5);
    for chunk in data.chunks(MAX_DATA_LEN) {
        out.extend_from_slice(format!("{:04x}", chunk.len() + 5).as_bytes());
        out.push(channel as u8);
        out.extend_from_slice(chunk);
    }
    out
}
