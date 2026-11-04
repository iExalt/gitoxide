//! The records stored in reftable blocks and their byte encodings.

use std::cmp::Ordering;

use bstr::BString;
use gix_error::Result;
use gix_hash::ObjectId;

use crate::basics::{self, common_prefix_size, corrupt, get_string, get_varint, put_string, put_varint, take};

/// The type of a block, which is also the type of all records within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BlockType {
    /// Reference records, keyed by reference name.
    Ref = b'r',
    /// Object index records, keyed by an object id prefix.
    Obj = b'o',
    /// Reflog records, keyed by reference name and reversed update index.
    Log = b'g',
    /// Index records pointing at other blocks, keyed by the last key of the block they point at.
    Index = b'i',
}

impl BlockType {
    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            b'r' => Self::Ref,
            b'o' => Self::Obj,
            b'g' => Self::Log,
            b'i' => Self::Index,
            _ => return None,
        })
    }
}

const REF_VAL_DELETION: u8 = 0;
const REF_VAL_VAL1: u8 = 1;
const REF_VAL_VAL2: u8 = 2;
const REF_VAL_SYMREF: u8 = 3;
const LOG_VAL_DELETION: u8 = 0;
const LOG_VAL_UPDATE: u8 = 1;

/// The value of a [`RefRecord`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefValue {
    /// A tombstone which hides the reference in older tables of a stack.
    Deletion,
    /// A direct reference.
    Val1(ObjectId),
    /// A direct reference to an annotated tag, along with the object it ultimately peels to.
    Val2 {
        /// The id of the tag object.
        value: ObjectId,
        /// The id of the object the tag peels to.
        target_value: ObjectId,
    },
    /// A symbolic reference holding the full name of its target.
    Symref(BString),
}

/// A reference stored in a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefRecord {
    /// The full name of the reference, which is not necessarily valid UTF-8.
    pub refname: BString,
    /// The update index of the transaction which wrote this record, which is within the table's limits.
    pub update_index: u64,
    /// What the reference points at, or a tombstone.
    pub value: RefValue,
}

/// The value of a [`LogRecord`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogValue {
    /// A tombstone which hides the log record with the same key in older tables of a stack.
    Deletion,
    /// A reflog entry.
    Update {
        /// The value of the reference before the update.
        old_hash: ObjectId,
        /// The value of the reference after the update.
        new_hash: ObjectId,
        /// The name of the committer.
        name: BString,
        /// The email address of the committer.
        email: BString,
        /// The time of the update in seconds since the epoch.
        time: u64,
        /// The offset of the committer's timezone to UTC as `±hhmm`, exactly as it is in commits, so `-830` is UTC-08:30.
        tz_offset: i16,
        /// The message of the entry, which usually ends in a newline.
        message: BString,
    },
}

/// A reflog entry stored in a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// The full name of the reference, which is not necessarily valid UTF-8.
    pub refname: BString,
    /// The update index of the transaction that made the entry. Together with the name it is the key.
    pub update_index: u64,
    /// The entry itself, or a tombstone.
    pub value: LogValue,
}

/// An entry of the object index, which maps object id prefixes to the ref blocks that mention them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjRecord {
    /// The leading bytes of the object ids the offsets belong to.
    pub hash_prefix: Vec<u8>,
    /// The absolute file offsets of the ref blocks, in ascending order.
    pub offsets: Vec<u64>,
}

/// An entry of a block index, pointing at the block whose last key it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRecord {
    /// The last key of the block at `offset`.
    pub last_key: Vec<u8>,
    /// The absolute file offset of the block.
    pub offset: u64,
}

/// Any record that can be stored in a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// See [`RefRecord`].
    Ref(RefRecord),
    /// See [`LogRecord`].
    Log(LogRecord),
    /// See [`ObjRecord`].
    Obj(ObjRecord),
    /// See [`IndexRecord`].
    Index(IndexRecord),
}

impl Record {
    /// The type of the block this record is stored in.
    pub fn block_type(&self) -> BlockType {
        match self {
            Record::Ref(_) => BlockType::Ref,
            Record::Log(_) => BlockType::Log,
            Record::Obj(_) => BlockType::Obj,
            Record::Index(_) => BlockType::Index,
        }
    }

