//! What decides how a receiver treats pushes, read from the configuration of the repository the way `git receive-pack` does.
use gix_error::{ErrorExt, Result, ResultExt, bail, message, unsupported, validation};
use gix_object::bstr::ByteSlice;

use super::hidden::HiddenRefs;
use crate::Repository;

/// What to do about a push to the branch that is checked out, or the deletion of the current branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyAction {
    /// Nothing was configured, which refuses and explains how to configure it.
    Unconfigured,
    /// Allow it.
    Ignore,
    /// Allow it, but warn.
    Warn,
    /// Refuse it.
    Refuse,
    /// Update the working tree as well, which isn't supported yet.
    UpdateInstead,
}

/// The settings of a receiver.
#[derive(Debug, Clone)]
pub struct Options {
    /// `receive.denyDeletes`: refuse to delete branches.
    pub deny_deletes: bool,
    /// `receive.denyNonFastForwards`: refuse to move branches to commits that don't have the old value as ancestor.
    pub deny_non_fast_forwards: bool,
    /// `receive.denyCurrentBranch`.
    pub deny_current_branch: DenyAction,
    /// `receive.denyDeleteCurrent`.
    pub deny_delete_current: DenyAction,
    /// `receive.hideRefs` and `transfer.hideRefs`.
    pub hidden: HiddenRefs,
    /// `receive.advertiseAtomic`.
    pub advertise_atomic: bool,
    /// `receive.advertisePushOptions`.
    pub advertise_push_options: bool,
    /// `repack.useDeltaBaseOffset`.
    pub prefer_ofs_delta: bool,
    /// `receive.keepAlive`: how long a client is left waiting while hooks run, before it is told that all is well, if at all.
    pub keep_alive: Option<std::time::Duration>,
    /// `receive.autogc`: whether maintenance is due after objects were received, as Git runs `gc --auto` then.
    pub auto_gc: bool,
    /// How to store received objects.
    pub quarantine: gix_odb::quarantine::Options,
}

fn parse_bool(key: &str, value: &str) -> Result<bool> {
    gix_config::Boolean::try_from(value.as_bytes().as_bstr())
        .map(|boolean| boolean.0)
        .map_err(|_| ErrorExt::raise(validation(format!("bad boolean config value '{value}' for '{key}'"))))
}

fn parse_deny_action(key: &str, value: &str) -> Result<DenyAction> {
    Ok(match value.to_ascii_lowercase().as_str() {
        "ignore" => DenyAction::Ignore,
        "warn" => DenyAction::Warn,
        "refuse" => DenyAction::Refuse,
        "updateinstead" => DenyAction::UpdateInstead,
        _ => {
            if parse_bool(key, value)? {
                DenyAction::Refuse
            } else {
                DenyAction::Ignore
            }
        }
    })
}

