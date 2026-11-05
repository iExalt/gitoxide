//! Run the hooks of a receiver the way `receive-pack` does: `pre-receive` and `update` while the push is judged, and
//! `post-receive` and `post-update` after the report was sent.
use std::{cell::RefCell, ffi::OsString, sync::Arc, time::Duration};

use gix_error::{Result, ResultExt, message};
use gix_hook::{Stderr, Stdin, Stdout};
use gix_object::bstr::{BString, ByteSlice};

use super::policy::{Command, Messages};
use crate::Repository;

/// What a hook is told to do while it runs, which a receiver has to answer to even if the hook says nothing.
pub(super) type Heartbeat<'h> = &'h mut (dyn FnMut() + 'h);

/// Runs the hooks of one push.
pub(super) struct Runner<'r, 'h> {
    repo: &'r Repository,
    settings: gix_hook::Settings,
    dir: std::path::PathBuf,
    /// Called every so often while a hook runs, if there is somebody who wants to know that it's still running.
    heartbeat: Option<(Duration, RefCell<Heartbeat<'h>>)>,
    push_options: &'r [BString],
    /// The name of the namespace that hooks are told about with `GIT_NAMESPACE`, as the environment of Git passes it on.
    namespace: Option<String>,
    /// Whether hooks are denied what describes the process of the client, as they are if a program was started by it.
    without_local_repo_env: bool,
    /// The file that hooks are told about with `GIT_SHALLOW_FILE`, once there is one.
    shallow_file: RefCell<Option<std::path::PathBuf>>,
}

impl<'r, 'h> Runner<'r, 'h> {
    pub fn new(
        repo: &'r Repository,
        push_options: &'r [BString],
        namespace: Option<String>,
        without_local_repo_env: bool,
        heartbeat: Option<(Duration, Heartbeat<'h>)>,
    ) -> Result<Self> {
        let settings = gix_hook::Settings::from_config(&repo.config.resolved, &mut repo.filter_config_section())
            .or_raise(|| message("Failed to read hook configuration"))?;
        let dir = repo
            .hooks_dir(gix_hook::Side::Receive)
            .or_raise(|| message("Could not find out where hooks are"))?;
        Ok(Runner {
            repo,
            settings,
            dir,
            heartbeat: heartbeat.map(|(interval, beat)| (interval, RefCell::new(beat))),
            push_options,
            namespace,
            without_local_repo_env,
            shallow_file: RefCell::new(None),
        })
    }

    /// The file that hooks are told about as the `shallow` file, if there is one.
    pub fn shallow_file(&self) -> Option<std::path::PathBuf> {
        self.shallow_file.borrow().clone()
    }

    /// Tell hooks about `file` as the `shallow` file to use, as Git does for pushes of a client that is shallow.
    pub fn set_shallow_file(&self, file: Option<std::path::PathBuf>) {
        *self.shallow_file.borrow_mut() = file;
    }

    /// Whether there is a hook for `event` that can be run.
    pub fn exists(&self, event: &str) -> bool {
        !gix_hook::Hooks::resolve(event, &self.dir, Some(&self.settings)).is_empty()
    }

    /// `pre-receive`: it gets all commands, whether they failed already or not, and the objects of the push if there are any.
    /// Return `true` if the push is declined.
    pub fn pre_receive(
        &self,
        commands: &[Command],
        quarantine: Option<&gix_odb::quarantine::Quarantine>,
        messages: &mut Messages,
    ) -> bool {
        if !self.exists("pre-receive") {
            return false;
        }
        let Some(mut env) = self.push_option_env(messages) else {
            return true;
        };
        if let Some(quarantine) = quarantine {
            let inherited = std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES");
            env.extend(quarantine.environment(inherited.as_deref()));
        }
        self.run("pre-receive", Vec::new(), env, command_lines(commands.iter()), messages) != 0
    }

    /// `update`, which is asked about every command that is about to be executed. Return `true` if it declines it.
    pub fn update(&self, command: &Command, messages: &mut Messages) -> bool {
        if !self.exists("update") {
            return false;
        }
        let Some(name) = name_arg(&command.ref_name) else {
            messages.diagnostic("the name of a reference can't be passed to the hook");
            return true;
        };
        let args = vec![
            name,
            command.old_id.to_string().into(),
            command.new_id.to_string().into(),
        ];
        self.run("update", args, Vec::new(), Vec::new(), messages) != 0
    }

    /// `push-to-checkout`, which does what `updateInstead` does itself if there is one. It runs where `receive-pack` is, with the
    /// work tree and Git directory of `worktree` in the environment, and with neither input nor a place on the side band.
    ///
    /// Return `None` if no hook ran to its end, which includes hooks that couldn't be started. Git does what it does without
    /// one then. Otherwise return whether the hook declined.
    pub fn push_to_checkout(
        &self,
        worktree: &Repository,
        new_id: &gix_hash::oid,
        messages: &mut Messages,
    ) -> Option<bool> {
        if !self.exists("push-to-checkout") {
            return None;
        }
        let absolute = |path: &std::path::Path| self.repo.current_dir().join(path);
        let work_dir = absolute(worktree.workdir()?);
        let mut prepare = match self.repo.hook("push-to-checkout") {
            Ok(prepare) => prepare,
            Err(err) => {
                messages.diagnostic(format!("cannot run the 'push-to-checkout' hook: {err}"));
                return None;
            }
        };
        if self.without_local_repo_env {
            for key in gix_command::LOCAL_REPO_ENV {
                prepare = prepare.env_remove(*key);
            }
        }
        if let Some(file) = &*self.shallow_file.borrow() {
            prepare = prepare.env("GIT_SHALLOW_FILE", file);
        }
        // Git names the Git directory of the main work tree as `.`, which it makes absolute without simplifying it, so that
        // `dirname "$GIT_DIR"` is the same for hooks of both.
        let mut git_dir = absolute(worktree.git_dir());
        if worktree.git_dir() == worktree.common_dir() {
            git_dir.push(".");
        }
        let outcome = prepare
            .with_context(gix_command::Context {
                git_dir: Some(git_dir),
                worktree_dir: Some(work_dir),
                ..Default::default()
            })
            .arg(new_id.to_hex().to_string())
            .stdin(Stdin::Null)
            .stdout(Stdout::ToStderr)
            .stderr(Stderr::Capture)
            .force_serial()
            .run()
            .ok()?;
        // Everything the hook says goes to the standard error of the receiver, which isn't for the client.
        for hook in &outcome.hooks {
            if !hook.output.is_empty() {
                messages.diagnostic(String::from_utf8_lossy(&hook.output).into_owned());
            }
        }
        // A hook that never ran to its end counts as none, which is how Git knows that it has to do the update itself. If one did,
        // a hook that couldn't be started after it declines, as it counts as failed, and nothing is started after it.
        outcome
            .hooks
            .iter()
            .any(|hook| hook.status.is_some())
            .then(|| outcome.exit_code() != 0)
    }

    /// `post-receive` and `post-update`, which can't change what happened, so their exit codes are ignored.
    pub fn post(&self, commands: &[Command], messages: &mut Messages) {
        let mut valid = commands
            .iter()
            .filter(|command| command.error.is_none() && !command.did_not_exist)
            .peekable();
        if valid.peek().is_none() {
            return;
        }
        if self.exists("post-receive")
            && let Some(env) = self.push_option_env(messages)
        {
            self.run(
                "post-receive",
                Vec::new(),
                env,
                command_lines(commands.iter().filter(|c| c.error.is_none() && !c.did_not_exist)),
                messages,
            );
        }
        if self.exists("post-update") {
            let args: Option<Vec<_>> = valid.map(|command| name_arg(&command.ref_name)).collect();
            let Some(args) = args else {
                messages.diagnostic("the name of a reference can't be passed to the hook");
                return;
            };
            self.run("post-update", args, Vec::new(), Vec::new(), messages);
        }
    }

    /// The variables for push options, or `None` if one of them can't be passed on, which declines the push.
    fn push_option_env(&self, messages: &mut Messages) -> Option<Vec<(OsString, OsString)>> {
        // Git sets the count even if there are no options.
        let mut env = Vec::with_capacity(self.push_options.len() + 1);
        for (index, option) in self.push_options.iter().enumerate() {
            let Ok(value) = gix_path::from_bstr(option.as_bstr()) else {
                messages.diagnostic("a push option can't be passed to the hook");
                return None;
            };
            env.push((
                format!("GIT_PUSH_OPTION_{index}").into(),
                value.into_owned().into_os_string(),
            ));
        }
        env.push((
            "GIT_PUSH_OPTION_COUNT".into(),
            self.push_options.len().to_string().into(),
        ));
        Some(env)
    }

    /// Run the hooks of `event` and return the exit code that Git would see, after relaying what they say.
    fn run(
        &self,
        event: &str,
        args: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
        stdin: Vec<u8>,
        messages: &mut Messages,
    ) -> i32 {
        let Ok(mut prepare) = self.repo.hook(event) else {
            messages.diagnostic(format!("cannot run the '{event}' hook"));
            return 1;
        };
        if self.without_local_repo_env {
            for key in gix_command::LOCAL_REPO_ENV {
                prepare = prepare.env_remove(*key);
            }
        }
        prepare = prepare
            .args(args)
            .stdin(Stdin::Feed(Arc::new(move |writer| writer.write_all(&stdin))))
            .stdout(Stdout::ToStderr)
            .stderr(Stderr::Capture);
        for (key, value) in env {
            prepare = prepare.env(key, value);
        }
        if let Some(file) = &*self.shallow_file.borrow() {
            prepare = prepare.env("GIT_SHALLOW_FILE", file);
        }
        if let Some(namespace) = &self.namespace {
            prepare = prepare.env("GIT_NAMESPACE", namespace);
        }
        let outcome = std::thread::scope(|scope| {
            let (sender, receiver) = std::sync::mpsc::channel();
            scope.spawn(move || {
                sender.send(prepare.run()).ok();
            });
            loop {
                let received = match &self.heartbeat {
                    Some((interval, _)) => receiver.recv_timeout(*interval),
                    None => receiver
                        .recv()
                        .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                };
                match received {
                    Ok(outcome) => break Some(outcome),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if let Some((_, beat)) = &self.heartbeat {
                            (beat.borrow_mut())();
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
                }
            }
        });
        match outcome {
            Some(Ok(outcome)) => {
                let resolved = gix_hook::Hooks::resolve(event, &self.dir, Some(&self.settings));
                for (hook, resolved) in outcome.hooks.iter().zip(resolved.hooks()) {
                    if !hook.output.is_empty() {
                        messages.hook_output(hook.output.clone());
                    }
                    // Git says so for hooks that were killed, naming the program like it started it.
                    if let Some(signal) = killed_by(hook.status) {
                        let program = match &resolved.kind {
                            gix_hook::Kind::Configured { command, .. } => command.clone(),
                            gix_hook::Kind::File { path } => {
                                let git_dir = self.repo.current_dir().join(self.repo.git_dir());
                                path.strip_prefix(git_dir).unwrap_or(path).display().to_string()
                            }
                        };
                        messages.hook_output(format!("error: {program} died of signal {signal}\n").into_bytes());
                    }
                }
                outcome.exit_code()
            }
            Some(Err(_)) | None => {
                messages.diagnostic(format!("cannot run the '{event}' hook"));
                1
            }
        }
    }
}

/// The signal that ended a hook, other than those that Git doesn't complain about as they are about an interrupt or about
/// the end of what is read.
#[cfg(unix)]
fn killed_by(status: Option<std::process::ExitStatus>) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status?.signal().filter(|signal| ![2, 3, 13].contains(signal))
}

#[cfg(not(unix))]
fn killed_by(_status: Option<std::process::ExitStatus>) -> Option<i32> {
    None
}

/// `<old> <new> <name>` of every command, one per line.
fn command_lines<'a>(commands: impl Iterator<Item = &'a Command>) -> Vec<u8> {
    let mut out = Vec::new();
    for command in commands {
        out.extend_from_slice(format!("{} {} ", command.old_id, command.new_id).as_bytes());
        out.extend_from_slice(&command.ref_name);
        out.push(b'\n');
    }
    out
}

/// The name of a reference as argument of a hook, which can't be done if the platform can't represent it.
fn name_arg(name: &BString) -> Option<OsString> {
    gix_path::from_bstr(name.as_bstr())
        .ok()
        .map(|path| path.into_owned().into_os_string())
}
