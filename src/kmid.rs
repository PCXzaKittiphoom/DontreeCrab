//! KMID: karaoke lyrics embedded inside an ordinary MIDI file.
//!
//! KMID files come from the eXtreme Karaoke / All In One Karaoke ecosystem.
//! Their word-by-word lyric cursor timeline is authored with Lyric Maker and
//! stored in a text meta event that starts with `LyrHdr`; the same tool can
//! convert a KMID to NCN. The payload is:
//!
//! ```text
//! "LyrHdr" + version (one character) + Base64( zlib( XML ) )
//! ```
//!
//! The XML document is named `SONG_LYRIC` and holds the title/artist/key plus
//! every lyric line, word and its start time. Despite declaring itself as
//! UTF-8, the document is normally encoded as TIS-620 / Windows-874 (Thai).

use std::io::Read;

use anyhow::{Context, Result, bail};
use flate2::read::ZlibDecoder;
use roxmltree::Document;

use crate::lyrics::{build_lines, is_control_marker};
use crate::midi::MidiDoc;
use crate::model::{Song, SourceFormat};
use crate::text::{EncodingChoice, decode};

const MAGIC: &[u8] = b"LyrHdr";
/// Safety limit for the decompressed XML document.
const MAX_XML_BYTES: u64 = 8 * 1024 * 1024;

/// If `doc` carries a KMID lyric payload, decode it and build a [`Song`].
///
/// Returns `Ok(None)` for ordinary MIDI/KAR/NCN files (no `LyrHdr` payload).
pub fn try_build(doc: &MidiDoc) -> Result<Option<Song>> {
    let Some(payload) = find_payload(doc) else {
        return Ok(None);
    };
    let xml = decode_payload(payload)?;
    let data = parse_xml(&xml, doc)?;
    Ok(Some(build(doc, data)))
}

/// Find the first text event that contains the KMID magic.
fn find_payload(doc: &MidiDoc) -> Option<&[u8]> {
    doc.text_events.iter().find_map(|event| {
        event
            .raw
            .windows(MAGIC.len())
            .position(|window| window == MAGIC)
            .map(|start| &event.raw[start..])
    })
}

/// Turn `LyrHdr<version><base64>` into the XML text.
fn decode_payload(payload: &[u8]) -> Result<String> {
    let rest = payload
        .strip_prefix(MAGIC)
        .context("missing KMID lyric magic")?;

    // The version is one byte in the files seen so far. Accept a few extra
    // header bytes in case another writer used a longer version field.
    let mut last_error: Option<anyhow::Error> = None;
    for skip in 0..=4usize.min(rest.len()) {
        let Ok(compressed) = base64_decode(&rest[skip..]) else {
            continue;
        };
        if compressed.len() < 2 {
            continue;
        }

        let mut decoder = ZlibDecoder::new(compressed.as_slice()).take(MAX_XML_BYTES + 1);
        let mut xml = Vec::new();
        match decoder.read_to_end(&mut xml) {
            Ok(_) if !xml.is_empty() && xml.len() as u64 <= MAX_XML_BYTES => {
                let text = decode(&xml, EncodingChoice::Auto);
                if text.trim_start().starts_with('<') {
                    return Ok(text);
                }
            }
            Ok(_) => {
                last_error = Some(anyhow::anyhow!(
                    "decompressed KMID lyric is empty or larger than {MAX_XML_BYTES} bytes"
                ));
            }
            Err(err) => last_error = Some(anyhow::Error::new(err)),
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no valid Base64/zlib KMID payload")))
}

/// Minimal standard-alphabet Base64 decoder (whitespace and `=` padding are
/// allowed). Keeping this local avoids another dependency for a 40-line job.
fn base64_decode(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;

    for &byte in input {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            byte if byte.is_ascii_whitespace() => continue,
            _ => bail!("invalid Base64 character 0x{byte:02x}"),
        } as u32;

        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1u32 << bits) - 1;
        }
    }

    Ok(out)
}

struct KmidData {
    title: Option<String>,
    artist: Option<String>,
    key: Option<String>,
    language: Option<String>,
    creator: Option<String>,
    year: Option<String>,
    time_format: String,
    /// One entry per line, each holding `(text, start_us)` pairs.
    lines: Vec<Vec<(String, u64)>>,
}

