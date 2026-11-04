//! Transactions on references that are stored in reftables.
//!
//! A transaction locks the stacks it changes before it verifies anything, so that what it verified can't change under
//! it, and it adds one table to each of them when it is committed, which is when the committer is known. Stacks are
//! published one after the other, in the order in which they were locked, and a failure after the first stack was
//! published leaves the transaction applied in part, which the error says. Compacting the stacks afterwards is a
//! courtesy that never fails the transaction.
//!
//! The decisions of what to write are made without any storage in [`plan`].
use gix_error::{ErrorExt, Result, ResultExt, bail, message, validation};
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_reftable::{
    Stack,
    record::{LogRecord, LogValue, Record, RefRecord},
    stack::{Addition, Snapshot, UncertainAfterPublish},
};

use super::{
    StackOptions, Store,
    find::{is_special_head, reference_from_record},
    stacks::StackId,
};
use crate::{
    FullName, PartialNameRef, Target,
    file::log::create_or_update::MissingCommitter,
    transaction::{
        Change, ExistingRefs, PreviousValue, RefEdit, RefEditsExt, RefLog,
        commit_failure::{NothingApplied, Uncertain},
        observe::{Event, Observer, Scope, State, Update, Veto, Vetoed},
    },
};

mod plan;
use plan::{Lookup, Planned, Settings};
mod progress;

/// A stack that is locked by a transaction.
struct Locked {
    id: StackId,
    addition: Addition<'static>,
}

/// An edit with everything that is needed to write it.
struct Item {
    planned: Planned,
    /// The position of the stack that holds the reference in the order in which stacks are locked.
    stack: usize,
    /// The name of the reference in its stack.
    local: BString,
}

/// A transaction that wasn't prepared yet.
pub(crate) struct Transaction<'s, 'p> {
    store: &'s Store,
    observer: Option<Box<dyn Observer + 'p>>,
}

/// A transaction whose stacks are locked and whose edits are verified.
pub(crate) struct Prepared<'s, 'p> {
    store: &'s Store,
    options: StackOptions,
    /// The stacks in the order in which they were locked.
    locked: Vec<Locked>,
    items: Vec<Item>,
    notifier: Notifier<'p>,
}

/// Tells the observer about what happens to the transaction, which is reported as aborted if it is dropped once prepared.
#[derive(Default)]
struct Notifier<'p> {
    observer: Option<Box<dyn Observer + 'p>>,
    prepared: Option<Vec<Update>>,
}

impl Notifier<'_> {
    fn notify(&mut self, state: State, updates: &[Update]) -> std::result::Result<(), Vetoed> {
        let Some(observer) = self.observer.as_mut() else {
            return Ok(());
        };
        match observer.observe(&Event {
            state,
            scope: Scope::Transaction,
            updates,
        }) {
            Err(Veto) if matches!(state, State::Preparing | State::Prepared) => Err(Vetoed {
                state,
                scope: Scope::Transaction,
            }),
            Ok(()) | Err(Veto) => Ok(()),
        }
    }

    fn is_observing(&self) -> bool {
        self.observer.is_some()
    }

    /// Nothing is locked yet. A veto ends the transaction.
    fn preparing(&mut self, updates: &[Update]) -> std::result::Result<(), Vetoed> {
        self.notify(State::Preparing, updates).inspect_err(|_| {
            self.notify(State::Aborted, updates).ok();
        })
    }

    /// Everything is locked and verified. A veto leaves it to the caller to release the locks and drop the notifier, which
    /// then reports that the transaction was aborted.
    fn prepared(&mut self, updates: Vec<Update>) -> std::result::Result<(), Vetoed> {
        if !self.is_observing() {
            return Ok(());
        }
        let result = self.notify(State::Prepared, &updates);
        self.prepared = Some(updates);
        result
    }

    fn committed(&mut self) {
        if let Some(updates) = self.prepared.take() {
            self.notify(State::Committed, &updates).ok();
        }
    }

    /// Forget about the transaction without telling anybody more, as failing to commit does.
    fn disarm(&mut self) {
        self.prepared = None;
    }
}

impl Drop for Notifier<'_> {
    fn drop(&mut self) {
        if let Some(updates) = self.prepared.take() {
            self.notify(State::Aborted, &updates).ok();
        }
    }
}

