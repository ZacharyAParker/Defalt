use super::*;
use crate::station::client::Failure;
use protocol::rate_curve_from;

#[test]
fn studio_levels_follow_each_active_host_channel_and_stop_with_playback() {
    let mut air = Airtime::new(Path::new("."), 9, 48000);
    let snapshot = snapshot_from(&serde_json::json!({"now": 1, "items": [
        {"id":"m", "url":"/m", "kind":"voice", "start_at":0, "duration":3, "meta":{"host":"mav"}},
        {"id":"r", "url":"/r", "kind":"voice", "start_at":0, "duration":1.5, "meta":{"host":"rue"}}
    ]}), 0);
    air.set_schedule(snapshot.items); air.station_now = 1.0; air.on = true;
    air.voices.on_air.insert("m".into(), 2); air.voices.on_air.insert("r".into(), 0);
    let mut peaks = [0.0; crate::engine::playout::CHANNELS];
    peaks[0] = 0.2; peaks[2] = 0.7;
    assert_eq!(air.host_levels(&peaks), [0.7, 0.2]);
    air.station_now = 2.0;
    assert_eq!(air.host_levels(&peaks), [0.7, 0.0]);
    air.on = false;
    assert_eq!(air.host_levels(&peaks), [0.0, 0.0]);
}

/// A schedule shaped exactly like the station's, taken from a live one.
const REAL: &str = r#"{
    "now": 593.4, "epoch": 0, "items": [
        {"id": "43ac", "kind": "music", "url": "/media/audio/658c.m4a",
         "start_at": 553.5, "duration": 198.5, "offset": 0.0,
         "envelope": [[0.0, 0.0001], [6.0, 1.0], [198.5, 1.0]],
         "meta": {"title": "GESHUOU", "artist": "INOHA", "bpm": 107.7,
                  "camelot": "8A", "key": "inoha|geshuou",
                  "automation": {"low": [[0.0, -26.0], [6.0, 0.0], [198.5, 0.0]],
                                 "lpf": [[0.0, 380.0], [4.2, 6090.5], [6.0, 20000.0]]},
                  "transition": {"preset": "rise", "overlap": 6.0,
                                 "eq": "end_bass_swap",
                                 "reason": "stepping up in energy"}}}
    ]}"#;

fn real() -> Snapshot {
    snapshot_from(&serde_json::from_str(REAL).unwrap(), 0)
}

/// A deck carrying `id`: whether it has been started, and whether its
/// record has loaded.
fn held(id: &str, started: bool, ready: bool) -> Option<Assigned> {
    let mut assigned = Assigned::new(id, id);
    assigned.started = started;
    assigned.ready = ready;
    Some(assigned)
}

fn prepared_pair() -> Airtime {
    let mut a = real().items[0].clone();
    a.id = "a".into();
    a.start_at = 100.0;
    a.duration = 300.0;
    a.offset = 7.0;
    a.playback_rate = 1.1;
    a.rate_curve = vec![[0.0, 1.1], [100.0, 1.0]];
    let mut b = a.clone();
    b.id = "b".into();
    b.start_at = 392.0;
    b.duration = 200.0;
    b.offset = 12.0;
    b.playback_rate = 1.05;
    b.rate_curve.clear();
    b.transition.as_mut().unwrap().overlap = 8.0;
    let mut airtime = airtime_with(vec![a, b], 150.0);
    airtime.decks[0] = held("a", true, true);
    airtime.decks[1] = held("b", false, false);
    airtime
}

#[test]
fn mix_markers_use_source_offsets_and_integrate_tempo_recovery() {
    let mut airtime = prepared_pair();
    let outgoing = airtime.transition_windows(0);
    let incoming = airtime.transition_windows(1);
    assert_eq!(outgoing.len(), 1);
    assert_eq!(incoming.len(), 1);
    assert!((outgoing[0].start - 304.0).abs() < 1e-6);
    assert!((outgoing[0].end - 312.0).abs() < 1e-6);
    assert!(!outgoing[0].incoming);
    assert_eq!(incoming[0].start, 12.0);
    assert!((incoming[0].end - 20.4).abs() < 1e-6);
    assert!(incoming[0].incoming);
    airtime.station_now = 388.0;
    assert_eq!(airtime.transition_windows(0), outgoing, "Skip moved the waveform's source markers");
}

#[test]
fn markers_follow_the_actual_pair_and_disappear_when_it_is_removed() {
    let mut airtime = prepared_pair();
    airtime.schedule[1].start_at += 5.0;
    let windows = airtime.transition_windows(0);
    assert!((windows[0].end - windows[0].start - 3.0).abs() < 1e-6);
    airtime.schedule[1].transition = None; // A dry break has no overlap markers.
    assert!(airtime.transition_windows(0).is_empty());
    airtime.schedule.pop();
    assert!(airtime.transition_windows(1).is_empty());
}

#[test]
fn skip_waits_for_the_incoming_decode_then_dispatches_once() {
    let mut airtime = prepared_pair();
    airtime.skip();
    assert!(!airtime.take_ready_skip());
    assert!(airtime.skip_pending.is_some());
    airtime.decks[1].as_mut().unwrap().ready = true;
    assert!(airtime.take_ready_skip());
    assert!(!airtime.take_ready_skip());
}

#[test]
fn pending_skip_is_cancelled_when_the_mix_starts_or_radio_stops() {
    let mut airtime = prepared_pair();
    airtime.skip();
    airtime.station_now = 393.0;
    airtime.decks[1].as_mut().unwrap().ready = true;
    assert!(!airtime.take_ready_skip());
    assert!(airtime.skip_pending.is_none());
    airtime.station_now = 150.0;
    airtime.skip();
    airtime.set_on(false);
    assert!(airtime.skip_pending.is_none());
}

#[test]
fn the_stations_own_schedule_reads_completely() {
    let parsed = real();
    let item = &parsed.items[0];
    assert_eq!(item.title, "GESHUOU");
    assert_eq!(item.artist, "INOHA");
    assert_eq!(item.bpm, Some(107.7));
    assert_eq!(item.camelot.as_deref(), Some("8A"));
    assert!(item.is_music());

    let transition = item.transition.as_ref().expect("no transition");
    assert_eq!(transition.preset, "rise");
    assert_eq!(transition.overlap, 6.0);
    assert_eq!(transition.reason, "stepping up in energy");

    assert!(!item.automation.low.is_empty(), "the bass swap was dropped");
    assert!(!item.automation.lpf.is_empty(), "the filter rise was dropped");
}

#[test]
fn a_malformed_field_loses_the_field_not_the_item() {
    let parsed = snapshot_from(&serde_json::json!({"now": 1, "epoch": "x", "items": [
        {"id": "a", "kind": "music", "url": "/a", "start_at": 0, "duration": 10,
         "meta": {"bpm": "fast", "deck": "A", "title": "Fine", "echo": 7}},
        {"id": "b", "url": "/b"},
        "not an item"
    ]}), 0);
    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.epoch, 0);
    let item = &parsed.items[0];
    assert_eq!(item.title, "Fine");
    assert_eq!(item.bpm, None);
    assert_eq!(item.preferred_deck, None);
    assert!(item.echo.is_none());
}

#[test]
fn the_record_carries_the_stations_analysis_and_file() {
    let parsed = snapshot_from(&serde_json::json!({"now": 1, "items": [
        {"id": "a", "kind": "music", "url": "/media/track/k", "start_at": 0, "duration": 10,
         "meta": {"key": "k", "file": "C:\\Music\\a.flac", "lufs": -9.5, "trim_db": -4.5,
                  "bpm": 124.0, "camelot": "8A", "beat_offset": 0.12, "beat_period": 0.4839,
                  "downbeat_offset": 0.12}}
    ]}), 0);
    let item = &parsed.items[0];
    assert_eq!(item.trim_db, -4.5);
    assert_eq!(item.file.as_deref(), Some(Path::new("C:\\Music\\a.flac")));
    let record = item.record(PathBuf::from("C:\\Music\\a.flac"));
    assert_eq!(record.lufs, Some(-9.5));
    assert_eq!(record.beat_offset, Some(0.12));
    assert_eq!(record.beat_period, Some(0.4839));
    assert_eq!(record.downbeat_offset, Some(0.12));
}

