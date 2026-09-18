//! Assist: the part that stops you making the boring mistakes.
//!
//! It suggests, it never acts. Two jobs:
//!
//! - **Levels.** A record's measured loudness says how much trim it needs to
//!   sit where the last one sat. Riding the fader to find that out is the
//!   single most common thing a beginner does wrong and the most tedious
//!   thing an experienced person does right.
//! - **What mixes.** Given what is playing, score everything in the crate by
//!   how little work the transition would be: tempo you can reach on the
//!   fader, a key that does not fight, a level that is not a cliff.
//!
//! None of this decides anything. It orders a list and prints a number.

use crate::library::Record;

/// What we aim a channel at. Streaming platforms settled on about -14 LUFS
/// and most modern masters land near it, so a record already at -14 needs no
/// trim at all -- which is the point: assist should be invisible when the
/// material is already consistent.
pub const TARGET_LUFS: f64 = -14.0;

/// Trim for a record, as a linear gain.
///
/// Clamped hard. A badly measured file claiming -40 LUFS would otherwise ask
/// for eight times gain and take the roof off.
pub fn trim_for(lufs: Option<f64>) -> f32 {
    let Some(lufs) = lufs else { return 1.0 };
    if !lufs.is_finite() || lufs > 0.0 {
        return 1.0;
    }
    let decibels = (TARGET_LUFS - lufs).clamp(-12.0, 12.0);
    (10f64.powf(decibels / 20.0) as f32).clamp(0.25, 2.0)
}

/// How two keys sit against each other on the Camelot wheel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyFit {
    /// The same key.
    Same,
    /// One step around the wheel: the classic harmonic move.
    Neighbour,
    /// Relative major or minor -- same number, other letter.
    Relative,
    /// Neither. Mixable, but you are doing it on purpose.
    Clash,
    /// One or both records have no detected key.
    Unknown,
}

impl KeyFit {
    pub fn label(self) -> &'static str {
        match self {
            KeyFit::Same => "same key",
            KeyFit::Neighbour => "one step",
            KeyFit::Relative => "relative",
            KeyFit::Clash => "clashes",
            KeyFit::Unknown => "no key",
        }
    }

    fn weight(self) -> f32 {
        match self {
            KeyFit::Same => 1.0,
            KeyFit::Neighbour => 0.85,
            KeyFit::Relative => 0.8,
            KeyFit::Clash => 0.15,
            // Unknown must not beat a known clash by default, nor be punished
            // as if it were one. Neutral.
            KeyFit::Unknown => 0.5,
        }
    }
}

/// "8A" -> (8, minor). The wheel is 1..=12 with A minor and B major.
fn camelot(text: &str) -> Option<(i32, bool)> {
    let text = text.trim();
    let (digits, letter) = text.split_at(text.len().checked_sub(1)?);
    let number: i32 = digits.parse().ok()?;
    if !(1..=12).contains(&number) {
        return None;
    }
    match letter.to_ascii_uppercase().as_str() {
        "A" => Some((number, true)),
        "B" => Some((number, false)),
        _ => None,
    }
}

pub fn key_fit(a: Option<&str>, b: Option<&str>) -> KeyFit {
    let (Some(a), Some(b)) = (a.and_then(camelot), b.and_then(camelot)) else {
        return KeyFit::Unknown;
    };
    if a == b {
        return KeyFit::Same;
    }
    if a.0 == b.0 {
        return KeyFit::Relative;
    }
    // The wheel wraps: 12 and 1 are neighbours.
    let step = (a.0 - b.0).rem_euclid(12);
    if (step == 1 || step == 11) && a.1 == b.1 {
        return KeyFit::Neighbour;
    }
    KeyFit::Clash
}

/// Percent the candidate's tempo must move to match the target.
///
/// Octave-invariant: a record detected at half time is matched rather than
/// declared unreachable, because 70 and 140 are the same tempo and every DJ
/// knows it even when the analyser does not.
pub fn tempo_shift(target: f64, candidate: f64) -> Option<f64> {
    if target <= 0.0 || candidate <= 0.0 {
        return None;
    }
    let mut ratio = target / candidate;
    while ratio > 1.41 {
        ratio /= 2.0;
    }
    while ratio < 0.71 {
        ratio *= 2.0;
    }
    Some((ratio - 1.0) * 100.0)
}

/// How well a record would follow what is playing.
#[derive(Clone, Copy, Debug)]
pub struct Fit {
    /// 0..1. Higher is less work.
    pub score: f32,
    /// Percent the pitch fader would have to move, if both tempos are known.
    pub shift: Option<f64>,
    pub key: KeyFit,
    /// Whether that shift is inside the fader's own range.
    pub reachable: bool,
}

impl Fit {
    /// A short badge for the crate: the thing you actually read while
    /// deciding. Tempo first, because tempo is what stops a mix dead.
    ///
    /// Always the number, never a word. "far" told you the answer was no
    /// without telling you by how much, and nine percent and ninety percent
    /// are very different kinds of no -- one is a wider tempo range away, the
    /// other is a different record.
    pub fn badge(&self) -> String {
        match self.shift {
            Some(shift) => format!("{shift:+.1}%"),
            None => "--".to_string(),
        }
    }
}

/// The pitch fader's range. A shift beyond it cannot be reached without a
/// timestretcher, which is why "far" is a different answer from "a lot".
pub const FADER_RANGE: f64 = 8.0;

