//! A small WAV reader, so recorded assets can be dropped in without adding an
//! audio-decoding dependency to a project that is otherwise dependency-light.
//!
//! Handles the formats a recording actually arrives in: 16- and 24-bit PCM and
//! 32-bit float, mono or stereo. Anything else is refused by name rather than
//! played as noise.

/// Decoded audio, always interleaved stereo at whatever rate the file had.
pub struct Clip {
    pub samples: Vec<f32>,
    pub rate: f32,
}

impl Clip {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    /// Linear interpolation at a fractional frame, which is how the mixer reads
    /// a clip whose rate does not match the device.
    pub fn sample(&self, frame: f32) -> (f32, f32) {
        let frames = self.frames();
        if frames == 0 {
            return (0.0, 0.0);
        }
        let i = frame.floor().max(0.0) as usize;
        if i + 1 >= frames {
            let last = (frames - 1) * 2;
            return (self.samples[last], self.samples[last + 1]);
        }
        let t = frame - frame.floor();
        let a = i * 2;
        let b = a + 2;
        (
            self.samples[a] + (self.samples[b] - self.samples[a]) * t,
            self.samples[a + 1] + (self.samples[b + 1] - self.samples[a + 1]) * t,
        )
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

pub fn decode(bytes: &[u8]) -> Result<Clip, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }

    let mut format = None;
    let mut data = None;
    let mut at = 12;
    // Walk the chunk list rather than assuming fmt then data: real files carry
    // LIST and fact chunks between them, and editors add their own.
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(bytes, at + 4).ok_or("truncated chunk header")? as usize;
        let body = at + 8;
        let end = body.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " => format = Some((body, end)),
            b"data" => data = Some((body, end)),
            _ => {}
        }
        // Chunks are word aligned, and an odd-sized one is followed by a pad
        // byte that is not counted in its size.
        at = body + size + (size & 1);
    }

    let (fmt_at, _) = format.ok_or("no fmt chunk")?;
    let (data_at, data_end) = data.ok_or("no data chunk")?;

    let tag = u16_at(bytes, fmt_at).ok_or("short fmt chunk")?;
    let channels = u16_at(bytes, fmt_at + 2).ok_or("short fmt chunk")? as usize;
    let rate = u32_at(bytes, fmt_at + 4).ok_or("short fmt chunk")? as f32;
    let bits = u16_at(bytes, fmt_at + 14).ok_or("short fmt chunk")? as usize;

    if channels == 0 || channels > 2 {
        return Err(format!("{channels} channels; only mono and stereo are supported"));
    }

    let body = &bytes[data_at..data_end];
    // 0xFFFE is WAVE_FORMAT_EXTENSIBLE, whose real format lives in a subformat
    // GUID; for the 16-bit PCM case it always means PCM, which is the only way
    // it turns up from a normal export.
    let raw: Vec<f32> = match (tag, bits) {
        (1, 16) | (0xFFFE, 16) => body
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (1, 24) | (0xFFFE, 24) => body
            .chunks_exact(3)
            .map(|c| {
                // Sign-extend by putting the three bytes in the top of an i32.
                let v = i32::from_le_bytes([0, c[0], c[1], c[2]]);
                v as f32 / 2_147_483_648.0
            })
            .collect(),
        (3, 32) | (0xFFFE, 32) => body
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        (1, 8) => body.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        _ => return Err(format!("format tag {tag} at {bits} bits is not supported")),
    };

    let samples = if channels == 2 {
        raw
    } else {
        // Mono is duplicated on load rather than branched on at every sample.
        raw.iter().flat_map(|&s| [s, s]).collect()
    };

    Ok(Clip { samples, rate: if rate > 0.0 { rate } else { 48_000.0 } })
}

pub fn load(path: &std::path::Path) -> Result<Clip, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(tag: u16, bits: u16, channels: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&48_000u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        // An odd-sized chunk between fmt and data, to prove the walk handles
        // both unknown chunks and their pad byte.
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&[1, 2, 3, 0]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn decodes_sixteen_bit_stereo_past_an_unknown_chunk() {
        let body: Vec<u8> = [0i16, 16384, -16384, 32767]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let clip = decode(&build(1, 16, 2, &body)).expect("decode failed");
        assert_eq!(clip.frames(), 2);
        assert_eq!(clip.rate, 48_000.0);
        assert!((clip.samples[1] - 0.5).abs() < 0.001);
        assert!((clip.samples[2] + 0.5).abs() < 0.001);
    }

    #[test]
    fn mono_is_widened_to_stereo() {
        let body: Vec<u8> = [8192i16, -8192].iter().flat_map(|v| v.to_le_bytes()).collect();
        let clip = decode(&build(1, 16, 1, &body)).expect("decode failed");
        assert_eq!(clip.frames(), 2);
        assert_eq!(clip.samples[0], clip.samples[1]);
        assert!((clip.samples[0] - 0.25).abs() < 0.001);
    }

    #[test]
    fn interpolates_between_frames_and_holds_at_the_end() {
        let clip = Clip { samples: vec![0.0, 0.0, 1.0, -1.0], rate: 48_000.0 };
        let (l, r) = clip.sample(0.5);
        assert!((l - 0.5).abs() < 0.001 && (r + 0.5).abs() < 0.001);
        assert_eq!(clip.sample(99.0), (1.0, -1.0));
    }

    /// A damaged or unsupported file has to be refused, not played as noise.
    #[test]
    fn rubbish_is_rejected() {
        assert!(decode(b"not a wav at all").is_err());
        assert!(decode(&build(1, 16, 7, &[0; 8])).is_err(), "7 channels accepted");
        assert!(decode(&build(99, 16, 2, &[0; 8])).is_err(), "unknown tag accepted");
    }
}
