//! Create the delta that turns one buffer into another, in the format of the entries of a pack.
//!
//! A delta starts with the size of the source and the size of the target, each as a little-endian base-128 number, followed by
//! instructions: *copy* a range of the source, or *insert* up to 127 bytes that the delta carries itself.
//!
//! Finding what to copy works with a rolling hash, and makes the same choices as `git diff-delta` does, so that deltas come out
//! about as large. The source is indexed in blocks of `BLOCK` bytes that start at multiples of that size, plus one as the first byte
//! isn't part of any, so that the index is small. Of blocks that follow each other and are the same only the first is kept, and
//! if a hash is shared by more than `BUCKET_LIMIT` blocks, evenly spread ones are kept. The target is scanned byte by byte for a
//! window, the last `BLOCK` bytes, that has the hash of one of them. The first of the blocks that continue to agree for the
//! longest is the one that is copied from, which prefers the lowest offsets as those are the smallest to write. A copy is
//! extended backward into what would otherwise be inserted, and so a copy can start right after a change.

/// The size of the blocks that are indexed, which is also the size of the window that is hashed.
const BLOCK: usize = 16;
/// How many places with the same hash are remembered, which limits the time spent on repetitive data.
const BUCKET_LIMIT: usize = 64;
/// The largest range that a single copy instruction can name, where a size of `0` stands for this value.
const MAX_COPY: usize = 0x10000;
/// The least that a copy is worth, which is as much as Git takes.
const MIN_COPY: usize = 4;
/// Once this much matches, nothing is looked up until it is copied, as nothing could be much better.
const GOOD_ENOUGH: usize = 4096;
/// The longest run of bytes that a single insert instruction carries.
const MAX_INSERT: usize = 127;
const MULTIPLIER: u32 = 0x0100_0193;

fn hash_block(block: &[u8]) -> u32 {
    block.iter().fold(0u32, |hash, byte| {
        hash.wrapping_mul(MULTIPLIER).wrapping_add(u32::from(*byte) + 1)
    })
}

/// `MULTIPLIER` to the power of `BLOCK - 1`, which is what the byte that leaves the window is multiplied with.
fn leaving_factor() -> u32 {
    (1..BLOCK).fold(1u32, |factor, _| factor.wrapping_mul(MULTIPLIER))
}

/// The bucket of a hash. The low bits of a hash that is made by multiplying and adding only depend on the low bits of the
/// bytes, so the high bits are folded in, or data whose bytes have the same low bits would end up in few buckets.
fn bucket_of(hash: u32, mask: u32) -> usize {
    ((hash ^ (hash >> 16)) & mask) as usize
}

/// A block of the source that is indexed.
#[derive(Debug, Clone, Copy)]
struct Entry {
    hash: u32,
    /// The offset of the last byte of the block, which is where a window that ends in its last byte continues.
    last: u32,
}

/// The index of a source buffer, which is what a delta is made against. It can be used for any number of targets.
#[derive(Debug, Clone)]
pub struct Index {
    source_len: usize,
    mask: u32,
    /// For each bucket, where its entries begin in `entries`, with one more at the end. Entries of a bucket are ordered by
    /// their offset, lowest first.
    starts: Vec<u32>,
    entries: Vec<Entry>,
}