impl Options {
    /// Read the settings from the configuration of `repo`.
    ///
    /// Fail if a setting has an invalid value or can't be honored, which is to refuse to receive instead of receiving in a
    /// way that differs from what the configuration says. That's the case if there are hooks, push certificates, `proc-receive`,
    /// `updateInstead`, namespaces, object checking, or anything the [`quarantine`](gix_odb::quarantine::Options::from_config())
    /// can't handle, and if the repository is shallow.
    pub fn from_repository(repo: &Repository) -> Result<Self> {
        let config = repo.config_snapshot();
        let get = |key: &str| config.string(key).map(|value| value.to_string());
        let boolean = |key: &str, default: bool| -> Result<bool> {
            get(key).map_or(Ok(default), |value| parse_bool(key, &value))
        };
        let deny_action = |key: &str| -> Result<DenyAction> {
            get(key).map_or(Ok(DenyAction::Unconfigured), |value| parse_deny_action(key, &value))
        };

        let deny_current_branch = deny_action("receive.denyCurrentBranch")?;
        if deny_current_branch == DenyAction::UpdateInstead {
            bail!(unsupported(
                "receive.denyCurrentBranch=updateInstead isn't supported yet"
            ));
        }
        for (key, what) in [
            ("receive.certNonceSeed", "signed pushes"),
            ("receive.procReceiveRefs", "proc-receive"),
            ("transfer.advertiseSID", "advertising session ids"),
            ("receive.updateServerInfo", "updating server info after a push"),
            (
                "core.alternateRefsCommand",
                "running a command to list the references of alternates",
            ),
            (
                "core.alternateRefsPrefixes",
                "limiting the references of alternates to prefixes",
            ),
            ("extensions.partialClone", "partial clones"),
        ] {
            // `receive.updateServerInfo=false` is what is done anyway.
            let enabled = if key == "receive.updateServerInfo" || key == "transfer.advertiseSID" {
                boolean(key, false)?
            } else {
                get(key).is_some()
            };
            if enabled {
                bail!(unsupported(format!("{key} isn't supported, as {what} aren't")));
            }
        }
        if config
            .plumbing()
            .sections_by_name("remote")
            .into_iter()
            .flatten()
            .any(|section| {
                section.body().value("promisor").is_some() || section.body().value("partialclonefilter").is_some()
            })
        {
            bail!(unsupported(
                "a promisor remote is configured, and partial clones aren't supported"
            ));
        }
        // Git doesn't enter a repository that isn't owned by the user, and a reduced trust hides configured hooks from the gate
        // below, so what Git would refuse must not be received into at all.
        if repo.git_dir_trust() != gix_sec::Trust::Full {
            bail!(unsupported(
                "the repository isn't fully trusted, which Git refuses to receive into as well"
            ));
        }
        // What a repository declares to need is unknown unless it was checked to not matter here, as `compatObjectFormat`
        // makes Git store a mapping of every object that it writes.
        for section in config.plumbing().sections_by_name("extensions").into_iter().flatten() {
            for key in section.body().value_names() {
                let key = key.to_string();
                if ![
                    "objectformat",
                    "refstorage",
                    "worktreeconfig",
                    "noop",
                    "noop-v1",
                    "preciousobjects",
                    "relativeworktrees",
                    "submodulepathconfig",
                ]
                .iter()
                .any(|known| key.eq_ignore_ascii_case(known))
                {
                    bail!(unsupported(format!(
                        "extensions.{key} isn't known to be safe to receive with"
                    )));
                }
            }
        }
        if repo.namespace().is_some() {
            bail!(unsupported("namespaces aren't supported yet"));
        }
        if repo
            .is_shallow()
            .or_raise(|| message("Could not read the shallow file"))?
        {
            bail!(unsupported("receiving into a shallow repository isn't supported"));
        }
        reject_hooks(repo)?;

        let quarantine = gix_odb::quarantine::Options::from_config(
            repo.object_hash(),
            repo.config
                .fsync_options()
                .or_raise(|| message("Could not read the flush configuration"))?,
            &get,
        )?;
        Ok(Options {
            deny_deletes: boolean("receive.denyDeletes", false)?,
            deny_non_fast_forwards: boolean("receive.denyNonFastForwards", false)?,
            deny_current_branch,
            deny_delete_current: deny_action("receive.denyDeleteCurrent")?,
            hidden: HiddenRefs::from_config(config.plumbing(), "receive"),
            advertise_atomic: boolean("receive.advertiseAtomic", true)?,
            advertise_push_options: boolean("receive.advertisePushOptions", false)?,
            prefer_ofs_delta: boolean("repack.useDeltaBaseOffset", true)?,
            keep_alive: {
                let seconds = config.integer("receive.keepAlive").unwrap_or(5);
                u64::try_from(seconds)
                    .ok()
                    .filter(|seconds| *seconds > 0)
                    .map(std::time::Duration::from_secs)
            },
            auto_gc: boolean("receive.autogc", true)?,
            quarantine,
        })
    }
}

/// Hooks that a receiver would run, which can't be run yet. Refuse instead of ignoring them, as skipping a hook that guards
/// a repository is not what whoever installed it wants.
fn reject_hooks(repo: &Repository) -> Result<()> {
    let settings = gix_hook::Settings::from_config(&repo.config.resolved, &mut repo.filter_config_section())
        .or_raise(|| message("Failed to read hook configuration"))?;
    let dir = repo
        .hooks_dir(gix_hook::Side::Receive)
        .or_raise(|| message("Could not find out where hooks are"))?;
    for event in ["push-to-checkout", "reference-transaction"] {
        if !gix_hook::Hooks::resolve(event, &dir, Some(&settings)).is_empty() {
            bail!(unsupported(format!(
                "a '{event}' hook is defined, and hooks aren't supported yet"
            )));
        }
    }
    Ok(())
}
