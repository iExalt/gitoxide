//! Receive a push into a repository, from reading what the client sends to the report, like `git receive-pack` does it.
use std::{
    collections::HashSet,
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

use gix_error::{Result, ResultExt, bail, message, unsupported};
use gix_hash::{ObjectId, oid};
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_protocol::{
    receive::{
        Advertisement, Capabilities, Reference, Report, Request, report::CommandReport, side_band, write_advertisement,
        write_report,
    },
    transport::Protocol,
};
use gix_ref::transaction::{LogChange, PreviousValue, RefEdit, RefLog};

use super::{
    Options,
    connectivity::{self, Cutoffs},
    hooks::{Heartbeat, Runner},
    policy::{
        self, Ancestry, CheckedOut, Command, Level, Message, Messages, Plan, Policy, Queued, Refs, Resolved,
        StoreFailure, World,
    },
};
use crate::Repository;

/// Receives pushes into a repository, see [`Repository::receive()`].
pub struct Receiver<'repo> {
    repo: &'repo Repository,
    options: Options,
    fault: Option<Fault>,
    auto_maintenance: Option<AutoMaintenance<'repo>>,
}

/// What runs maintenance after a push, if it's due, as Git does with `maintenance run --auto` after the hooks.
type AutoMaintenance<'repo> = Box<dyn Fn(&Repository) -> Result<()> + 'repo>;

/// Something that fails on purpose, to see how the receiver deals with failures that are hard to cause otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum Fault {
    /// Starting a transaction on references fails.
    StartTransaction,
    /// Committing a transaction on references fails as a whole.
    CommitTransaction,
}

/// What happened when a push was received.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// What to tell the client, which has an entry for each command in the order they were sent.
    pub report: Report,
    /// What to tell the client about what is happening, which are the warnings and errors that Git prints while it works.
    pub messages: Vec<Message>,
    /// `true` if the objects of the push were moved into the object database, which is the case unless all
    /// commands failed before.
    pub objects_migrated: bool,
    /// `true` if objects were received and `receive.autogc` is enabled, which is when Git runs `gc --auto` after a push.
    ///
    /// This is the seam for native maintenance: nothing is run by the receiver itself.
    pub maintenance_due: bool,
    /// What the commands became, for the hooks that run after the report was sent.
    commands: Vec<Command>,
    push_options: Vec<BString>,
}

impl Repository {
    /// Create a [`Receiver`] that receives pushes into this repository, like `git receive-pack` does, without running Git.
    ///
    /// Fail if the configuration of the repository asks for something that isn't supported, instead of ignoring it,
    /// see [`Options::from_repository()`].
    pub fn receive(&self) -> crate::Result<Receiver<'_>> {
        Ok(Receiver {
            repo: self,
            options: Options::from_repository(self)?,
            fault: None,
            auto_maintenance: None,
        })
    }
}

impl<'repo> Receiver<'repo> {
    /// The settings that were read from the configuration.
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Run `maintenance` with the repository after the hooks that follow a push, if objects were received and
    /// [`Options::auto_gc`] says so, like `git receive-pack` runs `maintenance run --auto` then. Its failure is ignored.
    pub fn with_auto_maintenance(mut self, maintenance: impl Fn(&Repository) -> Result<()> + 'repo) -> Self {
        self.auto_maintenance = Some(Box::new(maintenance));
        self
    }

    /// Make the receiver fail where a [`Fault`] says.
    #[doc(hidden)]
    pub fn with_fault(mut self, fault: Fault) -> Self {
        self.fault = Some(fault);
        self
    }

    /// Return what to tell clients about the repository: its references that aren't hidden, and what it offers.
    ///
    /// The references of alternates that are repositories are added as `.have`, which clients ignore other than to send less.
    pub fn advertisement(&self) -> Result<Advertisement> {
        let mut capabilities = Capabilities::new(self.repo.object_hash(), gix_protocol::agent(crate::env::agent()));
        capabilities.atomic = self.options.advertise_atomic;
        capabilities.ofs_delta = self.options.prefer_ofs_delta;
        capabilities.push_options = self.options.advertise_push_options;

        let mut seen = HashSet::new();
        let mut references = Vec::new();
        for (name, id) in all_references(self.repo)? {
            if self.options.hidden.is_hidden(name.as_bstr()) {
                continue;
            }
            seen.insert(id);
            references.push(Reference::new(name, id));
        }
        for alternate in alternate_repositories(self.repo)? {
            for (_, id) in all_references(&alternate)? {
                if seen.insert(id) {
                    references.push(Reference::new(".have", id));
                }
            }
        }
        Ok(Advertisement {
            capabilities,
            references,
            shallow: Vec::new(),
        })
    }

