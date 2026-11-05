//! Port of `fsck_gitmodules_fn()` of Git 2.56's `fsck.c`, together with the parser for configuration files of `config.c` that it relies on,
//! as the checks have to find what Git's parser finds, even in corrupted or adversarial files.
use gix_object::bstr::{BStr, ByteSlice};

use crate::{MessageId, check::Reporter, check::url::is_disallowed_submodule_url};

/// Check the content of a `.gitmodules` file, and return `true` if one of the problems is an error.
pub(super) fn check(r: &mut Reporter<'_>, data: &[u8]) -> bool {
    let mut failed = false;
    let parsed = parse(data, &mut |var, value| {
        failed |= check_entry(r, var, value);
    });
    if !parsed {
        failed |= r.report(
            MessageId::GitmodulesParse,
            format_args!("could not parse gitmodules blob"),
        );
    }
    failed
}

fn check_entry(r: &mut Reporter<'_>, var: &[u8], value: Option<&[u8]>) -> bool {
    // These are C strings in Git, so they end at the first NUL byte.
    let c_str = |bytes: &'_ [u8]| bytes.split(|b| *b == 0).next().unwrap_or_default().to_vec();
    let var = c_str(var);
    let value = value.map(c_str);
    // `parse_config_key(var, "submodule", &subsection, &subsection_len, &key)`
    let Some(rest) = var
        .strip_prefix(b"submodule")
        .filter(|rest| rest.first() == Some(&b'.'))
    else {
        return false;
    };
    let Some(dot) = rest.iter().rposition(|b| *b == b'.') else {
        return false;
    };
    if dot == 0 {
        // There is no subsection.
        return false;
    }
    let (name, key) = (&rest[1..dot], &rest[dot + 1..]);
    let mut failed = false;
    if gix_validate::submodule::name(name.as_bstr()).is_err() {
        failed |= r.report_value(MessageId::GitmodulesName, "disallowed submodule name: ", name.as_bstr());
    }
    if let Some(value) = value.as_deref() {
        let value: &BStr = value.as_bstr();
        if key == b"url" && is_disallowed_submodule_url(value) {
            failed |= r.report_value(MessageId::GitmodulesUrl, "disallowed submodule url: ", value);
        }
        if key == b"path" && value.first() == Some(&b'-') {
            failed |= r.report_value(MessageId::GitmodulesPath, "disallowed submodule path: ", value);
        }
        if key == b"update" && is_update_command(value) {
            failed |= r.report_value(
                MessageId::GitmodulesUpdate,
                "disallowed submodule update setting: ",
                value,
            );
        }
    }
    failed
}

/// Return `true` if `value` is `parse_submodule_update_type()`'s `SM_UPDATE_COMMAND`.
fn is_update_command(value: &[u8]) -> bool {
    !matches!(value, b"none" | b"checkout" | b"rebase" | b"merge") && value.first() == Some(&b'!')
}

/// Called with the name of a variable and its value, if it has one.
type EntryFn<'a> = dyn FnMut(&[u8], Option<&[u8]>) + 'a;

/// The characters of the configuration file as `get_next_char()` produces them.
///
/// This reads the bytes like Git does when it reads the `.gitmodules` file from a work tree, which is what is going to be parsed
/// after the checkout: all bytes but the end of the data are characters, and a byte order mark is skipped.
///
/// That is **a deliberate deviation** from Git's check, which reads the object from memory as `char` that is signed on many platforms.
/// There, the byte `0xff` reads as the end of the file and a byte order mark isn't skipped, so that a `.gitmodules` file with
/// a `0xff` byte or a byte order mark before a dangerous entry passes `fsck` while the file read after the checkout has the entry.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    /// Set once the end of the data was reached, after which `'\n'` is returned for good.
    eof: bool,
}

impl Reader<'_> {
    fn fgetc(&mut self) -> Option<u8> {
        let c = self.data.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn next(&mut self) -> u8 {
        let mut c = self.fgetc();
        if c == Some(b'\r') {
            // DOS-like line ends.
            c = self.fgetc();
            if c != Some(b'\n') {
                if c.is_some() {
                    self.pos -= 1;
                }
                c = Some(b'\r');
            }
        }
        match c {
            Some(c) => c,
            None => {
                self.eof = true;
                b'\n'
            }
        }
    }
}

const BOM: [u8; 3] = [0xef, 0xbb, 0xbf];

/// Git's `isspace()`, which is only these four characters.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

fn is_key_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

