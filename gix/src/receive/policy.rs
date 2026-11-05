//! What `git receive-pack` decides about the commands of a push, in the order it decides it, without looking at any repository.
//!
//! Everything that depends on the repository is asked of a [`World`], and updating references is left to [`Refs`], so
//! that the decisions can be tested on their own. The functions follow `receive-pack.c` of Git 2.56: [`reject_hidden()`],
//! [`check_aliased_updates()`], [`check_update()`] and [`execute()`] are `reject_updates_to_hidden()`,
//! `check_aliased_updates()`, `update()` and `execute_commands_atomic()` or `execute_commands_non_atomic()`.
use gix_hash::{ObjectId, oid};
use gix_object::bstr::{BStr, BString, ByteSlice};

use super::DenyAction;

/// A command of a push and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Command {
    pub ref_name: BString,
    pub old_id: ObjectId,
    pub new_id: ObjectId,
    /// Why the command is rejected, as reported to the client.
    pub error: Option<String>,
    /// The command is part of another one that updates the same reference through a symbolic reference, and isn't to be executed.
    pub skip_update: bool,
    /// The reference to delete didn't exist. This only matters to hooks.
    pub did_not_exist: bool,
}

impl Command {
    pub fn new(ref_name: impl Into<BString>, old_id: ObjectId, new_id: ObjectId) -> Self {
        Command {
            ref_name: ref_name.into(),
            old_id,
            new_id,
            error: None,
            skip_update: false,
            did_not_exist: false,
        }
    }

    fn is_delete(&self) -> bool {
        self.new_id.is_null()
    }

    fn should_process(&self) -> bool {
        self.error.is_none() && !self.skip_update
    }
}

/// Whether a message is an error or a warning, which is how Git prefixes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The message explains a rejection.
    Error,
    /// The message points out something that was allowed.
    Warning,
    /// The message is for whoever runs the receiver, and not sent to the pusher as Git doesn't either.
    Diagnostic,
    /// What a hook said, as it said it.
    Hook,
}

/// A message for the pusher that explains what happens, which Git sends on the side band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// How serious it is.
    pub level: Level,
    /// What to tell. Messages of hooks are what they printed, and others are a line without its end.
    pub text: BString,
}

impl Message {
    /// The bytes to send on the side band for people: lines of Git's own messages end with a newline, whereas what a hook
    /// says is relayed as it came.
    pub fn to_side_band_bytes(&self) -> Vec<u8> {
        match self.level {
            Level::Hook => self.text.to_vec(),
            _ => format!("{self}\n").into_bytes(),
        }
    }
}

impl std::fmt::Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self.level {
            Level::Error | Level::Diagnostic => "error",
            Level::Warning => "warning",
            Level::Hook => return write!(f, "{}", self.text),
        };
        write!(f, "{prefix}: {}", self.text)
    }
}

/// Collects messages.
#[derive(Debug, Default)]
pub(super) struct Messages(pub Vec<Message>);

impl Messages {
    pub(super) fn error(&mut self, text: impl Into<String>) {
        self.0.push(Message {
            level: Level::Error,
            text: text.into().into(),
        });
    }
    pub fn diagnostic(&mut self, text: impl Into<String>) {
        self.0.push(Message {
            level: Level::Diagnostic,
            text: text.into().into(),
        });
    }
    fn warning(&mut self, text: impl Into<String>) {
        self.0.push(Message {
            level: Level::Warning,
            text: text.into().into(),
        });
    }
    /// What a program that checked the received objects printed, to be relayed as it is, if it printed anything.
    pub(super) fn program_output(&mut self, output: Vec<u8>) {
        if !output.is_empty() {
            self.hook_output(output);
        }
    }
    pub fn hook_output(&mut self, output: Vec<u8>) {
        self.0.push(Message {
            level: Level::Hook,
            text: output.into(),
        });
    }
}

/// The settings that decide about updates.
#[derive(Debug, Clone, Copy)]
pub(super) struct Policy {
    pub deny_deletes: bool,
    pub deny_non_fast_forwards: bool,
    pub deny_current_branch: DenyAction,
    pub deny_delete_current: DenyAction,
}

/// A worktree that has a reference checked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CheckedOut {
    pub bare: bool,
}

/// How an old and a new value relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ancestry {
    /// One of them isn't a commit, or can't be read.
    BadObjects,
    /// The old commit is an ancestor of the new one.
    Ancestor,
    /// It isn't.
    NotAncestor,
}

/// What decisions depend on that isn't part of a command.
pub(super) trait World {
    /// The worktree whose `HEAD` is a symbolic reference to `name`, which includes the bare repository itself.
    fn checked_out(&self, name: &BStr) -> Option<CheckedOut>;
    /// The full name that `HEAD` leads to.
    fn head_name(&self) -> Option<BString>;
    /// Run the `update` hook for `command` and return `true` if it declines it. What it says is added to `messages`.
    fn update_hook(&self, command: &Command, messages: &mut Messages) -> bool;
    /// Bring the work tree that has the branch `name` checked out up to the new value of `command`, which is what
    /// `receive.denyCurrentBranch=updateInstead` is for, and return the reason to refuse the update with if that isn't possible.
    fn update_worktree(&self, name: &BStr, command: &Command, messages: &mut Messages) -> Option<&'static str>;
    /// Take what `command` needs of the shallow boundary of a client into the `shallow` file, which is what
    /// `receive.shallowUpdate` is for, and return `true` if that failed, as it does if what the command brings isn't connected.
    fn update_shallow(&self, _command: &Command, _messages: &mut Messages) -> bool {
        false
    }
    /// The name of the reference that `name` leads to if it is a symbolic reference, following all of them.
    fn alias_target(&self, name: &BStr) -> Option<BString>;
    /// Whether the object exists.
    fn has_object(&self, id: &oid) -> bool;
    /// Whether the object can be read and is valid, which is what `parse_object()` tells.
    fn is_readable(&self, id: &oid) -> bool;
    /// Whether the reference exists, even if it is broken.
    fn ref_exists(&self, name: &BStr) -> bool;
    /// Whether `name`, which is what follows `refs/`, is a valid name for a reference. A name of one level is valid
    /// only if `allow_one_level` is set.
    fn is_valid_name(&self, name: &BStr, allow_one_level: bool) -> bool;
    /// How `old` and `new` relate.
    fn ancestry(&self, old: &oid, new: &oid) -> Ancestry;
    /// The shortest unambiguous abbreviation of `id`, which is at least seven characters long.
    fn abbreviate(&self, id: &oid) -> String;
    /// Why the reference store would refuse to write `new` to the reference `name`, which is the case if it doesn't exist
    /// or isn't a commit while the reference is a branch.
    fn invalid_new_value(&self, name: &BStr, new: &oid) -> Option<String>;
}

