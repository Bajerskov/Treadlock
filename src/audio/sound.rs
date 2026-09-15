//! Loading recorded audio.
//!
//! WAV is read here directly. It is a handful of lines, it is what the game's
//! own tooling emits, and it needs no dependency. Everything else - MP3, OGG,
//! FLAC - goes through symphonia, which is pure Rust and so cross-compiles to
//! the target as cleanly as the rest of this.
//!
//! Clips are decoded whole into memory rather than streamed. A three minute
//! track is about 35 MB held this way, which is a fair trade on a board with
//! 16 GB; an hour of music would want streaming instead.

use std::path::Path;

/// Formats the game will try to play, and the ones symphonia is built with.
pub const EXTENSIONS: [&str; 6] = ["wav", "mp3", "ogg", "oga", "flac", "m4a"];

/// Decoded audio, always interleaved stereo at whatever rate the file had.
///
/// Held as 16-bit rather than float. The mixer wants floats and converting one
/// is a single multiply, but keeping an album as f32 costs twice the memory for
/// precision nothing downstream can hear.
pub struct Clip {
    pub samples: Vec<i16>,
    pub rate: f32,
}

impl Clip {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    pub fn seconds(&self) -> f32 {
        self.frames() as f32 / self.rate.max(1.0)
    }

    /// Linear interpolation at a fractional frame, which is how the mixer reads
    /// a clip whose rate does not match the device.
    pub fn sample(&self, frame: f32) -> (f32, f32) {
        const SCALE: f32 = 1.0 / 32768.0;
        let frames = self.frames();
        if frames == 0 {
            return (0.0, 0.0);
        }
        let i = frame.floor().max(0.0) as usize;
        if i + 1 >= frames {
            let last = (frames - 1) * 2;
            return (self.samples[last] as f32 * SCALE, self.samples[last + 1] as f32 * SCALE);
        }
        let t = frame - frame.floor();
        let a = i * 2;
        let b = a + 2;
        let lerp = |x: i16, y: i16| (x as f32 + (y as f32 - x as f32) * t) * SCALE;
        (lerp(self.samples[a], self.samples[b]), lerp(self.samples[a + 1], self.samples[b + 1]))
    }
}

/// Is this a file the game will try to play?
pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
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
        raw.into_iter().map(to_i16).collect()
    } else {
        // Mono is duplicated on load rather than branched on at every sample.
        raw.into_iter().flat_map(|s| [to_i16(s), to_i16(s)]).collect()
    };

    Ok(Clip { samples, rate: if rate > 0.0 { rate } else { 48_000.0 } })
}

/// Everything that is not a WAV.
///
/// Whatever symphonia hands back is folded to interleaved stereo: a mono track
/// is duplicated and anything wider keeps its first two channels, because the
/// mixer only knows how to place a stereo pair.
fn decode_compressed(bytes: Vec<u8>, extension: &str) -> Result<Clip, String> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let source = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if !extension.is_empty() {
        hint.with_extension(extension);
    }

    let mut reader = symphonia::default::get_probe()
        .probe(&hint, source, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("not an audio file this build can read ({e})"))?;

    let track = reader.first_track(TrackType::Audio).ok_or("no audio track in the file")?;
    let track_id = track.id;
    let params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => return Err("the audio track declares no codec".into()),
    };

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| format!("no decoder for this file ({e})"))?;

    let mut samples: Vec<i16> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut rate = params.sample_rate.unwrap_or(48_000) as f32;

    while let Some(packet) = reader.next_packet().map_err(|e| format!("{e}"))? {
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // One corrupt frame is worth skipping rather than losing the whole
            // track over. Anything worse stops the decode.
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("{e}")),
        };
        let channels = decoded.spec().channels().count().max(1);
        rate = decoded.spec().rate() as f32;

        // This resizes the destination rather than appending to it, so it needs
        // its own buffer and the result is copied on afterwards.
        decoded.copy_to_vec_interleaved(&mut scratch);
        match channels {
            1 => samples.extend(scratch.iter().flat_map(|&s| [to_i16(s), to_i16(s)])),
            2 => samples.extend(scratch.iter().map(|&s| to_i16(s))),
            n => samples
                .extend(scratch.chunks_exact(n).flat_map(|f| [to_i16(f[0]), to_i16(f[1])])),
        }
    }

    if samples.is_empty() {
        return Err("decoded to nothing".into());
    }
    Ok(Clip { samples, rate: if rate > 0.0 { rate } else { 48_000.0 } })
}