#[test]
fn a_bass_swap_reads_as_a_knob_that_starts_down_and_comes_up() {
    let parsed = real();
    let low = &parsed.items[0].automation.low;
    let start = band_knob(low.at(0.0).unwrap());
    let end = band_knob(low.at(6.0).unwrap());
    assert!(start < 0.1, "the bass did not start swapped out: {start}");
    assert!((end - 0.5).abs() < 0.01, "the bass did not come back: {end}");
}

#[test]
fn a_filter_rise_reads_as_a_fader_that_opens() {
    let parsed = real();
    let lpf = &parsed.items[0].automation.lpf;
    let start = filter_fader(lpf.at(0.0).unwrap(), true);
    let middle = filter_fader(lpf.at(4.2).unwrap(), true);
    // A low-pass sits on the negative half and comes back towards centre
    // as it opens.
    assert!(start < -0.5, "the filter did not start closed: {start}");
    assert!(middle > start, "the filter did not open: {start} -> {middle}");
}

/* -- Holding a control -- */

fn airtime_with(items: Vec<Scheduled>, now: f64) -> Airtime {
    let mut airtime = Airtime::new(Path::new("."), 9, 48_000);
    airtime.on = true;
    airtime.station_now = now;
    airtime.set_schedule(items);
    airtime
}

fn automated(plan: &Plan, deck: usize, lane: Lane) -> Option<Arc<LaneCurve>> {
    plan.automation.iter().find_map(|command| match command {
        Command::Automate { deck: d, lane: l, curve } if *d == deck && *l == lane => Some(curve.clone()),
        _ => None,
    })
}

#[test]
fn a_held_knob_is_left_out_of_the_plan_and_the_rest_is_not() {
    let parsed = real();
    let mut airtime = airtime_with(parsed.items, 553.5);
    airtime.decks[0] = held("43ac", true, true);

    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    let free = plan.tone[0].expect("nothing was automated at all");
    assert!(automated(&plan, 0, Lane::Low).is_some());

    // Now take the low band by hand.
    airtime.held.tone[0][0] = true;
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    let held = plan.tone[0].expect("holding one control stopped all of them");

    assert!(free[0] < 0.1, "the bass swap was not being driven: {free:?}");
    assert!((held[0] - 0.5).abs() < 1e-6, "a held knob was still written: {held:?}");
    assert_eq!(free[3], held[3], "holding the low band moved the filter");
    assert!(automated(&plan, 0, Lane::Low).is_none(), "the held lane was sent again");
    assert!(plan.automation.iter().any(|c| matches!(c, Command::Detach { deck: 0, lane: Lane::Low })),
            "the held lane was not let go");
    assert!(automated(&plan, 0, Lane::Sweep).is_some(), "the rest stopped being automated");
}

#[test]
fn a_held_crossfader_is_never_written() {
    let parsed = real();
    let mut airtime = airtime_with(parsed.items, 600.0);
    airtime.decks[0] = held("43ac", true, true);
    airtime.held.crossfade = true;

    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert!(plan.crossfade.is_none(), "the crossfader was moved under your hand");
}

#[test]
fn taking_the_crossfader_keeps_each_deck_where_it_was() {
    let parsed = real();
    let mut airtime = airtime_with(parsed.items, 600.0);
    airtime.decks[0] = held("43ac", true, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    airtime.take_crossfader([1.0, 0.0]);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    let level = automated(&plan, 0, Lane::Level).expect("the level was not re-sent");
    assert_eq!(level.at(level.start_frame() + 48_000 * 60, &mut 0), Some(1.0));
    assert!(plan.crossfade.is_none());
}

#[test]
fn one_record_playing_puts_the_crossfader_on_its_deck() {
    let parsed = real();
    let mut airtime = airtime_with(parsed.items, 600.0);
    airtime.decks[0] = held("43ac", true, true);

    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert_eq!(plan.crossfade, Some(0.0), "the fader was not on deck A");
}

#[test]
fn a_transition_walks_the_crossfader_across() {
    let mut items = real().items;
    let mut second = items[0].clone();
    second.id = "next".into();
    second.start_at = 700.0;
    second.duration = 180.0;
    items[0].duration = 152.5; // ends at 706, so a six second overlap
    items[0].envelope = vec![[0.0, 1.0], [146.5, 1.0], [149.5, 0.707107], [152.5, 0.0]];
    second.envelope = vec![[0.0, 0.0], [3.0, 0.707107], [6.0, 1.0]];
    items.push(second);

    let readings: Vec<f32> = [700.0, 703.0, 706.0]
        .into_iter()
        .map(|now| {
            let mut airtime = airtime_with(items.clone(), now);
            airtime.decks[0] = held("43ac", true, true);
            airtime.decks[1] = held("next", true, true);
            let mut plan = Plan::default();
            airtime.drive([busy(true, true), busy(true, true)], &mut plan);
            plan.crossfade.expect("no crossfade during a transition")
        })
        .collect();

    assert!(readings[0] < 0.05, "it did not start on the outgoing deck: {readings:?}");
    assert!(readings[1] > 0.4 && readings[1] < 0.6, "it did not pass through the middle: {readings:?}");
    assert!(readings[2] > 0.95, "it did not arrive on the incoming deck: {readings:?}");
}

#[test]
fn a_transition_is_sent_once_as_curves_the_engine_plays_by_itself() {
    // The reason all this moved into the engine: a minimised window ticks
    // ten times a second at best, and a fade stepped at that rate is a
    // staircase. Sent once, the curve is exact on every frame.
    let mut items = real().items;
    items[0].duration = 152.5;
    items[0].envelope = vec![[0.0, 1.0], [146.5, 1.0], [152.5, 0.0]];
    let mut airtime = airtime_with(items, 690.0);
    airtime.decks[0] = held("43ac", true, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    let level = automated(&plan, 0, Lane::Level).expect("no level curve");
    let frame = |t: f64| airtime.clock.frame_at(t).unwrap();
    assert_eq!(level.at(frame(700.0), &mut 0), Some(1.0));
    let halfway = level.at(frame(703.0), &mut 0).unwrap();
    assert!((halfway - 0.5).abs() < 1e-3, "{halfway}");
    assert_eq!(level.at(frame(706.0), &mut 0), Some(0.0));

    // Nothing changed, so nothing is sent again.
    for _ in 0..10 {
        airtime.station_now += 0.1;
        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        assert!(plan.automation.is_empty(), "curves were re-sent every tick: {}", plan.automation.len());
    }
}

#[test]
fn a_record_is_put_on_the_clock_for_its_exact_frame() {
    let items = pair();
    let mut airtime = airtime_with(items, 192.5);
    airtime.clock.anchor(192.5, 1_000_000);
    airtime.decks[0] = held("43ac", true, true);
    airtime.decks[1] = held("next", false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    assert_eq!(plan.start, vec![(1, 0.0)], "it was not armed ahead of its air time");
    // 194 - 192.5 = 1.5 s after the anchor, at 48 kHz.
    assert_eq!(plan.start_frame[1], Some(1_000_000 + 72_000));
    // Armed once, not again the next frame.
    let mut again = Plan::default();
    airtime.station_now = 192.6;
    airtime.drive([busy(true, true), busy(true, false)], &mut again);
    assert!(again.start.is_empty());
}

#[test]
fn a_duck_rides_the_deck_level_not_the_crossfader() {
    // Outside a transition the station's envelope is only ever a duck, and
    // a duck is a level move on the deck.
    let mut items = real().items;
    items[0].envelope = vec![[0.0, 1.0], [10.0, 1.0], [12.0, 0.4], [20.0, 0.4]];
    let mut airtime = airtime_with(items, 553.5 + 15.0);
    airtime.decks[0] = held("43ac", true, true);

    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    let level = plan.level[0].expect("the level was not driven");
    assert!((level - 0.4).abs() < 0.01, "the deck did not duck: {level}");
    assert_eq!(plan.crossfade, Some(0.0));
}

#[test]
fn a_record_is_started_where_the_station_says_and_only_once() {
    let mut items = real().items;
    items[0].offset = 3.0;
    let mut airtime = airtime_with(items, 553.5);
    airtime.decks[0] = held("43ac", false, true);

    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert_eq!(plan.start.len(), 1);
    assert_eq!(plan.start[0].0, 0);
    assert!((plan.start[0].1 - 3.0).abs() < 0.01, "wrong cue point: {:?}", plan.start[0]);

    // A second turn must not restart it.
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert!(plan.start.is_empty(), "it was started twice");
}

#[test]
fn native_playback_reports_each_airing_once_even_after_resync() {
    let mut airtime = airtime_with(real().items, 554.0);
    airtime.decks[0] = held("43ac", true, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, false), busy(false, false)], &mut plan);
    assert!(plan.report_started.is_empty(), "a paused deck is not a play");
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert_eq!(plan.report_started.len(), 1);
    assert_eq!(plan.report_started[0].0, "43ac");
    let mut next = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut next);
    assert!(next.report_started.is_empty());
}