    /// Serve a client that talks to `input` and `output` like `git receive-pack` does: write the advertisement, read what
    /// the client wants to do, receive it and report what happened.
    ///
    /// `version` is the protocol the client asked for. Return `None` if the client sent no commands, which is when
    /// nothing else is written.
    pub fn serve(
        &self,
        input: &mut dyn Read,
        output: &mut dyn Write,
        version: Protocol,
        interrupt: &AtomicBool,
    ) -> Result<Option<Outcome>> {
        let advertisement = self.advertisement()?;
        write_advertisement(output, &advertisement, version)?;
        let request = gix_protocol::receive::read_request(input, &advertisement.capabilities)?;
        if request.commands.is_empty() {
            return Ok(None);
        }
        let side_band = request.features.side_band_64k;
        // A client that waits for hooks is told that all is well every so often, in a packet that has no data.
        let outcome = {
            let mut keep_alive = |()| {
                output.write_all(b"0005\x01").and_then(|()| output.flush()).ok();
            };
            let mut beat = || keep_alive(());
            self.receive_with(
                &request,
                input,
                interrupt,
                side_band.then_some(&mut beat as Heartbeat<'_>),
            )?
        };
        if side_band {
            write_messages(output, &outcome.messages)?;
        }
        if let Some(format) = request.features.status_format() {
            write_report(output, &outcome.report, format, side_band)?;
        }
        let after = {
            let mut keep_alive = |()| {
                output.write_all(b"0005\x01").and_then(|()| output.flush()).ok();
            };
            let mut beat = || keep_alive(());
            self.after_report_with(&outcome, side_band.then_some(&mut beat as Heartbeat<'_>))?
        };
        if side_band {
            write_messages(output, &after)?;
        }
        if side_band {
            output
                .write_all(b"0000")
                .and_then(|()| output.flush())
                .or_raise(|| message("Could not end the response"))?;
        }
        Ok(Some(outcome))
    }

    /// Receive what `request` asks for, with the objects it needs in `pack`, which is read if there are commands that
    /// need objects.
    ///
    /// Commands that can't be executed are reported as such, which isn't an error. Errors are returned if the request
    /// can't be handled at all, like if it asks for what isn't supported.
    pub fn receive(&self, request: &Request, pack: &mut dyn Read, interrupt: &AtomicBool) -> Result<Outcome> {
        self.receive_with(request, pack, interrupt, None)
    }

    /// Run the hooks that follow a push, which Git runs after it reported what happened to the client, and then
    /// maintenance if it is due. Return what the hooks said, which is for the client as well.
    pub fn after_report(&self, outcome: &Outcome) -> Result<Vec<Message>> {
        self.after_report_with(outcome, None)
    }

    fn after_report_with(&self, outcome: &Outcome, heartbeat: Option<Heartbeat<'_>>) -> Result<Vec<Message>> {
        let mut messages = Messages::default();
        let heartbeat = heartbeat
            .zip(self.options.keep_alive)
            .map(|(beat, interval)| (interval, beat));
        Runner::new(self.repo, &outcome.push_options, heartbeat)?.post(&outcome.commands, &mut messages);
        if outcome.maintenance_due
            && let Some(maintenance) = &self.auto_maintenance
        {
            maintenance(self.repo).ok();
        }
        Ok(messages.0)
    }

    fn receive_with(
        &self,
        request: &Request,
        pack: &mut dyn Read,
        interrupt: &AtomicBool,
        heartbeat: Option<Heartbeat<'_>>,
    ) -> Result<Outcome> {
        if !request.shallow.is_empty() {
            bail!(unsupported(
                "pushes from or to shallow repositories aren't supported yet"
            ));
        }
        let repo = self.repo;
        let mut commands: Vec<Command> = request
            .commands
            .iter()
            .map(|command| Command::new(command.ref_name.clone(), command.old_id, command.new_id))
            .collect();
        let mut messages = Messages::default();
        let heartbeat = heartbeat
            .zip(self.options.keep_alive)
            .map(|(beat, interval)| (interval, beat));
        let runner = Runner::new(repo, &request.push_options, heartbeat)?;
        let mut unpack_failure = None;
        let mut quarantine = None;
        let mut objects_migrated = false;
        let mut maintenance_due = false;

        if request.expects_pack() {
            let objects_dir = repo.objects.store_ref().path().to_owned();
            match gix_odb::quarantine::Quarantine::new(objects_dir, self.options.quarantine.clone()) {
                Err(_) => unpack_failure = Some("unable to start object transaction".to_owned()),
                Ok(mut created) => match created.receive_pack(pack, interrupt) {
                    Ok(_) => {
                        maintenance_due = self.options.auto_gc;
                        quarantine = Some(created);
                    }
                    Err(rejected) => unpack_failure = Some(rejected.reason.to_string()),
                },
            }
        }

        let mut keep = None;
        if unpack_failure.is_some() {
            for command in &mut commands {
                command.error = Some("unpacker error".into());
            }
        } else {
            self.verify_connectivity(&mut commands, quarantine.as_ref())?;
            policy::reject_hidden(&mut commands, &|name| self.options.hidden.is_hidden(name));
            // The hook sees all commands, including those that failed already, and decides about all of them.
            if runner.pre_receive(&commands, quarantine.as_ref(), &mut messages) {
                for command in commands.iter_mut().filter(|command| command.error.is_none()) {
                    command.error = Some("pre-receive hook declined".into());
                }
            }
            // If nothing is going to be updated, nothing is going to be kept either.
            if commands.iter().any(|command| command.error.is_none()) {
                let migrated = quarantine
                    .take()
                    .map(gix_odb::quarantine::Quarantine::migrate)
                    .transpose();
                match migrated {
                    Err(_) => {
                        for command in commands.iter_mut().filter(|command| command.error.is_none()) {
                            command.error = Some("unable to migrate objects to permanent storage".into());
                        }
                    }
                    Ok(migrated) => {
                        objects_migrated = migrated.is_some();
                        keep = migrated;
                        self.update_references(&mut commands, request.features.atomic, &runner, &mut messages)?;
                    }
                }
            }
        }
        // Whatever had to wait for the references is reachable now, or never will be.
        if let Some(keep) = keep {
            keep.release().ok();
        }
        drop(quarantine);
        drop(runner);

        let post_commands = commands.clone();
        Ok(Outcome {
            report: Report {
                unpack: unpack_failure.map(Into::into),
                commands: commands
                    .into_iter()
                    .map(|command| CommandReport {
                        ref_name: command.ref_name,
                        outcome: command.error.map_or(Ok(Vec::new()), |error| Err(error.into())),
                    })
                    .collect(),
            },
            messages: messages.0,
            objects_migrated,
            maintenance_due: maintenance_due && objects_migrated,
            commands: post_commands,
            push_options: request.push_options.clone(),
        })
    }

    /// Fail commands whose objects aren't all there. The commands are checked as a whole first, and one by one only
    /// if that fails, to tell which of them is at fault.
    fn verify_connectivity(
        &self,
        commands: &mut [Command],
        quarantine: Option<&gix_odb::quarantine::Quarantine>,
    ) -> Result<()> {
        let tips: Vec<ObjectId> = commands
            .iter()
            .filter(|command| !command.new_id.is_null())
            .map(|command| command.new_id)
            .collect();
        // Without objects to check, as with deletions only, it doesn't matter what the references look like.
        if tips.is_empty() {
            return Ok(());
        }
        let view;
        let repo = match quarantine {
            Some(quarantine) => {
                view = self.repo.with_quarantine(quarantine);
                &view
            }
            None => self.repo,
        };
        let hidden = &self.options.hidden;
        let cutoffs = Cutoffs::collect(repo, &|name| hidden.is_hidden(name)).ok();
        let all_connected = cutoffs
            .as_ref()
            .is_some_and(|cutoffs| connectivity::verify(repo, cutoffs, &tips).is_ok());
        if all_connected {
            return Ok(());
        }
        for command in commands.iter_mut().filter(|command| !command.new_id.is_null()) {
            let connected = cutoffs
                .as_ref()
                .is_some_and(|cutoffs| connectivity::verify(repo, cutoffs, &[command.new_id]).is_ok());
            if !connected {
                command.error = Some("missing necessary objects".into());
            }
        }
        Ok(())
    }

    fn update_references(
        &self,
        commands: &mut [Command],
        atomic: bool,
        runner: &Runner<'_, '_>,
        messages: &mut Messages,
    ) -> Result<()> {
        let repo = self.repo;
        let update_hook = |command: &Command, messages: &mut Messages| runner.update(command, messages);
        let world = RepoWorld {
            repo,
            update_hook: Some(&update_hook),
        };
        policy::check_aliased_updates(
            commands,
            &|name| world.resolve(name),
            &|id| world.abbreviate(id),
            messages,
        );
        let policy = Policy {
            deny_deletes: self.options.deny_deletes,
            deny_non_fast_forwards: self.options.deny_non_fast_forwards,
            deny_current_branch: self.options.deny_current_branch,
            deny_delete_current: self.options.deny_delete_current,
        };
        // Git derives an identity for the reflogs if there is none configured, which a server often hasn't.
        let mut identity = repo.clone();
        let _ = identity.committer_or_set_generic_fallback();
        let mut refs = RepoRefs {
            repo,
            identity: &identity,
            commands: commands
                .iter()
                .map(|command| (command.ref_name.clone(), command.old_id, command.new_id))
                .collect(),
            atomic: false,
            fault: self.fault,
            messages: Vec::new(),
        };
        policy::execute(commands, atomic, &policy, &world, &mut refs, messages);
        messages.0.extend(refs.messages.into_iter().map(|text| Message {
            level: policy::Level::Error,
            text: text.into(),
        }));
        Ok(())
    }
}

/// Tell the client what is going on, on the side band that is for people.
fn write_messages(output: &mut dyn Write, messages: &[Message]) -> Result<()> {
    let mut text = Vec::new();
    for message in messages.iter().filter(|message| message.level != Level::Diagnostic) {
        text.extend(message.to_side_band_bytes());
    }
    output
        .write_all(&side_band::encode(side_band::Channel::Progress, &text))
        .or_raise(|| message("Could not write messages"))
}

/// All references with the id they lead to, in the order of their names, without those that lead nowhere.
fn all_references(repo: &Repository) -> Result<Vec<(BString, ObjectId)>> {
    let mut out = Vec::new();
    for reference in repo
        .references()
        .or_raise(|| message("Could not read the references"))?
        .all()
        .or_raise(|| message("Could not read the references"))?
    {
        let reference = reference.or_raise(|| message("Could not read a reference"))?;
        let name = reference.name().as_bstr().to_owned();
        let id = match reference.try_id() {
            Some(id) => id.detach(),
            // Symbolic references show what they lead to, and are left out if that isn't there.
            None => match reference.follow() {
                Some(Ok(target)) => match target.try_id() {
                    Some(id) => id.detach(),
                    None => continue,
                },
                _ => continue,
            },
        };
        out.push((name, id));
    }
    Ok(out)
}

/// The alternates of `repo` that are repositories themselves.
fn alternate_repositories(repo: &Repository) -> Result<Vec<Repository>> {
    let alternates = gix_odb::alternate::resolve(repo.objects.store_ref().path().to_owned(), repo.current_dir())
        .or_raise(|| message("Could not resolve the alternates of the object database"))?;
    Ok(alternates
        .into_iter()
        .filter_map(|alternate| {
            let git_dir = alternate.parent().filter(|_| alternate.ends_with("objects"))?;
            crate::open_opts(git_dir, crate::open::Options::isolated()).ok()
        })
        .collect())
}

impl Repository {
    /// A copy that sees the objects of `quarantine`, which are those of the repository as well.
    fn with_quarantine(&self, quarantine: &gix_odb::quarantine::Quarantine) -> Repository {
        let mut view = self.clone();
        let mut objects: crate::OdbHandle = quarantine.store().to_cache().into();
        crate::repository::init::setup_objects(&mut objects, &self.config);
        view.objects = objects;
        view
    }
}

/// Asks whether the `update` hook declines a command.
type UpdateHook<'a> = dyn Fn(&Command, &mut Messages) -> bool + 'a;

/// What decisions about commands need to know about the repository.
struct RepoWorld<'a> {
    repo: &'a Repository,
    update_hook: Option<&'a UpdateHook<'a>>,
}

fn read_trimmed(path: &Path) -> Option<BString> {
    std::fs::read(path).ok().map(|content| content.trim().into())
}

/// Whether the worktree of `repo` has the branch `name` checked out, or is in the middle of rebasing or bisecting it.
fn worktree_uses(repo: &Repository, name: &BStr) -> bool {
    if let Ok(Some(head)) = repo.head_name() {
        return head.as_bstr() == name;
    }
    let git_dir = repo.git_dir();
    let rebasing = ["rebase-merge", "rebase-apply"]
        .iter()
        .filter_map(|dir| read_trimmed(&git_dir.join(dir).join("head-name")))
        .any(|branch| branch.as_bstr() == name);
    let bisecting = read_trimmed(&git_dir.join("BISECT_START")).is_some_and(|start| {
        let mut full = BString::from("refs/heads/");
        full.extend_from_slice(&start);
        full.as_bstr() == name
    });
    rebasing || bisecting
}

impl RepoWorld<'_> {
    fn resolve(&self, name: &BStr) -> Resolved {
        // Like Git, follow symbolic references for as long as they lead somewhere, to the name of the last one.
        let mut current = name.to_owned();
        let mut first = true;
        for _ in 0..6 {
            let Ok(Some(reference)) = self.repo.try_find_reference(current.as_bstr()) else {
                return if first {
                    Resolved::Direct
                } else {
                    Resolved::Target(current)
                };
            };
            match reference.target() {
                gix_ref::TargetRef::Object(_) => {
                    return if first {
                        Resolved::Direct
                    } else {
                        Resolved::Target(current)
                    };
                }
                gix_ref::TargetRef::Symbolic(next) => {
                    current = next.as_bstr().to_owned();
                    first = false;
                }
            }
        }
        Resolved::Broken
    }
}

impl World for RepoWorld<'_> {
    fn checked_out(&self, name: &BStr) -> Option<CheckedOut> {
        let repo = self.repo;
        if worktree_uses(repo, name) {
            return Some(CheckedOut { bare: repo.is_bare() });
        }
        repo.worktrees()
            .ok()?
            .into_iter()
            .any(|worktree| {
                worktree
                    .into_repo_with_possibly_inaccessible_worktree()
                    .is_ok_and(|linked| worktree_uses(&linked, name))
            })
            .then_some(CheckedOut { bare: false })
    }

