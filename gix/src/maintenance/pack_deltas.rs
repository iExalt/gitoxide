//! Write a pack in which objects are stored as deltas of each other where that makes it smaller, for `repack`.
//!
//! Which objects are deltas is decided by [`gix_pack::data::output::delta::search`] before anything is written, from the
//! objects alone: it needs the kind and the size of each, and the data of those that are tried. The pack is then written
//! with every base before what is built on it, and only the data of deltas that were found is held in memory, as the
//! other objects are read when they are written.
//!
//! An object that is stored as a delta in one of the packs of the repository stays that delta if its base is in the new pack, as
//! in Git, unless it's asked to start over. Objects that are stored whole in a pack have that data copied as it is.
use std::{io::Write, sync::atomic::AtomicBool};

use gix_error::{Result, ResultExt, bail, message, not_found};
use gix_hash::ObjectId;
use gix_hashtable::HashMap;
use gix_pack::data::{
    entry::Header,
    output::{self, delta::search},
};

/// What decides how deltas are searched for.
pub(super) struct Settings {
    pub window: usize,
    pub depth: u32,
    pub window_memory: Option<u64>,
    pub max_object_size: u64,
    /// If `true`, deltas that exist are kept.
    pub reuse: bool,
}

/// What was written.
pub(super) struct Written {
    /// How many entries are deltas, whether they were found or exist.
    pub deltas: usize,
}

struct Meta {
    kind: gix_object::Kind,
    size: u64,
    location: Option<gix_pack::data::entry::Location>,
}

fn entry_of(
    db: &impl gix_pack::Find,
    location: &gix_pack::data::entry::Location,
    hash: gix_hash::Kind,
) -> Option<(gix_pack::find::Entry, gix_pack::data::Entry)> {
    let entry = db.entry_by_location(location)?;
    if entry.version != gix_pack::data::Version::default() {
        return None;
    }
    let parsed = gix_pack::data::Entry::from_bytes(&entry.data, 0, hash).ok()?;
    Some((entry, parsed))
}

