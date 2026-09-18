//! The keyboard.
//!
//! Taken from the reference application's own default bindings rather than
//! invented: its plist stores Windows Set-1 scan codes in decimal, which is
//! how `#57` turns out to be Space and the EQ kills turn out to be X/C/V and
//! B/N/M. That decoding is the only reason these are the real layout and not
//! a plausible one.
//!
//! Deck 1 lives under the left hand and deck 2 under the right, mirrored
//! about the middle of the keyboard the same way the panel is mirrored about
//! the middle of the window.
//!
//! ```text
//!   1  2  3  4                                      7  8  9  0
//!   |  |  |  |                                      |  |  |  |
//!   |  |  +--+-- skip back / forward ---------------+--+  |  |
//!   |  +-------- sync -----------------------------------+  |
//!   +----------- play / pause -----------------------------+
//!
//!   Q  W  E  R  T                                Y  U  I  O  P
//!   |  +--+--+--+-- jump to cue 1-4 -------------+--+--+--+  |
//!   +-------------- back to the start --------------------- +
//!
//!   A  S  D    F  G  H                              J  K  L
//!   |  |  |    |  |  +-- auto-cut fast              |  |  |
//!   |  |  |    |  +----- auto-cut slow              |  |  |
//!   |  |  |    +-------- cut crossfader             |  |  |
//!   |  |  +-- loop out ------------------------ loop out  |
//!   |  +----- loop in -------------------------- loop in  |
//!   +-------- loop on/off --------------------- loop on/off
//!
//!   X  C  V                                         B  N  M
//!   +--+--+-- low / mid / high kill ----------------+--+--+
//! ```
//!
//! Held modifiers follow the reference too: `alt` sets a cue where the plain
//! key jumps to it, `ctrl+alt` reaches the second function of a transport
//! key, and `shift+ctrl+alt` reaches the third.

use egui::{Context, Key, Modifiers};

use crate::Defalt;

/// One deck's worth of keys. Everything is a pair, so the two decks are the
/// same table read from opposite ends.
struct Deck {
    play: Key,
    sync: Key,
    skip_back: Key,
    skip_forward: Key,
    start: Key,
    cues: [Key; 4],
    loop_toggle: Key,
    loop_in: Key,
    loop_out: Key,
    kills: [Key; 3],
}

const DECKS: [Deck; 2] = [
    Deck {
        play: Key::Num1,
        sync: Key::Num2,
        skip_back: Key::Num3,
        skip_forward: Key::Num4,
        start: Key::Q,
        cues: [Key::W, Key::E, Key::R, Key::T],
        loop_toggle: Key::A,
        loop_in: Key::S,
        loop_out: Key::D,
        // low, mid, high
        kills: [Key::X, Key::C, Key::V],
    },
    Deck {
        play: Key::Num0,
        sync: Key::Num9,
        skip_back: Key::Num7,
        skip_forward: Key::Num8,
        start: Key::P,
        cues: [Key::O, Key::I, Key::U, Key::Y],
        loop_toggle: Key::L,
        loop_in: Key::J,
        loop_out: Key::K,
        kills: [Key::B, Key::N, Key::M],
    },
];

/// Beats a skip moves. The reference skips by a beat; without a grid we fall
/// back to a fixed slice of time so the key still does something sensible.
const SKIP_BEATS: f64 = 4.0;
const SKIP_FALLBACK_SECONDS: f64 = 2.0;

/// How far a pitch bend pulls while held, and how far a speed step moves.
const BEND_PERCENT: f32 = 4.0;
const SPEED_STEP: f32 = 0.1;