    fn update_hook(&self, command: &Command, messages: &mut Messages) -> bool {
        self.update_hook.is_some_and(|hook| hook(command, messages))
    }

    fn head_name(&self) -> Option<BString> {
        self.repo
            .head_name()
            .ok()
            .flatten()
            .map(|name| name.as_bstr().to_owned())
    }

    fn alias_target(&self, name: &BStr) -> Option<BString> {
        match self.resolve(name) {
            Resolved::Target(target) => Some(target),
            Resolved::Direct | Resolved::Broken => None,
        }
    }

    fn has_object(&self, id: &oid) -> bool {
        self.repo.has_object(id)
    }

    fn is_readable(&self, id: &oid) -> bool {
        !id.is_null() && self.repo.find_object(id).is_ok()
    }

    fn ref_exists(&self, name: &BStr) -> bool {
        matches!(self.repo.try_find_reference(name), Ok(Some(_)))
    }

    fn is_valid_name(&self, name: &BStr, allow_one_level: bool) -> bool {
        if !allow_one_level && !name.contains(&b'/') {
            return false;
        }
        let mut full = BString::from("refs/");
        full.extend_from_slice(name);
        gix_validate::reference::name(full.as_bstr()).is_ok()
    }

    fn ancestry(&self, old: &oid, new: &oid) -> Ancestry {
        let is_commit = |id: &oid| {
            self.repo
                .find_object(id)
                .is_ok_and(|object| object.kind == gix_object::Kind::Commit)
        };
        if !is_commit(old) || !is_commit(new) {
            return Ancestry::BadObjects;
        }
        match self.repo.merge_base(old, new) {
            Ok(Some(base)) if base.as_ref() == old => Ancestry::Ancestor,
            _ => Ancestry::NotAncestor,
        }
    }

