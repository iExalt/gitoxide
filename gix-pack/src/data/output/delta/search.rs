//! Find the objects of a pack that are cheapest to store as the delta of another one, with a sliding window as Git does.
//!
//! The objects are sorted by kind, then by the hash of the name they had in the tree that they were found in, then by size, and each
//! one is tried as the delta of the objects that came before it within a window of a few entries. Sorting by name puts the versions
//! of a file next to each other, and sorting by size makes the larger and usually newer version the base, so that it is the one
//! that is stored whole and the one that is read most often.
//!
//! How much a delta may be worth is decided like in Git: it has to be smaller than half the size of the object minus a little
//! for the header, which is made stricter by the depth of the base so that chains stay short, and objects that are tiny or much
//! larger than their base aren't tried.
use std::{collections::VecDeque, sync::atomic::AtomicBool};

use gix_error::{Result, bail, message};
use gix_hash::ObjectId;

use super::create;

/// An object that is considered for being stored as a delta, or as the base of one.
#[derive(Debug, Clone)]
pub struct Item {
    /// The id of the object.
    pub id: ObjectId,
    /// The kind of the object. Only objects of the same kind are deltas of each other.
    pub kind: gix_object::Kind,
    /// The size of the object once decompressed.
    pub size: u64,
    /// The hash of the path the object was found at, see [`name_hash()`], or `0` if it is not known. Objects that
    /// have a similar name end up next to each other, which is where the best deltas are.
    pub name_hash: u32,
    /// The delta that the object is stored as already, which is kept as it is. It still serves as base for other objects.
    pub existing: Option<Existing>,
    /// If `true`, the object won't be part of the pack, as the receiver of a thin pack has it already, so it is never made a
    /// delta but can be the base of one. Whoever writes the pack has to name the base by its id, which
    /// [`Entry::from_delta()`](crate::data::output::Entry::from_delta()) doesn't do, as it refers to entries that are written.
    pub preferred_base: bool,
}

/// A delta that exists already and is going to be reused.
#[derive(Debug, Clone, Copy)]
pub struct Existing {
    /// The index of the object that is the base of the delta, in the list of items.
    pub base: usize,
    /// The number of deltas that have to be applied to get the object, which is at least one.
    pub depth: u32,
}

/// A delta that was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    /// The index of the item that is the base.
    pub base: usize,
    /// The delta, with its size header and instructions. It is not compressed.
    pub data: Vec<u8>,
    /// The number of deltas that have to be applied to get the object.
    pub depth: u32,
}

/// Options for [`search()`].
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// How many objects before an object are tried as its base, which is `pack.window` and defaults to 10. `0` finds nothing.
    pub window: usize,
    /// The longest chain of deltas, which is `pack.depth` and defaults to 50. `0` finds nothing.
    pub depth: u32,
    /// The most memory that the objects of the window and their indices may take up, which is `pack.windowMemory`, or
    /// `None` for no limit. The window shrinks to stay below it, to a single object.
    pub window_memory: Option<u64>,
    /// Objects that are larger than this are neither made deltas nor used as bases, which is `core.bigFileThreshold` and
    /// defaults to 512 MiB, as such objects are expensive to load, and to index.
    pub max_object_size: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            window: 10,
            depth: 50,
            window_memory: None,
            max_object_size: 512 * 1024 * 1024,
        }
    }
}

/// The smallest object that is tried, as smaller ones have no room for a delta that is smaller.
const MIN_SIZE: u64 = 50;
/// What is taken off the size an object may have as delta, for the header of the entry.
const HEADER_ROOM: u64 = 20;

/// The hash of `path` that makes names with the same last characters, like those of the same file in other directories, similar.
/// Whitespace is left out, and only the last characters count for much.
pub fn name_hash(path: &[u8]) -> u32 {
    name_hash_extend(0, path)
}

