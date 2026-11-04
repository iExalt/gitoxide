use crate::bstr::BString;
use gix_hash::ObjectId;
use gix_ref::FullName;

/// What the caller believes the remote reference currently has, which the remote verifies when it changes the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lease {
    /// The reference must not exist.
    Absent,
    /// The reference must point to this object.
    At(ObjectId),
}

/// How to treat the history of an update of an existing reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Policy {
    /// The new value has to be a descendant of the value the reference has, like in a log. Updates of tags are refused, as Git does without force.
    #[default]
    FastForwardOnly,
    /// Replace the value even if the history is rewritten, like with `--force`.
    Rewrite,
}

/// One reference to change on the remote, including the value it is expected to have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub(crate) name: FullName,
    pub(crate) new: Option<ObjectId>,
    pub(crate) lease: Lease,
    pub(crate) policy: Policy,
    pub(crate) local_name: Option<BString>,
}

impl Update {
    /// Create the reference `name` with value `new`, which must not exist yet.
    pub fn create(name: FullName, new: impl Into<ObjectId>) -> Self {
        Update::new(name, Lease::Absent, Some(new.into()))
    }

    /// Change the reference `name`, which has to have the value `lease`, to `new`.
    pub fn change(name: FullName, lease: impl Into<ObjectId>, new: impl Into<ObjectId>) -> Self {
        Update::new(name, Lease::At(lease.into()), Some(new.into()))
    }

    /// Delete the reference `name`, which has to have the value `lease`.
    pub fn delete(name: FullName, lease: impl Into<ObjectId>) -> Self {
        Update::new(name, Lease::At(lease.into()), None)
    }

    /// Create an update from its parts, with `new` being `None` to delete the reference.
    ///
    /// Invalid combinations, like deleting a reference that is expected to be absent, are refused when preparing the push.
    pub fn new(name: FullName, lease: Lease, new: Option<ObjectId>) -> Self {
        Update {
            name,
            new,
            lease,
            policy: Policy::default(),
            local_name: None,
        }
    }

    /// Choose how to treat history when the reference is updated, which is [`Policy::FastForwardOnly`] by default.
    pub fn with_policy(mut self, policy: Policy) -> Self {
        self.policy = policy;
        self
    }

    /// Set the name of the local reference this update originates from, for hooks that want to know. This is
    /// only informational and defaults to the id that is pushed.
    pub fn with_local_name(mut self, name: impl Into<BString>) -> Self {
        self.local_name = Some(name.into());
        self
    }

    /// The name of the reference on the remote.
    pub fn name(&self) -> &FullName {
        &self.name
    }

    /// The value the reference should have afterwards, or `None` if it is to be deleted.
    pub fn new_id(&self) -> Option<&gix_hash::oid> {
        self.new.as_deref()
    }

    /// What the remote reference is expected to have.
    pub fn lease(&self) -> Lease {
        self.lease
    }

    /// How history is treated.
    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// Return `true` if this update deletes the reference.
    pub fn is_delete(&self) -> bool {
        self.new.is_none()
    }
}
