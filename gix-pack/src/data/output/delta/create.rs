//! Create the delta that turns one buffer into another, in the format of the entries of a pack.
//!
//! A delta starts with the size of the source and the size of the target, each as a little-endian base-128 number, followed by
//! instructions: *copy* a range of the source, or *insert* up to 127 bytes that the delta carries itself.
//!
//! Finding what to copy works with a rolling hash. The source is indexed in blocks of `BLOCK` bytes that start at multiples of
//! that size, so the index is small. The target is scanned byte by byte for a window, the last `BLOCK` bytes, that has the hash
//! of one of them, which like in Git can reach back into what the previous copy took, so that a copy can start right after a
//! change. A match is extended forward as far as the two agree and backward into what would otherwise be inserted, and the
//! longest candidate wins. This finds the same kind of delta as Git's does, but it isn't the same algorithm and doesn't make the
//! same deltas.

/// The size of the blocks that are indexed, which is also the least that a copy is worth.
const BLOCK: usize = 16;
/// How many places with the same hash are remembered, which limits the time spent on repetitive data.
const BUCKET_LIMIT: usize = 64;
/// The largest range that a single copy instruction can name, where a size of `0` stands for this value.
const MAX_COPY: usize = 0x10000;
/// The least that a copy is worth, which is as much as Git takes.
const MIN_COPY: usize = 4;
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

/// The index of a source buffer, which is what a delta is made against. It can be used for any number of targets.
#[derive(Debug, Clone)]
pub struct Index {
    shift: u32,
    /// For each bucket, where its positions begin in `positions`, with one more at the end.
    starts: Vec<u32>,
    positions: Vec<u32>,
}

impl Index {
    /// Index `source`. One that is 4 GiB or larger has no deltas, so its index is empty.
    pub fn new(source: &[u8]) -> Index {
        let blocks = if u32::try_from(source.len()).is_ok() {
            source.len() / BLOCK
        } else {
            0
        };
        let bits = blocks.max(1).bit_width().clamp(4, 28);
        let shift = 32 - bits;
        let buckets = 1usize << bits;
        let mut counts = vec![0u32; buckets + 1];
        let hashes: Vec<u32> = (0..blocks)
            .map(|block| hash_block(&source[block * BLOCK..(block + 1) * BLOCK]))
            .collect();
        for hash in &hashes {
            let bucket = Self::bucket_of(*hash, shift);
            if (counts[bucket] as usize) < BUCKET_LIMIT {
                counts[bucket] += 1;
            }
        }
        let mut starts = vec![0u32; buckets + 1];
        for bucket in 0..buckets {
            starts[bucket + 1] = starts[bucket] + counts[bucket];
        }
        let mut positions = vec![0u32; starts[buckets] as usize];
        let mut filled = vec![0u32; buckets];
        // Later blocks first, as the end of a buffer is where what's new tends to be.
        for (block, hash) in hashes.iter().enumerate().rev() {
            let bucket = Self::bucket_of(*hash, shift);
            if filled[bucket] < counts[bucket] {
                positions[(starts[bucket] + filled[bucket]) as usize] = (block * BLOCK) as u32;
                filled[bucket] += 1;
            }
        }
        Index {
            shift,
            starts,
            positions,
        }
    }

    /// The memory the index occupies, in bytes.
    pub fn memory_size(&self) -> usize {
        (self.starts.len() + self.positions.len()) * std::mem::size_of::<u32>()
    }

    fn bucket_of(hash: u32, shift: u32) -> usize {
        (hash.wrapping_mul(0x9E37_79B1) >> shift) as usize
    }

    fn candidates(&self, hash: u32) -> &[u32] {
        let bucket = Self::bucket_of(hash, self.shift);
        &self.positions[self.starts[bucket] as usize..self.starts[bucket + 1] as usize]
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
        let (now, later) = data.split_at(data.len().min(127));
        out.push(now.len() as u8);
        out.extend_from_slice(now);
        data = later;
    }
}

fn push_copy(out: &mut Vec<u8>, mut offset: usize, mut size: usize) {
    while size > 0 {
        let now = size.min(MAX_COPY);
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
        if now != MAX_COPY {
            for byte in 0..2 {
                let part = ((now >> (8 * byte)) & 0xff) as u8;
                if part != 0 {
                    out[command_at] |= 0x10 << byte;
                    out.push(part);
                }
            }
        }
        offset += now;
        size -= now;
    }
}

/// Write the delta that makes `target` out of `source`, for which `index` was made, into `out`, which is cleared first.
///
/// Return `false` and leave `out` unspecified if the delta would be larger than `max_size`, or if `source` or `target` are
/// too large to be described, which is at 4 GiB.
pub fn create(source: &[u8], index: &Index, target: &[u8], max_size: Option<usize>, out: &mut Vec<u8>) -> bool {
    out.clear();
    if u32::try_from(source.len()).is_err() || u32::try_from(target.len()).is_err() {
        return false;
    }
    let too_large = |out: &Vec<u8>| max_size.is_some_and(|max| out.len() > max);
    push_size(out, source.len());
    push_size(out, target.len());

    let leaving = leaving_factor();
    // The window is the `BLOCK` bytes before `end`, and what comes before `pending_from` is dealt with. Like Git does, look for
    // the window among the blocks of the source even when it reaches back into what a copy just took, so that a copy can start
    // right after a change.
    let mut pending_from = 0;
    let mut end = BLOCK;
    if target.len() < BLOCK {
        push_inserts(out, target);
        return !too_large(out);
    }
    let mut hash = hash_block(&target[..BLOCK]);
    loop {
        let mut best: Option<(usize, usize, usize)> = None; // where the source continues, bytes forward, bytes backward
        for &candidate in index.candidates(hash) {
            let source_end = candidate as usize + BLOCK;
            // An index that doesn't belong to the source has positions that aren't in it, which is a mistake that finds no delta.
            if source_end > source.len() {
                continue;
            }
            let mut forward = 0;
            while source_end + forward < source.len()
                && end + forward < target.len()
                && source[source_end + forward] == target[end + forward]
            {
                forward += 1;
            }
            let mut backward = 0;
            while backward < end - pending_from
                && backward < source_end
                && source[source_end - 1 - backward] == target[end - 1 - backward]
            {
                backward += 1;
            }
            if best.is_none_or(|(_, f, b)| forward + backward > f + b) {
                best = Some((source_end, forward, backward));
            }
        }
        match best {
            Some((source_end, forward, backward)) if forward + backward >= MIN_COPY => {
                push_inserts(out, &target[pending_from..end - backward]);
                push_copy(out, source_end - backward, forward + backward);
                end += forward;
                pending_from = end;
                hash = hash_block(&target[end - BLOCK..end]);
                if too_large(out) {
                    return false;
                }
            }
            _ => {
                if end == target.len() {
                    break;
                }
                hash = hash
                    .wrapping_sub((u32::from(target[end - BLOCK]) + 1).wrapping_mul(leaving))
                    .wrapping_mul(MULTIPLIER)
                    .wrapping_add(u32::from(target[end]) + 1);
                end += 1;
                if out.len() + (end - pending_from) > max_size.unwrap_or(usize::MAX) {
                    return false;
                }
            }
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
}