pub fn fit(playing: &Record, playing_pitch: f32, candidate: &Record) -> Fit {
    let target = playing.tempo_at(playing_pitch as f64);
    let shift = match (target, candidate.bpm) {
        (Some(target), Some(bpm)) => tempo_shift(target, bpm),
        _ => None,
    };
    let reachable = shift.is_some_and(|value| value.abs() <= FADER_RANGE);

    let key = key_fit(playing.camelot.as_deref(), candidate.camelot.as_deref());

    // Tempo: full marks at a perfect match, nothing once it is off the fader.
    let tempo_term = match shift {
        Some(value) => 1.0 - (value.abs() / FADER_RANGE).min(1.0) as f32,
        // An unknown tempo is not a bad tempo, but it is unknown, and you
        // will find out the hard way. Neutral, slightly shy.
        None => 0.45,
    };

    // Loudness: a big step is work, not a disaster. Small weight.
    let level_term = match (playing.lufs, candidate.lufs) {
        (Some(a), Some(b)) => 1.0 - ((a - b).abs() / 10.0).min(1.0) as f32,
        _ => 0.6,
    };

    let score = 0.5 * tempo_term + 0.4 * key.weight() + 0.1 * level_term;
    Fit { score, shift, key, reachable }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn record(bpm: Option<f64>, camelot: Option<&str>, lufs: Option<f64>) -> Record {
        Record {
            key: "a|b".into(),
            artist: "a".into(),
            title: "b".into(),
            album: None,
            duration: None,
            bpm,
            camelot: camelot.map(str::to_string),
            lufs,
            file: PathBuf::new(),
            beat_offset: None,
            beat_period: None,
            downbeat_offset: None,
        }
    }

    #[test]
    fn a_record_already_at_target_needs_no_trim() {
        assert!((trim_for(Some(TARGET_LUFS)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_quiet_record_is_brought_up_and_a_loud_one_down() {
        assert!(trim_for(Some(-20.0)) > 1.0);
        assert!(trim_for(Some(-8.0)) < 1.0);
    }

    #[test]
    fn a_nonsense_measurement_cannot_take_the_roof_off() {
        assert!(trim_for(Some(-90.0)) <= 2.0);
        assert_eq!(trim_for(None), 1.0);
        assert_eq!(trim_for(Some(f64::NAN)), 1.0);
    }

    #[test]
    fn the_camelot_wheel_wraps_at_twelve() {
        assert_eq!(key_fit(Some("12A"), Some("1A")), KeyFit::Neighbour);
        assert_eq!(key_fit(Some("1A"), Some("12A")), KeyFit::Neighbour);
    }

    #[test]
    fn relative_major_and_minor_share_a_number() {
        assert_eq!(key_fit(Some("8A"), Some("8B")), KeyFit::Relative);
    }

    #[test]
    fn a_step_across_letters_is_not_a_neighbour() {
        // 8A to 9B is two moves on the wheel, not one.
        assert_eq!(key_fit(Some("8A"), Some("9B")), KeyFit::Clash);
    }

    #[test]
    fn unknown_keys_are_neutral_rather_than_wrong() {
        assert_eq!(key_fit(None, Some("8A")), KeyFit::Unknown);
        assert!(KeyFit::Unknown.weight() > KeyFit::Clash.weight());
        assert!(KeyFit::Unknown.weight() < KeyFit::Relative.weight());
    }

    #[test]
    fn half_time_counts_as_a_match_rather_than_a_gulf() {
        // 70 against 140 is the same tempo and every DJ knows it.
        let shift = tempo_shift(140.0, 70.0).unwrap();
        assert!(shift.abs() < 0.01, "got {shift}");
    }

    #[test]
    fn the_shift_says_which_way_the_fader_goes() {
        // A slower record has to speed up: positive.
        let shift = tempo_shift(128.0, 124.0).unwrap();
        assert!(shift > 0.0, "got {shift}");
        assert!((shift - 3.2258).abs() < 0.01, "got {shift}");
    }

    #[test]
    fn a_perfect_follow_on_scores_higher_than_a_clash() {
        let playing = record(Some(128.0), Some("8A"), Some(-9.0));
        let perfect = fit(&playing, 0.0, &record(Some(128.0), Some("8A"), Some(-9.0)));
        let clashing = fit(&playing, 0.0, &record(Some(128.0), Some("3B"), Some(-9.0)));
        assert!(perfect.score > clashing.score);
        assert_eq!(perfect.key, KeyFit::Same);
    }

    #[test]
    fn a_tempo_off_the_fader_is_marked_unreachable() {
        let playing = record(Some(128.0), Some("8A"), None);
        let far = fit(&playing, 0.0, &record(Some(112.0), Some("8A"), None));
        assert!(!far.reachable);
        // Still numeric: how far is the useful part of the answer.
        assert_eq!(far.badge(), "+14.3%");
    }

    #[test]
    fn the_playing_decks_pitch_is_what_the_candidate_matches() {
        let playing = record(Some(124.0), None, None);
        let candidate = record(Some(128.0), None, None);
        // With deck A pulled up to 128, a 128 record now needs no move.
        let shift = fit(&playing, 3.2258, &candidate).shift.unwrap();
        assert!(shift.abs() < 0.05, "got {shift}");
    }

    #[test]
    fn a_badge_reads_as_a_fader_move() {
        let playing = record(Some(128.0), None, None);
        let fit = fit(&playing, 0.0, &record(Some(124.0), None, None));
        assert_eq!(fit.badge(), "+3.2%");
    }
}
