//! Wire types for the agmen protocol v1: envelope, verbs, topics.
//!
//! The normative spec is `docs/protocol.md`.

#[cfg(test)]
mod tests {
    #[test]
    fn proto_crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "agmen-proto");
    }
}
