use std::{
    ffi::OsString,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use gix_error::{Result, message};

use crate::{Hook, Hooks, Kind};

/// A function producing the standard input of one hook. It is called once per hook, and a broken pipe is not an error.
pub type Feed = Arc<dyn Fn(&mut dyn Write) -> std::io::Result<()> + Send + Sync>;

/// What a hook gets as standard input.
#[derive(Clone, Default)]
pub enum Stdin {
    /// Nothing, like `/dev/null`.
    #[default]
    Null,
    /// These bytes.
    Bytes(Vec<u8>),
    /// The contents of the file at this path, which is opened for each hook.
    File(PathBuf),
    /// Whatever the function writes, called once for each hook.
    Feed(Feed),
}

/// Where a hook's standard output goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stdout {
    /// To standard error, just like `git` does by default.
    #[default]
    ToStderr,
    /// To standard output.
    Inherit,
    /// Nowhere.
    Null,
}

/// Where a hook's standard error goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stderr {
    /// To standard error.
    #[default]
    Inherit,
    /// Nowhere.
    Null,
    /// Into [`OutcomeOfHook::output`], together with standard output if that is [sent to standard error](Stdout::ToStderr).
    Capture,
}

/// The result of one hook.
#[derive(Debug)]
pub struct OutcomeOfHook {
    /// The [name](Hook::name) of the hook.
    pub name: String,
    /// The exit status, or `None` if the hook could not be started.
    pub status: Option<ExitStatus>,
    /// A failure to start the hook or to feed it standard input (other than a broken pipe).
    pub error: Option<std::io::Error>,
    /// The captured output if [captured](Stderr::Capture), which is empty otherwise.
    pub output: Vec<u8>,
}

impl OutcomeOfHook {
    /// The exit code as `git` sees it: the status code, 128 plus the signal number for a killed hook, and 1 if
    /// the hook failed to start or to be fed.
    pub fn exit_code(&self) -> i32 {
        let code = match self.status {
            None => 1,
            Some(status) => status.code().unwrap_or_else(|| signal_code(status)),
        };
        if code == 0 && self.error.is_some() { 1 } else { code }
    }
}

#[cfg(unix)]
fn signal_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map_or(1, |signal| 128 + signal)
}

#[cfg(not(unix))]
fn signal_code(_status: ExitStatus) -> i32 {
    1
}

/// What happened when running the [`Hooks`] of an event.
#[derive(Debug)]
pub struct Outcome {
    /// One entry for each hook that was run, in the order of [`Hooks::hooks()`].
    pub hooks: Vec<OutcomeOfHook>,
    /// The hook file that was skipped for not being executable, which warrants [advice](crate::ignored_hook_hint()).
    pub not_executable: Option<PathBuf>,
}

impl Outcome {
    /// Return `true` if at least one hook was run.
    pub fn invoked(&self) -> bool {
        !self.hooks.is_empty()
    }

    /// The bitwise OR of all hook exit codes, like `git`, so it is zero only if all hooks succeeded or none ran.
    pub fn exit_code(&self) -> i32 {
        self.hooks.iter().fold(0, |acc, hook| acc | hook.exit_code())
    }

    /// Return `true` if no hook failed.
    pub fn is_success(&self) -> bool {
        self.exit_code() == 0
    }

    /// Use the outcome of hooks that can veto an operation: any failing hook is an error.
    pub fn into_veto(self) -> Result<()> {
        match self.hooks.iter().find(|hook| hook.exit_code() != 0) {
            Some(hook) => gix_error::bail!(message!(
                "hook '{}' failed with exit code {}",
                hook.name,
                hook.exit_code()
            )),
            None => Ok(()),
        }
    }
}

/// The configuration for running [`Hooks`], which are started with [`run()`](Self::run()).
pub struct Prepare {
    hooks: Hooks,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    env_remove: Vec<OsString>,
    stdin: Stdin,
    stdout: Stdout,
    stderr: Stderr,
    current_dir: Option<PathBuf>,
    context: Option<gix_command::Context>,
    jobs: Option<usize>,
    force_serial: bool,
}

