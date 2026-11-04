//! Integer and byte-string primitives shared by the block and record codecs.

use gix_error::{Error, ErrorExt, Result};

/// Build the error for data that is not a valid reftable.
pub(crate) fn corrupt(message: &'static str) -> Error {
    gix_error::corruption(message).raise()
}

/// Build the error for data that needs a capability this build does not have.
pub(crate) fn unsupported(message: &'static str) -> Error {
    gix_error::unsupported(message).raise()
}

/// Build the error for an API call that violates the writer's contract.
pub(crate) fn misuse(message: &'static str) -> Error {
    gix_error::validation(message).raise()
}

/// Return the number of leading bytes `a` and `b` have in common.
pub(crate) fn common_prefix_size(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(a, b)| a == b).count()
}

/// Return `len` bytes of `data` starting at `at`, or a corruption error if `data` is too short.
pub(crate) fn take(data: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    at.checked_add(len)
        .and_then(|end| data.get(at..end))
        .ok_or_else(|| corrupt("data ends before a record or field is complete"))
}

pub(crate) fn be16(data: &[u8], at: usize) -> Result<u16> {
    let bytes = take(data, at, 2)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn be24(data: &[u8], at: usize) -> Result<u32> {
    let bytes = take(data, at, 3)?;
    Ok(u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]))
}

pub(crate) fn be32(data: &[u8], at: usize) -> Result<u32> {
    let bytes = take(data, at, 4)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(crate) fn be64(data: &[u8], at: usize) -> Result<u64> {
    let bytes = take(data, at, 8)?;
    Ok(u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

/// Append `value` as a 24-bit big-endian integer. The caller guarantees that it fits.
pub(crate) fn put_be24(out: &mut Vec<u8>, value: u32) {
    debug_assert!(value < 1 << 24, "value must fit 24 bits");
    out.extend_from_slice(&value.to_be_bytes()[1..]);
}

/// Append `value` in the reftable varint encoding, which is the offset encoding of Git packs.
pub(crate) fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    let mut buf = [0u8; 10];
    let mut pos = buf.len() - 1;
    buf[pos] = (value & 0x7f) as u8;
    value >>= 7;
    while value != 0 {
        value -= 1;
        pos -= 1;
        buf[pos] = 0x80 | (value & 0x7f) as u8;
        value >>= 7;
    }
    out.extend_from_slice(&buf[pos..]);
}

/// Decode a varint from the start of `input`, returning it and the number of bytes consumed.
pub(crate) fn get_varint(input: &[u8]) -> Result<(u64, usize)> {
    let mut bytes = input.iter().copied().enumerate();
    let (_, first) = bytes
        .next()
        .ok_or_else(|| corrupt("data ends inside a variable-length integer"))?;
    let mut value = u64::from(first & 0x7f);
    let mut more = first & 0x80 != 0;
    let mut consumed = 1;
    while more {
        let (idx, byte) = bytes
            .next()
            .ok_or_else(|| corrupt("data ends inside a variable-length integer"))?;
        // `value + 1` shifted by 7 must not lose bits.
        if value >= (u64::MAX >> 7) {
            return Err(corrupt("variable-length integer does not fit 64 bits"));
        }
        value = ((value + 1) << 7) | u64::from(byte & 0x7f);
        more = byte & 0x80 != 0;
        consumed = idx + 1;
    }
    Ok((value, consumed))
}

/// Append a varint length followed by `bytes`.
pub(crate) fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// Read a varint-prefixed byte string at `*cursor`, advancing it.
pub(crate) fn get_string<'a>(input: &'a [u8], cursor: &mut usize) -> Result<&'a [u8]> {
    let (len, consumed) = get_varint(input.get(*cursor..).unwrap_or_default())?;
    *cursor += consumed;
    let len = usize::try_from(len).map_err(|_| corrupt("string length does not fit memory"))?;
    let bytes = take(input, *cursor, len)?;
    *cursor += len;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips_at_encoding_boundaries() {
        for value in [
            0,
            1,
            126,
            127,
            128,
            129,
            16_383,
            16_384,
            16_511,
            16_512,
            u64::from(u32::MAX),
            u64::MAX >> 1,
            u64::MAX - 1,
            u64::MAX,
        ] {
            let mut buf = Vec::new();
            put_varint(&mut buf, value);
            assert_eq!(get_varint(&buf).expect("valid"), (value, buf.len()), "value {value}");
        }
    }

    #[test]
    fn varint_matches_git_offset_encoding() {
        let mut buf = Vec::new();
        put_varint(&mut buf, 128);
        assert_eq!(
            buf,
            [0x80, 0x00],
            "128 is two bytes with the +1 bias of ofs-delta encoding"
        );
        buf.clear();
        put_varint(&mut buf, 16_511);
        assert_eq!(buf, [0xff, 0x7f], "the largest value of two bytes");
        buf.clear();
        put_varint(&mut buf, 16_512);
        assert_eq!(buf, [0x80, 0x80, 0x00], "the smallest value of three bytes");
    }

    #[test]
    fn truncated_and_overlong_varints_are_corruption() {
        assert!(get_varint(&[]).expect_err("empty").is_corrupted());
        assert!(
            get_varint(&[0x80])
                .expect_err("continuation without data")
                .is_corrupted()
        );
        assert!(
            get_varint(&[0xff; 11])
                .expect_err("too many continuation bytes")
                .is_corrupted(),
            "values above 64 bits are rejected instead of silently wrapping"
        );
    }

    #[test]
    fn strings_are_length_prefixed_and_bounds_checked() {
        let mut buf = Vec::new();
        put_string(&mut buf, b"abc");
        let mut cursor = 0;
        assert_eq!(get_string(&buf, &mut cursor).expect("valid"), b"abc");
        assert_eq!(cursor, 4);

        let mut cursor = 0;
        assert!(
            get_string(&buf[..3], &mut cursor)
                .expect_err("truncated")
                .is_corrupted(),
            "a length larger than the remaining data must not panic"
        );
    }

    #[test]
    fn be24_reads_three_bytes() {
        assert_eq!(be24(&[1, 2, 3], 0).expect("valid"), 0x01_02_03);
        assert!(be24(&[1, 2], 0).is_err());
    }
}
