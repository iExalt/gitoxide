//! A native implementation of the `store` credential helper that comes with Git as `git credential-store`.
//!
//! It keeps one credential per line as a URL like `https://user:password@example.com/path`, in the files that it is
//! given or in `~/.git-credentials` and `$XDG_CONFIG_HOME/git/credentials` by default. It follows `credential-store.c`
//! of Git, with its way of parsing and encoding URLs, so that files are interchangeable between the two.
//!
//! As any `credential.helper` named `store` is this implementation, running it never involves `git`.
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use bstr::{BStr, BString, ByteSlice, ByteVec};
use gix_error::{Result, ResultExt, bail, message, validation};

use crate::{
    helper::Action,
    program::Environment,
    protocol::{Context, ContextOptions},
};

/// The files of a `store` helper, with which to [get](Store::get()), [store](Store::store()) and [erase](Store::erase()) credentials.
#[derive(Debug, Clone)]
pub struct Store {
    files: Vec<PathBuf>,
    lock_timeout: Duration,
}

/// Creation and configuration
impl Store {
    /// Use only `file`, which is what `git credential-store --file <file>` does.
    pub fn at_file(file: impl Into<PathBuf>) -> Self {
        Store {
            files: vec![file.into()],
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    /// Use the default locations, which are `~/.git-credentials` and then `$XDG_CONFIG_HOME/git/credentials`, with `.config` in the home
    /// directory standing in for `XDG_CONFIG_HOME` if it isn't set.
    ///
    /// Fails if `environment` has neither a home directory nor `XDG_CONFIG_HOME`, in which case a file has to be chosen with
    /// [`at_file()`](Self::at_file()).
    pub fn at_default_locations(environment: &Environment) -> Result<Self> {
        let mut files = Vec::new();
        if let Some(home) = &environment.home {
            files.push(home.join(".git-credentials"));
        }
        match (&environment.xdg_config_home, &environment.home) {
            (Some(xdg), _) => files.push(xdg.join("git").join("credentials")),
            (None, Some(home)) => files.push(home.join(".config").join("git").join("credentials")),
            (None, None) => {}
        }
        if files.is_empty() {
            bail!(validation(
                "Unable to set up default path for the credential store, as there is neither a home directory nor XDG_CONFIG_HOME; use a file"
            ));
        }
        Ok(Store {
            files,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        })
    }

    /// Wait this long for a lock on a credential file, which is one second by default and what `credentialStore.lockTimeoutMs` configures in Git.
    /// That configuration isn't read by `gix` yet.
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    /// The files in the order in which they are consulted.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }
}

const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(1);

/// The operations of the credential helper protocol
impl Store {
    /// Return the username and password of the first stored credential that matches `want`, searching the files in order.
    ///
    /// All fields of `want` that are set need to equal those of the stored entry, so a `want` without a username finds
    /// any username. Unreadable files are skipped, and so are entries that aren't complete credentials.
    pub fn get(&self, want: &Context) -> Result<Option<(BString, BString)>> {
        let want = Fields::of(want)?;
        for file in &self.files {
            let Some(content) = read_file(file)? else { continue };
            for line in content.split(|b| *b == b'\n') {
                if let Some(entry) = Fields::from_url(line.as_bstr())
                    && let (Some(username), Some(password)) = (&entry.username, &entry.password)
                    && want.matches(&entry, false)
                {
                    return Ok(Some((username.clone(), password.clone())));
                }
            }
        }
        Ok(None)
    }

    /// Store `credential`, which needs a protocol, a host or path, a username and a password, or else nothing happens.
    ///
    /// The credential is added to the first file that exists, or else the first file is created. Other entries
    /// that [match](Self::get()) it are replaced.
    pub fn store(&self, credential: &Context) -> Result<()> {
        let credential = Fields::of(credential)?;
        let (Some(protocol), Some(username), Some(password)) =
            (&credential.protocol, &credential.username, &credential.password)
        else {
            return Ok(());
        };
        if credential.host.is_none() && credential.path.is_none() {
            return Ok(());
        }
        let Some(file) = self.files.iter().find(|file| file.exists()).or(self.files.first()) else {
            return Ok(());
        };

        let mut line = BString::from(protocol.as_slice());
        line.push_str("://");
        urlencode(&mut line, username, is_unreserved);
        line.push(b':');
        urlencode(&mut line, password, is_unreserved);
        line.push(b'@');
        if let Some(host) = &credential.host {
            urlencode(&mut line, host, is_unreserved);
        }
        if let Some(path) = &credential.path {
            line.push(b'/');
            urlencode(&mut line, path, is_reserved_or_unreserved);
        }
        self.rewrite(file, Some(line.as_bstr()), &credential, false)
    }

    /// Remove the entries that match `credential`, including its password, from all files that exist.
    ///
    /// A `credential` without protocol, host, path and username is ignored, as it would otherwise match everything.
    pub fn erase(&self, credential: &Context) -> Result<()> {
        let credential = Fields::of(credential)?;
        if credential.protocol.is_none()
            && credential.host.is_none()
            && credential.path.is_none()
            && credential.username.is_none()
        {
            return Ok(());
        }
        for file in self.files.iter().filter(|file| file.exists()) {
            self.rewrite(file, None, &credential, true)?;
        }
        Ok(())
    }

    /// Perform `action` like the helper program does, with the output of `get` being what is printed.
    pub(crate) fn invoke(&self, action: &Action) -> Result<Option<Vec<u8>>> {
        let options = ContextOptions {
            protect_protocol: false,
        };
        match action {
            Action::Get(context) => {
                let mut out = Vec::new();
                if let Some((username, password)) = self.get(context)? {
                    out.extend_from_slice(b"username=");
                    out.extend_from_slice(&username);
                    out.extend_from_slice(b"\npassword=");
                    out.extend_from_slice(&password);
                    out.push(b'\n');
                }
                Ok(Some(out))
            }
            Action::Store(payload) => {
                self.store(&Context::from_bytes(payload, options)?)?;
                Ok(None)
            }
            Action::Erase(payload) => {
                self.erase(&Context::from_bytes(payload, options)?)?;
                Ok(None)
            }
        }
    }
}

impl Store {
    /// Write `first` if there is a line, followed by all lines in `file` that don't match `credential`, to a lock file which then replaces `file`.
    fn rewrite(&self, file: &Path, first: Option<&BStr>, credential: &Fields, match_password: bool) -> Result<()> {
        let mut lock = gix_lock::File::acquire(
            file,
            self.lock_timeout.into(),
            None,
            0,
            Some(&gix_lock::acquire::resolve_symlink),
        )
        .or_raise(|| {
            message!(
                "Unable to get credential storage lock for {} in {:?}",
                file.display(),
                self.lock_timeout
            )
        })?;
        // Git runs with a umask that makes credential files private.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            lock.with_mut(|lock| lock.set_permissions(std::fs::Permissions::from_mode(0o600)))
                .or_raise(|| message!("Could not make the lock of {} private", file.display()))?;
        }
        let mut out = Vec::new();
        if let Some(first) = first {
            out.extend_from_slice(first);
            out.push(b'\n');
        }
        if let Some(content) = read_file(file)? {
            for line in lines(&content) {
                let is_match = Fields::from_url(line.as_bstr()).is_some_and(|entry| {
                    entry.username.is_some() && entry.password.is_some() && credential.matches(&entry, match_password)
                });
                if !is_match {
                    out.extend_from_slice(line);
                    out.push(b'\n');
                }
            }
        }
        lock.write_all(&out)
            .or_raise(|| message!("Could not write the credential store at {}", file.display()))?;
        lock.commit()
            .map_err(|err| err.error)
            .or_raise(|| message!("Unable to write credential store {}", file.display()))?;
        Ok(())
    }
}

/// Split `content` into lines like `git` does when reading credential files: at each `\n`, with a final line not needing one.
fn lines(content: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut lines = content.split(|b| *b == b'\n').peekable();
    std::iter::from_fn(move || {
        let line = lines.next()?;
        (lines.peek().is_some() || !line.is_empty()).then_some(line)
    })
}

/// Read `file`, treating a missing file or one that can't be accessed as having no content, like Git does.
fn read_file(file: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(file) {
        Ok(content) => Ok(Some(content)),
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(None)
        }
        Err(err) => Err(err).or_raise(|| message!("unable to open {}", file.display())),
    }
}

