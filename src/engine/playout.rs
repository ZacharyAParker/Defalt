//! The voice bus: host lines, scheduled to the sample.
//!
//! Records go on decks, where you can reach them. Speech cannot -- a deck
//! holds a record, and a break is three or four lines landing on exact moments
//! over the top of one. So the hosts get their own small bank: an item, where
//! to start in it, how long it runs, and a gain envelope.
//!
//! No transport, no pitch and no EQ, because nothing here is going to change
//! its mind. A line that has to land on the vocal is just an item that starts
//! when it starts.
//!
//! Timing is in output frames rather than seconds. The station thinks in its
//! own clock and `airtime` converts once, when a schedule arrives, because a
//! conversion redone every callback is a conversion that drifts. Being sample
//! accurate is the reason speech is here and not on a deck: a deck is started
//! from the UI thread, which is close enough for a record and not for a word.

use std::sync::Arc;

use super::decode::Track;

/// Enough for a crossfade with a break over it and room to spare. Overlapping
/// items past this are refused rather than allowed to steal a channel that is
/// still sounding.
pub const CHANNELS: usize = 8;

/// A gain curve over an item, as breakpoints of (seconds in, gain).
///
/// Linear between points and flat outside them, which is what the station
/// assembled them to mean. Built off-thread and read through an `Arc`, so the
/// audio thread never touches the allocator.
pub struct Envelope {
    points: Box<[[f32; 2]]>,
}

impl Envelope {
    pub fn new(points: Vec<[f32; 2]>) -> Self {
        Envelope { points: points.into_boxed_slice() }
    }

    /// Flat, for an item the station gave no envelope.
    pub fn flat(gain: f32) -> Self {
        Envelope { points: vec![[0.0, gain]].into_boxed_slice() }
    }

    /// The gain at `t` seconds into the item.
    ///
    /// `cursor` is where the last lookup landed. Playout only ever moves
    /// forward, so carrying it turns a search per sample into a step per
    /// breakpoint -- and it stays correct if it is stale, because it is walked
    /// rather than trusted.
    pub fn at(&self, t: f32, cursor: &mut usize) -> f32 {
        if self.points.is_empty() {
            return 1.0;
        }
        let last = self.points.len() - 1;
        if t <= self.points[0][0] {
            *cursor = 0;
            return self.points[0][1];
        }
        if t >= self.points[last][0] {
            *cursor = last;
            return self.points[last][1];
        }
        if *cursor > last {
            *cursor = 0;
        }
        while *cursor > 0 && self.points[*cursor][0] > t {
            *cursor -= 1;
        }
        while *cursor + 1 < last && self.points[*cursor + 1][0] <= t {
            *cursor += 1;
        }
        let [t0, g0] = self.points[*cursor];
        let [t1, g1] = self.points[*cursor + 1];
        if t1 <= t0 {
            return g1;
        }
        g0 + (g1 - g0) * ((t - t0) / (t1 - t0))
    }
}

/// One scheduled thing: a record, a voice line, a sounder.
pub struct Item {
    pub track: Arc<Track>,
    /// The output frame at which the item's `offset` is heard. An item that is
    /// already under way when it is handed over starts in the past, which is
    /// normal and is handled by reading further into it.
    pub start_frame: u64,
    /// Where in the source the item begins, in seconds.
    pub offset: f64,
    /// How long it runs. It ends here even if the file is longer, because the
    /// schedule said so.
    pub duration: f64,
    pub envelope: Arc<Envelope>,
}

#[derive(Default)]
struct Channel {
    item: Option<Item>,
    cursor: usize,
}

pub struct Playout {
    channels: [Channel; CHANNELS],
    /// Output frames since the stream opened. The one clock everything here is
    /// scheduled against.
    pub frame: u64,
    pub gain: f32,
    pub peak: f32,
}

impl Default for Playout {
    fn default() -> Self {
        Playout {
            channels: std::array::from_fn(|_| Channel::default()),
            frame: 0,
            gain: 1.0,
            peak: 0.0,
        }
    }
}

