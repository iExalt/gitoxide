//! Look at a pack that was written to disk before its objects are accepted, which is what checking them needs.
//!
//! An `Inspection` reads the objects of a pack in the order of the pack and leaves the decoding of their data to the
//! caller, who may not need it. It can be made for any pack of a repository that isn't part of its object database yet.
use std::{
    collections::HashSet,
    ops::ControlFlow,
    sync::{Arc, Mutex},
};

use gix_error::Result;
use gix_hash::{ObjectId, oid};
use gix_object::{Find, FindHeader};

/// The pack that was received, as seen by an inspector.
pub struct Inspection<'a> {
    bundle: &'a gix_pack::Bundle,
    thin_bases: &'a HashSet<ObjectId>,
    odb: &'a dyn ObjectDatabase,
    kept: bool,
}

/// Something that finds objects and their headers, which the objects of a pack are looked up in along with.
trait ObjectDatabase: Find + FindHeader {}

impl<T: Find + FindHeader> ObjectDatabase for T {}

impl<'a> Inspection<'a> {
    /// Create an inspection of the pack `bundle`, which is a kept pack if `kept` is set and else one that will be exploded into
    /// loose objects, with `odb` as the object database whose objects are found along with those of the pack.
    /// `thin_bases` are the ids of objects that were added to complete a thin pack and which aren't part of what was received.
    pub fn new(
        bundle: &'a gix_pack::Bundle,
        thin_bases: &'a HashSet<ObjectId>,
        odb: &'a (impl Find + FindHeader),
        kept: bool,
    ) -> Self {
        Inspection {
            bundle,
            thin_bases,
            odb,
            kept,
        }
    }

    /// `true` if the pack is kept as pack, and `false` if it will be exploded into loose objects.
    /// This is the difference between `index-pack` and `unpack-objects`.
    pub fn kept(&self) -> bool {
        self.kept
    }

    /// Call `each` with every object that was received, in the order of the pack, until it breaks.
    /// The kind and size of the object are known without more ado, while its data is only decoded if [`Object::data()`] is called,
    /// which saves much of the work if what is looked at are only some of the objects.
    ///
    /// The objects that were added to complete a thin pack aren't received, and are left out.
    pub fn objects(&self, each: &mut dyn FnMut(&mut Object<'_>) -> Result<ControlFlow<()>>) -> Result {
        let mut by_offset: Vec<_> = (0..self.bundle.index.num_objects())
            .map(|index| (self.bundle.index.pack_offset_at_index(index), index))
            .collect();
        by_offset.sort_unstable();
        let mut inflate = gix_zlib::Inflate::default();
        let mut buf = Vec::new();
        // Objects are decoded in the order of the pack, in which what a delta is based on is usually close by.
        let mut cache = gix_pack::cache::lru::StaticLinkedList::<64>::new(64 * 1024 * 1024);
        for (offset, index) in by_offset {
            let id = self.bundle.index.oid_at_index(index);
            if self.thin_bases.contains(id) {
                continue;
            }
            let header = pack_header(self.bundle, offset, &mut inflate)?;
            let mut object = Object {
                id,
                kind: header.kind,
                size: header.object_size,
                index,
                bundle: self.bundle,
                buf: &mut buf,
                inflate: &mut inflate,
                cache: &mut cache,
            };
            if each(&mut object)?.is_break() {
                break;
            }
        }
        Ok(())
    }

    /// Find objects in what was received and in the repository with the quarantine that has no more than that.
    pub fn find(&self) -> impl Find + FindHeader + '_ {
        PackAndRepository {
            bundle: self.bundle,
            odb: self.odb,
            inflate: Default::default(),
        }
    }
}

/// An object of a received pack, which is only decoded if its [data](Object::data()) is asked for.
pub struct Object<'a> {
    id: &'a oid,
    kind: gix_object::Kind,
    size: u64,
    index: u32,
    bundle: &'a gix_pack::Bundle,
    buf: &'a mut Vec<u8>,
    inflate: &'a mut gix_zlib::Inflate,
    cache: &'a mut dyn gix_pack::cache::DecodeEntry,
}

