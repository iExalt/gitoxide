//! Port of the parts of `urlmatch.c`, `url.c` and `submodule-config.c` of Git 2.56 which decide whether a submodule URL is allowed.
//!
//! All inputs are treated as C strings, so they end at the first NUL byte.

/// Return `true` if the submodule `url` is one Git refuses to accept, which is `check_submodule_url()` returning an error.
pub(super) fn is_disallowed_submodule_url(url: &[u8]) -> bool {
    let url = c_str(url);
    if url.first() == Some(&b'-') {
        return true;
    }
    if is_relative(url) || url.starts_with(b"git://") {
        // This could be appended to an http URL and url-decoded, check for malicious characters.
        if decode(url).contains(&b'\n') {
            return true;
        }
        // URLs which escape their root via "../" can overwrite the host field and previous components.
        let (dotdots, next) = count_leading_dotdots(url);
        return dotdots > 0 && matches!(next.first(), Some(b':' | b'/'));
    }
    let Some(curl_url) = url_to_curl_url(url) else {
        return false;
    };
    match normalize(curl_url) {
        Some(normalized) => decode(&normalized).contains(&b'\n'),
        None => true,
    }
}

fn c_str(bytes: &[u8]) -> &[u8] {
    bytes.split(|b| *b == 0).next().unwrap_or_default()
}

fn starts_with_dot_slash(url: &[u8]) -> bool {
    matches!(url, [b'.', b'/' | b'\\', ..])
}

fn starts_with_dot_dot_slash(url: &[u8]) -> bool {
    matches!(url, [b'.', b'.', b'/' | b'\\', ..])
}

fn is_relative(url: &[u8]) -> bool {
    starts_with_dot_slash(url) || starts_with_dot_dot_slash(url)
}

/// Count the `../` components at the start of `url`, and return the rest after all leading `./` and `../`.
fn count_leading_dotdots(mut url: &[u8]) -> (usize, &[u8]) {
    let mut count = 0;
    loop {
        if starts_with_dot_dot_slash(url) {
            count += 1;
            url = &url[3..];
        } else if starts_with_dot_slash(url) {
            url = &url[2..];
        } else {
            return (count, url);
        }
    }
}

/// The URL as `git-remote-curl` would be given it, if it is one for which it is used.
fn url_to_curl_url(url: &[u8]) -> Option<&[u8]> {
    for prefix in [&b"http::"[..], b"https::", b"ftp::", b"ftps::"] {
        if let Some(rest) = url.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    [&b"http://"[..], b"https://", b"ftp://", b"ftps://"]
        .iter()
        .any(|prefix| url.starts_with(prefix))
        .then_some(url)
}

fn hex_value(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|v| v as u8)
}

fn hex2chr(pair: &[u8]) -> Option<u8> {
    Some(hex_value(*pair.first()?)? << 4 | hex_value(*pair.get(1)?)?)
}

/// Decode percent-escapes like `url_decode()` does, which leaves everything up to the first colon alone.
/// A `%00` isn't decoded.
fn decode(url: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(url.len());
    let mut rest = url;
    if let Some(colon) = url.iter().position(|b| *b == b':').filter(|colon| *colon > 0) {
        out.extend_from_slice(&url[..colon]);
        rest = &url[colon..];
    }
    let mut idx = 0;
    while let Some(&byte) = rest.get(idx) {
        if byte == b'%'
            && rest.len() - idx >= 3
            && let Some(value) = hex2chr(&rest[idx + 1..]).filter(|v| *v > 0)
        {
            out.push(value);
            idx += 3;
            continue;
        }
        out.push(byte);
        idx += 1;
    }
    out
}

const UNSAFE: &[u8] = b" <>\"%{}|\\^`";
const RESERVED: &[u8] = b":/?#[]@!$&'()*+,;=";

/// Append `from` with escapes normalized like `append_normalized_escapes()` does for escapes that aren't `esc_extra`,
/// and fail if there is an invalid one.
fn append_normalized_escapes(buf: &mut Vec<u8>, from: &[u8]) -> bool {
    let mut idx = 0;
    while idx < from.len() {
        let mut ch = from[idx];
        idx += 1;
        let mut was_escaped = false;
        if ch == b'%' {
            let Some(value) = from.get(idx..idx + 2).and_then(hex2chr) else {
                return false;
            };
            ch = value;
            idx += 2;
            was_escaped = true;
        }
        if ch <= 0x1f || ch >= 0x7f || UNSAFE.contains(&ch) || (was_escaped && RESERVED.contains(&ch)) {
            buf.extend_from_slice(format!("%{ch:02X}").as_bytes());
        } else {
            buf.push(ch);
        }
    }
    true
}

fn span(bytes: &[u8], set: impl Fn(u8) -> bool) -> usize {
    bytes.iter().take_while(|b| set(**b)).count()
}

