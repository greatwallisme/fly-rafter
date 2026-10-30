//! The shared base64 codec for byte payloads crossing the dynamic
//! channel (the fr byte-transport spelling): the standard RFC 4648
//! alphabet with padding, the shape the fs byte reads already ride.
//!
//! The kernel-family service faces carry JSON; binary payloads ride
//! this encoding inside the JSON document (the `attachments` save
//! face's image bytes, the fs byte reads).

const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes one byte slice as padded standard base64.
#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map_or(0, |byte| u32::from(*byte));
        let b2 = chunk.get(2).map_or(0, |byte| u32::from(*byte));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The base64 decode failure: which character position was invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    /// The zero-based index of the offending character.
    pub position: usize,
}

/// Decodes one padded standard base64 string; whitespace and invalid
/// characters reject with the first offending position.
///
/// # Errors
///
/// [`DecodeError`] at the first invalid character or a dangling
/// non-canonical tail.
pub fn decode_base64(text: &str) -> Result<Vec<u8>, DecodeError> {
    const fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut index = 0;
    while index < bytes.len() {
        let end = (index + 4).min(bytes.len());
        let window = &bytes[index..end];
        // Canonical text carries only full four-character groups; a
        // dangling short tail is non-canonical.
        if window.len() != 4 {
            return Err(DecodeError { position: index });
        }
        let mut sextets = [0_u8; 4];
        let mut written = 0;
        for (offset, &byte) in window.iter().enumerate() {
            if byte == b'=' {
                // Padding is legal only as the window's last one or two
                // characters.
                if offset + 1 != window.len() && offset + 2 != window.len() {
                    return Err(DecodeError {
                        position: index + offset,
                    });
                }
                for &tail in &window[offset..] {
                    if tail != b'=' {
                        return Err(DecodeError {
                            position: index + offset,
                        });
                    }
                }
                break;
            }
            let Some(decoded) = value(byte) else {
                return Err(DecodeError {
                    position: index + offset,
                });
            };
            sextets[written] = decoded;
            written += 1;
        }
        match written {
            0 => {}
            1 => {
                return Err(DecodeError { position: index });
            }
            2 => out.push(sextets[0] << 2 | sextets[1] >> 4),
            3 => {
                out.push(sextets[0] << 2 | sextets[1] >> 4);
                out.push(sextets[1] << 4 | sextets[2] >> 2);
            }
            _ => {
                out.push(sextets[0] << 2 | sextets[1] >> 4);
                out.push(sextets[1] << 4 | sextets[2] >> 2);
                out.push(sextets[2] << 6 | sextets[3]);
            }
        }
        index = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{decode_base64, encode_base64};

    #[test]
    fn round_trips_known_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foob"), "Zm9vYg==");
        assert_eq!(encode_base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
        for vector in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0_u8, 255, 10, 128, 7],
        ] {
            assert_eq!(
                decode_base64(&encode_base64(vector)).expect("decodes"),
                vector
            );
        }
    }

    #[test]
    fn decode_rejects_invalid_characters_and_tails() {
        let _ = decode_base64("Zg==x").unwrap_err();
        let _ = decode_base64("Z*==").unwrap_err();
        let _ = decode_base64("Z").unwrap_err();
        let _ = decode_base64("Zg=").unwrap_err();
        let error = decode_base64("Zg*=Zm9v").expect_err("first bad position");
        assert_eq!(error.position, 2);
    }
}
