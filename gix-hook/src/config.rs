use std::collections::{HashMap, HashSet};

use gix_config::{Boolean, Integer};
use gix_error::{Result, bail, message};

/// Names of the events `git` knows, as listed in the `githooks` manual page that Git's `hook-list.h` is generated from, which can't be used as friendly-names of configured hooks.
const KNOWN_EVENTS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
];

/// The `hook.*` configuration, which defines hooks and how to run them.
///
/// Read it with [`Settings::from_config()`] and pass it to [`Hooks::resolve()`](crate::Hooks::resolve()).
#[derive(Debug, Default, Clone)]
pub struct Settings {
    commands: HashMap<String, String>,
    /// Friendly-names per event, in the order `git` runs them.
    events: HashMap<String, Vec<String>>,
    disabled: HashSet<String>,
    parallel: HashSet<String>,
    jobs: Option<usize>,
    event_jobs: HashMap<String, usize>,
}

fn parse_jobs(value: bstr::BString) -> Option<usize> {
    let value = Integer::try_from(value).ok()?.to_decimal()?;
    match value {
        -1 => Some(std::thread::available_parallelism().map_or(1, usize::from)),
        1.. => usize::try_from(value).ok(),
        _ => None,
    }
}

fn parse_bool(value: bstr::BString) -> Option<bool> {
    Boolean::try_from(value).ok().map(|b| b.0)
}

impl Settings {
    /// Collect all `hook.*` configuration from the sections of `config` that `filter` accepts, which should be only those
    /// from trusted sources as hooks are programs. It behaves like `git` does, and fail like it does if a configured hook
    /// has an event but no command, or if its name collides with an event name.
    pub fn from_config(
        config: &gix_config::File,
        filter: &mut dyn FnMut(&gix_config::file::Metadata) -> bool,
    ) -> Result<Self> {
        let mut out = Settings::default();
        for section in config.sections_by_name("hook").into_iter().flatten() {
            if !filter(section.meta()) {
                continue;
            }
            let body = section.body();
            let Some(name) = section.header().subsection_name() else {
                for value in body.values("jobs") {
                    if let Some(jobs) = parse_jobs(value) {
                        out.jobs = Some(jobs);
                    }
                }
                continue;
            };
            let name = name.to_string();
            for value in body.values("event") {
                if value.is_empty() {
                    out.events.values_mut().for_each(|names| names.retain(|n| *n != name));
                    continue;
                }
                if KNOWN_EVENTS.contains(&name.as_str()) {
                    bail!(message!(
                        "hook friendly-name '{name}' collides with a known event name; please choose a different friendly-name"
                    ));
                }
                let names = out.events.entry(value.to_string()).or_default();
                names.retain(|n| *n != name);
                names.push(name.clone());
            }
            if let Some(value) = body.values("command").pop() {
                out.commands.insert(name.clone(), value.to_string());
            }
            for value in body.values("enabled") {
                match parse_bool(value) {
                    Some(false) => {
                        out.disabled.insert(name.clone());
                    }
                    Some(true) => {
                        out.disabled.remove(&name);
                    }
                    None => {}
                }
            }
            for value in body.values("parallel") {
                match parse_bool(value) {
                    Some(true) => {
                        out.parallel.insert(name.clone());
                    }
                    Some(false) => {
                        out.parallel.remove(&name);
                    }
                    None => {}
                }
            }
            for value in body.values("jobs") {
                if let Some(jobs) = parse_jobs(value) {
                    out.event_jobs.insert(name.clone(), jobs);
                }
            }
        }
        let mut events: Vec<_> = out.events.iter().collect();
        events.sort_unstable();
        for (event, names) in events {
            for name in names {
                if !out.commands.contains_key(name) && !out.disabled.contains(name) {
                    bail!(message!(
                        "'hook.{name}.command' must be configured or 'hook.{name}.event' must be removed (event '{event}')"
                    ));
                }
            }
        }
        Ok(out)
    }

    fn is_friendly_name(&self, name: &str) -> bool {
        self.commands.contains_key(name)
            || self.parallel.contains(name)
            || self.events.values().any(|names| names.iter().any(|n| n == name))
    }

    /// All configured hooks of `event` as `(friendly-name, command, parallel, enabled)` in the order they run, including
    /// disabled ones as these still count when limiting jobs. Hooks of a disabled event are all disabled.
    pub(crate) fn configured(&self, event: &str) -> Vec<(&str, Option<&str>, bool, bool)> {
        let event_disabled = self.disabled.contains(event) && !self.is_friendly_name(event);
        self.events
            .get(event)
            .into_iter()
            .flatten()
            .map(|name| {
                (
                    name.as_str(),
                    self.commands.get(name).map(String::as_str),
                    self.parallel.contains(name),
                    !event_disabled && !self.disabled.contains(name),
                )
            })
            .collect()
    }

    /// The number of jobs configured for `event`, or `None` to use the default of one.
    pub(crate) fn jobs(&self, event: &str) -> Option<usize> {
        self.event_jobs.get(event).copied().or(self.jobs)
    }
}
