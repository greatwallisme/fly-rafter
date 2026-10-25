//! The one definition of a well-formed provider API key, shared by every
//! adapter that puts one in an HTTP header (dsh `dsh-llm/api-key.ts`).

/// Why a supplied API key cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyRejection {
    /// The trimmed key was empty.
    Empty,
    /// The key carries characters no HTTP header can carry.
    IllegalCharacters,
}

/// Judge one *supplied* API key, trimming surrounding whitespace first.
///
/// Trimming is silent because a padded key has one unambiguous reading;
/// every other defect is reported. Absence is a configuration state this
/// function never sees — a profile naming no credential authenticates
/// through the provider's own ambient discovery — so callers decide whether
/// a value was supplied before asking (dsh `normalizeApiKey`).
///
/// # Errors
/// [`ApiKeyRejection::Empty`] for a blank key, [`ApiKeyRejection::IllegalCharacters`]
/// for characters outside the printable-ASCII header set.
pub fn normalize_api_key(raw: &str) -> Result<String, ApiKeyRejection> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(ApiKeyRejection::Empty);
    }
    let legal = value
        .chars()
        .all(|character| ('\x21'..='\x7E').contains(&character));
    if !legal {
        return Err(ApiKeyRejection::IllegalCharacters);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_api_key_trims_and_rejects() {
        // A padded legal key trims silently to its unambiguous reading.
        assert_eq!(
            normalize_api_key("  sk-abc123  ").expect("padded legal key"),
            "sk-abc123"
        );
        // Whitespace-only is empty after the silent trim.
        assert_eq!(
            normalize_api_key("   ").expect_err("blank key"),
            ApiKeyRejection::Empty
        );
        // A non-ASCII character cannot ride an HTTP header verbatim.
        assert_eq!(
            normalize_api_key("sk-abc\u{00e9}def").expect_err("Latin-1 key"),
            ApiKeyRejection::IllegalCharacters
        );
    }
}
