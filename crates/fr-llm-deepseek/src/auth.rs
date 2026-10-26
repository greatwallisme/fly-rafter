//! API-key authentication for the official `DeepSeek` route (dsh
//! `llm-deepseek-api-key`): per-request key resolution and the
//! `x-api-key` header.
//!
//! fr M1 has no interactive credentials service, so only the launch
//! environment arm resolves a key here; the caller performs the
//! environment read and passes the value in.

use fr_llm::api_key::normalize_api_key;
use fr_llm::error::LlmError;

use crate::codes::MISSING_CREDENTIAL_CODE;

/// The official `DeepSeek` provider route.
pub const PROVIDER: &str = "deepseek-official";

/// Credential reference naming the launch-environment variable that
/// carries the API key (dsh `apiKeyEnv` default).
pub const DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// The request header carrying the key.
pub const API_KEY_HEADER: &str = "x-api-key";

/// Resolve one usable API key from the launch environment (dsh
/// `resolveApiKey`, environment arm).
///
/// An absent or blank variable is a missing credential, and a supplied key
/// must pass the shared header-safety check.
///
/// # Errors
/// [`MISSING_CREDENTIAL_CODE`] when no usable value is supplied, and
/// [`fr_llm::error::INVALID_CREDENTIAL_CODE`] for a blank or
/// header-unsafe key.
///
/// # Panics
/// Never: the `expect` covers static message/code pairs that satisfy the
/// [`LlmError`] invariants by construction.
pub fn resolve_api_key(env_value: Option<&str>, api_key_env: &str) -> Result<String, LlmError> {
    let Some(ambient) = env_value.filter(|value| !value.is_empty()) else {
        return Err(missing_credential(api_key_env));
    };
    normalize_api_key(ambient).map_err(|rejection| {
        let message = match rejection {
            fr_llm::api_key::ApiKeyRejection::Empty => format!(
                "llm-deepseek: the API key resolved from {api_key_env} is blank; set \
                 {api_key_env} to the raw key in the fly-rafter configuration, or export \
                 it in the launching environment"
            ),
            fr_llm::api_key::ApiKeyRejection::IllegalCharacters => format!(
                "llm-deepseek: the API key resolved from {api_key_env} contains characters \
                 no HTTP header can carry; set {api_key_env} to the raw key alone in the \
                 fly-rafter configuration"
            ),
        };
        LlmError::try_new(
            &message,
            fr_llm::error::INVALID_CREDENTIAL_CODE,
            None,
            None,
            None,
            None,
        )
        .expect("static message and code satisfy the LlmError invariants")
    })
}

fn missing_credential(api_key_env: &str) -> LlmError {
    LlmError::try_new(
        &format!(
            "llm-deepseek: no API key for provider route \"{PROVIDER}\"; set {api_key_env} \
             in the fly-rafter configuration, or export {api_key_env} in the launching \
             environment"
        ),
        MISSING_CREDENTIAL_CODE,
        None,
        None,
        None,
        None,
    )
    .expect("static message and code satisfy the LlmError invariants")
}

/// The single request header carrying the resolved key (dsh
/// `resolveAuth`).
#[must_use]
pub fn auth_headers(api_key: &str) -> [(&'static str, String); 1] {
    [(API_KEY_HEADER, api_key.to_owned())]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The headless/SDK reading comes from the environment only: absent,
    /// empty, and whitespace-only values are missing credentials with the
    /// verbatim dsh message; a padded legal key trims; an unsafe key
    /// carries the shared header-safety message.
    #[test]
    fn resolve_api_key_pins_dsh_texts() {
        for ambient in [None, Some("")] {
            let error = resolve_api_key(ambient, "DEEPSEEK_API_KEY").expect_err("missing");
            assert_eq!(error.code(), MISSING_CREDENTIAL_CODE);
            assert_eq!(
                error.message(),
                "llm-deepseek: no API key for provider route \"deepseek-official\"; \
                 set DEEPSEEK_API_KEY in the fly-rafter configuration, or export \
                 DEEPSEEK_API_KEY in the launching environment"
            );
        }
        // A non-empty raw value passes the presence gate; a whitespace-only
        // one is blank after the shared trim and reports INVALID_CREDENTIAL.
        let error = resolve_api_key(Some("   "), "DEEPSEEK_API_KEY").expect_err("blank");
        assert_eq!(error.code(), fr_llm::error::INVALID_CREDENTIAL_CODE);
        assert_eq!(
            error.message(),
            "llm-deepseek: the API key resolved from DEEPSEEK_API_KEY is blank; set \
             DEEPSEEK_API_KEY to the raw key in the fly-rafter configuration, or export \
             it in the launching environment"
        );
        assert_eq!(
            resolve_api_key(Some("  sk-abc123  "), "DEEPSEEK_API_KEY").expect("padded key"),
            "sk-abc123"
        );
        let error =
            resolve_api_key(Some("sk-abc\u{00e9}def"), "DEEPSEEK_API_KEY").expect_err("unsafe key");
        assert_eq!(error.code(), fr_llm::error::INVALID_CREDENTIAL_CODE);
        assert_eq!(
            error.message(),
            "llm-deepseek: the API key resolved from DEEPSEEK_API_KEY contains characters \
             no HTTP header can carry; set DEEPSEEK_API_KEY to the raw key alone in the \
             fly-rafter configuration"
        );
        assert_eq!(
            auth_headers("sk-abc123"),
            [("x-api-key", "sk-abc123".to_owned())]
        );
    }
}

// Rust guideline compliant 2026-10-26
