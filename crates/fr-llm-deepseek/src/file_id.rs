//! `DeepSeek` Files API identifiers (dsh `file-id.ts`); both are opaque
//! branded strings on the dsh side whose brand constructors perform no
//! validation, so the Rust newtypes mirror that exactly.

/// Opaque identifier returned by the `DeepSeek` Files API (dsh
/// `DeepSeekFileId`); branded after wire validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct DeepSeekFileId(pub String);

impl DeepSeekFileId {
    /// Brands a provider-returned file identifier.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The opaque id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Non-secret digest identifying one endpoint and API-key file namespace
/// (dsh `DeepSeekFileScope`); a SHA-256 digest of endpoint and API key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct DeepSeekFileScope(pub String);

impl DeepSeekFileScope {
    /// Brands a locally derived namespace digest.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The digest text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// Rust guideline compliant 2026-10-26