impl Prepare {
    /// Prepare running `hooks` without arguments, with an empty standard input and the current directory.
    pub fn new(hooks: Hooks) -> Self {
        Prepare {
            hooks,
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            stdin: Stdin::Null,
            stdout: Stdout::default(),
            stderr: Stderr::default(),
            current_dir: None,
            context: None,
            jobs: None,
            force_serial: false,
        }
    }

    /// Add one argument.
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set an environment variable.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Remove an environment variable that would be inherited, unless it's [set explicitly](Self::env()) or by the
    /// [context](Self::with_context()).
    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    /// Set what the hooks get as standard input.
    pub fn stdin(mut self, stdin: Stdin) -> Self {
        self.stdin = stdin;
        self
    }

    /// Set where standard output goes. When hooks run in parallel it always goes to standard error, as in `git`.
    pub fn stdout(mut self, stdout: Stdout) -> Self {
        self.stdout = stdout;
        self
    }

    /// Set where standard error goes. In parallel, each hook's output is buffered and replayed when it is done.
    pub fn stderr(mut self, stderr: Stderr) -> Self {
        self.stderr = stderr;
        self
    }

    /// Run the hooks in `dir`, which for `git` is the top of the worktree, or the Git directory of a bare repository.
    /// A relative hook path is made absolute using the current directory of this process, so callers should anchor relative hook directories themselves.
    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    /// Provide the repository environment the hooks should see.
    pub fn with_context(mut self, context: gix_command::Context) -> Self {
        self.context = Some(context);
        self
    }

    /// Run up to `jobs` hooks at once instead of the number configured, like `git hook run -j`.
    pub fn jobs(mut self, jobs: usize) -> Self {
        self.jobs = Some(jobs.max(1));
        self
    }

    /// Never run hooks in parallel, regardless of configuration, as `git` does for `push-to-checkout`.
    pub fn force_serial(mut self) -> Self {
        self.force_serial = true;
        self
    }

    /// Run all hooks, waiting for them, and continue after a failing hook like `git`.
    ///
    /// A hook that fails to start or exits unsuccessfully is reported in the [`Outcome`], and an error is only returned
    /// if a command can't be prepared at all.
    pub fn run(self) -> Result<Outcome> {
        let jobs = if self.force_serial {
            1
        } else {
            self.jobs.unwrap_or(self.hooks.jobs)
        };
        let mut commands = Vec::with_capacity(self.hooks.hooks.len());
        for hook in &self.hooks.hooks {
            commands.push((self.command(hook, false)?, self.command(hook, true)?));
        }
        let parallel = jobs > 1;
        let results = Mutex::new((0..commands.len()).map(|_| None).collect::<Vec<_>>());
        let next = AtomicUsize::new(0);
        // Like Git, no hook is started after one that couldn't be, which leaves the ones before it as what ran.
        let start_failed = std::sync::atomic::AtomicBool::new(false);
        let replay = Mutex::new(());
        let work = || {
            loop {
                if start_failed.load(Ordering::Relaxed) {
                    break;
                }
                let idx = next.fetch_add(1, Ordering::Relaxed);
                let Some(((direct, via_shell), hook)) = commands.get(idx).zip(self.hooks.hooks.get(idx)) else {
                    break;
                };
                let outcome = self.run_one(hook, direct, via_shell, parallel, &replay);
                if outcome.status.is_none() && outcome.error.is_some() {
                    start_failed.store(true, Ordering::Relaxed);
                }
                results.lock().expect("a panicking hook run poisons nothing else")[idx] = Some(outcome);
            }
        };
        if parallel {
            std::thread::scope(|scope| {
                for _ in 0..jobs.min(commands.len()) {
                    scope.spawn(work);
                }
            });
        } else {
            work();
        }
        Ok(Outcome {
            hooks: results
                .into_inner()
                .expect("no panic while holding the lock")
                .into_iter()
                .flatten()
                .collect(),
            not_executable: self.hooks.not_executable.clone(),
        })
    }