    fn abbreviate(&self, id: &oid) -> String {
        match self.repo.find_object(id) {
            Ok(object) => object.id().shorten_or_id().to_string(),
            Err(_) => id.to_hex_with_len(7).to_string(),
        }
    }

    fn invalid_new_value(&self, name: &BStr, new: &oid) -> Option<String> {
        let Ok(object) = self.repo.find_object(new) else {
            return Some(format!("trying to write ref '{name}' with nonexistent object {new}"));
        };
        (object.kind != gix_object::Kind::Commit && name.starts_with(b"refs/heads/"))
            .then(|| format!("trying to write non-commit object {new} to branch '{name}'"))
    }
}

/// Phrase what the reference store says as Git does where there is an equivalent that people may know.
fn as_git_says(text: String) -> String {
    match text
        .strip_prefix("A reference named '")
        .and_then(|rest| rest.strip_suffix("' has multiple edits"))
    {
        Some(name) => format!("multiple updates for ref '{name}' not allowed"),
        None => text,
    }
}

/// Updates the references of a repository.
struct RepoRefs<'a> {
    repo: &'a Repository,
    /// The repository to get the committer of reflog entries from, which has a fallback.
    identity: &'a Repository,
    /// The name, old and new id of the commands, which are what edits are made from.
    commands: Vec<(BString, ObjectId, ObjectId)>,
    atomic: bool,
    fault: Option<Fault>,
    /// What went wrong, as Git tells the pusher.
    messages: Vec<String>,
}