impl<'s, 'p> Transaction<'s, 'p> {
    pub(crate) fn new(store: &'s Store) -> Self {
        Transaction { store, observer: None }
    }

    pub(crate) fn observer(mut self, observer: impl Observer + 'p) -> Self {
        self.observer = Some(Box::new(observer));
        self
    }

    /// Lock the stacks, verify the `edits` and decide what to write. Nothing is changed if this fails.
    pub(crate) fn prepare(self, edits: impl IntoIterator<Item = RefEdit>) -> Result<Prepared<'s, 'p>> {
        let Transaction { store, observer } = self;
        store.assure_backend_supported()?;
        let options = *store.stack_options()?;
        let mut edits: Vec<RefEdit> = edits.into_iter().collect();
        let settings = Settings {
            hash: store.object_hash(),
            write_reflog: store.write_reflog,
            message_limit: effective_block_size(&options) / 2,
            namespace: store.namespace.as_ref(),
        };
        let duplicate = |name: BString| validation(format!("A reference named '{name}' has multiple edits")).raise();
        edits.assure_one_name_has_one_edit().map_err(duplicate)?;
        for edit in &edits {
            if is_special_head(edit.name.as_bstr()) {
                bail!(
                    gix_error::unsupported(
                        "References that Git keeps in files, like FETCH_HEAD, can't be edited in reftables"
                    )
                    .with("reference", edit.name.as_bstr())
                );
            }
        }

        let mut notifier = Notifier {
            observer,
            prepared: None,
        };
        if notifier.is_observing() {
            let requested = updates(&edits, &edits.iter().map(expected).collect::<Vec<_>>());
            if let Err(vetoed) = notifier.preparing(&requested) {
                bail!(vetoed);
            }
        }

        let mut locked: Vec<Locked> = Vec::new();
        for edit in &edits {
            lock(
                store,
                &options,
                &mut locked,
                settings.stored(edit.name.as_ref()).as_bstr(),
            )?;
        }
        // Symbolic references are split by looking at what is locked, as what isn't there at the time can't be relied upon.
        let mut failure = None;
        let split = |locked: &mut Vec<Locked>, name: &PartialNameRef| -> Result<Option<Target>> {
            let stored = settings.stored_bstr(name.as_bstr());
            let id = lock(store, &options, locked, stored.as_ref())?;
            let (_, local) = store.route(stored.as_ref());
            let record = locked[id].addition.snapshot().find_ref(local).or_raise(|| {
                message("Could not look up a reference in a reftable").with("reference", stored.clone())
            })?;
            Ok(record
                .map(|record| reference_from_record(stored.as_ref(), record.value))
                .transpose()?
                .flatten()
                .map(|reference| reference.target))
        };
        edits
            .extend_with_splits_of_symbolic_refs(
                &mut |name| {
                    if failure.is_some() {
                        return None;
                    }
                    match split(&mut locked, name) {
                        Ok(target) => target,
                        Err(err) => {
                            failure = Some(err);
                            None
                        }
                    }
                },
                &mut |_, edit| edit,
            )
            .or_raise(|| message("Could not preprocess reference edits"))?;
        if let Some(err) = failure {
            return Err(err);
        }
        edits.assure_one_name_has_one_edit().map_err(duplicate)?;

        let head_referent = head_referent(store, &locked, &settings)?;
        if let Some(head) = plan::head_log_edit(&edits, head_referent.as_ref())? {
            lock(
                store,
                &options,
                &mut locked,
                settings.stored(head.name.as_ref()).as_bstr(),
            )?;
            edits.push(head);
        }

        // What each caller expects is lost while verifying, so remember it now.
        let previous: Vec<_> = edits.iter().map(expected).collect();
        let lookup = Existing { store, locked: &locked };
        let planned = plan::plan(edits, &settings, &lookup)?;
        let observed = {
            let edits: Vec<_> = planned.iter().map(|planned| planned.edit.clone()).collect();
            updates(&edits, &previous)
        };
        let items = planned
            .into_iter()
            .map(|planned| {
                let (id, local) = store.route(planned.stored.as_ref());
                let local = local.to_owned();
                let stack = locked
                    .iter()
                    .position(|locked| locked.id == id)
                    .expect("every stack that is routed to was locked");
                Item { planned, stack, local }
            })
            .collect();
        if let Err(vetoed) = notifier.prepared(observed) {
            // The locks are released first, and the observer is told about the abort when the notifier is dropped.
            drop(locked);
            drop(notifier);
            bail!(vetoed);
        }
        Ok(Prepared {
            store,
            options,
            locked,
            items,
            notifier,
        })
    }
}

