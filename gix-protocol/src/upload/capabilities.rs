use gix_error::{Result, bail, validation};

use super::response::write_line;

/// What an upload side announces to clients before they ask anything, which is what `git upload-pack` announces in the same order.
///
/// `ls-refs` with `unborn`, `fetch`, `server-option` and `object-format` are always announced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// The hash of the repository, announced as `object-format`.
    pub object_format: gix_hash::Kind,
    /// The user agent to announce as `agent=<agent>`, which looks like `git/2.56.0-gitoxide`.
    pub agent: String,
    /// The identifier to announce as `session-id=<id>`, which is controlled by `transfer.advertiseSID`.
    pub session_id: Option<String>,
    /// Announce `unborn` as feature of `ls-refs`, which `lsrefs.unborn=advertise` controls and which is on by default.
    pub unborn: bool,
}

impl Capabilities {
    /// Create capabilities for a repository using `object_format` which are announced by `agent`, and all that Git offers by default.
    pub fn new(object_format: gix_hash::Kind, agent: impl Into<String>) -> Self {
        Capabilities {
            object_format,
            agent: agent.into(),
            session_id: None,
            unborn: true,
        }
    }

    /// Encode the advertisement as `git upload-pack` writes it for a client that asked for protocol version 2, including the flush
    /// packet at its end.
    ///
    /// Fail if a value can't be represented, which is the case if it is empty or contains whitespace or a NUL byte.
    pub fn encode(&self) -> Result<Vec<u8>> {
        fn check(name: &str, value: &str) -> Result {
            if value.is_empty() || value.contains(|c: char| c.is_whitespace() || c == '\0') {
                bail!(validation(format!(
                    "The value {value:?} of capability '{name}' must be non-empty and contain no whitespace or NUL"
                )));
            }
            Ok(())
        }
        let mut out = Vec::new();
        write_line(&mut out, b"version 2\n")?;
        check("agent", &self.agent)?;
        write_line(&mut out, format!("agent={}\n", self.agent).as_bytes())?;
        write_line(
            &mut out,
            if self.unborn { "ls-refs=unborn\n" } else { "ls-refs\n" }.as_bytes(),
        )?;
        write_line(&mut out, b"fetch\n")?;
        write_line(&mut out, b"server-option\n")?;
        write_line(&mut out, format!("object-format={}\n", self.object_format).as_bytes())?;
        if let Some(id) = &self.session_id {
            check("session-id", id)?;
            write_line(&mut out, format!("session-id={id}\n").as_bytes())?;
        }
        out.extend_from_slice(b"0000");
        Ok(out)
    }
}
