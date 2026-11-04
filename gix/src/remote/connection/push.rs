use gix_error::{ResultExt, bail, message, not_found, validation};
use gix_hash::ObjectId;
use gix_protocol::push::ServerCapabilities;
use gix_transport::client::blocking_io::Transport;
use gix_utils::progress::Progress;

use crate::{
    Repository, Result,
    remote::{
        Connection, Direction,
        push::{self, Advertisement, Lease, Update, evaluate},
    },
};

impl<'repo, T> Connection<'_, '_, 'repo, T>
where
    T: Transport,
{
    /// Validate `updates`, read what the remote advertises for pushing, and decide for each update what is to be done about it,
    /// without sending anything yet. See the [`push` module](crate::remote::push) for how leases are treated.
    ///
    /// Updates are validated first, so that obviously wrong requests don't even connect. The handshake always uses the
    /// receive-pack service, and protocol version 0 or 1, which is all it speaks.
    pub fn prepare_push<'cb>(
        self,
        mut progress: impl Progress,
        updates: Vec<Update>,
        options: push::Options,
    ) -> Result<push::Prepare<'cb, 'repo, T>> {
        let repo = self.remote.repo;
        validate(repo, &updates)?;
        let remote_url = self
            .remote
            .url(Direction::Push)
            .map(gix_url::Url::to_bstring)
            .unwrap_or_default();
        let remote_name = match self.remote.name() {
            Some(name) => name.as_bstr().to_owned(),
            None => remote_url.clone(),
        };
        let trace = self.trace;
        let mut connection = self.into_detached();
        let handshake =
            connection.handshake_with(repo, gix_transport::Service::ReceivePack, Vec::new(), &mut progress)?;
        let server = ServerCapabilities::from_capabilities(&handshake.capabilities);
        let advertisement = Advertisement::from_refs(handshake.refs.as_deref().unwrap_or_default());
        let plan = evaluate(updates, &advertisement, &mut |lease, new| ancestry(repo, lease, new))?;
        Ok(push::Prepare {
            repo,
            transport: connection.transport,
            remote_name,
            remote_url,
            server,
            advertisement,
            plan,
            options,
            trace,
            before_send: None,
            pre_push_hook: false,
            on_remote_message: None,
        })
    }
}

impl<T> Connection<'_, '_, '_, T>
where
    T: Transport,
{
    /// Look at what the remote has for each update of `outcome` that [was sent](push::OutcomeEntry::was_sent()), and compare it with
    /// what the update expected and wanted. Use this when the outcome is [uncertain](push::Outcome::is_uncertain()), before
    /// retrying or recording anything as done or failed.
    ///
    /// It reads the references that `receive-pack` advertises, not those of `upload-pack`, as the two can hide different ones, and
    /// it sends no command. The server of the push has finished or was stopped before [`Prepare::send()`](push::Prepare::send())
    /// returned, so what is observed is final, except for what other writers do.
    ///
    /// Observations are not verdicts. Only [`Observed::AtNew`](push::Observed::AtNew) shows that an update happened, so it is the
    /// only one that permits to remove a marker that says it is pending. [`Observed::AtLease`](push::Observed::AtLease) permits
    /// another attempt, which must keep the original lease and is checked against the advertisement again by
    /// [`prepare_push()`](Self::prepare_push()), but can't tell an update that wasn't applied from one that was applied and reverted.
    pub fn reconcile(self, mut progress: impl Progress, outcome: &push::Outcome) -> Result<push::Reconciliation> {
        let repo = self.remote.repo;
        let mut connection = self.into_detached();
        let handshake =
            connection.handshake_with(repo, gix_transport::Service::ReceivePack, Vec::new(), &mut progress)?;
        let advertisement = Advertisement::from_refs(handshake.refs.as_deref().unwrap_or_default());
        // Nothing is going to be sent, which `receive-pack` is told with a flush, and its end is awaited so it doesn't linger.
        connection.transport.indicate_end_of_interaction().ok();
        connection
            .transport
            .inner
            .wait_for_server(std::time::Duration::from_secs(10))
            .ok();
        Ok(push::Reconciliation {
            observations: outcome
                .entries
                .iter()
                .filter(|entry| entry.was_sent())
                .map(|entry| push::Observation {
                    update: entry.update.clone(),
                    observed: push::Observed::of(
                        &entry.update,
                        advertisement.refs.get(entry.update.name().as_bstr()).copied(),
                    ),
                })
                .collect(),
        })
    }
}

/// Check what can be checked without talking to the remote.
fn validate(repo: &Repository, updates: &[Update]) -> Result<()> {
    if updates.is_empty() {
        bail!(validation("There must be at least one update to push"));
    }
    let kind = repo.object_hash();
    let mut seen = std::collections::HashSet::with_capacity(updates.len());
    for update in updates {
        let name = update.name().as_bstr();
        if !name.starts_with(b"refs/") {
            bail!(validation(format!("Reference '{name}' is not below 'refs/'")));
        }
        if !seen.insert(name) {
            bail!(validation(format!(
                "Reference '{name}' is the subject of more than one update"
            )));
        }
        let lease = match update.lease() {
            Lease::At(id) => Some(id),
            Lease::Absent => None,
        };
        let ids = update.new_id().into_iter().chain(lease.as_deref());
        for id in ids {
            if id.kind() != kind {
                bail!(validation(format!(
                    "The update of '{name}' uses {} ids, but the repository uses {kind}",
                    id.kind()
                )));
            }
            if id.is_null() {
                bail!(validation(format!(
                    "The update of '{name}' contains the null id, which is expressed with a lease of `Absent` or deletion instead"
                )));
            }
        }
        match (update.lease(), update.new_id()) {
            (Lease::Absent, None) => bail!(validation(format!(
                "'{name}' is to be deleted, but expected to be absent, which leaves nothing to do"
            ))),
            (_, Some(new)) if !repo.has_object(new) => bail!(not_found(format!(
                "The new value {new} of '{name}' is not in the object database"
            ))),
            _ => {}
        }
    }
    Ok(())
}

/// Tell if `lease` is an ancestor of `new`, as needed by updates that must fast-forward.
fn ancestry(repo: &Repository, lease: &gix_hash::oid, new: &gix_hash::oid) -> Result<push::plan::Ancestry> {
    use push::plan::Ancestry;
    let (lease, new): (ObjectId, ObjectId) = (lease.to_owned(), new.to_owned());
    let is_commit = |id: ObjectId| -> Result<Option<bool>> {
        Ok(repo
            .try_find_header(id)?
            .map(|header| header.kind() == gix_object::Kind::Commit))
    };
    match (is_commit(lease)?, is_commit(new)?) {
        (None, _) => return Ok(Ancestry::LeaseObjectMissing),
        (Some(true), Some(true)) => {}
        _ => return Ok(Ancestry::NotDescendant),
    }
    let cache = repo.commit_graph_if_enabled().ok().flatten();
    let mut graph = repo.revision_graph(cache.as_ref());
    let bases = gix_revision::merge_base(new, &[lease], &mut graph)
        .or_raise(|| message!("Could not determine if {lease} is an ancestor of {new}"))?;
    Ok(if bases.is_some_and(|bases| bases.contains(&lease)) {
        Ancestry::Descendant
    } else {
        Ancestry::NotDescendant
    })
}
