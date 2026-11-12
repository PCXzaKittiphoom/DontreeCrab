//! Core library for reading MIDI karaoke songs.
//!
//! Three karaoke container formats are supported:
//!
//! * [`SourceFormat::Kar`] - a Standard MIDI File (`.kar`) where the lyric
//!   syllables are stored as `Lyric` (0x05) meta events.
//! * [`SourceFormat::Ncn`] - Nick Karaoke's three-file layout
//!   (`.mid` + `.lyr` + `.cur`).
//! * [`SourceFormat::Kmid`] - a plain `.mid` with an embedded `LyrHdr`
//!   Base64/zlib XML lyric document.
//!
//! [`SourceFormat::Midi`] covers an ordinary MIDI file that carries none of
//! those markers; it still plays, but the UI labels it `MIDI Common`.
//!
//! All are normalised into the same [`Song`] model so the playback and UI
//! layers never need to know which format a song came from.

pub mod audio;
pub mod i18n;
pub mod kar;
pub mod kmid;
pub mod loader;
pub mod lyrics;
pub mod meter;
pub mod midi;
pub mod model;
pub mod ncn;
pub mod playlist;
pub mod settings;
pub mod soundfont;
pub mod tempo;
pub mod text;

pub use loader::{LoadOptions, load};
pub use model::{EventKind, LyricLine, MetaTextKind, Song, SourceFormat, Syllable, TimedEvent};
