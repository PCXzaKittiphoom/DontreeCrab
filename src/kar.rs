use std::path::Path;

use anyhow::Result;

use crate::lyrics::{build_lines, is_control_marker};
use crate::midi::{self, MidiDoc, TextEvent};
use crate::model::{LyricLine, MetaTextKind, Song, SourceFormat};
use crate::text::{EncodingChoice, decode};

/// Read a `.kar` (or plain `.mid`) file and normalise it.
pub fn load(path: &Path, enc: EncodingChoice) -> Result<Song> {
    let bytes = std::fs::read(path)?;
    let doc = midi::parse(&bytes, enc)?;
    Ok(from_doc(doc, enc))
}

/// Build a [`Song`] from an already parsed MIDI document.
pub fn from_doc(doc: MidiDoc, enc: EncodingChoice) -> Song {
    let mut title = None;
    let mut artist = None;
    let key = None;
    let mut language = None;
    let mut info = Vec::new();

    for event in doc
        .text_events
        .iter()
        .filter(|t| t.kind == MetaTextKind::Text)
    {
        let text = decode(&event.raw, enc);
        let text = text.trim();

        if let Some(value) = text.strip_prefix("@T") {
            if title.is_none() && !value.trim().is_empty() {
                title = Some(value.trim().to_owned());
            }
        } else if let Some(value) = text.strip_prefix("@I") {
            let value = value.trim();
            if !value.is_empty() {
                if artist.is_none() {
                    artist = Some(value.to_owned());
                } else {
                    info.push(value.to_owned());
                }
            }
        } else if let Some(value) = text.strip_prefix("@L") {
            if language.is_none() && !value.trim().is_empty() {
                language = Some(value.trim().to_owned());
            }
        } else if let Some(value) = text.strip_prefix("@V") {
            info.push(format!("KAR version {}", value.trim()));
        }
    }

    let lyric_lines = extract_lyrics(&doc, enc);

    let bpm = doc.bpm();
    let ticks_per_quarter = doc.ticks_per_quarter;
    let duration_us = doc.duration_us;
    let track_count = doc.track_count;

    Song {
        format: SourceFormat::Kar,
        title,
        artist,
        key,
        language,
        info,
        tempo_map: doc.tempo_map,
        time_signatures: doc.time_signatures,
        ticks_per_quarter,
        bpm,
        duration_us,
        track_count,
        events: doc.events,
        lyric_lines,
    }
}

/// Collect lyric syllables.
///
/// Real KAR files use `Lyric` (0x05) meta events. Some authors dump everything
/// into `Text` (0x01) events instead, so fall back to those while skipping
/// `@T`/`@I`/... metadata. Only the three control markers (`@INTRO`, `@SOLO`,
/// `@OUTTRO`) are hidden; branding/credit lines remain normal lyrics.
fn extract_lyrics(doc: &MidiDoc, enc: EncodingChoice) -> Vec<LyricLine> {
    let mut events: Vec<&TextEvent> = doc
        .text_events
        .iter()
        .filter(|event| event.kind == MetaTextKind::Lyric)
        .collect();

    if events.is_empty() {
        events = doc
            .text_events
            .iter()
            .filter(|event| {
                event.kind == MetaTextKind::Text
                    && !decode(&event.raw, enc).trim_start().starts_with('@')
            })
            .collect();
    }

    let mut raw: Vec<Vec<(String, u64)>> = vec![Vec::new()];

    for event in events {
        let mut text = decode(&event.raw, enc);
        if text.is_empty() || is_control_marker(&text) {
            continue;
        }

        // Both '\' and '/' are used as "end of line" markers.
        let mut break_after = false;
        while text.ends_with('\\') || text.ends_with('/') {
            text.pop();
            break_after = true;
        }

        // Nick Karaoke style KARs use '_' for a space.
        let text = text.replace('_', " ");

        let mut first = true;
        for part in text.split(['\n', '\r']) {
            if !first {
                raw.push(Vec::new());
            }
            if !part.is_empty() {
                raw.last_mut()
                    .expect("raw always has a line")
                    .push((part.to_owned(), event.at_us));
            }
            first = false;
        }

        if break_after {
            raw.push(Vec::new());
        }
    }

    build_lines(raw, doc.duration_us)
}
