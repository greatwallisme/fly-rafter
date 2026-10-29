// Rust guideline compliant 2026-10-29
//! Wire-face helpers shared by the three service carriers (the
//! `'subprocess'` low band, the `'jobs'` 1000+ band, and the `'shell'`
//! 2000+ band).
//!
//! The full method faces land with the executors module; the JS-number
//! rendering helper serves every validation text.

/// Renders one finite number the way dsh `JSON.stringify` does for the
/// error texts.
///
/// Integers render without a decimal tail, everything else in Rust's
/// shortest round-trip form (the f64 Display judgment from the
/// fr-session stateVersion precedent).
#[must_use]
pub fn js_number(value: f64) -> String {
    if value.to_bits() & !(1_u64 << 63) == 0 {
        return "0".to_owned();
    }
    if value.fract() == 0.0 && value.abs() < 1e21 {
        let truncated = value.trunc();
        // Exact equality is the branch predicate itself: trunc() on an
        // integral value is the identity by construction.
        #[expect(
            clippy::float_cmp,
            reason = "trunc() on an integral value is the identity, so the equality is the branch's own definition"
        )]
        if truncated == value {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the branch bounds the value under 1e21 and integral, so the i128 cast keeps every digit"
            )]
            let as_int = truncated as i128;
            return as_int.to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::js_number;

    #[test]
    fn integers_render_without_decimal_tail() {
        assert_eq!(js_number(120_000.0), "120000");
        assert_eq!(js_number(-3.0), "-3");
        assert_eq!(js_number(0.0), "0");
        assert_eq!(js_number(2_147_483_647.0), "2147483647");
    }

    #[test]
    fn non_integers_round_trip() {
        assert_eq!(js_number(1.5), "1.5");
        assert_eq!(js_number(-0.5), "-0.5");
    }
}
