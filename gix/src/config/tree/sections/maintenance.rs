use crate::{
    config,
    config::tree::{Key, Maintenance, Section, keys, traits::SubSectionRequirement},
};

const TASK: Option<SubSectionRequirement> = Some(SubSectionRequirement::Parameter("task"));

impl Maintenance {
    /// The `maintenance.auto` key, which allows automatic maintenance to run after operations that may have made it due.
    /// If it is unset, `gc.auto` decides instead. The default is `true`.
    pub const AUTO: keys::Boolean = keys::Boolean::new_boolean("auto", &config::Tree::MAINTENANCE);
    /// The `maintenance.autoDetach` key, which tells automatic maintenance to run in the background, with `gc.autoDetach`
    /// as fallback.
    pub const AUTO_DETACH: keys::Boolean = keys::Boolean::new_boolean("autoDetach", &config::Tree::MAINTENANCE);
    /// The `maintenance.strategy` key, which is `incremental`, `gc` or `geometric` (Git refuses any other value), and decides which tasks are enabled
    /// unless `maintenance.<task>.enabled` says otherwise. The default is `geometric`.
    pub const STRATEGY: keys::String = keys::String::new_string("strategy", &config::Tree::MAINTENANCE);
    /// The `maintenance.<task>.enabled` key, which turns a task on or off regardless of the strategy.
    pub const TASK_ENABLED: keys::Boolean =
        keys::Boolean::new_boolean("enabled", &config::Tree::MAINTENANCE).with_subsection_requirement(TASK);
    /// The `maintenance.<task>.auto` key, which is the amount of work, as counted by the task, that makes it due during
    /// automatic maintenance. The defaults are the ones of Git, like 100 for `loose-objects` and 6700 for
    /// `geometric-repack`, where `0` disables the task.
    pub const TASK_AUTO: keys::Integer =
        keys::Integer::new_integer("auto", &config::Tree::MAINTENANCE).with_subsection_requirement(TASK);
    /// The `maintenance.loose-objects.batchSize` key, which is how many loose objects the `loose-objects` task packs
    /// at once, with `0` for all of them. The default is 50000.
    pub const LOOSE_OBJECTS_BATCH_SIZE: keys::UnsignedInteger =
        keys::UnsignedInteger::new_unsigned_integer("batchSize", &config::Tree::MAINTENANCE)
            .with_subsection_requirement(TASK);
    /// The `maintenance.geometric-repack.splitFactor` key, which is the factor by which each pack has to be bigger than the one
    /// before it to be left alone. The default is 2.
    pub const GEOMETRIC_REPACK_SPLIT_FACTOR: keys::Integer =
        keys::Integer::new_integer("splitFactor", &config::Tree::MAINTENANCE).with_subsection_requirement(TASK);
}

impl Section for Maintenance {
    fn name(&self) -> &str {
        "maintenance"
    }

    fn keys(&self) -> &[&dyn Key] {
        &[
            &Self::AUTO,
            &Self::AUTO_DETACH,
            &Self::STRATEGY,
            &Self::TASK_ENABLED,
            &Self::TASK_AUTO,
            &Self::LOOSE_OBJECTS_BATCH_SIZE,
            &Self::GEOMETRIC_REPACK_SPLIT_FACTOR,
        ]
    }
}
