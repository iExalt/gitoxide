use std::{sync::atomic::AtomicBool, time::Duration};

use crate::bstr::{BStr, BString};
use gix_error::{ErrorExt, Result, ResultExt, bail, message, not_found, unsupported, validation};
use gix_hash::ObjectId;
use gix_protocol::push::{Command, ServerCapabilities, StatusFormat};
use gix_transport::client::blocking_io::Transport;

use super::{Decision, Entry, Lease, Outcome, Plan, Status, outcome, plan::Advertisement};
use crate::Repository;

/// Choices about how to push.
#[derive(Debug, Clone)]
pub struct Options {
    /// Ask the server to apply all updates or none, which is the default. The push is then refused, without sending anything, if
    /// any update is [refused](super::Refusal). Without it, refused updates are skipped and the others are sent.
    pub atomic: bool,
    /// Push options for the hooks of the server. The push is refused if the server doesn't accept them.
    pub push_options: Vec<BString>,
    /// How long to wait for the server to finish after it replied or the push failed, which is 60 seconds by default.
    /// Servers run hooks after they replied, and a server that is still running when the connection is dropped may be killed.
    pub server_wait: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            atomic: true,
            push_options: Vec::new(),
            server_wait: Duration::from_secs(60),
        }
    }
}

/// The error returned if the history to push reaches the boundary of a shallow repository, behind which commits are missing.
///
/// This is more careful than Git, which sends the commits it has and lets the server check if it has what is missing.
/// The refusal doesn't know what the remote has beyond the references it advertises, so it can refuse a push that
/// the remote could accept, for example when it covers the boundary only through a reference whose commit isn't available locally.
#[derive(Debug)]
pub struct ShallowSource {
    /// The reference whose new value can't be sent.
    pub ref_name: BString,
    /// The shallow commit the history reaches, which hasn't been hidden by something the remote already has.
    pub shallow_commit: ObjectId,
}

impl std::fmt::Display for ShallowSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The history of '{}' reaches commit {}, the boundary of this shallow repository, behind which commits are missing that the remote doesn't have",
            self.ref_name, self.shallow_commit
        )
    }
}

impl std::error::Error for ShallowSource {}

/// What a `pre-push` hook gets to see, as Git passes it: the name and URL of the remote as arguments, and one line per update on standard input.
#[derive(Debug)]
pub struct PrePush<'a> {
    /// The name of the remote, or its URL if it has no name, which is what Git passes as first argument.
    pub remote_name: &'a BStr,
    /// The URL that is pushed to, which is the second argument.
    pub remote_url: &'a BStr,
    /// The updates that are about to be sent.
    pub entries: Vec<PrePushEntry>,
}

/// An update that is about to be sent, as a `pre-push` hook sees it.
#[derive(Debug)]
pub struct PrePushEntry {
    /// The local reference or the id that is pushed, or `(delete)` for a deletion.
    pub local_ref: BString,
    /// The id that is pushed, or the null id for a deletion.
    pub local_id: ObjectId,
    /// The reference on the remote.
    pub remote_ref: BString,
    /// The value of the remote reference, or the null id if it doesn't exist.
    pub remote_id: ObjectId,
}

impl PrePush<'_> {
    /// The standard input of a `pre-push` hook: `<local ref> <local id> <remote ref> <remote id>` per line.
    pub fn stdin(&self) -> BString {
        let mut out = BString::default();
        for entry in &self.entries {
            out.extend_from_slice(
                format!(
                    "{} {} {} {}\n",
                    entry.local_ref, entry.local_id, entry.remote_ref, entry.remote_id
                )
                .as_bytes(),
            );
        }
        out
    }
}

type BeforeSend<'cb> = Box<dyn FnOnce(&PrePush<'_>) -> Result<()> + 'cb>;
type RemoteMessages<'cb> = Box<dyn FnMut(bool, &[u8]) + 'cb>;

