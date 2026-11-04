//! Generate the pack which brings a receiver that has some objects up to date with some other objects.
//!
//! This is what pushing needs: given the `tips` to send and the `haves` the receiver is known to have, the objects of
//! the pack are those reachable from the tips and not from the haves.
//!
//! ## Closure
//!
//! The pack contains every object reachable from `tips` that isn't reachable from `haves`, so the receiver has the complete history
//! behind each tip once it has the `haves`. It can contain more: the commits at which the walk stopped, as
//! they are the parents of new commits, and their root trees. The contents of new commits are compared to the contents of their parents
//! by path, so an object that also exists at another path in a parent is sent again. Annotated tags are followed to their targets, which may be commits,
//! trees or blobs. Tags that point at trees send the entire tree, as nothing is known about the trees the receiver has.
//! Submodule entries of trees are skipped, as they refer to commits of other repositories.
//!
//! `haves` the object database doesn't have are ignored, as the receiver may know more than we do, and so are those
//! that aren't commits after peeling tags. Missing objects that are needed for the pack are an error, which includes
//! the parents of commits in a shallow repository: the history behind the shallow boundary can't be sent, and it can't be proven
//! the receiver has it.
//!
//! Without usable commit-graph generation numbers, commits are hidden with Git's date based heuristic, which can send
//! commits the receiver has if clocks were skewed. This makes the pack bigger, but doesn't make it invalid.
//!
//! ## Memory
//!
//! Memory is bounded by the number of objects, and by the size of the objects of the chunks in flight, but not by the total size of the pack.
//! That is, the list of objects to send is kept in memory, as well as the metadata about each object which is written, along
//! with the objects of the chunks that are compressed at a time and then written, in order. With a single thread, a chunk is
//! [`Options::chunk_size`] objects and about three chunks are alive at a time. Each object is held in memory as a whole while it is processed,
//! which makes the size of the largest object the least that is needed. Git streams blobs above `core.bigFileThreshold` instead, so this
//! doesn't match its memory use. By default, a single thread produces the pack. More threads make it faster, but the size of chunks is chosen
//! to distribute the work, which can make a chunk contain most of the pack, and the order in which
//! the chunks are written is restored with a buffer which isn't bounded, so memory is not bounded either.
use std::{collections::HashSet, io::Write, sync::atomic::AtomicBool};

use gix_error::{OptionExt, Result, ResultExt, bail, message, not_found};
use gix_hash::ObjectId;
use gix_object::TagRefIter;
use gix_parallel::Finalize;

use crate::data::output::{self, count};

/// Configuration for [`counts()`] and [`write_pack()`].
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The amount of threads to use at most for counting and compressing objects, with `Some(1)` being the default.
    ///
    /// More than one thread makes the number of chunks held in memory unbounded, see the [module documentation](self).
    pub thread_limit: Option<usize>,
    /// The amount of objects that are compressed together before being written, which is also the amount of
    /// objects held in memory at a time per chunk in flight, besides the objects counted.
    ///
    /// With one thread, and chunks being in flight while they are compressed, waiting and written,
    /// at most `chunk_size * 3` objects are held whole. More threads let the implementation pick the size of chunks
    /// to distribute the work, see the [module documentation](self).
    pub chunk_size: usize,
    /// The compression level for objects that are not copied from another pack.
    pub compression: gix_zlib::Compression,
    /// If `false`, the pack won't contain deltas with offsets to their bases, as the receiver doesn't support them.
    /// Objects are then never copied from existing packs, which makes writing the pack slower and the pack bigger.
    pub allow_ofs_delta: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            thread_limit: Some(1),
            chunk_size: 10,
            compression: gix_zlib::Compression::DEFAULT,
            allow_ofs_delta: true,
        }
    }
}

/// Information about a pack written by [`write_pack()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The amount of objects in the pack.
    pub objects: usize,
    /// The amount of bytes written, including the trailing hash.
    pub bytes_written: u64,
    /// The hash of the pack, which is also its trailer.
    pub trailer: ObjectId,
}

/// Follow tags from `id`, calling `on_tag` for each tag object on the way, and return the first object that isn't a tag along with its kind.
/// Returns `None` if an object is not available.
fn peel<Find>(
    db: &Find,
    mut id: ObjectId,
    buf: &mut Vec<u8>,
    mut on_tag: impl FnMut(ObjectId),
) -> Result<Option<(ObjectId, gix_object::Kind)>>
where
    Find: gix_object::Find,
{
    // Chains of tags are short, but nothing prevents a long one. A cycle is impossible as ids are hashes of their content.
    let start = id;
    for _ in 0..10_000 {
        let Some(data) = db.try_find(&id, buf).or_raise(|| message!("Could not look up {id}"))? else {
            return Ok(None);
        };
        match data.kind {
            gix_object::Kind::Tag => {
                on_tag(id);
                id = TagRefIter::from_bytes(data.data, data.object_hash)
                    .target_id()
                    .or_raise(|| message!("Tag {id} has no valid target"))?;
            }
            kind => return Ok(Some((id, kind))),
        }
    }
    bail!(message!("The chain of tags starting at {start} is unreasonably long"))
}

