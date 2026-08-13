//! Reading configuration from environment variables.
//!
//! Containers are configured through the environment rather than files, so this
//! module is the one place that reads it.

/// Reads a TCP port from an environment variable, falling back to `default`.
///
/// # Rust concepts in this function
///
/// - `&str` is a *borrowed string slice*: a read-only view into text somebody
///   else owns. `String` is the owned, growable counterpart. Taking `&str` here
///   means the caller keeps ownership and nothing is copied.
/// - `u16` is an unsigned 16-bit integer (0–65535) — exactly the range of a TCP
///   port, so the type itself rules out impossible values.
/// - `std::env::var` returns `Result<String, VarError>`: either `Ok(value)` or
///   `Err(reason)`. Rust has no exceptions; fallibility lives in the return type.
/// - `.ok()` discards the error detail, turning `Result<T, E>` into `Option<T>`.
///   We do not care *why* the variable was missing, only that it was.
pub fn port_from_env(name: &str, default: u16) -> u16 {
    parse_port(std::env::var(name).ok(), default)
}

/// The decision-making half of [`port_from_env`], with the environment removed.
///
/// Splitting it this way is a common Rust testing pattern: the part that talks
/// to the outside world stays trivial, and the part with logic worth testing
/// takes plain values. Tests then need no global state — which matters because
/// `cargo test` runs tests in parallel threads by default, and setting an
/// environment variable would leak across them.
///
/// `Option<String>` means "maybe a string": `Some(text)` or `None`.
fn parse_port(raw: Option<String>, default: u16) -> u16 {
    raw
        // `.and_then(...)` runs the closure only when we have `Some`, and the
        // closure itself returns an `Option`. Chaining avoids nested `match`
        // blocks. A closure is Rust's anonymous function: `|arg| body`.
        .and_then(|value| {
            // `::<u16>` is the "turbofish": it tells `parse` which type to
            // produce, because the compiler cannot infer it from context here.
            value.trim().parse::<u16>().ok()
        })
        // `.unwrap_or(x)` yields the inner value, or `x` when there is none.
        // (Beware the similarly named `.unwrap()`, which *panics* on `None`.
        // The workspace lints deny it outside tests for exactly that reason.)
        .unwrap_or(default)
}

// `#[cfg(test)]` is a *conditional compilation* attribute: this module only
// exists during `cargo test`, so test code never ships in the release binary.
#[cfg(test)]
mod tests {
    // `super` means "the parent module" — this file's top level. `*` imports
    // everything from it, which is how these tests reach the private
    // `parse_port`. Child modules can see their parent's private items; that is
    // why unit tests in Rust live inside the file they test.
    use super::*;

    #[test]
    fn uses_the_default_when_unset() {
        assert_eq!(parse_port(None, 8080), 8080);
    }

    #[test]
    fn reads_a_valid_value() {
        // `.to_owned()` copies the `&str` literal into an owned `String`,
        // because that is what the parameter asks for.
        assert_eq!(parse_port(Some("9090".to_owned()), 8080), 9090);
    }

    #[test]
    fn falls_back_when_the_value_is_not_a_port() {
        assert_eq!(parse_port(Some("not-a-number".to_owned()), 8080), 8080);
    }

    #[test]
    fn falls_back_when_the_value_is_out_of_range() {
        // 70000 does not fit in a u16, so parsing fails and we take the default.
        assert_eq!(parse_port(Some("70000".to_owned()), 8080), 8080);
    }

    #[test]
    fn ignores_surrounding_whitespace() {
        assert_eq!(parse_port(Some("  7000 ".to_owned()), 8080), 7000);
    }

    #[test]
    fn reads_the_real_environment() {
        // Exercises the thin wrapper too. A name nothing sets is guaranteed to
        // be absent, so this needs no cleanup and cannot clash with other tests.
        assert_eq!(
            port_from_env("SERVICE_CORE_PORT_THAT_IS_NEVER_SET", 1234),
            1234
        );
    }
}
