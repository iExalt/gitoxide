//! A stand-in for `ssh` that runs the requested command locally, like the `test-fake-ssh` program of Git does, and records how it was invoked.
//!
//! Use [`FakeSsh::command()`](crate::fake_ssh::FakeSsh::command()) as `core.sshCommand`, `GIT_SSH` or `GIT_SSH_COMMAND` to see which arguments and environment
//! a client passes to `ssh`, and to talk to `git upload-pack` and `git receive-pack` without a server.
//! It authenticates nobody, so authentication is [simulated](crate::fake_ssh::Behavior) at best, and real servers are needed to prove it.
use std::path::{Path, PathBuf};

/// What the fake `ssh` does after recording its invocation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Behavior {
    /// Run the command that was passed after the host, with the programs of Git's exec-path in `PATH`.
    #[default]
    RunCommand,
    /// Fail like `ssh` does when the server refuses a key: print `Permission denied (publickey).` to standard error and exit with 255.
    PermissionDenied,
    /// Sleep for this many seconds before running the command, to simulate a connection that hangs.
    SleepThenRunCommand(u32),
    /// Never run the command, but sleep for this many seconds, which simulates a server that accepted the connection and then says nothing.
    Hang(u32),
}

/// The recorded invocation of the fake `ssh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The arguments as received, without the program name.
    pub args: Vec<String>,
    /// The values of the environment variables that are interesting for clients: `GIT_PROTOCOL`, `LANG` and `LC_ALL`, when set.
    pub env: Vec<(String, String)>,
}

impl Invocation {
    /// The command to run on the host, which is everything after the host as ssh would join it.
    pub fn remote_command(&self) -> Option<String> {
        let mut args = self.args.iter();
        while let Some(arg) = args.next() {
            if arg.starts_with('-') {
                if arg.len() == 2
                    && arg
                        .strip_prefix('-')
                        .is_some_and(|option| "oplPiFJLRDEeSbcmOQWwBI".contains(option))
                {
                    args.next();
                }
                continue;
            }
            return Some(args.cloned().collect::<Vec<_>>().join(" "));
        }
        None
    }

    /// The value of the environment variable `name`, if it was set.
    pub fn env(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_str()))
    }
}

/// A fake `ssh` executable in a temporary directory.
pub struct FakeSsh {
    dir: tempfile::TempDir,
}

impl FakeSsh {
    /// Create a fake `ssh` with `behavior`.
    ///
    /// The location of Git's exec path is determined now, so tests that put programs on `PATH` which record how Git is
    /// invoked have to create it before they do. Only the OpenSSH way of passing a port, `-p`, and the options that take
    /// values and that OpenSSH knows, are understood.
    pub fn new(behavior: Behavior) -> std::io::Result<Self> {
        let dir = tempfile::tempdir()?;
        let exec_path = gix_path::env::core_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        let log = dir.path().join("invocations");
        let action = match behavior {
            Behavior::RunCommand => String::new(),
            Behavior::PermissionDenied => "echo 'Permission denied (publickey).' >&2\nexit 255\n".into(),
            Behavior::SleepThenRunCommand(seconds) => format!("sleep {seconds}\n"),
            // `exec` so that killing the script ends the sleep, rather than leaving it holding the pipes.
            Behavior::Hang(seconds) => format!("exec sleep {seconds}\n"),
        };
        let script = format!(
            r#"#!/bin/sh
# Record the invocation, one argument per line between the start and end markers.
{{
  echo "invocation"
  for arg in "$@"; do printf 'arg:%s\n' "$arg"; done
  [ -n "${{GIT_PROTOCOL+x}}" ] && printf 'env:GIT_PROTOCOL=%s\n' "$GIT_PROTOCOL"
  [ -n "${{LANG+x}}" ] && printf 'env:LANG=%s\n' "$LANG"
  [ -n "${{LC_ALL+x}}" ] && printf 'env:LC_ALL=%s\n' "$LC_ALL"
  echo "end"
}} >> '{log}'
{action}
# Skip the options, and the host, to find the command.
while [ $# -gt 0 ]; do
  case "$1" in
    -[oplPliFJLRDEeSbcmOQWwBI]) shift 2;;
    -*) shift;;
    *) break;;
  esac
done
shift
# Run the command like a server would: without the configuration or the Git environment of the client.
for name in $(env | sed -n 's/^\(GIT_[A-Za-z0-9_]*\)=.*/\1/p'); do
  [ "$name" = GIT_PROTOCOL ] || unset "$name"
done
HOME='{home}'
XDG_CONFIG_HOME='{home}/.config'
GIT_CONFIG_NOSYSTEM=1
GIT_CONFIG_GLOBAL=/dev/null
GIT_TERMINAL_PROMPT=false
PATH='{exec_path}':"$PATH"
export HOME XDG_CONFIG_HOME GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL GIT_TERMINAL_PROMPT PATH
eval "exec $*"
"#,
            home = dir.path().join("server-home").display(),
            log = log.display(),
        );
        let path = dir.path().join("ssh");
        std::fs::write(&path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(FakeSsh { dir })
    }

    /// The path of the executable, to be used as `ssh` program.
    pub fn command(&self) -> PathBuf {
        self.dir.path().join("ssh")
    }

    /// The directory containing the executable.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// All invocations so far, oldest first.
    pub fn invocations(&self) -> Vec<Invocation> {
        let content = std::fs::read_to_string(self.dir.path().join("invocations")).unwrap_or_default();
        let mut out = Vec::new();
        let mut current: Option<Invocation> = None;
        for line in content.lines() {
            match (line, current.as_mut()) {
                ("invocation", _) => {
                    current = Some(Invocation {
                        args: Vec::new(),
                        env: Vec::new(),
                    });
                }
                ("end", _) => out.extend(current.take()),
                (line, Some(invocation)) => {
                    if let Some(arg) = line.strip_prefix("arg:") {
                        invocation.args.push(arg.to_owned());
                    } else if let Some((key, value)) = line.strip_prefix("env:").and_then(|env| env.split_once('=')) {
                        invocation.env.push((key.to_owned(), value.to_owned()));
                    }
                }
                _ => {}
            }
        }
        out
    }
}