impl Object<'_> {
    /// The id of the object.
    pub fn id(&self) -> &oid {
        self.id
    }

    /// The kind of the object.
    pub fn kind(&self) -> gix_object::Kind {
        self.kind
    }

    /// The size of the object in bytes, which is that of its [data](Self::data()).
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Decode the object, which can be expensive if it's a delta, and return its data.
    pub fn data(&mut self) -> Result<&[u8]> {
        let (object, _) = self
            .bundle
            .get_object_by_index(self.index, self.buf, self.inflate, self.cache)?;
        Ok(object.data)
    }
}

/// Decode the header of the object whose entry is at `offset` in the pack of `bundle`, which is cheap as only as much of a delta
/// is read as it takes to learn the size of the result, and the kind that its base has.
fn pack_header(
    bundle: &gix_pack::Bundle,
    offset: gix_pack::data::Offset,
    inflate: &mut gix_zlib::Inflate,
) -> Result<gix_pack::data::decode::header::Outcome> {
    let resolve = |base: &oid| -> Result<Option<gix_pack::data::decode::header::ResolvedBase>> {
        bundle
            .index
            .lookup(base)
            .map(|index| {
                bundle
                    .pack
                    .entry(bundle.index.pack_offset_at_index(index))
                    .map(gix_pack::data::decode::header::ResolvedBase::InPack)
            })
            .transpose()
    };
    bundle.pack.decode_header(bundle.pack.entry(offset)?, inflate, &resolve)
}

struct PackAndRepository<'a> {
    bundle: &'a gix_pack::Bundle,
    odb: &'a dyn ObjectDatabase,
    /// Reused by all lookups of headers.
    inflate: std::cell::RefCell<gix_zlib::Inflate>,
}

impl Find for PackAndRepository<'_> {
    fn try_find<'a>(&self, id: &oid, buffer: &'a mut Vec<u8>) -> Result<Option<gix_object::Data<'a>>> {
        let mut inflate = gix_zlib::Inflate::default();
        // The borrow checker can't see that `buffer` isn't used if nothing is found in the pack.
        if self.bundle.index.lookup(id).is_some() {
            return Ok(self
                .bundle
                .find(id, buffer, &mut inflate, &mut gix_pack::cache::Never)?
                .map(|(data, _)| data));
        }
        self.odb.try_find(id, buffer)
    }
}

impl FindHeader for PackAndRepository<'_> {
    /// Only the header is read, so that looking at the objects that something links to, which is most of the objects
    /// that aren't new, doesn't mean to decode them.
    fn try_header(&self, id: &oid) -> Result<Option<gix_object::Header>> {
        if let Some(index) = self.bundle.index.lookup(id) {
            let offset = self.bundle.index.pack_offset_at_index(index);
            let header = pack_header(self.bundle, offset, &mut self.inflate.borrow_mut())?;
            return Ok(Some(gix_object::Header {
                kind: header.kind,
                size: header.object_size,
            }));
        }
        self.odb.try_header(id)
    }
}

/// Look up the bases to complete a thin pack and remember which ones were asked for. Clones share what was asked for.
#[derive(Clone)]
pub struct RecordingFind<F> {
    odb: F,
    requested: Arc<Mutex<HashSet<ObjectId>>>,
}

impl<F> RecordingFind<F> {
    /// Create a lookup that finds objects in `odb`.
    pub fn new(odb: F) -> Self {
        RecordingFind {
            odb,
            requested: Arc::default(),
        }
    }

    /// Return the ids of the objects that were found so far and forget them.
    pub fn take_requested(&self) -> HashSet<ObjectId> {
        std::mem::take(&mut *self.requested.lock().expect("no panic while held"))
    }
}

impl<F: Find> Find for RecordingFind<F> {
    fn try_find<'a>(&self, id: &oid, buffer: &'a mut Vec<u8>) -> Result<Option<gix_object::Data<'a>>> {
        let found = self.odb.try_find(id, buffer)?;
        if found.is_some() {
            self.requested
                .lock()
                .expect("no panic while held")
                .insert(id.to_owned());
        }
        Ok(found)
    }
}
