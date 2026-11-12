use serde::Serialize;

use crate::tempo::TempoMap;

/// Which on-disk container the song was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceFormat {
    Kar,
    Ncn,
    Kmid,
    /// An ordinary MIDI file that could not be identified as a karaoke
    /// format (no lyrics, KAR marker, NCN companions or `LyrHdr` payload).
    Midi,
}

/// A time-signature change (MIDI meta event 0x58).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TimeSignatureChange {
    pub tick: u64,
    pub numerator: u8,
    pub denominator: u8,
}

/// A fully parsed song, format independent.
#[derive(Debug, Clone, Serialize)]
pub struct Song {
    pub format: SourceFormat,
    pub title: Option<String>,
    pub artist: Option<String>,
    /// Musical key as written by the author (NCN stores it on line 3).
    pub key: Option<String>,
    pub language: Option<String>,
    pub info: Vec<String>,
    pub tempo_map: TempoMap,
    /// Time-signature changes, sorted by tick. Empty means 4/4.
    pub time_signatures: Vec<TimeSignatureChange>,
    pub ticks_per_quarter: u16,
    pub bpm: f64,
    pub duration_us: u64,
    pub track_count: usize,
    /// Musical events, sorted by time.
    pub events: Vec<TimedEvent>,
    /// Lyric lines, sorted by time, each with syllable-level timing.
    pub lyric_lines: Vec<LyricLine>,
}

impl Song {
    pub fn syllable_count(&self) -> usize {
        self.lyric_lines.iter().map(|l| l.syllables.len()).sum()
    }

    /// Index of the lyric line that should be highlighted at `at_us`.
    ///
    /// Returns `None` before the first line and `Some(last)` after the song.
    pub fn line_at(&self, at_us: u64) -> Option<usize> {
        if self.lyric_lines.is_empty() {
            return None;
        }
        match self
            .lyric_lines
            .binary_search_by(|line| line.start_us.cmp(&at_us))
        {
            Ok(i) => Some(i),
            Err(0) => None,
            Err(i) => Some(i - 1),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TimedEvent {
    pub at_us: u64,
    pub tick: u64,
    pub track: u16,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    NoteOn {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    PitchBend {
        channel: u8,
        /// -8192..=8191, 0 is centred.
        value: i16,
    },
    ChannelPressure {
        channel: u8,
        value: u8,
    },
    KeyPressure {
        channel: u8,
        key: u8,
        value: u8,
    },
    Text {
        text_kind: MetaTextKind,
        text: String,
    },
    Tempo {
        us_per_quarter: u32,
        bpm: f64,
    },
    SysEx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaTextKind {
    Text,
    Lyric,
    Marker,
    CuePoint,
    TrackName,
    InstrumentName,
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct LyricLine {
    pub start_us: u64,
    pub end_us: u64,
    pub text: String,
    pub syllables: Vec<Syllable>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Syllable {
    pub start_us: u64,
    pub end_us: u64,
    pub text: String,
}
