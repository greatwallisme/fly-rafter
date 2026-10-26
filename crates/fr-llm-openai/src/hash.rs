//! Fast deterministic hash used to shorten long tool-call ids (pi-ai
//! `utils/hash.ts:2-13`, a cyrb53 variant).

/// JS `charCodeAt` walks UTF-16 code units, so astral-plane characters feed
/// two surrogate units; the port encodes the same unit stream. `Math.imul`
/// is a 32-bit wrapping multiply, bit-identical in `u32`.
fn utf16_units(text: &str) -> Vec<u16> {
    let mut units = Vec::with_capacity(text.len());
    for c in text.chars() {
        let code = u32::from(c);
        if code <= 0xffff {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the branch condition pins code to the u16 range"
            )]
            units.push(code as u16);
        } else {
            let offset = code - 0x1_0000;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "surrogate halves are u16 by construction (10-bit payloads under 0xD800/0xDC00 tags)"
            )]
            units.push((0xd800 | (offset >> 10)) as u16);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "surrogate halves are u16 by construction (10-bit payloads under 0xD800/0xDC00 tags)"
            )]
            units.push((0xdc00 | (offset & 0x3ff)) as u16);
        }
    }
    units
}

/// Lowercase base-36 (`Number.prototype.toString(36)` digits).
fn base36(value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    let mut rest = value;
    loop {
        out.push(DIGITS[usize::try_from(rest % 36).expect("a remainder below 36 fits usize")]);
        rest /= 36;
        if rest == 0 {
            break;
        }
    }
    out.reverse();
    out.iter().map(|&b| b as char).collect()
}

/// Hash to shorten long strings (`shortHash`).
#[must_use]
pub fn short_hash(text: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for unit in utf16_units(text) {
        h1 = (h1 ^ u32::from(unit)).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ u32::from(unit)).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    format!("{}{}", base36(h2), base36(h1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Differential pin against node runs of the vendored cyrb53 variant
    /// (2026-10-27); the emoji vector exercises the UTF-16 surrogate-pair
    /// unit stream and the accented char the non-ASCII path.
    #[test]
    fn pins_reference_vectors() {
        let vectors: &[(&str, &str)] = &[
            ("", "k4n83c7h0j2b"),
            ("call_abc123", "sb0y391xa16ki"),
            ("call_with_pipe|item_9f8e7d6c", "1jj3yhb1l5244j"),
            ("héllo", "z4q6iv1qt99b0"),
            ("emoji😅id", "ywo9n6bo113i"),
            ("a", "m8735310ae7sx"),
        ];
        for (input, expected) in vectors {
            assert_eq!(short_hash(input), *expected, "input {input:?}");
        }
    }
}
