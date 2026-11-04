//! The decoded status report of a push.
use bstr::{BStr, BString, ByteSlice};
use gix_error::{Result, bail, corruption};
use gix_hash::ObjectId;

use super::{Request, StatusFormat};

/// What the server says about unpacking the pack we sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unpack {
    /// The pack was unpacked successfully.
    Ok,
    /// Unpacking failed for the given reason. In that case, no command was applied.
    Failed(BString),
}

/// One update the server performed or was asked to perform, as described by the `option` lines of `report-status-v2`.
///
/// Absent values mean the server didn't state them, in which case the command's own values apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Update {
    /// The reference that was actually updated if it differs from the command's, for example because a hook redirected it.
    pub ref_name: Option<BString>,
    /// The value the updated reference had before.
    pub old_id: Option<ObjectId>,
    /// The value the updated reference has now.
    pub new_id: Option<ObjectId>,
    /// The update didn't fast-forward.
    pub forced: bool,
}

/// What happened to a single command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The server applied the command.
    ///
    /// With [`StatusFormat::V1`], `updates` is empty. With [`StatusFormat::V2`], there is one entry for every time
    /// the server reported the command as `ok`, so there is at least one.
    Accepted {
        /// The details of the updates, see [`Update`].
        updates: Vec<Update>,
    },
    /// The server refused to apply the command, and states why. The reason is empty if it didn't.
    Rejected {
        /// The server's explanation, like `pre-receive hook declined` or `atomic push failure`.
        reason: BString,
    },
    /// The server reported a failure that can't be trusted as proof the command was *not* applied.
    ///
    /// Git's `receive-pack` commits deletions in a first batch and everything else in a second one when not
    /// pushing atomically. If committing a batch fails with a generic error, it reports that error for *every*
    /// command, including deletions already committed. The reference may thus be updated, absent or unchanged, and
    /// the caller must reconcile with the remote before retrying or recording the command as failed.
    Uncertain {
        /// The server's explanation, which is classified by being `failed to update refs` or `transaction failed to start`.
        reason: BString,
    },
}

/// The outcome for one of the pushed commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandReport {
    /// The name of the reference as used in the command.
    pub ref_name: BString,
    /// What the server did with it.
    pub status: Status,
}

/// A complete and consistent status report from the server.
///
/// Note that this doesn't imply success: check [`Report::is_success()`] or inspect the individual statuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Whether the pack could be unpacked.
    pub unpack: Unpack,
    /// One entry per command, in the order the commands were sent.
    pub commands: Vec<CommandReport>,
}

impl Report {
    /// Return `true` if all commands have a trustworthy outcome, which means none of them is [`Status::Uncertain`].
    /// Rejections are certain outcomes if they are not marked as uncertain.
    pub fn is_definite(&self) -> bool {
        !self
            .commands
            .iter()
            .any(|command| matches!(command.status, Status::Uncertain { .. }))
    }

    /// Return `true` if the pack was unpacked and the server applied every command.
    pub fn is_success(&self) -> bool {
        self.unpack == Unpack::Ok
            && self
                .commands
                .iter()
                .all(|command| matches!(command.status, Status::Accepted { .. }))
    }
}

enum State {
    Unseen,
    Rejected(BString),
    Accepted(Vec<Update>),
}

fn strip_newline(line: &[u8]) -> &BStr {
    line.strip_suffix(b"\n").unwrap_or(line).as_bstr()
}

fn bad(line: &BStr, why: &str) -> gix_error::Error {
    gix_error::message!("Invalid status line {line:?} from the remote: {why}").corrupted_error()
}