impl Prepared<'_, '_> {
    /// Write and publish the tables, using `committer` for the entries of reflogs.
    pub(crate) fn commit(self, committer: Option<gix_actor::SignatureRef<'_>>) -> Result<Vec<RefEdit>> {
        let Prepared {
            store,
            options,
            mut locked,
            items,
            mut notifier,
        } = self;
        let stack_of_edit: Vec<_> = items.iter().map(|item| item.stack).collect();

        // Writing every table first means that failing to write one doesn't leave the others published.
        if let Err(err) = write_tables(&mut locked, &items, committer) {
            drop(locked);
            // The observer is told that nothing happened after the locks are released, as it is for files.
            drop(notifier);
            return Err(err.and_raise(NothingApplied));
        }

        let mut published = Vec::with_capacity(locked.len());
        let mut locked = locked.into_iter();
        let mut position = 0;
        while let Some(stack) = locked.next() {
            let Locked { id, addition } = stack;
            match addition.commit() {
                Ok(replaced_list) => {
                    if replaced_list {
                        published.push(id);
                    }
                }
                Err(err) => {
                    drop(locked);
                    let failed = if err.downcast_any_ref::<UncertainAfterPublish>().is_some() {
                        progress::Failed::Uncertain
                    } else {
                        progress::Failed::Cleanly
                    };
                    return Err(
                        match progress::outcome(&stack_of_edit, position, !published.is_empty(), failed) {
                            progress::Outcome::NothingApplied => {
                                drop(notifier);
                                err.and_raise(NothingApplied)
                            }
                            progress::Outcome::Partial(progress) => {
                                notifier.disarm();
                                err.and_raise(Uncertain::new(
                                    items
                                        .iter()
                                        .zip(progress)
                                        .map(|(item, progress)| (item.planned.edit.name.clone(), progress))
                                        .collect(),
                                ))
                            }
                        },
                    );
                }
            }
            position += 1;
        }

        for id in &published {
            compact(store, &options, id);
        }
        notifier.committed();
        Ok(items.into_iter().map(|item| item.planned.edit).collect())
    }

    /// Release all locks and return the edits as they were verified, without changing anything.
    pub(crate) fn rollback(self) -> Vec<RefEdit> {
        let Prepared { locked, items, .. } = self;
        drop(locked);
        items.into_iter().map(|item| item.planned.edit).collect()
    }
}

/// The size blocks are limited to, which is the default if it wasn't configured.
fn effective_block_size(options: &StackOptions) -> usize {
    match options.write.block_size {
        0 => 4096,
        size => usize::try_from(size).unwrap_or(usize::MAX),
    }
}

/// Lock the stack that holds the reference `stored`, unless that is done already, and return its position.
fn lock(store: &Store, options: &StackOptions, locked: &mut Vec<Locked>, stored: &BStr) -> Result<usize> {
    let (id, local) = store.route(stored);
    if local.is_empty() {
        bail!(validation("The name doesn't refer to a reference").with("reference", stored.to_owned()));
    }
    if let Some(position) = locked.iter().position(|locked| locked.id == id) {
        return Ok(position);
    }
    let cell = store.stacks.cell(&id)?;
    let snapshot = cell.snapshot(false)?;
    let dir = cell.dir().to_owned();
    let addition = Stack::from_snapshot(&dir, store.object_hash(), *options, snapshot)
        .into_addition()
        .or_raise(|| message("Could not lock the reftable stack").with("path", dir))?;
    locked.push(Locked { id, addition });
    Ok(locked.len() - 1)
}