/// What a reference to resolve leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Resolved {
    /// It isn't a symbolic reference.
    Direct,
    /// It's a symbolic reference that leads nowhere.
    Broken,
    /// It's a symbolic reference to this reference.
    Target(BString),
}

/// Reject updates to references that are hidden, as they must not be seen to exist.
pub(super) fn reject_hidden(commands: &mut [Command], is_hidden: &dyn Fn(&BStr) -> bool) {
    for command in commands.iter_mut().filter(|command| command.error.is_none()) {
        if is_hidden(command.ref_name.as_bstr()) {
            command.error = Some(
                if command.is_delete() {
                    "deny deleting a hidden ref"
                } else {
                    "deny updating a hidden ref"
                }
                .into(),
            );
        }
    }
}

/// Find commands that update a reference through a symbolic reference that another command updates directly, which
/// can't be executed both. Consistent ones are executed once, and inconsistent ones are rejected.
pub(super) fn check_aliased_updates(
    commands: &mut [Command],
    resolve: &dyn Fn(&BStr) -> Resolved,
    abbreviate: &dyn Fn(&oid) -> String,
    messages: &mut Messages,
) {
    for index in 0..commands.len() {
        if commands[index].error.is_some() {
            continue;
        }
        let target = match resolve(commands[index].ref_name.as_bstr()) {
            Resolved::Direct => continue,
            Resolved::Broken => {
                messages.error(format!(
                    "refusing update to broken symref '{}'",
                    commands[index].ref_name
                ));
                commands[index].skip_update = true;
                commands[index].error = Some("broken symref".into());
                continue;
            }
            Resolved::Target(target) => target,
        };
        let Some(found) = commands.iter().rposition(|candidate| candidate.ref_name == target) else {
            continue;
        };
        commands[index].skip_update = true;
        if commands[index].old_id == commands[found].old_id && commands[index].new_id == commands[found].new_id {
            continue;
        }
        commands[found].skip_update = true;
        messages.error(format!(
            "refusing inconsistent update between symref '{}' ({}..{}) and its target '{}' ({}..{})",
            commands[index].ref_name,
            abbreviate(&commands[index].old_id),
            abbreviate(&commands[index].new_id),
            commands[found].ref_name,
            abbreviate(&commands[found].old_id),
            abbreviate(&commands[found].new_id),
        ));
        commands[index].error = Some("inconsistent aliased update".into());
        commands[found].error = Some("inconsistent aliased update".into());
    }
}

/// What to do for a command that passed all checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Plan {
    /// Create or update the reference. The old value is `Command::old_id`, which must not exist if it is null.
    Update,
    /// Delete the reference, which has to be at `lease` unless there is none, as the old value is corrupt or nothing.
    Delete { lease: Option<ObjectId> },
}

const UNCONFIGURED_DENY: &str = "By default, updating the current branch in a non-bare repository\n\
is denied, because it will make the index and work tree inconsistent\n\
with what you pushed, and will require 'git reset --hard' to match\n\
the work tree to HEAD.\n\
\n\
You can set the 'receive.denyCurrentBranch' configuration variable\n\
to 'ignore' or 'warn' in the remote repository to allow pushing into\n\
its current branch; however, this is not recommended unless you\n\
arranged to update its work tree to match what you pushed in some\n\
other way.\n\
\n\
To squelch this message and still keep the default behaviour, set\n\
'receive.denyCurrentBranch' configuration variable to 'refuse'.";

const UNCONFIGURED_DENY_DELETE_CURRENT: &str = "By default, deleting the current branch is denied, because the next\n\
'git clone' won't result in any file checked out, causing confusion.\n\
\n\
You can set 'receive.denyDeleteCurrent' configuration variable to\n\
'warn' or 'ignore' in the remote repository to allow deleting the\n\
current branch, with or without a warning message.\n\
\n\
To squelch this message, you can set it to 'refuse'.";