/// The fields of a credential that identify it, in the form in which they are stored and compared: as bytes.
#[derive(Default, Debug)]
struct Fields {
    protocol: Option<BString>,
    host: Option<BString>,
    path: Option<BString>,
    username: Option<BString>,
    password: Option<BString>,
}

impl Fields {
    /// Take the fields of `context`, with its URL providing the ones that aren't set explicitly, as it would when read by the credential protocol.
    fn of(context: &Context) -> Result<Self> {
        let mut fields = match &context.url {
            Some(url) => Fields::from_url(url.as_bstr())
                .ok_or_else(|| gix_error::validation(format!("credential url cannot be parsed: {url}")))?,
            None => Fields::default(),
        };
        let bytes = |value: &Option<String>| value.as_deref().map(BString::from);
        for (target, value) in [
            (&mut fields.protocol, bytes(&context.protocol)),
            (&mut fields.host, bytes(&context.host)),
            (&mut fields.username, bytes(&context.username)),
            (&mut fields.password, bytes(&context.password)),
            (&mut fields.path, context.path.clone()),
        ] {
            if value.is_some() {
                *target = value;
            }
        }
        Ok(fields)
    }

    /// Parse `url` like `credential_from_url_gently()` does for complete URLs, which is how Git reads the lines of credential files.
    /// Return `None` if there is no scheme, or if a component contains a newline.
    fn from_url(url: &BStr) -> Option<Self> {
        let proto_end = url.find(b"://").filter(|pos| *pos > 0)?;
        let proto_end = Some(proto_end);
        let cp = proto_end.map_or(0, |pos| pos + 3);
        let rest = &url[cp..];
        let at = rest.find_byte(b'@').map(|pos| cp + pos);
        let colon = rest.find_byte(b':').map(|pos| cp + pos);
        let slash = cp + rest.find_byteset(b"/?#").unwrap_or(rest.len());

        let mut fields = Fields::default();
        let host_start = match at {
            Some(at) if slash > at => match colon {
                Some(colon) if at > colon => {
                    fields.username = Some(decode(&url[cp..colon]));
                    fields.password = Some(decode(&url[colon + 1..at]));
                    at + 1
                }
                _ => {
                    fields.username = Some(decode(&url[cp..at]));
                    at + 1
                }
            },
            _ => cp,
        };
        if let Some(proto_end) = proto_end {
            fields.protocol = Some(url[..proto_end].into());
        }
        fields.host = Some(decode(&url[host_start..slash]));
        let path = url[slash..].trim_start_with(|c| c == '/');
        if !path.is_empty() {
            let mut path = decode(path);
            while path.len() > 1 && path.ends_with(b"/") {
                path.pop();
            }
            fields.path = Some(path);
        }
        let has_newline = [
            &fields.username,
            &fields.password,
            &fields.protocol,
            &fields.host,
            &fields.path,
        ]
        .into_iter()
        .flatten()
        .any(|value| value.contains(&b'\n'));
        (!has_newline).then_some(fields)
    }