pub fn handle(app: &mut Defalt, ctx: &Context) {
    // Never while a text field has the caret: someone searching the crate for
    // "space" must not start deck A.
    if ctx.memory(|m| m.focused().is_some()) {
        release_bends(app);
        return;
    }

    let input = ctx.input(|i| Snapshot::from(i));

    // Space acts on the deck you last touched. With nothing touched yet it
    // takes whichever deck has a record, and with both it takes deck A --
    // guessing is better than doing nothing, and it is one key away from
    // being corrected.
    if input.pressed(Key::Space, Modifiers::NONE) {
        let deck = app.active_deck;
        app.play_pause(deck);
    }

    for (index, deck) in DECKS.iter().enumerate() {
        deck_keys(app, &input, index, deck);
    }

    mixer_keys(app, &input);
    library_keys(app, &input);

    // Held, not pressed: a bend lasts as long as your finger does.
    for index in 0..2 {
        let deck = &DECKS[index];
        let bend = if input.held(deck.skip_forward, Modifiers::CTRL | Modifiers::ALT) {
            BEND_PERCENT
        } else if input.held(deck.skip_back, Modifiers::CTRL | Modifiers::ALT) {
            -BEND_PERCENT
        } else {
            0.0
        };
        app.set_bend(index, bend);
    }
}

/// Focus changes and leaving the console must not leave a held bend latched.
pub fn release_bends(app: &mut Defalt) {
    for deck in 0..2 {
        app.set_bend(deck, 0.0);
    }
}

fn deck_keys(app: &mut Defalt, input: &Snapshot, index: usize, deck: &Deck) {
    let ctrl_alt = Modifiers::CTRL | Modifiers::ALT;
    let all_three = Modifiers::SHIFT | Modifiers::CTRL | Modifiers::ALT;

    if input.pressed(deck.play, Modifiers::NONE) {
        app.play_pause(index);
    }
    if input.pressed(deck.play, ctrl_alt) {
        app.toggle_reverse(index);
    }
    if input.pressed(deck.sync, Modifiers::NONE) {
        if let Err(error) = app.sync(index) {
            app.decks[index].error = Some(error);
        }
    }
    if input.pressed(deck.skip_back, Modifiers::NONE) {
        app.skip(index, -SKIP_BEATS);
    }
    if input.pressed(deck.skip_forward, Modifiers::NONE) {
        app.skip(index, SKIP_BEATS);
    }
    if input.pressed(deck.skip_back, all_three) {
        let next = (app.decks[index].pitch - SPEED_STEP).max(-8.0);
        app.set_pitch(index, next);
    }
    if input.pressed(deck.skip_forward, all_three) {
        let next = (app.decks[index].pitch + SPEED_STEP).min(8.0);
        app.set_pitch(index, next);
    }
    if input.pressed(deck.start, Modifiers::NONE) {
        app.cue(index);
    }

    for (slot, key) in deck.cues.iter().enumerate() {
        // Alt sets, plain jumps -- the reference's split, and the safe way
        // round: the destructive one needs a modifier.
        if input.pressed(*key, Modifiers::ALT) {
            app.set_cue(index, slot);
        } else if input.pressed(*key, Modifiers::NONE) {
            app.jump_to_cue(index, slot);
        }
    }

    for (band, key) in deck.kills.iter().enumerate() {
        if input.pressed(*key, Modifiers::NONE) {
            app.toggle_kill(index, band);
        }
    }

    // Loops have no engine behind them yet. Saying so beats silence, which
    // reads as a broken key.
    for key in [deck.loop_toggle, deck.loop_in, deck.loop_out] {
        if input.pressed(key, Modifiers::NONE) {
            app.say("Loops are not wired up yet.");
        }
    }
}

fn mixer_keys(app: &mut Defalt, input: &Snapshot) {
    let shift_ctrl = Modifiers::SHIFT | Modifiers::CTRL;

    // Arrows glide the crossfader; shift+ctrl throws it.
    if input.held(Key::ArrowLeft, Modifiers::NONE) {
        app.nudge_crossfade(-1.0);
    }
    if input.held(Key::ArrowRight, Modifiers::NONE) {
        app.nudge_crossfade(1.0);
    }
    if input.pressed(Key::ArrowUp, Modifiers::CTRL) {
        app.set_crossfade(0.5);
    }
    if input.pressed(Key::ArrowLeft, shift_ctrl) {
        app.set_crossfade(0.0);
    }
    if input.pressed(Key::ArrowRight, shift_ctrl) {
        app.set_crossfade(1.0);
    }
    if input.pressed(Key::ArrowUp, shift_ctrl) {
        app.set_crossfade(0.5);
    }

    // F cuts the crossfader to the other side and back.
    if input.pressed(Key::F, Modifiers::NONE) {
        let to = if app.crossfade > 0.5 { 0.0 } else { 1.0 };
        app.set_crossfade(to);
    }
}

