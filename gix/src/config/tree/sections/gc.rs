use crate::{
    config,
    config::tree::{Gc, Key, Section, keys, traits::SubSectionRequirement},
};

const PATTERN: Option<SubSectionRequirement> = Some(SubSectionRequirement::Parameter("pattern"));

impl Gc {
    /// The `gc.auto` key, which is the approximate number of loose objects that makes automatic maintenance repack and
    /// prune. `0` or less disables that. The default is 6700.
    pub const AUTO: keys::Integer = keys::Integer::new_integer("auto", &config::Tree::GC)
        .with_note("this decides about the `gc` task and `maintenance.auto` falls back to it when it is unset");
    /// The `gc.autoPackLimit` key, which is how many packs without a `.keep` file make automatic maintenance
    /// pack everything into one. `0` or less disables that. The default is 50.
    pub const AUTO_PACK_LIMIT: keys::Integer = keys::Integer::new_integer("autoPackLimit", &config::Tree::GC);
    /// The `gc.autoDetach` key, which tells automatic maintenance to run in the background. The default is `true`.
    pub const AUTO_DETACH: keys::Boolean = keys::Boolean::new_boolean("autoDetach", &config::Tree::GC);
    /// The `gc.bigPackThreshold` key, which is the size above which a pack isn't repacked, `0` for no limit.
    pub const BIG_PACK_THRESHOLD: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("bigPackThreshold", &config::Tree::GC);
    /// The `gc.packRefs` key, which is `true`, `false` or `notbare` to only pack references of repositories with a
    /// working tree. The default is `true`.
    pub const PACK_REFS: keys::String = keys::String::new_string("packRefs", &config::Tree::GC);
    /// The `gc.cruftPacks` key, which stores unreachable objects in a cruft pack instead of loosening them. The default is `true`.
    pub const CRUFT_PACKS: keys::Boolean = keys::Boolean::new_boolean("cruftPacks", &config::Tree::GC);
    /// The `gc.maxCruftSize` key, which is the maximum size of a cruft pack, `0` for no limit.
    pub const MAX_CRUFT_SIZE: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("maxCruftSize", &config::Tree::GC);
    /// The `gc.pruneExpire` key, which is the age that unreachable objects need to be pruned, or `now` or `never`.
    /// The default is `2.weeks.ago`.
    pub const PRUNE_EXPIRE: keys::String = keys::String::new_string("pruneExpire", &config::Tree::GC);
    /// The `gc.worktreePruneExpire` key, which is the age that the administrative data of a missing linked worktree
    /// needs to be pruned. The default is `3.months.ago`.
    pub const WORKTREE_PRUNE_EXPIRE: keys::String = keys::String::new_string("worktreePruneExpire", &config::Tree::GC);
    /// The `gc.logExpiry` key, which is the age of a `gc.log` file after which it stops preventing automatic
    /// maintenance. The default is `1.day.ago`.
    pub const LOG_EXPIRY: keys::String = keys::String::new_string("logExpiry", &config::Tree::GC);
    /// The `gc.reflogExpire` key, which is the age that reflog entries need to expire, or `never`.
    /// The default is `90.days.ago`.
    pub const REFLOG_EXPIRE: keys::String = keys::String::new_string("reflogExpire", &config::Tree::GC).with_note(
        "Git 2.56's code defaults to 30 days for this and 90 for `gc.reflogExpireUnreachable` while its documentation says the reverse",
    );
    /// The `gc.reflogExpireUnreachable` key, which is the age that reflog entries of unreachable commits need to expire,
    /// or `never`. The default is `30.days.ago`.
    pub const REFLOG_EXPIRE_UNREACHABLE: keys::String =
        keys::String::new_string("reflogExpireUnreachable", &config::Tree::GC);
    /// The `gc.<pattern>.reflogExpire` key, which is `gc.reflogExpire` for the references that match `pattern`.
    pub const PATTERN_REFLOG_EXPIRE: keys::String =
        keys::String::new_string("reflogExpire", &config::Tree::GC).with_subsection_requirement(PATTERN);
    /// The `gc.<pattern>.reflogExpireUnreachable` key, which is `gc.reflogExpireUnreachable` for the references that match `pattern`.
    pub const PATTERN_REFLOG_EXPIRE_UNREACHABLE: keys::String =
        keys::String::new_string("reflogExpireUnreachable", &config::Tree::GC).with_subsection_requirement(PATTERN);
    /// The `gc.recentObjectsHook` key, which is a program that prints object ids to keep even though they would be pruned.
    pub const RECENT_OBJECTS_HOOK: keys::Program = keys::Program::new_program("recentObjectsHook", &config::Tree::GC);
}

impl Section for Gc {
    fn name(&self) -> &str {
        "gc"
    }

    fn keys(&self) -> &[&dyn Key] {
        &[
            &Self::AUTO,
            &Self::AUTO_PACK_LIMIT,
            &Self::AUTO_DETACH,
            &Self::BIG_PACK_THRESHOLD,
            &Self::PACK_REFS,
            &Self::CRUFT_PACKS,
            &Self::MAX_CRUFT_SIZE,
            &Self::PRUNE_EXPIRE,
            &Self::WORKTREE_PRUNE_EXPIRE,
            &Self::LOG_EXPIRY,
            &Self::REFLOG_EXPIRE,
            &Self::REFLOG_EXPIRE_UNREACHABLE,
            &Self::PATTERN_REFLOG_EXPIRE,
            &Self::PATTERN_REFLOG_EXPIRE_UNREACHABLE,
            &Self::RECENT_OBJECTS_HOOK,
        ]
    }
}