impl Index {
    /// Index `source`. One that is 4 GiB or larger has no deltas, so its index is empty.
    pub fn new(source: &[u8]) -> Index {
        let blocks = if u32::try_from(source.len()).is_ok() {
            source.len().saturating_sub(1) / BLOCK
        } else {
            0
        };
        let mut buckets = 16usize;
        while buckets < blocks / 4 {
            buckets *= 2;
        }
        let mask = (buckets - 1) as u32;

        // From the end, so that of identical blocks that follow each other the first one, which has the lowest offset, is left.
        let mut found: Vec<Entry> = Vec::with_capacity(blocks);
        for block in (0..blocks).rev() {
            let first = block * BLOCK + 1;
            let hash = hash_block(&source[first..first + BLOCK]);
            let last = (first + BLOCK - 1) as u32;
            match found.last_mut() {
                Some(previous) if previous.hash == hash => previous.last = last,
                _ => found.push(Entry { hash, last }),
            }
        }
        // They are in order of decreasing offsets now.
        found.reverse();

        // Where a hash is shared by more blocks than `BUCKET_LIMIT`, drop some so that those that stay are spread out evenly:
        // each block that stays earns `excess`, and each one that goes costs `BUCKET_LIMIT`, and while the balance is positive
        // blocks go. This leaves exactly `BUCKET_LIMIT`.
        let mut counts = vec![0i64; buckets];
        for entry in &found {
            counts[bucket_of(entry.hash, mask)] += 1;
        }
        let mut balance = vec![0i64; buckets];
        found.retain(|entry| {
            let bucket = bucket_of(entry.hash, mask);
            let excess = counts[bucket] - BUCKET_LIMIT as i64;
            if excess <= 0 {
                return true;
            }
            if balance[bucket] > 0 {
                balance[bucket] -= BUCKET_LIMIT as i64;
                false
            } else {
                balance[bucket] += excess;
                true
            }
        });

        // A stable sort into buckets keeps the entries of each ordered by offset.
        let mut starts = vec![0u32; buckets + 1];
        for entry in &found {
            starts[bucket_of(entry.hash, mask) + 1] += 1;
        }
        for bucket in 0..buckets {
            starts[bucket + 1] += starts[bucket];
        }
        let mut filled = starts.clone();
        let mut entries = vec![Entry { hash: 0, last: 0 }; found.len()];
        for entry in &found {
            let slot = &mut filled[bucket_of(entry.hash, mask)];
            entries[*slot as usize] = *entry;
            *slot += 1;
        }
        Index {
            source_len: source.len(),
            mask,
            starts,
            entries,
        }
    }

    /// The memory the index occupies, in bytes.
    pub fn memory_size(&self) -> usize {
        self.starts.len() * std::mem::size_of::<u32>() + self.entries.len() * std::mem::size_of::<Entry>()
    }

    fn candidates(&self, hash: u32) -> &[Entry] {
        let bucket = bucket_of(hash, self.mask);
        &self.entries[self.starts[bucket] as usize..self.starts[bucket + 1] as usize]
    }
}