#[test]
fn joining_late_starts_further_into_the_record() {
    let mut airtime = airtime_with(real().items, 553.5 + 30.0);
    airtime.decks[0] = held("43ac", false, true);

    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert!((plan.start[0].1 - 30.0).abs() < 0.1, "it restarted: {:?}", plan.start[0]);
}

#[test]
fn a_record_is_not_started_before_it_has_finished_loading() {
    let mut airtime = airtime_with(real().items, 553.5);
    airtime.decks[0] = held("43ac", false, true);

    let mut plan = Plan::default();
    airtime.drive([busy(false, false), busy(false, false)], &mut plan);
    assert!(plan.start.is_empty(), "it played a deck with nothing on it");
}

/* -- Which deck the next record goes on -- */

fn busy(loaded: bool, playing: bool) -> DeckStatus {
    DeckStatus { loaded, playing, ..DeckStatus::default() }
}

#[test]
fn the_next_record_goes_on_the_deck_that_is_not_playing() {
    let mut airtime = airtime_with(real().items, 600.0);
    airtime.decks[0] = held("43ac", true, true);
    // A is playing the station's record; B is empty.
    let picked = airtime.free_deck([busy(true, true), busy(false, false)]);
    assert_eq!(picked, Some(1));
}

#[test]
fn a_deck_you_are_playing_yourself_is_not_taken() {
    // You loaded something onto B and started it. The station waits for A
    // rather than pulling the record out from under you.
    let airtime = airtime_with(real().items, 600.0);
    let picked = airtime.free_deck([busy(false, false), busy(true, true)]);
    assert_eq!(picked, Some(0));
}

#[test]
fn both_decks_playing_means_the_next_record_waits() {
    let mut airtime = airtime_with(real().items, 600.0);
    airtime.decks[0] = held("43ac", true, true);
    let picked = airtime.free_deck([busy(true, true), busy(true, true)]);
    assert_eq!(picked, None, "it took a deck that was sounding");
}

#[test]
fn an_empty_deck_is_preferred_over_a_stopped_one_with_a_record_on_it() {
    // Both are fair game, but taking the empty one leaves whatever you
    // cued up by hand sitting there.
    let airtime = airtime_with(real().items, 600.0);
    assert_eq!(airtime.free_deck([busy(true, false), busy(false, false)]), Some(1));
    assert_eq!(airtime.free_deck([busy(false, false), busy(true, false)]), Some(0));
}

/* -- Preloading -- */

/// A library holding a file for every url in `items`, so these tests
/// exercise which deck a record lands on rather than whether its file can
/// be found -- which has its own tests.
fn library_for(items: &[Scheduled]) -> Vec<Record> {
    items
        .iter()
        .map(|item| {
            let name = item.url.rsplit('/').next().unwrap_or("x");
            Record {
                key: item.key.clone(),
                artist: item.artist.clone(),
                title: item.title.clone(),
                album: None,
                duration: Some(item.duration),
                bpm: item.bpm,
                camelot: item.camelot.clone(),
                lufs: None,
                file: PathBuf::from(format!(r"E:\Music\{name}")),
                beat_offset: None,
                beat_period: None,
                downbeat_offset: None,
            }
        })
        .collect()
}

/// Two records back to back, the second a long way off.
fn pair() -> Vec<Scheduled> {
    let mut items = real().items;
    items[0].start_at = 0.0;
    items[0].duration = 200.0;
    let mut second = items[0].clone();
    second.id = "next".into();
    second.key = "second|record".into();
    second.url = "/media/audio/second.m4a".into();
    second.start_at = 194.0;
    second.duration = 200.0;
    items.push(second);
    items
}

#[test]
fn the_next_record_is_cued_up_long_before_it_airs() {
    // The whole reason a console has two decks. Waiting until a record is
    // nearly due means decoding it under time pressure, and a skip can
    // wind the clock straight past the window it was going to use.
    let items = pair();
    let records = library_for(&items);
    let mut airtime = airtime_with(items, 5.0);
    airtime.decks[0] = held("43ac", true, true);

    let mut plan = Plan::default();
    airtime.assign(&records, [busy(true, true), busy(false, false)], &mut plan);

    assert_eq!(plan.load.len(), 1, "it did not cue anything up");
    assert_eq!(plan.load[0].0, 1, "it cued onto the wrong deck");
    assert_eq!(plan.load[0].1.key, "second|record");
    assert!(airtime.decks[1].is_some(), "the deck was not claimed");
}

#[test]
fn a_skip_into_a_transition_finds_the_record_already_on_its_deck() {
    // The bug this guards: the incoming record used to be loaded twenty
    // seconds before it aired, and a skip winds the clock forward to just
    // before the next transition -- which could land inside that window.
    // The transition then began against an empty deck.
    let items = pair();
    let records = library_for(&items);
    let mut airtime = airtime_with(items.clone(), 5.0);
    airtime.epoch = 0;
    airtime.decks[0] = held("43ac", true, true);

    // Cue up as normal, well ahead.
    let mut plan = Plan::default();
    airtime.assign(&records, [busy(true, true), busy(false, false)], &mut plan);
    assert_eq!(plan.load.len(), 1, "nothing was cued up to begin with");

    // Now the station is skipped to just before the transition.
    let mut plan = Plan::default();
    airtime.absorb(snapshot_at(items, 193.0, 1), None, &mut plan);

    assert!(plan.load.is_empty(), "it reloaded a record it already had");
    assert_eq!(
        airtime.decks[1].as_ref().map(|a| a.id.as_str()),
        Some("next"),
        "the incoming record was not on a deck when the transition arrived"
    );
}

#[test]
fn starting_from_cold_fills_both_decks() {
    let items = pair();
    let records = library_for(&items);
    let mut airtime = airtime_with(items, 0.0);
    let mut plan = Plan::default();
    airtime.assign(&records, [busy(false, false), busy(false, false)], &mut plan);
    assert_eq!(plan.load.len(), 2, "it only cued one of two free decks");
    assert!(airtime.decks.iter().all(|d| d.is_some()));
}

#[test]
fn a_third_record_waits_rather_than_evicting_one_of_the_two() {
    let mut items = pair();
    let mut third = items[0].clone();
    third.id = "third".into();
    third.url = "/media/audio/third.m4a".into();
    third.start_at = 400.0;
    items.push(third);

    let records = library_for(&items);
    let mut airtime = airtime_with(items, 0.0);
    let mut plan = Plan::default();
    airtime.assign(&records, [busy(false, false), busy(false, false)], &mut plan);
    assert_eq!(plan.load.len(), 2, "it tried to cue three records onto two decks");
}

#[test]
fn a_finished_record_gives_its_deck_up_without_waiting_for_a_poll() {
    // A second and a half of the following record's loading time was going
    // into noticing that the last one had ended.
    let mut items = pair();
    items[1].start_at = 260.0; // no overlap, so deck A is plainly finished
    let records = library_for(&items);
    let mut airtime = airtime_with(items, 240.0);
    airtime.decks[0] = held("43ac", true, true);

    let plan = airtime.tick(0, &records, [busy(true, false), busy(false, false)], true);
    assert!(airtime.decks[0].is_none() || plan.load.iter().any(|(d, _, _)| *d == 0),
            "the deck was still held by a record that had ended");
}

