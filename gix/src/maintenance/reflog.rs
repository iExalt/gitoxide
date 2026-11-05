//! Expiring the entries of reflogs, like `git reflog expire --all` does for `git gc`.
//!
//! An entry is removed if it is old enough, or if it is a bit younger and the commits it mentions can't be reached any more,
//! which is decided by two ages: `gc.reflogExpire` and `gc.reflogExpireUnreachable`, which can be set for references that
//! match a pattern with `gc.<pattern>.reflogExpire` and `gc.<pattern>.reflogExpireUnreachable`. `refs/stash` never expires
//! unless it's configured to.
//!
//! # The defaults
//!
//! Git 2.56's documentation says that entries expire after 90 days, or after 30 days if they are unreachable. Its code does
//! the reverse: `REFLOG_EXPIRE_OPTIONS_INIT()` sets 30 days for the first and 90 days for the second, which makes every entry
//! older than 30 days expire, reachable or not. An entry is only removed here if the documented and the actual behavior
//! of Git would both remove it, which with nothing configured is the documented one, so that nothing is lost that
//! somebody who read the documentation expects to be there. Configuring the ages gets rid of the difference.
//!
//! Unlike Git, the previous values of the entries that remain aren't rewritten, and the references aren't updated. Like in
//! Git, replacement objects are followed while looking for what is reachable.
use gix_error::{Result, ResultExt, message};
use gix_hash::ObjectId;
use gix_hashtable::HashSet;
use gix_object::{
    Find,
    bstr::{BStr, BString, ByteSlice},
};
use gix_ref::{Target, log::Line};

use crate::Repository;

const DAY: i64 = 24 * 3600;
/// An age of `0` means that nothing expires, and `i64::MAX` that everything does.
type Timestamp = i64;

/// Options for [`Repository::expire_reflogs()`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Options {
    /// The time that ages are relative to, which is the current time if `None`.
    pub now: Option<gix_date::Time>,
    /// If `true`, decide which entries go and report that, but change nothing.
    pub dry_run: bool,
}

/// What expiring did, or, with [`dry_run`](Options::dry_run), would do.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Outcome {
    /// The number of reflogs that were looked at and changed or kept as they are.
    pub reflogs: usize,
    /// The number of entries that were removed.
    pub removed: usize,
    /// The number of entries that remain.
    pub kept: usize,
    /// The reflogs that couldn't be expired, with the reason, like those whose reference is locked by somebody else. The others
    /// were done nonetheless, as in Git, and a later run can try again. If deciding fails partway through a reflog, the
    /// entries that were chosen to go before that are removed, which is fine as each decision stands on its own.
    pub failed: Vec<(gix_ref::FullName, gix_error::Error)>,
}

/// The ages for one policy, which are `0` for never.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Policy {
    total: Timestamp,
    unreachable: Timestamp,
}

struct Pattern {
    pattern: BString,
    policy: Policy,
}

struct Configuration {
    patterns: Vec<Pattern>,
    total: Option<Timestamp>,
    unreachable: Option<Timestamp>,
}

