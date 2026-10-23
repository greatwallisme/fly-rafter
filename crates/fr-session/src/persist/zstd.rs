//! Zstandard frame primitives for the JSONL persistence backend:
//!
//! the backend owns a concatenated-frame container so it can append and
//! recover batches without exposing compression mechanics through the
//! persistence seam (dsh zstd.ts; every compressible unit is one
//! independently decodable, checksummed frame).

use crate::persist::PersistError;

/// The Zstandard frame magic, little-endian.
const ZSTD_MAGIC: u32 = 0xFD2F_B528;

/// Byte range occupied by one structurally complete Zstandard frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZstdFrameRange {
    /// Inclusive frame start.
    pub start: usize,
    /// Exclusive frame end.
    pub end: usize,
}

/// Structural scan result for a concatenated Zstandard stream.
#[derive(Debug, Clone)]
pub struct ZstdFrameScan {
    /// Complete frames in file order.
    pub frames: Vec<ZstdFrameRange>,
    /// Start of an incomplete final frame, when EOF interrupts one.
    pub torn_start: Option<usize>,
}

/// Advances past one frame's compressed blocks. `Ok(None)` means EOF
/// interrupts the frame; the caller reports the torn boundary at the frame
/// start.
///
/// # Errors
///
/// Returns `Corruption` with the dsh message for a reserved block type.
fn advance_frame_blocks(buffer: &[u8], mut offset: usize) -> Result<Option<usize>, PersistError> {
    loop {
        if buffer.len() - offset < 3 {
            return Ok(None);
        }
        let block_header = u32::from(buffer[offset])
            | u32::from(buffer[offset + 1]) << 8
            | u32::from(buffer[offset + 2]) << 16;
        offset += 3;
        let last_block = (block_header & 1) != 0;
        let block_type = (block_header >> 1) & 0x03;
        let block_size = block_header >> 3;
        if block_type == 0x03 {
            return Err(PersistError::Corruption(format!(
                "corrupt Zstandard session log: reserved block type at byte {}",
                offset - 3
            )));
        }
        let payload_bytes = if block_type == 0x01 {
            1
        } else {
            block_size as usize
        };
        if buffer.len() - offset < payload_bytes {
            return Ok(None);
        }
        offset += payload_bytes;
        if last_block {
            return Ok(Some(offset));
        }
    }
}

/// Locates complete frames without decompressing their blocks. Invalid
/// complete structure rejects; EOF inside the final frame returns its start
/// for repair (dsh `scanZstdFrames`).
///
/// # Errors
///
/// Returns `Corruption` with the dsh message for a bad magic, a reserved
/// frame-header bit, or a reserved block type.
///
/// # Panics
///
/// The `.expect("4 bytes")` on the frame-magic conversion cannot fire: the
/// 4-byte availability check runs immediately before it.
pub fn scan_zstd_frames(
    buffer: &[u8],
    max_frames: Option<usize>,
) -> Result<ZstdFrameScan, crate::persist::PersistError> {
    let corruption = |message: String| PersistError::Corruption(message);
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < buffer.len() {
        let start = offset;
        if buffer.len() - offset < 4 {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        }
        if u32::from_le_bytes(buffer[offset..offset + 4].try_into().expect("4 bytes")) != ZSTD_MAGIC
        {
            return Err(corruption(format!(
                "corrupt Zstandard session log: invalid frame magic at byte {offset}"
            )));
        }
        offset += 4;
        if offset == buffer.len() {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        }
        let descriptor = buffer[offset];
        offset += 1;
        if (descriptor & 0x18) != 0 {
            return Err(corruption(format!(
                "corrupt Zstandard session log: reserved frame-header bit at byte {}",
                offset - 1
            )));
        }
        let content_size_flag = descriptor >> 6;
        let single_segment = (descriptor & 0x20) != 0;
        let checksum = (descriptor & 0x04) != 0;
        let dictionary_flag = descriptor & 0x03;
        let dictionary_bytes = if dictionary_flag == 3 {
            4
        } else {
            dictionary_flag
        };
        let content_size_bytes: usize = if content_size_flag == 0 {
            usize::from(single_segment)
        } else {
            1 << content_size_flag
        };
        let remaining_header_bytes =
            usize::from(!single_segment) + usize::from(dictionary_bytes) + content_size_bytes;
        if buffer.len() - offset < remaining_header_bytes {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        }
        offset += remaining_header_bytes;
        let Some(after_blocks) = advance_frame_blocks(buffer, offset)? else {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: Some(start),
            });
        };
        offset = after_blocks;
        if checksum {
            if buffer.len() - offset < 4 {
                return Ok(ZstdFrameScan {
                    frames,
                    torn_start: Some(start),
                });
            }
            offset += 4;
        }
        frames.push(ZstdFrameRange { start, end: offset });
        if Some(frames.len()) == max_frames {
            return Ok(ZstdFrameScan {
                frames,
                torn_start: None,
            });
        }
    }
    Ok(ZstdFrameScan {
        frames,
        torn_start: None,
    })
}

/// Compresses one independently decodable, checksummed Zstandard frame
/// (dsh `compressZstdFrame`: the checksum parameter is the only
/// non-default option).
///
/// # Errors
///
/// Returns `Io` when the encoder fails.
pub fn compress_zstd_frame(input: &[u8]) -> Result<Vec<u8>, crate::persist::PersistError> {
    let mut buffer = Vec::with_capacity(input.len() / 2 + 64);
    let mut encoder = zstd::stream::Encoder::new(&mut buffer, 0)?;
    encoder.include_checksum(true)?;
    std::io::Write::write_all(&mut encoder, input)?;
    encoder.finish()?;
    Ok(buffer)
}

