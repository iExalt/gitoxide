use std::process::{Command, Stdio};

use bstr::{BString, ByteSlice, ByteVec};
use gix_error::{OptionExt, Result, bail, not_found, unsupported};

use crate::{Program, helper};

///
pub mod environment;
pub use environment::Environment;

/// Run the helper `name_and_args` like `foo --bar` as `git-credential-foo --bar`, with `action` as last argument, which is what
/// `git credential-foo --bar <action>` would run, without `git` being involved in finding it.
///
/// The executable is located in `environment` and correctly quoted, while assuming that the arguments are good to be put into a script.
/// A helper that can't be found is a [not-found](gix_error::Error::is_not_found()) error.
fn external_name_command(
    name_and_args: &bstr::BStr,
    environment: &Environment,
    action: &helper::Action,
) -> Result<std::process::Command> {
    let name_end = name_and_args.find_byteset(b" \t").unwrap_or(name_and_args.len());
    let (name, args) = name_and_args.split_at(name_end);
    let name = name.as_bstr();
    let program = name
        .to_str()
        .ok()
        .and_then(|name| environment.find_helper(name))
        .ok_or_raise(|| {
            not_found(format!(
                "Could not find an executable named 'git-credential-{name}' in the exec-path or in PATH to run the credential helper {name_and_args:?}",
            ))
        })?;
    let program = gix_path::to_unix_separators_on_windows(gix_path::into_bstr(program)?);
    let mut script = gix_quote::single(program.as_ref());
    script.push_str(args);
    gix_command::prepare(gix_path::from_bstr(script.as_bstr())?.into_owned())
        .arg(action.as_arg(true))
        .command_may_be_shell_script_allow_manual_argument_splitting()
        .try_into()
}

/// The kind of helper program to use.
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Kind {
    /// The built-in `git credential` helper program, part of any `git` distribution.
    Builtin,
    /// The `store` helper of Git, `git credential-store`, run natively as [`Store`](crate::store::Store) without involving `git`.
    ///
    /// [`Program::from_custom_definition()`] chooses it for every definition that starts with the word `store`, so it never
    /// runs a `git-credential-store` executable, which is the one of Git whenever it can be found.
    /// The arguments are understood when the helper runs, and anything other than `--file <path>` or `--file=<path>`, with
    /// `sh` quoting and a leading `~` being expanded to the home directory in the former, makes the helper fail.
    Store {
        /// The arguments that follow `store`, with `sh` quoting rules, like `--file "~/my credentials"`.
        arguments: BString,
    },
    /// A custom credentials helper, as identified just by the name with optional arguments
    ExternalName {
        /// The name like `foo` along with optional args, like `foo --arg --bar="a b"`, with arguments using `sh` shell quoting rules.
        /// The program executed will be `git-credential-foo [args]` if `name_and_args` starts with `foo [args]`.
        /// It is found in the directories of [`Environment::exec_path`] and then in `PATH`, so Git isn't involved, and
        /// it is a failure of the helper if there is no such program.
        /// Note that a shell is only used if it's needed.
        name_and_args: BString,
    },
    /// A custom credentials helper, as identified just by the absolute path to the program and optional arguments. The program is executed through a shell.
    ExternalPath {
        /// The absolute path to the executable, like `/path/to/exe` along with optional args, like `/path/to/exe --arg --bar="a b"`, with arguments using `sh`
        /// shell quoting rules.
        path_and_args: BString,
    },
    /// A script to execute with `sh`.
    ExternalShellScript(BString),
}

/// Initialization
impl Program {
    /// Create a new program of the given `kind`.
    pub fn from_kind(kind: Kind) -> Self {
        Program {
            kind,
            child: None,
            stderr: true,
            environment: None,
        }
    }

    /// Parse the given input as per the custom helper definition, supporting `!<script>`, `name` and `/absolute/name`, the latter two
    /// also support arguments which are ignored here.
    ///
    /// Returns an error if the helper name or path cannot be represented by the platform.
    pub fn from_custom_definition(input: impl Into<BString>) -> Result<Self> {
        fn from_custom_definition_inner(mut input: BString) -> Result<Program> {
            let kind = if let Some(arguments) = store_arguments(input.as_bstr()) {
                Kind::Store { arguments }
            } else if input.starts_with(b"!") {
                input.remove(0);
                Kind::ExternalShellScript(input)
            } else {
                let path = gix_path::from_bstr(
                    input
                        .find_byte(b' ')
                        .map_or(input.as_slice(), |pos| &input[..pos])
                        .as_bstr(),
                )?;
                if gix_path::is_absolute(path) {
                    Kind::ExternalPath { path_and_args: input }
                } else {
                    Kind::ExternalName { name_and_args: input }
                }
            };
            Ok(Program {
                kind,
                child: None,
                stderr: true,
                environment: None,
            })
        }
        from_custom_definition_inner(input.into())
    }