fn library_keys(app: &mut Defalt, input: &Snapshot) {
    if input.pressed(Key::ArrowDown, Modifiers::NONE) {
        app.move_selection(1);
    }
    if input.pressed(Key::ArrowUp, Modifiers::NONE) {
        app.move_selection(-1);
    }
    if input.pressed(Key::ArrowLeft, Modifiers::CTRL) {
        app.load_selected(0);
    }
    if input.pressed(Key::ArrowRight, Modifiers::CTRL) {
        app.load_selected(1);
    }
    if input.pressed(Key::F, Modifiers::CTRL) {
        app.focus_search = true;
    }
}

/// One frame's keyboard, read once.
///
/// egui's `pressed` ignores modifiers, so a plain binding would also fire on
/// every modified press of the same key: `1` would start the deck at the same
/// moment `ctrl+alt+1` reversed it.
struct Snapshot {
    pressed: Vec<(Key, Modifiers)>,
    held: Vec<Key>,
    modifiers: Modifiers,
}

impl Snapshot {
    fn from(input: &egui::InputState) -> Self {
        let mut pressed = Vec::new();
        for event in &input.events {
            if let egui::Event::Key { key, pressed: true, modifiers, repeat: false, .. } = event {
                pressed.push((*key, *modifiers));
            }
        }
        let held = ALL_KEYS
            .iter()
            .copied()
            .filter(|key| input.key_down(*key))
            .collect();
        Snapshot { pressed, held, modifiers: input.modifiers }
    }

    fn pressed(&self, key: Key, modifiers: Modifiers) -> bool {
        self.pressed
            .iter()
            .any(|(k, m)| *k == key && same(*m, modifiers))
    }

    fn held(&self, key: Key, modifiers: Modifiers) -> bool {
        self.held.contains(&key) && same(self.modifiers, modifiers)
    }
}

/// Exact modifier match, so a binding never fires as a side effect of a
/// longer one.
fn same(actual: Modifiers, wanted: Modifiers) -> bool {
    actual.shift == wanted.shift
        && actual.alt == wanted.alt
        && (actual.ctrl || actual.command) == (wanted.ctrl || wanted.command)
}

/// Only the keys that can be held for something. Kept small on purpose: this
/// is walked every frame.
const ALL_KEYS: [Key; 6] = [
    Key::Num3,
    Key::Num4,
    Key::Num7,
    Key::Num8,
    Key::ArrowLeft,
    Key::ArrowRight,
];

/// The layout, for the help overlay.
pub const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Decks",
        &[
            ("Space", "Play / pause the deck you last touched"),
            ("1 / 0", "Play / pause deck A / B"),
            ("2 / 9", "Sync deck A / B to the other one"),
            ("3 4 / 7 8", "Skip back / forward four beats"),
            ("Ctrl+Alt+3/4", "Pitch bend while held"),
            ("Shift+Ctrl+Alt+3/4", "Nudge the pitch fader"),
            ("Ctrl+Alt+1 / 0", "Reverse"),
            ("Q / P", "Back to the start"),
        ],
    ),
    (
        "Cues",
        &[
            ("W E R T", "Jump to deck A cue 1-4"),
            ("Y U I O", "Jump to deck B cue 4-1"),
            ("Alt + those", "Set that cue here"),
        ],
    ),
    (
        "EQ",
        &[
            ("X C V", "Kill deck A low / mid / high"),
            ("B N M", "Kill deck B low / mid / high"),
        ],
    ),
    (
        "Mixer",
        &[
            ("Left / Right", "Glide the crossfader"),
            ("Shift+Ctrl+Left/Right", "Throw it all the way"),
            ("Ctrl+Up", "Back to the middle"),
            ("F", "Cut to the other side"),
        ],
    ),
    (
        "Crate",
        &[
            ("Up / Down", "Move the selection"),
            ("Ctrl+Left / Right", "Load onto deck A / B"),
            ("Ctrl+F", "Search"),
            ("F1", "This list"),
            ("F12", "Save a screenshot to target/"),
        ],
    ),
];

pub const SKIP_FALLBACK: f64 = SKIP_FALLBACK_SECONDS;