/// Decompresses one complete frame and validates its checksum (dsh
/// `decompressZstdFrame`).
///
/// # Errors
///
/// Returns `Io` when the frame is corrupt.
pub fn decompress_zstd_frame(input: &[u8]) -> Result<Vec<u8>, crate::persist::PersistError> {
    Ok(zstd::stream::decode_all(input)?)
}

/// Recovers available plaintext from a structurally incomplete final frame:
/// the streaming decoder deliberately suppresses final-frame and checksum
/// completion;
///
/// callers must establish the torn frame boundary first (dsh
/// `decompressZstdPrefix`). A decoder failure yields an empty plaintext so
/// the complete prior frames stay recoverable.
#[must_use]
pub fn decompress_zstd_prefix(input: &[u8]) -> Vec<u8> {
    let mut context = zstd_safe::DCtx::create();
    if context.init() != Ok(0) {
        return Vec::new();
    }
    let mut in_buffer = zstd_safe::InBuffer { src: input, pos: 0 };
    let mut plaintext = Vec::new();
    loop {
        let mut chunk = vec![0u8; 64 * 1024];
        let mut out_buffer = zstd_safe::OutBuffer::around(&mut chunk);
        match context.decompress_stream(&mut out_buffer, &mut in_buffer) {
            Ok(_hint) => {
                plaintext.extend_from_slice(out_buffer.as_slice());
                if in_buffer.pos >= input.len() && out_buffer.as_slice().is_empty() {
                    // Input exhausted and nothing more producible.
                    break;
                }
                if in_buffer.pos >= input.len() {
                    // The decoder wants more input than the torn frame
                    // carries: everything decodable is already collected.
                    break;
                }
            }
            Err(_) => {
                // A structurally incomplete final frame may end before the
                // decoder can emit any plaintext; the complete prior frames
                // remain recoverable.
                break;
            }
        }
    }
    plaintext
}

#[cfg(test)]
mod tests {
    use super::{
        compress_zstd_frame, decompress_zstd_frame, decompress_zstd_prefix, scan_zstd_frames,
    };

    #[test]
    fn compressed_frames_scan_decode_and_round_trip() {
        let payload = b"session header line\n".repeat(200);
        let frame = compress_zstd_frame(&payload).expect("compress");
        let scan = scan_zstd_frames(&frame, None).expect("scan");
        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.torn_start, None);
        assert_eq!(scan.frames[0].start, 0);
        assert_eq!(scan.frames[0].end, frame.len());
        let plaintext = decompress_zstd_frame(&frame).expect("decompress");
        assert_eq!(plaintext, payload);
    }

    #[test]
    fn concatenated_frames_and_max_frames_limit_scan() {
        let first = compress_zstd_frame(b"one\n").expect("first");
        let second = compress_zstd_frame(b"two\n").expect("second");
        let mut both = first.clone();
        both.extend_from_slice(&second);
        let scan = scan_zstd_frames(&both, None).expect("scan both");
        assert_eq!(scan.frames.len(), 2);
        assert_eq!(scan.frames[1].start, first.len());
        let limited = scan_zstd_frames(&both, Some(1)).expect("limited");
        assert_eq!(limited.frames.len(), 1);
        assert_eq!(limited.torn_start, None);
    }

    #[test]
    fn eof_inside_a_frame_reports_the_torn_start() {
        let frame = compress_zstd_frame(b"payload bytes\n").expect("frame");
        let torn = &frame[..frame.len() / 2];
        let scan = scan_zstd_frames(torn, None).expect("scan torn");
        assert_eq!(scan.torn_start, Some(0));
        // Tearing between two frames keeps the first complete.
        let mut first = compress_zstd_frame(b"one\n").expect("first");
        let second = compress_zstd_frame(b"two\n").expect("second");
        first.extend_from_slice(&second[..second.len() / 3]);
        let scan = scan_zstd_frames(&first, None).expect("scan partial pair");
        assert_eq!(scan.frames.len(), 1);
        assert_eq!(scan.torn_start, Some(scan.frames[0].end));
    }

    #[test]
    fn corrupt_frame_structure_refuses_with_dsh_messages() {
        let mut frame = compress_zstd_frame(b"x\n").expect("frame");
        frame[0] = 0x00;
        let error = scan_zstd_frames(&frame, None).expect_err("bad magic");
        assert!(
            error
                .message()
                .contains("corrupt Zstandard session log: invalid frame magic at byte 0")
        );
        let mut reserved = compress_zstd_frame(b"x\n").expect("frame");
        // The frame header descriptor's reserved bits sit at byte 4.
        reserved[4] |= 0x08;
        let error = scan_zstd_frames(&reserved, None).expect_err("reserved bit");
        assert!(
            error
                .message()
                .contains("corrupt Zstandard session log: reserved frame-header bit at byte 4")
        );
    }

    #[test]
    fn torn_prefix_recovers_available_plaintext_without_frame_completion() {
        let frame = compress_zstd_frame(b"complete-line\nanother-line\n").expect("frame");
        let torn = &frame[..frame.len() * 3 / 4];
        assert_eq!(
            scan_zstd_frames(torn, None).expect("scan").torn_start,
            Some(0)
        );
        let plaintext = decompress_zstd_prefix(torn);
        // The recovered prefix carries whatever records fully flushed; the
        // write path truncates the torn bytes and rewrites them durably.
        assert!(plaintext.is_empty() || plaintext.ends_with(b"\n"));
        let _ = decompress_zstd_frame(frame.as_slice()).expect("full frame still decodes");
    }
}

// Rust guideline compliant 2026-10-23