/// Return the hash of the path that is the one with hash `hash` followed by `more`, so that the hash of a path can be had without
/// the path as long as its parts are known, as when walking trees.
pub fn name_hash_extend(hash: u32, more: &[u8]) -> u32 {
    more.iter()
        .filter(|byte| !byte.is_ascii_whitespace())
        .fold(hash, |hash, byte| (hash >> 2).wrapping_add(u32::from(*byte) << 24))
}

fn kind_rank(kind: gix_object::Kind) -> u8 {
    match kind {
        gix_object::Kind::Commit => 1,
        gix_object::Kind::Tree => 2,
        gix_object::Kind::Blob => 3,
        gix_object::Kind::Tag => 4,
    }
}

struct Entry {
    item: usize,
    data: Vec<u8>,
    index: Option<create::Index>,
    depth: u32,
}

impl Entry {
    fn memory(&self) -> u64 {
        (self.data.len() + self.index.as_ref().map_or(0, create::Index::memory_size)) as u64
    }
}

/// Find the deltas for `items`, in the order of `items`, with `None` for those that stay as they are. `read` is called with an
/// index into `items` and has to fill the buffer with the data of the object, whose size has to be the `size` of the item.
/// It is called once for each object that is tried, and once more for each that is only in the window as a base.
///
/// Objects of `items` whose [`existing`](Item::existing) delta is set are left out, as in Git: that delta is kept, and the
/// object is neither made another delta nor a base. What counts is the number of deltas that hang off an object like that, for
/// how deep the object can be made a delta itself, so no chain gets longer than `depth` and none can run in a circle.
/// The existing deltas have to be consistent, which is an error if they aren't: the base of one is another item, which has an
/// existing delta of a lower depth if it has any.
///
/// The deltas that were found refer to bases that are before them in no particular order, so a writer has to write bases first.
pub fn search(
    items: &[Item],
    mut read: impl FnMut(usize, &mut Vec<u8>) -> Result<()>,
    options: Options,
    should_interrupt: &AtomicBool,
) -> Result<Vec<Option<Delta>>> {
    let mut found: Vec<Option<Delta>> = vec![None; items.len()];
    if options.window == 0 || options.depth == 0 {
        return Ok(found);
    }
    // Depth and the height of what hangs off an object, for what exists already.
    let depth: Vec<u32> = items
        .iter()
        .map(|item| item.existing.map_or(0, |existing| existing.depth))
        .collect();
    let mut height = vec![0u32; items.len()];
    let mut by_depth: Vec<usize> = (0..items.len()).filter(|idx| items[*idx].existing.is_some()).collect();
    by_depth.sort_by_key(|idx| std::cmp::Reverse(depth[*idx]));
    for idx in by_depth {
        let existing = items[idx].existing.expect("selected for having an existing delta");
        if existing.base >= items.len() || existing.base == idx {
            bail!(message("The base of an existing delta is not another one of the items"));
        }
        if existing.depth == 0 || depth[existing.base] >= existing.depth {
            bail!(message(
                "The depth of an existing delta isn't more than that of its base, which would make for a cycle"
            ));
        }
        height[existing.base] = height[existing.base].max(height[idx].saturating_add(1));
    }

    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|a, b| {
        let (a_item, b_item) = (&items[*a], &items[*b]);
        kind_rank(b_item.kind)
            .cmp(&kind_rank(a_item.kind))
            .then(b_item.name_hash.cmp(&a_item.name_hash))
            .then(b_item.preferred_base.cmp(&a_item.preferred_base))
            .then(b_item.size.cmp(&a_item.size))
            .then(a.cmp(b))
    });

    let mut window: VecDeque<Entry> = VecDeque::with_capacity(options.window);
    let mut window_memory = 0u64;
    let mut delta_buf = Vec::new();
    let mut best_buf = Vec::new();
    for idx in order {
        if should_interrupt.load(std::sync::atomic::Ordering::Relaxed) {
            bail!(gix_error::cancelled("The search for deltas was interrupted"));
        }
        let item = &items[idx];
        if item.existing.is_some() || item.size > options.max_object_size {
            continue;
        }
        // The slot of the oldest object is the one that this object takes, as in Git, so that of a window of 10 nine are tried.
        while window.len() >= options.window {
            let gone = window.pop_front().expect("the window isn't empty");
            window_memory -= gone.memory();
        }
        let mut data = Vec::new();
        read(idx, &mut data)?;
        let searched = !item.preferred_base && item.size >= MIN_SIZE;
        let mut best: Option<(usize, u32)> = None; // position in window, depth of the result
        if searched {
            for (position, base) in window.iter_mut().enumerate().rev() {
                let base_item = &items[base.item];
                if base_item.kind != item.kind {
                    break;
                }
                // A chain that is built on top of the object has to fit, too.
                if base.depth.saturating_add(1).saturating_add(height[idx]) > options.depth {
                    continue;
                }
                let base_size = base.data.len() as u64;
                let target_size = data.len() as u64;
                if target_size < base_size / 32 {
                    continue;
                }
                // As in Git, what an object takes at most shrinks the deeper its base is, relative to the depth that the best so far has.
                let (mut max_size, reference_depth) = match &best {
                    Some((_, best_depth)) => (best_buf.len() as u64, *best_depth),
                    None => ((target_size / 2).saturating_sub(HEADER_ROOM), 1),
                };
                max_size =
                    max_size * u64::from(options.depth - base.depth) / u64::from(options.depth - reference_depth + 1);
                if max_size == 0 || base_size.saturating_sub(target_size) >= max_size {
                    continue;
                }
                if base.index.is_none() {
                    let index = create::Index::new(&base.data);
                    window_memory += index.memory_size() as u64;
                    base.index = Some(index);
                }
                let index = base.index.as_ref().expect("the index was made above");
                let limit = usize::try_from(max_size).unwrap_or(usize::MAX);
                if !create::create(&base.data, index, &data, Some(limit), &mut delta_buf) {
                    continue;
                }
                let new_depth = base.depth + 1;
                let better = match &best {
                    None => true,
                    Some((_, best_depth)) => {
                        delta_buf.len() < best_buf.len()
                            || (delta_buf.len() == best_buf.len() && new_depth < *best_depth)
                    }
                };
                if better {
                    std::mem::swap(&mut delta_buf, &mut best_buf);
                    best = Some((position, new_depth));
                }
            }
        }
        let mut entry_depth = 0;
        let mut best_position = None;
        if let Some((position, new_depth)) = best {
            best_position = Some(position);
            found[idx] = Some(Delta {
                base: window[position].item,
                data: std::mem::take(&mut best_buf),
                depth: new_depth,
            });
            entry_depth = new_depth;
        }
        let entry = Entry {
            item: idx,
            data,
            index: None,
            depth: entry_depth,
        };
        // An object that is at the depth limit can't be the base of anything, so it doesn't take up room in the window.
        if best_position.is_some() && entry_depth >= options.depth {
            continue;
        }
        window_memory += entry.memory();
        window.push_back(entry);
        // Like Git, keep the base that worked as the one that's tried first next, and that lives on longest.
        if let Some(position) = best_position {
            let base = window.remove(position).expect("the position is within the window");
            window.push_back(base);
        }
        while window.len() > options.window
            || (window.len() > 1 && options.window_memory.is_some_and(|limit| window_memory > limit))
        {
            let gone = window.pop_front().expect("the window isn't empty");
            window_memory -= gone.memory();
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::{fmt::Write as _, sync::atomic::AtomicBool};

    use gix_hash::ObjectId;

    use super::{Item, Options, name_hash, search};

    fn item(n: u8, size: usize, hash: u32) -> Item {
        let mut bytes = [0u8; 20];
        bytes[0] = n;
        Item {
            id: ObjectId::from_bytes_or_panic(&bytes),
            kind: gix_object::Kind::Blob,
            size: size as u64,
            name_hash: hash,
            existing: None,
            preferred_base: false,
        }
    }

    /// Versions of a text, each of which has one more line changed and one more appended than the one before, so that a version
    /// is closest to the ones next to it, and the longest is the last.
    fn versions(count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|version| {
                let mut text = String::new();
                for line in 0..300 {
                    if line % 10 == 0 && line / 10 <= version {
                        let _ = writeln!(text, "this line was changed in version {}", line / 10);
                    } else {
                        let _ = writeln!(text, "this is line number {line} of the file");
                    }
                }
                for edit in 0..=version {
                    let _ = writeln!(text, "added in version {edit}");
                }
                text.into_bytes()
            })
            .collect()
    }

    #[test]
    fn versions_of_a_file_become_a_chain_with_the_biggest_one_whole() {
        let data = versions(6);
        let items: Vec<_> = data
            .iter()
            .enumerate()
            .map(|(n, d)| item(n as u8, d.len(), 7))
            .collect();
        let found = search(
            &items,
            |idx, buf| {
                buf.clear();
                buf.extend_from_slice(&data[idx]);
                Ok(())
            },
            Options::default(),
            &AtomicBool::new(false),
        )
        .expect("searching works");
        assert!(found[5].is_none(), "the largest is the base of a chain and stays whole");
        for (idx, delta) in found.iter().enumerate().take(5) {
            let delta = delta.as_ref().unwrap_or_else(|| panic!("version {idx} is a delta"));
            assert!(
                delta.data.len() < data[idx].len() / 10,
                "the delta of version {idx} is tiny"
            );
        }
        assert_eq!(
            found.iter().flatten().map(|delta| delta.depth).max(),
            Some(5),
            "a chain, as each is the base of the next, which is closest"
        );
    }

    #[test]
    fn depth_and_window_limit_what_is_found() {
        let data = versions(8);
        let items: Vec<_> = data
            .iter()
            .enumerate()
            .map(|(n, d)| item(n as u8, d.len(), 7))
            .collect();
        let read = |idx: usize, buf: &mut Vec<u8>| {
            buf.clear();
            buf.extend_from_slice(&data[idx]);
            Ok(())
        };
        let depth_two = search(
            &items,
            read,
            Options {
                depth: 2,
                ..Default::default()
            },
            &AtomicBool::new(false),
        )
        .expect("works");
        assert!(
            depth_two.iter().flatten().all(|delta| delta.depth <= 2),
            "no chain is longer than the depth"
        );
        assert!(
            depth_two.iter().flatten().count() >= 4,
            "chains of two still cover many objects"
        );
        let none = search(
            &items,
            read,
            Options {
                window: 0,
                ..Default::default()
            },
            &AtomicBool::new(false),
        )
        .expect("works");
        assert!(
            none.iter().all(Option::is_none),
            "without a window there is nothing to find"
        );
        let tight = search(
            &items,
            read,
            Options {
                window_memory: Some(1),
                ..Default::default()
            },
            &AtomicBool::new(false),
        )
        .expect("works");
        assert!(
            tight.iter().flatten().count() >= 1,
            "a window of one object still finds the previous version"
        );
    }

    #[test]
    fn objects_of_different_kinds_or_names_far_apart_dont_mix_and_tiny_ones_stay_whole() {
        let data = versions(2);
        let mut items = vec![item(0, data[0].len(), 1), item(1, data[1].len(), 7)];
        items[1].kind = gix_object::Kind::Tree;
        let found = search(
            &items,
            |idx, buf| {
                buf.clear();
                buf.extend_from_slice(&data[idx]);
                Ok(())
            },
            Options::default(),
            &AtomicBool::new(false),
        )
        .expect("works");
        assert!(found.iter().all(Option::is_none), "a blob is not a delta of a tree");

        let tiny = [item(0, 10, 0), item(1, 10, 0)];
        let found = search(
            &tiny,
            |_, buf| {
                buf.clear();
                buf.extend_from_slice(b"0123456789");
                Ok(())
            },
            Options::default(),
            &AtomicBool::new(false),
        )
        .expect("works");
        assert!(
            found.iter().all(Option::is_none),
            "objects below the minimum size aren't tried"
        );
        assert_ne!(name_hash(b"dir/file.rs"), name_hash(b"dir/other.rs"));
        assert_eq!(
            name_hash(b"a/file.rs"),
            name_hash(b"a/ file.rs"),
            "whitespace doesn't count"
        );
    }

    fn read_from(data: &[Vec<u8>]) -> impl Fn(usize, &mut Vec<u8>) -> gix_error::Result<()> + Copy + '_ {
        move |idx, buf| {
            buf.clear();
            buf.extend_from_slice(&data[idx]);
            Ok(())
        }
    }

    /// The number of deltas to apply to get to an object, which fails if a delta chain runs in a circle.
    fn chain_length(items: &[Item], found: &[Option<super::Delta>], mut idx: usize) -> usize {
        let mut length = 0;
        loop {
            let base = match (&found[idx], items[idx].existing) {
                (Some(delta), _) => delta.base,
                (None, Some(existing)) => existing.base,
                (None, None) => return length,
            };
            length += 1;
            assert!(length <= items.len(), "a chain of deltas that runs in a circle");
            idx = base;
        }
    }

    #[test]
    fn a_delta_that_exists_can_not_become_the_base_of_its_own_base() {
        // The larger object is the delta of the smaller one, as after a file shrank, and sorts first for being larger.
        let data = [versions(1).remove(0), versions(3).remove(2)];
        let mut items = vec![item(0, data[0].len(), 7), item(1, data[1].len(), 7)];
        items[1].existing = Some(super::Existing { base: 0, depth: 1 });
        let found = search(&items, read_from(&data), Options::default(), &AtomicBool::new(false)).expect("works");
        assert!(
            found.iter().all(Option::is_none),
            "the base has nothing to be a delta of but its own delta"
        );
        assert_eq!(chain_length(&items, &found, 1), 1);
    }

    #[test]
    fn no_combination_of_existing_and_new_deltas_runs_in_a_circle_or_gets_too_deep() {
        let mut state = 99u64;
        let mut next = |below: usize| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as usize) % below
        };
        let data = versions(24);
        for round in 0..300 {
            let count = 2 + next(22);
            let mut items: Vec<Item> = (0..count)
                .map(|n| item(n as u8, data[n].len(), (next(2) * 7) as u32))
                .collect();
            // existing deltas, each against an earlier item, whichever its size
            for idx in 1..count {
                if next(3) == 0 {
                    let base = next(idx);
                    let depth = items[base].existing.map_or(1, |existing| existing.depth + 1);
                    items[idx].existing = Some(super::Existing { base, depth });
                }
            }
            let options = Options {
                depth: 1 + next(8) as u32,
                window: 1 + next(12),
                ..Default::default()
            };
            if items
                .iter()
                .any(|item| item.existing.is_some_and(|existing| existing.depth > options.depth))
            {
                continue;
            }
            let found = search(&items, read_from(&data), options, &AtomicBool::new(false)).expect("works");
            for idx in 0..count {
                assert!(
                    chain_length(&items, &found, idx) as u32 <= options.depth,
                    "round {round}: object {idx} is within the depth {} of what it was told, with {found:?}",
                    options.depth
                );
            }
        }
    }

    #[test]
    fn inconsistent_existing_deltas_are_refused() {
        let data = versions(2);
        for existing in [
            super::Existing { base: 0, depth: 1 },
            super::Existing { base: 5, depth: 1 },
            super::Existing { base: 1, depth: 0 },
        ] {
            let mut items = vec![item(0, data[0].len(), 7), item(1, data[1].len(), 7)];
            items[0].existing = Some(existing);
            assert!(
                search(&items, read_from(&data), Options::default(), &AtomicBool::new(false)).is_err(),
                "{existing:?} is a delta of itself, of nothing, or has no depth"
            );
        }
    }
}
