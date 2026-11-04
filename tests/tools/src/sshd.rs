//! A real `sshd` that runs as the current user on the loopback interface, with keys that are generated for it, to test
//! clients against the authentication and the environment handling of an actual server rather than a simulation.
//!
//! It accepts public keys only, from the keys that [`Sshd::client_config()`] points to, and passes `GIT_PROTOCOL` on, so that
//! protocol version 2 works. The commands that clients send are run as the current user, in a shell, like `sshd` always does,
//! so repositories are addressed by absolute path.
//!
//! Clients run their commands through a `ForceCommand` wrapper that isolates the server side like [`FakeSsh`](crate::fake_ssh::FakeSsh)
//! does: without the Git configuration and environment of the user, and with the exec path of Git in `PATH`. The login shell of the
//! user still runs its startup files before that, which only a different account could avoid.
//!
//! The server is not available everywhere: [`Sshd::start()`] returns `Ok(None)` if the programs it needs aren't installed.
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// A running `sshd`, which is stopped when dropped.
pub struct Sshd {
    dir: tempfile::TempDir,
    port: u16,
    user: String,
    child: Child,
}

fn find_program(name: &str, extra_dirs: &[&str]) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .chain(extra_dirs.iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn run(program: &Path, args: &[&str]) -> std::io::Result<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "{} {args:?} failed: {}",
            program.display(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

impl Sshd {
    /// Start a server, trying again if its port was taken in the meantime, or return `None` if `sshd`, `ssh` or `ssh-keygen` can't be found, or if the current user is `root`,
    /// whose logins `sshd` would have to restrict differently.
    ///
    /// The location of the exec path of Git is determined now, so tests that put programs on `PATH` which record how Git is
    /// invoked have to start the server before they do.
    pub fn start() -> std::io::Result<Option<Self>> {
        let mut attempt = 1;
        loop {
            match Self::start_once() {
                Err(err) if attempt < 3 => {
                    eprintln!("sshd didn't start on attempt {attempt}: {err}");
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    fn start_once() -> std::io::Result<Option<Self>> {
        let (Some(sshd), Some(keygen), Some(_ssh)) = (
            find_program("sshd", &["/usr/sbin", "/usr/local/sbin"]),
            find_program("ssh-keygen", &[]),
            find_program("ssh", &[]),
        ) else {
            return Ok(None);
        };
        let id = find_program("id", &["/usr/bin"]).ok_or_else(|| std::io::Error::other("no `id` program"))?;
        let user = run(&id, &["-un"])?;
        if user == "root" {
            return Ok(None);
        }
        let dir = tempfile::tempdir()?;
        let path = |name: &str| dir.path().join(name);
        for key in ["host_key", "client_key", "other_key"] {
            run(
                &keygen,
                &[
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-C",
                    "synthetic",
                    "-f",
                    &path(key).display().to_string(),
                ],
            )?;
        }
        std::fs::copy(path("client_key.pub"), path("authorized_keys"))?;

        let port = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            listener.local_addr()?.port()
        };
        std::fs::write(
            path("sshd_config"),
            format!(
                "Port {port}\n\
                 ListenAddress 127.0.0.1\n\
                 HostKey {host_key}\n\
                 PidFile none\n\
                 AuthorizedKeysFile {authorized_keys}\n\
                 PubkeyAuthentication yes\n\
                 PasswordAuthentication no\n\
                 KbdInteractiveAuthentication no\n\
                 UsePAM no\n\
                 StrictModes no\n\
                 AcceptEnv GIT_PROTOCOL\n\
                 AllowUsers {user}\n\
                 ForceCommand {wrapper}\n\
                 LogLevel ERROR\n",
                host_key = path("host_key").display(),
                authorized_keys = path("authorized_keys").display(),
                wrapper = path("server_side.sh").display(),
            ),
        )?;
        let exec_path = gix_path::env::core_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        let wrapper = path("server_side.sh");
        std::fs::write(
            &wrapper,
            format!(
                r#"#!/bin/sh
for name in $(env | sed -n 's/^\(GIT_[A-Za-z0-9_]*\)=.*/\1/p'); do
  [ "$name" = GIT_PROTOCOL ] || unset "$name"
done
HOME='{home}'
XDG_CONFIG_HOME='{home}/.config'
GIT_CONFIG_NOSYSTEM=1
GIT_CONFIG_GLOBAL=/dev/null
GIT_TERMINAL_PROMPT=false
{path_assignment}
export HOME XDG_CONFIG_HOME GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL GIT_TERMINAL_PROMPT PATH
# Record which Git serves the client, for tests to see.
git --exec-path >> '{log}'
exec sh -c "$SSH_ORIGINAL_COMMAND"
"#,
                home = path("server-home").display(),
                log = path("server_side.log").display(),
                // Without the exec path, the `PATH` stays as it is, and doesn't gain an empty entry that means the current directory.
                path_assignment = if exec_path.is_empty() {
                    "PATH=\"$PATH\"".to_owned()
                } else {
                    format!("PATH='{exec_path}':\"$PATH\"")
                },
            ),
        )?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
        }
        let log = std::fs::File::create(path("sshd.log"))?;
        let child = Command::new(&sshd)
            .args(["-D", "-e", "-f"])
            .arg(path("sshd_config"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        let mut server = Sshd { dir, port, user, child };
        // Wait until it accepts connections, which has to be quick for a program that does nothing else.
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if let Some(status) = server.child.try_wait()? {
                return Err(std::io::Error::other(format!(
                    "sshd exited with {status}: {}",
                    std::fs::read_to_string(server.dir.path().join("sshd.log")).unwrap_or_default()
                )));
            }
            if Instant::now() > deadline {
                return Err(std::io::Error::other("sshd didn't start listening in time"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(Some(server))
    }

    /// The exec paths of the Git programs that served clients, one line per command that was run.
    pub fn server_side_git_exec_paths(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("server_side.log"))
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    }

    /// The path of an `ssh` configuration with the host alias `testhost`, which logs in with the authorized key.
    pub fn client_config(&self) -> PathBuf {
        self.write_client_config("client_config", "client_key")
    }

    /// Like [`client_config()`](Self::client_config()), but with a key that the server doesn't know, so logins are refused.
    pub fn client_config_with_unknown_key(&self) -> PathBuf {
        self.write_client_config("unknown_config", "other_key")
    }

    fn write_client_config(&self, name: &str, key: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(
            &path,
            format!(
                "Host testhost\n\
                 \x20 HostName 127.0.0.1\n\
                 \x20 Port {}\n\
                 \x20 User {}\n\
                 \x20 IdentityFile {}\n\
                 \x20 IdentitiesOnly yes\n\
                 \x20 StrictHostKeyChecking no\n\
                 \x20 UserKnownHostsFile /dev/null\n\
                 \x20 BatchMode yes\n\
                 \x20 LogLevel ERROR\n",
                self.port,
                self.user,
                self.dir.path().join(key).display()
            ),
        )
        .expect("the temporary directory is writable");
        path
    }

    /// The command to configure as `ssh` program for clients, which uses `config` and so knows the host `testhost`.
    pub fn command(config: &Path) -> String {
        format!("ssh -F {}", config.display())
    }

    /// The URL of the repository at the absolute `path`, as clients would use it.
    pub fn url(path: &Path) -> String {
        format!("ssh://testhost{}", path.display())
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}