/// Normalize `url` like `url_normalize()` does, and return `None` if it isn't valid.
fn normalize(url: &[u8]) -> Option<Vec<u8>> {
    let all = c_str(url);
    let at = |i: usize| all.get(i).copied().unwrap_or(0);
    let url_len = all.len();

    // The scheme, followed by `://`.
    let scheme_chars = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-');
    let mut spanned = span(all, scheme_chars);
    if spanned == 0
        || !all[0].is_ascii_alphabetic()
        || spanned + 3 > url_len
        || at(spanned) != b':'
        || at(spanned + 1) != b'/'
        || at(spanned + 2) != b'/'
    {
        return None;
    }
    let scheme_len = spanned;
    spanned += 3;
    let mut norm: Vec<u8> = all[..spanned].iter().map(u8::to_ascii_lowercase).collect();
    let mut url = spanned;

    // Credentials.
    let rest = &all[url..];
    let at_ptr = rest.iter().position(|b| *b == b'@').map(|p| p + url);
    let slash_ptr = url + span(rest, |b| !matches!(b, b'/' | b'?' | b'#'));
    if let Some(at_ptr) = at_ptr.filter(|p| *p < slash_ptr) {
        if at_ptr > url && !append_normalized_escapes(&mut norm, &all[url..at_ptr]) {
            return None;
        }
        norm.push(b'@');
        url = at_ptr + 1;
    }

    // The host without the port, with no escapes allowed.
    let mut host_off = 0;
    if url >= url_len || matches!(at(url), b':' | b'/' | b'?' | b'#') {
        // Missing host is invalid for all URL schemes except file.
        if !norm.starts_with(b"file:") {
            return None;
        }
    } else {
        host_off = norm.len();
    }
    let mut colon_ptr = slash_ptr as isize - 1;
    while colon_ptr > url as isize && at(colon_ptr as usize) != b':' && at(colon_ptr as usize) != b']' {
        colon_ptr -= 1;
    }
    let colon_ptr = if colon_ptr < 0 || at(colon_ptr as usize) != b':' {
        slash_ptr
    } else {
        let colon_ptr = colon_ptr as usize;
        if host_off == 0 && colon_ptr < slash_ptr && colon_ptr + 1 != slash_ptr {
            // file: URLs may not have a port number.
            return None;
        }
        colon_ptr
    };
    let host_chars = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'[' | b':' | b']');
    if span(&all[url.min(url_len)..], host_chars) < colon_ptr.saturating_sub(url) {
        return None;
    }
    while url < colon_ptr {
        norm.push(at(url).to_ascii_lowercase());
        url += 1;
    }

    // The port, which isn't copied if it's the default.
    if colon_ptr < slash_ptr {
        url += 1;
        url += span(&all[url.min(url_len)..], |b| b == b'0');
        if url == slash_ptr && at(url - 1) == b'0' {
            url -= 1;
        }
        if url == slash_ptr {
            // A colon with no port number is the same as the default.
        } else if slash_ptr - url == 2 && norm.starts_with(b"http:") && &all[url..slash_ptr] == b"80" {
            // The default for http.
        } else if slash_ptr - url == 3 && norm.starts_with(b"https:") && &all[url..slash_ptr] == b"443" {
            // The default for https.
        } else {
            // All digits, in the range 1..=65535, and 0 means "any".
            let digits = &all[url..slash_ptr];
            if span(digits, |b| b.is_ascii_digit()) < digits.len() {
                return None;
            }
            let port = if digits.len() <= 5 {
                std::str::from_utf8(digits)
                    .ok()
                    .and_then(|d| d.parse::<u32>().ok())
                    .unwrap_or(0)
            } else {
                0
            };
            if port == 0 || port > 65535 {
                return None;
            }
            norm.push(b':');
            norm.extend_from_slice(digits);
        }
        url = slash_ptr;
    }
    let _ = (scheme_len, host_off);

    // The path, resolving dot segments, and adding the first slash if it's missing.
    let path_off = norm.len();
    norm.push(b'/');
    if at(url) == b'/' && url < url_len {
        url += 1;
    }
    loop {
        let seg_start = norm.len();
        let next_slash = url + span(&all[url.min(url_len)..], |b| !matches!(b, b'/' | b'?' | b'#'));
        if !append_normalized_escapes(&mut norm, &all[url.min(url_len)..next_slash]) {
            return None;
        }
        let mut skip_add_slash = false;
        match &norm[seg_start..] {
            b"." => {
                if seg_start == path_off + 1 {
                    norm.pop();
                    skip_add_slash = true;
                } else {
                    norm.truncate(norm.len() - 2);
                }
            }
            b".." => {
                // The slash before the segment, which can't be the first one as nothing precedes it then.
                let mut prev_slash = norm.len() - 3;
                if prev_slash == path_off {
                    return None;
                }
                loop {
                    prev_slash -= 1;
                    if norm[prev_slash] == b'/' {
                        break;
                    }
                }
                if prev_slash == path_off {
                    norm.truncate(prev_slash + 1);
                    skip_add_slash = true;
                } else {
                    norm.truncate(prev_slash);
                }
            }
            _ => {}
        }
        url = next_slash;
        if at(url) != b'/' || url >= url_len {
            break;
        }
        url += 1;
        if !skip_add_slash {
            norm.push(b'/');
        }
    }

    // The rest, which is a query and fragment.
    if url < url_len && !append_normalized_escapes(&mut norm, &all[url..]) {
        return None;
    }
    Some(norm)
}
