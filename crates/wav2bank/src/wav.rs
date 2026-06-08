//! Minimal RIFF/WAVE parsing and forward-loop injection.
//!
//! The DS has no concept of "background music" separate from a sample: a PCM
//! WAV played through maxmod's effect path stops after one shot unless the file
//! carries a loop. `mmutil` reads loop points from the WAVE `smpl` chunk, so to
//! make a sample loop as music we append a `smpl` chunk describing a single
//! forward loop spanning the whole sample.
//!
//! This module is intentionally pure (bytes in, bytes out) so it can be unit
//! tested on the host without `mmutil` or DS hardware.

/// The shape of a WAVE file we care about: the audio format from `fmt ` and the
/// size of the `data` chunk, enough to compute the sample-frame count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    /// Channel count (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Bits per sample (8 or 16 for maxmod).
    pub bits_per_sample: u16,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Byte length of the `data` chunk payload.
    pub data_len: u32,
}

impl WavInfo {
    /// Number of sample frames (one frame = one sample across all channels).
    pub fn frames(&self) -> u32 {
        let bytes_per_frame = u32::from(self.channels) * u32::from(self.bits_per_sample) / 8;
        if bytes_per_frame == 0 {
            0
        } else {
            self.data_len / bytes_per_frame
        }
    }
}

/// Read a little-endian `u32` at `off`, or `None` if out of range.
fn read_u32(bytes: &[u8], off: usize) -> Option<u32> {
    bytes
        .get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Read a little-endian `u16` at `off`, or `None` if out of range.
fn read_u16(bytes: &[u8], off: usize) -> Option<u16> {
    bytes
        .get(off..off + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

/// Parse the `fmt ` and `data` chunks of a RIFF/WAVE file.
pub fn parse(bytes: &[u8]) -> Result<WavInfo, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }

    let mut channels = None;
    let mut bits = None;
    let mut rate = None;
    let mut data_len = None;

    // Walk the chunk list after the 12-byte RIFF header.
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = read_u32(bytes, pos + 4).ok_or("truncated chunk header")? as usize;
        let body = pos + 8;

        if id == b"fmt " {
            channels = read_u16(bytes, body + 2);
            rate = read_u32(bytes, body + 4);
            bits = read_u16(bytes, body + 14);
        } else if id == b"data" {
            data_len = Some(size as u32);
        }

        // Chunks are word-aligned: an odd size carries a pad byte.
        pos = body + size + (size & 1);
    }

    Ok(WavInfo {
        channels: channels.ok_or("missing fmt chunk")?,
        bits_per_sample: bits.ok_or("missing fmt chunk")?,
        sample_rate: rate.ok_or("missing fmt chunk")?,
        data_len: data_len.ok_or("missing data chunk")?,
    })
}

/// True if the file already contains a `smpl` chunk (so we don't add a second).
pub fn has_smpl(bytes: &[u8]) -> bool {
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        if &bytes[pos..pos + 4] == b"smpl" {
            return true;
        }
        let Some(size) = read_u32(bytes, pos + 4) else {
            break;
        };
        let size = size as usize;
        pos = pos + 8 + size + (size & 1);
    }
    false
}

/// Build a `smpl` chunk describing one forward loop over `[0, frames)`.
///
/// Layout per the WAVE `smpl` spec: a 36-byte fixed header followed by one
/// 24-byte loop record. `play_count = 0` means loop forever.
fn smpl_chunk(info: &WavInfo) -> Vec<u8> {
    let frames = info.frames();
    // Loop end is the index of the last sample frame.
    let loop_end = frames.saturating_sub(1);
    // Sample period in nanoseconds = 1e9 / sample_rate.
    let sample_period = if info.sample_rate == 0 {
        0
    } else {
        1_000_000_000u32 / info.sample_rate
    };

    let mut body = Vec::with_capacity(60);
    let push = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    push(&mut body, 0); // manufacturer
    push(&mut body, 0); // product
    push(&mut body, sample_period); // sample period (ns)
    push(&mut body, 60); // MIDI unity note (middle C)
    push(&mut body, 0); // MIDI pitch fraction
    push(&mut body, 0); // SMPTE format
    push(&mut body, 0); // SMPTE offset
    push(&mut body, 1); // number of sample loops
    push(&mut body, 0); // sampler-specific data length
    // Loop record.
    push(&mut body, 0); // cue point id
    push(&mut body, 0); // type: 0 = forward
    push(&mut body, 0); // start frame
    push(&mut body, loop_end); // end frame
    push(&mut body, 0); // fraction
    push(&mut body, 0); // play count: 0 = infinite
    debug_assert_eq!(body.len(), 60);

    let mut chunk = Vec::with_capacity(8 + body.len());
    chunk.extend_from_slice(b"smpl");
    chunk.extend_from_slice(&(body.len() as u32).to_le_bytes());
    chunk.extend_from_slice(&body);
    chunk
}

