use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::lyrics::{build_lines, is_control_marker};
use crate::midi::MidiDoc;
use crate::model::{Song, SourceFormat};
use crate::text::{EncodingChoice, decode};

/// Nick Karaoke stores one song as three files that live in parallel trees:
///
/// ```text
/// NCN/Song/1/00001.mid
/// NCN/Lyrics/1/00001.lyr
/// NCN/Cursor/1/00001.cur
/// ```
///
/// The cursor file holds one little-endian `u16` per *displayed character*,
/// terminated by `0xFF`. A value `v` maps to a MIDI tick via
/// `tick = v * (division / 24)`.
pub fn locate(mid: &Path, opts: &crate::LoadOptions) -> Option<(PathBuf, PathBuf)> {
    if let (Some(lyr), Some(cur)) = (&opts.lyrics, &opts.cursor) {
        if lyr.exists() && cur.exists() {
            return Some((lyr.clone(), cur.clone()));
        }
    }
    let lyr = find_part(mid, "Lyrics", "lyr", opts.lyrics.as_deref())?;
    let cur = find_part(mid, "Cursor", "cur", opts.cursor.as_deref())?;
    Some((lyr, cur))
}

fn find_part(mid: &Path, dir: &str, ext: &str, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return path.exists().then(|| path.to_path_buf());
    }

    let stem = mid.file_stem()?.to_string_lossy().to_string();

    // Same folder as the .mid (some collections keep all three together).
    let same = mid.with_extension(ext);
    if same.exists() {
        return Some(same);
    }

    // <ancestor>/<dir>/<subdir>/<stem>.<ext>  (the usual NCN layout)
    // <ancestor>/<dir>/<stem>.<ext>
    let subdir = mid.parent().and_then(|p| p.file_name());
    for ancestor in mid.ancestors().skip(1).take(4) {
        if let Some(subdir) = subdir {
            let candidate = ancestor
                .join(dir)
                .join(subdir)
                .join(format!("{stem}.{ext}"));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        let candidate = ancestor.join(dir).join(format!("{stem}.{ext}"));
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

pub fn build(
    mid_path: &Path,
    doc: MidiDoc,
    lyr_path: &Path,
    cur_path: &Path,
    enc: EncodingChoice,
) -> Result<Song> {
    let lyric_text = decode(&std::fs::read(lyr_path)?, enc);
    let cursor = read_cursor(&std::fs::read(cur_path)?);

    let mut lines = split_lines(&lyric_text);
    if lines.len() < 5 {
        bail!(
            "{}: expected title/artist/key/blank header, found {} lines",
            lyr_path.display(),
            lines.len()
        );
    }

    let raw_title = lines[0].trim().to_owned();
    let raw_key = lines[2].trim().to_owned();
    let key = (!raw_key.is_empty()).then(|| raw_key.clone());
    let title = clean_title(&raw_title, key.as_deref());
    let artist = {
        let value = lines[1].trim();
        (!value.is_empty()).then(|| value.to_owned())
    };

    let body: Vec<String> = lines
        .split_off(4)
        .into_iter()
        .map(|s| s.to_owned())
        .collect();

    // Scale factor between cursor units and MIDI ticks. The reference
    // implementation uses integer division, so 192 -> 8 and 96 -> 4.
    let scale = (doc.ticks_per_quarter as u64 / 24).max(1);

    let mut raw_lines: Vec<Vec<(String, u64)>> = vec![Vec::new(); body.len()];
    let last_value = cursor.last().copied().unwrap_or(0);
    let mut index = 0usize;

    // The reference converter builds the display stream as
    //   '/' + line1 + '/' + line2 + '/' + ...
    // so every line consumes one extra cursor value for its separator.
    index += 1; // leading separator

    for (line_index, line) in body.iter().enumerate() {
        // Only the three control markers are hidden; brand/credit lines stay.
        let skip = is_control_marker(line);
        for ch in line.chars() {
            let value = cursor.get(index).copied().unwrap_or(last_value);
            index += 1;
            let tick = value as u64 * scale;
            let at_us = doc.tempo_map.tick_to_us(tick);
            if !skip {
                raw_lines[line_index].push((ch.to_string(), at_us));
            }
        }
        index += 1; // separator after this line
    }

    let lyric_lines = build_lines(raw_lines, doc.duration_us);

    let info = vec![
        "NCN (Nick Karaoke) - .mid + .lyr + .cur".to_owned(),
        format!("midi: {}", mid_path.display()),
        format!("lyrics: {}", lyr_path.display()),
        format!("cursor: {}", cur_path.display()),
    ];

    let bpm = doc.bpm();
    let ticks_per_quarter = doc.ticks_per_quarter;
    let duration_us = doc.duration_us;
    let track_count = doc.track_count;

    Ok(Song {
        format: SourceFormat::Ncn,
        title: (!title.is_empty()).then_some(title),
        artist,
        key,
        language: None,
        info,
        tempo_map: doc.tempo_map,
        time_signatures: doc.time_signatures,
        ticks_per_quarter,
        bpm,
        duration_us,
        track_count,
        events: doc.events,
        lyric_lines,
    })
}

/// `\r\n`, `\n` and lone `\r` all become line breaks. A trailing newline at
/// the end of the file does not create an extra empty line.
fn split_lines(text: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&text[start..i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                out.push(&text[start..i]);
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 1;
                }
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        out.push(&text[start..]);
    } else if out.last() == Some(&"") {
        // File ended with a newline: drop the artificial empty tail.
        out.pop();
    }
    out
}

fn read_cursor(bytes: &[u8]) -> Vec<u16> {
    let mut values = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        let value = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        i += 2;
        if value == 0xFFFF {
            break;
        }
        values.push(value);
    }
    values
}

/// NCN titles often carry the key in parentheses: `Song Title ( G )`.
fn clean_title(title: &str, key: Option<&str>) -> String {
    if let Some(key) = key {
        if let Some(open) = title.rfind('(') {
            if title.ends_with(')') {
                let inside: String = title[open + 1..title.len() - 1]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                let wanted: String = key.chars().filter(|c| !c.is_whitespace()).collect();
                if inside == wanted {
                    return title[..open].trim().to_owned();
                }
            }
        }
    }
    title.to_owned()
}