    /// Whether this record hides an older one with the same key.
    pub fn is_deletion(&self) -> bool {
        matches!(
            self,
            Record::Ref(RefRecord {
                value: RefValue::Deletion,
                ..
            }) | Record::Log(LogRecord {
                value: LogValue::Deletion,
                ..
            })
        )
    }

    /// The bytes records are sorted by, and which the key encoding of a block compresses.
    pub fn key(&self) -> Vec<u8> {
        match self {
            Record::Ref(r) => r.refname.to_vec(),
            Record::Log(l) => log_key(&l.refname, l.update_index),
            Record::Obj(o) => o.hash_prefix.clone(),
            Record::Index(i) => i.last_key.clone(),
        }
    }

    /// Compare two records of the same type by key, or fail if their types differ.
    pub fn cmp_key(&self, other: &Self) -> Result<Ordering> {
        if self.block_type() != other.block_type() {
            return Err(basics::misuse("records of different types cannot be compared"));
        }
        Ok(self.key().cmp(&other.key()))
    }

    /// The 3-bit value type which is stored along the key.
    pub(crate) fn val_type(&self) -> u8 {
        match self {
            Record::Ref(r) => match r.value {
                RefValue::Deletion => REF_VAL_DELETION,
                RefValue::Val1(_) => REF_VAL_VAL1,
                RefValue::Val2 { .. } => REF_VAL_VAL2,
                RefValue::Symref(_) => REF_VAL_SYMREF,
            },
            Record::Log(l) => match l.value {
                LogValue::Deletion => LOG_VAL_DELETION,
                LogValue::Update { .. } => LOG_VAL_UPDATE,
            },
            // Up to seven offsets are counted in the value type, otherwise it is zero and the count follows.
            Record::Obj(o) => u8::try_from(o.offsets.len())
                .ok()
                .filter(|n| (1..8).contains(n))
                .unwrap_or(0),
            Record::Index(_) => 0,
        }
    }

    /// Encode everything which follows the key.
    ///
    /// Reference records store their update index relative to the `min_update_index` of the table they are written to.
    pub(crate) fn encode_value(&self, min_update_index: u64) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(64);
        match self {
            Record::Ref(r) => {
                let delta = r
                    .update_index
                    .checked_sub(min_update_index)
                    .ok_or_else(|| basics::misuse("update index is below the minimum of the table"))?;
                put_varint(&mut out, delta);
                match &r.value {
                    RefValue::Deletion => {}
                    RefValue::Val1(id) => out.extend_from_slice(id.as_slice()),
                    RefValue::Val2 { value, target_value } => {
                        out.extend_from_slice(value.as_slice());
                        out.extend_from_slice(target_value.as_slice());
                    }
                    RefValue::Symref(target) => put_string(&mut out, target),
                }
            }
            Record::Log(l) => {
                if let LogValue::Update {
                    old_hash,
                    new_hash,
                    name,
                    email,
                    time,
                    tz_offset,
                    message,
                } = &l.value
                {
                    out.extend_from_slice(old_hash.as_slice());
                    out.extend_from_slice(new_hash.as_slice());
                    put_string(&mut out, name);
                    put_string(&mut out, email);
                    put_varint(&mut out, *time);
                    out.extend_from_slice(&tz_offset.to_be_bytes());
                    put_string(&mut out, message);
                }
            }
            Record::Obj(o) => {
                if o.offsets.is_empty() || o.offsets.len() >= 8 {
                    put_varint(&mut out, o.offsets.len() as u64);
                }
                let mut last = 0;
                for (idx, offset) in o.offsets.iter().copied().enumerate() {
                    let delta = if idx == 0 {
                        offset
                    } else {
                        offset
                            .checked_sub(last)
                            .ok_or_else(|| basics::misuse("object index offsets must be ascending"))?
                    };
                    put_varint(&mut out, delta);
                    last = offset;
                }
            }
            Record::Index(i) => put_varint(&mut out, i.offset),
        }
        Ok(out)
    }

    /// Decode a record from its `key`, 3-bit `val_type` and the `payload` that starts right after the key.
    ///
    /// Returns the record and the amount of payload bytes it occupied. `hash_size` is the size of the table's object ids.
    pub(crate) fn decode(
        block_type: BlockType,
        key: &[u8],
        val_type: u8,
        payload: &[u8],
        hash_size: usize,
    ) -> Result<(Self, usize)> {
        match block_type {
            BlockType::Ref => decode_ref(key, val_type, payload, hash_size).map(|(r, n)| (Record::Ref(r), n)),
            BlockType::Log => decode_log(key, val_type, payload, hash_size).map(|(r, n)| (Record::Log(r), n)),
            BlockType::Obj => decode_obj(key, val_type, payload).map(|(r, n)| (Record::Obj(r), n)),
            BlockType::Index => {
                let (offset, consumed) = get_varint(payload)?;
                Ok((
                    Record::Index(IndexRecord {
                        last_key: key.to_vec(),
                        offset,
                    }),
                    consumed,
                ))
            }
        }
    }
}