#[test]
fn the_stations_trim_rides_along_with_the_record() {
    let mut items = pair();
    items[1].trim_db = -3.5;
    let records = library_for(&items);
    let mut airtime = airtime_with(items, 5.0);
    airtime.decks[0] = held("43ac", true, true);
    let mut plan = Plan::default();
    airtime.assign(&records, [busy(true, true), busy(false, false)], &mut plan);
    assert_eq!(plan.load[0].2, -3.5);
}

/* -- Skipping -- */

fn snapshot_at(items: Vec<Scheduled>, now: f64, epoch: i64) -> Snapshot {
    Snapshot { now, epoch, items, frame: 0 }
}

#[test]
fn a_skip_re_cues_the_decks_rather_than_reloading_them() {
    // The clock jumped, not the lineup. Tearing the decks down and
    // decoding again would put a hole in the output exactly where the
    // skip is.
    let items = real().items;
    let mut airtime = airtime_with(items.clone(), 560.0);
    airtime.epoch = 0;
    airtime.decks[0] = held("43ac", true, true);

    let mut plan = Plan::default();
    airtime.absorb(snapshot_at(items, 700.0, 1), None, &mut plan);

    assert!(plan.stop.is_empty(), "it stopped a deck it did not need to");
    assert!(plan.load.is_empty(), "it reloaded a record it already had");
    assert!(airtime.decks[0].is_some(), "it gave the deck up");
    assert!(
        !airtime.decks[0].as_ref().unwrap().started,
        "it did not re-cue, so the record is still playing where it was"
    );

    // And the re-cue lands where the station now is.
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    let (deck, at) = plan.start[0];
    assert_eq!(deck, 0);
    assert!((at - (700.0 - 553.5)).abs() < 0.1, "re-cued to the wrong place: {at}");
}

#[test]
fn a_skip_takes_the_voices_off_because_their_timing_is_now_wrong() {
    // Speech is scheduled to the sample against the old clock, so unlike
    // the records it really is wrong after a jump.
    let items = real().items;
    let mut airtime = airtime_with(items.clone(), 560.0);
    airtime.epoch = 0;
    airtime.voices.on_air.insert("a-line".into(), 0);
    airtime.voices.free.retain(|c| *c != 0);

    let mut plan = Plan::default();
    airtime.absorb(snapshot_at(items, 700.0, 1), None, &mut plan);

    assert!(
        plan.voice.iter().any(|c| matches!(c, Command::OffAir)),
        "a line kept playing against a clock that had moved"
    );
    assert_eq!(airtime.voices.free.len(), voice::VOICE_CHANNELS);
}

#[test]
fn a_record_the_skip_left_behind_is_stopped_and_given_up() {
    let items = real().items;
    let mut airtime = airtime_with(items, 560.0);
    airtime.epoch = 0;
    airtime.decks[0] = held("gone", true, true);

    let mut plan = Plan::default();
    airtime.absorb(snapshot_at(real().items, 700.0, 1), None, &mut plan);

    assert_eq!(plan.stop, vec![0]);
    assert!(airtime.decks[0].is_none());
}

#[test]
fn the_first_schedule_is_not_mistaken_for_a_skip() {
    let mut airtime = Airtime::new(Path::new("."), 9, 48_000);
    airtime.on = true;
    let mut plan = Plan::default();
    airtime.absorb(snapshot_at(real().items, 600.0, 7), None, &mut plan);
    assert!(plan.voice.is_empty(), "it went off air before it went on");
    assert!(plan.stop.is_empty());
}

/* -- Rating -- */

#[test]
fn rating_needs_something_on_air() {
    let mut airtime = airtime_with(Vec::new(), 600.0);
    airtime.rate(true);
    assert!(airtime.note.take().is_some_and(|n| n.contains("Nothing on air")));
}

#[test]
fn what_is_on_air_is_the_most_recently_started_record() {
    // Mid-transition both decks are sounding. A thumb belongs to the one
    // coming in, which is the one you are reacting to.
    let mut items = real().items;
    let mut second = items[0].clone();
    second.id = "next".into();
    second.key = "second|record".into();
    second.start_at = 700.0;
    items[0].duration = 152.5;
    items.push(second);

    let mut airtime = airtime_with(items, 703.0);
    airtime.decks[0] = held("43ac", true, true);
    airtime.decks[1] = held("next", true, true);
    assert_eq!(airtime.current().map(|i| i.key.as_str()), Some("second|record"));
}

/* -- The queue -- */

/// The three stages, exactly as the station reports them.
const QUEUE: &str = r#"{
    "now": 600.0, "items": [
        {"id": "a1", "stage": "on_deck", "playing": true, "eta": 0.0,
         "artist": "INOHA", "title": "GESHUOU", "key": "inoha|geshuou",
         "bpm": 107.7, "camelot": "8A", "can_move": false, "can_remove": false},
        {"id": "a2", "stage": "on_deck", "playing": false, "eta": 92.4,
         "artist": "Alex G", "title": "Pretend", "bpm": 104.2,
         "camelot": "9A", "can_move": false, "can_remove": true},
        {"id": "q1", "stage": "queued", "playing": false, "eta": null,
         "artist": "Tame Impala", "title": "The Less I Know The Better",
         "bpm": 116.9, "camelot": "5B", "can_move": true, "can_remove": true},
        {"id": "req:7", "stage": "finding", "playing": false, "eta": null,
         "artist": null, "title": "something jazzy",
         "source": "request", "can_move": false, "can_remove": true}
    ]}"#;

fn queue() -> Vec<QueueRow> {
    queue_from(&serde_json::from_str(QUEUE).unwrap())
}

#[test]
fn the_queue_reads_all_three_stages() {
    let rows = queue();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].stage, "on_deck");
    assert!(rows[0].playing);
    assert_eq!(rows[2].stage, "queued");
    assert_eq!(rows[3].stage, "finding");
}

#[test]
fn what_is_playing_can_never_be_dropped_or_moved() {
    // Its air time is the thing every transition after it was computed
    // against, and it is already sounding.
    let playing = &queue()[0];
    assert!(!playing.can_remove, "the record on air was offered a drop");
    assert!(!playing.can_move);
}

#[test]
fn a_record_on_the_clock_can_be_dropped_but_not_reordered() {
    let on_deck = &queue()[1];
    assert!(on_deck.can_remove);
    assert!(!on_deck.can_move, "moving it would invalidate every time after it");
}

#[test]
fn a_request_still_being_found_can_only_be_called_off() {
    let finding = &queue()[3];
    assert!(finding.can_remove);
    assert!(!finding.can_move);
    // It has no artist yet, so the label must not read " - something".
    assert_eq!(finding.label(), "something jazzy");
}

#[test]
fn a_row_with_an_artist_reads_as_artist_and_title() {
    assert_eq!(queue()[0].label(), "INOHA - GESHUOU");
}

#[test]
fn an_empty_answer_is_an_empty_queue_rather_than_an_error() {
    assert!(queue_from(&serde_json::json!({"items": []})).is_empty());
    assert!(queue_from(&serde_json::json!({})).is_empty());
}

#[test]
fn a_row_missing_its_id_is_dropped_rather_than_guessed() {
    // Every action is addressed by id. A row without one is a button that
    // would fail, so it never gets drawn.
    let rows = queue_from(&serde_json::json!({
        "items": [{"stage": "queued", "title": "no id here"},
                  {"id": "ok", "stage": "queued", "title": "fine"}]
    }));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "ok");
}

#[test]
fn a_station_that_is_not_running_shows_no_queue_at_all() {
    // Rather than the last one it answered with, which would be a list of
    // records that are not going to play.
    let mut airtime = Airtime::new(Path::new("."), 9, 48_000);
    airtime.queue = queue();
    let plan = airtime.tick(0, &[], [DeckStatus::default(); DECKS], false);
    assert!(airtime.queue.is_empty(), "it kept a queue from a dead station");
    assert!(plan.load.is_empty());
}

/* -- Asking for things -- */

#[test]
fn an_empty_request_is_not_sent() {
    let mut airtime = Airtime::new(Path::new("."), 9, 48_000);
    airtime.request = "   ".into();
    airtime.submit_request();
    assert!(airtime.note.is_none(), "it sent whitespace to the station");
    assert_eq!(airtime.request, "   ", "it cleared a box it never sent");
}

