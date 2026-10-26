//! `OpenAI` prompt-cache key clamping (pi-ai `api/openai-prompt-cache.ts`).

/// The `prompt_cache_key` length limit.
pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// Truncates an over-long session id to the cache-key limit. JS `Array.from`
/// walks code points, so `chars()` is the exact counterpart.
#[must_use]
pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    let count = key.chars().count();
    if count <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(key.to_owned());
    }
    Some(
        key.chars()
            .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absent passes through, under-limit keys are untouched, and the cap
    /// counts code points (an astral-plane character is one).
    #[test]
    fn clamps_at_sixty_four_code_points() {
        assert_eq!(clamp_openai_prompt_cache_key(None), None);
        let short = "session-1";
        assert_eq!(
            clamp_openai_prompt_cache_key(Some(short)).as_deref(),
            Some(short)
        );
        let astral = "😀".repeat(64);
        assert_eq!(
            clamp_openai_prompt_cache_key(Some(&astral)).as_deref(),
            Some(astral.as_str())
        );
        let mut long = "x".repeat(70);
        long.truncate(64);
        assert_eq!(
            clamp_openai_prompt_cache_key(Some(&"x".repeat(70))).as_deref(),
            Some(long.as_str())
        );
    }
}