/// Parse `data` like `git_config_from_mem()` does, calling `entry` with the lowercased variable name
/// (`section.subsection.key`) and the value, if there is any, for each entry. Return `false` if the file can't be parsed,
/// which happens after the entries before the problem were passed on.
fn parse(data: &[u8], entry: &mut EntryFn<'_>) -> bool {
    let mut reader = Reader {
        data,
        pos: 0,
        eof: false,
    };
    let mut var: Vec<u8> = Vec::new();
    let (mut base_len, mut comment) = (0, false);
    // How much of the byte order mark was seen at the start, with `BOM.len()` meaning that none of it is expected anymore.
    let mut bom = 0;
    loop {
        let c = reader.next();
        if bom < BOM.len() {
            if c == BOM[bom] {
                bom += 1;
                continue;
            }
            if bom != 0 {
                // A partial byte order mark isn't tolerated.
                return false;
            }
            bom = BOM.len();
        }
        if c == b'\n' {
            if reader.eof {
                return true;
            }
            comment = false;
            continue;
        }
        if comment || is_space(c) {
            continue;
        }
        if c == b'#' || c == b';' {
            comment = true;
            continue;
        }
        if c == b'[' {
            var.clear();
            if !section_header(&mut reader, &mut var) || var.is_empty() {
                return false;
            }
            var.push(b'.');
            base_len = var.len();
            continue;
        }
        if !c.is_ascii_alphabetic() {
            return false;
        }
        var.truncate(base_len);
        var.push(c.to_ascii_lowercase());
        if !key_and_value(&mut reader, &mut var, entry) {
            return false;
        }
    }
}

fn key_and_value(reader: &mut Reader<'_>, name: &mut Vec<u8>, entry: &mut EntryFn<'_>) -> bool {
    let mut c;
    loop {
        c = reader.next();
        if reader.eof || !is_key_char(c) {
            break;
        }
        name.push(c.to_ascii_lowercase());
    }
    while c == b' ' || c == b'\t' {
        c = reader.next();
    }
    let mut value = None;
    if c != b'\n' {
        if c != b'=' {
            return false;
        }
        match value_of(reader) {
            Some(parsed) => value = Some(parsed),
            None => return false,
        }
    }
    entry(name, value.as_deref());
    true
}

/// Parse a value like `parse_value()` does.
fn value_of(reader: &mut Reader<'_>) -> Option<Vec<u8>> {
    let (mut quote, mut comment) = (false, false);
    let mut trim_len = 0;
    let mut value: Vec<u8> = Vec::new();
    loop {
        let mut c = reader.next();
        if c == b'\n' {
            if quote {
                return None;
            }
            if trim_len != 0 {
                value.truncate(trim_len);
            }
            return Some(value);
        }
        if comment {
            continue;
        }
        if is_space(c) && !quote {
            if trim_len == 0 {
                trim_len = value.len();
            }
            if !value.is_empty() {
                value.push(c);
            }
            continue;
        }
        if !quote && (c == b';' || c == b'#') {
            comment = true;
            continue;
        }
        trim_len = 0;
        if c == b'\\' {
            c = reader.next();
            match c {
                b'\n' => continue,
                b't' => c = b'\t',
                b'b' => c = 0x08,
                b'n' => c = b'\n',
                // Some characters escape as themselves.
                b'\\' | b'"' => {}
                // Unknown escape sequences are rejected.
                _ => return None,
            }
            value.push(c);
            continue;
        }
        if c == b'"' {
            quote = !quote;
            continue;
        }
        value.push(c);
    }
}

/// Parse what follows a `[` like `get_base_var()` does, and return `false` if there is a problem.
fn section_header(reader: &mut Reader<'_>, name: &mut Vec<u8>) -> bool {
    loop {
        let c = reader.next();
        if reader.eof {
            return false;
        }
        if c == b']' {
            return true;
        }
        if is_space(c) {
            return extended_section_header(reader, name, c);
        }
        if !is_key_char(c) && c != b'.' {
            return false;
        }
        name.push(c.to_ascii_lowercase());
    }
}

/// Parse what follows the base of a section like `[base "extension"]`, like `get_extended_base_var()` does.
fn extended_section_header(reader: &mut Reader<'_>, name: &mut Vec<u8>, mut c: u8) -> bool {
    loop {
        if c == b'\n' {
            return false;
        }
        c = reader.next();
        if !is_space(c) {
            break;
        }
    }
    if c != b'"' {
        return false;
    }
    name.push(b'.');
    loop {
        let mut c = reader.next();
        if c == b'\n' {
            return false;
        }
        if c == b'"' {
            break;
        }
        if c == b'\\' {
            c = reader.next();
            if c == b'\n' {
                return false;
            }
        }
        name.push(c);
    }
    reader.next() == b']'
}
