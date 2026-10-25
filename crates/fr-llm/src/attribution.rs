//! Static public application identity sent to every LLM provider (dsh
//! `dsh-llm/attribution.ts`).
//!
//! No secrets, local paths, session ids, prompt text, or per-user identifiers
//! belong here, and nothing per-request may influence the values; there is
//! no path that suppresses attribution.

/// Static public application identity sent to LLM providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppIdentity {
    /// `User-Agent` product token (lowercase, hyphenated).
    pub product: &'static str,
    /// Product version; sourced from crate metadata, never hand-copied.
    pub version: &'static str,
    /// Repository home URL of the app, used as the `User-Agent` comment.
    pub url: &'static str,
}

/// The Fly Rafter identity: the default every adapter sends (dsh
/// `APP_IDENTITY` with the project's own facts).
pub const APP_IDENTITY: AppIdentity = AppIdentity {
    product: "fly-rafter",
    version: env!("CARGO_PKG_VERSION"),
    url: "https://github.com/greatwallisme/fly-rafter",
};

/// The standard `User-Agent` value: `product/version (+url)` (RFC 9110
/// Section 10.1.5 product + comment syntax).
#[must_use]
pub fn user_agent() -> String {
    format!(
        "{}/{} (+{})",
        APP_IDENTITY.product, APP_IDENTITY.version, APP_IDENTITY.url
    )
}

/// The attribution headers an adapter must send on every provider request.
///
/// Header names are lowercase (HTTP field names are case-insensitive on the
/// wire). Omission of the identity falls back to [`APP_IDENTITY`]: nothing
/// can suppress attribution.
#[must_use]
pub fn attribution_headers() -> [(&'static str, String); 1] {
    [("user-agent", user_agent())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_headers_pin_identity() {
        let agent = user_agent();
        let expected_prefix = concat!(
            "fly-rafter/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/greatwallisme/fly-rafter)"
        );
        assert_eq!(agent, expected_prefix);
        let headers = attribution_headers();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "user-agent");
        assert_eq!(headers[0].1, agent);
        // The identity constants are the documented public facts.
        assert_eq!(APP_IDENTITY.product, "fly-rafter");
        assert_eq!(
            APP_IDENTITY.url,
            "https://github.com/greatwallisme/fly-rafter"
        );
    }
}