    /// Return `true` if all fields set in `self` equal the ones in `have`, with the password counting only if `match_password` is set.
    fn matches(&self, have: &Fields, match_password: bool) -> bool {
        let check = |want: &Option<BString>, have: &Option<BString>| match want {
            None => true,
            Some(want) => have.as_ref() == Some(want),
        };
        check(&self.protocol, &have.protocol)
            && check(&self.host, &have.host)
            && check(&self.path, &have.path)
            && check(&self.username, &have.username)
            && (!match_password || check(&self.password, &have.password))
    }
}

/// Decode `%XX` sequences like `url_decode_mem()` does: `%00` and malformed sequences stay as they are, and a NUL ends the value.
fn decode(input: &[u8]) -> BString {
    let mut out = BString::default();
    let mut rest = input;
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == 0 {
            break;
        }
        if byte == b'%'
            && let [hi, lo, ..] = tail
            && let (Some(hi), Some(lo)) = ((*hi as char).to_digit(16), (*lo as char).to_digit(16))
            && let value @ 1.. = (hi * 16 + lo) as u8
        {
            out.push(value);
            rest = &tail[2..];
            continue;
        }
        out.push(byte);
        rest = tail;
    }
    out
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

fn is_reserved_or_unreserved(byte: u8) -> bool {
    is_unreserved(byte)
        || matches!(
            byte,
            b'!' | b'*'
                | b'\''
                | b'('
                | b')'
                | b';'
                | b':'
                | b'@'
                | b'&'
                | b'='
                | b'+'
                | b'$'
                | b','
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
        )
}

/// Percent-encode `input` into `out`, with lower-case hex digits as Git does.
fn urlencode(out: &mut BString, input: &[u8], keep: fn(u8) -> bool) {
    for &byte in input {
        if keep(byte) {
            out.push(byte);
        } else {
            out.push_str(format!("%{byte:02x}"));
        }
    }
}