impl Playout {
    /// Put an item on a channel, handing back whatever it displaced so the
    /// caller can drop it somewhere allowed to take its time.
    pub fn set(&mut self, channel: usize, item: Option<Item>) -> Option<Arc<Track>> {
        let channel = self.channels.get_mut(channel)?;
        channel.cursor = 0;
        let old = channel.item.take();
        channel.item = item;
        old.map(|item| item.track)
    }

    /// True when something is sounding or still to come. Read by the tests,
    /// which is the only place that has to ask rather than be told.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn busy(&self) -> bool {
        self.channels.iter().any(|channel| channel.item.is_some())
    }

    /// Mix every live channel into `out`, and advance the clock.
    ///
    /// Finished tracks leave through `retire`, which must not allocate:
    /// dropping an `Arc<Track>` here could be the last reference, and freeing
    /// a few megabytes inside a callback is how a stream underruns.
    pub fn mix_into(
        &mut self,
        out: &mut [f32],
        device_rate: u32,
        mut retire: impl FnMut(Arc<Track>),
    ) {
        let frames = out.len() / 2;
        let start = self.frame;
        self.frame = self.frame.wrapping_add(frames as u64);
        if device_rate == 0 {
            return;
        }

        let seconds_per_frame = 1.0 / device_rate as f64;
        let mut peak = self.peak;

        for channel in self.channels.iter_mut() {
            let Some(item) = channel.item.as_ref() else { continue };

            // A part with no rate cannot be read at any speed. Let it go
            // rather than divide by it.
            if item.track.sample_rate == 0 {
                if let Some(done) = channel.item.take() {
                    retire(done.track);
                }
                continue;
            }
            // Still in the future for the whole of this buffer.
            if item.start_frame >= start + frames as u64 {
                continue;
            }

            let last = item.track.frames() as f64;
            let rate = item.track.sample_rate as f64;
            let mut finished = false;

            for (index, pair) in out.chunks_exact_mut(2).enumerate() {
                let here = start + index as u64;
                if here < item.start_frame {
                    continue;
                }
                let elapsed = (here - item.start_frame) as f64 * seconds_per_frame;
                if elapsed >= item.duration {
                    finished = true;
                    break;
                }
                let position = (item.offset + elapsed) * rate;
                if position < 0.0 || position >= last {
                    finished = true;
                    break;
                }

                let gain = item.envelope.at(elapsed as f32, &mut channel.cursor) * self.gain;
                let [left, right] = sample_at(&item.track, position);
                let (left, right) = (left * gain, right * gain);
                pair[0] += left;
                pair[1] += right;
                peak = peak.max(left.abs()).max(right.abs());
            }

            if finished {
                if let Some(done) = channel.item.take() {
                    retire(done.track);
                }
            }
        }

        self.peak = peak;
    }

    /// Off the air at once.
    pub fn clear(&mut self, mut retire: impl FnMut(Arc<Track>)) {
        for channel in self.channels.iter_mut() {
            channel.cursor = 0;
            if let Some(item) = channel.item.take() {
                retire(item.track);
            }
        }
    }
}

