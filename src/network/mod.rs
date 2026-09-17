pub mod events;

/// The first `max_bytes` of `s`, cut back to a character boundary.
///
/// Every body we truncate is third-party text: it is the app's own HTTP traffic,
/// so it is routinely UTF-8 and routinely non-ASCII. `&s[..n]` PANICS when `n`
/// lands inside a multi-byte character, and both truncation sites did exactly
/// that. A crash in the capture path takes the whole MCP server down on nothing
/// worse than an accented word in an error message.
pub fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The byte-slice guard.
///
/// `&s[..n]` on a `String` panics when `n` lands inside a multi-byte character.
/// Two truncation sites did that on third-party text (an app's own HTTP bodies),
/// so one accented character in an error response took the whole MCP server down.
///
/// The property: nothing slices a string by a raw byte count. Truncation goes
/// through `truncate_on_char_boundary`, which cuts back to a boundary.
#[cfg(test)]
mod byte_slice_guard {
    use crate::source_guard::{line_of, rust_files_under, src_root, without_comments};

    /// Slices that are provably safe, with the reason.
    const ALLOWED: &[(&str, &str)] = &[(
        "network/mod.rs",
        "truncate_on_char_boundary is the safe cut itself; it checks is_char_boundary first",
    )];

    #[test]
    fn nothing_slices_a_string_by_a_raw_byte_count() {
        let mut offenders = Vec::new();
        for (name, src) in rust_files_under(&src_root()) {
            if ALLOWED.iter().any(|(a, _)| *a == name) {
                continue;
            }
            let code = without_comments(&src);
            for (at, _) in code.match_indices("_body[..") {
                offenders.push(format!(
                    "{name}:{} slices a body by byte count",
                    line_of(&code, at)
                ));
            }
            for (at, _) in code.match_indices("body[..") {
                // `_body[..` already counted above; skip the overlap.
                if at > 0 && code.as_bytes()[at - 1] == b'_' {
                    continue;
                }
                offenders.push(format!(
                    "{name}:{} slices a body by byte count",
                    line_of(&code, at)
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "a string body is being sliced by a raw byte count, which panics mid-character.\n{}\n\
             Use network::truncate_on_char_boundary.",
            offenders.join("\n")
        );
    }
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate_on_char_boundary;

    #[test]
    fn a_multibyte_char_on_the_boundary_does_not_panic() {
        // 'é' is two bytes. Cutting at 500 lands inside it, which used to panic.
        let body = format!("{}é{}", "a".repeat(499), "b".repeat(100));
        let out = truncate_on_char_boundary(&body, 500);
        assert_eq!(
            out.len(),
            499,
            "must cut back to the boundary, not through it"
        );
        assert!(body.starts_with(out));
    }

    #[test]
    fn shorter_than_the_cap_is_returned_whole() {
        assert_eq!(truncate_on_char_boundary("hi", 500), "hi");
    }

    #[test]
    fn a_string_that_is_one_long_char_truncates_to_empty_rather_than_panicking() {
        assert_eq!(truncate_on_char_boundary("😀", 2), "");
    }
}
pub mod logcat;
pub mod sink;