#[test]
fn sending_a_request_clears_the_box_and_says_what_went() {
    let mut airtime = Airtime::new(Path::new("."), 9, 48_000);
    airtime.request = "  do the news  ".into();
    airtime.submit_request();
    assert!(airtime.request.is_empty(), "the box kept what was already sent");
    let note = airtime.note.take().expect("it said nothing");
    assert!(note.contains("do the news"), "{note}");
    assert!(!note.contains("  do"), "it did not trim: {note}");
}

#[test]
fn a_percent_encoded_name_decodes() {
    assert_eq!(percent_decode("a%20b.flac"), "a b.flac");
    assert_eq!(percent_decode("100%.flac"), "100%.flac");
}

fn record_at(key: &str, file: &str) -> Record {
    Record {
        key: key.into(), artist: "A".into(), title: "B".into(), album: None, duration: None,
        bpm: None, camelot: None, lufs: None, file: PathBuf::from(file),
        beat_offset: None, beat_period: None, downbeat_offset: None,
    }
}

#[test]
fn a_record_the_station_never_downloaded_resolves_through_the_library() {
    let airtime = Airtime::new(Path::new(r"C:\nowhere"), 9, 48_000);
    let mut item = real().items.remove(0);
    item.url = "/media/audio/Some%20Record.wav".into();
    let found = airtime.resolve(&item, &[record_at("a|b", r"E:\Music\Some Record.wav")]);
    assert_eq!(found, Some(PathBuf::from(r"E:\Music\Some Record.wav")));
}

#[test]
fn a_local_track_resolves_by_its_key_and_a_real_file_wins_outright() {
    let airtime = Airtime::new(Path::new(r"C:\nowhere"), 9, 48_000);
    let mut item = real().items.remove(0);
    item.url = "/media/track/inoha|geshuou".into();
    let records = [record_at("inoha|geshuou", r"E:\Music\Deep\Name With Spaces.flac")];
    assert_eq!(airtime.resolve(&item, &records), Some(PathBuf::from(r"E:\Music\Deep\Name With Spaces.flac")));
    // The station's own path, when it exists, beats any guess.
    let here = std::env::current_exe().unwrap();
    item.file = Some(here.clone());
    assert_eq!(airtime.resolve(&item, &records), Some(here));
}

#[test]
fn a_record_with_no_audio_is_looked_for_once_a_second_then_given_up() {
    let items = pair();
    let mut airtime = airtime_with(items, 5.0);
    airtime.decks[0] = held("43ac", true, true);
    let mut plan = Plan::default();
    airtime.assign(&[], [busy(true, true), busy(false, false)], &mut plan);
    assert!(plan.load.is_empty());
    assert!(airtime.note.take().is_some_and(|n| n.contains("Cannot find")), "it said nothing");
    // Asked again at once: not looked for again, and not said again.
    airtime.assign(&[], [busy(true, true), busy(false, false)], &mut plan);
    assert!(airtime.note.is_none(), "the notice flooded");
    // After the grace it is given up on for good.
    let (_, tried) = airtime.missing["next"];
    airtime.missing.insert("next".into(), (Instant::now() - Duration::from_secs(10), tried - Duration::from_secs(2)));
    airtime.assign(&[], [busy(true, true), busy(false, false)], &mut plan);
    assert!(airtime.failed.contains("next"));
    assert!(airtime.note.take().is_some_and(|n| n.contains("skipped")));
    assert!(airtime.decks[1].is_none());
}

#[test]
fn turning_it_off_gives_every_deck_and_channel_back() {
    let mut airtime = airtime_with(real().items, 600.0);
    airtime.decks[0] = held("43ac", true, true);
    airtime.held.crossfade = true;

    assert!(airtime.set_on(false));
    assert!(!airtime.live());
    assert!(!airtime.held.any(), "it kept holding controls after going off");
}

#[test]
fn preloaded_records_are_not_mistaken_for_completed_queue_loads() {
    let items = pair();
    let records = library_for(&items);
    let mut airtime = airtime_with(items, 1.0);
    let status = [busy(true, false); DECKS]; // both old records are loaded
    let mut plan = Plan::default();
    airtime.assign(&records, status, &mut plan);
    airtime.drive(status, &mut plan);
    assert_eq!(plan.load.len(), 2);
    assert!(plan.start.is_empty());
    let mut pending = Plan::default();
    airtime.drive(status, &mut pending);
    assert!(pending.start.is_empty(), "old loaded flag acknowledged the new record");
    airtime.loading(0, 4);
    airtime.deck_ready(0, "inoha|geshuou", 4);
    let mut ready = Plan::default();
    airtime.drive(status, &mut ready);
    assert_eq!(ready.start, vec![(0, 1.0)]);
}

#[test]
fn a_load_finishing_for_another_record_or_generation_is_not_this_one() {
    let items = pair();
    let records = library_for(&items);
    let mut airtime = airtime_with(items, 1.0);
    let mut plan = Plan::default();
    airtime.assign(&records, [busy(false, false); DECKS], &mut plan);
    airtime.loading(0, 7);
    airtime.deck_ready(0, "something else", 7);
    assert!(!airtime.decks[0].as_ref().unwrap().ready, "another record's load was taken for this one");
    airtime.deck_ready(0, "inoha|geshuou", 6);
    assert!(!airtime.decks[0].as_ref().unwrap().ready, "a stale decode was taken for this one");
    airtime.deck_failed(0, "inoha|geshuou", 6);
    assert!(airtime.decks[0].is_some(), "a stale failure released the deck");
    airtime.deck_ready(0, "inoha|geshuou", 7);
    assert!(airtime.decks[0].as_ref().unwrap().ready);
}

#[test]
fn a_record_you_load_yourself_takes_the_deck_back() {
    let mut airtime = airtime_with(pair(), 1.0);
    airtime.decks[1] = held("next", false, true);
    airtime.release_deck(1);
    assert!(airtime.decks[1].is_none());
    let records = library_for(&airtime.schedule);
    let mut plan = Plan::default();
    airtime.assign(&records, [busy(true, true), busy(true, false)], &mut plan);
    assert!(plan.load.iter().all(|(_, record, _)| record.key != "second|record"),
            "the station put its record straight back");
}

#[test]
fn opening_pair_honors_their_original_decks() {
    let mut items = pair();
    items[0].preferred_deck = Some(1);
    items[1].preferred_deck = Some(0);
    let records = library_for(&items);
    let mut airtime = airtime_with(items, 0.0);
    let mut plan = Plan::default();
    airtime.assign(&records, [busy(true, false); DECKS], &mut plan);
    assert_eq!(plan.load.iter().map(|(d, _, _)| *d).collect::<Vec<_>>(), vec![1, 0]);
}

#[test]
fn clock_advances_between_polls_and_finished_decks_keep_refilling() {
    let mut items = pair();
    let mut third = items[1].clone();
    third.id = "third".into();
    third.key = "third|record".into();
    third.start_at = 388.0;
    items.push(third);
    let records = library_for(&items);
    let mut airtime = airtime_with(items, 199.0);
    airtime.clock.anchor(199.0, 0);
    airtime.last_poll = Instant::now();
    airtime.last_queue = Instant::now();
    for (deck, id) in ["43ac", "next"].into_iter().enumerate() {
        airtime.decks[deck] = held(id, true, true);
    }
    let plan = airtime.tick(96_000, &records, [busy(true, true); DECKS], true);
    assert_eq!(airtime.station_now, 201.0);
    assert_eq!(plan.stop, vec![0]);
    let refill = airtime.tick(96_480, &records, [busy(true, false), busy(true, true)], true);
    assert_eq!(refill.load.len(), 1);
    assert_eq!(refill.load[0].0, 0);
    airtime.loading(0, 1);
    airtime.deck_ready(0, "third|record", 1);
    let play = airtime.tick(189 * 48_000, &records, [busy(true, false), busy(true, true)], true);
    assert!(play.start.iter().any(|(d, _)| *d == 0), "third record never started");
}