pub(super) fn parse<'a>(request: &Request, lines: impl IntoIterator<Item = &'a [u8]>) -> Result<Report> {
    let mut lines = lines.into_iter().map(strip_newline);
    let unpack = {
        let line = lines.next().ok_or_else(|| {
            corruption("The status report was empty, so the outcome of the push is unknown").corrupted_error()
        })?;
        match line.strip_prefix(b"unpack ") {
            Some(status) if status == b"ok" => Unpack::Ok,
            Some(reason) if !reason.is_empty() => Unpack::Failed(reason.into()),
            _ => return Err(bad(line, "expected 'unpack <status>'")),
        }
    };

    let commands = request.commands();
    let command_indices: std::collections::HashMap<&BStr, usize> = commands
        .iter()
        .enumerate()
        .map(|(index, command)| (command.ref_name(), index))
        .collect();
    let mut states: Vec<State> = commands.iter().map(|_| State::Unseen).collect();
    // The command whose latest `ok` line may still receive `option` lines, which is only the case with V2.
    let mut open_update: Option<(usize, usize)> = None;

    for line in lines {
        let (head, rest) = line
            .split_once_str(" ")
            .ok_or_else(|| bad(line, "expected '<ok|ng|option> <value>'"))?;
        if head == b"option" {
            if request.status_format() != StatusFormat::V2 {
                return Err(bad(line, "'option' is only valid with report-status-v2"));
            }
            let (command_index, update_index) =
                open_update.ok_or_else(|| bad(line, "'option' without a preceding 'ok'"))?;
            let State::Accepted(updates) = &mut states[command_index] else {
                unreachable!("open updates only exist for accepted commands")
            };
            apply_option(&mut updates[update_index], rest.as_bstr(), request.hash_kind(), line)?;
            continue;
        }
        open_update = None;
        if head != b"ok" && head != b"ng" {
            return Err(bad(line, "expected 'ok', 'ng' or 'option'"));
        }
        let (ref_name, reason) = match rest.split_once_str(" ") {
            Some((name, reason)) => (name, Some(reason)),
            None => (rest, None),
        };
        let command_index = *command_indices
            .get(ref_name.as_bstr())
            .ok_or_else(|| bad(line, "the reference was not part of this push"))?;
        match (head, &mut states[command_index]) {
            (b"ng", state @ State::Unseen) => *state = State::Rejected(reason.unwrap_or_default().into()),
            (b"ng", _) => {
                return Err(bad(
                    line,
                    "the command was reported before, so it can't be rejected now",
                ));
            }
            (_, State::Rejected(_)) => {
                return Err(bad(
                    line,
                    "the command was rejected before, so it can't be accepted now",
                ));
            }
            (_, _) if reason.is_some() => return Err(bad(line, "an 'ok' line has no further text")),
            (_, state @ State::Unseen) => {
                *state = State::Accepted(match request.status_format() {
                    StatusFormat::V1 => Vec::new(),
                    StatusFormat::V2 => vec![Update::default()],
                });
                if request.status_format() == StatusFormat::V2 {
                    open_update = Some((command_index, 0));
                }
            }
            (_, State::Accepted(updates)) => {
                if request.status_format() == StatusFormat::V1 {
                    return Err(bad(line, "the command was reported before"));
                }
                updates.push(Update::default());
                open_update = Some((command_index, updates.len() - 1));
            }
        }
    }

    let mut out = Vec::with_capacity(commands.len());
    for (command, state) in commands.iter().zip(states) {
        let status = match state {
            State::Unseen => bail!(corruption(format!(
                "The remote didn't report the status of '{}', so it may or may not have been updated",
                command.ref_name()
            ))),
            State::Rejected(reason) if !request.is_atomic() && is_generic_commit_failure(reason.as_bstr()) => {
                Status::Uncertain { reason }
            }
            State::Rejected(reason) => Status::Rejected { reason },
            State::Accepted(updates) => Status::Accepted { updates },
        };
        out.push(CommandReport {
            ref_name: command.ref_name().into(),
            status,
        });
    }
    let accepted = out
        .iter()
        .filter(|command| matches!(command.status, Status::Accepted { .. }))
        .count();
    if matches!(unpack, Unpack::Failed(_)) && accepted != 0 {
        bail!(corruption(
            "The remote reported that unpacking failed, yet it accepted some commands"
        ));
    }
    // Git rejects commands before the atomic transaction starts if their objects are missing, if they target hidden
    // references or if they are inconsistent, and commits the others. So a mix is legitimate and definite, except
    // for the failures of the transaction itself: nothing is committed if they are reported.
    if request.is_atomic()
        && accepted != 0
        && out.iter().any(|command| {
            matches!(&command.status, Status::Rejected { reason }
                if reason == "atomic push failure" || reason == "atomic transaction failed")
        })
    {
        bail!(corruption(
            "The remote reported a failed atomic transaction, yet it accepted some commands"
        ));
    }
    Ok(Report { unpack, commands: out })
}

/// The reasons `receive-pack` assigns to all commands if a non-atomic batch of ref updates failed as a whole,
/// which includes deletions committed by the first batch. In atomic mode, nothing is committed on failure.
fn is_generic_commit_failure(reason: &BStr) -> bool {
    reason == "failed to update refs" || reason == "transaction failed to start"
}

fn apply_option(update: &mut Update, option: &BStr, hash_kind: gix_hash::Kind, line: &BStr) -> Result {
    let (key, value) = match option.split_once_str(" ") {
        Some((key, value)) => (key.as_bstr(), Some(value.as_bstr())),
        None => (option, None),
    };
    let object_id = |value: Option<&BStr>| -> Result<ObjectId> {
        let id = value
            .and_then(|value| ObjectId::from_hex(value).ok())
            .ok_or_else(|| bad(line, "expected a hexadecimal object id"))?;
        if id.kind() != hash_kind {
            return Err(bad(line, "the object id doesn't use the repository's hash"));
        }
        Ok(id)
    };
    let duplicate = || bad(line, "the option was given more than once for one update");
    match key.as_bytes() {
        b"refname" => {
            let name = value
                .filter(|name| !name.is_empty())
                .ok_or_else(|| bad(line, "expected a reference name"))?;
            if update.ref_name.replace(name.into()).is_some() {
                return Err(duplicate());
            }
        }
        b"old-oid" => {
            if update.old_id.replace(object_id(value)?).is_some() {
                return Err(duplicate());
            }
        }
        b"new-oid" => {
            if update.new_id.replace(object_id(value)?).is_some() {
                return Err(duplicate());
            }
        }
        b"forced-update" if std::mem::replace(&mut update.forced, true) => return Err(duplicate()),
        b"forced-update" => {}
        // Unknown options are informational and may be added by the server at any time.
        _ => {}
    }
    Ok(())
}