/// Decide if `command` may be executed, and how, or else return the reason it is rejected, in the order Git decides.
pub(super) fn check_update(
    command: &mut Command,
    policy: &Policy,
    world: &dyn World,
    messages: &mut Messages,
) -> Result<Plan, &'static str> {
    let name = command.ref_name.as_bstr();
    let deleting = command.is_delete();

    let valid = name
        .strip_prefix(b"refs/")
        .is_some_and(|rest| world.is_valid_name(rest.as_bstr(), deleting));
    if !valid {
        messages.error(format!("refusing to update funny ref '{name}' remotely"));
        return Err("funny refname");
    }

    // Git looks at the name of the reference alone, which makes a symbolic reference a way around its protection of the
    // checked out branch. Here, what the reference leads to is protected as well.
    let target = world.alias_target(name);
    let protected = target.as_ref().map_or(name, |target| target.as_bstr());
    let worktree = world.checked_out(name).or_else(|| world.checked_out(protected));
    let mut update_worktree = false;
    if worktree.is_some_and(|worktree| !worktree.bare) {
        match policy.deny_current_branch {
            DenyAction::Ignore => {}
            DenyAction::Warn => messages.warning("updating the current branch"),
            // Other checks come first, and the work tree changes after the hook agreed, and before the reference does.
            DenyAction::UpdateInstead => update_worktree = true,
            DenyAction::Refuse | DenyAction::Unconfigured => {
                messages.error(format!("refusing to update checked out branch: {protected}"));
                if policy.deny_current_branch == DenyAction::Unconfigured {
                    messages.error(UNCONFIGURED_DENY);
                }
                return Err("branch is currently checked out");
            }
        }
    }

    if !deleting && !world.has_object(&command.new_id) {
        messages.diagnostic(format!(
            "unpack should have generated {}, but I can't find it!",
            command.new_id
        ));
        return Err("bad pack");
    }

    let is_branch = name.starts_with(b"refs/heads/");
    // Git skips these checks for a deletion that has no old value, which lets a crafted request go around them. They apply here.
    if deleting {
        if policy.deny_deletes && is_branch {
            messages.error(format!("denying ref deletion for {name}"));
            return Err("deletion prohibited");
        }
        let is_head = world.head_name().is_some_and(|head| head == name || head == protected);
        if worktree.is_some() || is_head {
            match policy.deny_delete_current {
                DenyAction::Ignore => {}
                DenyAction::Warn => messages.warning("deleting the current branch"),
                DenyAction::Refuse | DenyAction::Unconfigured | DenyAction::UpdateInstead => {
                    if policy.deny_delete_current == DenyAction::Unconfigured {
                        messages.error(UNCONFIGURED_DENY_DELETE_CURRENT);
                    }
                    messages.error(format!("refusing to delete the current branch: {protected}"));
                    return Err("deletion of the current branch prohibited");
                }
            }
        }
    }

    if policy.deny_non_fast_forwards && !deleting && !command.old_id.is_null() && is_branch {
        match world.ancestry(&command.old_id, &command.new_id) {
            Ancestry::BadObjects => {
                messages.diagnostic(format!("bad sha1 objects for {name}"));
                return Err("bad ref");
            }
            Ancestry::NotAncestor => {
                messages.error(format!("denying non-fast-forward {name} (you should pull first)"));
                return Err("non-fast-forward");
            }
            Ancestry::Ancestor => {}
        }
    }

    if world.update_hook(command, messages) {
        messages.error(format!("hook declined to update {name}"));
        return Err("hook declined");
    }

    if update_worktree && let Some(reason) = world.update_worktree(protected, command, messages) {
        return Err(reason);
    }

    if !deleting && world.update_shallow(command, messages) {
        return Err("shallow error");
    }

    if !deleting {
        // Git finds this out when it queues the update.
        if let Some(reason) = world.invalid_new_value(name, &command.new_id) {
            messages.error(reason);
            return Err("invalid new value provided");
        }
        return Ok(Plan::Update);
    }
    // A deletion is expected to find the value that was announced, unless that is corrupt, in which case it can only go.
    let lease = if world.is_readable(&command.old_id) {
        Some(command.old_id)
    } else {
        if world.ref_exists(name) {
            messages.warning("allowing deletion of corrupt ref");
        } else {
            messages.warning("deleting a non-existent ref");
            command.did_not_exist = true;
        }
        None
    };
    Ok(Plan::Delete { lease })
}

/// A command that is to be executed with the reference store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Queued {
    /// The position of the command.
    pub index: usize,
    pub plan: Plan,
}

/// Why the reference store couldn't do what was asked, which is reported as is for all commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StoreFailure {
    /// A transaction couldn't be started.
    Start,
    /// The transaction failed as a whole, which is the only way for an atomic transaction to fail, or committing failed.
    Commit,
    /// A hook refused the transaction, which makes Git stop without telling the client what happened to the commands.
    Died,
}

/// The reference store, which executes a transaction at a time.
pub(super) trait Refs {
    /// Begin a transaction, which is atomic if all edits have to succeed or else none must be applied.
    fn begin(&mut self, atomic: bool) -> Result<(), StoreFailure>;
    /// Apply `queued` and return the positions in it and the reasons of those that were rejected, which isn't possible if
    /// the transaction is atomic.
    fn commit(&mut self, queued: &[Queued]) -> Result<Vec<(usize, &'static str)>, StoreFailure>;
}

/// Execute all commands that are to be executed, atomically if `atomic` is set, and set the error of those that failed.
pub(super) fn execute(
    commands: &mut [Command],
    atomic: bool,
    policy: &Policy,
    world: &dyn World,
    refs: &mut dyn Refs,
    messages: &mut Messages,
) {
    if atomic {
        execute_atomic(commands, policy, world, refs, messages);
    } else {
        execute_non_atomic(commands, policy, world, refs, messages);
    }
}

fn execute_atomic(
    commands: &mut [Command],
    policy: &Policy,
    world: &dyn World,
    refs: &mut dyn Refs,
    messages: &mut Messages,
) {
    let mut reported = "atomic push failure";
    let failed = 'run: {
        if refs.begin(true).is_err() {
            reported = "transaction failed to start";
            break 'run true;
        }
        let mut queued = Vec::new();
        for (index, command) in commands.iter_mut().enumerate() {
            if !command.should_process() {
                continue;
            }
            match check_update(command, policy, world, messages) {
                Ok(plan) => queued.push(Queued { index, plan }),
                Err(reason) => {
                    commands[index].error = Some(reason.into());
                    break 'run true;
                }
            }
        }
        if refs.commit(&queued).is_err() {
            reported = "atomic transaction failed";
            break 'run true;
        }
        false
    };
    if failed {
        for command in commands.iter_mut().filter(|command| command.error.is_none()) {
            command.error = Some(reported.into());
        }
    }
}