#[test]
fn actual_deck_mixer_preserves_both_envelopes_including_ducks_and_cuts() {
    for levels in [[1.0, 1.0], [0.22, 0.22], [0.75, 0.25], [0.0, 1.0], [0.0, 0.0]] {
        let (x, gain) = mixer_levels(levels);
        for deck in 0..DECKS {
            let mut audio = crate::engine::deck::Deck::new(48_000);
            audio.track = Some(Arc::new(crate::engine::decode::Track { samples: vec![0.1; 200], sample_rate: 48_000 }));
            audio.playing = true;
            let position = if deck == 0 { x } else { 1.0 - x };
            audio.gain = gain * (position * std::f32::consts::FRAC_PI_2).cos();
            let mut out = [0.0; 100];
            audio.mix_into(&mut out, 48_000);
            assert!((out[80] - 0.1 * levels[deck]).abs() < 0.0001, "{levels:?}: {out:?}");
            assert!(audio.seconds() > 0.0, "a muted deck stopped advancing");
        }
    }
}

#[test]
fn an_incoming_decode_does_not_fade_or_filter_the_only_ready_deck() {
    let mut airtime = airtime_with(pair(), 197.0);
    airtime.decks[0] = held("43ac", true, true);
    airtime.decks[1] = held("next", false, false);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    assert_eq!(plan.level, [Some(1.0), Some(0.0)]);
    assert_eq!(plan.tone[0], Some([0.5, 0.5, 0.5, 0.0]));
    assert!(plan.start.is_empty());
}

#[test]
fn station_stopping_pauses_the_music_decks_as_well_as_speech() {
    let mut airtime = airtime_with(pair(), 197.0);
    airtime.decks[0] = held("43ac", true, true);
    let plan = airtime.tick(0, &[], [busy(true, true); DECKS], false);
    assert_eq!(plan.stop, vec![0]);
    assert!(!airtime.on);
    assert!(plan.voice.iter().any(|c| matches!(c, Command::OffAir)));
    assert!(plan.restore);
}

#[test]
fn an_old_poll_cannot_replace_a_new_opening_lineup() {
    let mut airtime = airtime_with(pair(), 0.0);
    airtime.start_with(serde_json::json!([]));
    airtime.last_poll = Instant::now();
    airtime.last_queue = Instant::now();
    let old = airtime.session - 1;
    airtime.asks.inflight_for_test(Ask::Schedule(old), Ok(serde_json::from_str(REAL).unwrap()));
    airtime.tick(0, &[], [busy(true, false); DECKS], true);
    assert!(airtime.schedule.is_empty());
    assert!(airtime.startup.is_some());
}

#[test]
fn rejected_opening_tracks_stop_retrying_and_restore_channel_levels() {
    let mut airtime = airtime_with(pair(), 0.0);
    airtime.start_with(serde_json::json!([]));
    airtime.last_queue = Instant::now();
    let refused = Failure { status: Some(400), message: "opening track missing".into(), timed_out: false };
    airtime.asks.inflight_for_test(Ask::Start(airtime.session), Err(refused));
    let plan = airtime.tick(0, &[], [busy(true, false); DECKS], true);
    assert!(!airtime.on);
    assert!(plan.restore);
    assert_eq!(plan.note.as_deref(), Some("Could not start deck playback: opening track missing"));
}

#[test]
fn a_station_not_ready_yet_is_retried_quietly_rather_than_refused() {
    let mut airtime = airtime_with(pair(), 0.0);
    airtime.start_with(serde_json::json!([]));
    airtime.last_queue = Instant::now();
    let busy_station = Failure { status: Some(503), message: "warming up".into(), timed_out: false };
    airtime.asks.inflight_for_test(Ask::Start(airtime.session), Err(busy_station));
    let plan = airtime.tick(0, &[], [busy(true, false); DECKS], true);
    assert!(airtime.on, "a cold station was taken for a refusal");
    assert!(airtime.startup.is_some());
    assert!(plan.note.is_none(), "cold start spammed a notice");
    assert!(airtime.startup_next > Instant::now(), "it will ask again at once");
}

#[test]
fn the_opening_lineup_waits_for_the_station_to_answer_first() {
    let mut airtime = airtime_with(pair(), 0.0);
    airtime.start_with(serde_json::json!([]));
    airtime.last_queue = Instant::now();
    airtime.tick(0, &[], [busy(true, false); DECKS], true);
    assert!(!airtime.asks.busy(&Ask::Start(airtime.session)), "posted to a station that never answered");
    airtime.station_ready(true);
    airtime.tick(0, &[], [busy(true, false); DECKS], true);
    assert!(airtime.asks.busy(&Ask::Start(airtime.session)));
}

#[test]
fn a_restarted_station_takes_over_the_records_already_playing() {
    let mut airtime = airtime_with(pair(), 20.0);
    airtime.epoch = 3;
    airtime.decks[0] = Some(Assigned { key: "inoha|geshuou".into(), ..held("43ac", true, true).unwrap() });
    airtime.resume_with(serde_json::json!([{"key": "inoha|geshuou", "deck": 0, "offset": 20.0}]));
    let mut renamed = pair();
    renamed[0].id = "new-43ac".into();
    renamed[0].start_at = 21.0;
    renamed[0].offset = 20.0;
    let mut plan = Plan::default();
    airtime.absorb(snapshot_at(renamed, 21.0, 0), None, &mut plan);
    assert!(plan.stop.is_empty(), "the record playing through the restart was stopped");
    let assigned = airtime.decks[0].as_ref().unwrap();
    assert_eq!(assigned.id, "new-43ac");
    assert!(assigned.started, "it was re-cued, which is a jump in the music");
}

#[test]
fn speech_ducks_the_deck_level_whoever_holds_the_fader() {
    let mut items = pair();
    items[0].deck_envelope = vec![[0.0, 1.0], [200.0, 1.0]];
    items[0].envelope = vec![[0.0, 0.1], [200.0, 0.1]];
    let mut speech = items[1].clone();
    speech.kind = "voice".into();
    speech.id = "speech".into();
    speech.start_at = 10.0;
    speech.duration = 10.0;
    items.push(speech);
    let mut airtime = airtime_with(items, 12.0);
    airtime.decks[0] = held("43ac", true, true);
    airtime.held.gain[0] = true;
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert_eq!(plan.duck, Some(0.1));
    assert!((plan.level[0].unwrap() - 0.1).abs() < 1e-4, "the duck missed a held fader");
    airtime.held.gain[0] = false;
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert!((plan.level[0].unwrap() - 0.1).abs() < 1e-4, "ducked twice, or not at all");
    assert_eq!(airtime.speech_duck(21.7), 1.0);
}

#[test]
fn pitched_records_start_at_the_correct_source_position() {
    let mut items = pair();
    items[0].playback_rate = 0.95;
    let mut airtime = airtime_with(items, 20.0);
    airtime.decks[0] = held("43ac", false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, false); DECKS], &mut plan);
    assert_eq!(plan.start, vec![(0, 19.0)]);
    assert_eq!(plan.speed[0], Some(0.95));
    let rate = automated(&plan, 0, Lane::Rate).expect("no rate lane");
    assert!((rate.at(rate.start_frame(), &mut 0).unwrap() - 0.95).abs() < 1e-6);
}

fn following(position: f64, rate: f64) -> [DeckStatus; DECKS] {
    [DeckStatus { loaded: true, playing: true, position: Some(position), playback_rate: rate, base_gain: 1.0 },
     DeckStatus::default()]
}

fn started_at(id: &str, at: f64) -> Option<Assigned> {
    let mut assigned = held(id, true, true);
    assigned.as_mut().unwrap().armed_at = Some(at);
    assigned
}

#[test]
fn feedback_recovers_small_clock_error_without_a_seek() {
    let mut airtime = airtime_with(pair(), 20.0);
    airtime.decks[0] = started_at("43ac", 0.0);
    let mut position = 19.8;
    let mut speed = 1.0;
    for _ in 0..600 {
        let mut plan = Plan::default();
        airtime.drive(following(position, speed), &mut plan);
        assert!(plan.start.is_empty(), "feedback must never seek audible playback");
        speed = plan.speed[0].unwrap_or(speed);
        assert!((speed - 1.0).abs() <= 0.005001);
        position += speed * 0.1;
        airtime.station_now += 0.1;
    }
    assert!((airtime.station_now - position).abs() < 0.035);
}

