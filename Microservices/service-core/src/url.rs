//! Building URLs by hand.
//!
//! # Why this exists at all
//!
//! `reqwest` has a `.query()` helper, but it lives behind features this
//! workspace switched off to keep container builds small. Anything assembling
//! a query string therefore has to encode it itself — and PromQL, the main
//! thing we put in one, is full of characters that must not travel raw in a
//! URL: `{}`, `()`, `"`, spaces, `=`.
//!
//! It lives in `service-core` because two callers already need it. Per the
//! Team SOP this is platform surface, owned by the Orchestration Agent.

/// Percent-encodes a string for use as a URL query value.
///
/// Only the RFC 3986 *unreserved* set passes through untouched; everything
/// else becomes `%XX`. Always safe, occasionally more verbose than necessary —
/// the right trade for something whose failure mode is a silently truncated
/// query.
///
/// # Rust concepts here
///
/// - `&str` in, `String` out: the caller keeps its own text, and gets back a
///   new owned value rather than the function mutating something it borrowed.
/// - `.bytes()` rather than `.chars()`: percent-encoding is defined over
///   bytes, so a multi-byte character correctly becomes several `%XX` pairs.
/// - `{:02X}` formats one byte as two uppercase hex digits.
pub fn percent_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len() * 2);

    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreserved_characters_pass_through_untouched() {
        assert_eq!(percent_encode("up"), "up");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(percent_encode("abcXYZ0189"), "abcXYZ0189");
    }

    #[test]
    fn promql_survives_a_round_trip_into_a_url() {
        // The exact characters that appear in the queries this workspace runs,
        // and that would otherwise break the URL or truncate the expression.
        assert_eq!(
            percent_encode("count(up{service=\"worker\"} == 1)"),
            "count%28up%7Bservice%3D%22worker%22%7D%20%3D%3D%201%29"
        );
    }

    #[test]
    fn a_multibyte_character_becomes_several_escapes() {
        // "é" is two bytes in UTF-8, so it must produce two escapes rather
        // than one - the reason this walks bytes and not chars.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn an_empty_string_is_left_alone() {
        assert_eq!(percent_encode(""), "");
    }
}