    fn command(&self, hook: &Hook, via_shell: bool) -> Result<CommandFactory> {
        let (mut program, mut args) = match &hook.kind {
            Kind::Configured { command, .. } => configured_command(command, &self.args),
            Kind::File { path } => {
                // Like Git, make the path absolute relative to our directory as the hook may run elsewhere.
                let path = if self.current_dir.is_some() {
                    std::path::absolute(path).unwrap_or_else(|_| path.clone())
                } else {
                    path.clone()
                };
                (path.into(), self.args.clone())
            }
        };
        if via_shell {
            // Like Git when exec fails with ENOEXEC: `sh <program> <args>…`.
            let shell = gix_path::env::shell_command();
            let mut shell_args: Vec<OsString> = shell.get_args().map(Into::into).collect();
            shell_args.push(program);
            shell_args.append(&mut args);
            program = shell.get_program().into();
            args = shell_args;
        }
        let mut prep = gix_command::prepare(program).args(args);
        for (key, value) in &self.env {
            prep = prep.env(key, value);
        }
        if let Some(ctx) = &self.context {
            prep = prep.with_context(ctx.clone());
        }
        let mut cmd = Command::try_from(prep)?;
        let set_by_context = |key: &OsString| {
            self.context.as_ref().is_some_and(|ctx| {
                (key == "GIT_DIR" && ctx.git_dir.is_some()) || (key == "GIT_WORK_TREE" && ctx.worktree_dir.is_some())
            })
        };
        for key in &self.env_remove {
            if !self.env.iter().any(|(set, _)| set == key) && !set_by_context(key) {
                cmd.env_remove(key);
            }
        }
        if let Some(dir) = &self.current_dir {
            cmd.current_dir(dir);
        }
        Ok(CommandFactory(Mutex::new(Some(cmd))))
    }

    fn run_one(
        &self,
        hook: &Hook,
        direct: &CommandFactory,
        via_shell: &CommandFactory,
        parallel: bool,
        replay: &Mutex<()>,
    ) -> OutcomeOfHook {
        let mut outcome = OutcomeOfHook {
            name: hook.name.clone(),
            status: None,
            error: None,
            output: Vec::new(),
        };
        let stdout = if parallel { Stdout::ToStderr } else { self.stdout };
        let capture = parallel || self.stderr == Stderr::Capture;
        let result = self.spawn(direct, stdout, capture).or_else(|err| {
            if is_exec_format_error(&err) {
                self.spawn(via_shell, stdout, capture)
            } else {
                Err(err)
            }
        });
        let (mut child, reader) = match result {
            Ok(res) => res,
            Err(err) => {
                outcome.error = Some(err);
                return outcome;
            }
        };
        let output = std::thread::scope(|scope| {
            let reader = reader.map(|mut reader| {
                scope.spawn(move || {
                    let mut buf = Vec::new();
                    reader.read_to_end(&mut buf).ok();
                    buf
                })
            });
            if let Err(err) = self.feed(&mut child) {
                outcome.error = Some(err);
            }
            outcome.status = child.wait().map_err(|err| outcome.error.get_or_insert(err)).ok();
            reader.map(|reader| reader.join().unwrap_or_default())
        });
        if let Some(output) = output {
            if self.stderr == Stderr::Capture {
                outcome.output = output;
            } else if self.stderr == Stderr::Inherit {
                let _guard = replay.lock();
                std::io::stderr().write_all(&output).ok();
            }
        }
        outcome
    }

