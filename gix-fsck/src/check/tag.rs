//! Port of `fsck_tag_standalone()` of Git 2.56's `fsck.c`, and the parsing of `parse_tag_buffer()` that precedes it during transfers.
use gix_hash::{ObjectId, oid};
use gix_object::Kind;
use gix_object::bstr::ByteSlice;

use crate::{
    MessageId,
    check::{
        Refusal, Reporter,
        commit::{next_line, skip_prefix},
        ident::{self, verify_headers},
    },
};

/// Parse a tag like `parse_tag_buffer()` does, which transfers do before checking it, and return what it points to
/// or fail with what Git says if the tag isn't well-formed enough.
pub(super) fn parse(id: &oid, data: &[u8], hash_kind: gix_hash::Kind) -> Result<(ObjectId, Kind), Refusal> {
    let hex = hash_kind.len_in_hex();
    let unparsable = |detail: Option<String>| Refusal::Unparsable {
        oid: id.to_owned(),
        kind: Kind::Tag,
        detail,
    };
    if data.len() < hex + 24 || !data.starts_with(b"object ") {
        return Err(unparsable(None));
    }
    let target = data
        .get(7..7 + hex)
        .and_then(|hex_id| ObjectId::from_hex(hex_id).ok())
        .ok_or_else(|| unparsable(None))?;
    let mut pos = 7 + hex;
    if data.get(pos) != Some(&b'\n') {
        return Err(unparsable(None));
    }
    pos += 1;
    let Some(next) = skip_prefix(data, pos, b"type ") else {
        return Err(unparsable(None));
    };
    pos = next;
    let Some(newline) = data[pos..].iter().position(|b| *b == b'\n').filter(|len| *len < 20) else {
        return Err(unparsable(None));
    };
    let type_name = &data[pos..pos + newline];
    pos += newline + 1;
    let kind = Kind::from_bytes(type_name)
        .map_err(|_| unparsable(Some(format!("unknown tag type '{}' in {id}", type_name.to_str_lossy()))))?;
    if !(pos + 4 < data.len() && data[pos..].starts_with(b"tag ")) {
        return Err(unparsable(None));
    }
    if !data[pos + 4..].contains(&b'\n') {
        return Err(unparsable(None));
    }
    Ok((target, kind))
}

/// Check the tag `data` like `fsck_tag_standalone()` does, which stops at the first problem that is reported as error. Return `true` on failure.
///
/// Note that Git 2.56 overwrites the result of checking the `tagger` line with that of what follows, which is only an
/// extra header if there is no signature. A tag with a bad `tagger` line and an extra header is thus reported as having an error,
/// but isn't failed, as long as `extraHeaderEntry` isn't an error. This is replicated.
pub(super) fn check(r: &mut Reporter<'_>, data: &[u8], hash_kind: gix_hash::Kind) -> bool {
    if r.is_skipped() {
        return false;
    }
    if verify_headers(r, data) {
        return true;
    }
    let hex = hash_kind.len_in_hex();
    let mut pos = 0;
    let Some(next) = skip_prefix(data, pos, b"object ") else {
        return r.report(
            MessageId::MissingObject,
            format_args!("invalid format - expected 'object' line"),
        );
    };
    pos = next;
    let hex_ok = data
        .get(pos..pos + hex)
        .is_some_and(|id| id.iter().all(u8::is_ascii_hexdigit))
        && data.get(pos + hex) == Some(&b'\n');
    if hex_ok {
        pos += hex + 1;
    } else {
        if r.report(
            MessageId::BadObjectSha1,
            format_args!("invalid 'object' line format - bad sha1"),
        ) {
            return true;
        }
        pos = next_line(data, pos);
    }

    let Some(next) = skip_prefix(data, pos, b"type ") else {
        return r.report(
            MessageId::MissingTypeEntry,
            format_args!("invalid format - expected 'type' line"),
        );
    };
    pos = next;
    let Some(eol) = data[pos..].iter().position(|b| *b == b'\n').map(|len| pos + len) else {
        return r.report(
            MessageId::MissingType,
            format_args!("invalid format - unexpected end after 'type' line"),
        );
    };
    if Kind::from_bytes(&data[pos..eol]).is_err() && r.report(MessageId::BadType, format_args!("invalid 'type' value"))
    {
        return true;
    }
    pos = eol + 1;

    let Some(next) = skip_prefix(data, pos, b"tag ") else {
        return r.report(
            MessageId::MissingTagEntry,
            format_args!("invalid format - expected 'tag' line"),
        );
    };
    pos = next;
    let Some(eol) = data[pos..].iter().position(|b| *b == b'\n').map(|len| pos + len) else {
        return r.report(
            MessageId::MissingTag,
            format_args!("invalid format - unexpected end after 'type' line"),
        );
    };
    let name = &data[pos..eol];
    let mut full_name = b"refs/tags/".to_vec();
    full_name.extend_from_slice(name);
    if gix_validate::reference::name(full_name.as_bstr()).is_err()
        && r.report(
            MessageId::BadTagName,
            format_args!("invalid 'tag' name: {}", name.as_bstr()),
        )
    {
        return true;
    }
    pos = eol + 1;

    // This is where Git's `ret` is a variable that's overwritten by what follows.
    let ret = if let Some(next) = skip_prefix(data, pos, b"tagger ") {
        pos = next;
        ident::check(r, data, &mut pos)
    } else {
        // Early tags don't have a tagger, which is only a warning.
        if r.report(
            MessageId::MissingTaggerEntry,
            format_args!("invalid format - expected 'tagger' line"),
        ) {
            return true;
        }
        false
    };

    let signature_prefix = skip_prefix(data, pos, b"gpgsig ").or_else(|| skip_prefix(data, pos, b"gpgsig-sha256 "));
    if let Some(next) = signature_prefix {
        pos = next;
        let Some(eol) = data[pos..].iter().position(|b| *b == b'\n').map(|len| pos + len) else {
            return r.report(
                MessageId::BadGpgsig,
                format_args!("invalid format - unexpected end after 'gpgsig' or 'gpgsig-sha256' line"),
            );
        };
        pos = eol + 1;
        while data.get(pos) == Some(&b' ') {
            let Some(eol) = data[pos..].iter().position(|b| *b == b'\n').map(|len| pos + len) else {
                return r.report(
                    MessageId::BadHeaderContinuation,
                    format_args!("invalid format - unexpected end in 'gpgsig' or 'gpgsig-sha256' continuation line"),
                );
            };
            pos = eol + 1;
        }
    }

    if pos < data.len() && !data[pos..].starts_with(b"\n") {
        // The check of the headers lets garbage that looks like another header pass, which `mktag` doesn't want.
        return r.report(
            MessageId::ExtraHeaderEntry,
            format_args!("invalid format - extra header(s) after 'tagger'"),
        );
    }
    ret
}
