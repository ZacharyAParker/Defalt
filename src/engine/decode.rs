//! Getting a file off the disk and into memory as plain stereo f32.
//!
//! Decoding happens on whatever thread asks for it, never on the audio
//! thread. A deck is handed a finished `Arc<Track>` and does nothing but read
//! from it.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Decoded audio, always two channels, always interleaved.
///
/// Stereo regardless of the source: mono is duplicated and anything wider is
/// cut to the front pair. A deck that had to ask how many channels it was
/// holding would have to ask once per sample.
pub struct Track {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Track {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    pub fn seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.frames() as f64 / self.sample_rate as f64
    }

    /// One frame, or silence past either end.
    #[inline]
    pub fn frame(&self, index: isize) -> [f32; 2] {
        if index < 0 {
            return [0.0, 0.0];
        }
        let index = index as usize;
        if index >= self.frames() {
            return [0.0, 0.0];
        }
        [self.samples[index * 2], self.samples[index * 2 + 1]]
    }
}

/// The rate the output device is running at, or 0 before one has opened.
///
/// Set by the engine whenever it opens (or reopens) a device, and read by
/// `load`, so every decode thread converts to the right rate without anyone
/// having to pass it along.
static OUTPUT_RATE: AtomicU32 = AtomicU32::new(0);

/// Tell the decoders what rate the device runs at. The engine calls this; the
/// app has no reason to.
pub fn set_output_rate(rate: u32) {
    OUTPUT_RATE.store(rate, Ordering::Relaxed);
}

/// The rate `load` converts to, or 0 when no device is open (tests, or a
/// console started without audio), in which case it converts nothing.
pub fn output_rate() -> u32 {
    OUTPUT_RATE.load(Ordering::Relaxed)
}

/// Decode a file and convert it to the output device's rate.
///
/// Conversion happens here, once, with a proper filter, rather than on every
/// callback with the deck's cubic reader. The returned track's `sample_rate`
/// is the device rate, so stems loaded the same way still match their record.
/// With no device open the file comes back at its own rate.
pub fn load(path: impl AsRef<Path>) -> Result<Arc<Track>, String> {
    load_at(path, output_rate())
}

/// Decode a file and convert it to `rate`; 0 means leave it as it is.
pub fn load_at(path: impl AsRef<Path>, rate: u32) -> Result<Arc<Track>, String> {
    let track = decode(path.as_ref())?;
    if rate == 0 || rate == track.sample_rate {
        return Ok(Arc::new(track));
    }
    let samples = super::resample::stereo(&track.samples, track.sample_rate, rate);
    drop(track);
    Ok(Arc::new(Track { samples, sample_rate: rate }))
}

/// Decode a file at its own rate, with no conversion.
#[cfg_attr(not(test), allow(dead_code))]
pub fn load_native(path: impl AsRef<Path>) -> Result<Arc<Track>, String> {
    decode(path.as_ref()).map(Arc::new)
}

fn decode(path: &Path) -> Result<Track, String> {
    let file = File::open(path).map_err(|error| format!("{path:?}: {error}"))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|error| format!("unreadable: {error}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio in that file")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|codec| codec.audio())
        .ok_or("no audio codec parameters")?
        .clone();

    let sample_rate = params.sample_rate.ok_or("unknown sample rate")?;
    let expected_frames = track.num_frames;

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|error| {
            // Name the format. "unsupported codec" on its own sends you
            // looking at the file when the answer is the extension.
            let kind = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            match kind.as_deref() {
                Some("opus") => "Opus is not supported. Re-request the record                                  and it will be saved as FLAC."
                    .to_string(),
                Some(ext) => format!("cannot decode {ext}: {error}"),
                None => format!("cannot decode this file: {error}"),
            }
        })?;

    // Sized from the container when it says how long the record is, so a
    // five minute record is one allocation and a two second sounder is not
    // handed four minutes of memory. Without a length, start at half a
    // minute and let it grow.
    let mut samples: Vec<f32> = Vec::with_capacity(match expected_frames {
        Some(frames) => (frames as usize).saturating_add(4096).saturating_mul(2),
        None => sample_rate as usize * 2 * 30,
    });
    let mut packet_buffer: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // Some demuxers still report the end of the file as an
            // unexpected EOF. That is the end, not a fault.
            Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            // Anything else is a read that failed part way, and playing the
            // half that arrived as if it were the whole record is worse than
            // saying so.
            Err(error) => return Err(format!("read failed: {error}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                // The buffer knows how wide it is; the header is only a
                // claim, and a wrong claim folds the wrong samples together.
                let channels = decoded.spec().channels().count().max(1);
                packet_buffer.resize(decoded.samples_interleaved(), 0.0);
                decoded.copy_to_slice_interleaved(&mut packet_buffer);
                widen(&packet_buffer, channels, &mut samples);
            }
            // A damaged packet is a click, not a reason to lose the record.
            Err(Error::DecodeError(_)) => continue,
            Err(error) => return Err(format!("decode failed: {error}")),
        }
    }

    if samples.is_empty() {
        return Err("decoded to nothing".into());
    }

    samples.shrink_to_fit();
    Ok(Track { samples, sample_rate })
}

/// Fold an interleaved buffer of any width into stereo.
fn widen(source: &[f32], channels: usize, out: &mut Vec<f32>) {
    let start = out.len();
    widen_raw(source, channels, out);
    // A corrupt frame can decode to NaN or infinity, and one of those in a
    // filter or the reverb poisons it for good -- the whole console goes
    // silent. Silence here instead, for that sample only.
    for sample in out[start..].iter_mut() {
        if !sample.is_finite() {
            *sample = 0.0;
        }
    }
}