fn execute_non_atomic(
    commands: &mut [Command],
    policy: &Policy,
    world: &dyn World,
    refs: &mut dyn Refs,
    messages: &mut Messages,
) {
    // Deleting a reference and creating one that can't exist next to it can't happen in one transaction, so deletions go first.
    for deletions in [true, false] {
        let mut queued = Vec::new();
        let mut began = false;
        let mut reported = None;
        // The failure of one command is recorded on all the others, so `commands` can't be borrowed by the loop.
        #[allow(clippy::needless_range_loop)]
        for index in 0..commands.len() {
            if !commands[index].should_process() || commands[index].is_delete() != deletions {
                continue;
            }
            if !began {
                began = true;
                if refs.begin(false).is_err() {
                    reported = Some("transaction failed to start");
                    break;
                }
            }
            match check_update(&mut commands[index], policy, world, messages) {
                Ok(plan) => queued.push(Queued { index, plan }),
                Err(reason) => commands[index].error = Some(reason.into()),
            }
        }
        if !began {
            continue;
        }
        let rejected = if reported.is_some() {
            Vec::new()
        } else {
            match refs.commit(&queued) {
                Ok(rejected) => rejected,
                Err(StoreFailure::Died) => return,
                Err(_) => {
                    reported = Some("failed to update refs");
                    Vec::new()
                }
            }
        };
        if let Some(reported) = reported {
            // Everything fails, even what was rejected for another reason, or what went through earlier.
            for command in commands.iter_mut() {
                command.error = Some(reported.into());
            }
        } else {
            for (position, reason) in rejected {
                commands[queued[position].index].error = Some(reason.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn id(byte: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&[byte; 20])
    }

    fn null() -> ObjectId {
        ObjectId::null(gix_hash::Kind::Sha1)
    }

    #[derive(Default)]
    struct Fake {
        checked_out: Vec<(&'static str, bool)>,
        head: Option<&'static str>,
        aliases: Vec<(&'static str, &'static str)>,
        missing: Vec<ObjectId>,
        unreadable: Vec<ObjectId>,
        existing_refs: Vec<&'static str>,
        ancestry: Option<Ancestry>,
        invalid: Vec<ObjectId>,
        /// The references whose update hook declines.
        declined_by_hook: Vec<&'static str>,
        /// The references that the update hook was asked about, in order.
        hook_asked: RefCell<Vec<String>>,
        /// Why updating the work tree refuses, if it does.
        worktree_refusal: Option<&'static str>,
        /// The references for which the work tree was updated, in order, with what they are updated to.
        worktree_updated: RefCell<Vec<(String, ObjectId)>>,
    }

    impl World for Fake {
        fn checked_out(&self, name: &BStr) -> Option<CheckedOut> {
            self.checked_out
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .map(|(_, bare)| CheckedOut { bare: *bare })
        }
        fn head_name(&self) -> Option<BString> {
            self.head.map(Into::into)
        }
        fn update_hook(&self, command: &Command, messages: &mut Messages) -> bool {
            self.hook_asked.borrow_mut().push(command.ref_name.to_string());
            let declined = self.declined_by_hook.iter().any(|name| *name == command.ref_name);
            if declined {
                messages.hook_output(b"said by the hook\n".to_vec());
            }
            declined
        }
        fn update_worktree(&self, name: &BStr, command: &Command, _: &mut Messages) -> Option<&'static str> {
            self.worktree_updated
                .borrow_mut()
                .push((name.to_string(), command.new_id));
            self.worktree_refusal
        }
        fn alias_target(&self, name: &BStr) -> Option<BString> {
            self.aliases
                .iter()
                .find(|(alias, _)| *alias == name)
                .map(|(_, target)| (*target).into())
        }
        fn has_object(&self, id: &oid) -> bool {
            !self.missing.iter().any(|missing| missing.as_ref() == id)
        }
        fn is_readable(&self, id: &oid) -> bool {
            !id.is_null() && !self.unreadable.iter().any(|unreadable| unreadable.as_ref() == id)
        }
        fn ref_exists(&self, name: &BStr) -> bool {
            self.existing_refs.iter().any(|candidate| *candidate == name)
        }
        fn is_valid_name(&self, name: &BStr, allow_one_level: bool) -> bool {
            (allow_one_level || name.contains(&b'/')) && !name.contains(&b' ')
        }
        fn ancestry(&self, _: &oid, _: &oid) -> Ancestry {
            self.ancestry.unwrap_or(Ancestry::Ancestor)
        }
        fn abbreviate(&self, id: &oid) -> String {
            id.to_hex().to_string()
        }
        fn invalid_new_value(&self, _: &BStr, new: &oid) -> Option<String> {
            self.invalid
                .iter()
                .any(|invalid| invalid.as_ref() == new)
                .then(|| "invalid".into())
        }
    }

    fn policy() -> Policy {
        Policy {
            deny_deletes: false,
            deny_non_fast_forwards: false,
            deny_current_branch: DenyAction::Unconfigured,
            deny_delete_current: DenyAction::Unconfigured,
        }
    }

    fn check(command: &mut Command, policy: &Policy, world: &Fake) -> Result<Plan, &'static str> {
        check_update(command, policy, world, &mut Messages::default())
    }

    fn update(name: &str) -> Command {
        Command::new(name, id(1), id(2))
    }

    fn delete(name: &str) -> Command {
        Command::new(name, id(1), null())
    }

    #[test]
    fn funny_names_are_refused_first_and_deletions_may_have_one_level() {
        let world = Fake::default();
        let mut cases = vec![
            (update("heads/main"), Err("funny refname")),
            (update("refs/heads"), Err("funny refname")),
            (update("refs/heads/a b"), Err("funny refname")),
            (update("refs/heads/main"), Ok(Plan::Update)),
            (delete("refs/heads"), Ok(Plan::Delete { lease: Some(id(1)) })),
            (delete("refs/a b"), Err("funny refname")),
        ];
        for (command, expected) in &mut cases {
            assert_eq!(
                check(command, &policy(), &world),
                *expected,
                "{}: the name decides before anything else",
                command.ref_name
            );
        }
        let mut missing = Fake::default();
        missing.missing.push(id(2));
        assert_eq!(
            check(&mut update("heads/main"), &policy(), &missing),
            Err("funny refname"),
            "a funny name wins over a missing object"
        );
    }

    #[test]
    fn the_update_hook_decides_after_the_deny_rules_and_before_the_new_value_is_judged() {
        let world = Fake {
            declined_by_hook: vec!["refs/heads/a"],
            invalid: vec![id(1)],
            ancestry: Some(Ancestry::NotAncestor),
            ..Fake::default()
        };
        let strict = Policy {
            deny_non_fast_forwards: true,
            ..policy()
        };
        let mut messages = Messages::default();
        let mut non_ff = update("refs/heads/a");
        non_ff.old_id = id(2);
        assert_eq!(
            check_update(&mut non_ff, &strict, &world, &mut messages),
            Err("non-fast-forward"),
            "the deny rules come first, so the hook isn't asked"
        );
        assert!(
            world.hook_asked.borrow().is_empty(),
            "the hook is not asked about what failed"
        );

        let mut messages = Messages::default();
        assert_eq!(
            check_update(&mut update("refs/heads/a"), &policy(), &world, &mut messages),
            Err("hook declined"),
            "a hook that declines wins over the judgement of the new value"
        );
        let texts: Vec<_> = messages.0.iter().map(|message| message.text.to_string()).collect();
        assert_eq!(
            texts,
            ["said by the hook\n", "hook declined to update refs/heads/a"],
            "what the hook said comes before the refusal"
        );
    }

    #[test]
    fn update_hooks_follow_the_phases_of_execution() {
        let commands = || {
            vec![
                Command::new("refs/heads/create", null(), id(1)),
                Command::new("refs/heads/delete", id(2), null()),
            ]
        };
        let world = Fake::default();
        let mut refs = FakeRefs::default();
        execute(
            &mut commands(),
            false,
            &policy(),
            &world,
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(
            *world.hook_asked.borrow(),
            ["refs/heads/delete", "refs/heads/create"],
            "without atomicity, deletions are executed first and so are their hooks asked first"
        );

        let world = Fake {
            declined_by_hook: vec!["refs/heads/create"],
            ..Fake::default()
        };
        let mut cmds = commands();
        cmds.reverse();
        cmds.insert(0, Command::new("refs/heads/first", null(), id(3)));
        execute(
            &mut cmds,
            true,
            &policy(),
            &world,
            &mut FakeRefs::default(),
            &mut Messages::default(),
        );
        assert_eq!(
            *world.hook_asked.borrow(),
            ["refs/heads/first", "refs/heads/delete", "refs/heads/create"],
            "atomic pushes ask in the order of commands, and stop at the first that fails, so what follows isn't asked"
        );
    }

    #[test]
    fn the_current_branch_is_protected_through_a_symbolic_reference_too() {
        let world = Fake {
            checked_out: vec![("refs/heads/main", false)],
            head: Some("refs/heads/main"),
            aliases: vec![("refs/heads/alias", "refs/heads/main")],
            ..Fake::default()
        };
        let mut update = update("refs/heads/alias");
        assert_eq!(
            check(&mut update, &policy(), &world),
            Err("branch is currently checked out"),
            "unconfigured is refusing, as it is for the name of the branch itself"
        );
        assert_eq!(
            check(&mut delete("refs/heads/alias"), &policy(), &world),
            Err("branch is currently checked out"),
            "the checked out branch is protected before deletions are"
        );
        let lax = Policy {
            deny_current_branch: DenyAction::Ignore,
            ..policy()
        };
        assert_eq!(
            check(&mut delete("refs/heads/alias"), &lax, &world),
            Err("deletion of the current branch prohibited"),
            "the current branch can't be deleted through the alias either"
        );
        assert!(
            check(&mut delete("refs/heads/other"), &lax, &world).is_ok(),
            "what doesn't lead to the current branch isn't affected"
        );
    }

    #[test]
    fn a_checked_out_branch_is_refused_unless_ignored_or_warned_about() {
        let mut world = Fake::default();
        world.checked_out.push(("refs/heads/main", false));
        let mut messages = Messages::default();
        for (action, expected) in [
            (DenyAction::Unconfigured, Err("branch is currently checked out")),
            (DenyAction::Refuse, Err("branch is currently checked out")),
            (DenyAction::Warn, Ok(Plan::Update)),
            (DenyAction::Ignore, Ok(Plan::Update)),
        ] {
            let policy = Policy {
                deny_current_branch: action,
                ..policy()
            };
            assert_eq!(
                check_update(&mut update("refs/heads/main"), &policy, &world, &mut messages),
                expected,
                "{action:?}"
            );
        }
        assert_eq!(
            messages
                .0
                .iter()
                .filter(|message| message.text.starts_with(b"By default"))
                .count(),
            1,
            "only an unconfigured setting explains how to configure it"
        );
        let mut bare = Fake::default();
        bare.checked_out.push(("refs/heads/main", true));
        assert_eq!(
            check(&mut update("refs/heads/main"), &policy(), &bare),
            Ok(Plan::Update),
            "the branch of a bare repository can be updated"
        );
    }

    #[test]
    fn the_work_tree_follows_the_update_hook_and_precedes_the_new_value_with_update_instead() {
        let update_instead = Policy {
            deny_current_branch: DenyAction::UpdateInstead,
            ..policy()
        };
        let mut world = Fake::default();
        world.checked_out.push(("refs/heads/main", false));
        assert_eq!(
            check(&mut update("refs/heads/main"), &update_instead, &world),
            Ok(Plan::Update)
        );
        assert_eq!(
            *world.worktree_updated.borrow(),
            [("refs/heads/main".to_owned(), id(2))],
            "the work tree is brought up to the new value of the branch that is checked out"
        );
        assert_eq!(
            *world.hook_asked.borrow(),
            ["refs/heads/main"],
            "after the update hook was asked"
        );

        world.declined_by_hook.push("refs/heads/main");
        world.worktree_updated.borrow_mut().clear();
        assert_eq!(
            check(&mut update("refs/heads/main"), &update_instead, &world),
            Err("hook declined")
        );
        assert!(
            world.worktree_updated.borrow().is_empty(),
            "a hook that declines keeps the work tree as it is"
        );

        let mut refusing = Fake::default();
        refusing.checked_out.push(("refs/heads/main", false));
        refusing.worktree_refusal = Some("Working directory has unstaged changes");
        assert_eq!(
            check(&mut update("refs/heads/main"), &update_instead, &refusing),
            Err("Working directory has unstaged changes"),
            "what the work tree says is the reason of the rejection"
        );

        let mut elsewhere = Fake::default();
        elsewhere.checked_out.push(("refs/heads/other", false));
        assert_eq!(
            check(&mut update("refs/heads/main"), &update_instead, &elsewhere),
            Ok(Plan::Update)
        );
        assert!(
            elsewhere.worktree_updated.borrow().is_empty(),
            "a branch that isn't checked out leaves the work tree alone"
        );

        let mut bare = Fake::default();
        bare.checked_out.push(("refs/heads/main", true));
        assert_eq!(
            check(&mut update("refs/heads/main"), &update_instead, &bare),
            Ok(Plan::Update)
        );
        assert!(
            bare.worktree_updated.borrow().is_empty(),
            "a bare repository has no work tree to update"
        );

        let mut ignoring = Fake::default();
        ignoring.checked_out.push(("refs/heads/main", false));
        let ignoring_delete = Policy {
            deny_delete_current: DenyAction::Ignore,
            ..update_instead
        };
        assert_eq!(
            check(&mut delete("refs/heads/main"), &ignoring_delete, &ignoring),
            Ok(Plan::Delete { lease: Some(id(1)) })
        );
        assert_eq!(
            *ignoring.worktree_updated.borrow(),
            [("refs/heads/main".to_owned(), null())],
            "deleting the checked out branch asks for the work tree to become nothing, which Git then can't do"
        );
    }

    #[test]
    fn a_missing_new_object_is_a_bad_pack_before_deletions_and_fast_forwards_are_looked_at() {
        let mut world = Fake::default();
        world.missing.push(id(2));
        world.ancestry = Some(Ancestry::NotAncestor);
        let strict = Policy {
            deny_non_fast_forwards: true,
            ..policy()
        };
        assert_eq!(check(&mut update("refs/heads/a"), &strict, &world), Err("bad pack"));
        let mut deletion = delete("refs/heads/a");
        assert_eq!(
            check(&mut deletion, &strict, &world),
            Ok(Plan::Delete { lease: Some(id(1)) }),
            "deletions need no new object"
        );
    }

    #[test]
    fn deleting_branches_and_the_current_branch_can_be_denied() {
        let denying = Policy {
            deny_deletes: true,
            ..policy()
        };
        let world = Fake::default();
        assert_eq!(
            check(&mut delete("refs/heads/a"), &denying, &world),
            Err("deletion prohibited")
        );
        assert!(
            check(&mut delete("refs/tags/a"), &denying, &world).is_ok(),
            "only branches are protected"
        );
        assert_eq!(
            check(&mut Command::new("refs/heads/a", null(), null()), &denying, &world),
            Err("deletion prohibited"),
            "a deletion without an old value is denied like others, which Git doesn't do"
        );

        let head = Fake {
            head: Some("refs/heads/main"),
            ..Fake::default()
        };
        assert_eq!(
            check(&mut delete("refs/heads/main"), &policy(), &head),
            Err("deletion of the current branch prohibited"),
            "unconfigured is refusing"
        );
        let allowing = Policy {
            deny_delete_current: DenyAction::Warn,
            ..policy()
        };
        let mut messages = Messages::default();
        assert!(check_update(&mut delete("refs/heads/main"), &allowing, &head, &mut messages).is_ok());
        assert_eq!(messages.0.len(), 1, "a warning explains it");
        let mut worktree = Fake::default();
        worktree.checked_out.push(("refs/heads/other", true));
        assert_eq!(
            check(&mut delete("refs/heads/other"), &policy(), &worktree),
            Err("deletion of the current branch prohibited"),
            "a worktree counts, even of a bare repository"
        );
        let both = Policy {
            deny_deletes: true,
            ..policy()
        };
        assert_eq!(
            check(&mut delete("refs/heads/main"), &both, &head),
            Err("deletion prohibited"),
            "deletions are denied before the current branch is looked at"
        );
    }

    #[test]
    fn non_fast_forwards_are_denied_only_for_branches_when_configured() {
        let strict = Policy {
            deny_non_fast_forwards: true,
            ..policy()
        };
        let mut world = Fake {
            ancestry: Some(Ancestry::NotAncestor),
            ..Fake::default()
        };
        assert_eq!(
            check(&mut update("refs/heads/a"), &strict, &world),
            Err("non-fast-forward")
        );
        assert!(
            check(&mut update("refs/tags/a"), &strict, &world).is_ok(),
            "tags may move"
        );
        assert!(
            check(&mut update("refs/heads/a"), &policy(), &world).is_ok(),
            "not configured"
        );
        assert!(
            check(&mut Command::new("refs/heads/a", null(), id(2)), &strict, &world).is_ok(),
            "creations have no history to keep"
        );
        world.ancestry = Some(Ancestry::BadObjects);
        assert_eq!(check(&mut update("refs/heads/a"), &strict, &world), Err("bad ref"));
    }

    #[test]
    fn a_value_the_store_refuses_is_reported_after_the_policies_have_been_applied() {
        let mut world = Fake::default();
        world.invalid.push(id(2));
        world.ancestry = Some(Ancestry::BadObjects);
        let strict = Policy {
            deny_non_fast_forwards: true,
            ..policy()
        };
        assert_eq!(
            check(&mut update("refs/heads/a"), &strict, &world),
            Err("bad ref"),
            "a non-commit that is checked for fast-forwards is a bad ref"
        );
        assert_eq!(
            check(&mut update("refs/heads/a"), &policy(), &world),
            Err("invalid new value provided")
        );
        assert!(check(&mut delete("refs/heads/a"), &policy(), &world).is_ok());
    }

    #[test]
    fn deletions_of_corrupt_or_missing_references_are_leaseless() {
        let mut world = Fake::default();
        world.unreadable.push(id(1));
        world.existing_refs.push("refs/heads/corrupt");
        let mut corrupt = delete("refs/heads/corrupt");
        assert_eq!(check(&mut corrupt, &policy(), &world), Ok(Plan::Delete { lease: None }));
        assert!(
            !corrupt.did_not_exist,
            "the reference is there, even though it is broken"
        );
        let mut gone = delete("refs/heads/gone");
        assert_eq!(check(&mut gone, &policy(), &world), Ok(Plan::Delete { lease: None }));
        assert!(gone.did_not_exist);
        let mut null_old = Command::new("refs/heads/gone", null(), null());
        assert_eq!(
            check(&mut null_old, &policy(), &world),
            Ok(Plan::Delete { lease: None }),
            "an unknown old value can't be a lease either"
        );
    }

    #[test]
    fn hidden_references_are_rejected_unless_they_failed_before() {
        let mut commands = vec![
            update("refs/heads/h"),
            delete("refs/heads/h"),
            update("refs/heads/ok"),
            update("refs/heads/h"),
        ];
        commands[3].error = Some("earlier".into());
        reject_hidden(&mut commands, &|name| name.starts_with(b"refs/heads/h"));
        let errors: Vec<_> = commands.iter().map(|command| command.error.as_deref()).collect();
        assert_eq!(
            errors,
            [
                Some("deny updating a hidden ref"),
                Some("deny deleting a hidden ref"),
                None,
                Some("earlier")
            ]
        );
    }

    #[test]
    fn aliased_updates_are_executed_once_when_consistent_and_rejected_otherwise() {
        let resolve = |name: &BStr| match name.to_str().expect("utf8") {
            "refs/heads/alias" => Resolved::Target("refs/heads/main".into()),
            "refs/heads/broken" => Resolved::Broken,
            "refs/heads/other-alias" => Resolved::Target("refs/heads/elsewhere".into()),
            _ => Resolved::Direct,
        };
        let mut consistent = vec![update("refs/heads/alias"), update("refs/heads/main")];
        check_aliased_updates(
            &mut consistent,
            &resolve,
            &|id| id.to_hex().to_string(),
            &mut Messages::default(),
        );
        assert_eq!(
            consistent
                .iter()
                .map(|c| (c.skip_update, c.error.clone()))
                .collect::<Vec<_>>(),
            [(true, None), (false, None)],
            "the symbolic reference is reported ok without being executed"
        );

        let mut inconsistent = vec![
            update("refs/heads/alias"),
            Command::new("refs/heads/main", id(1), id(3)),
        ];
        check_aliased_updates(
            &mut inconsistent,
            &resolve,
            &|id| id.to_hex().to_string(),
            &mut Messages::default(),
        );
        assert!(
            inconsistent
                .iter()
                .all(|c| c.skip_update && c.error.as_deref() == Some("inconsistent aliased update"))
        );

        let mut others = vec![update("refs/heads/broken"), update("refs/heads/other-alias")];
        check_aliased_updates(
            &mut others,
            &resolve,
            &|id| id.to_hex().to_string(),
            &mut Messages::default(),
        );
        assert_eq!(others[0].error.as_deref(), Some("broken symref"));
        assert!(others[0].skip_update);
        assert_eq!(
            others[1].error, None,
            "an alias of a reference that isn't updated is just updated"
        );
        assert!(!others[1].skip_update);

        let mut failed = vec![update("refs/heads/alias"), update("refs/heads/main")];
        failed[0].error = Some("earlier".into());
        check_aliased_updates(
            &mut failed,
            &resolve,
            &|id| id.to_hex().to_string(),
            &mut Messages::default(),
        );
        assert!(!failed[1].skip_update, "commands that failed don't take part");
    }

    #[derive(Default)]
    struct FakeRefs {
        begin_fails: bool,
        commit_fails: bool,
        /// Die in the first transaction that is committed.
        dies: bool,
        reject: Vec<(usize, &'static str)>,
        commits: RefCell<Vec<Vec<usize>>>,
        began: RefCell<Vec<bool>>,
    }

    impl Refs for FakeRefs {
        fn begin(&mut self, atomic: bool) -> Result<(), StoreFailure> {
            self.began.borrow_mut().push(atomic);
            if self.begin_fails {
                Err(StoreFailure::Start)
            } else {
                Ok(())
            }
        }
        fn commit(&mut self, queued: &[Queued]) -> Result<Vec<(usize, &'static str)>, StoreFailure> {
            self.commits
                .borrow_mut()
                .push(queued.iter().map(|queued| queued.index).collect());
            if self.dies {
                return Err(StoreFailure::Died);
            }
            if self.commit_fails {
                return Err(StoreFailure::Commit);
            }
            Ok(self.reject.clone())
        }
    }

    fn errors(commands: &[Command]) -> Vec<Option<&str>> {
        commands.iter().map(|command| command.error.as_deref()).collect()
    }

    #[test]
    fn a_death_in_the_transaction_of_deletions_stops_the_transaction_after_it() {
        let mut commands = vec![update("refs/heads/a"), delete("refs/heads/b")];
        let mut refs = FakeRefs {
            dies: true,
            ..Default::default()
        };
        execute(
            &mut commands,
            false,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(
            *refs.commits.borrow(),
            [vec![1]],
            "Git exits where the hook refused, so the other updates are never attempted"
        );
    }

    #[test]
    fn non_atomic_execution_deletes_first_and_rejects_single_commands() {
        let mut commands = vec![
            update("refs/heads/a"),
            delete("refs/heads/b"),
            update("refs/heads/funny name"),
            update("refs/heads/c"),
        ];
        let mut refs = FakeRefs {
            // The second queued command of the second transaction, which is `c`.
            reject: vec![],
            ..Default::default()
        };
        execute(
            &mut commands,
            false,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(errors(&commands), [None, None, Some("funny refname"), None]);
        assert_eq!(
            *refs.commits.borrow(),
            [vec![1], vec![0, 3]],
            "deletions are a transaction of their own, first"
        );
        assert_eq!(*refs.began.borrow(), [false, false]);

        let mut commands = vec![update("refs/heads/a"), update("refs/heads/c")];
        let mut refs = FakeRefs {
            reject: vec![(1, "incorrect old value provided")],
            ..Default::default()
        };
        execute(
            &mut commands,
            false,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(errors(&commands), [None, Some("incorrect old value provided")]);
    }

    #[test]
    fn a_failure_of_the_store_fails_every_command_even_those_that_failed_for_another_reason() {
        let mut commands = vec![
            update("refs/heads/a"),
            update("refs/heads/funny name"),
            delete("refs/heads/d"),
        ];
        let mut refs = FakeRefs {
            commit_fails: true,
            ..Default::default()
        };
        execute(
            &mut commands,
            false,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(
            errors(&commands),
            [
                Some("failed to update refs"),
                Some("failed to update refs"),
                Some("failed to update refs")
            ]
        );
        assert_eq!(
            refs.commits.borrow().len(),
            1,
            "nothing is left to process in the second transaction, as everything failed"
        );

        let mut commands = vec![update("refs/heads/a")];
        let mut refs = FakeRefs {
            begin_fails: true,
            ..Default::default()
        };
        execute(
            &mut commands,
            false,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(errors(&commands), [Some("transaction failed to start")]);
        assert!(refs.commits.borrow().is_empty());
    }

    #[test]
    fn nothing_is_started_without_commands_to_execute() {
        let mut commands = vec![update("refs/heads/a")];
        commands[0].error = Some("earlier".into());
        let mut refs = FakeRefs::default();
        execute(
            &mut commands,
            false,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert!(refs.began.borrow().is_empty());
        assert_eq!(errors(&commands), [Some("earlier")]);
    }

    #[test]
    fn an_atomic_execution_fails_as_a_whole_at_the_first_failure() {
        let mut commands = vec![
            update("refs/heads/a"),
            update("refs/heads/funny name"),
            update("refs/heads/c"),
        ];
        let mut refs = FakeRefs::default();
        execute(
            &mut commands,
            true,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(
            errors(&commands),
            [
                Some("atomic push failure"),
                Some("funny refname"),
                Some("atomic push failure")
            ]
        );
        assert!(refs.commits.borrow().is_empty(), "nothing is committed");

        let mut commands = vec![update("refs/heads/a"), update("refs/heads/c")];
        let mut refs = FakeRefs {
            commit_fails: true,
            ..Default::default()
        };
        execute(
            &mut commands,
            true,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(
            errors(&commands),
            [Some("atomic transaction failed"), Some("atomic transaction failed")]
        );

        let mut commands = vec![update("refs/heads/a"), delete("refs/heads/c")];
        let mut refs = FakeRefs::default();
        execute(
            &mut commands,
            true,
            &policy(),
            &Fake::default(),
            &mut refs,
            &mut Messages::default(),
        );
        assert_eq!(errors(&commands), [None, None]);
        assert_eq!(
            *refs.commits.borrow(),
            [vec![0, 1]],
            "all in one transaction, in the order they came"
        );
        assert_eq!(*refs.began.borrow(), [true]);
    }
}