impl Refs for RepoRefs<'_> {
    fn begin(&mut self, atomic: bool) -> std::result::Result<(), StoreFailure> {
        self.atomic = atomic;
        if self.fault == Some(Fault::StartTransaction) {
            self.messages.push("failed to start the transaction".into());
            return Err(StoreFailure::Start);
        }
        Ok(())
    }

    fn commit(&mut self, queued: &[Queued]) -> std::result::Result<Vec<(usize, &'static str)>, StoreFailure> {
        let edits: Vec<RefEdit> = queued
            .iter()
            .map(|queued| {
                let (name, old, new) = &self.commands[queued.index];
                let log = LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "push".into(),
                };
                let name = gix_ref::FullName::try_from(name.as_bstr()).expect("the name was validated");
                match &queued.plan {
                    Plan::Update => RefEdit {
                        change: gix_ref::transaction::Change::Update {
                            log,
                            expected: if old.is_null() {
                                PreviousValue::MustNotExist
                            } else {
                                PreviousValue::MustExistAndMatch(gix_ref::Target::Object(*old))
                            },
                            new: gix_ref::Target::Object(*new),
                        },
                        name,
                        deref: true,
                    },
                    Plan::Delete { lease } => RefEdit {
                        change: gix_ref::transaction::Change::Delete {
                            expected: lease.map_or(PreviousValue::Any, |lease| {
                                PreviousValue::MustExistAndMatch(gix_ref::Target::Object(lease))
                            }),
                            log: RefLog::AndReference,
                        },
                        name,
                        deref: true,
                    },
                }
            })
            .collect();
        let failed = |this: &mut Self, err: &dyn std::fmt::Display| {
            this.messages.push(as_git_says(err.to_string()));
            StoreFailure::Commit
        };
        if self.fault == Some(Fault::CommitTransaction) {
            return Err(failed(self, &"failed to commit the transaction"));
        }
        let (file_lock_fail, packed_refs_lock_fail) = match self.repo.config.lock_timeout() {
            Ok(timeouts) => timeouts,
            Err(err) => return Err(failed(self, &err)),
        };
        let mut transaction = self
            .repo
            .ref_store()
            .transaction()
            .head_log(gix_ref::transaction::HeadLog::Git);
        if !self.atomic {
            transaction = transaction.batch_updates();
        }
        let prepared = match transaction.prepare(
            edits,
            gix_ref::view::transaction::LockOptions::new(file_lock_fail, packed_refs_lock_fail),
        ) {
            Ok(prepared) => prepared,
            Err(err) => return Err(failed(self, &err)),
        };
        let rejected: Vec<_> = prepared
            .rejected()
            .iter()
            .map(|rejected| (rejected.index, rejected.reason.as_str()))
            .collect();
        for rejected in prepared.rejected() {
            self.messages.push(as_git_says(rejected.error.to_string()));
        }
        let committer = self.identity.committer().and_then(std::result::Result::ok);
        match prepared.commit(committer) {
            Ok(_) => Ok(rejected),
            Err(err) => Err(failed(self, &err)),
        }
    }
}