/// A push whose updates were validated and compared with what the remote advertised, which is ready to be [sent](Self::send()).
///
/// Dropping it sends nothing.
pub struct Prepare<'cb, 'repo, T>
where
    T: Transport,
{
    pub(crate) repo: &'repo Repository,
    pub(crate) transport: gix_protocol::SendFlushOnDrop<T>,
    pub(crate) remote_name: BString,
    pub(crate) remote_url: BString,
    pub(crate) server: ServerCapabilities,
    pub(crate) advertisement: Advertisement,
    pub(crate) plan: Plan,
    pub(crate) options: Options,
    pub(crate) trace: bool,
    pub(crate) before_send: Option<BeforeSend<'cb>>,
    pub(crate) on_remote_message: Option<RemoteMessages<'cb>>,
}

impl<'cb, T> Prepare<'cb, '_, T>
where
    T: Transport,
{
    /// Return a handle that ends what the push blocks on from another thread, if the transport has one.
    ///
    /// [`send()`](Self::send()) uses it itself to end the push when its `should_interrupt` flag is set, so this is for
    /// code that wants to cancel on its own terms.
    pub fn interrupt_handle(&self) -> Option<gix_protocol::transport::client::InterruptHandle> {
        gix_protocol::transport::client::TransportWithoutIO::interrupt_handle(&self.transport.inner)
    }

    /// What is going to happen to each update.
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// Run `check` once everything is ready to be sent but before anything is, like Git runs its `pre-push` hook. If it fails,
    /// its error is returned from [`send()`](Self::send()), and nothing was sent.
    pub fn with_before_send(mut self, check: impl FnOnce(&PrePush<'_>) -> Result<()> + 'cb) -> Self {
        self.before_send = Some(Box::new(check));
        self
    }

    /// Call `on_message` with the messages of the server as `(is_error, text)`.
    pub fn with_remote_messages(mut self, on_message: impl FnMut(bool, &[u8]) + 'cb) -> Self {
        self.on_remote_message = Some(Box::new(on_message));
        self
    }

    /// Send what the [plan](Self::plan()) allows.
    ///
    /// An error means that nothing was sent: the push was atomic and some update was refused, the check of
    /// [`with_before_send()`](Self::with_before_send()) failed, the server doesn't support what was asked for, or the pack
    /// couldn't be produced. If the request was sent, the result is an [`Outcome`], even if no usable report came back.
    pub fn send(mut self, should_interrupt: &AtomicBool) -> Result<Outcome> {
        let repo = self.repo;
        let refused: Vec<_> = self.plan.refused().collect();
        if self.options.atomic && !refused.is_empty() {
            bail!(validation(format!(
                "The push is atomic, but {} update(s) were refused: {}",
                refused.len(),
                refused
                    .iter()
                    .map(|(entry, refusal)| format!("{} ({refusal:?})", entry.update.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let to_send: Vec<&Entry> = self.plan.to_send().collect();
        if to_send.is_empty() {
            return Ok(self.finish(None, false));
        }
        if should_interrupt.load(std::sync::atomic::Ordering::SeqCst) {
            // Nothing was sent, so this is a plain failure rather than an outcome that has to be reconciled.
            bail!(gix_error::cancelled(
                "The push was interrupted before anything was sent"
            ));
        }

        let null = repo.object_hash().null();
        let commands = to_send
            .iter()
            .map(|entry| {
                let old_id = match entry.update.lease {
                    Lease::At(id) => id,
                    Lease::Absent => null,
                };
                Command::new(
                    entry.update.name.as_bstr().to_owned(),
                    old_id,
                    entry.update.new.unwrap_or(null),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let request = gix_protocol::push::Request::new(
            commands,
            repo.object_hash(),
            &self.server,
            gix_protocol::push::Options {
                status_format: if self.server.report_status_v2 {
                    StatusFormat::V2
                } else {
                    StatusFormat::V1
                },
                atomic: self.options.atomic && to_send.len() > 1,
                push_options: self.options.push_options.clone(),
                progress: false,
                agent: repo.config.user_agent_tuple().1,
            },
        )?;

        if let Some(check) = self.before_send.take() {
            let pre_push = PrePush {
                remote_name: self.remote_name.as_ref(),
                remote_url: self.remote_url.as_ref(),
                entries: to_send
                    .iter()
                    .map(|entry| PrePushEntry {
                        local_ref: match entry.update.new {
                            Some(new) => entry
                                .update
                                .local_name
                                .clone()
                                .unwrap_or_else(|| new.to_string().into()),
                            None => "(delete)".into(),
                        },
                        local_id: entry.update.new.unwrap_or(null),
                        remote_ref: entry.update.name.as_bstr().to_owned(),
                        remote_id: match entry.update.lease {
                            Lease::At(id) => id,
                            Lease::Absent => null,
                        },
                    })
                    .collect(),
            };
            check(&pre_push).or_raise(|| message("The check before sending the push failed, so nothing was sent"))?;
        }

        let pack = if request.expects_pack() {
            let tips: Vec<(ObjectId, &Entry)> = to_send
                .iter()
                .filter_map(|entry| entry.update.new.map(|new| (new, *entry)))
                .collect();
            refuse_shallow_source(repo, &tips, &self.advertisement.haves)?;
            let tip_ids: Vec<ObjectId> = tips.iter().map(|(id, _)| *id).collect();
            let options = gix_pack::data::output::missing::Options {
                allow_ofs_delta: request.ofs_delta_allowed(),
                ..Default::default()
            };
            if repo.objects.num_objects_in_memory() > 0 {
                bail!(unsupported(
                    "Objects that only exist in memory can't be pushed, as they can't be packed"
                ));
            }
            let mut db = repo
                .objects
                .clone()
                .into_arc()
                .or_raise(|| message("Could not prepare the object database for reading"))?
                .into_inner();
            // The pack is written from locations of objects that were determined before, which must not go away if maintenance
            // replaces packs in the meantime.
            db.prevent_pack_unload();
            let counts = gix_pack::data::output::missing::counts(
                db.clone(),
                &tip_ids,
                &self.advertisement.haves,
                &gix_utils::progress::Discard,
                should_interrupt,
                options,
            )
            .or_raise(|| message("Could not determine which objects to send, so nothing was sent"))?;
            Some((db, counts, options))
        } else {
            None
        };

        let mut pack_failed = false;
        let mut on_message = self.on_remote_message.take();
        // A peer that stopped responding blocks writes and reads that don't look at the flag, so the transport is ended
        // from a thread of its own if the flag is set. An interrupted push is still reported as one that failed or whose
        // outcome is uncertain, depending on how far it got.
        let interrupt = self.interrupt_handle();
        let trace = self.trace;
        let transport = &mut self.transport.inner;
        let push = || {
            gix_protocol::push::push(
                transport,
                &request,
                |out| match pack.map(|(db, counts, options)| {
                    gix_pack::data::output::missing::write_pack(db, counts, out, repo.object_hash(), options)
                }) {
                    Some(Err(err)) => {
                        pack_failed = true;
                        Err(err)
                    }
                    _ => Ok(()),
                },
                on_message
                    .as_mut()
                    .map(|callback| &mut **callback as &mut dyn FnMut(bool, &[u8])),
                trace,
            )
        };
        // The hook and the counting may have taken a while, and nothing was sent so far.
        if should_interrupt.load(std::sync::atomic::Ordering::SeqCst) {
            bail!(gix_error::cancelled(
                "The push was interrupted before anything was sent"
            ));
        }
        let result = match &interrupt {
            Some(handle) => handle.watch(should_interrupt, push),
            None => push(),
        };
        // The server may still run hooks, or be waiting for what we didn't send. Either way, we let it finish so we don't
        // interrupt whatever it does, which also is a prerequisite for looking at its state afterwards.
        let server_finished = self
            .transport
            .inner
            .wait_for_server(self.options.server_wait)
            .unwrap_or(false);
        match result {
            Ok(report) => Ok(self.finish(Some(Ok(report)), server_finished)),
            Err(err)
                if err
                    .downcast_any_ref::<gix_protocol::push::PackProducerFailedAfterCompletion>()
                    .is_some()
                    || (!pack_failed && err.classify().is_corrupted()) =>
            {
                Ok(self.finish(Some(Err(err)), server_finished))
            }
            Err(err) => Err(err.and_raise(message("The push failed before it could have changed the remote"))),
        }
    }

    /// Combine the plan with what the server said, if anything was sent.
    fn finish(self, sent: Option<Result<gix_protocol::push::Report>>, server_finished: bool) -> Outcome {
        let mut reported = std::collections::HashMap::new();
        let (unpack, uncertain) = match sent {
            Some(Ok(report)) => {
                for command in report.commands {
                    reported.insert(command.ref_name, command.status);
                }
                (Some(report.unpack), None)
            }
            Some(Err(err)) => (None, Some(err)),
            None => (None, None),
        };
        let entries = self
            .plan
            .entries
            .into_iter()
            .map(|entry| {
                let status = match entry.decision {
                    Decision::UpToDate => Status::UpToDate,
                    Decision::Refused(refusal) => Status::Refused(refusal),
                    Decision::Send => match reported.remove(entry.update.name.as_bstr()) {
                        Some(gix_protocol::push::report::Status::Accepted { updates }) => Status::Applied { updates },
                        Some(gix_protocol::push::report::Status::Rejected { reason }) => Status::Rejected { reason },
                        Some(gix_protocol::push::report::Status::Uncertain { reason }) => Status::Uncertain { reason },
                        None => Status::Unknown,
                    },
                };
                outcome::Entry {
                    update: entry.update,
                    status,
                }
            })
            .collect();
        Outcome {
            entries,
            unpack,
            uncertain,
            server_finished,
        }
    }
}

/// Fail with a [`ShallowSource`] error if the history of one of the `tips`, up to what the remote has, includes the boundary of a shallow repository.
fn refuse_shallow_source(repo: &Repository, tips: &[(ObjectId, &Entry)], haves: &[ObjectId]) -> Result<()> {
    let Some(shallow) = repo.shallow_commits()? else {
        return Ok(());
    };
    let hidden: Vec<ObjectId> = haves
        .iter()
        .filter_map(|id| peel_to_commit(repo, *id).ok().flatten())
        .collect();
    for (tip, entry) in tips {
        let Some(commit_id) = peel_to_commit(repo, *tip)? else {
            continue;
        };
        let walk = gix_traverse::commit::Simple::new([commit_id], &repo.objects)
            .hide(hidden.iter().copied())
            .or_raise(|| message("Could not set up the walk to find the boundary of the shallow repository"))?;
        for info in walk {
            let info = info.or_raise(|| message("Could not walk the history to send"))?;
            if shallow.contains(&info.id) {
                return Err(ShallowSource {
                    ref_name: entry.update.name.as_bstr().to_owned(),
                    shallow_commit: info.id,
                }
                .and_raise(unsupported(
                    "A shallow repository can't send the history behind its boundary",
                )));
            }
        }
    }
    Ok(())
}

/// Follow tags from `id` to the commit they point to, or return `None` if the object is missing or isn't a commit eventually.
fn peel_to_commit(repo: &Repository, mut id: ObjectId) -> Result<Option<ObjectId>> {
    for _ in 0..10_000 {
        let Some(object) = repo.try_find_object(id)? else {
            return Ok(None);
        };
        match object.kind {
            gix_object::Kind::Commit => return Ok(Some(id)),
            gix_object::Kind::Tag => {
                id = gix_object::TagRefIter::from_bytes(&object.data, repo.object_hash())
                    .target_id()
                    .or_raise(|| message!("Tag {id} has no valid target"))?;
            }
            _ => return Ok(None),
        }
    }
    bail!(not_found(format!(
        "The chain of tags starting at {id} is unreasonably long"
    )))
}
