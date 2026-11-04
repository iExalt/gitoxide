use crate::config::{
    Tree,
    tree::{Feature, Key, Section, keys},
};

impl Feature {
    /// The `feature.experimental` key.
    ///
    /// Git selects the `reftable` reference storage for new repositories if it is set and `init.defaultRefFormat` isn't.
    pub const EXPERIMENTAL: keys::Boolean = keys::Boolean::new_boolean("experimental", &Tree::FEATURE);
}

impl Section for Feature {
    fn name(&self) -> &str {
        "feature"
    }

    fn keys(&self) -> &[&dyn Key] {
        &[&Self::EXPERIMENTAL]
    }
}
