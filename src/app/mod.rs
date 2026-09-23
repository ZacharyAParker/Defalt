//! What the console does, as opposed to what it draws.
//!
//! `Defalt` is one struct -- the panel reads and writes the same fields a hand
//! would move -- but what it does falls into four parts, one file each: the
//! decks themselves, getting records onto them, the radio driving them, and
//! the background jobs that pull and separate records.

mod decks;
mod jobs;
mod loader;
mod radio_bridge;