pub fn load(path: &Path) -> Result<Clip, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let result = if bytes.starts_with(b"RIFF") {
        decode(&bytes)
    } else {
        let extension =
            path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        decode_compressed(bytes, &extension)
    };
    result.map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(tag: u16, bits: u16, channels: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0u32.to_le_bytes()); // back-filled below
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
        // both unknown chunks and their pad byte. JUNK rather than LIST: LIST
        // has a required form type, and a malformed one is refused by stricter
        // readers than the one here.
        out.extend_from_slice(b"JUNK");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&[1, 2, 3, 0]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        // Back-fill the RIFF length. The reader here ignores it, but a real
        // file always has it right and symphonia refuses a file that does not.
        let riff = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff.to_le_bytes());
        out
    }

    /// Peak of the decoded clip, as the mixer would read it.
    fn peak(clip: &Clip) -> f32 {
        (0..clip.frames())
            .map(|f| {
                let (l, r) = clip.sample(f as f32);
                l.abs().max(r.abs())
            })
            .fold(0.0f32, f32::max)
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
        let (l, r) = clip.sample(0.0);
        assert!(l.abs() < 0.001, "first sample was {l}");
        assert!((r - 0.5).abs() < 0.002, "second sample was {r}");
        let (l, _) = clip.sample(1.0);
        assert!((l + 0.5).abs() < 0.002, "third sample was {l}");
    }

    #[test]
    fn mono_is_widened_to_stereo() {
        let body: Vec<u8> = [8192i16, -8192].iter().flat_map(|v| v.to_le_bytes()).collect();
        let clip = decode(&build(1, 16, 1, &body)).expect("decode failed");
        assert_eq!(clip.frames(), 2);
        assert_eq!(clip.samples[0], clip.samples[1]);
        let (l, r) = clip.sample(0.0);
        assert!((l - 0.25).abs() < 0.002 && (r - 0.25).abs() < 0.002);
    }

    #[test]
    fn interpolates_between_frames_and_holds_at_the_end() {
        let full = i16::MAX;
        let clip = Clip { samples: vec![0, 0, full, -full], rate: 48_000.0 };
        let (l, r) = clip.sample(0.5);
        assert!((l - 0.5).abs() < 0.002 && (r + 0.5).abs() < 0.002);
        let (l, r) = clip.sample(99.0);
        assert!((l - 1.0).abs() < 0.001 && (r + 1.0).abs() < 0.001);
    }

    /// A damaged or unsupported file has to be refused, not played as noise.
    #[test]
    fn rubbish_is_rejected() {
        assert!(decode(b"not a wav at all").is_err());
        assert!(decode(&build(1, 16, 7, &[0; 8])).is_err(), "7 channels accepted");
        assert!(decode(&build(99, 16, 2, &[0; 8])).is_err(), "unknown tag accepted");
    }

    /// The symphonia path, end to end, against the reader written here.
    ///
    /// There is no encoder on this machine to make an MP3 with, so the codec
    /// itself cannot be exercised - but the codec is symphonia's code. What is
    /// this project's code is everything around it: probing, finding the track,
    /// draining packets, and folding whatever comes back into interleaved
    /// stereo. All of that is the same for a WAV as for an MP3, so running a
    /// WAV through it and requiring the two decoders to agree tests the part
    /// that can actually be wrong here.
    #[test]
    fn the_symphonia_path_agrees_with_the_built_in_reader() {
        // A second of a quiet tone, so a channel swap or a dropped packet shows
        // up as a difference rather than as matching silence.
        let mut body = Vec::new();
        for i in 0..48_000 {
            let t = i as f32 / 48_000.0;
            let left = ((t * 220.0 * std::f32::consts::TAU).sin() * 12_000.0) as i16;
            let right = ((t * 330.0 * std::f32::consts::TAU).sin() * 9_000.0) as i16;
            body.extend_from_slice(&left.to_le_bytes());
            body.extend_from_slice(&right.to_le_bytes());
        }
        let wav = build(1, 16, 2, &body);

        let mine = decode(&wav).expect("the built-in reader failed");
        let theirs = decode_compressed(wav, "wav").expect("the symphonia path failed");

        assert_eq!(theirs.rate, mine.rate, "the two decoders disagree on the sample rate");
        assert_eq!(theirs.frames(), mine.frames(), "the two decoders returned different lengths");
        assert!(peak(&mine) > 0.2, "the test signal was too quiet to prove anything");

        let mut worst = 0.0f32;
        for frame in 0..mine.frames() {
            let (al, ar) = mine.sample(frame as f32);
            let (bl, br) = theirs.sample(frame as f32);
            worst = worst.max((al - bl).abs()).max((ar - br).abs());
        }
        assert!(worst < 0.002, "the two decoders differ by {worst}, so one is wrong");
    }

    /// Anything that is not audio has to be refused by both paths, with a
    /// message rather than a panic.
    #[test]
    fn the_symphonia_path_refuses_rubbish() {
        assert!(decode_compressed(vec![0u8; 4096], "mp3").is_err());
        assert!(decode_compressed(b"this is a text file".to_vec(), "ogg").is_err());
        assert!(decode_compressed(Vec::new(), "").is_err());
    }

    /// The loader has to recognise what the game will try to play, and only
    /// that: a stray readme in the music folder is not a track.
    #[test]
    fn audio_files_are_recognised_by_extension() {
        for good in ["a.mp3", "a.MP3", "b.wav", "c.ogg", "d.flac", "e.m4a"] {
            assert!(is_audio(Path::new(good)), "{good} was not recognised as audio");
        }
        for bad in ["readme.txt", "cover.jpg", "track", "a.mp3.bak"] {
            assert!(!is_audio(Path::new(bad)), "{bad} was taken for audio");
        }
    }
}
