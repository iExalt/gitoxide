use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use gix_error::{OptionExt, Result, bail, message};
use serde_json::{Value, json};

use super::{
    requirements::Requirements,
    run::{self, Command},
};

pub const PLATFORMS: &[&str] = &[
    "linux-x86_64",
    "linux-aarch64",
    "linux-x86",
    "linux-arm",
    "macos-aarch64",
    "macos-x86_64",
    "windows-x86_64",
    "windows-aarch64",
];
const CI_PLATFORMS: &[&str] = &[
    "linux-x86_64",
    "linux-aarch64",
    "linux-x86",
    "linux-arm",
    "macos-aarch64",
    "windows-x86_64",
    "windows-aarch64",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: &'static str,
    pub group: &'static str,
    pub stages: &'static [&'static str],
    pub blocking: bool,
    pub commands: Vec<Command>,
    pub requirements: Requirements,
}

impl Task {
    fn new(id: &'static str, group: &'static str, stages: &'static [&'static str], commands: Vec<Command>) -> Self {
        Self {
            id,
            group,
            stages,
            blocking: true,
            requirements: Requirements::commands(&commands),
            commands,
        }
    }

    fn optional(mut self) -> Self {
        self.blocking = false;
        self
    }

    fn json(&self, metadata: &Value) -> Result<Value> {
        Ok(json!({"id": self.id, "group": self.group, "blocking": self.blocking,
            "commands": self.commands.iter().map(|command| json!({
                "program": command.program, "args": command.args, "env": command.env, "requires": command.requires
            })).collect::<Vec<_>>(), "requirements": self.requirements.json(metadata)?}))
    }
}

pub fn host() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn parts(platform: &str) -> Result<(&str, &str)> {
    if !PLATFORMS.contains(&platform) {
        bail!(
            "Unsupported QA platform {platform}; supported platforms: {}",
            PLATFORMS.join(", ")
        );
    }
    platform
        .rsplit_once('-')
        .ok_or_raise(|| message("platform must include OS and architecture"))
}

