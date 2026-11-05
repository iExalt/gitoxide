use gix_hash::ObjectId;
use gix_ref::{
    FullName, PartialNameRef, Target,
    transaction::{PreviousValue, RefEdit},
};

use crate::error::{ResultExt, validation};
use crate::{Reference, Result, bstr::BString, ext::ReferenceExt, reference};

/// Obtain and alter references comfortably
impl crate::Repository {
    /// Return a backend-neutral view of the store that holds this repository's references, to find, iterate and
    /// edit them without depending on how they are stored.
    ///
    /// The view borrows the repository, so it reflects its namespace and reflog settings.
    pub fn ref_store(&self) -> gix_ref::StoreRef<'_> {
        gix_ref::StoreRef::from(&self.refs)
    }

    /// Create a lightweight tag with given `name` (and without `refs/tags/` prefix) pointing to the given `target`, and return it as reference.
    ///
    /// It will be created with `constraint` which is most commonly to [only create it](PreviousValue::MustNotExist)
    /// or to [force overwriting a possibly existing tag](PreviousValue::Any).
    pub fn tag_reference(
        &self,
        name: impl AsRef<str>,
        target: impl Into<ObjectId>,
        constraint: PreviousValue,
    ) -> Result<Reference<'_>> {
        let id = target.into();
        let name = FullName::try_from(format!("refs/tags/{}", name.as_ref()))
            .or_raise(|| validation("The tag reference name is invalid"))?;
        let mut edits = self.edit_reference(RefEdit::update(name, id, constraint, ""))?;
        assert_eq!(edits.len(), 1, "reference splits should ever happen");
        let edit = edits.pop().expect("exactly one item");
        Ok(Reference {
            inner: gix_ref::Reference {
                name: edit.name,
                target: id.into(),
                peeled: None,
            },
            repo: self,
        })
    }

    /// Returns the currently set namespace for references, or `None` if it is not set.
    ///
    /// Namespaces allow to partition references.
    pub fn namespace(&self) -> Option<&gix_ref::Namespace> {
        self.refs.namespace()
    }

    /// Remove the currently set reference namespace and return it.
    pub fn clear_namespace(&mut self) -> Option<gix_ref::Namespace> {
        self.refs.set_namespace(None)
    }

    /// Set the reference namespace to the given value, like `"foo"` or `"foo/bar"`.
    pub fn set_namespace<'a, Name, E>(&mut self, namespace: Name) -> Result<Option<gix_ref::Namespace>>
    where
        Name: TryInto<&'a PartialNameRef, Error = E>,
        gix_validate::reference::name::Error: From<E>,
    {
        let namespace = gix_ref::namespace::expand(namespace).or_error()?;
        Ok(self.refs.set_namespace(Some(namespace)))
    }

    // TODO: more tests or usage
    /// Create a new reference with `name`, like `refs/heads/branch`, pointing to `target`, adhering to `constraint`
    /// during creation and writing `log_message` into the reflog. Note that a ref-log will be written even if `log_message` is empty.
    ///
    /// Note that this accepts any valid full reference name, including `refs/heads/HEAD`.
    /// Git rejects creating a local branch with that name in branch-specific code paths, but this API operates on generic
    /// references instead. Branch names can be validated with [`gix_validate::reference::branch_name()`].
    ///
    /// The newly created Reference is returned.
    pub fn reference<Name, E>(
        &self,
        name: Name,
        target: impl Into<ObjectId>,
        constraint: PreviousValue,
        log_message: impl Into<BString>,
    ) -> Result<Reference<'_>>
    where
        Name: TryInto<FullName, Error = E>,
        gix_validate::reference::name::Error: From<E>,
    {
        self.reference_inner(
            name.try_into()
                .map_err(gix_validate::reference::name::Error::from)
                .or_raise(|| validation("The reference name is invalid"))?,
            target.into(),
            constraint,
            log_message.into(),
        )
    }

    fn reference_inner(
        &self,
        name: FullName,
        id: ObjectId,
        constraint: PreviousValue,
        log_message: BString,
    ) -> Result<Reference<'_>> {
        let mut edits = self.edit_reference(RefEdit::update(name, id, constraint, log_message))?;
        assert_eq!(
            edits.len(),
            1,
            "only one reference can be created, splits aren't possible"
        );

        Ok(gix_ref::Reference {
            name: edits.pop().expect("exactly one edit").name,
            target: Target::Object(id),
            peeled: None,
        }
        .attach(self))
    }

    /// Edit a single reference as described in `edit`, and write reference logs as `log_committer`.
    ///
    /// One or more `RefEdit`s  are returned - symbolic reference splits can cause more edits to be performed. All edits have the previous
    /// reference values set to the ones encountered at rest after acquiring the respective reference's lock.
    pub fn edit_reference(&self, edit: RefEdit) -> Result<Vec<RefEdit>> {
        self.edit_references(Some(edit))
    }

    /// Edit one or more references as described by their `edits`. This fails while `GIT_QUARANTINE_PATH` is set, as Git
    /// doesn't allow to change references in the hooks that check what is received, unless the repository was opened with
    /// [`ignore_repository_environment()`](crate::open::Options::ignore_repository_environment()).
    /// Note that one can set the committer name for use in the ref-log by temporarily
    /// [overriding the git-config](crate::Repository::config_snapshot_mut()), or use
    /// [`edit_references_as(committer)`](Self::edit_references_as()) for convenience.
    ///
    /// Returns all reference edits, which might be more than where provided due the splitting of symbolic references, and
    /// whose previous (_old_) values are the ones seen on in storage after the reference was locked.
    pub fn edit_references(&self, edits: impl IntoIterator<Item = RefEdit>) -> Result<Vec<RefEdit>> {
        self.edit_references_as(edits, self.committer().transpose()?)
    }

    /// A way to apply reference `edits` similar to [edit_references(…)](Self::edit_references()), but set a specific
    /// `commiter` for use in the reflog. It can be `None` if it's the purpose `edits` are configured to not update the
    /// reference log, or cause a failure otherwise.
    pub fn edit_references_as(
        &self,
        edits: impl IntoIterator<Item = RefEdit>,
        committer: Option<gix_actor::SignatureRef<'_>>,
    ) -> Result<Vec<RefEdit>> {
        self.edit_references_with_head_log(edits, committer, gix_ref::transaction::HeadLog::Git)
    }

    /// Like [`edit_references_as()`](Self::edit_references_as()), but decide with `head_log` how the reflog of `HEAD`
    /// follows the branch it points at, which is what Git does with [`HeadLog::Git`](gix_ref::transaction::HeadLog::Git).
    pub fn edit_references_with_head_log(
        &self,
        edits: impl IntoIterator<Item = RefEdit>,
        committer: Option<gix_actor::SignatureRef<'_>>,
        head_log: gix_ref::transaction::HeadLog,
    ) -> Result<Vec<RefEdit>> {
        self.edit_references_inner(edits, committer, head_log, false)
    }

    /// Like [`edit_references_as()`](Self::edit_references_as()), but run the `reference-transaction` hook the way
    /// `git update-ref` does it, so that a hook can watch and refuse the changes.
    ///
    /// The hook is called with `preparing`, `prepared` and then `committed`, or `aborted` if the transaction was refused or
    /// isn't committed, with a line `<old> <new> <name>` for each reference on standard input. A hook that fails
    /// in the first two states refuses the transaction, which then fails with
    /// [`Vetoed`](gix_ref::transaction::observe::Vetoed) as marker and changes nothing. The exit code is ignored in the
    /// last two states, as in Git. The sequence of calls is the one of Git, including the one for the transaction
    /// Git opens on `packed-refs` when it deletes references, and a hook that is configured with `hook.*` or `core.hooksPath` is found
    /// like [`hook()`](Self::hook()) finds it.
    ///
    /// Hooks are programs, so it's an error to use this in a repository that isn't fully trusted, as
    /// [`hook()`](Self::hook()) has it. Like all methods that edit references, this fails while `GIT_QUARANTINE_PATH` is
    /// set, as it is in the hooks that check what is received, where Git doesn't allow to change references either, unless the
    /// repository was opened with [`ignore_repository_environment()`](crate::open::Options::ignore_repository_environment()).
    #[cfg(feature = "hook")]
    pub fn edit_references_with_hooks(
        &self,
        edits: impl IntoIterator<Item = RefEdit>,
        committer: Option<gix_actor::SignatureRef<'_>>,
    ) -> Result<Vec<RefEdit>> {
        self.edit_references_inner(edits, committer, gix_ref::transaction::HeadLog::Git, true)
    }

    #[cfg_attr(not(feature = "hook"), allow(unused_variables, unused_mut))]
    fn edit_references_inner(
        &self,
        edits: impl IntoIterator<Item = RefEdit>,
        committer: Option<gix_actor::SignatureRef<'_>>,
        head_log: gix_ref::transaction::HeadLog,
        run_hooks: bool,
    ) -> Result<Vec<RefEdit>> {
        let (file_lock_fail, packed_refs_lock_fail) = self.config.lock_timeout().or_raise(|| {
            gix_error::message(
                "Could not interpret core.filesRefLockTimeout or core.packedRefsTimeout, it must be the number in \
                 milliseconds to wait for locks or negative to wait forever",
            )
        })?;
        // Git doesn't let a process that checks what is received, and has this variable, change references,
        // unless it was told to ignore the environment of the process that started it.
        if !self.options.ignores_repository_environment() && std::env::var_os("GIT_QUARANTINE_PATH").is_some() {
            gix_error::bail!(gix_error::message(
                "ref updates forbidden inside quarantine environment"
            ));
        }
        let mut transaction = self.ref_store().transaction().head_log(head_log);
        #[cfg(feature = "hook")]
        if run_hooks {
            // Fail early, and once, if hooks can't be run at all.
            self.hook("reference-transaction")?;
            transaction = transaction.observer(ReferenceTransactionHook {
                repo: self,
                side: gix_hook::Side::of_event("reference-transaction"),
                namespace: None,
                shallow_file: None,
            });
        }
        Ok(transaction
            .prepare(
                edits,
                gix_ref::view::transaction::LockOptions::new(file_lock_fail, packed_refs_lock_fail),
            )?
            .commit(committer)?
            .edits)
    }

    /// Return the repository head, an abstraction to help dealing with the `HEAD` reference.
    ///
    /// Symbolic references are followed to the final referent, without peeling annotated tags.
    ///
    /// The `HEAD` reference can be in various states, for more information, the documentation of [`Head`](crate::Head).
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    /// # mod doctest { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/doctest.rs")); }
    /// # let repo = doctest::open_repo(doctest::basic_repo_dir()?)?;
    /// let head = repo.head()?;
    ///
    /// assert_eq!(head.referent_name().expect("born"), "refs/heads/main");
    /// assert!(!head.is_detached());
    /// assert!(!head.is_unborn());
    /// # Ok(()) }
    /// ```
    pub fn head(&self) -> Result<crate::Head<'_>> {
        let mut head = self.find_reference("HEAD")?;
        Ok(match head.inner.target {
            Target::Symbolic(_) => match head.follow_to_object() {
                Ok(_) => crate::head::Kind::Symbolic(head.inner),
                Err(err) if err.is_not_found() => match head.inner.target {
                    Target::Symbolic(branch) => crate::head::Kind::Unborn(branch),
                    Target::Object(_) => return Err(err),
                },
                Err(err) => return Err(err),
            },
            Target::Object(target) => crate::head::Kind::Detached {
                target,
                peeled: head.inner.peeled,
            },
        }
        .attach(self))
    }

    /// Resolve the `HEAD` reference, follow and peel its target and obtain its object id,
    /// following symbolic references and tags until a commit is found.
    ///
    /// Note that this may fail for various reasons, most notably because the repository
    /// is freshly initialized and doesn't have any commits yet.
    ///
    /// Also note that the returned id is likely to point to a commit, but could also
    /// point to a tree or blob. It won't, however, point to a tag as these are always peeled.
    pub fn head_id(&self) -> Result<crate::Id<'_>> {
        self.head()?.into_peeled_id()
    }

    /// Return the name to the symbolic reference `HEAD` points to, or `None` if the head is detached.
    ///
    /// The difference to [`head_ref()`](Self::head_ref()) is that the latter requires the reference to exist,
    /// whereas here we merely return a the name of the possibly unborn reference.
    pub fn head_name(&self) -> Result<Option<FullName>> {
        Ok(self.head()?.referent_name().map(std::borrow::ToOwned::to_owned))
    }

    /// Return the reference that `HEAD` points to, or `None` if the head is detached or unborn.
    pub fn head_ref(&self) -> Result<Option<Reference<'_>>> {
        Ok(self.head()?.try_into_referent())
    }

    /// Return the commit object the `HEAD` reference currently points to after peeling it fully,
    /// following symbolic references and tags until a commit is found.
    ///
    /// Note that this may fail for various reasons, most notably because the repository
    /// is freshly initialized and doesn't have any commits yet. It could also fail if the
    /// head does not point to a commit.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    /// # mod doctest { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/doctest.rs")); }
    /// # let repo = doctest::open_repo(doctest::basic_repo_dir()?)?;
    /// let head = repo.head_commit()?;
    ///
    /// assert_eq!(head.decode()?.message, "c2\n");
    /// assert_eq!(repo.head_tree_id()?, head.tree_id()?);
    ///
    /// #[cfg(feature = "revision")]
    /// {
    ///     let previous = repo.rev_parse_single("HEAD^")?;
    ///     assert_ne!(previous, head.id);
    /// }
    /// # Ok(()) }
    /// ```
    pub fn head_commit(&self) -> Result<crate::Commit<'_>> {
        self.head()?.peel_to_commit()
    }

    /// Return the tree id the `HEAD` reference currently points to after peeling it fully,
    /// following symbolic references and tags until a commit is found.
    ///
    /// Note that this may fail for various reasons, most notably because the repository
    /// is freshly initialized and doesn't have any commits yet. It could also fail if the
    /// head does not point to a commit.
    pub fn head_tree_id(&self) -> Result<crate::Id<'_>> {
        self.head_commit()?.tree_id()
    }

    /// Like [`Self::head_tree_id()`], but will return an empty tree hash if the repository HEAD is unborn.
    pub fn head_tree_id_or_empty(&self) -> Result<crate::Id<'_>> {
        let mut head = self.head()?;
        if head.is_unborn() {
            Ok(self.empty_tree().id())
        } else {
            head.peel_to_commit()?.tree_id()
        }
    }

    /// Return the tree object the `HEAD^{tree}` reference currently points to after peeling it fully,
    /// following symbolic references and tags until a tree is found.
    ///
    /// Note that this may fail for various reasons, most notably because the repository
    /// is freshly initialized and doesn't have any commits yet. It could also fail if the
    /// head does not point to a tree, unlikely but possible.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    /// # mod doctest { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/doctest.rs")); }
    /// # let repo = doctest::open_repo(doctest::basic_repo_dir()?)?;
    /// let tree = repo.head_tree()?;
    ///
    /// assert_eq!(tree.find_entry("this").expect("present").filename(), "this");
    /// # Ok(()) }
    /// ```
    pub fn head_tree(&self) -> Result<crate::Tree<'_>> {
        self.head_commit()?.tree()
    }

    /// Find the reference with the given partial or full `name`, like `main`, `HEAD`, `heads/branch` or `origin/other`,
    /// or return an error if it wasn't found.
    ///
    /// Consider [`try_find_reference(…)`](crate::Repository::try_find_reference()) if the reference might not exist
    /// without that being considered an error.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    /// # mod doctest { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/doctest.rs")); }
    /// # let repo = doctest::open_repo(doctest::basic_repo_dir()?)?;
    /// let mut reference = repo.find_reference("main")?;
    ///
    /// assert_eq!(reference, "refs/heads/main");
    /// assert_eq!(reference.peel_to_commit()?.message()?.title, "c2");
    /// # Ok(()) }
    /// ```
    pub fn find_reference<'a, Name, E>(&self, name: Name) -> Result<Reference<'_>>
    where
        Name: TryInto<&'a PartialNameRef, Error = E>,
        std::result::Result<&'a PartialNameRef, E>: ResultExt<Success = &'a PartialNameRef>,
    {
        Ok(Reference::from_ref(self.ref_store().find(name)?, self))
    }

    /// Return a platform for iterating references.
    ///
    /// Common kinds of iteration are [all](crate::reference::iter::Platform::all()) or [prefixed](crate::reference::iter::Platform::prefixed())
    /// references.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    /// # mod doctest { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/doctest.rs")); }
    /// # let repo = doctest::open_repo(doctest::basic_repo_dir()?)?;
    /// let branches = repo
    ///     .references()?
    ///     .local_branches()?
    ///     .collect::<Result<Vec<_>, _>>()?;
    ///
    /// assert_eq!(branches, vec!["refs/heads/main".to_owned()]);
    /// # Ok(()) }
    /// ```
    pub fn references(&self) -> Result<reference::iter::Platform<'_>> {
        Ok(reference::iter::Platform {
            platform: self.ref_store().iter()?,
            repo: self,
        })
    }

    /// Try to find the reference named `name`, like `main`, `heads/branch`, `HEAD` or `origin/other`, and return it.
    ///
    /// Otherwise return `None` if the reference wasn't found.
    /// If the reference is expected to exist, use [`find_reference()`](crate::Repository::find_reference()).
    pub fn try_find_reference<'a, Name, E>(&self, name: Name) -> Result<Option<Reference<'_>>>
    where
        Name: TryInto<&'a PartialNameRef, Error = E>,
        std::result::Result<&'a PartialNameRef, E>: ResultExt<Success = &'a PartialNameRef>,
    {
        Ok(self.ref_store().try_find(name)?.map(|r| Reference::from_ref(r, self)))
    }
}