    fn spawn(
        &self,
        factory: &CommandFactory,
        stdout: Stdout,
        capture: bool,
    ) -> std::io::Result<(std::process::Child, Option<std::io::PipeReader>)> {
        let mut cmd = factory.take();
        cmd.stdin(match self.stdin {
            Stdin::Null => Stdio::null(),
            Stdin::File(ref path) => Stdio::from(std::fs::File::open(path)?),
            Stdin::Bytes(_) | Stdin::Feed(_) => Stdio::piped(),
        });
        let no_stderr = self.context.as_ref().is_some_and(|ctx| ctx.stderr == Some(false));
        let mut reader = None;
        if capture && !no_stderr {
            let (pipe_reader, writer) = std::io::pipe()?;
            if stdout == Stdout::ToStderr {
                cmd.stdout(writer.try_clone()?);
            }
            cmd.stderr(writer);
            reader = Some(pipe_reader);
        } else {
            cmd.stderr(if no_stderr || self.stderr == Stderr::Null {
                Stdio::null()
            } else {
                Stdio::inherit()
            });
            if stdout == Stdout::ToStderr {
                cmd.stdout(if no_stderr || self.stderr == Stderr::Null {
                    Stdio::null()
                } else {
                    stdout_to_stderr()
                });
            }
        }
        match stdout {
            Stdout::ToStderr => {}
            Stdout::Inherit => {
                cmd.stdout(Stdio::inherit());
            }
            Stdout::Null => {
                cmd.stdout(Stdio::null());
            }
        }
        let child = cmd.spawn();
        drop(cmd); // closes our copies of the pipe writers so readers see EOF
        child.map(|child| (child, reader))
    }

    fn feed(&self, child: &mut std::process::Child) -> std::io::Result<()> {
        let Some(mut stdin) = child.stdin.take() else {
            return Ok(());
        };
        let res = match &self.stdin {
            Stdin::Bytes(bytes) => stdin.write_all(bytes),
            Stdin::Feed(feed) => feed(&mut stdin),
            Stdin::Null | Stdin::File(_) => Ok(()),
        };
        match res {
            Err(err) if err.kind() != std::io::ErrorKind::BrokenPipe => Err(err),
            _ => Ok(()),
        }
    }
}

/// A command that can be taken once, as spawning consumes its pipes.
struct CommandFactory(Mutex<Option<Command>>);

impl CommandFactory {
    fn take(&self) -> Command {
        self.0
            .lock()
            .expect("not poisoned")
            .take()
            .expect("each prepared command is spawned at most once")
    }
}

/// `git` runs hooks with their standard output redirected to standard error.
fn stdout_to_stderr() -> Stdio {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        if let Ok(fd) = std::io::stderr().as_fd().try_clone_to_owned() {
            return Stdio::from(fd);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        if let Ok(handle) = std::io::stderr().as_handle().try_clone_to_owned() {
            return Stdio::from(handle);
        }
    }
    Stdio::inherit()
}

/// Scripts without a shebang fail to `exec`, but `git` runs them with the shell.
fn is_exec_format_error(err: &std::io::Error) -> bool {
    cfg!(unix) && err.raw_os_error() == Some(8)
}

/// Build the program and arguments for a configured hook like Git's `prepare_shell_cmd()`: run the command directly unless
/// it has shell syntax, in which case `sh -c '<command> "$@"' <command> <args>…` makes `$0` the command itself.
fn configured_command(command: &str, args: &[OsString]) -> (OsString, Vec<OsString>) {
    const SHELL_METACHARACTERS: &[u8] = b"|&;<>()$`\\\"' \t\n*?[#~=%";
    if !command.bytes().any(|b| SHELL_METACHARACTERS.contains(&b)) {
        return (command.into(), args.to_vec());
    }
    let shell = gix_path::env::shell_command();
    let mut out: Vec<OsString> = shell.get_args().map(Into::into).collect();
    out.push("-c".into());
    out.push(if args.is_empty() {
        command.into()
    } else {
        format!("{command} \"$@\"").into()
    });
    out.push(command.into());
    out.extend(args.iter().cloned());
    (shell.get_program().into(), out)
}
