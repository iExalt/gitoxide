use std::{collections::HashSet, io::Cursor, sync::atomic::AtomicBool};

use gix_error::{ResultExt, bail, message};
use gix_hash::ObjectId;
use gix_pack::data::output;

use crate::{Repository, Result};

/// Writing objects
impl Repository {
    /// Write `objects`, each a kind with its data, as one pack with its index, and return their ids in the order in which they
    /// were given. An object that is given twice is in the pack once.
    ///
    /// This is what `git fast-import` does with `fastimport.unpackLimit=0`, and what a caller that makes many small objects at once
    /// wants: one file to write and flush instead of one for each object, which maintenance can pack together with others later.
    /// Unlike [`write_object()`](Self::write_object()), the objects are always written and not looked for first, so that they
    /// are in the pack that is returned.
    ///
    /// The pack is deflated as `pack.compression` has it and indexed as `pack.indexVersion` and `pack.threads` have it, and flushed as
    /// `core.fsync` and `core.fsyncMethod` ask for: the pack and its index are made durable before the pack can be found
    /// by its index. Objects are held in memory while the pack is made, so this isn't for large objects, which `git fast-import`
    /// streams.
    ///
    /// The pack is made in a directory of its own, `objects/pack/tmp_pack_dir_*`, and moved into place, pack before index, once it is
    /// complete. A process that is killed meanwhile leaves that directory, which [`prune()`](Self::prune()) removes once it is older
    /// than the expiry, like Git's `tmp_pack_*` files. No `.keep` file is left, as `fast-import` leaves none: the caller makes the objects
    /// reachable, and maintenance may repack them as any others in the meantime, which doesn't lose them as it keeps what isn't
    /// reachable until it expires, counted from the time of the pack. Writing objects that are in a pack of the same name already
    /// leaves that pack as it is, with its time set to now.
    ///
    /// The pack and its index are read-only, as Git makes them. Their mode is `0444` regardless of the umask, where Git filters it through
    /// the umask, and `core.sharedRepository` isn't applied, as it isn't for loose objects.
    ///
    /// Nothing is written if there are no objects. A pack of objects that are all in the repository already is written regardless.
    pub fn write_objects_as_pack<D>(
        &self,
        objects: impl IntoIterator<Item = (gix_object::Kind, D)>,
        should_interrupt: &AtomicBool,
    ) -> Result<Vec<ObjectId>>
    where
        D: AsRef<[u8]>,
    {
        let hash = self.object_hash();
        let compression = self.pack_compression()?;
        let mut ids = Vec::new();
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        for (kind, data) in objects {
            let data = data.as_ref();
            let id = gix_object::compute_hash(hash, kind, data).or_error()?;
            ids.push(id);
            if seen.insert(id) {
                entries.push(
                    output::Entry::from_data(
                        &output::Count::from_data(id, None),
                        &gix_object::Data::new(data, kind, hash),
                        compression,
                    )
                    .or_raise(|| message("Could not compress an object for the pack"))?,
                );
            }
        }
        if entries.is_empty() {
            return Ok(ids);
        }
        let num_objects =
            u32::try_from(entries.len()).or_raise(|| message("A pack can't have more than u32::MAX objects"))?;
        let mut pack = Vec::new();
        let mut writer = output::bytes::FromEntriesIter::new(
            std::iter::once(Ok::<_, gix_error::Error>(entries)),
            &mut pack,
            num_objects,
            gix_pack::data::Version::default(),
            hash,
        );
        for written in writer.by_ref() {
            written.or_raise(|| message("Could not write the pack"))?;
        }
        drop(writer);
        let pack_dir = self.objects.store_ref().path().join("pack");
        std::fs::create_dir_all(&pack_dir).or_raise(|| message!("Could not create {}", pack_dir.display()))?;
        let scratch = Scratch::new(&pack_dir)?;
        let written = gix_pack::Bundle::write_to_directory(
            &mut Cursor::new(pack),
            Some(&scratch.0),
            &mut gix_utils::progress::Discard,
            should_interrupt,
            None::<gix_object::find::Never>,
            hash,
            gix_pack::bundle::write::Options {
                thread_limit: crate::maintenance::repack::index_threads(self)?,
                index_version: crate::maintenance::repack::pack_index_version(self)?,
                iteration_mode: gix_pack::data::input::Mode::Verify,
                alloc_limit_bytes: self.config.alloc_limit_bytes,
                compression: self.config.loose_compression,
                fsync: self.config.fsync_options()?,
            },
        )
        .or_raise(|| message("Could not index the new pack, so the objects were not written"))?;
        let (Some(data_path), Some(index_path)) = (written.data_path, written.index_path) else {
            bail!(message(
                "The new pack has no objects, which can't be as there are some to write"
            ));
        };
        let pack_destination = pack_dir.join(data_path.file_name().expect("a pack has a file name"));
        let index_destination = pack_dir.join(index_path.file_name().expect("an index has a file name"));
        if pack_destination.is_file() {
            // The same objects in the same order are there already. Their time is now, so that what a pack that was not
            // reachable and old is expected to protect isn't lost.
            if let Ok(file) = std::fs::File::open(&pack_destination) {
                file.set_modified(std::time::SystemTime::now()).ok();
            }
        } else {
            // Git makes packs and their indices read-only, which the writer doesn't, as it gives a received pack the mode of a
            // temporary file.
            #[cfg(unix)]
            for path in [&data_path, &index_path] {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))
                    .or_raise(|| message!("Could not make {} read-only", path.display()))?;
            }
            std::fs::rename(&data_path, &pack_destination)
                .or_raise(|| message!("Could not move the new pack to {}", pack_destination.display()))?;
        }
        if !index_destination.is_file() {
            std::fs::rename(&index_path, &index_destination).or_raise(|| {
                message!(
                    "Could not move the index of the new pack to {}",
                    index_destination.display()
                )
            })?;
        }
        Ok(ids)
    }
}

/// A directory in which a pack is made, removed when it goes out of scope. A kill leaves it, and it is named so that pruning finds it.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(pack_dir: &std::path::Path) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos());
        let path = pack_dir.join(format!(
            "tmp_pack_dir_{}_{nanos}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).or_raise(|| message!("Could not create {}", path.display()))?;
        Ok(Scratch(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}