/// Return the reference that `HEAD` points to, which is the one whose update `HEAD` logs, as read from the stack of `HEAD`.
fn head_referent(store: &Store, locked: &[Locked], settings: &Settings<'_>) -> Result<Option<FullName>> {
    let head = settings.stored_bstr("HEAD".into());
    let (id, local) = store.route(head.as_ref());
    let snapshot = match locked.iter().find(|locked| locked.id == id) {
        Some(locked) => locked.addition.snapshot().clone(),
        None => store.stack_snapshot(&id, true)?,
    };
    let record = snapshot
        .find_ref(local)
        .or_raise(|| message("Could not look up a reference in a reftable").with("reference", head.clone()))?;
    Ok(record
        .map(|record| reference_from_record(head.as_ref(), record.value))
        .transpose()?
        .flatten()
        .and_then(|reference| match reference.target {
            Target::Symbolic(name) => Some(name),
            Target::Object(_) => None,
        }))
}

/// The value that the caller of `edit` expected the reference to have, if any.
fn expected(edit: &RefEdit) -> Option<Target> {
    match &edit.change {
        Change::Update { expected, .. } | Change::Delete { expected, .. } => match expected {
            PreviousValue::MustExistAndMatch(target) | PreviousValue::ExistingMustMatch(target) => Some(target.clone()),
            PreviousValue::Any | PreviousValue::MustExist | PreviousValue::MustNotExist => None,
        },
    }
}

/// The updates that `edits` stand for, with the expected values `previous` that go with them, leaving out edits that only
/// change reflogs.
fn updates(edits: &[RefEdit], previous: &[Option<Target>]) -> Vec<Update> {
    edits
        .iter()
        .zip(previous)
        .filter_map(|(edit, previous)| {
            let (mode, new) = match &edit.change {
                Change::Delete { log, .. } => (*log, None),
                Change::Update { log, new, .. } => (log.mode, Some(new.clone())),
            };
            (mode != RefLog::Only).then(|| Update {
                name: edit.name.clone(),
                previous: previous.clone(),
                new,
            })
        })
        .collect()
}

/// What the stacks of a transaction say about references, from the locked snapshots where there are any.
struct Existing<'a> {
    store: &'a Store,
    locked: &'a [Locked],
}

impl Existing<'_> {
    fn snapshot(&self, id: &StackId) -> Result<Snapshot> {
        match self.locked.iter().find(|locked| &locked.id == id) {
            Some(locked) => Ok(locked.addition.snapshot().clone()),
            None => self.store.stack_snapshot(id, true),
        }
    }
}

impl ExistingRefs for Existing<'_> {
    fn exists(&self, name: &BStr) -> Result<bool> {
        Ok(self.current(name)?.is_some())
    }

    fn any_below(&self, directory: &BStr) -> Result<Option<BString>> {
        // References below a directory may be in any stack: `refs/worktree` is a file of the shared stack, `refs/worktree/a` is
        // private to a worktree.
        let mut ids = vec![StackId::Main];
        if self.store.has_worktree_stack() {
            ids.push(StackId::Worktree);
        }
        for id in ids {
            let snapshot = self.snapshot(&id)?;
            let mut iter = snapshot
                .merged()
                .ref_iter()
                .or_raise(|| message("Could not iterate a reftable"))?;
            iter.seek_key(directory)
                .or_raise(|| message("Could not iterate a reftable"))?;
            if let Some(Record::Ref(record)) = iter
                .next_record()
                .or_raise(|| message("Could not iterate a reftable"))?
                && record.refname.starts_with(directory.as_bytes())
            {
                return Ok(Some(record.refname));
            }
        }
        Ok(None)
    }
}