/// Compute what has to be sent to a receiver who has `haves`, so it has `tips` afterwards. See the [module documentation](self).
///
/// Fails with a [`NotFound`](gix_error::Class::NotFound) error if a tip, or any object needed for the pack, is missing.
/// `progress` receives the amount of objects counted.
pub fn counts<Find>(
    db: Find,
    tips: &[ObjectId],
    haves: &[ObjectId],
    progress: &dyn gix_utils::progress::Count,
    should_interrupt: &AtomicBool,
    options: Options,
) -> Result<Vec<output::Count>>
where
    Find: crate::Find + gix_object::Find + Send + Clone + 'static,
{
    let mut buf = Vec::new();
    let mut hidden = Vec::new();
    for have in haves {
        if let Some((peeled, gix_object::Kind::Commit)) = peel(&db, *have, &mut buf, |_| {})? {
            hidden.push(peeled);
        }
    }

    let (mut tag_ids, mut blob_ids, mut tree_ids, mut commit_ids) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for tip in tips {
        let (peeled, kind) = peel(&db, *tip, &mut buf, |tag_id| tag_ids.push(tag_id))?.ok_or_raise(|| {
            not_found(format!(
                "Cannot send {tip}, as it or an object it refers to is not in the object database"
            ))
        })?;
        match kind {
            gix_object::Kind::Commit => commit_ids.push(peeled),
            gix_object::Kind::Tree => tree_ids.push(peeled),
            gix_object::Kind::Blob => blob_ids.push(peeled),
            gix_object::Kind::Tag => unreachable!("tags are peeled"),
        }
    }

    let thread_limit = options.thread_limit;
    let chunk_size = options.chunk_size;
    let mut all: Vec<output::Count> = Vec::new();
    let mut run = |input: Box<dyn Iterator<Item = Result<ObjectId>> + Send>, expansion| -> Result {
        let (counts, _) = count::objects(
            db.clone(),
            input,
            progress,
            should_interrupt,
            count::objects::Options {
                thread_limit,
                chunk_size,
                input_object_expansion: expansion,
            },
        )?;
        all.extend(counts);
        Ok(())
    };
    let as_is: Vec<_> = tag_ids.into_iter().chain(blob_ids).map(Ok).collect();
    run(Box::new(as_is.into_iter()), count::objects::ObjectExpansion::AsIs)?;
    let trees: Vec<_> = tree_ids.into_iter().map(Ok).collect();
    run(
        Box::new(trees.into_iter()),
        count::objects::ObjectExpansion::TreeContents,
    )?;
    let walk = gix_traverse::commit::Simple::new(commit_ids, db.clone())
        .hide(hidden)
        .or_raise(|| message("Could not set up the commit traversal"))?
        .map(|commit| {
            commit
                .map(|commit| commit.id)
                .or_raise(|| message("Could not traverse the commits to send, which fails if the history is incomplete as in a shallow repository"))
        });
    run(
        Box::new(walk),
        count::objects::ObjectExpansion::TreeAdditionsComparedToAncestor,
    )?;

    // The passes don't know of each other, so objects can be present more than once.
    let mut seen = HashSet::with_capacity(all.len());
    all.retain(|count| seen.insert(count.id));

    // Objects that can't be found would produce an invalid pack entry after the pack's header, which counted them.
    for count in &all {
        if !crate::Find::contains(&db, &count.id) {
            bail!(not_found(format!(
                "Object {} is needed for the pack, but is not in the object database",
                count.id
            )));
        }
    }
    Ok(all)
}

/// Write the pack for `counts`, which were obtained from [`counts()`], to `out`, using a pack of `object_hash`,
/// and return information about it.
///
/// Counting first lets callers fail on missing objects before they start talking to a receiver. An object that disappeared
/// since, or an error while writing, leaves `out` with an incomplete pack.
pub fn write_pack<Find>(
    db: Find,
    counts: Vec<output::Count>,
    out: &mut dyn Write,
    object_hash: gix_hash::Kind,
    options: Options,
) -> Result<Outcome>
where
    Find: crate::Find + gix_object::Find + Send + Clone + 'static,
{
    let counts = if options.allow_ofs_delta {
        counts
    } else {
        // Without a location in a pack, objects are read and compressed on their own, so none is a delta.
        counts
            .into_iter()
            .map(|count| output::Count::from_data(count.id, None))
            .collect()
    };
    let num_objects =
        u32::try_from(counts.len()).or_raise(|| message("A pack can't have more than u32::MAX objects"))?;
    let mut entries = gix_parallel::InOrderIter::from(output::entry::iter_from_counts(
        counts,
        db,
        Box::new(gix_utils::progress::Discard),
        output::entry::iter_from_counts::Options {
            thread_limit: options.thread_limit,
            mode: output::entry::iter_from_counts::Mode::PackCopyAndBaseObjects,
            allow_thin_pack: false,
            chunk_size: options.chunk_size,
            version: crate::data::Version::default(),
            compression: options.compression,
        },
    )?);

    let checked = entries.by_ref().map(|chunk| {
        let chunk = chunk?;
        if chunk.iter().any(output::Entry::is_invalid) {
            bail!(not_found(
                "An object disappeared from the object database while the pack was written"
            ));
        }
        Ok(chunk)
    });
    let mut writer =
        output::bytes::FromEntriesIter::new(checked, out, num_objects, crate::data::Version::default(), object_hash);
    let mut bytes_written = 0;
    for written in writer.by_ref() {
        bytes_written += written?;
    }
    let trailer = writer.digest().expect("iteration is done");
    drop(writer);
    let stats = entries.inner.finalize()?;
    if stats.missing_objects != 0 {
        bail!(not_found("Objects were missing while the pack was written"));
    }
    Ok(Outcome {
        objects: num_objects as usize,
        bytes_written,
        trailer,
    })
}