/// Write the pack with the objects `ids` to `out`, with `name_hash` telling for an object which hash its path has.
#[allow(clippy::too_many_arguments)]
pub(super) fn write(
    db: &(impl gix_pack::Find + gix_object::Find + gix_object::FindHeader),
    ids: &[ObjectId],
    name_hash: &dyn Fn(&ObjectId) -> u32,
    settings: &Settings,
    compression: gix_zlib::Compression,
    out: &mut dyn Write,
    hash: gix_hash::Kind,
    should_interrupt: &AtomicBool,
) -> Result<Written> {
    let interrupted = || {
        if should_interrupt.load(std::sync::atomic::Ordering::Relaxed) {
            bail!(gix_error::cancelled("The repack was interrupted"));
        }
        Ok::<_, gix_error::Error>(())
    };
    let count = ids.len();
    let num_objects = u32::try_from(count).or_raise(|| message("A pack can't have more than u32::MAX objects"))?;
    let index_of: HashMap<ObjectId, usize> = ids.iter().enumerate().map(|(idx, id)| (*id, idx)).collect();
    let mut buf = Vec::new();

    // 1. What is known about each object without reading it, and which deltas exist.
    let mut metas = Vec::with_capacity(count);
    for id in ids {
        interrupted()?;
        let header = db
            .try_header(id)
            .or_raise(|| message!("Could not look up {id}"))?
            .ok_or_else(|| not_found(format!("The object {id} that is to be packed is missing")))?;
        let location = db
            .location_by_oid(id, &mut buf)
            .or_raise(|| message!("Could not find where {id} is stored"))?;
        metas.push(Meta {
            kind: header.kind,
            size: header.size,
            location,
        });
    }
    let mut existing_base: Vec<Option<usize>> = vec![None; count];
    if settings.reuse {
        // Not the hash table of `gix_hashtable`, which is for object ids as keys only.
        let mut offsets: std::collections::HashMap<u32, std::collections::HashMap<u64, ObjectId>> =
            std::collections::HashMap::new();
        for (idx, meta) in metas.iter().enumerate() {
            let Some(location) = &meta.location else { continue };
            let Some((_, parsed)) = entry_of(db, location, hash) else {
                continue;
            };
            let Header::OfsDelta { base_distance } = parsed.header else {
                continue;
            };
            let Some(base_offset) = Header::verified_base_pack_offset(location.pack_offset, base_distance) else {
                continue;
            };
            let listed = match offsets.entry(location.pack_id) {
                std::collections::hash_map::Entry::Occupied(listed) => listed.into_mut(),
                std::collections::hash_map::Entry::Vacant(slot) => slot.insert(
                    db.pack_offsets_and_oid(location.pack_id)
                        .or_raise(|| message("Could not list the objects of a pack"))?
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                ),
            };
            if let Some(base) = listed.get(&base_offset).and_then(|base_id| index_of.get(base_id)) {
                existing_base[idx] = Some(*base);
            }
        }
    }
    // The number of deltas it takes to get to an object, where a chain that is too long is cut by starting over with its
    // object as a base.
    let mut depth = vec![0u32; count];
    let mut known = vec![false; count];
    let mut in_chain = vec![false; count];
    for start in 0..count {
        let mut chain = vec![start];
        in_chain[start] = true;
        while let Some(&idx) = chain.last() {
            match existing_base[idx] {
                _ if known[idx] => {
                    in_chain[idx] = false;
                    chain.pop();
                }
                Some(base) if in_chain[base] => {
                    // Deltas of different packs that are each other's bases, which can't be put in a pack. Like Git, give up one.
                    existing_base[idx] = None;
                }
                Some(base) if !known[base] => {
                    in_chain[base] = true;
                    chain.push(base);
                }
                Some(base) => {
                    depth[idx] = depth[base] + 1;
                    if depth[idx] > settings.depth {
                        existing_base[idx] = None;
                        depth[idx] = 0;
                    }
                    known[idx] = true;
                    in_chain[idx] = false;
                    chain.pop();
                }
                None => {
                    known[idx] = true;
                    in_chain[idx] = false;
                    chain.pop();
                }
            }
        }
    }

    // 2. The search.
    let items: Vec<search::Item> = ids
        .iter()
        .zip(&metas)
        .enumerate()
        .map(|(idx, (id, meta))| search::Item {
            id: *id,
            kind: meta.kind,
            size: meta.size,
            name_hash: name_hash(id),
            existing: existing_base[idx].map(|base| search::Existing {
                base,
                depth: depth[idx],
            }),
            preferred_base: false,
        })
        .collect();
    let found = search::search(
        &items,
        |idx, buf| {
            interrupted()?;
            let data = gix_object::Find::try_find(db, &ids[idx], buf)
                .or_raise(|| message!("Could not read {}", ids[idx]))?
                .ok_or_else(|| not_found(format!("The object {} that is to be packed is missing", ids[idx])))?;
            let len = data.data.len();
            buf.truncate(len);
            Ok(())
        },
        search::Options {
            window: settings.window,
            depth: settings.depth,
            window_memory: settings.window_memory,
            max_object_size: settings.max_object_size,
        },
        should_interrupt,
    )?;
    let base_of = |idx: usize| found[idx].as_ref().map(|delta| delta.base).or(existing_base[idx]);

    // 3. Bases come before what is built on them.
    let mut position: Vec<Option<usize>> = vec![None; count];
    let mut order = Vec::with_capacity(count);
    for start in 0..count {
        let mut stack = vec![start];
        while let Some(&idx) = stack.last() {
            if position[idx].is_some() {
                stack.pop();
                continue;
            }
            match base_of(idx) {
                Some(base) if position[base].is_none() => {
                    if stack.len() > count {
                        bail!(message("The deltas that were chosen run in a circle"));
                    }
                    stack.push(base);
                }
                _ => {
                    position[idx] = Some(order.len());
                    order.push(idx);
                    stack.pop();
                }
            }
        }
    }
    let position_of = |idx: usize| position[idx].expect("everything has a position");

    // 4. Write, an entry at a time.
    let version = gix_pack::data::Version::default();
    let mut deltas = 0;
    let entries = order.iter().map(|&idx| {
        interrupted()?;
        let id = ids[idx];
        let entry = if let Some(delta) = &found[idx] {
            deltas += 1;
            output::Entry::from_delta(id, position_of(delta.base), &delta.data, compression)?
        } else if let Some(base) = existing_base[idx] {
            let location = metas[idx].location.as_ref().expect("existing deltas are in packs");
            match entry_of(db, location, hash) {
                Some((mut entry, parsed)) => {
                    deltas += 1;
                    entry.data.drain(..parsed.data_offset as usize);
                    output::Entry {
                        id,
                        kind: output::entry::Kind::DeltaRef {
                            object_index: position_of(base),
                        },
                        decompressed_size: parsed.decompressed_size as usize,
                        compressed_data: entry.data,
                    }
                }
                None => bail!(message!("The pack entry of {id} that was going to be reused is gone")),
            }
        } else {
            let copied = metas[idx]
                .location
                .as_ref()
                .and_then(|location| {
                    let entry = db.entry_by_location(location)?;
                    let counted = output::Count::from_data(id, Some(location.clone()));
                    output::Entry::from_pack_entry(
                        entry,
                        &counted,
                        &[],
                        0,
                        None::<fn(u32, u64) -> Result<Option<ObjectId>>>,
                        version,
                    )
                })
                .transpose()?;
            match copied {
                Some(entry) => entry,
                None => {
                    let data = gix_object::Find::try_find(db, &id, &mut buf)
                        .or_raise(|| message!("Could not read {id}"))?
                        .ok_or_else(|| not_found(format!("The object {id} that is to be packed is missing")))?;
                    output::Entry::from_data(&output::Count::from_data(id, None), &data, compression)?
                }
            }
        };
        Ok(vec![entry])
    });
    let mut writer = output::bytes::FromEntriesIter::new(entries, out, num_objects, version, hash);
    for written in writer.by_ref() {
        written?;
    }
    drop(writer);
    Ok(Written { deltas })
}
