//! The `tables.list` file: its format and the names it may contain.

use gix_error::{ErrorExt, Result};

use crate::basics::corrupt;

/// The extension of table files, which is also what a name in `tables.list` must end with.
pub(crate) const TABLE_SUFFIX: &str = ".ref";

/// Parse the content of `tables.list` into table names, ordered from the oldest table to the newest.
///
/// Like Git, skip blank lines and refuse content whose last line isn't terminated, as that is what a
/// truncated write looks like. An empty file is an empty stack.
///
/// Unlike Git, which opens whatever name it is given, refuse names that could leave the directory of the stack.
pub(crate) fn parse(content: &[u8]) -> Result<Vec<String>> {
    if content.last().is_some_and(|last| *last != b'\n') {
        return Err(corrupt(
            "tables.list doesn't end with a newline, as if its writing was cut short",
        ));
    }
    content
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(validate_name)
        .collect()
}

fn validate_name(name: &[u8]) -> Result<String> {
    let invalid = |why: &str| {
        gix_error::corruption(format!(
            "tables.list contains the invalid table name {:?}: {why}",
            bstr::BStr::new(name)
        ))
        .raise()
    };
    if name.contains(&0) {
        return Err(invalid("a table name must not contain NUL"));
    }
    let name = std::str::from_utf8(name).map_err(|_| invalid("a table name must be UTF-8"))?;
    // Names are joined onto the directory of the stack, so refuse everything that wouldn't stay in it, on any platform:
    // path separators, relative components, drive prefixes like `C:x.ref` and device names like `CON.ref`.
    gix_validate::path::component(name.into(), None, gix_validate::path::component::Options::default())
        .map_err(|err| invalid(&err.to_string()))?;
    if !name.ends_with(TABLE_SUFFIX) {
        return Err(invalid("a table name must end with `.ref`"));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn names_are_kept_in_order_and_blank_lines_are_skipped() {
        let names =
            parse(b"0x000000000001-0x000000000001-aaaaaaaa.ref\n\n0x000000000002-0x000000000003-bbbbbbbb.ref\n")
                .expect("valid");
        assert_eq!(
            names,
            [
                "0x000000000001-0x000000000001-aaaaaaaa.ref",
                "0x000000000002-0x000000000003-bbbbbbbb.ref"
            ],
            "oldest table first"
        );
    }

    #[test]
    fn an_empty_list_is_an_empty_stack() {
        assert!(parse(b"").expect("valid").is_empty());
        assert!(parse(b"\n\n").expect("blank lines only").is_empty());
    }

    #[test]
    fn a_missing_final_newline_is_corruption() {
        let err = parse(b"a.ref\nb.ref").expect_err("cut short");
        assert!(err.is_corrupted(), "{err:?}");
    }

    #[test]
    fn names_that_could_leave_the_directory_are_refused() {
        for bad in [
            &b"../x.ref\n"[..],
            b"sub/x.ref\n",
            b"sub\\x.ref\n",
            b"..\n",
            b".\n",
            b"x.ref\0\n",
            b"table\n",
            b"\xff.ref\n",
            b"x.ref \n",
            b"C:x.ref\n",
            b"c:\\x.ref\n",
            b"CON.ref\n",
            b"nul.ref\n",
        ] {
            let result = parse(bad);
            if bad == b"x.ref \n" {
                assert!(result.is_err(), "a name that doesn't end with the suffix is refused");
            } else {
                let err = result.expect_err("refused");
                assert!(err.is_corrupted(), "{bad:?}: {err:?}");
            }
        }
    }
}
