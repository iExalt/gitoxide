use crate::{
    config,
    config::tree::{Key, Reftable, Section, keys},
};

impl Reftable {
    /// The `reftable.blockSize` key, which is the size blocks of a table are limited to, with `0` standing for the default
    /// of 4096 bytes. Like in Git, it can't exceed 16 MiB - 1.
    pub const BLOCK_SIZE: BlockSize =
        BlockSize::new_with_validate("blockSize", &config::Tree::REFTABLE, validate::BlockSize);

    /// The `reftable.restartInterval` key, which is after how many records a block restarts key compression,
    /// with `0` standing for the default of 16. Like in Git, it can't exceed 65535.
    pub const RESTART_INTERVAL: RestartInterval =
        RestartInterval::new_with_validate("restartInterval", &config::Tree::REFTABLE, validate::RestartInterval);

    /// The `reftable.geometricFactor` key, which is how much larger each table of a stack is supposed to be than the one
    /// above it before they are compacted, with `0` standing for the default of 2. Like in Git, it can't exceed 255.
    pub const GEOMETRIC_FACTOR: GeometricFactor =
        GeometricFactor::new_with_validate("geometricFactor", &config::Tree::REFTABLE, validate::GeometricFactor);

    /// The `reftable.lockTimeout` key, which is how long to wait for the lock of `tables.list` in milliseconds.
    /// `-1` waits as long as it takes, `0` doesn't wait, and the default is 100. Unlike for `core.filesRefLockTimeout`,
    /// other negative values are invalid.
    pub const LOCK_TIMEOUT: LockTimeout =
        LockTimeout::new_with_validate("lockTimeout", &config::Tree::REFTABLE, validate::LockTimeout);
}

/// The `reftable.blockSize` key.
pub type BlockSize = keys::Any<validate::BlockSize>;
/// The `reftable.restartInterval` key.
pub type RestartInterval = keys::Any<validate::RestartInterval>;
/// The `reftable.geometricFactor` key.
pub type GeometricFactor = keys::Any<validate::GeometricFactor>;
/// The `reftable.lockTimeout` key.
pub type LockTimeout = keys::Any<validate::LockTimeout>;

mod parse {
    use std::time::Duration;

    use gix_error::{ResultExt, bail};

    use super::{BlockSize, GeometricFactor, LockTimeout, RestartInterval};
    use crate::{Result, config, config::tree::Key};

    /// A configured number that has to be between 0 and `max`, as a value of type `T`.
    fn bounded<T: TryFrom<i64>>(
        key: &dyn Key,
        value: Result<Option<i64>>,
        max: i64,
        message: &'static str,
    ) -> Result<Option<T>> {
        let Some(value) = value.or_raise(|| config::key::error(key, message))? else {
            return Ok(None);
        };
        if !(0..=max).contains(&value) {
            bail!(config::key::error_with_value(key, message, value));
        }
        Ok(Some(T::try_from(value).unwrap_or_else(|_| {
            unreachable!("the range check above is within the type")
        })))
    }

    impl BlockSize {
        /// Interpret `value` as block size, or return `None` if it isn't set. `0` is valid and stands for the default.
        pub fn try_into_block_size(&'static self, value: Result<Option<i64>>) -> Result<Option<u32>> {
            bounded(
                self,
                value,
                16_777_215,
                "The reftable block size must be between 0 and 16777215",
            )
        }
    }

    impl RestartInterval {
        /// Interpret `value` as restart interval, or return `None` if it isn't set. `0` is valid and stands for the default.
        pub fn try_into_restart_interval(&'static self, value: Result<Option<i64>>) -> Result<Option<u16>> {
            bounded(
                self,
                value,
                i64::from(u16::MAX),
                "The reftable restart interval must be between 0 and 65535",
            )
        }
    }

    impl GeometricFactor {
        /// Interpret `value` as geometric factor, or return `None` if it isn't set. `0` is valid and stands for the default.
        pub fn try_into_geometric_factor(&'static self, value: Result<Option<i64>>) -> Result<Option<u8>> {
            bounded(
                self,
                value,
                i64::from(u8::MAX),
                "The reftable geometric factor must be between 0 and 255",
            )
        }
    }

    impl LockTimeout {
        /// Interpret `value` as milliseconds to wait for a lock. Return `None` if it isn't set, `Some(None)` to wait
        /// as long as it takes, and the time to wait otherwise.
        pub fn try_into_lock_timeout(&'static self, value: Result<Option<i64>>) -> Result<Option<Option<Duration>>> {
            let Some(value) = value.or_raise(|| config::key::error(self, "Invalid reftable lock timeout"))? else {
                return Ok(None);
            };
            Ok(Some(match u64::try_from(value) {
                Ok(milliseconds) => Some(Duration::from_millis(milliseconds)),
                Err(_) if value == -1 => None,
                Err(_) => bail!(config::key::error_with_value(
                    self,
                    "The reftable lock timeout doesn't support negative values other than -1",
                    value
                )),
            }))
        }
    }
}

impl Section for Reftable {
    fn name(&self) -> &str {
        "reftable"
    }

    fn keys(&self) -> &[&dyn Key] {
        &[
            &Self::BLOCK_SIZE,
            &Self::RESTART_INTERVAL,
            &Self::GEOMETRIC_FACTOR,
            &Self::LOCK_TIMEOUT,
        ]
    }
}

mod validate {
    use crate::{Result, bstr::BStr, config::tree::Reftable, config::tree::keys::Validate};

    macro_rules! validator {
        ($name:ident, $key:ident, $method:ident) => {
            #[derive(Clone, Copy)]
            pub struct $name;
            impl Validate for $name {
                fn validate(&self, value: &BStr) -> Result {
                    let value = gix_config::Integer::from_bytes::<i64>(value)?;
                    Reftable::$key.$method(Ok(Some(value)))?;
                    Ok(())
                }
            }
        };
    }

    validator!(BlockSize, BLOCK_SIZE, try_into_block_size);
    validator!(RestartInterval, RESTART_INTERVAL, try_into_restart_interval);
    validator!(GeometricFactor, GEOMETRIC_FACTOR, try_into_geometric_factor);
    validator!(LockTimeout, LOCK_TIMEOUT, try_into_lock_timeout);
}