/// The key of a reflog record, which sorts the records of one reference with the newest first.
///
/// Seek to it to find the entry with `update_index` of the reference `refname` in a log iterator.
pub fn log_key(refname: &[u8], update_index: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(refname.len() + 9);
    key.extend_from_slice(refname);
    key.push(0);
    key.extend_from_slice(&(u64::MAX - update_index).to_be_bytes());
    key
}

/// Encode a key relative to `prev_key`, with `extra` as the 3-bit value type.
///
/// Returns whether the key starts a new restart point, which is the case if nothing is shared with `prev_key`.
pub(crate) fn encode_key(out: &mut Vec<u8>, prev_key: &[u8], key: &[u8], extra: u8) -> bool {
    debug_assert!(extra < 8, "the value type has three bits");
    let prefix_len = common_prefix_size(prev_key, key);
    put_varint(out, prefix_len as u64);
    put_varint(out, (((key.len() - prefix_len) as u64) << 3) | u64::from(extra));
    out.extend_from_slice(&key[prefix_len..]);
    prefix_len == 0
}

/// Decode a key which is relative to the one in `last_key` into that buffer.
///
/// Returns the amount of bytes consumed from `input` and the 3-bit value type.
pub(crate) fn decode_key(last_key: &mut Vec<u8>, input: &[u8]) -> Result<(usize, u8)> {
    let (prefix_len, suffix_len, extra, mut consumed) = decode_key_len(input)?;
    if prefix_len > last_key.len() {
        return Err(corrupt("key shares more bytes with the previous key than it has"));
    }
    let suffix = take(input, consumed, suffix_len)?;
    last_key.truncate(prefix_len);
    last_key.extend_from_slice(suffix);
    consumed += suffix_len;
    Ok((consumed, extra))
}

/// Decode the lengths at the start of a key: shared prefix length, suffix length, value type and header size.
pub(crate) fn decode_key_len(input: &[u8]) -> Result<(usize, usize, u8, usize)> {
    let (prefix_len, mut consumed) = get_varint(input)?;
    let (suffix_and_extra, n) = get_varint(input.get(consumed..).unwrap_or_default())?;
    consumed += n;
    let prefix_len = usize::try_from(prefix_len).map_err(|_| corrupt("key prefix length does not fit memory"))?;
    let suffix_len =
        usize::try_from(suffix_and_extra >> 3).map_err(|_| corrupt("key suffix length does not fit memory"))?;
    Ok((prefix_len, suffix_len, (suffix_and_extra & 7) as u8, consumed))
}

fn decode_id(payload: &[u8], cursor: &mut usize, hash_size: usize) -> Result<ObjectId> {
    let bytes = take(payload, *cursor, hash_size)?;
    *cursor += hash_size;
    ObjectId::try_from(bytes).map_err(|_| basics::corrupt("object id has a size no enabled hash produces"))
}

fn decode_ref(key: &[u8], val_type: u8, payload: &[u8], hash_size: usize) -> Result<(RefRecord, usize)> {
    let (update_index, mut cursor) = get_varint(payload)?;
    let value = match val_type {
        REF_VAL_DELETION => RefValue::Deletion,
        REF_VAL_VAL1 => RefValue::Val1(decode_id(payload, &mut cursor, hash_size)?),
        REF_VAL_VAL2 => RefValue::Val2 {
            value: decode_id(payload, &mut cursor, hash_size)?,
            target_value: decode_id(payload, &mut cursor, hash_size)?,
        },
        REF_VAL_SYMREF => RefValue::Symref(get_string(payload, &mut cursor)?.into()),
        _ => return Err(corrupt("unknown reference value type")),
    };
    Ok((
        RefRecord {
            refname: key.into(),
            update_index,
            value,
        },
        cursor,
    ))
}