    /// Convert the program into the respective command, suitable to invoke `action`.
    ///
    /// Returns an error if helper or executable paths cannot be represented in the required encoding.
    pub fn to_command(&self, action: &helper::Action) -> Result<std::process::Command> {
        let mut cmd = match &self.kind {
            Kind::Builtin => {
                let mut cmd = Command::try_from(gix_command::prepare(gix_path::env::exe_invocation()))?;
                cmd.arg("credential").arg(action.as_arg(false));
                cmd
            }
            Kind::ExternalName { name_and_args } => {
                let environment = self.environment.clone().unwrap_or_else(Environment::from_process);
                external_name_command(name_and_args.as_bstr(), &environment, action)?
            }
            Kind::ExternalShellScript(for_shell)
            | Kind::ExternalPath {
                path_and_args: for_shell,
            } => gix_command::prepare(gix_path::from_bstr(for_shell.as_bstr())?.as_ref())
                .command_may_be_shell_script()
                .arg(action.as_arg(true))
                .try_into()?,
            Kind::Store { .. } => bail!(unsupported(
                "The `store` helper runs natively and has no command, call `helper::invoke()` instead"
            )),
        };
        cmd.stdin(Stdio::piped())
            .stdout(if action.expects_output() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if self.stderr { Stdio::inherit() } else { Stdio::null() });
        Ok(cmd)
    }
}

/// Return the arguments of the `store` helper if `input` starts with the word `store`.
fn store_arguments(input: &bstr::BStr) -> Option<BString> {
    let arguments = input.strip_prefix(b"store")?;
    (arguments.is_empty() || arguments[0].is_ascii_whitespace()).then(|| arguments.trim().into())
}

/// Builder
impl Program {
    /// By default `stderr` of programs is inherited and typically displayed in the terminal.
    pub fn suppress_stderr(mut self) -> Self {
        self.stderr = false;
        self
    }

    /// Look for helpers in `environment` instead of the one of the process.
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = Some(environment);
        self
    }
}

impl Program {
    pub(crate) fn start(
        &mut self,
        action: &helper::Action,
    ) -> std::io::Result<(std::process::ChildStdin, Option<std::process::ChildStdout>)> {
        assert!(self.child.is_none(), "BUG: must not call `start()` twice");
        let mut cmd = self.to_command(action).map_err(|err| {
            let kind = if err.is_not_found() {
                std::io::ErrorKind::NotFound
            } else {
                std::io::ErrorKind::InvalidInput
            };
            std::io::Error::new(kind, err)
        })?;
        gix_trace::debug!(cmd = ?cmd, "launching credential helper");
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin to be configured");
        let stdout = child.stdout.take();

        self.child = child.into();
        Ok((stdin, stdout))
    }

    pub(crate) fn finish(&mut self) -> std::io::Result<()> {
        let mut child = self.child.take().expect("Call `start()` before calling finish()");
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            let failure = gix_error::message("Credentials helper program failed").with_exit_status(status);
            Err(std::io::Error::other(failure))
        }
    }
}

///
pub mod main;
pub use main::function::main;

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::Environment;
    use crate::helper;

    #[test]
    #[cfg(unix)]
    fn the_resolved_helper_is_a_single_shell_token_even_if_its_path_contains_spaces() -> gix_testtools::TestResult {
        use std::os::unix::fs::PermissionsExt;
        let dir = gix_testtools::tempfile::tempdir()?;
        let exec_dir = dir.path().join("with space");
        std::fs::create_dir(&exec_dir)?;
        let helper = exec_dir.join("git-credential-manager");
        std::fs::write(&helper, "#!/bin/sh\n")?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))?;
        let environment = Environment {
            exec_path: vec![exec_dir],
            path: Some("".into()),
            ..Default::default()
        };

        let cmd = super::external_name_command(
            "manager --config=~/credentials".into(),
            &environment,
            &helper::Action::Get(Default::default()),
        )?;
        let script = cmd
            .get_args()
            .skip_while(|arg| *arg != OsStr::new("-c"))
            .nth(1)
            .expect("a shell invocation has its script after the '-c' argument");

        assert_eq!(
            script.to_string_lossy(),
            format!("'{}' --config=~/credentials \"$@\"", helper.display()),
            "the helper's path is quoted as one token and its arguments follow verbatim"
        );
        Ok(())
    }
}
