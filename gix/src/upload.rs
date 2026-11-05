//! Serve the references and objects of a repository on the local machine to clients, like `git upload-pack` does with protocol
//! version 2, without running Git.
//!
//! Create a [`Server`] with [`Repository::upload()`] and let it [serve](Server::serve()) a client that talks to a reader and a
//! writer, or answer single requests with [`Server::ls_refs()`] and [`Server::fetch()`].
//!
//! ## Compatibility with Git
//!
//! * References are listed like `ls-refs` does, including `hideRefs`, the namespace, `HEAD` of the namespace, and
//!   a `HEAD` that points to a reference that doesn't exist yet.
//! * `fetch` sends a pack with what the client wants and doesn't have, which is a superset of what Git sends, see
//!   [`gix_pack::data::output::missing`]. Like Git, it doesn't check that the objects it is asked for were advertised, so hidden
//!   references don't protect the objects they point to.
//! * Negotiation takes as many rounds as the client needs: the `have` lines of each request are acknowledged, and the pack is sent
//!   once the client says it is `done`, or the server finds that it has a common commit for everything that is wanted.
//! * Requests for shallow clones, and for what the configuration of the repository would enable in Git, like filters, are refused with
//!   an error packet or at [`Repository::upload()`], as are the settings that can't be honored, instead of serving less than Git would.
//!   `thin-pack` is ignored, which makes the pack bigger but not different for the client that asked.
use std::{
    io::{Read, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use gix_error::{ErrorExt, Result, ResultExt, bail, message, unsupported, validation};
use gix_hash::ObjectId;
use gix_protocol::upload::{Capabilities, Command, Fetch, LsRefs, read_request, response, write_advertisement};

use crate::{
    Repository,
    bstr::{BStr, BString, ByteSlice},
    served::{HiddenRefs, Role, refuse_unsupported_repository},
};

/// What `lsrefs.unborn` allows a client to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unborn {
    Advertise,
    Allow,
    Ignore,
}

/// Serves a repository to clients like `git upload-pack` does.
pub struct Server<'repo> {
    repo: &'repo Repository,
    /// What references are looked up in: `repo` without its namespace, with the names of all references in full.
    global: Repository,
    /// What names of the namespace begin with, which is empty without one.
    prefix: BString,
    hidden: HiddenRefs,
    unborn: Unborn,
}

impl Repository {
    /// Create a [`Server`] that serves this repository like `git upload-pack` does, without running Git.
    ///
    /// Fail with an [unsupported](gix_error::Class::Unsupported) error if the configuration of the repository asks for something
    /// that isn't supported, instead of ignoring it. That's the case for shallow and partial repositories, for repositories that
    /// aren't owned by the user, and for what makes Git serve more than is supported: `uploadpack.allowFilter`,
    /// `uploadpack.allowRefInWant`, `uploadpack.allowSidebandAll`, `uploadpack.blobPackfileUri`, `uploadpack.packObjectsHook`,
    /// `uploadpack.advertiseBundleURIs`, `transfer.advertiseSID` and `transfer.advertiseObjectInfo`.
    pub fn upload(&self) -> crate::Result<Server<'_>> {
        refuse_unsupported_repository(self, Role::Upload)?;
        let config = self.config_snapshot();
        for (key, what) in [
            ("uploadpack.allowFilter", "filters"),
            ("uploadpack.allowRefInWant", "`want-ref`"),
            ("uploadpack.allowSidebandAll", "`sideband-all`"),
            ("uploadpack.advertiseBundleURIs", "advertising bundles"),
            ("transfer.advertiseSID", "advertising session ids"),
            ("transfer.advertiseObjectInfo", "`object-info`"),
        ] {
            if config.boolean(key).unwrap_or(false) {
                bail!(unsupported(format!("{key} isn't supported, as {what} aren't")));
            }
        }
        for (key, what) in [
            ("uploadpack.packObjectsHook", "running a program to create packs"),
            ("uploadpack.blobPackfileUri", "packfile URIs"),
        ] {
            if config.string(key).is_some() {
                bail!(unsupported(format!("{key} isn't supported, as {what} isn't")));
            }
        }
        let unborn = match config.string("lsrefs.unborn") {
            None => Unborn::Advertise,
            Some(value) => match value.as_bstr().to_str_lossy().as_ref() {
                "advertise" => Unborn::Advertise,
                "allow" => Unborn::Allow,
                "ignore" => Unborn::Ignore,
                other => bail!(validation(format!("invalid value for 'lsrefs.unborn': '{other}'"))),
            },
        };
        let mut global = self.clone();
        let prefix = global
            .clear_namespace()
            .map_or_else(BString::default, |namespace| namespace.as_bstr().to_owned());
        Ok(Server {
            repo: self,
            global,
            prefix,
            hidden: HiddenRefs::from_config(config.plumbing(), "uploadpack"),
            unborn,
        })
    }
}