fn decode_log(key: &[u8], val_type: u8, payload: &[u8], hash_size: usize) -> Result<(LogRecord, usize)> {
    let name_len = key
        .len()
        .checked_sub(9)
        .filter(|len| *len > 0 && key[*len] == 0)
        .ok_or_else(|| corrupt("log key is not a reference name, a NUL byte and a reversed update index"))?;
    let reversed_index = basics::be64(key, name_len + 1)?;
    let (value, consumed) = match val_type {
        LOG_VAL_DELETION => (LogValue::Deletion, 0),
        LOG_VAL_UPDATE => {
            let mut cursor = 0;
            let old_hash = decode_id(payload, &mut cursor, hash_size)?;
            let new_hash = decode_id(payload, &mut cursor, hash_size)?;
            let name = get_string(payload, &mut cursor)?.into();
            let email = get_string(payload, &mut cursor)?.into();
            let (time, n) = get_varint(payload.get(cursor..).unwrap_or_default())?;
            cursor += n;
            let tz_offset = basics::be16(payload, cursor)? as i16;
            cursor += 2;
            let message = get_string(payload, &mut cursor)?.into();
            (
                LogValue::Update {
                    old_hash,
                    new_hash,
                    name,
                    email,
                    time,
                    tz_offset,
                    message,
                },
                cursor,
            )
        }
        _ => return Err(corrupt("unknown log value type")),
    };
    Ok((
        LogRecord {
            refname: key[..name_len].into(),
            update_index: u64::MAX - reversed_index,
            value,
        },
        consumed,
    ))
}

