use gix_hook::{Hooks, Kind, Settings};

use crate::Result;

fn settings(config: &str, filter: &mut dyn FnMut(&gix_config::file::Metadata) -> bool) -> gix_error::Result<Settings> {
    let config = gix_config::File::try_from(config).expect("valid config");
    Settings::from_config(&config, filter)
}

fn names(settings: &Settings, event: &str) -> Vec<String> {
    Hooks::resolve(event, "/nonexistent".as_ref(), Some(settings))
        .hooks()
        .iter()
        .map(|hook| hook.name.clone())
        .collect()
}

#[test]
fn events_keep_last_seen_order_and_empty_event_resets_a_hook() -> Result {
    let settings = settings(
        "
        [hook \"a\"]
            event = pre-push
            command = a
        [hook \"b\"]
            event = pre-push
            command = b
        [hook \"a\"]
            event = pre-push
        [hook \"b\"]
            event =
        ",
        &mut |_| true,
    )?;
    assert_eq!(
        names(&settings, "pre-push"),
        ["a"],
        "b was reset, and a moved to its last mention"
    );
    Ok(())
}

#[test]
fn configured_hooks_ignore_untrusted_sections() -> Result {
    let config = "[hook \"a\"]\n event = pre-push\n command = a\n";
    assert!(names(&settings(config, &mut |_| false)?, "pre-push").is_empty());
    Ok(())
}

#[test]
fn a_configured_hook_without_command_is_an_error_unless_disabled() {
    let config = "[hook \"a\"]\n event = pre-push\n";
    assert!(settings(config, &mut |_| true).is_err(), "Git dies");
    let disabled = format!("{config}[hook \"a\"]\n enabled = false\n");
    assert!(settings(&disabled, &mut |_| true).is_ok());
}

#[test]
fn a_friendly_name_must_not_collide_with_an_event() {
    let config = "[hook \"pre-push\"]\n event = pre-commit\n command = x\n";
    assert!(settings(config, &mut |_| true).is_err());
}

#[test]
fn jobs_are_capped_to_one_unless_all_configured_hooks_are_parallel() -> Result {
    let jobs = |config: &str| -> Result<usize> {
        let settings = settings(config, &mut |_| true)?;
        let hooks = Hooks::resolve("pre-push", "/nonexistent".as_ref(), Some(&settings));
        assert!(
            hooks
                .hooks()
                .iter()
                .all(|hook| matches!(hook.kind, Kind::Configured { .. }))
        );
        Ok(hooks.jobs())
    };
    let base = "[hook]\n jobs = 4\n[hook \"a\"]\n event = pre-push\n command = a\n";
    assert_eq!(jobs(base)?, 1, "a hook that isn't parallel forces serial execution");
    assert_eq!(jobs(&format!("{base}[hook \"a\"]\n parallel = true\n"))?, 4);
    assert_eq!(
        jobs(&format!(
            "{base}[hook \"a\"]\n parallel = true\n[hook \"pre-push\"]\n jobs = 2\n"
        ))?,
        2,
        "the event's setting wins over the global one"
    );
    Ok(())
}