#[test]
fn feedback_goes_through_the_rate_lane() {
    let mut airtime = airtime_with(pair(), 20.0);
    airtime.decks[0] = started_at("43ac", 0.0);
    let mut plan = Plan::default();
    airtime.drive(following(20.0, 1.0), &mut plan);
    let mut plan = Plan::default();
    airtime.drive(following(19.8, 1.0), &mut plan);
    let rate = automated(&plan, 0, Lane::Rate).expect("the correction was not sent to the engine");
    assert!((rate.at(rate.start_frame(), &mut 0).unwrap() - 1.005).abs() < 1e-6);
    assert!(automated(&plan, 0, Lane::Level).is_none(), "a tempo correction re-sent every lane");
}

#[test]
fn feedback_respects_manual_tempo_and_waits_out_a_large_error() {
    let mut airtime = airtime_with(pair(), 20.0);
    airtime.decks[0] = started_at("43ac", 0.0);
    airtime.held.tempo[0] = true;
    let mut plan = Plan::default();
    airtime.drive(following(19.8, 0.95), &mut plan);
    assert_eq!(plan.speed[0], None);
    airtime.held.tempo[0] = false;
    // A reading twelve seconds out is a stale one, not a reason to stop
    // following the clock for the rest of the record.
    let mut plan = Plan::default();
    airtime.drive(following(8.0, 1.0), &mut plan);
    assert_eq!(plan.speed[0], None);
    assert!(plan.start.is_empty());
    assert!(!airtime.held.tempo[0], "jitter latched tempo correction off");
    airtime.station_now += 0.1;
    let mut plan = Plan::default();
    airtime.drive(following(19.9, 1.0), &mut plan);
    assert!(plan.speed[0].is_some_and(|s| (s - 1.005).abs() < 1e-6), "feedback did not resume after the bad reading");
}

#[test]
fn a_deck_that_stays_far_off_the_clock_is_put_back_once() {
    let mut airtime = airtime_with(pair(), 20.0);
    airtime.decks[0] = started_at("43ac", 0.0);
    let mut started = 0;
    for _ in 0..40 {
        let mut plan = Plan::default();
        airtime.drive(following(5.0, 1.0), &mut plan);
        started += plan.start.len();
        airtime.station_now += 0.1;
    }
    assert_eq!(started, 1, "a lasting error was never corrected, or corrected over and over");
}

#[test]
fn feedback_waits_for_start_telemetry_and_can_be_disabled() {
    let mut airtime = airtime_with(pair(), 20.0);
    airtime.decks[0] = started_at("43ac", 20.0);
    let mut plan = Plan::default();
    airtime.drive(following(0.0, 1.0), &mut plan);
    assert!(!airtime.held.tempo[0]);
    assert_eq!(plan.speed[0], None);
    airtime.decks[0].as_mut().unwrap().armed_at = Some(0.0);
    airtime.schedule[0].playback_feedback[0] = 0.0;
    airtime.drive(following(19.8, 1.0), &mut plan);
    assert_eq!(plan.speed[0], None);
}

#[test]
fn a_manual_tempo_on_the_cued_deck_survives_its_automatic_start() {
    let mut airtime = airtime_with(pair(), 194.0);
    airtime.decks[1] = held("next", false, true);
    airtime.held.tempo[1] = true;
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    assert_eq!(plan.start, vec![(1, 0.0)]);
    assert_eq!(plan.speed[1], None);
    assert!(automated(&plan, 1, Lane::Rate).is_none(), "the held tempo got a rate lane");
}

#[test]
fn paused_outgoing_deck_does_not_leave_incoming_filtered_and_quiet() {
    let mut airtime = airtime_with(pair(), 197.0);
    airtime.decks[0] = held("43ac", true, true);
    airtime.decks[1] = held("next", true, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, false), busy(true, true)], &mut plan);
    assert_eq!(plan.level, [Some(0.0), Some(1.0)]);
    assert_eq!(plan.crossfade, Some(1.0));
    assert_eq!(plan.tone[1], Some([0.5, 0.5, 0.5, 0.0]));
    assert!(plan.start.is_empty(), "a manual pause must not resume itself");
}

#[test]
fn late_incoming_decode_rejoins_the_blend_gradually() {
    let mut airtime = airtime_with(pair(), 197.0);
    airtime.decks[0] = held("43ac", true, true);
    airtime.decks[1] = held("next", false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    assert_eq!(plan.crossfade, Some(0.0));
    assert_eq!(plan.tone[0], Some([0.5, 0.5, 0.5, 0.0]));
    airtime.station_now += 0.25;
    let mut half = Plan::default();
    airtime.drive([busy(true, true); DECKS], &mut half);
    assert!(half.crossfade.unwrap() > 0.0);
    assert!(half.start.is_empty());
}

#[test]
fn tempo_recovery_integrates_source_position_and_late_cues() {
    let mut item = pair().remove(0);
    item.offset = 7.0;
    item.playback_rate = 0.96;
    item.rate_curve = rate_curve_from(&serde_json::json!([[0,0.96],[10,0.96],[30,1.0]]));
    assert!((item.source_at(20.0) - 26.3).abs() < 1e-8);
    assert!((item.source_at(35.0) - 41.2).abs() < 1e-8);
    assert!((item.rate_at(20.0) - 0.98).abs() < 1e-8);
    assert_eq!(item.rate_at(35.0), 1.0);
    item.start_at = 0.0;
    let mut airtime = airtime_with(vec![item], 20.0);
    let id = airtime.schedule[0].id.clone();
    airtime.decks[0] = held(&id, false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, false), DeckStatus::default()], &mut plan);
    assert!((plan.start[0].1 - 26.3).abs() < 1e-8);
    assert!((plan.speed[0].unwrap() - 0.98).abs() < 1e-8);
}

#[test]
fn malformed_rate_curves_fall_back_to_constant_speed() {
    for value in [serde_json::json!([[1,1.0]]), serde_json::json!([[0,1.0],[0,1.02]]),
                  serde_json::json!([[0,1.0],[10,0.5]]), serde_json::json!([[0,1.0],[10,null]])] {
        assert!(rate_curve_from(&value).is_empty());
    }
}

/* -- Written transitions -- */

fn written_pair() -> Vec<Scheduled> {
    snapshot_from(&serde_json::json!({"now": 0, "items": [
        {"id": "a", "kind": "music", "url": "/a", "start_at": 0, "duration": 200,
         "envelope": [[0, 1], [194, 1], [200, 0]], "meta": {"key": "a", "beat_period": 0.5}},
        {"id": "b", "kind": "music", "url": "/b", "start_at": 194, "duration": 200,
         "envelope": [[0, 0], [6, 1]],
         "meta": {"key": "b", "transition": {"preset": "stutter_drop", "technique": "stutter_drop",
            "lanes": {"out": {"sweep": [[194, 0.0], [199, 0.8]], "reverb_send": [[196, 0.0], [199, 0.6]]},
                      "in": {"stem_drums": [[194, 0.0], [196, 0.0], [196, 1.0]]}},
            "events": [{"type": "roll", "deck": "out", "at": 198, "length_beats": 1, "until": 199},
                       {"type": "loop", "deck": "in", "at": 195, "length_seconds": 0.25, "until": 196},
                       {"type": "spin", "deck": "in", "at": 195, "length_seconds": 1, "until": 196}]}}}
    ]}), 0).items
}