fn decode_obj(key: &[u8], val_type: u8, payload: &[u8]) -> Result<(ObjRecord, usize)> {
    let mut cursor = 0;
    let count = if val_type == 0 {
        let (count, n) = get_varint(payload)?;
        cursor += n;
        count
    } else {
        u64::from(val_type)
    };
    // Every offset takes at least one byte, which bounds the allocation by the data that is actually there.
    let count = usize::try_from(count)
        .ok()
        .filter(|count| *count <= payload.len().saturating_sub(cursor))
        .ok_or_else(|| corrupt("object index record claims more offsets than it has data for"))?;
    let mut offsets = Vec::with_capacity(count);
    let mut last = 0u64;
    for idx in 0..count {
        let (delta, n) = get_varint(payload.get(cursor..).unwrap_or_default())?;
        cursor += n;
        last = if idx == 0 {
            delta
        } else {
            last.checked_add(delta)
                .ok_or_else(|| corrupt("object index offsets overflow"))?
        };
        offsets.push(last);
    }
    Ok((
        ObjRecord {
            hash_prefix: key.to_vec(),
            offsets,
        },
        cursor,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(seed: u8, size: usize) -> ObjectId {
        let bytes: Vec<u8> = (0..size).map(|i| seed.wrapping_add(i as u8)).collect();
        ObjectId::try_from(bytes.as_slice()).expect("both hashes are enabled in tests")
    }

    fn round_trip(record: &Record, hash_size: usize) {
        let value = record.encode_value(0).expect("encodes");
        let (decoded, consumed) =
            Record::decode(record.block_type(), &record.key(), record.val_type(), &value, hash_size).expect("decodes");
        assert_eq!(consumed, value.len(), "all bytes of the value are consumed");
        // Ref records store the update index as delta to the table, which the table reader resolves, so it round-trips as is here.
        assert_eq!(&decoded, record);
    }

    #[test]
    fn all_record_kinds_round_trip_for_both_hashes() {
        for hash_size in [20, 32] {
            for record in [
                Record::Ref(RefRecord {
                    refname: "refs/heads/main".into(),
                    update_index: 1,
                    value: RefValue::Deletion,
                }),
                Record::Ref(RefRecord {
                    refname: "refs/heads/main".into(),
                    update_index: 2,
                    value: RefValue::Val1(id(1, hash_size)),
                }),
                Record::Ref(RefRecord {
                    refname: "refs/tags/v1".into(),
                    update_index: 3,
                    value: RefValue::Val2 {
                        value: id(2, hash_size),
                        target_value: id(3, hash_size),
                    },
                }),
                Record::Ref(RefRecord {
                    refname: "HEAD".into(),
                    update_index: 4,
                    value: RefValue::Symref("refs/heads/main".into()),
                }),
                Record::Ref(RefRecord {
                    refname: b"refs/heads/caf\xe9".as_slice().into(),
                    update_index: 5,
                    value: RefValue::Val1(id(4, hash_size)),
                }),
                Record::Log(LogRecord {
                    refname: "refs/heads/main".into(),
                    update_index: 5,
                    value: LogValue::Deletion,
                }),
                Record::Log(LogRecord {
                    refname: b"refs/heads/caf\xe9".as_slice().into(),
                    update_index: u64::MAX - 1,
                    value: LogValue::Update {
                        old_hash: id(10, hash_size),
                        new_hash: id(20, hash_size),
                        name: b"Z\xf6e".as_slice().into(),
                        email: "zoe@example.com".into(),
                        time: 1_577_123_507,
                        tz_offset: -510,
                        message: "multi\nline is stored verbatim\n".into(),
                    },
                }),
                Record::Obj(ObjRecord {
                    hash_prefix: vec![1, 2, 3, 4, 0],
                    offsets: vec![],
                }),
                Record::Obj(ObjRecord {
                    hash_prefix: vec![1, 2, 3, 4, 0],
                    offsets: vec![1, 2, 3],
                }),
                Record::Obj(ObjRecord {
                    hash_prefix: vec![1, 2, 3, 4, 0],
                    offsets: vec![1, 2, 3, 4, 500, 600, 700, 800, 9_000],
                }),
                Record::Index(IndexRecord {
                    last_key: b"refs/heads/main".to_vec(),
                    offset: 42,
                }),
            ] {
                round_trip(&record, hash_size);
            }
        }
    }

    #[test]
    fn keys_share_prefixes_and_carry_the_value_type() {
        let mut encoded = Vec::new();
        let restart = encode_key(&mut encoded, b"refs/heads/master", b"refs/tags/v1.0", 6);
        assert!(!restart, "a key sharing a prefix is no restart point");
        let mut key = b"refs/heads/master".to_vec();
        let (consumed, extra) = decode_key(&mut key, &encoded).expect("valid");
        assert_eq!(
            (consumed, extra, key.as_slice()),
            (encoded.len(), 6, b"refs/tags/v1.0".as_slice())
        );

        let mut encoded = Vec::new();
        assert!(
            encode_key(&mut encoded, b"abc", b"xyz", 0),
            "nothing shared means restart"
        );
    }

    #[test]
    fn log_records_sort_newest_first_within_a_reference() {
        let log = |name: &str, index| {
            Record::Log(LogRecord {
                refname: name.into(),
                update_index: index,
                value: LogValue::Deletion,
            })
        };
        assert_eq!(log("a", 42).cmp_key(&log("a", 22)).expect("same type"), Ordering::Less);
        assert_eq!(log("a", 1).cmp_key(&log("b", 9)).expect("same type"), Ordering::Less);
        assert!(
            log("a", 1)
                .cmp_key(&Record::Index(IndexRecord {
                    last_key: vec![],
                    offset: 0
                }))
                .is_err()
        );
    }

    #[test]
    fn malformed_records_are_corruption_and_never_panic() {
        let key = log_key(b"refs/heads/a", 1);
        for (what, result) in [
            ("short key", Record::decode(BlockType::Log, b"abc", 1, &[], 20)),
            ("missing hashes", Record::decode(BlockType::Log, &key, 1, &[0; 10], 20)),
            ("unknown ref type", Record::decode(BlockType::Ref, b"a", 7, &[0], 20)),
            (
                "offset count beyond the data",
                Record::decode(BlockType::Obj, b"a", 0, &[0x7f, 1], 20),
            ),
        ] {
            assert!(result.expect_err(what).is_corrupted(), "{what}");
        }
    }
}
