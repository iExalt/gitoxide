use crate::{
    config,
    config::tree::{Extensions, Key, Section, keys},
};

impl Extensions {
    /// The `extensions.worktreeConfig` key.
    pub const WORKTREE_CONFIG: keys::Boolean = keys::Boolean::new_boolean("worktreeConfig", &config::Tree::EXTENSIONS);
    /// The `extensions.relativeWorktrees` key, indicating that worktrees may use relative links.
    pub const RELATIVE_WORKTREES: keys::Boolean =
        keys::Boolean::new_boolean("relativeWorktrees", &config::Tree::EXTENSIONS);
    /// The `extensions.objectFormat` key.
    pub const OBJECT_FORMAT: ObjectFormat = ObjectFormat::new_object_format("objectFormat", &config::Tree::EXTENSIONS);
    /// The `extensions.refStorage` key, selecting the reference storage backend.
    ///
    /// Only `files` is supported: `gix` can't read or write references stored as `reftable`, or at a location
    /// given with the `<format>://<payload>` form, and refuses to operate on references in these cases.
    pub const REF_STORAGE: RefStorage = RefStorage::new_ref_storage("refStorage", &config::Tree::EXTENSIONS)
        .with_environment_override("GIT_REFERENCE_BACKEND");
}

/// The `extensions.objectFormat` key.
pub type ObjectFormat = keys::Any<validate::ObjectFormat>;

/// The `extensions.refStorage` key.
pub type RefStorage = keys::Any<validate::RefStorage>;

mod object_format {
    use gix_error::{ResultExt, message};

    use crate::{
        Result,
        config::tree::{Key, Section, keys, sections::extensions::ObjectFormat},
    };

    impl ObjectFormat {
        /// Create a new instance.
        pub const fn new_object_format(name: &'static str, section: &'static dyn Section) -> Self {
            keys::Any::new_with_validate(name, section, super::validate::ObjectFormat)
        }

        /// Parse an object format, distinguishing unknown names from known hashes disabled in this build.
        /// Disabled hashes are classified as [`gix_error::Class::Unsupported`], unknown names as validation failures.
        pub fn try_into_object_format(&'static self, value: impl gix_utils::AsBStr) -> Result<gix_hash::Kind> {
            super::validate::ObjectFormat::parse(value.as_bstr()).or_raise(|| {
                let mut context = message("Invalid configuration value").with("key", self.logical_name());
                if let Some(environment) = self.environment_override() {
                    context = context.with("environment_override", environment);
                }
                context
            })
        }
    }
}

mod ref_storage {
    use gix_error::{ResultExt, message};

    use crate::{
        Result,
        config::tree::{Key, Section, keys, sections::extensions::RefStorage},
    };

    impl RefStorage {
        /// Create a new instance.
        pub const fn new_ref_storage(name: &'static str, section: &'static dyn Section) -> Self {
            keys::Any::new_with_validate(name, section, super::validate::RefStorage)
        }

        /// Parse a reference storage selection like Git does, and return `None` if the references are stored in
        /// plain files and `packed-refs`, or `Some(description)` of the unsupported storage otherwise.
        ///
        /// Fail for a value that doesn't name a known backend, with `files` and `reftable` being known.
        /// Unsupported values are `reftable` and anything with a `://<payload>` suffix, as that moves the references elsewhere.
        pub(crate) fn try_into_unsupported_backend(
            &'static self,
            value: impl gix_utils::AsBStr,
        ) -> Result<Option<String>> {
            super::validate::RefStorage::parse(value.as_bstr()).or_raise(|| {
                let mut context = message("Invalid configuration value").with("key", self.logical_name());
                if let Some(environment) = self.environment_override() {
                    context = context.with("environment_override", environment);
                }
                context
            })
        }
    }
}

impl Section for Extensions {
    fn name(&self) -> &str {
        "extensions"
    }

    fn keys(&self) -> &[&dyn Key] {
        &[
            &Self::OBJECT_FORMAT,
            &Self::WORKTREE_CONFIG,
            &Self::RELATIVE_WORKTREES,
            &Self::REF_STORAGE,
        ]
    }
}

mod validate {
    use gix_error::message;

    use crate::{Result, bstr::BStr, config::tree::keys};

    #[derive(Clone, Copy)]
    pub struct ObjectFormat;

    impl keys::Validate for ObjectFormat {
        fn validate(&self, value: &BStr) -> Result {
            Self::parse(value).map(|_| ())
        }
    }

    impl ObjectFormat {
        pub(super) fn parse(value: &BStr) -> Result<gix_hash::Kind> {
            #[cfg(feature = "sha1")]
            if value.eq_ignore_ascii_case(b"sha1") {
                return Ok(gix_hash::Kind::Sha1);
            }

            #[cfg(feature = "sha256")]
            if value.eq_ignore_ascii_case(b"sha256") {
                return Ok(gix_hash::Kind::Sha256);
            }

            if value.eq_ignore_ascii_case(b"sha1") || value.eq_ignore_ascii_case(b"sha256") {
                return Err(message("Object format is not enabled in this build")
                    .with_input(value)
                    .unsupported_error());
            }

            Err(message("Invalid configuration value")
                .with_input(value)
                .validation_error())
        }
    }

    #[derive(Clone, Copy)]
    pub struct RefStorage;

    impl keys::Validate for RefStorage {
        fn validate(&self, value: &BStr) -> Result {
            Self::parse(value).map(|_| ())
        }
    }

    impl RefStorage {
        pub(super) fn parse(value: &BStr) -> Result<Option<String>> {
            use crate::bstr::ByteSlice;
            let (format, payload) = match value.find(b"://") {
                Some(pos) => (value[..pos].as_bytes(), Some(&value[pos + 3..])),
                None => (value.as_bytes(), None),
            };
            match (format, payload) {
                (b"files", None) => Ok(None),
                (b"files" | b"reftable", _) => Ok(Some(value.to_string())),
                _ => Err(message("Unknown reference storage format")
                    .with_input(value)
                    .validation_error()),
            }
        }
    }
}
