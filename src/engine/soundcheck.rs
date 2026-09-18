//! Does it actually make a noise?
//!
//! The unit tests cover the arithmetic. This covers the part the arithmetic
//! cannot: that a device opens, the callback runs on time, and samples reach
//! it. Ignored by default because it needs real hardware.
//!
//!     cargo test -- --ignored --nocapture

use std::f32::consts::TAU;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{Command, Engine};

/// A 440 Hz tone as a 16-bit WAV, written by hand so the test carries its own
/// material rather than depending on what happens to be on the machine.
fn tone(path: &std::path::Path, seconds: u32, rate: u32) -> std::io::Result<()> {
    let frames = seconds * rate;
    let data_bytes = frames * 4; // stereo, 16-bit

    let mut file = std::fs::File::create(path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&2u16.to_le_bytes())?; // stereo
    file.write_all(&rate.to_le_bytes())?;
    file.write_all(&(rate * 4).to_le_bytes())?; // byte rate
    file.write_all(&4u16.to_le_bytes())?; // block align
    file.write_all(&16u16.to_le_bytes())?; // bits
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;

    let mut pcm = Vec::with_capacity(data_bytes as usize);
    for frame in 0..frames {
        let value = (TAU * 440.0 * frame as f32 / rate as f32).sin();
        let sample = (value * 16_000.0) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    file.write_all(&pcm)
}

#[test]
#[ignore = "needs a real output device"]
fn the_engine_opens_a_device_and_moves_a_playhead() {
    let path = std::env::temp_dir().join("defalt-soundcheck.wav");
    tone(&path, 3, 44_100).expect("could not write the test tone");

    let mut engine = Engine::start().expect("no audio output");
    println!("device: {} at {} Hz", engine.device, engine.sample_rate);

    // Audible enough to prove samples are flowing, quiet enough not to be a
    // nuisance to whatever else is playing on this machine.
    engine.send(Command::Master { value: 0.02 }).unwrap();

    let track = super::decode::load(&path).expect("could not decode the test tone");
    // Deliberately 44.1k against whatever the device runs at, so the
    // sample-rate conversion is exercised rather than skipped.
    assert_eq!(track.sample_rate, 44_100);
    let length = track.seconds();

    engine.send(Command::Load { deck: 0, track }).unwrap();
    engine.send(Command::Play { deck: 0 }).unwrap();

    std::thread::sleep(Duration::from_millis(1500));

    let telemetry = engine.telemetry.clone();
    let position = telemetry.position(0);
    let peak = telemetry.peak();
    let underruns = telemetry.underruns.load(Ordering::Relaxed);

    println!(
        "position {position:.3}s of {length:.3}s, peak {:.4}/{:.4}, underruns {underruns}",
        peak[0], peak[1]
    );

    assert!(telemetry.loaded(0), "the deck never reported a record");
    assert!(telemetry.playing(0), "the deck stopped early");
    assert!(
        position > 1.0,
        "playhead barely moved ({position:.3}s) -- the callback is not running"
    );
    assert!(
        position < 2.0,
        "playhead ran away ({position:.3}s) -- sample rate conversion is wrong"
    );
    assert!(peak[0] > 0.0, "no signal reached the output");
    assert_eq!(underruns, 0, "the callback missed its deadline");

    // Pausing has to actually stop the playhead, not just the sound.
    engine.send(Command::Pause { deck: 0 }).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let stopped = telemetry.position(0);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(stopped, telemetry.position(0), "paused deck kept moving");

    let _ = std::fs::remove_file(&path);
}

/// Scrubbing is the reason any of this is in Rust, so it gets its own check:
/// a negative rate has to walk the playhead backwards through the record.
#[test]
#[ignore = "needs a real output device"]
fn a_negative_scrub_rate_walks_the_record_backwards() {
    let path = std::env::temp_dir().join("defalt-reverse.wav");
    tone(&path, 4, 44_100).expect("could not write the test tone");

    let mut engine = Engine::start().expect("no audio output");
    engine.send(Command::Master { value: 0.02 }).unwrap();

    let track = super::decode::load(&path).expect("could not decode");
    engine.send(Command::Load { deck: 0, track }).unwrap();
    engine.send(Command::Seek { deck: 0, seconds: 2.5 }).unwrap();
    engine.send(Command::Play { deck: 0 }).unwrap();

    let telemetry = engine.telemetry.clone();
    std::thread::sleep(Duration::from_millis(200));
    let forwards = telemetry.position(0);

    engine.send(Command::Scrub { deck: 0, rate: Some(-2.0) }).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let backwards = telemetry.position(0);

    println!("forwards {forwards:.3}s -> backwards {backwards:.3}s");
    assert!(
        backwards < forwards - 0.3,
        "playhead did not reverse ({forwards:.3}s then {backwards:.3}s)"
    );

    // And letting go puts it back on its own speed rather than leaving it in
    // reverse, which would be a deck that never recovers from a scratch.
    engine.send(Command::Scrub { deck: 0, rate: None }).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let released = telemetry.position(0);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        telemetry.position(0) > released,
        "the record did not resume forwards after the hand came off"
    );

    assert_eq!(telemetry.underruns.load(Ordering::Relaxed), 0);
    let _ = std::fs::remove_file(&path);
}