pub fn tasks(metadata: &Value, selection: &Value, platform: &str) -> Result<Vec<Task>> {
    let (os, arch) = parts(platform)?;
    // Validate even empty selections before adding mandatory policy checks.
    run::commands(metadata, selection, &[])?;
    let exhaustive = selection["mode"] == "exhaustive";
    let selected: Vec<&str> = if exhaustive {
        metadata["packages"]
            .as_array()
            .ok_or_raise(|| message("missing packages"))?
            .iter()
            .filter_map(|package| package["name"].as_str())
            .collect()
    } else {
        selection["packages"]
            .as_array()
            .ok_or_raise(|| message("missing selection"))?
            .iter()
            .filter_map(Value::as_str)
            .collect()
    };
    let has_work = !selected.is_empty();
    let core = |stage: &str| run::commands(metadata, selection, &[stage.into()]);
    let mut out = vec![
        Task::new(
            "self-test",
            "self-test",
            &["self-test"],
            vec![Command::new(
                "cargo",
                &[
                    "test",
                    "--locked",
                    "-p",
                    env!("CARGO_PKG_NAME"),
                    "--no-default-features",
                    "--bin",
                    env!("CARGO_BIN_NAME"),
                ],
            )],
        ),
        Task::new(
            "policy",
            "policy",
            &["policy", "audit"],
            vec![Command::new(
                "cargo",
                &[
                    "deny",
                    "--workspace",
                    "--all-features",
                    "--locked",
                    "check",
                    "bans",
                    "licenses",
                    "sources",
                ],
            )],
        ),
        Task::new(
            "advisories",
            "advisories",
            &["advisories", "audit"],
            vec![Command::new(
                "cargo",
                &[
                    "deny",
                    "--workspace",
                    "--all-features",
                    "--locked",
                    "check",
                    "advisories",
                ],
            )],
        )
        .optional(),
        Task::new(
            "housekeeping",
            "housekeeping",
            &["housekeeping"],
            vec![
                Command::new("@qa", &["workflows"]),
                Command::new("@qa", &["badge"]),
                Command::new("@qa", &["mode"]),
            ],
        ),
    ];
    if !has_work {
        for task in &out {
            task.json(metadata)?;
        }
        return Ok(out);
    }
    let packages = metadata["packages"]
        .as_array()
        .ok_or_raise(|| message("missing packages"))?;
    let selected_packages: Vec<_> = packages
        .iter()
        .filter(|p| selected.contains(&p["name"].as_str().expect("validated package name")))
        .collect();

    // Clippy performs the default compilation check too; do not schedule cargo check again.
    out.push(Task::new("lint", "lint", &["lint", "check"], core("lint")?));
    let checks = core("feature-checks")?;
    if !checks.is_empty() {
        out.push(Task::new("feature-builds", "features", &["check"], checks));
    }
    if exhaustive {
        out.push(Task::new(
            "feature-contracts",
            // A single script of about 3.5 CI minutes, so it gets its own lane instead of
            // making one `features` shard the slowest.
            "feature-contracts",
            &["check"],
            vec![Command::new("bash", &["tests/it/qa/check-features.sh"])],
        ));
    }

    if os == "windows" {
        // Nextest compiles examples but can omit binaries declared test = false.
        let binaries: Vec<_> = selected_packages
            .iter()
            .filter(|p| run::has_target(p, "bins"))
            .collect();
        if !binaries.is_empty() {
            let mut command = Command::new("cargo", &["check", "--locked", "--bins"]);
            if exhaustive {
                // Keep workspace feature unification, as for exhaustive nextest runs.
                command.args.push("--workspace".into());
            } else {
                for package in binaries {
                    command
                        .args
                        .extend(["-p".into(), package["name"].as_str().expect("validated name").into()]);
                }
            }
            out.push(Task::new(
                "native-bins",
                "tests",
                &["check", "tests", "default-tests"],
                vec![command],
            ));
        }
    }

    let isolated: Vec<_> = packages
        .iter()
        .filter_map(|package| match run::flag(package, "isolated") {
            Ok(true) => Some(Ok(package["name"].as_str().expect("validated name"))),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<_>>()?;
    let tested: Vec<_> = selected_packages
        .iter()
        .filter(|p| run::has_target(p, "test") && !isolated.contains(&p["name"].as_str().expect("validated name")))
        .map(|p| p["name"].as_str().expect("validated name"))
        .collect();
    if !tested.is_empty() {
        // The developer loop (`mise run test`): the exhaustive native build's exact package set,
        // so both share artifacts, with tests filtered to the selection and no source snapshot.
        let mut command = Command::new(
            "cargo",
            &["nextest", "run", "--locked", "--workspace", "--no-fail-fast"],
        )
        .requiring(&["cargo-nextest"]);
        for name in &isolated {
            command.args.extend(["--exclude".into(), (*name).into()]);
        }
        if !exhaustive {
            let filter = tested
                .iter()
                .map(|name| format!("package(={name})"))
                .collect::<Vec<_>>();
            command.args.extend(["-E".into(), filter.join(" | ")]);
        }
        out.push(Task::new("local-tests", "local-tests", LOCAL_ONLY, vec![command]));
    }
    if !tested.is_empty() {
        let mut command = Command::new("@qa", &["native-tests", if exhaustive { "exhaustive" } else { "fast" }])
            .requiring(&["cargo-nextest"]);
        if exhaustive {
            command.args.push("--workspace".into());
            for name in isolated {
                command.args.extend(["--exclude".into(), name.into()]);
            }
        } else {
            for name in tested {
                command.args.extend(["-p".into(), name.into()]);
            }
        }
        out.push(Task::new(
            "native-tests",
            "tests",
            &["tests", "default-tests"],
            vec![command],
        ));
    }
    let isolated_tests = core("isolated-tests")?;
    if !isolated_tests.is_empty() {
        out.push(Task::new(
            "isolated-tests",
            "tests",
            &["tests", "default-tests", "isolated"],
            isolated_tests,
        ));
    }
    let variants = core("variants")?;
    if !variants.is_empty() {
        out.push(Task::new("feature-tests", "features", &["tests"], variants));
    }
    for (id, stage) in [
        ("default-doctests", "default-doctests"),
        ("feature-doctests", "doctest-variants"),
    ] {
        let commands = core(stage)?;
        if !commands.is_empty() {
            out.push(Task::new(id, "docs", &["doctests"], commands));
        }
    }
    let docs = core("docs")?;
    if !docs.is_empty() {
        out.push(Task::new("docs", "docs", &["docs"], docs));
    }
    if os != "windows" {
        let journey = core("journey")?;
        if !journey.is_empty() {
            out.push(Task::new("journey", "journey", &["journey"], journey));
        }
    }
    let mut api = core("api")?;
    if !api.is_empty() {
        let checker = packages
            .iter()
            .find(|p| p["metadata"]["qa"]["api-checker"] == true)
            .ok_or_raise(|| message("Declare a QA API checker"))?;
        let toolchain = checker["metadata"]["qa"]["api-toolchain"]
            .as_str()
            .ok_or_raise(|| message("QA API checker needs api-toolchain"))?;
        for command in &mut api {
            command.env.insert("RUSTUP_TOOLCHAIN".into(), toolchain.into());
        }
        out.push(Task::new("api", "api", &["api"], api));
    }
    if exhaustive {
        let toolchains = super::ops::msrv_toolchains(metadata)?;
        if !toolchains.is_empty() {
            let command = Command::new("@qa", &["msrv"]).requiring(&["toolchain:nightly"]);
            let mut task = Task::new("msrv", "msrv", &["msrv"], vec![command]);
            task.requirements
                .merge(Requirements::commands(&super::ops::msrv_commands(metadata)?));
            out.push(task);
        }
        out.push(Task::new("auditable", "auditable", &["auditable"], core("auditable")?));
        out.push(
            Task::new(
                "size",
                "size",
                &["size"],
                vec![Command::new("bash", &["etc/scripts/check-package-size.sh"]).requiring(&["cargo-diet"])],
            )
            .optional(),
        );
        if os == "linux" && arch == "x86_64" {
            out.push(Task::new(
                "pure-rust",
                "pure-rust",
                &["pure-rust"],
                vec![Command::new("@qa", &["pure-rust"]).requiring(&["docker"])],
            ));
        }
        if os == "windows" {
            let without_git: Vec<_> = selected_packages
                .iter()
                .filter(|p| p["metadata"]["qa"]["without-git"] == true)
                .collect();
            if !without_git.is_empty() {
                let mut command = Command::new("@qa", &["no-git", "--no-fail-fast"]).requiring(&["cargo-nextest"]);
                for package in without_git {
                    command
                        .args
                        .extend(["-p".into(), package["name"].as_str().expect("validated name").into()]);
                }
                if arch == "aarch64" {
                    command
                        .args
                        .extend(["--", "--skip", "performance", "--skip", "speed"].map(String::from));
                }
                out.push(Task::new(
                    "without-git",
                    "tests",
                    &["tests", "default-tests"],
                    vec![command],
                ));
            }
            if arch == "x86_64" {
                out.push(Task::new(
                    "size32",
                    "size32",
                    &["tests", "default-tests"],
                    vec![Command::new(
                        "cargo",
                        &[
                            "nextest",
                            "run",
                            "--locked",
                            "--target",
                            "i686-pc-windows-msvc",
                            "--workspace",
                            "--no-fail-fast",
                            "size",
                        ],
                    )],
                ));
            }
        }
    }
    let commands = super::ops::wasm_commands(metadata, &selected)?;
    if !commands.is_empty() {
        out.push(Task::new("wasm", "wasm", &["wasm"], commands).optional());
    }
    for task in &out {
        task.json(metadata)?;
    }

    Ok(out)
}

/// Stages of tasks that `all` and CI plans never select.
const LOCAL_ONLY: &[&str] = &["local-tests"];

pub const STAGES: [&str; 21] = [
    "all",
    "self-test",
    "policy",
    "advisories",
    "audit",
    "housekeeping",
    "lint",
    "check",
    "tests",
    "default-tests",
    "docs",
    "doctests",
    "journey",
    "api",
    "msrv",
    "auditable",
    "size",
    "pure-rust",
    "wasm",
    "local-tests",
    "isolated",
];

pub fn select(tasks: Vec<Task>, stages: &[String]) -> Result<Vec<Task>> {
    for stage in stages {
        if !STAGES.contains(&stage.as_str()) {
            bail!("Unknown QA stage {stage}");
        }
    }
    Ok(tasks
        .into_iter()
        .filter(|task| {
            stages.iter().any(|stage| {
                (stage == "all" && !task.stages.iter().any(|stage| LOCAL_ONLY.contains(stage)))
                    || task.stages.contains(&stage.as_str())
            })
        })
        .collect())
}

fn assigned(task: &Task, platform: &str, exhaustive: bool) -> bool {
    match task.id {
        // The gate runs this task before it publishes a matrix.
        "self-test" => false,
        // Only `mise run test` selects it.
        "local-tests" => false,
        "native-tests" | "native-bins" | "isolated-tests" | "without-git" | "size32" => {
            exhaustive || matches!(platform, "linux-x86_64" | "macos-aarch64" | "windows-x86_64")
        }
        "msrv" => matches!(platform, "linux-x86_64" | "windows-x86_64"),
        "default-doctests" => platform == "linux-x86_64" || (exhaustive && platform == "windows-x86_64"),
        _ => platform == "linux-x86_64",
    }
}

fn job(metadata: &Value, platform: &str, group: &str, tasks: &[Task]) -> Result<Value> {
    let (runner, container, host, architecture) = match platform {
        "linux-x86_64" => ("ubuntu-latest", "", "", ""),
        "linux-aarch64" => ("ubuntu-24.04-arm", "", "", ""),
        // Keep Ubuntu 24.04 until actions/runner-images#14790 is fixed.
        "linux-x86" => (
            "ubuntu-24.04",
            "i386/debian:stable-slim@sha256:c60c0e2d9f3cf661bcfdb673b7da1c550562efefc802cd9f90c153f8f4a0b491",
            "i686-unknown-linux-gnu",
            "amd64",
        ),
        "linux-arm" => (
            "ubuntu-24.04-arm",
            "arm32v7/debian:stable-slim@sha256:f388bccfe138b76e6f6b8b235bcd00ea9935c8310b40a34592541b4f7a176b39",
            "armv7-unknown-linux-gnueabihf",
            "arm64",
        ),
        "macos-aarch64" => ("macos-latest", "", "", ""),
        "macos-x86_64" => ("macos-15-intel", "", "", ""),
        "windows-x86_64" => ("windows-latest", "", "", ""),
        "windows-aarch64" => ("windows-11-arm", "", "", ""),
        _ => unreachable!("validated platform"),
    };
    let mut requirements = Requirements::default();
    for task in tasks {
        requirements.merge(task.requirements.clone());
    }
    let requirements = requirements.json(metadata)?;
    let mut row = json!({"id": format!("{group}@{platform}"), "platform": platform, "runner": runner, "container": container,
        "host": host, "runner_architecture": architecture, "cache": format!("ci-{group}-{platform}"),
        "blocking": tasks.iter().any(|task| task.blocking), "tasks": tasks.iter().map(|task| task.id).collect::<Vec<_>>()});
    row.as_object_mut()
        .expect("job row is an object")
        .extend(requirements.as_object().expect("requirements are an object").clone());
    Ok(row)
}

/// Tasks whose commands are independent cases, so CI shards may split them by command.
/// Keep this in sync with the per-command dispatch in `execute()`.
const SPLITTABLE: &[&str] = &[
    "feature-builds",
    "feature-tests",
    "isolated-tests",
    "default-doctests",
    "feature-doctests",
];

/// The number of independently assignable units `shard()` deals out.
fn units(tasks: &[Task]) -> usize {
    tasks
        .iter()
        .map(|task| {
            if SPLITTABLE.contains(&task.id) {
                task.commands.len()
            } else {
                1
            }
        })
        .sum()
}

/// Keep shard `index` (1-based) of `count`: splittable tasks contribute each command as a unit,
/// other tasks one unit each, and units are dealt round-robin so every unit runs exactly once.
pub fn shard(tasks: Vec<Task>, index: usize, count: usize) -> Result<Vec<Task>> {
    if count == 0 || index == 0 || index > count {
        bail!(message!("Invalid QA shard {index}/{count}"));
    }
    let mut unit = 0;
    let mut owned = || {
        let mine = unit % count == index - 1;
        unit += 1;
        mine
    };
    let mut out = Vec::new();
    for mut task in tasks {
        if SPLITTABLE.contains(&task.id) {
            task.commands.retain(|_| owned());
            if !task.commands.is_empty() {
                out.push(task);
            }
        } else if owned() {
            out.push(task);
        }
    }
    Ok(out)
}

/// Parse `GROUP=COUNT` CI shard requests.
pub fn shard_counts(requests: &[String]) -> Result<BTreeMap<String, usize>> {
    let mut counts = BTreeMap::new();
    for request in requests {
        let (group, count) = request
            .split_once('=')
            .ok_or_raise(|| message!("Shard request {request} must look like GROUP=COUNT"))?;
        let Ok(count @ 1..) = count.parse::<usize>() else {
            bail!(message!("Shard request {request} needs a positive count"));
        };
        if counts.insert(group.to_owned(), count).is_some() {
            bail!(message!("Shard request {request} repeats group {group}"));
        }
    }
    Ok(counts)
}

pub fn plan(
    metadata: &Value,
    selection: &Value,
    platform: &str,
    all_platforms: bool,
    stages: &[String],
    shards: &BTreeMap<String, usize>,
) -> Result<Value> {
    let mut described = Vec::new();
    let mut jobs = Vec::new();
    let mut seen = BTreeSet::new();
    for platform in if all_platforms {
        CI_PLATFORMS
    } else {
        std::slice::from_ref(&platform)
    } {
        let tasks = select(tasks(metadata, selection, platform)?, stages)?;
        let mut grouped: BTreeMap<&str, Vec<Task>> = BTreeMap::new();
        for task in tasks {
            if !all_platforms || assigned(&task, platform, selection["mode"] == "exhaustive") {
                described.push(json!({"platform": platform, "task": task.json(metadata)?}));
                grouped.entry(task.group).or_default().push(task);
            } else if task.id == "self-test" && *platform == "linux-x86_64" {
                described.push(json!({"platform": platform, "task": task.json(metadata)?}));
            }
        }
        for (group, tasks) in grouped {
            seen.insert(group);
            let row = job(metadata, platform, group, &tasks)?;
            // Never plan more shards than the group has units, so small selections get no empty lanes.
            let count = shards.get(group).copied().unwrap_or(1).min(units(&tasks));
            if count <= 1 {
                jobs.push(row);
                continue;
            }
            for index in 1..=count {
                let mut row = row.clone();
                row["id"] = format!("{group}#{index}of{count}@{platform}").into();
                row["cache"] = format!("ci-{group}-{platform}-{index}of{count}").into();
                row["shard"] = json!([index, count]);
                jobs.push(row);
            }
        }
    }
    // Fast selections may legitimately omit a sharded group; exhaustive ones catch typos.
    if selection["mode"] == "exhaustive" {
        for group in shards.keys() {
            if !seen.contains(group.as_str()) {
                bail!(message!("Cannot shard {group}: no selected job has that group"));
            }
        }
    }
    Ok(json!({"selection": selection, "platform": platform, "tasks": described, "jobs": jobs}))
}

pub fn job_tasks(metadata: &Value, plan: &Value, job_id: &str) -> Result<Vec<Task>> {
    let row = plan["jobs"]
        .as_array()
        .ok_or_raise(|| message("plan is missing jobs"))?
        .iter()
        .find(|row| row["id"] == job_id)
        .ok_or_raise(|| message!("Job {job_id} is absent from the plan"))?;
    let platform = row["platform"]
        .as_str()
        .ok_or_raise(|| message("job has no platform"))?;
    if platform != host() {
        bail!("Job {job_id} requires {platform}; this runner is {}", host());
    }
    let ids: BTreeSet<&str> = row["tasks"]
        .as_array()
        .ok_or_raise(|| message("job is missing task IDs"))?
        .iter()
        .map(|value| value.as_str().ok_or_raise(|| message("task IDs must be strings")))
        .collect::<Result<_>>()?;
    let selected: Vec<_> = tasks(metadata, &plan["selection"], platform)?
        .into_iter()
        .filter(|task| ids.contains(task.id))
        .collect();
    if selected.len() != ids.len() {
        bail!("Job {job_id} contains stale task IDs; regenerate the plan");
    }
    match row["shard"].as_array().map(|pair| {
        (
            pair.first().and_then(Value::as_u64),
            pair.get(1).and_then(Value::as_u64),
        )
    }) {
        None => Ok(selected),
        Some((Some(index), Some(count))) => shard(selected, index as usize, count as usize),
        Some(_) => bail!("Job {job_id} has a malformed shard"),
    }
}

pub fn execute(root: &Path, tasks: Vec<Task>, dry_run: bool, ci_job: bool) -> Result<()> {
    let mut failed = Vec::new();
    for task in tasks {
        eprintln!(
            "\nQA task: {}{}",
            task.id,
            if task.blocking { "" } else { " (nonblocking)" }
        );
        // Every task in `SPLITTABLE` must treat its commands as independent cases here.
        let result = if matches!(task.id, "feature-tests" | "isolated-tests") {
            super::native_tests::variants(root, task.commands, dry_run)
        } else if task.id == "feature-builds" {
            run::each_case(task.commands, |command| run::execute(root, vec![command], dry_run))
        } else {
            run::execute(root, task.commands, dry_run)
        };
        if let Err(err) = result {
            eprintln!("{err:?}");
            // CI's continue-on-error preserves a visible failure for optional jobs.
            if task.blocking || ci_job {
                failed.push(task.id);
            } else {
                eprintln!("Nonblocking QA task failed: {}", task.id);
            }
        }
    }
    if !failed.is_empty() {
        bail!("Failed QA tasks: {}", failed.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> Value {
        json!({"packages": [
            {"name":"gix-path", "targets":[{"kind":["lib"], "test":true,"doc":true,"doctest":true}], "metadata":{"qa":{"without-git":true}}},
            {"name":"internal-tools", "targets":[{"kind":["bin"],"test":true}], "metadata":{"qa":{"api-checker":true,"api-toolchain":"1.99.0"}}},
            {"name":"gitoxide", "targets":[{"kind":["bin"],"test":false}], "metadata":{"qa":{"journey":true,"auditable":true}}}
        ],"metadata":{"qa":{"tool-versions":{
            "cargo-nextest":"0.9.122","cargo-deny":"0.20.0","cargo-machete":"0.9.1",
            "cargo-semver-checks":"0.51.0","cargo-auditable":"0.7.6","cargo-diet":"1.2.7","just":"1.58.0"
        }}}})
    }

    #[test]
    fn empty_fast_selection_keeps_required_policy() -> gix_error::TestResult {
        let selection = json!({"mode":"fast", "packages":[]});
        for platform in PLATFORMS {
            let tasks = tasks(&metadata(), &selection, platform)?;
            assert!(
                tasks.iter().any(|task| task.id == "policy" && task.blocking),
                "licenses cannot be selected away"
            );
            assert!(
                !tasks.iter().any(|task| task.id == "native-tests"),
                "empty selections do not run package tests"
            );
        }
        Ok(())
    }

    #[test]
    fn overlapping_stages_select_each_task_once() -> gix_error::TestResult {
        let selection = json!({"mode":"exhaustive", "packages":[]});
        let all = tasks(&metadata(), &selection, "windows-x86_64")?;
        let tests = select(all.clone(), &["tests".into()])?;
        assert_eq!(
            tests,
            select(all, &["tests".into(), "default-tests".into(), "tests".into()])?,
            "stage union cannot repeat native tests"
        );
        Ok(())
    }

    #[test]
    fn specialties_follow_manifest_roles_for_new_package_names() -> gix_error::TestResult {
        let mut metadata = metadata();
        metadata["packages"].as_array_mut().expect("test packages").push(json!({
            "name":"new-isolated-library", "targets":[{"kind":["lib"],"test":true,"doc":false,"doctest":false}],
            "metadata":{"qa":{"isolated":true}}
        }));
        let selection = json!({"mode":"fast", "packages":["new-isolated-library"]});
        let tasks = tasks(&metadata, &selection, "linux-x86_64")?;
        assert!(
            !tasks
                .iter()
                .any(|task| matches!(task.id, "native-tests" | "docs" | "default-doctests")),
            "target flags and isolation omit inapplicable baseline work"
        );
        let isolated = tasks
            .iter()
            .find(|task| task.id == "isolated-tests")
            .expect("isolated tests");
        assert!(
            isolated.commands[0]
                .args
                .ends_with(&["-p".into(), "new-isolated-library".into()]),
            "new crates need no central name registration"
        );
        Ok(())
    }

    #[test]
    fn msrv_jobs_preserve_nested_command_requirements() -> gix_error::TestResult {
        let mut metadata = metadata();
        metadata["packages"][0]["rust_version"] = json!("1.98");
        metadata["packages"][0]["metadata"]["qa"]["msrv"] = json!({"checks":[{
            "args":["--target", "wasm32-wasip1"], "requires":["rustfmt"]
        }]});
        let plan = plan(
            &metadata,
            &json!({"mode":"exhaustive","packages":[]}),
            "linux-x86_64",
            true,
            &["all".into()],
            &BTreeMap::new(),
        )?;
        let job = plan["jobs"]
            .as_array()
            .expect("jobs")
            .iter()
            .find(|job| job["id"] == "msrv@linux-x86_64")
            .expect("MSRV job");
        assert_eq!(job["rust_toolchains"], json!(["1.98.0", "nightly"]));
        assert_eq!(
            job["rust_targets"],
            json!([{"toolchain":"1.98.0", "target":"wasm32-wasip1"}]),
            "the snapshot wrapper preserves the inner command's target toolchain"
        );
        assert_eq!(
            job["rust_components"],
            json!([{"toolchain":"1.98.0", "component":"rustfmt"}])
        );
        Ok(())
    }

    #[test]
    fn windows_binary_checks_preserve_exhaustive_feature_unification() -> gix_error::TestResult {
        for mode in ["fast", "exhaustive"] {
            let selection = json!({"mode":mode,"packages":["gitoxide"]});
            let tasks = tasks(&metadata(), &selection, "windows-x86_64")?;
            let command = &tasks
                .iter()
                .find(|task| task.id == "native-bins")
                .expect("binary check")
                .commands[0];
            assert_eq!(
                command.args.contains(&"--workspace".into()),
                mode == "exhaustive",
                "exhaustive compilation retains Cargo's workspace feature unification"
            );
            assert_eq!(
                command.args.contains(&"-p".into()),
                mode == "fast",
                "fast compilation selects only packages with untested binaries"
            );
        }
        Ok(())
    }

    fn unit_list(tasks: &[Task]) -> Vec<(&'static str, Vec<String>)> {
        tasks
            .iter()
            .flat_map(|task| task.commands.iter().map(move |command| (task.id, command.args.clone())))
            .collect()
    }

    #[test]
    fn shards_run_every_case_exactly_once() -> gix_error::TestResult {
        let case = |n: usize| Command::new("cargo", &["check", &n.to_string()]);
        let tasks = vec![
            Task::new("feature-builds", "features", &["check"], (0..7).map(case).collect()),
            // Stands in for any unsplittable task in a sharded group.
            Task::new("whole-script", "features", &["check"], vec![case(100), case(101)]),
            Task::new("feature-tests", "features", &["tests"], (200..205).map(case).collect()),
        ];
        let all = unit_list(&tasks);
        for count in 1..=5 {
            let mut seen = Vec::new();
            for index in 1..=count {
                let part = shard(tasks.clone(), index, count)?;
                assert!(
                    part.iter()
                        .filter(|task| task.id == "whole-script")
                        .all(|task| task.commands.len() == 2),
                    "an unsplittable task keeps all of its commands in one shard"
                );
                seen.extend(unit_list(&part));
            }
            seen.sort();
            let mut expected = all.clone();
            expected.sort();
            assert_eq!(seen, expected, "{count} shards are disjoint and complete");
        }
        assert!(shard(tasks, 3, 2).is_err(), "out-of-range shards are rejected");
        Ok(())
    }

    #[test]
    fn plans_select_stages_and_expand_shards() -> gix_error::TestResult {
        let selection = json!({"mode":"exhaustive", "packages":["gix-path","gitoxide","internal-tools"]});
        // `docs` holds two units here: the default doctests and the docs build.
        let shards = shard_counts(&["docs=5".into()])?;
        let planned = plan(
            &metadata(),
            &selection,
            "linux-x86_64",
            false,
            &["docs".into(), "doctests".into()],
            &shards,
        )?;
        let jobs = planned["jobs"].as_array().expect("plan jobs");
        assert_eq!(
            jobs.iter()
                .map(|job| (job["id"].clone(), job["cache"].clone(), job["shard"].clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    json!("docs#1of2@linux-x86_64"),
                    json!("ci-docs-linux-x86_64-1of2"),
                    json!([1, 2])
                ),
                (
                    json!("docs#2of2@linux-x86_64"),
                    json!("ci-docs-linux-x86_64-2of2"),
                    json!([2, 2])
                ),
            ],
            "only selected stages are planned, once per shard with its own cache, never more shards than units"
        );
        assert!(
            plan(
                &metadata(),
                &selection,
                "linux-x86_64",
                false,
                &["lint".into()],
                &shard_counts(&["features=2".into()])?
            )
            .is_err(),
            "sharding an unselected group is a configuration error"
        );
        let fast = json!({"mode":"fast", "packages":["gix-path"]});
        assert!(
            plan(
                &metadata(),
                &fast,
                "linux-x86_64",
                false,
                &["lint".into()],
                &shard_counts(&["features=2".into(), "lint=50".into()])?
            )?["jobs"]
                .as_array()
                .expect("plan jobs")
                .iter()
                .all(|job| job["shard"].is_null()),
            "fast plans skip absent groups and never exceed a group's units"
        );
        assert!(
            shard_counts(&["features=0".into()]).is_err(),
            "zero shards are rejected"
        );
        assert!(
            shard_counts(&["features=2".into(), "features=3".into()]).is_err(),
            "a group cannot be sharded twice"
        );
        Ok(())
    }

    #[test]
    fn local_tests_share_the_exhaustive_native_build() -> gix_error::TestResult {
        let mut metadata = metadata();
        metadata["packages"].as_array_mut().expect("test packages").push(json!({
            "name":"gix-error", "targets":[{"kind":["lib"],"test":true,"doc":false,"doctest":false}],
            "metadata":{"qa":{"isolated":true,"tests":[{"args":[]}]}}
        }));
        let command = |mode: &str, id: &str| -> gix_error::Result<Command> {
            let selection = json!({"mode":mode, "packages":["gix-path","gix-error"]});
            let task = tasks(&metadata, &selection, "linux-x86_64")?
                .into_iter()
                .find(|task| task.id == id)
                .expect("task is planned");
            Ok(task.commands[0].clone())
        };
        let package_flags = |args: &[String]| {
            args.iter()
                .enumerate()
                .filter(|(index, arg)| {
                    matches!(arg.as_str(), "--workspace" | "--exclude")
                        || (*index > 0 && args[index - 1] == "--exclude")
                })
                .map(|(_, arg)| arg.clone())
                .collect::<Vec<_>>()
        };
        let native = command("exhaustive", "native-tests")?;
        for mode in ["fast", "exhaustive"] {
            let local = command(mode, "local-tests")?;
            assert_eq!(
                package_flags(&local.args),
                package_flags(&native.args),
                "the loop builds exactly the exhaustive native package set, so they share artifacts"
            );
            let filter = local.args.iter().position(|arg| arg == "-E");
            if mode == "fast" {
                assert_eq!(
                    filter.map(|index| local.args[index + 1].as_str()),
                    Some("package(=gix-path)"),
                    "fast mode filters to the tested packages, never an isolated one"
                );
            } else {
                assert_eq!(filter, None, "exhaustive mode runs every non-isolated test");
            }
        }
        let selection = json!({"mode":"fast", "packages":["gix-path","gix-error"]});
        let all = tasks(&metadata, &selection, "linux-x86_64")?;
        assert!(
            !select(all.clone(), &["all".into()])?
                .iter()
                .any(|task| task.id == "local-tests"),
            "`all` never selects the developer loop"
        );
        assert_eq!(
            select(all, &["isolated".into()])?
                .iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["isolated-tests"],
            "the isolated stage selects only isolated package tests"
        );
        Ok(())
    }

    #[test]
    fn ci_jobs_partition_the_same_local_tasks() -> gix_error::TestResult {
        for mode in ["fast", "exhaustive"] {
            let selection = json!({"mode":mode, "packages":["gix-path","gitoxide","internal-tools"]});
            let plan = plan(
                &metadata(),
                &selection,
                "linux-x86_64",
                true,
                &["all".into()],
                &BTreeMap::new(),
            )?;
            let mut seen = BTreeSet::new();
            for job in plan["jobs"].as_array().expect("plan jobs") {
                let platform = job["platform"].as_str().expect("job platform");
                let local = tasks(&metadata(), &selection, platform)?;
                for id in job["tasks"].as_array().expect("job task IDs") {
                    let id = id.as_str().expect("task ID");
                    assert!(
                        seen.insert((platform, id)),
                        "a platform task must have exactly one CI owner"
                    );
                    assert!(
                        local.iter().any(|task| task.id == id),
                        "every QA task is runnable through the local catalogue"
                    );
                }
            }
            for platform in CI_PLATFORMS {
                for task in tasks(&metadata(), &selection, platform)? {
                    assert_eq!(
                        seen.contains(&(*platform, task.id)),
                        assigned(&task, platform, mode == "exhaustive"),
                        "every selected task is assigned once, including nonblocking tasks"
                    );
                }
            }
        }
        Ok(())
    }
}