impl Lookup for Existing<'_> {
    fn current(&self, name: &BStr) -> Result<Option<Target>> {
        let (id, local) = self.store.route(name);
        if local.is_empty() {
            return Ok(None);
        }
        let record = self
            .snapshot(&id)?
            .find_ref(local)
            .or_raise(|| message("Could not look up a reference in a reftable").with("reference", name.to_owned()))?;
        Ok(record
            .map(|record| reference_from_record(name, record.value))
            .transpose()?
            .flatten()
            .map(|reference| reference.target))
    }

    fn resolve(&self, name: &BStr) -> Result<Option<gix_hash::ObjectId>> {
        let mut name = name.to_owned();
        for _ in 0..=5 {
            match self.current(name.as_ref())? {
                Some(Target::Object(id)) => return Ok(Some(id)),
                Some(Target::Symbolic(target)) => name = target.0,
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    fn reflog_exists(&self, name: &BStr) -> Result<bool> {
        Ok(!self.reflog_indices(name)?.is_empty())
    }

    fn reflog_indices(&self, name: &BStr) -> Result<Vec<u64>> {
        let (id, local) = self.store.route(name);
        let snapshot = self.snapshot(&id)?;
        let mut indices = Vec::new();
        for record in snapshot
            .reflog(local)
            .or_raise(|| message("Could not read a reflog").with("reference", name.to_owned()))?
        {
            let record = record?;
            if matches!(record.value, LogValue::Update { .. }) {
                indices.push(record.update_index);
            }
        }
        Ok(indices)
    }
}

/// Write one table for every locked stack that has something to change, which isn't visible yet.
fn write_tables(locked: &mut [Locked], items: &[Item], committer: Option<gix_actor::SignatureRef<'_>>) -> Result<()> {
    let needs_committer = items
        .iter()
        .any(|item| matches!(item.planned.log, plan::Log::Entry { .. }));
    let committer = match (committer, needs_committer) {
        (Some(committer), _) => Some(identity(committer)?),
        (None, true) => bail!(MissingCommitter.and_raise(message("Could not update reflog"))),
        (None, false) => None,
    };
    for (position, stack) in locked.iter_mut().enumerate() {
        let mine: Vec<_> = items.iter().filter(|item| item.stack == position).collect();
        stack.addition.add_table(|writer| {
            // Like Git does, everything of a transaction has the one update index of the table. A name appears once in a
            // transaction, and thus once per update index, so this doesn't make records indistinguishable.
            let first = writer.limits().0;
            writer.set_limits(first, first)?;
            let mut refs: Vec<_> = mine
                .iter()
                .filter_map(|item| item.planned.record.clone().map(|value| (item.local.clone(), value)))
                .collect();
            refs.sort_by(|a, b| a.0.cmp(&b.0));
            for (refname, value) in refs {
                writer.add_ref(RefRecord {
                    refname,
                    update_index: first,
                    value,
                })?;
            }
            let mut logs = Vec::new();
            for item in &mine {
                match &item.planned.log {
                    plan::Log::None => {}
                    plan::Log::Delete(indices) => logs.extend(indices.iter().map(|update_index| LogRecord {
                        refname: item.local.clone(),
                        update_index: *update_index,
                        value: LogValue::Deletion,
                    })),
                    plan::Log::Entry { old, new, message } => {
                        let who = committer.as_ref().expect("checked above");
                        logs.push(LogRecord {
                            refname: item.local.clone(),
                            update_index: first,
                            value: LogValue::Update {
                                old_hash: *old,
                                new_hash: *new,
                                name: who.name.clone(),
                                email: who.email.clone(),
                                time: who.seconds,
                                tz_offset: who.tz_offset,
                                message: message.clone(),
                            },
                        });
                    }
                }
            }
            logs.sort_by(|a, b| a.refname.cmp(&b.refname).then(b.update_index.cmp(&a.update_index)));
            for log in logs {
                writer.add_log(log)?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// The committer in the form in which tables store it.
struct Identity {
    name: BString,
    email: BString,
    seconds: u64,
    tz_offset: i16,
}

fn identity(committer: gix_actor::SignatureRef<'_>) -> Result<Identity> {
    let time = committer
        .time()
        .or_raise(|| message("The time of the committer is not valid"))?;
    let offset = time.offset;
    let hhmm = (offset.abs() / 3600) * 100 + (offset.abs() % 3600) / 60;
    Ok(Identity {
        name: committer.name.to_owned(),
        email: committer.email.to_owned(),
        seconds: u64::try_from(time.seconds).unwrap_or(0),
        tz_offset: i16::try_from(if offset < 0 { -hhmm } else { hhmm }).unwrap_or(0),
    })
}

/// Merge tables like Git does after a transaction, ignoring whatever goes wrong, as the transaction succeeded.
pub(super) fn compact(store: &Store, options: &StackOptions, id: &StackId) {
    let Ok(cell) = store.stacks.cell(id) else { return };
    let Ok(snapshot) = store.stack_snapshot(id, true) else {
        return;
    };
    Stack::from_snapshot(cell.dir(), store.object_hash(), *options, snapshot)
        .auto_compact()
        .ok();
}