/// Linear interpolation between neighbouring frames.
///
/// Linear rather than the deck's Catmull-Rom on purpose: playout runs at the
/// source's own speed, so the read head lands within a fraction of a sample of
/// where it started and there is nothing for a wider kernel to recover. The
/// deck needs one because it is being pitched and scrubbed.
fn sample_at(track: &Track, position: f64) -> [f32; 2] {
    let index = position.floor() as usize;
    let frames = track.frames();
    if index >= frames {
        return [0.0, 0.0];
    }
    let here = [track.samples[index * 2], track.samples[index * 2 + 1]];
    if index + 1 >= frames {
        return here;
    }
    let fraction = (position - index as f64) as f32;
    let next = [track.samples[index * 2 + 2], track.samples[index * 2 + 3]];
    [
        here[0] + (next[0] - here[0]) * fraction,
        here[1] + (next[1] - here[1]) * fraction,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f64, rate: u32, level: f32) -> Arc<Track> {
        let frames = (seconds * rate as f64) as usize;
        Arc::new(Track { samples: vec![level; frames * 2], sample_rate: rate })
    }

    fn item(track: Arc<Track>, start_frame: u64, envelope: Envelope) -> Item {
        let duration = track.seconds();
        Item {
            track,
            start_frame,
            offset: 0.0,
            duration,
            envelope: Arc::new(envelope),
        }
    }

    #[test]
    fn an_envelope_reads_flat_outside_its_points() {
        let envelope = Envelope::new(vec![[1.0, 0.0], [2.0, 1.0]]);
        let mut cursor = 0;
        assert_eq!(envelope.at(0.0, &mut cursor), 0.0);
        assert_eq!(envelope.at(9.0, &mut cursor), 1.0);
    }

    #[test]
    fn an_envelope_ramps_between_its_points() {
        let envelope = Envelope::new(vec![[0.0, 0.0], [4.0, 1.0]]);
        let mut cursor = 0;
        assert!((envelope.at(2.0, &mut cursor) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_stale_cursor_still_reads_correctly() {
        // The cursor is an optimisation, not a source of truth. A lookup that
        // jumps backwards -- which a re-scheduled item does -- must not read
        // off the wrong segment.
        let envelope = Envelope::new(vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]]);
        let mut cursor = 0;
        let forward = envelope.at(1.5, &mut cursor);
        let back = envelope.at(0.5, &mut cursor);
        let again = envelope.at(1.5, &mut cursor);
        assert!((forward - 0.5).abs() < 1e-6, "{forward}");
        assert!((back - 0.5).abs() < 1e-6, "{back}");
        assert!((again - 0.5).abs() < 1e-6, "{again}");
    }

    #[test]
    fn an_item_is_silent_until_its_frame_comes_round() {
        let mut playout = Playout::default();
        playout.set(0, Some(item(tone(1.0, 48_000, 0.5), 100, Envelope::flat(1.0))));

        let mut out = vec![0.0; 64 * 2];
        playout.mix_into(&mut out, 48_000, |_| {});
        assert!(out.iter().all(|s| *s == 0.0), "it started early");

        let mut out = vec![0.0; 64 * 2];
        playout.mix_into(&mut out, 48_000, |_| {});
        assert!(out.iter().any(|s| *s != 0.0), "it never started");
    }

    #[test]
    fn two_items_crossing_sum_into_one_bus() {
        // The whole point of a bank rather than a deck: a crossfade is two
        // channels sounding at once, not one channel switching.
        let mut playout = Playout::default();
        playout.set(0, Some(item(tone(1.0, 48_000, 0.25), 0, Envelope::flat(1.0))));
        playout.set(1, Some(item(tone(1.0, 48_000, 0.25), 0, Envelope::flat(1.0))));

        let mut out = vec![0.0; 32 * 2];
        playout.mix_into(&mut out, 48_000, |_| {});
        assert!((out[0] - 0.5).abs() < 1e-5, "they did not sum: {}", out[0]);
    }

    #[test]
    fn an_envelope_of_zero_is_actually_silent() {
        let mut playout = Playout::default();
        playout.set(0, Some(item(tone(1.0, 48_000, 1.0), 0, Envelope::flat(0.0))));
        let mut out = vec![0.0; 32 * 2];
        playout.mix_into(&mut out, 48_000, |_| {});
        assert!(out.iter().all(|s| *s == 0.0), "a ducked item was audible");
    }

    #[test]
    fn a_finished_item_hands_its_record_back() {
        // Nothing may be dropped on the audio thread, so the channel has to
        // let go of the track by handing it out, not by freeing it.
        let mut playout = Playout::default();
        playout.set(0, Some(item(tone(0.001, 48_000, 0.5), 0, Envelope::flat(1.0))));

        let mut retired = 0;
        let mut out = vec![0.0; 512 * 2];
        playout.mix_into(&mut out, 48_000, |_| retired += 1);
        assert_eq!(retired, 1, "the record was not handed back");
        assert!(!playout.busy(), "the channel is still holding it");
    }

    #[test]
    fn an_item_starting_in_the_past_is_joined_part_way_through() {
        // The schedule arrives after the station has already started playing
        // it. Joining late has to mean joining in the right place, not from
        // the top.
        let rate = 48_000;
        let mut ramp = vec![0.0_f32; rate as usize * 2];
        for frame in 0..rate as usize {
            let value = frame as f32 / rate as f32;
            ramp[frame * 2] = value;
            ramp[frame * 2 + 1] = value;
        }
        let track = Arc::new(Track { samples: ramp, sample_rate: rate });

        let mut playout = Playout::default();
        playout.frame = rate as u64 / 2;
        playout.set(0, Some(Item {
            duration: track.seconds(),
            track,
            start_frame: 0,
            offset: 0.0,
            envelope: Arc::new(Envelope::flat(1.0)),
        }));

        let mut out = vec![0.0; 32 * 2];
        playout.mix_into(&mut out, rate, |_| {});
        assert!((out[0] - 0.5).abs() < 0.01, "it restarted rather than joined: {}", out[0]);
    }

    #[test]
    fn an_offset_reads_further_into_the_record() {
        let rate = 48_000;
        let mut ramp = vec![0.0_f32; rate as usize * 2];
        for frame in 0..rate as usize {
            let value = frame as f32 / rate as f32;
            ramp[frame * 2] = value;
            ramp[frame * 2 + 1] = value;
        }
        let track = Arc::new(Track { samples: ramp, sample_rate: rate });

        let mut playout = Playout::default();
        playout.set(0, Some(Item {
            duration: 0.4,
            track,
            start_frame: 0,
            offset: 0.25,
            envelope: Arc::new(Envelope::flat(1.0)),
        }));

        let mut out = vec![0.0; 32 * 2];
        playout.mix_into(&mut out, rate, |_| {});
        assert!((out[0] - 0.25).abs() < 0.01, "the offset was ignored: {}", out[0]);
    }

    #[test]
    fn an_item_stops_at_its_scheduled_length_not_the_files() {
        // A record trimmed of its cold ending must not play the ending.
        let mut playout = Playout::default();
        let track = tone(1.0, 48_000, 0.5);
        playout.set(0, Some(Item {
            track,
            start_frame: 0,
            offset: 0.0,
            duration: 0.005,
            envelope: Arc::new(Envelope::flat(1.0)),
        }));

        let mut out = vec![0.0; 48_000 * 2 / 100];
        playout.mix_into(&mut out, 48_000, |_| {});
        let tail = &out[out.len() - 32..];
        assert!(tail.iter().all(|s| *s == 0.0), "it ran past its scheduled end");
    }

    #[test]
    fn clearing_takes_everything_off_the_air() {
        let mut playout = Playout::default();
        playout.set(0, Some(item(tone(1.0, 48_000, 0.5), 0, Envelope::flat(1.0))));
        playout.set(1, Some(item(tone(1.0, 48_000, 0.5), 0, Envelope::flat(1.0))));
        let mut retired = 0;
        playout.clear(|_| retired += 1);
        assert_eq!(retired, 2);
        assert!(!playout.busy());
    }

    #[test]
    fn a_record_at_another_rate_plays_at_its_own_speed() {
        // A voice line is rarely at the device rate. Reading it at the wrong
        // one is the same bug that made separated parts play sharp.
        let device = 48_000;
        let seconds = 0.5;
        let track = tone(seconds, 24_000, 0.5);

        let mut playout = Playout::default();
        playout.set(0, Some(item(track, 0, Envelope::flat(1.0))));

        // Run past where it would end if it were read at the device rate.
        let mut out = vec![0.0; (device as f64 * 0.3) as usize * 2];
        playout.mix_into(&mut out, device, |_| {});
        assert!(playout.busy(), "a 24 kHz item ran out twice as fast as it should");
    }
}
