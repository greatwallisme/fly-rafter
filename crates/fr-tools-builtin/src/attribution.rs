// Rust guideline compliant 2026-11-01
//! The product attribution the outbound-request faces stamp on
//! their HTTP calls.
//!
//! This is the fr-llm attribution face, mirrored locally because
//! domain crates do not depend on each other (PRD 4.5); the version
//! rides [`APP_IDENTITY`] — never a literal to bump by hand.

#![forbid(unsafe_code)]

/// The project's own facts (the fr-llm `APP_IDENTITY` under the same
/// workspace facts).
pub const APP_IDENTITY: AppIdentity = AppIdentity {
    product: "fly-rafter",
    version: env!("CARGO_PKG_VERSION"),
    url: "https://github.com/greatwallisme/fly-rafter",
};

/// The identity facts one outbound-request face stamps onto its calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppIdentity {
    /// The product name.
    pub product: &'static str,
    /// The crate version (compile-time; no hand-maintained literal).
    pub version: &'static str,
    /// The project URL.
    pub url: &'static str,
}

/// The standard `User-Agent` value: `product/version (+url)` (RFC 9110
/// Section 10.1.5 product + comment syntax) — the dsh
/// `deepseek-harness/0.0.1 (+https://github.com/deepseek-ai)` shape.
#[must_use]
pub fn user_agent() -> String {
    format!(
        "{}/{} (+{})",
        APP_IDENTITY.product, APP_IDENTITY.version, APP_IDENTITY.url
    )
}

/// The short attribution value: `product/version` — the dsh
/// `deepseek-harness/0.0.1` shape the search/fetch API providers send.
#[must_use]
pub fn product_agent() -> String {
    format!("{}/{}", APP_IDENTITY.product, APP_IDENTITY.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both attribution forms ride the crate version, never a literal.
    #[test]
    fn attribution_pins_the_identity_forms() {
        let expected_prefix = concat!(
            "fly-rafter/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/greatwallisme/fly-rafter)"
        );
        assert_eq!(user_agent(), expected_prefix);
        assert_eq!(
            product_agent(),
            format!("fly-rafter/{}", env!("CARGO_PKG_VERSION"))
        );
    }
}
