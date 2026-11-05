//! Check the objects of a received pack and say what Git's `unpack-objects` or `index-pack` would print about them.
use std::ops::ControlFlow;

use gix_hash::oid;
use gix_object::{Find, FindHeader, Kind, bstr::BString};

use crate::{Checker, Diagnostic, Options, Refusal, Severity};

/// The program of Git whose output is reproduced, which phrases some of what it says differently.
///
/// `receive-pack` and `fetch` run `unpack-objects` for packs with fewer objects than the unpack limit, and `index-pack`
/// for all others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    /// `git unpack-objects --strict`.
    UnpackObjects,
    /// `git index-pack --strict`.
    IndexPack,
}

/// What came out of [checking a received pack](Transfer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// `true` if the pack is acceptable, which can go along with warnings in the [`report`](Self::report).
    pub accepted: bool,
    /// What Git prints about the objects, one line per problem, with the prefix `error: `, `warning: ` or `fatal: `
    /// and a newline at the end of each, and with control characters other than tab and newline replaced with `?` as Git does.
    pub report: BString,
}

/// Check the objects of one received pack like `git unpack-objects --strict` or `git index-pack --strict` do, and
/// collect what they print.
///
/// Feed the objects in the order of the pack to [`object()`](Self::object()) until it asks to stop, then call
/// [`finish()`](Self::finish()). Objects that complete a thin pack aren't part of what was received and must not be passed.
#[derive(Debug)]
pub struct Transfer {
    checker: Checker,
    program: Program,
    report: BString,
    refused: bool,
}

impl Transfer {
    /// Create a check for objects of `hash_kind` with `options`.
    pub fn new(options: Options, hash_kind: gix_hash::Kind, program: Program) -> Self {
        Transfer {
            checker: match program {
                Program::UnpackObjects => Checker::new(options, hash_kind).with_deferred_blob_checks(),
                Program::IndexPack => Checker::new(options, hash_kind),
            },
            program,
            report: BString::default(),
            refused: false,
        }
    }

    /// Treat the commits in `boundary` as if they had no parents when the links are checked, as those of a shallow
    /// repository.
    pub fn with_shallow(mut self, boundary: impl IntoIterator<Item = gix_hash::ObjectId>) -> Self {
        self.checker = self.checker.with_shallow(boundary);
        self
    }

    /// Return `true` if [`object()`](Self::object()) needs the data of the object `id` which has `kind` and `size` bytes.
    /// Only some blobs don't, and they can be passed with `None`.
    pub fn needs_content(&self, id: &oid, kind: Kind, size: u64) -> bool {
        kind != Kind::Blob || self.checker.needs_blob_content(id, size)
    }

    /// Check the received object `id` of `kind` with `size` bytes, which has `data` unless it's [not needed](Self::needs_content()).
    ///
    /// Return [`ControlFlow::Break`] if the pack is refused and checking can stop, as it does in Git.
    pub fn object(&mut self, id: &oid, kind: Kind, size: u64, data: Option<&[u8]>) -> ControlFlow<()> {
        let mut out = Vec::new();
        let result = self.checker.check(id, kind, size, data, &mut out);
        self.render(&out);
        match result {
            Ok(false) => ControlFlow::Continue(()),
            Ok(true) => {
                self.refuse("fsck error in packed object");
                ControlFlow::Break(())
            }
            Err(refusal) => {
                if let Some(detail) = refusal.detail() {
                    self.line("error: ", detail.as_bytes());
                }
                let text = match (&refusal, self.program) {
                    (Refusal::Link(_), Program::UnpackObjects) => "object of unexpected type".to_owned(),
                    (Refusal::BrokenLinks { oid }, Program::UnpackObjects) => {
                        format!("Error on reachable objects of {oid}")
                    }
                    _ => refusal.to_string(),
                };
                self.refuse(&text);
                ControlFlow::Break(())
            }
        }
    }

    /// Check what is left once all objects were seen, which is the links between them and the objects that were already there,
    /// found through `find`. It has to find the objects of the pack as well, in addition to those of the repository.
    pub fn finish<F>(mut self, find: &F) -> Outcome
    where
        F: Find + FindHeader + ?Sized,
    {
        if !self.refused {
            let mut out = Vec::new();
            match self.checker.finish(find, &mut out) {
                Err(err) => {
                    let text = match self.program {
                        Program::UnpackObjects => "object of unexpected type".to_owned(),
                        Program::IndexPack => err.to_string(),
                    };
                    self.refuse(&text);
                }
                Ok(failed) => {
                    self.render(&out);
                    if failed {
                        self.refuse("fsck error in pack objects");
                    }
                }
            }
        }
        Outcome {
            accepted: !self.refused,
            report: self.report,
        }
    }

    fn refuse(&mut self, fatal: &str) {
        self.refused = true;
        self.line("fatal: ", fatal.as_bytes());
    }

    fn render(&mut self, diagnostics: &[Diagnostic]) {
        for diagnostic in diagnostics {
            match diagnostic {
                Diagnostic::Note(text) => self.line("error: ", text.as_bytes()),
                Diagnostic::Report(report) => {
                    let mut text = format!("object {}: {}: ", report.oid, report.id).into_bytes();
                    text.extend_from_slice(&report.message);
                    let prefix = if report.severity == Severity::Error {
                        "error: "
                    } else {
                        "warning: "
                    };
                    self.line(prefix, &text);
                }
            }
        }
    }

    /// Add a line like Git's `error()`, `warning()` and `die()` print it: control characters other than tab and newline are shown as `?`.
    fn line(&mut self, prefix: &str, text: &[u8]) {
        self.report.extend_from_slice(prefix.as_bytes());
        self.report.extend(text.iter().map(|b| {
            if b.is_ascii_control() && !matches!(b, b'\t' | b'\n') {
                b'?'
            } else {
                *b
            }
        }));
        self.report.push(b'\n');
    }
}
