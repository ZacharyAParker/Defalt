//! Getting a file off the disk and into memory as plain stereo f32.
//!
//! Decoding happens on whatever thread asks for it, never on the audio
//! thread. A deck is handed a finished `Arc<Track>` and does nothing but read
//! from it.

use std::fs::File;
use std::path::Path;
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

pub fn load(path: impl AsRef<Path>) -> Result<Arc<Track>, String> {
    let path = path.as_ref();
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
    let channels = params.channels.as_ref().map_or(2, |set| set.count()).max(1);

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

    // Rough guess so the common case does not reallocate its way through a
    // five minute record.
    let mut samples: Vec<f32> = Vec::with_capacity(sample_rate as usize * 2 * 240);
    let mut packet_buffer: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(Error::IoError(_)) => break,
            Err(error) => return Err(format!("read failed: {error}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
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
    Ok(Arc::new(Track { samples, sample_rate }))
}

/// Fold an interleaved buffer of any width into stereo.
fn widen(source: &[f32], channels: usize, out: &mut Vec<f32>) {
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

    #[test]
    fn reads_past_either_end_are_silent_rather_than_a_panic() {
        let track = Track { samples: vec![1.0, 1.0], sample_rate: 48_000 };
        assert_eq!(track.frame(-1), [0.0, 0.0]);
        assert_eq!(track.frame(0), [1.0, 1.0]);
        assert_eq!(track.frame(1), [0.0, 0.0]);
    }
}