#[test]
fn written_lanes_go_to_the_deck_their_role_names() {
    let mut airtime = airtime_with(written_pair(), 192.5);
    airtime.decks[0] = held("a", true, true);
    airtime.decks[1] = held("b", false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    let frame = |t: f64| airtime.clock.frame_at(t).unwrap();
    let sweep = automated(&plan, 0, Lane::Sweep).expect("the outgoing deck's sweep");
    assert!((sweep.at(frame(199.0), &mut 0).unwrap() - 0.8).abs() < 1e-6);
    let reverb = automated(&plan, 0, Lane::ReverbSend).expect("the outgoing deck's reverb send");
    assert!((reverb.at(frame(199.0), &mut 0).unwrap() - 0.6).abs() < 1e-6);
    let drums = automated(&plan, 1, Lane::StemDrums).expect("the incoming deck's drums");
    assert_eq!(drums.at(frame(195.0), &mut 0), Some(0.0));
    assert_eq!(drums.at(frame(196.0), &mut 0), Some(1.0), "the drop is a step on its frame");
    assert!(automated(&plan, 1, Lane::Sweep).map_or(true, |s| s.at(frame(199.0), &mut 0) == Some(0.0)),
            "the outgoing lane leaked onto the incoming deck");
}

#[test]
fn rolls_go_to_the_engine_once_on_their_frames() {
    let mut airtime = airtime_with(written_pair(), 192.5);
    airtime.clock.anchor(192.5, 0);
    airtime.decks[0] = held("a", true, true);
    airtime.decks[1] = held("b", false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    let rolls: Vec<(usize, u64, f64, u64)> = plan.automation.iter().filter_map(|c| match c {
        Command::LoopAt { deck, frame, length_seconds, until_frame } => Some((*deck, *frame, *length_seconds, *until_frame)),
        _ => None,
    }).collect();
    assert_eq!(rolls, vec![
        (0, 5 * 48_000 + 24_000, 0.5, 6 * 48_000 + 24_000),
        (1, 2 * 48_000 + 24_000, 0.25, 3 * 48_000 + 24_000),
    ], "unknown event types are dropped; beats use the record's grid");
    let mut again = Plan::default();
    airtime.station_now = 192.6;
    airtime.drive([busy(true, true), busy(true, false)], &mut again);
    assert!(!again.automation.iter().any(|c| matches!(c, Command::LoopAt { .. })), "rolls were sent twice");
}

#[test]
fn a_written_lane_is_handed_back_when_it_is_done() {
    let mut airtime = airtime_with(written_pair(), 190.0);
    airtime.decks[0] = held("a", true, true);
    airtime.decks[1] = held("b", false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    airtime.station_now = 199.5;
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, true)], &mut plan);
    assert!(plan.released.contains(&(0, Lane::ReverbSend)));
    assert!(plan.automation.iter().any(|c| matches!(c, Command::ClearAutomation { deck: 0, lane: Lane::ReverbSend })));
}

/* -- Letting go -- */

fn cleared(plan: &Plan, deck: usize, lane: Lane) -> bool {
    plan.automation.iter().any(|c| matches!(c, Command::ClearAutomation { deck: d, lane: l } if *d == deck && *l == lane))
}

fn uncued(plan: &Plan, deck: usize) -> bool {
    plan.automation.iter().any(|c| matches!(c, Command::Loop { deck: d, range: None } if *d == deck))
}

#[test]
fn a_record_ending_lets_go_of_every_lane_even_one_ending_with_it() {
    // A lane that ends when its record does never reaches its own clear
    // time before the record is let go; whatever it was left at would
    // carry into the next record on that deck.
    let mut items = written_pair();
    if let Some(transition) = items[1].transition.as_mut() {
        transition.lanes.push(protocol::Authored {
            role: protocol::Role::Out, lane: Lane::StemVocals, points: vec![(196.0, 1.0), (200.0, 0.0)],
        });
    }
    let mut airtime = airtime_with(items, 190.0);
    airtime.decks[0] = held("a", true, true);
    airtime.decks[1] = held("b", false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    assert!(automated(&plan, 0, Lane::StemVocals).is_some());
    let mut plan = Plan::default();
    airtime.finish(0, &mut plan);
    for lane in [Lane::StemVocals, Lane::ReverbSend, Lane::Level, Lane::Low, Lane::Rate] {
        assert!(cleared(&plan, 0, lane), "{lane:?} was left on the deck");
        assert!(plan.released.contains(&(0, lane)), "{lane:?} was not handed back");
    }
    assert!(uncued(&plan, 0), "rolls planned for a finished record were left to fire");
}

#[test]
fn a_deck_taken_back_by_hand_is_cleared_at_the_next_plan() {
    let mut airtime = airtime_with(written_pair(), 190.0);
    airtime.decks[0] = held("a", true, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    airtime.release_deck(0);
    let plan = airtime.tick(0, &[], [busy(true, true), busy(false, false)], true);
    assert!(cleared(&plan, 0, Lane::Level));
    assert!(uncued(&plan, 0));
}

#[test]
fn a_fresh_deck_lets_go_of_lanes_only_a_transition_moves() {
    let mut airtime = airtime_with(written_pair(), 190.0);
    airtime.decks[0] = held("a", true, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert!(cleared(&plan, 0, Lane::StemDrums), "a stem lane from before could still be holding");
    assert!(!cleared(&plan, 0, Lane::ReverbSend), "a lane being sent was cleared as well");
}

#[test]
fn rolls_follow_the_clock_when_it_moves_under_them_and_go_on_a_skip() {
    let mut airtime = airtime_with(written_pair(), 192.5);
    airtime.clock.anchor(192.5, 0);
    airtime.decks[0] = held("a", true, true);
    airtime.decks[1] = held("b", false, true);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    // A 50 ms correction lands before the first roll.
    airtime.clock.anchor(192.55, 0);
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    assert!(uncued(&plan, 1) && uncued(&plan, 0), "the old rolls were not called off");
    let rolls = plan.automation.iter().filter(|c| matches!(c, Command::LoopAt { .. })).count();
    assert_eq!(rolls, 2, "the rolls were not placed again");
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(true, false)], &mut plan);
    assert!(!plan.automation.iter().any(|c| matches!(c, Command::LoopAt { .. })), "placed again with nothing moved");

    airtime.epoch = 0;
    let mut plan = Plan::default();
    let items = airtime.schedule.clone();
    airtime.absorb(snapshot_at(items, 150.0, 1), None, &mut plan);
    assert!(uncued(&plan, 0) && uncued(&plan, 1), "a skip left rolls placed on the old clock");
}

#[test]
fn a_tempo_correction_does_not_move_the_clock_reference() {
    let mut airtime = airtime_with(pair(), 20.0);
    airtime.decks[0] = started_at("43ac", 0.0);
    let mut plan = Plan::default();
    airtime.drive(following(20.0, 1.0), &mut plan);
    let reference = airtime.sent[0].as_ref().map(|s| (s.ref_frame, s.ref_time)).unwrap();
    airtime.station_now += 1.0;
    let mut plan = Plan::default();
    airtime.drive(following(20.8, 1.0), &mut plan);
    assert!(automated(&plan, 0, Lane::Rate).is_some());
    let after = airtime.sent[0].as_ref().map(|s| (s.ref_frame, s.ref_time)).unwrap();
    assert_eq!(after, reference, "a rate-only resend hid the drift of every other curve");
}

#[test]
fn the_echo_return_is_set_once_per_record() {
    let mut items = pair();
    items[0].echo = Some([190.0, 200.0, 0.5, 0.3, 0.4]);
    let mut airtime = airtime_with(items, 185.0);
    airtime.decks[0] = held("43ac", true, true);
    let echoes = |plan: &Plan| plan.automation.iter().filter(|c| matches!(c, Command::Echo { .. })).count();
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert_eq!(echoes(&plan), 1);
    // Something unrelated sends every curve again: the ringing tail is left be.
    airtime.held.tone[0][1] = true;
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    assert!(automated(&plan, 0, Lane::Level).is_some());
    assert_eq!(echoes(&plan), 0, "a resend reset the echo");
}

#[test]
fn a_device_at_a_new_rate_places_everything_again() {
    let mut airtime = airtime_with(pair(), 20.0);
    airtime.decks[0] = held("43ac", true, true);
    airtime.clock.anchor(20.0, 960_000);
    airtime.frame = 960_000;
    let mut plan = Plan::default();
    airtime.drive([busy(true, true), busy(false, false)], &mut plan);
    airtime.voices.on_air.insert("line".into(), 2);
    assert!(airtime.device(44_100, 1, 961_000));
    assert!(!airtime.device(44_100, 1, 961_000));
    let plan = airtime.tick(961_000, &[], [busy(true, true), busy(false, false)], true);
    assert!(automated(&plan, 0, Lane::Level).is_some(), "curves kept their old-rate frames");
    assert!(plan.voice.iter().any(|c| matches!(c, Command::Air { channel: 2, item: None })),
            "a host line kept its old-rate start");
    assert!(airtime.voices.on_air.is_empty());
}
