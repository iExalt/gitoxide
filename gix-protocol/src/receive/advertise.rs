//! The advertisement a receiving side sends first.
use bstr::BString;
use gix_error::{Result, bail, validation};
use gix_hash::ObjectId;
use gix_transport::Protocol;

use super::Capabilities;

/// The pkt-line payload limit, excluding the four length bytes.
const MAX_PACKET_DATA_LEN: usize = 65516;

/// A reference as it is shown to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The name as it is sent. For references outside the namespace of the repository, which Git advertises so that clients
    /// can avoid sending objects the repository has, as well as for the references of alternates, this is `.have`.
    pub name: BString,
    /// The object the reference points to.
    pub id: ObjectId,
}

impl Reference {
    /// Create a reference named `name` that points to `id`.
    pub fn new(name: impl Into<BString>, id: impl Into<ObjectId>) -> Self {
        Reference {
            name: name.into(),
            id: id.into(),
        }
    }
}

/// Everything a client learns before it sends commands.
#[derive(Debug, Clone)]
pub struct Advertisement {
    /// The capabilities to announce, which are attached to the first reference.
    pub capabilities: Capabilities,
    /// The references to show, in the order they are shown.
    pub references: Vec<Reference>,
    /// The ids of commits at the shallow boundary of the repository.
    pub shallow: Vec<ObjectId>,
}

impl Advertisement {
    /// Encode the advertisement as `git receive-pack` writes it, including the flush packet at its end.
    ///
    /// If there are no references, a reference called `capabilities^{}` with the null id carries the capabilities.
    /// `version` is what the client asked for. Like in Git, version 1 adds a `version 1` line before everything,
    /// and version 2, which `receive-pack` doesn't support, is answered like version 0, without a version line.
    ///
    /// Fail if a value is invalid, or if a line doesn't fit into a packet.
    pub fn encode(&self, version: Protocol) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        if version == Protocol::V1 {
            write_line(&mut out, b"version 1\n")?;
        }
        let capabilities = self.capabilities.to_line()?;
        let null_id = self.capabilities.object_format.null();
        let empty;
        let references = if self.references.is_empty() {
            empty = [Reference::new("capabilities^{}", null_id)];
            &empty[..]
        } else {
            &self.references[..]
        };
        for (index, reference) in references.iter().enumerate() {
            let mut line = BString::from(format!("{} ", reference.id));
            line.extend_from_slice(&reference.name);
            if index == 0 {
                line.push(0);
                line.extend_from_slice(capabilities.as_bytes());
            }
            line.push(b'\n');
            write_line(&mut out, &line)?;
        }
        for id in &self.shallow {
            write_line(&mut out, format!("shallow {id}\n").as_bytes())?;
        }
        out.extend_from_slice(b"0000");
        Ok(out)
    }
}

pub(super) fn write_line(out: &mut Vec<u8>, line: &[u8]) -> Result {
    if line.len() > MAX_PACKET_DATA_LEN {
        bail!(validation(format!(
            "A line of {} bytes exceeds the packet line limit of {MAX_PACKET_DATA_LEN}",
            line.len()
        )));
    }
    out.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    out.extend_from_slice(line);
    Ok(())
}