fn widen_raw(source: &[f32], channels: usize, out: &mut Vec<f32>) {
    match channels {
        1 => {
            out.reserve(source.len() * 2);
            for &sample in source {
                out.push(sample);
                out.push(sample);
            }
        }
        2 => out.extend_from_slice(source),
        n => {
            out.reserve(source.len() / n * 2);
            for frame in source.chunks_exact(n) {
                out.push(frame[0]);
                out.push(frame[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_finite_sample_decodes_as_silence() {
        let mut out = Vec::new();
        widen(&[0.5, f32::NAN, f32::INFINITY, -0.25], 2, &mut out);
        assert_eq!(out, [0.5, 0.0, 0.0, -0.25]);
        let mut mono = Vec::new();
        widen(&[f32::NEG_INFINITY], 1, &mut mono);
        assert_eq!(mono, [0.0, 0.0]);
    }

    /// Every record in the library, actually decoded.
    ///
    /// Unit tests prove the folding; this proves the codec list. It exists
    /// because a request was once saved as Opus, which symphonia cannot
    /// decode at all, and nothing caught it until a deck refused to load.
    #[test]
    #[ignore = "needs the project's library"]
    fn every_record_in_the_library_decodes() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let records = crate::library::load(&root).expect("could not read the library");
        assert!(!records.is_empty(), "no records to check");

        let mut failed = Vec::new();
        for record in &records {
            let extension = record
                .file
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("?")
                .to_string();
            match load(&record.file) {
                Ok(track) => println!(
                    "  ok    {:<6} {:>7.1}s  {}",
                    extension,
                    track.seconds(),
                    record.label()
                ),
                Err(error) => {
                    println!("  FAIL  {extension:<6}          {}: {error}", record.label());
                    failed.push(record.label());
                }
            }
        }
        assert!(failed.is_empty(), "these would not decode: {failed:?}");
    }

    /// Everything the station has cached, actually decoded.
    ///
    /// The library test above covers records you own. This covers the other
    /// half, and it is the half that broke: the station downloaded and
    /// rendered its working audio to Opus, which no deck here can play, so
    /// the schedule was full of items the console could show and never
    /// sound. Anything that cannot be decoded is a record the station can
    /// only play in a browser, which is the thing this is meant to stop.
    #[test]
    #[ignore = "needs the project's cache"]
    fn every_record_the_station_cached_decodes() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cache = root.join("cache").join("audio");
        let Ok(entries) = std::fs::read_dir(&cache) else {
            println!("no cache to check at {}", cache.display());
            return;
        };

        let mut failed = Vec::new();
        let mut checked = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            checked += 1;
            match load(&path) {
                Ok(track) => {
                    if checked <= 3 {
                        println!("  ok    {:>7.1}s  {name}", track.seconds());
                    }
                }
                Err(error) => {
                    println!("  FAIL  {name}: {error}");
                    failed.push(name);
                }
            }
        }
        println!("  {checked} cached files checked");
        assert!(failed.is_empty(), "the station cached audio it cannot play: {failed:?}");
    }

    #[test]
    fn mono_is_duplicated_across_both_channels() {
        let mut out = Vec::new();
        widen(&[0.5, -0.25], 1, &mut out);
        assert_eq!(out, vec![0.5, 0.5, -0.25, -0.25]);
    }

    #[test]
    fn wide_sources_keep_the_front_pair() {
        let mut out = Vec::new();
        // 5.1: L R C LFE Ls Rs
        widen(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 6, &mut out);
        assert_eq!(out, vec![1.0, 2.0]);
    }

    /// A 16-bit PCM WAV of a quiet ramp, written by hand.
    fn wav(path: &std::path::Path, channels: u16, rate: u32, frames: u32) {
        use std::io::Write;
        let data = frames * channels as u32 * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data.to_le_bytes());
        for frame in 0..frames {
            let value = ((frame as f32 / frames as f32) * 8_000.0) as i16;
            for _ in 0..channels { bytes.extend_from_slice(&value.to_le_bytes()); }
        }
        std::fs::File::create(path).unwrap().write_all(&bytes).unwrap();
    }

    #[test]
    fn a_mono_file_decodes_to_stereo_at_its_own_rate_or_the_one_asked_for() {
        let path = std::env::temp_dir().join(format!("defalt-decode-{}.wav", std::process::id()));
        wav(&path, 1, 24_000, 2_400);
        let native = load_at(&path, 0).unwrap();
        assert_eq!(native.sample_rate, 24_000);
        assert_eq!(native.frames(), 2_400);
        assert_eq!(native.samples[200], native.samples[201], "mono was not duplicated");
        let converted = load_at(&path, 48_000).unwrap();
        assert_eq!(converted.sample_rate, 48_000);
        assert_eq!(converted.frames(), 4_800);
        assert!((converted.seconds() - native.seconds()).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_is_an_error_not_an_empty_record() {
        assert!(load_at("definitely/not/here.wav", 48_000).is_err());
    }

    #[test]
    fn reads_past_either_end_are_silent_rather_than_a_panic() {
        let track = Track { samples: vec![1.0, 1.0], sample_rate: 48_000 };
        assert_eq!(track.frame(-1), [0.0, 0.0]);
        assert_eq!(track.frame(0), [1.0, 1.0]);
        assert_eq!(track.frame(1), [0.0, 0.0]);
    }
}
