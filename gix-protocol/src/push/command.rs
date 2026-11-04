use bstr::{BStr, BString, ByteSlice};
use gix_error::{Result, bail, validation};
use gix_hash::{ObjectId, oid};

/// A request to move one reference on the remote from `old_id` to `new_id`.
///
/// `old_id` is the value the caller expects the remote to have: the null id requests creation and has to fail
/// if the reference exists, and a `new_id` that is null requests deletion. The server verifies `old_id` itself,
/// so a stale expectation is rejected by the remote instead of silently overwriting a newer value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Command {
    ref_name: BString,
    old_id: ObjectId,
    new_id: ObjectId,
}

impl Command {
    /// Create a command for the reference `ref_name`, which has to be a valid full name below `refs/`.
    ///
    /// Both ids must use the same hash kind, and they must differ as a no-op update is never sent.
    pub fn new(ref_name: impl Into<BString>, old_id: impl Into<ObjectId>, new_id: impl Into<ObjectId>) -> Result<Self> {
        let (ref_name, old_id, new_id) = (ref_name.into(), old_id.into(), new_id.into());
        validate_ref_name(ref_name.as_bstr())?;
        if old_id.kind() != new_id.kind() {
            bail!(validation(format!(
                "Command for '{ref_name}' mixes object hash kinds: old id is {}, new id is {}",
                old_id.kind(),
                new_id.kind()
            )));
        }
        if old_id == new_id {
            bail!(validation(format!(
                "Command for '{ref_name}' has identical old and new ids, which is a no-op"
            )));
        }
        Ok(Command {
            ref_name,
            old_id,
            new_id,
        })
    }

    /// The full name of the reference to update.
    pub fn ref_name(&self) -> &BStr {
        self.ref_name.as_bstr()
    }

    /// The value the remote reference is expected to have, or the null id if it is expected to be absent.
    pub fn old_id(&self) -> &oid {
        &self.old_id
    }

    /// The value the remote reference should have afterwards, or the null id to delete it.
    pub fn new_id(&self) -> &oid {
        &self.new_id
    }

    /// Return `true` if this command creates the reference, as it is expected to be absent.
    pub fn is_create(&self) -> bool {
        self.old_id.is_null()
    }

    /// Return `true` if this command deletes the reference.
    pub fn is_delete(&self) -> bool {
        self.new_id.is_null()
    }
}

/// Receive-pack only accepts names below `refs/` that are valid reference names.
fn validate_ref_name(name: &BStr) -> Result {
    if !name.starts_with_str("refs/") {
        bail!(validation(format!("Reference name '{name}' is not below 'refs/'")));
    }
    if let Err(err) = <&gix_ref::FullNameRef>::try_from(name) {
        bail!(validation(format!("Reference name '{name}' is invalid: {err}")));
    }
    Ok(())
}