/// How a reference ends: with an object, or with nothing if it points to a reference that doesn't exist.
struct Resolved {
    id: Option<ObjectId>,
    /// If the reference was symbolic, the name of the reference at the end of the chain, which is its target.
    symbolic_target: Option<BString>,
}

fn resolve(mut reference: crate::Reference<'_>) -> Resolved {
    let mut symbolic_target = None;
    loop {
        match reference.target() {
            gix_ref::TargetRef::Object(id) => {
                return Resolved {
                    id: Some(id.to_owned()),
                    symbolic_target,
                };
            }
            gix_ref::TargetRef::Symbolic(name) => {
                symbolic_target = Some(name.as_bstr().to_owned());
                match reference.follow() {
                    Some(Ok(next)) => reference = next,
                    _ => {
                        return Resolved {
                            id: None,
                            symbolic_target,
                        };
                    }
                }
            }
        }
    }
}

impl Server<'_> {
    /// The name of `full` in the namespace of the server, or `None` if it is in another one.
    fn strip<'n>(&self, full: &'n BStr) -> Option<&'n BStr> {
        full.strip_prefix(self.prefix.as_slice()).map(ByteSlice::as_bstr)
    }

    /// What to announce to clients, which is what `git upload-pack` announces for protocol version 2.
    pub fn capabilities(&self) -> Capabilities {
        let mut capabilities = Capabilities::new(self.repo.object_hash(), gix_protocol::agent(crate::env::agent()));
        capabilities.unborn = self.unborn == Unborn::Advertise;
        capabilities
    }

    /// Return the lines of the answer to `ls-refs` with `arguments`, in the order Git lists references.
    pub fn ls_refs(&self, arguments: &LsRefs) -> Result<Vec<response::Reference>> {
        let wanted = |name: &BStr| {
            arguments.ref_prefixes.is_empty()
                || arguments
                    .ref_prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix.as_slice()))
        };
        let mut out = Vec::new();
        let peeled = |id: ObjectId| -> Result<Option<ObjectId>> {
            if !arguments.peel {
                return Ok(None);
            }
            let kind = self
                .global
                .find_header(id)
                .or_raise(|| message("Could not read the header of a reference's object"))?
                .kind();
            if kind != gix_object::Kind::Tag {
                return Ok(None);
            }
            let object = self
                .global
                .find_object(id)
                .or_raise(|| message("Could not read a tag"))?;
            Ok(Some(
                object
                    .peel_tags_to_end()
                    .or_raise(|| message("Could not peel a tag"))?
                    .id,
            ))
        };

        // `HEAD` of the namespace comes first, and is shown even if it's unborn if the client asked for that.
        let head_name = BString::from([self.prefix.as_slice(), b"HEAD"].concat());
        if let Some(head) = self
            .global
            .try_find_reference(head_name.as_bstr())
            .or_raise(|| message("Could not read HEAD"))?
        {
            let resolved = resolve(head);
            let allow_unborn = arguments.unborn && arguments.symrefs && self.unborn != Unborn::Ignore;
            let shown = resolved.id.is_some() || (allow_unborn && resolved.symbolic_target.is_some());
            if shown && !self.hidden.is_hidden_in(Some("HEAD".into()), head_name.as_bstr()) && wanted("HEAD".into()) {
                out.push(response::Reference {
                    id: resolved.id,
                    name: "HEAD".into(),
                    symref_target: arguments
                        .symrefs
                        .then(|| {
                            resolved.symbolic_target.map(|target| {
                                self.strip(target.as_bstr())
                                    .map_or_else(|| target.clone(), ToOwned::to_owned)
                            })
                        })
                        .flatten(),
                    peeled: resolved.id.map(peeled).transpose()?.flatten(),
                });
            }
        }

        let mut found = Vec::new();
        for reference in self
            .global
            .references()
            .or_raise(|| message("Could not read the references"))?
            .all()
            .or_raise(|| message("Could not read the references"))?
        {
            let reference = reference.or_raise(|| message("Could not read a reference"))?;
            let full = reference.name().as_bstr().to_owned();
            // References of other namespaces are not ours. The `HEAD` of the namespace is, as it is below the namespace, and so Git
            // lists it a second time after showing it first, which is replicated.
            let Some(name) = self.strip(full.as_bstr()) else {
                continue;
            };
            if self.hidden.is_hidden_in(Some(name), full.as_bstr()) || !wanted(name) {
                continue;
            }
            let name = name.to_owned();
            let resolved = resolve(reference);
            // References that lead nowhere are skipped, as they are by Git.
            let Some(id) = resolved.id else { continue };
            found.push(response::Reference {
                id: Some(id),
                name,
                symref_target: arguments
                    .symrefs
                    .then(|| {
                        resolved.symbolic_target.map(|target| {
                            self.strip(target.as_bstr())
                                .map_or_else(|| target.clone(), ToOwned::to_owned)
                        })
                    })
                    .flatten(),
                peeled: peeled(id)?,
            });
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        out.extend(found);
        Ok(out)
    }

    /// Answer `request` by writing the pack with what it wants and doesn't have to `out`, in the way `fetch` does.
    ///
    /// Requests that can't be served are refused with an error packet, and an error. A request without wants gets no answer.
    pub fn fetch(&self, request: &Fetch, out: &mut dyn Write, interrupt: &AtomicBool) -> Result {
        let refuse = |out: &mut dyn Write, error: gix_error::Error, text: String| -> Result {
            // Whoever is on the other end can't do more than read this, so a failure to write it is of no interest.
            out.write_all(&response::error(&text)).and_then(|()| out.flush()).ok();
            Err(error)
        };
        for id in &request.wants {
            if !self.repo.has_object(id) {
                let text = format!("upload-pack: not our ref {id}");
                return refuse(out, validation(text.clone()).raise(), text);
            }
        }
        if !request.shallow_lines.is_empty() || request.wait_for_done {
            let text = "shallow clones and `wait-for-done` aren't supported".to_owned();
            return refuse(out, unsupported(text.clone()).raise(), text);
        }
        if request.wants.is_empty() {
            return Ok(());
        }
        if !request.haves.is_empty() && !request.done {
            // The client is still telling what it has. Everything is said again in each request, so nothing is remembered.
            let (acknowledged, known, oldest) = self.process_haves(&request.haves)?;
            let ready = self.ok_to_give_up(&request.wants, &acknowledged, &known, oldest)?;
            out.write_all(&response::acknowledgments(&acknowledged, ready)?)
                .and_then(|()| out.flush())
                .or_raise(|| message("Could not write the acknowledgments"))?;
            if !ready {
                return Ok(());
            }
        }

        let options = gix_pack::data::output::missing::Options {
            allow_ofs_delta: request.ofs_delta,
            ..Default::default()
        };
        let mut db = self
            .repo
            .objects
            .clone()
            .into_arc()
            .or_raise(|| message("Could not prepare the object database for reading"))?
            .into_inner();
        // The pack is written from locations of objects that were determined before, which must not go away if maintenance
        // replaces packs in the meantime.
        db.prevent_pack_unload();
        let mut counts = match gix_pack::data::output::missing::counts(
            db.clone(),
            &request.wants,
            &request.haves,
            &gix_utils::progress::Discard,
            interrupt,
            options,
        ) {
            Ok(counts) => counts,
            Err(err) => {
                let text = format!("upload-pack: could not determine which objects to send: {err}");
                return refuse(
                    out,
                    err.and_raise(message("Could not determine which objects to send")),
                    text,
                );
            }
        };
        if request.include_tag {
            self.add_tags(&mut counts)?;
        }
        out.write_all(response::packfile())
            .or_raise(|| message("Could not write the start of the pack"))?;
        let written = {
            let mut band = SideBandWriter::new(out, interrupt);
            gix_pack::data::output::missing::write_pack(db, counts, &mut band, self.repo.object_hash(), options)
                .map_err(|err| (err, "write the pack"))
                .and_then(|_| {
                    band.finish()
                        .map_err(|err| (err.and_raise(message("")), "write the end of the pack"))
                })
        };
        if let Err((err, what)) = written {
            // The pack can't be completed, so what the client has is cut. It learns why from the band for errors, which ends its wait.
            use gix_protocol::receive::side_band::{Channel, encode};
            let text = format!("upload-pack: could not {what}: {err:#}\n");
            out.write_all(&encode(Channel::Error, text.as_bytes()))
                .and_then(|()| out.flush())
                .ok();
            return Err(err.and_raise(message!("Could not {what}")));
        }
        out.write_all(b"0000")
            .and_then(|()| out.flush())
            .or_raise(|| message("Could not end the response"))
    }

    /// Add the annotated tags that point at something in the pack, as `git pack-objects` does for clients that ask with `include-tag`.
    ///
    /// Like Git, this looks at the tags of the repository as a whole and not at those of the namespace, and at hidden ones as well.
    fn add_tags(&self, counts: &mut Vec<gix_pack::data::output::Count>) -> Result {
        let mut packed: std::collections::HashSet<ObjectId> = counts.iter().map(|count| count.id).collect();
        for reference in self
            .global
            .references()
            .or_raise(|| message("Could not read the references"))?
            .tags()
            .or_raise(|| message("Could not read the tags"))?
        {
            let reference = reference.or_raise(|| message("Could not read a tag reference"))?;
            let Some(id) = resolve(reference).id else { continue };
            // The chain of tags, from the one the reference has to the object it finally points to.
            let mut chain = Vec::new();
            let mut current = id;
            loop {
                let object = self
                    .global
                    .find_object(current)
                    .or_raise(|| message("Could not read the object of a tag reference"))?;
                if object.kind != gix_object::Kind::Tag {
                    break;
                }
                chain.push(current);
                current = object
                    .into_tag()
                    .target_id()
                    .or_raise(|| message("Could not read a tag"))?
                    .detach();
            }
            if chain.is_empty() || !packed.contains(&current) {
                continue;
            }
            // Like Git, skip a chain whose outer tag is in the pack already, and otherwise add each tag that isn't.
            if packed.contains(&chain[0]) {
                continue;
            }
            for tag in chain {
                if packed.insert(tag) {
                    counts.push(gix_pack::data::output::Count::from_data(tag, None));
                }
            }
        }
        Ok(())
    }

    /// Read the `have` lines in order like `do_got_oid()` of Git does, and return what to acknowledge, which commits and their parents
    /// the client is known to have, and the date of the oldest commit among the `haves`.
    ///
    /// What the server doesn't have is skipped. The parents of a commit count as had from there on, so that a `have` that follows
    /// its child, which is how clients send them, or that is repeated, isn't acknowledged a second time.
    fn process_haves(
        &self,
        haves: &[ObjectId],
    ) -> Result<(Vec<ObjectId>, std::collections::HashSet<ObjectId>, Option<i64>)> {
        let mut acknowledged = Vec::new();
        let mut known = std::collections::HashSet::new();
        let mut oldest = None::<i64>;
        for id in haves {
            if !self.repo.has_object(id) {
                continue;
            }
            if let Ok(commit) = self.repo.find_commit(*id) {
                let seconds = commit
                    .time()
                    .or_raise(|| message("Could not read the date of a commit"))?
                    .seconds;
                oldest = Some(oldest.map_or(seconds, |oldest| oldest.min(seconds)));
                known.extend(commit.parent_ids().map(crate::Id::detach));
            }
            if known.insert(*id) {
                acknowledged.push(*id);
            }
        }
        Ok((acknowledged, known, oldest))
    }

    /// Return `true` if every want has an ancestor among `haves`, or isn't a commit, which is when Git stops negotiating and says it
    /// is ready to send the pack, as `ok_to_give_up()` does.
    ///
    /// Like in Git, commits older than the oldest of the `haves` are not looked at, as they can't lead to one of them.
    ///
    /// `acknowledged`, `known` and `oldest` are what [`process_haves()`](Self::process_haves()) returns. A want is looked at on its
    /// own, so the history that wants share is walked once for each of them, where Git shares the flags between wants.
    fn ok_to_give_up(
        &self,
        wants: &[ObjectId],
        acknowledged: &[ObjectId],
        known: &std::collections::HashSet<ObjectId>,
        oldest: Option<i64>,
    ) -> Result<bool> {
        if acknowledged.is_empty() {
            return Ok(false);
        }
        for want in wants {
            let object = self
                .repo
                .find_object(*want)
                .or_raise(|| message("Could not find a wanted object"))?;
            // Nothing can be said about what isn't a commit, so it doesn't stand in the way.
            let Ok(commit) = object.peel_tags_to_end().and_then(crate::Object::try_into_commit) else {
                continue;
            };
            let mut seen = std::collections::HashSet::new();
            let mut stack = vec![commit.id];
            let mut reached = false;
            while let Some(id) = stack.pop() {
                if known.contains(&id) {
                    reached = true;
                    break;
                }
                if !seen.insert(id) {
                    continue;
                }
                let commit = self
                    .repo
                    .find_commit(id)
                    .or_raise(|| message("Could not read a commit"))?;
                for parent in commit.parent_ids() {
                    let parent = parent.detach();
                    // Git counts a parent that the client is known to have before it looks at the date, and such a parent is
                    // usually older than the oldest `have`.
                    if known.contains(&parent) {
                        reached = true;
                        break;
                    }
                    let too_old = match (oldest, self.repo.find_commit(parent)) {
                        (Some(oldest), Ok(parent)) => {
                            parent
                                .time()
                                .or_raise(|| message("Could not read the date of a commit"))?
                                .seconds
                                < oldest
                        }
                        _ => false,
                    };
                    if !too_old {
                        stack.push(parent);
                    }
                }
                if reached {
                    break;
                }
            }
            if !reached {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Serve a client that talks to `input` and `output` like `git upload-pack` does with protocol version 2: write the
    /// advertisement and answer requests until the client has nothing more to ask.
    ///
    /// `interrupt` ends the operation when it is set, the next time it is looked at.
    pub fn serve(&self, input: &mut dyn Read, output: &mut dyn Write, interrupt: &AtomicBool) -> Result {
        let capabilities = self.capabilities();
        write_advertisement(output, &capabilities)?;
        while !interrupt.load(Ordering::Relaxed) {
            let Some(request) = read_request(input, &capabilities)? else {
                break;
            };
            match request.command {
                Command::LsRefs(arguments) => {
                    let lines = self.ls_refs(&arguments)?;
                    output
                        .write_all(&response::ls_refs(&lines)?)
                        .and_then(|()| output.flush())
                        .or_raise(|| message("Could not write the references"))?;
                }
                Command::Fetch(fetch) => self.fetch(&fetch, output, interrupt)?,
            }
        }
        Ok(())
    }
}

/// Writes what it is given as packets of the data band, as large as they can be, and stops once `interrupt` is set, which is
/// noticed with each packet.
struct SideBandWriter<'a> {
    out: &'a mut dyn Write,
    interrupt: &'a AtomicBool,
    buffer: Vec<u8>,
}

impl<'a> SideBandWriter<'a> {
    fn new(out: &'a mut dyn Write, interrupt: &'a AtomicBool) -> Self {
        SideBandWriter {
            out,
            interrupt,
            buffer: Vec::with_capacity(gix_protocol::receive::side_band::MAX_DATA_LEN),
        }
    }

    fn emit(&mut self) -> std::io::Result<()> {
        use gix_protocol::receive::side_band::{Channel, encode};
        if self.interrupt.load(Ordering::Relaxed) {
            // Not `ErrorKind::Interrupted`, as that is what callers of `write_all()` retry.
            return Err(std::io::Error::other("the pack was interrupted"));
        }
        let packet = encode(Channel::Data, &self.buffer);
        self.buffer.clear();
        self.out.write_all(&packet)
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.emit()
    }
}

impl Write for SideBandWriter<'_> {
    fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
        let len = buf.len();
        while !buf.is_empty() {
            let room = gix_protocol::receive::side_band::MAX_DATA_LEN - self.buffer.len();
            let (now, later) = buf.split_at(room.min(buf.len()));
            self.buffer.extend_from_slice(now);
            buf = later;
            if self.buffer.len() == gix_protocol::receive::side_band::MAX_DATA_LEN {
                self.emit()?;
            }
        }
        Ok(len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Write, sync::atomic::AtomicBool};

    use super::SideBandWriter;

    #[test]
    fn the_pack_stops_at_the_first_packet_once_interrupted() {
        let interrupt = AtomicBool::new(false);
        let mut out = Vec::new();
        {
            let mut writer = SideBandWriter::new(&mut out, &interrupt);
            writer
                .write_all(&vec![0; gix_protocol::receive::side_band::MAX_DATA_LEN])
                .expect("a full packet is written while nobody interrupts");
            interrupt.store(true, std::sync::atomic::Ordering::Relaxed);
            let err = writer
                .write_all(&vec![0; gix_protocol::receive::side_band::MAX_DATA_LEN])
                .expect_err("the next packet is not");
            assert_eq!(err.to_string(), "the pack was interrupted");
        }
        assert_eq!(
            out.len(),
            gix_protocol::receive::side_band::MAX_DATA_LEN + 5,
            "only the packet before the interrupt was written"
        );
    }
}