fn push_size(out: &mut Vec<u8>, mut size: usize) {
    loop {
        let byte = (size & 0x7f) as u8;
        size >>= 7;
        if size == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn push_inserts(out: &mut Vec<u8>, mut data: &[u8]) {
    while !data.is_empty() {
        let (now, later) = data.split_at(data.len().min(MAX_INSERT));
        out.push(now.len() as u8);
        out.extend_from_slice(now);
        data = later;
    }
}

fn push_copy(out: &mut Vec<u8>, offset: usize, size: usize) {
    let command_at = out.len();
    out.push(0x80);
    for byte in 0..4 {
        let part = ((offset >> (8 * byte)) & 0xff) as u8;
        if part != 0 {
            out[command_at] |= 1 << byte;
            out.push(part);
        }
    }
    // A size of `MAX_COPY` is what no size bytes at all stand for.
    if size != MAX_COPY {
        for byte in 0..2 {
            let part = ((size >> (8 * byte)) & 0xff) as u8;
            if part != 0 {
                out[command_at] |= 0x10 << byte;
                out.push(part);
            }
        }
    }
}

/// Write the delta that makes `target` out of `source`, for which `index` was made, into `out`, which is cleared first.
///
/// Return `false` and leave `out` unspecified if the delta would be larger than `max_size`, if `index` is not the one of
/// `source`, or if `source` or `target` are
/// too large to be described, which is at 4 GiB.
pub fn create(source: &[u8], index: &Index, target: &[u8], max_size: Option<usize>, out: &mut Vec<u8>) -> bool {
    out.clear();
    // An index of another source would name places that aren't there.
    if source.len() != index.source_len || u32::try_from(source.len()).is_err() || u32::try_from(target.len()).is_err()
    {
        return false;
    }
    let too_large = |out: &Vec<u8>| max_size.is_some_and(|max| out.len() > max);
    push_size(out, source.len());
    push_size(out, target.len());
    if target.len() <= BLOCK {
        push_inserts(out, target);
        return !too_large(out);
    }

    let leaving = leaving_factor();
    // What is before `pending_from` is dealt with, and what is before `at` and not before `pending_from` is to be inserted
    // unless a copy that reaches back takes it. The window is the `BLOCK` bytes up to and including the one at `at`. The first
    // few bytes are never matched, as there is no window yet.
    let mut pending_from = 0;
    let mut at = BLOCK;
    let mut hash = hash_block(&target[..BLOCK]);
    // The size and the offset of what is to be copied, which continues where a copy of the largest size left off.
    let (mut copy_size, mut copy_from) = (0usize, 0usize);
    while at < target.len() {
        if copy_size < GOOD_ENOUGH {
            hash = hash
                .wrapping_sub((u32::from(target[at - BLOCK]) + 1).wrapping_mul(leaving))
                .wrapping_mul(MULTIPLIER)
                .wrapping_add(u32::from(target[at]) + 1);
            for entry in index.candidates(hash) {
                if entry.hash != hash {
                    continue;
                }
                let last = entry.last as usize;
                // Where it can't be longer than what is known, neither can what comes after it, which is closer to the end.
                if (source.len() - last).min(target.len() - at) <= copy_size {
                    break;
                }
                let size = source[last..]
                    .iter()
                    .zip(&target[at..])
                    .take_while(|(source_byte, target_byte)| source_byte == target_byte)
                    .count();
                if size > copy_size {
                    copy_size = size;
                    copy_from = last;
                    if copy_size >= GOOD_ENOUGH {
                        break;
                    }
                }
            }
        }

        if copy_size < MIN_COPY {
            at += 1;
            copy_size = 0;
            if at - pending_from == MAX_INSERT {
                push_inserts(out, &target[pending_from..at]);
                pending_from = at;
            }
            if too_large(out) || out.len() + (at - pending_from) > max_size.unwrap_or(usize::MAX) {
                return false;
            }
            continue;
        }

        // Reach back into what was going to be inserted, as far as source and target agree.
        while copy_from > 0 && at > pending_from && source[copy_from - 1] == target[at - 1] {
            copy_from -= 1;
            at -= 1;
            copy_size += 1;
        }
        push_inserts(out, &target[pending_from..at]);
        let now = copy_size.min(MAX_COPY);
        push_copy(out, copy_from, now);
        at += now;
        copy_from += now;
        copy_size -= now;
        pending_from = at;
        if copy_from > u32::MAX as usize {
            copy_size = 0;
        }
        if too_large(out) {
            return false;
        }
        if copy_size < GOOD_ENOUGH {
            if at >= target.len() {
                break;
            }
            hash = hash_block(&target[at - BLOCK..at]);
        }
    }
    push_inserts(out, &target[pending_from..]);
    !too_large(out)
}

#[cfg(test)]
mod tests {
    use super::{Index, create};

    /// Apply `delta`, which has its two sizes and instructions, to `source`.
    fn apply(source: &[u8], delta: &[u8]) -> Vec<u8> {
        let (source_size, consumed) = crate::data::delta::decode_header_size(delta).expect("a source size");
        assert_eq!(
            source_size as usize,
            source.len(),
            "the delta knows the size of its source"
        );
        let delta = &delta[consumed..];
        let (target_size, consumed) = crate::data::delta::decode_header_size(delta).expect("a target size");
        let mut target = vec![0; target_size as usize];
        crate::data::delta::apply(source, &mut target, &delta[consumed..]).expect("a valid delta");
        target
    }

    /// A deterministic source of numbers, so that tests don't need a dependency to be random.
    struct Numbers(u64);
    impl Numbers {
        fn next(&mut self, below: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % below.max(1)
        }
    }

    fn text(numbers: &mut Numbers, lines: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for line in 0..lines {
            out.extend_from_slice(format!("line {line} {}\n", numbers.next(1_000_000)).as_bytes());
        }
        out
    }

    #[test]
    fn what_is_created_applies_to_the_source_and_is_small_for_edits() {
        let mut numbers = Numbers(7);
        let mut out = Vec::new();
        for round in 0..200 {
            let lines = 1 + numbers.next(400);
            let source = text(&mut numbers, lines);
            let mut target = source.clone();
            for _ in 0..numbers.next(5) {
                let at = numbers.next(target.len());
                match numbers.next(3) {
                    0 => target.insert(at, b'x'),
                    1 => {
                        target.remove(at.min(target.len() - 1));
                    }
                    _ => target.splice(at..at, text(&mut numbers, 3)).for_each(drop),
                }
            }
            let index = Index::new(&source);
            assert!(
                create(&source, &index, &target, None, &mut out),
                "round {round}: without a limit it works"
            );
            assert_eq!(
                apply(&source, &out),
                target,
                "round {round}: the delta reproduces the target"
            );
            if source.len() > 2000 {
                assert!(
                    out.len() < target.len() / 2,
                    "round {round}: a few edits of a large text are far smaller than the text: {} of {}",
                    out.len(),
                    target.len()
                );
            }
        }
    }

    #[test]
    fn a_copy_starts_right_after_a_change_even_if_it_is_shorter_than_a_block() {
        let mut numbers = Numbers(11);
        let source = text(&mut numbers, 400);
        // Replace a few bytes in the middle of lines that are spread out, so that each change costs a copy before it, the few
        // bytes themselves, and then a copy again, without a need to wait for a whole block of what follows to match.
        let mut target = source.clone();
        let edits = 20;
        for edit in 0..edits {
            let at = 100 + edit * (target.len() - 200) / edits;
            target.splice(at..at + 3, *b"???");
        }
        let index = Index::new(&source);
        let mut out = Vec::new();
        assert!(create(&source, &index, &target, None, &mut out));
        assert_eq!(apply(&source, &out), target, "the delta reproduces the target");
        assert!(
            out.len() <= 10 + edits * 12,
            "each of the {edits} edits takes about as much as a copy and its few bytes, not more: {} bytes",
            out.len()
        );
    }

    #[test]
    fn unrelated_and_empty_buffers_work_and_the_size_limit_is_honoured() {
        let mut out = Vec::new();
        for (source, target) in [
            (&b""[..], &b""[..]),
            (b"", b"something"),
            (b"something", b""),
            (b"abc", b"abc"),
            (&[0u8; 200_000][..], &[0u8; 300_000][..]),
        ] {
            let index = Index::new(source);
            assert!(create(source, &index, target, None, &mut out));
            assert_eq!(apply(source, &out), target);
        }
        let source = vec![b'a'; 1000];
        let target: Vec<u8> = (0..1000).map(|n| (n % 251) as u8).collect();
        let index = Index::new(&source);
        assert!(
            !create(&source, &index, &target, Some(10), &mut out),
            "a delta that can't be small enough is given up on"
        );
    }

    #[test]
    fn of_equal_candidates_the_one_with_the_lowest_offset_is_copied_as_it_takes_the_fewest_bytes_to_name() {
        let mut numbers = Numbers(3);
        let copied = text(&mut numbers, 20);
        let filler = text(&mut numbers, 400);
        // The text is in the source twice, with enough in between to be a far offset, which needs more bytes to write.
        let source = [&copied[..], &filler, b"=== a separator ===", &copied].concat();
        let index = Index::new(&source);
        let mut out = Vec::new();
        assert!(create(&source, &index, &copied, None, &mut out));
        assert_eq!(apply(&source, &out), copied, "the delta reproduces the target");
        let size = copied.len();
        assert!(
            size > 255 && size < 0x10000 && size & 0xff != 0,
            "the size takes two bytes to name"
        );
        let copy = [0x80 | 0x10 | 0x20, (size & 0xff) as u8, (size >> 8) as u8];
        assert!(
            out.ends_with(&copy),
            "one copy from offset zero, which has no offset bytes at all, and no insert: {out:?}"
        );
    }

    #[test]
    fn identical_blocks_in_a_row_leave_the_first_and_a_crowded_hash_is_thinned_out_evenly() {
        let run = [b"-".as_slice(), &[b'a'; 16 * 10], b"-end of the run"].concat();
        let index = Index::new(&run);
        let hash = super::hash_block(&[b'a'; super::BLOCK]);
        assert_eq!(
            index
                .candidates(hash)
                .iter()
                .filter(|entry| entry.hash == hash)
                .map(|entry| entry.last)
                .collect::<Vec<_>>(),
            [16],
            "of ten blocks in a row only the one with the lowest offset is kept"
        );

        // A block that is in the source 100 times with something different between each, where blocks start at 1 + 16 * n.
        let mut numbers = Numbers(5);
        let block = *b"same block here!";
        let mut source = vec![b'^'];
        for _ in 0..100 {
            source.extend_from_slice(&block);
            source.extend_from_slice(&(numbers.next(1 << 30) as u128).to_le_bytes());
        }
        let hash = super::hash_block(&block);
        let index = Index::new(&source);
        let kept: Vec<_> = index
            .candidates(hash)
            .iter()
            .filter(|entry| entry.hash == hash)
            .map(|entry| entry.last)
            .collect();
        assert_eq!(
            kept.len(),
            super::BUCKET_LIMIT,
            "exactly the limit is left of a crowded hash"
        );
        assert_eq!(kept[0], 16, "the lowest offset is kept");
        assert!(
            kept.windows(2).all(|pair| (32..=64).contains(&(pair[1] - pair[0]))),
            "100 are thinned to 64 by dropping every third or so, not by cutting off one end: {kept:?}"
        );
    }

    #[test]
    fn a_copy_of_more_than_the_largest_size_is_split_where_it_must() {
        let mut numbers = Numbers(9);
        let source = text(&mut numbers, 20_000);
        assert!(source.len() > 3 * super::MAX_COPY, "the source takes several copies");
        let mut target = source.clone();
        target.splice(5..5, *b"x");
        let index = Index::new(&source);
        let mut out = Vec::new();
        assert!(create(&source, &index, &target, None, &mut out));
        assert_eq!(apply(&source, &out), target, "the delta reproduces the target");
        assert!(
            out.len() < 40,
            "an insert and a few copies, nothing more: {} bytes",
            out.len()
        );
    }

    #[test]
    fn data_whose_bytes_share_their_low_bits_is_spread_over_the_buckets_all_the_same() {
        let mut numbers = Numbers(13);
        // Four values with the same low six bits, so that without the high bits of the hash few buckets would be in use.
        let source: Vec<u8> = (0..64 * 1024).map(|_| (numbers.next(4) * 64) as u8).collect();
        let mut target = source.clone();
        let edits = 40;
        for edit in 0..edits {
            let at = (edit + 1) * (target.len() / (edits + 1));
            target[at] ^= 0x40;
        }
        let index = Index::new(&source);
        let mut out = Vec::new();
        assert!(create(&source, &index, &target, None, &mut out));
        assert_eq!(apply(&source, &out), target, "the delta reproduces the target");
        assert!(
            out.len() < edits * 12,
            "{edits} single-byte edits cost about a copy and an insert each (289 bytes), not far more as when most blocks are lost (793): {} bytes",
            out.len()
        );
    }

    #[test]
    fn an_index_of_another_source_gives_up_instead_of_naming_places_that_are_not_there() {
        let (long, short) = (vec![7u8; 1000], vec![7u8; 100]);
        let index = Index::new(&long);
        let mut out = Vec::new();
        assert!(!create(&short, &index, &short, None, &mut out));
    }
}