#[cfg(feature = "hook")]
/// Runs the `reference-transaction` hook for each state of a transaction, and refuses the transaction if the hook fails
/// in a state that can be refused.
pub(crate) struct ReferenceTransactionHook<'repo> {
    pub(crate) repo: &'repo crate::Repository,
    /// The side of the hook, which decides where it runs and what it sees.
    pub(crate) side: gix_hook::Side,
    /// The namespace to tell the hook about with `GIT_NAMESPACE`, if it's to be told.
    pub(crate) namespace: Option<String>,
    /// The file to tell the hook about with `GIT_SHALLOW_FILE`, if it's to be told.
    pub(crate) shallow_file: Option<std::path::PathBuf>,
}

#[cfg(feature = "hook")]
impl gix_ref::transaction::observe::Observer for ReferenceTransactionHook<'_> {
    fn observe(
        &mut self,
        event: &gix_ref::transaction::observe::Event<'_>,
    ) -> std::result::Result<(), gix_ref::transaction::observe::Veto> {
        use gix_ref::transaction::observe::{State, Veto};
        let can_refuse = matches!(event.state, State::Preparing | State::Prepared);
        let value = |target: Option<&Target>| -> BString {
            match target {
                None => self.repo.object_hash().null().to_string().into(),
                Some(Target::Object(id)) => id.to_string().into(),
                Some(Target::Symbolic(name)) => {
                    let mut value = BString::from("ref:");
                    value.extend_from_slice(name.as_bstr());
                    value
                }
            }
        };
        let mut input = BString::default();
        for update in event.updates {
            input.extend_from_slice(&value(update.previous.as_ref()));
            input.push(b' ');
            input.extend_from_slice(&value(update.new.as_ref()));
            input.push(b' ');
            input.extend_from_slice(update.name.as_bstr());
            input.push(b'\n');
        }
        let outcome = self
            .repo
            .hook_for("reference-transaction", self.side)
            .and_then(|mut hook| {
                if let Some(namespace) = &self.namespace {
                    hook = hook.env("GIT_NAMESPACE", namespace);
                }
                if let Some(file) = &self.shallow_file {
                    hook = hook.env("GIT_SHALLOW_FILE", file);
                }
                hook.arg(event.state.as_str())
                    .stdin(gix_hook::Stdin::Bytes(input.into()))
                    .run()
            });
        match outcome {
            Ok(outcome) if outcome.is_success() => Ok(()),
            _ if !can_refuse => Ok(()),
            _ => Err(Veto),
        }
    }
}