/// Return `bytes` with a forward-loop `smpl` chunk appended, so `mmutil` marks
/// the sample as looping. If the file already has a `smpl` chunk it is returned
/// unchanged. The trailing `RIFF` size field is fixed up to cover the addition.
pub fn inject_forward_loop(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let info = parse(bytes)?;
    if has_smpl(bytes) {
        return Ok(bytes.to_vec());
    }

    let chunk = smpl_chunk(&info);
    let mut out = Vec::with_capacity(bytes.len() + chunk.len());
    out.extend_from_slice(bytes);
    // Word-align before appending if the file length is odd.
    if out.len() & 1 == 1 {
        out.push(0);
    }
    out.extend_from_slice(&chunk);

    // RIFF size = total file size minus the 8-byte "RIFF<size>" prefix.
    let riff_size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal mono 8-bit WAV with `n` sample bytes.
    fn make_wav(rate: u32, n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&((36 + n) as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes()); // byte rate
        v.extend_from_slice(&1u16.to_le_bytes()); // block align
        v.extend_from_slice(&8u16.to_le_bytes()); // bits
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(n as u32).to_le_bytes());
        v.extend(core::iter::repeat(0x80u8).take(n));
        v
    }

    #[test]
    fn parses_format_and_frames() {
        let wav = make_wav(16000, 100);
        let info = parse(&wav).unwrap();
        assert_eq!(info.channels, 1);
        assert_eq!(info.bits_per_sample, 8);
        assert_eq!(info.sample_rate, 16000);
        assert_eq!(info.data_len, 100);
        assert_eq!(info.frames(), 100);
    }

    #[test]
    fn rejects_non_wav() {
        assert!(parse(b"not a wav at all").is_err());
    }

    #[test]
    fn injects_a_loop_and_fixes_riff_size() {
        let wav = make_wav(16000, 100);
        assert!(!has_smpl(&wav));
        let looped = inject_forward_loop(&wav).unwrap();
        assert!(has_smpl(&looped));
        // The appended chunk is 8 + 60 bytes.
        assert_eq!(looped.len(), wav.len() + 68);
        // RIFF size must equal total length minus 8.
        let riff = read_u32(&looped, 4).unwrap() as usize;
        assert_eq!(riff, looped.len() - 8);
        // The data is still parseable and unchanged in shape.
        let info = parse(&looped).unwrap();
        assert_eq!(info.frames(), 100);
    }

    #[test]
    fn loop_end_is_last_frame() {
        let wav = make_wav(8000, 50);
        let looped = inject_forward_loop(&wav).unwrap();
        // Find the smpl chunk and read its single loop record's end field.
        let mut pos = 12usize;
        let mut end = None;
        while pos + 8 <= looped.len() {
            if &looped[pos..pos + 4] == b"smpl" {
                // body starts at pos+8; loop record at +36; end at +12 of record.
                let rec = pos + 8 + 36;
                end = read_u32(&looped, rec + 12);
                break;
            }
            let size = read_u32(&looped, pos + 4).unwrap() as usize;
            pos = pos + 8 + size + (size & 1);
        }
        assert_eq!(end, Some(49));
    }

    #[test]
    fn injection_is_idempotent() {
        let wav = make_wav(16000, 100);
        let once = inject_forward_loop(&wav).unwrap();
        let twice = inject_forward_loop(&once).unwrap();
        assert_eq!(once, twice, "should not append a second smpl chunk");
    }
}
