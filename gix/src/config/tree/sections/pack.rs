use crate::{
    config,
    config::tree::{Key, Pack, Section, keys},
};

impl Pack {
    /// The `pack.threads` key.
    pub const THREADS: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("threads", &config::Tree::PACK)
            .with_deviation("Leaving this key unspecified uses all available cores, instead of 1");

    /// The `pack.indexVersion` key.
    pub const INDEX_VERSION: IndexVersion =
        IndexVersion::new_with_validate("indexVersion", &config::Tree::PACK, validate::IndexVersion);

    /// The `pack.compression` key.
    pub const COMPRESSION: keys::Compression = keys::Compression::new_compression("compression", &config::Tree::PACK);

    /// The `pack.window` key, which is how many objects before an object are tried as the base of its delta. The default is 10.
    pub const WINDOW: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("window", &config::Tree::PACK);

    /// The `pack.depth` key, which is the longest chain of deltas. The default is 50.
    pub const DEPTH: keys::UnsignedInteger = keys::UnsignedInteger::new_unsigned_integer("depth", &config::Tree::PACK);

    /// The `pack.windowMemory` key, which is the most memory that the window of objects may take up, `0` for no limit.
    pub const WINDOW_MEMORY: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("windowMemory", &config::Tree::PACK);
}

/// The `pack.indexVersion` key.
pub type IndexVersion = keys::Any<validate::IndexVersion>;

mod index_version {
    use gix_error::{ResultExt, bail};

    use crate::{Result, config, config::tree::sections::pack::IndexVersion};

    impl IndexVersion {
        /// Try to interpret an integer value as index version.
        pub fn try_into_index_version(
            &'static self,
            value: Result<Option<i64>>,
        ) -> Result<Option<gix_pack::index::Version>> {
            let Some(value) = value.or_raise(|| config::key::error(self, "Invalid pack index version"))? else {
                return Ok(None);
            };
            Ok(Some(match value {
                1 => gix_pack::index::Version::V1,
                2 => gix_pack::index::Version::V2,
                _ => {
                    bail!(config::key::error_with_value(self, "Invalid pack index version", value));
                }
            }))
        }
    }
}

impl Section for Pack {
    fn name(&self) -> &str {
        "pack"
    }

    fn keys(&self) -> &[&dyn Key] {
        &[
            &Self::THREADS,
            &Self::INDEX_VERSION,
            &Self::COMPRESSION,
            &Self::WINDOW,
            &Self::DEPTH,
            &Self::WINDOW_MEMORY,
        ]
    }
}

mod validate {
    use crate::{Result, bstr::BStr, config::tree::keys};

    #[derive(Clone, Copy)]
    pub struct IndexVersion;
    impl keys::Validate for IndexVersion {
        fn validate(&self, value: &BStr) -> Result {
            super::Pack::INDEX_VERSION
                .try_into_index_version(gix_config::Integer::from_bytes::<i64>(value).map(Some))?;
            Ok(())
        }
    }
}