fn parse_xml(xml: &str, doc: &MidiDoc) -> Result<KmidData> {
    let document = Document::parse(xml).context("parsing KMID lyric XML")?;
    let root = document.root_element();
    if root.tag_name().name() != "SONG_LYRIC" {
        bail!("unexpected KMID XML root <{}>", root.tag_name().name());
    }

    let info = root.children().find(|node| node.has_tag_name("INFO"));
    let field = |tag: &str| -> Option<String> {
        info.and_then(|info| info.children().find(|node| node.has_tag_name(tag)))
            .and_then(|node| node.text())
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    };

    let time_format = field("TIME_FORMAT").unwrap_or_else(|| "MIDI_TIME_24".to_owned());
    // `MIDI_TIME_24` means 24 units per quarter note; the digits can differ.
    let units_per_quarter = time_format
        .strip_prefix("MIDI_TIME_")
        .and_then(|digits| digits.parse::<f64>().ok())
        .filter(|units| *units > 0.0)
        .unwrap_or(24.0);
    let scale = doc.ticks_per_quarter as f64 / units_per_quarter;

    let mut lines: Vec<Vec<(String, u64)>> = Vec::new();
    if let Some(lyric) = root.children().find(|node| node.has_tag_name("LYRIC")) {
        for line in lyric.children().filter(|node| node.has_tag_name("LINE")) {
            let mut words: Vec<(String, u64)> = Vec::new();

            for word in line.children().filter(|node| node.has_tag_name("WORD")) {
                let text = word
                    .children()
                    .find(|node| node.has_tag_name("TEXT"))
                    .and_then(|node| node.text())
                    .unwrap_or("");

                let time = word
                    .children()
                    .find(|node| node.has_tag_name("TIME"))
                    .and_then(|node| node.text())
                    .and_then(|text| text.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                let tick = (time as f64 * scale).round() as u64;
                let at_us = doc.tempo_map.tick_to_us(tick);

                if text.is_empty() || is_control_marker(text) {
                    continue;
                }

                // Two words at the same tick would give the first one a
                // zero-length highlight; merge them instead.
                if let Some(last) = words.last_mut() {
                    if last.1 == at_us {
                        last.0.push_str(text);
                        continue;
                    }
                }
                words.push((text.to_owned(), at_us));
            }

            if words.is_empty() {
                continue;
            }
            lines.push(words);
        }
    }

    Ok(KmidData {
        title: field("TITLE"),
        artist: field("ARTIST"),
        key: field("KEY"),
        language: field("LANGUAGE"),
        creator: field("CREATOR"),
        year: field("YEAR"),
        time_format,
        lines,
    })
}

fn build(doc: &MidiDoc, data: KmidData) -> Song {
    let lyric_lines = build_lines(data.lines, doc.duration_us);

    let mut info = vec!["KMID (embedded @LYRIC)".to_owned()];
    if let Some(creator) = data.creator {
        info.push(format!("creator: {creator}"));
    }
    if let Some(year) = data.year {
        info.push(format!("year: {year}"));
    }
    info.push(format!("time base: {}", data.time_format));

    let language = data
        .language
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("DEFAULT"));

    Song {
        format: SourceFormat::Kmid,
        title: data.title,
        artist: data.artist,
        key: data.key,
        language,
        info,
        tempo_map: doc.tempo_map.clone(),
        time_signatures: doc.time_signatures.clone(),
        ticks_per_quarter: doc.ticks_per_quarter,
        bpm: doc.bpm(),
        duration_us: doc.duration_us,
        track_count: doc.track_count,
        events: doc.events.clone(),
        lyric_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        // "hello KMID" with and without padding.
        assert_eq!(base64_decode(b"aGVsbG8gS01JRA==").unwrap(), b"hello KMID");
        assert_eq!(base64_decode(b"aGVsbG8gS01JRA").unwrap(), b"hello KMID");
        assert_eq!(base64_decode(b"aGVs\nbG8gS01JRA==").unwrap(), b"hello KMID");
        assert!(base64_decode(b"not base64!!").is_err());
    }
}