impl Configuration {
    fn read(repo: &Repository, now: &gix_date::Zoned) -> Result<Self> {
        let mut out = Configuration {
            patterns: Vec::new(),
            total: None,
            unreachable: None,
        };
        let parse = |value: BString, key: &str| -> Result<Timestamp> {
            let value = value.to_str().or_raise(|| message!("The value of {key} isn't text"))?;
            Ok(match value {
                "never" | "false" => 0,
                "all" | "now" => i64::MAX,
                other => {
                    gix_date::parse(other, Some(now.clone()))
                        .or_raise(|| message!("Could not interpret {key} = {other}"))?
                        .seconds
                }
            })
        };
        let config = repo.config_snapshot();
        for section in config.plumbing().sections_by_name("gc").into_iter().flatten() {
            let pattern = section.header().subsection_name().map(ToOwned::to_owned);
            for (key, is_total) in [("reflogExpire", true), ("reflogExpireUnreachable", false)] {
                let Some(value) = section.value(key) else { continue };
                let age = parse(value, &format!("gc.{key}"))?;
                match &pattern {
                    None if is_total => out.total = Some(age),
                    None => out.unreachable = Some(age),
                    Some(pattern) => {
                        // A pattern starts with nothing expiring, and keeps what it was configured with, like in Git.
                        let position = match out.patterns.iter().position(|existing| existing.pattern == *pattern) {
                            Some(position) => position,
                            None => {
                                out.patterns.push(Pattern {
                                    pattern: pattern.clone(),
                                    policy: Policy {
                                        total: 0,
                                        unreachable: 0,
                                    },
                                });
                                out.patterns.len() - 1
                            }
                        };
                        let policy = &mut out.patterns[position].policy;
                        if is_total {
                            policy.total = age;
                        } else {
                            policy.unreachable = age;
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// The policy for the reference `name`, with `defaults` for what isn't configured, which is what Git's
    /// `reflog_expire_options_set_refname()` does.
    fn policy(&self, name: &BStr, defaults: Policy) -> Policy {
        for entry in &self.patterns {
            if gix_glob::wildmatch(entry.pattern.as_bstr(), name, gix_glob::wildmatch::Mode::empty()) {
                return entry.policy;
            }
        }
        if name == "refs/stash" {
            return Policy {
                total: 0,
                unreachable: 0,
            };
        }
        Policy {
            total: self.total.unwrap_or(defaults.total),
            unreachable: self.unreachable.unwrap_or(defaults.unreachable),
        }
    }
}

/// What a commit has that matters for finding out what is reachable.
struct CommitInfo {
    parents: Vec<ObjectId>,
    date: Timestamp,
}

struct Objects<'a> {
    repo: &'a Repository,
    buf: Vec<u8>,
}

impl Objects<'_> {
    /// Return the commit that `id` is or points to through tags, or `None` if it isn't one, or doesn't exist.
    fn commit(&mut self, id: &gix_hash::oid) -> Result<Option<ObjectId>> {
        let mut id = id.to_owned();
        for _ in 0..64 {
            let Some(data) = self.repo.objects.try_find(&id, &mut self.buf)? else {
                return Ok(None);
            };
            match data.kind {
                gix_object::Kind::Commit => return Ok(Some(id)),
                gix_object::Kind::Tag => {
                    id = gix_object::TagRefIter::from_bytes(data.data, id.kind()).target_id()?;
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    fn info(&mut self, id: &gix_hash::oid) -> Result<Option<CommitInfo>> {
        let Some(data) = self.repo.objects.try_find(id, &mut self.buf)? else {
            return Ok(None);
        };
        if data.kind != gix_object::Kind::Commit {
            return Ok(None);
        }
        let hash = id.kind();
        let parents = gix_object::CommitRefIter::from_bytes(data.data, hash)
            .parent_ids()
            .collect();
        let date = gix_object::CommitRefIter::from_bytes(data.data, hash)
            .committer()?
            .seconds();
        Ok(Some(CommitInfo { parents, date }))
    }
}

/// How entries are found to be unreachable for the reflog of one reference.
enum Kind {
    /// Everything that is old enough expires.
    Always,
    /// Unreachable from the tips that are in the set.
    Reachable(Reach),
}

/// The commits that can be reached from the tips, as far as it was needed so far, which is Git's `mark_reachable()`.
struct Reach {
    reachable: HashSet<ObjectId>,
    /// Commits that were reached, but whose parents weren't followed as they were older than `limit`.
    leftover: Vec<ObjectId>,
    limit: Timestamp,
}

impl Reach {
    fn new(tips: Vec<ObjectId>, limit: Timestamp, objects: &mut Objects<'_>) -> Result<Self> {
        let mut out = Reach {
            reachable: HashSet::default(),
            leftover: Vec::new(),
            limit,
        };
        out.mark(tips, objects)?;
        Ok(out)
    }

    fn mark(&mut self, mut pending: Vec<ObjectId>, objects: &mut Objects<'_>) -> Result<()> {
        let mut leftover = Vec::new();
        while let Some(id) = pending.pop() {
            if self.reachable.contains(&id) {
                continue;
            }
            let Some(info) = objects.info(&id)? else {
                continue;
            };
            self.reachable.insert(id);
            if info.date < self.limit {
                leftover.push(id);
                continue;
            }
            pending.extend(
                info.parents
                    .into_iter()
                    .filter(|parent| !self.reachable.contains(parent)),
            );
        }
        self.leftover = leftover;
        Ok(())
    }

    /// Return `true` if the commit that `id` is or points to isn't reachable. What isn't a commit is never unreachable.
    fn is_unreachable(&mut self, id: &gix_hash::oid, objects: &mut Objects<'_>) -> Result<bool> {
        if id.is_null() {
            return Ok(false);
        }
        let Some(commit) = objects.commit(id)? else {
            return Ok(false);
        };
        if self.reachable.contains(&commit) {
            return Ok(false);
        }
        if !self.leftover.is_empty() && self.limit != 0 {
            // Dig down to the root, as what we look for may be further down than what was needed to get here.
            self.limit = 0;
            let pending = std::mem::take(&mut self.leftover);
            for id in &pending {
                self.reachable.remove(id);
            }
            self.mark(pending, objects)?;
        }
        Ok(!self.reachable.contains(&commit))
    }
}

/// Decides for the entries of one reflog, with one policy.
struct Decider {
    policy: Policy,
    kind: Kind,
}

impl Decider {
    fn new(
        policy: Policy,
        is_head: bool,
        current: Option<&gix_hash::oid>,
        tips: &mut dyn FnMut() -> Result<Vec<ObjectId>>,
        objects: &mut Objects<'_>,
    ) -> Result<Self> {
        let unreachable_tips = if policy.unreachable == 0 || is_head {
            Some(tips()?)
        } else {
            current
                .map(|id| objects.commit(id))
                .transpose()?
                .flatten()
                .map(|commit| vec![commit])
        };
        let kind = match unreachable_tips {
            Some(tips) if policy.unreachable > policy.total => {
                Kind::Reachable(Reach::new(tips, policy.total, objects)?)
            }
            _ => Kind::Always,
        };
        Ok(Decider { policy, kind })
    }

    fn should_expire(&mut self, line: &Line, objects: &mut Objects<'_>) -> Result<bool> {
        let timestamp = line.signature.time.seconds;
        if timestamp < self.policy.total {
            return Ok(true);
        }
        if timestamp < self.policy.unreachable {
            match &mut self.kind {
                Kind::Always => return Ok(true),
                Kind::Reachable(reach) => {
                    if reach.is_unreachable(&line.previous_oid, objects)?
                        || reach.is_unreachable(&line.new_oid, objects)?
                    {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

impl Repository {
    /// Expire the entries of all reflogs, of all worktrees, like `git reflog expire --all` does, with the ages from the
    /// `gc.*` configuration. See the [module documentation](crate::maintenance::reflog) for what is different.
    pub fn expire_reflogs(&self, options: Options) -> Result<Outcome> {
        self.expire_matching_reflogs(options, |_| true)
    }

    /// Return `true` if at least `limit` entries of the reflog of `HEAD` would expire, which is what
    /// `maintenance.reflog-expire.auto` asks of the `reflog-expire` task. `0` is never and a negative `limit` always.
    pub(super) fn reflog_expiry_is_due(&self, limit: i64) -> Result<bool> {
        if limit <= 0 {
            return Ok(limit < 0);
        }
        let options = Options {
            dry_run: true,
            ..Default::default()
        };
        let outcome = self.expire_matching_reflogs(options, |name| name == "HEAD")?;
        Ok(i64::try_from(outcome.removed).unwrap_or(i64::MAX) >= limit)
    }

    fn expire_matching_reflogs(&self, options: Options, wanted: impl Fn(&BStr) -> bool) -> Result<Outcome> {
        let now_time = options.now.unwrap_or_else(gix_date::Time::now_local_or_utc);
        let now = now_time
            .to_zoned()
            .or_raise(|| message("Could not turn the current time into a date"))?;
        let configuration = Configuration::read(self, &now)?;
        let seconds = now_time.seconds;
        // What the documentation says, and what the code of Git does, see the module documentation.
        let policies = [
            Policy {
                total: seconds - 90 * DAY,
                unreachable: seconds - 30 * DAY,
            },
            Policy {
                total: seconds - 30 * DAY,
                unreachable: seconds - 90 * DAY,
            },
        ];

        // A namespace limits what a handle sees, but the reflogs of all references are expired.
        let mut main = self.main_repo()?;
        main.clear_namespace();
        let store = main.ref_store();
        let expire_options = {
            let (lock, _packed) = main.config.lock_timeout()?;
            let mut options_for_store = gix_ref::log::expire::Options::default();
            options_for_store.dry_run = options.dry_run;
            options_for_store.lock = lock;
            options_for_store
        };

        let mut outcome = Outcome::default();
        let mut cached_tips: Option<Vec<ObjectId>> = None;
        for name in store.reflog_names()? {
            let ref_name: &BStr = name.as_bstr();
            if !wanted(ref_name) {
                continue;
            }
            let is_head = ref_name == "HEAD" || ref_name.ends_with(b"/HEAD") && ref_name.starts_with(b"worktrees/");
            let name_policies = policies.map(|defaults| configuration.policy(ref_name, defaults));
            let mut objects = Objects {
                repo: &main,
                buf: Vec::new(),
            };
            let mut deciders: Option<Vec<Decider>> = None;
            let mut failure: Option<gix_error::Error> = None;
            let mut tips = || -> Result<Vec<ObjectId>> {
                if let Some(tips) = &cached_tips {
                    return Ok(tips.clone());
                }
                let found = ref_tips(&main)?;
                cached_tips = Some(found.clone());
                Ok(found)
            };
            let result = store.reflog_expire(name.as_ref(), expire_options, |line: &Line, current| {
                if failure.is_some() {
                    return true;
                }
                let step = (|| -> Result<bool> {
                    if deciders.is_none() {
                        // With ages that are configured, both policies are the same and one walk does.
                        let mut distinct = vec![name_policies[0]];
                        if name_policies[1] != name_policies[0] {
                            distinct.push(name_policies[1]);
                        }
                        deciders = Some(
                            distinct
                                .into_iter()
                                .map(|policy| Decider::new(policy, is_head, current, &mut tips, &mut objects))
                                .collect::<Result<_>>()?,
                        );
                    }
                    // Only what all of them would expire goes.
                    for decider in deciders.as_mut().expect("initialized above") {
                        if !decider.should_expire(line, &mut objects)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })();
                match step {
                    Ok(expire) => !expire,
                    Err(err) => {
                        failure = Some(err);
                        true
                    }
                }
            });
            match (failure, result) {
                (Some(err), _) | (None, Err(err)) => outcome.failed.push((name, err)),
                (None, Ok(done)) => {
                    outcome.reflogs += 1;
                    outcome.removed += done.removed;
                    outcome.kept += done.kept;
                }
            }
        }
        Ok(outcome)
    }
}

/// The commits that the references of the main repository point to, directly or through tags.
fn ref_tips(repo: &Repository) -> Result<Vec<ObjectId>> {
    let mut objects = Objects { repo, buf: Vec::new() };
    let store = repo.ref_store();
    let mut tips = Vec::new();
    for reference in store.iter()?.all()? {
        let reference = reference?;
        if let Target::Object(id) = &reference.target
            && let Some(commit) = objects.commit(id)?
        {
            tips.push(commit);
        }
    }
    Ok(tips)
}
