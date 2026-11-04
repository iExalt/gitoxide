//! The status report a receiving side sends after handling a request.
use bstr::{BString, ByteSlice};
use gix_error::Result;

use super::advertise::write_line;
use crate::push::{StatusFormat, report::Update};

/// What happened to a single command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandReport {
    /// The name of the reference as it was sent.
    pub ref_name: BString,
    /// The updates that were performed, or the reason the command was rejected.
    ///
    /// Updates are only reported with [`StatusFormat::V2`], and only if the performed update differs from the command, for
    /// instance because a hook redirected it. Otherwise, the vector is empty. An accepted command is reported once, followed by
    /// one more time for each update after the first.
    pub outcome: std::result::Result<Vec<Update>, BString>,
}

/// The report of a receiving side, which Git calls `report-status` and `report-status-v2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The reason unpacking the pack failed, or `None` if it succeeded. An empty reason counts as success, like in Git.
    pub unpack: Option<BString>,
    /// One entry for each command, in the order they were sent.
    pub commands: Vec<CommandReport>,
}

impl Report {
    /// Encode the report as the packet lines Git sends, including the flush packet at the end.
    ///
    /// If side bands are used, pass the result to [`side_band::encode()`](super::side_band::encode()).
    ///
    /// Fail if a line doesn't fit into a packet.
    pub fn encode(&self, format: StatusFormat) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let line = |out: &mut Vec<u8>, parts: &[&[u8]]| write_line(out, &parts.concat());
        let unpack = self.unpack.as_ref().filter(|reason| !reason.is_empty());
        line(
            &mut out,
            &[b"unpack ", unpack.map_or(&b"ok"[..], |reason| reason.as_bytes()), b"\n"],
        )?;
        for command in &self.commands {
            match &command.outcome {
                Err(reason) => line(&mut out, &[b"ng ", &command.ref_name, b" ", reason, b"\n"])?,
                Ok(updates) => {
                    line(&mut out, &[b"ok ", &command.ref_name, b"\n"])?;
                    if format == StatusFormat::V2 {
                        for (index, update) in updates.iter().enumerate() {
                            if index > 0 {
                                line(&mut out, &[b"ok ", &command.ref_name, b"\n"])?;
                            }
                            if let Some(name) = &update.ref_name {
                                line(&mut out, &[b"option refname ", name, b"\n"])?;
                            }
                            if let Some(id) = &update.old_id {
                                line(&mut out, &[b"option old-oid ", id.to_string().as_bytes(), b"\n"])?;
                            }
                            if let Some(id) = &update.new_id {
                                line(&mut out, &[b"option new-oid ", id.to_string().as_bytes(), b"\n"])?;
                            }
                            if update.forced {
                                line(&mut out, &[b"option forced-update\n"])?;
                            }
                        }
                    }
                }
            }
        }
        out.extend_from_slice(b"0000");
        Ok(out)
    }
}