/// Interpret the `arguments` of a `store` helper definition, which is what follows the word `store`, and return the file given with `--file`.
///
/// This follows how a shell would pass them to `git credential-store`: words are split at whitespace, with single and double quotes and
/// backslashes protecting characters, and an unquoted `~` or `~/` at the start of the file is the home directory when it is
/// the separate word after `--file`. Options may be abbreviated like `--fi`, as Git does, and `--file=<path>` isn't expanded.
/// Everything that a shell would expand, like `$HOME`, is rejected rather than used literally.
pub(crate) fn file_from_arguments(arguments: &BStr, home: Option<&Path>) -> Result<Option<PathBuf>> {
    let words = shell_words(arguments)?;
    let mut words = words.iter();
    let Some(first) = words.next() else {
        return Ok(None);
    };
    let is_file_option = |word: &str| word.len() >= 3 && "--file".starts_with(word);
    let (value, expand_tilde) = if let Some(inline) = first
        .text
        .split_once('=')
        .filter(|(option, _)| is_file_option(option))
        .map(|(_, value)| value)
    {
        (inline.to_owned(), false)
    } else if is_file_option(&first.text) {
        let value = words
            .next()
            .ok_or_else(|| validation("The --file option of the store helper needs a value"))?;
        (value.text.clone(), value.starts_with_tilde)
    } else {
        bail!(validation(format!(
            "The store helper only supports the --file option, but got {:?}",
            first.text
        )));
    };
    if let Some(extra) = words.next() {
        bail!(validation(format!(
            "The store helper doesn't support the argument {:?}",
            extra.text
        )));
    }
    let path = if expand_tilde && (value == "~" || value.starts_with("~/")) {
        let home =
            home.ok_or_else(|| validation("The home directory is needed to expand ~ in the file of the store helper"))?;
        home.join(value[1..].trim_start_matches('/'))
    } else if expand_tilde && value.starts_with('~') {
        bail!(validation(format!(
            "The store helper can't expand the home directory of another user in {value:?}"
        )));
    } else {
        PathBuf::from(value)
    };
    Ok(Some(path))
}

struct Word {
    text: String,
    /// `true` if the word started with a `~` that wasn't quoted.
    starts_with_tilde: bool,
}

/// Split `input` into words like `sh`, but refuse everything a shell would expand, like variables, command substitution and globs.
fn shell_words(input: &BStr) -> Result<Vec<Word>> {
    let text = input
        .to_str()
        .map_err(|_| validation("The arguments of the store helper must be valid UTF-8"))?;
    let unsupported = |what: &str| {
        validation(format!(
            "The shell syntax {what} in the arguments of the store helper is not supported: {text:?}"
        ))
    };
    let mut words = Vec::new();
    let mut current: Option<Word> = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            c if c.is_whitespace() => words.extend(current.take()),
            '\'' => {
                let word = current.get_or_insert_with(|| Word {
                    text: String::new(),
                    starts_with_tilde: false,
                });
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.text.push(c),
                        None => bail!(validation(
                            "Unterminated single quote in the arguments of the store helper"
                        )),
                    }
                }
            }
            '"' => {
                let word = current.get_or_insert_with(|| Word {
                    text: String::new(),
                    starts_with_tilde: false,
                });
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.text.push(c),
                            Some(c) => {
                                word.text.push('\\');
                                word.text.push(c);
                            }
                            None => bail!(validation(
                                "Unterminated double quote in the arguments of the store helper"
                            )),
                        },
                        Some('$' | '`') => bail!(unsupported("expansion")),
                        Some(c) => word.text.push(c),
                        None => bail!(validation(
                            "Unterminated double quote in the arguments of the store helper"
                        )),
                    }
                }
            }
            '\\' => {
                let word = current.get_or_insert_with(|| Word {
                    text: String::new(),
                    starts_with_tilde: false,
                });
                match chars.next() {
                    Some('\n') => {}
                    Some(c) => word.text.push(c),
                    None => bail!(validation("A trailing backslash in the arguments of the store helper")),
                }
            }
            '$' | '`' => bail!(unsupported("expansion")),
            '*' | '?' | '[' | '{' => bail!(unsupported("globbing")),
            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '#' => bail!(unsupported("operators and comments")),
            c => {
                let word = current.get_or_insert_with(|| Word {
                    text: String::new(),
                    starts_with_tilde: c == '~',
                });
                word.text.push(c);
            }
        }
    }
    words.extend(current);
    Ok(words)
}
