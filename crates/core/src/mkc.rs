//! MKC (MIDI Karaoke Container) version 1.
//!
//! An MKC file is a normal ZIP archive with a small, documented layout:
//!
//! ```text
//! mimetype      application/mkc+zip   (stored, first entry)
//! manifest.json UTF-8 JSON metadata
//! lyrics.json   UTF-8 timed lyrics
//! lyrics.txt    UTF-8 plain lyrics (for simple readers)
//! song.mid      the original Standard MIDI File
//! ```
//!
//! Compression is regular ZIP deflate. There is no XOR or encryption, so the
//! archive can be inspected with any ZIP tool.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::{Compression, Crc};
use serde::{Deserialize, Serialize};

use crate::model::{LyricLine, Syllable};
use crate::{LoadOptions, Song, SourceFormat, load};

pub const MKC_FORMAT: &str = "MKC";
pub const MKC_VERSION: u32 = 1;
pub const MKC_MIME: &str = "application/mkc+zip";
pub const MKC_EXTENSION: &str = "mkc";
pub const MANIFEST_ENTRY: &str = "manifest.json";
pub const MIDI_ENTRY: &str = "song.mid";
pub const LYRICS_ENTRY: &str = "lyrics.json";
pub const LYRICS_TEXT_ENTRY: &str = "lyrics.txt";

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_CENTRAL: u32 = 0x0605_4b50;

/// Metadata stored in `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkcManifest {
    pub format: String,
    pub version: u32,
    pub code: String,
    pub title: String,
    pub artist: String,
    pub key: Option<String>,
    pub language: Option<String>,
    pub tempo: f64,
    pub duration_ms: u64,
    pub ticks_per_quarter: u16,
    pub track_count: usize,
    pub vocal_channel: Option<u8>,
    pub created_by: String,
    pub sections: MkcSections,
    pub source: MkcSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkcSections {
    pub midi: String,
    pub lyrics: String,
    pub lyrics_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkcSource {
    pub format: String,
    pub midi_file: String,
    pub lyric_file: String,
    pub cursor_file: String,
    pub lyric_encoding: String,
}

/// Timed lyrics stored in `lyrics.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkcLyrics {
    pub format: String,
    pub version: u32,
    pub time_unit: String,
    pub lines: Vec<MkcLyricLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkcLyricLine {
    pub start_us: u64,
    pub end_us: u64,
    pub text: String,
    pub syllables: Vec<MkcSyllable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkcSyllable {
    pub start_us: u64,
    pub end_us: u64,
    pub text: String,
}

/// Result of packing one song.
#[derive(Debug, Clone)]
pub struct PackResult {
    pub output: PathBuf,
    pub bytes: u64,
    pub manifest: MkcManifest,
    pub lyric_lines: usize,
}

/// Optional changes applied while replacing the MIDI payload.
#[derive(Debug, Clone, Default)]
pub struct MkcEdit {
    pub code: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub key: Option<String>,
    pub tempo: Option<f64>,
    /// Scale lyric timestamps with the tempo change (old BPM / new BPM).
    pub time_scale: f64,
}

/// Replace the `song.mid` payload and optional manifest metadata.
///
/// This is used by the MIDI editor when the opened song is an MKC package. All
/// other entries (lyrics, unknown future sections) are preserved.
pub fn rewrite_midi(
    source: &Path,
    target: &Path,
    midi: &[u8],
    edit: Option<&MkcEdit>,
    overwrite: bool,
) -> Result<()> {
    if target.exists() && target != source && !overwrite {
        bail!("{} already exists", target.display());
    }
    let package = read_package(source)?;
    let mut manifest = package.manifest()?;
    let mut entries = package.entries.clone();

    if let Some(edit) = edit {
        if let Some(code) = &edit.code {
            manifest.code = code.clone();
        }
        if let Some(title) = &edit.title {
            manifest.title = title.clone();
        }
        if let Some(artist) = &edit.artist {
            manifest.artist = artist.clone();
        }
        if let Some(key) = &edit.key {
            manifest.key = Some(key.clone());
        }
        if let Some(tempo) = edit.tempo {
            manifest.tempo = tempo;
        }
        if edit.time_scale > 0.0 && (edit.time_scale - 1.0).abs() > 0.000_001 {
            manifest.duration_ms = (manifest.duration_ms as f64 * edit.time_scale).round() as u64;
            if let Some(bytes) = entries.get_mut(LYRICS_ENTRY)
                && let Ok(mut lyrics) = serde_json::from_slice::<MkcLyrics>(bytes)
            {
                for line in &mut lyrics.lines {
                    line.start_us = (line.start_us as f64 * edit.time_scale).round() as u64;
                    line.end_us = (line.end_us as f64 * edit.time_scale).round() as u64;
                    for syllable in &mut line.syllables {
                        syllable.start_us =
                            (syllable.start_us as f64 * edit.time_scale).round() as u64;
                        syllable.end_us = (syllable.end_us as f64 * edit.time_scale).round() as u64;
                    }
                }
                *bytes = serde_json::to_vec_pretty(&lyrics)?;
            }
        }
        if entries.contains_key(LYRICS_TEXT_ENTRY)
            && let Ok(lyrics) = package.lyrics()
        {
            let mut text = format!(
                "{}\n{}\n{}\n\n",
                manifest.title,
                manifest.artist,
                manifest.key.as_deref().unwrap_or("")
            );
            for line in &lyrics.lines {
                text.push_str(&line.text);
                text.push('\n');
            }
            entries.insert(LYRICS_TEXT_ENTRY.to_owned(), text.into_bytes());
        }
    }

    entries.insert(MIDI_ENTRY.to_owned(), midi.to_vec());
    entries.insert(
        MANIFEST_ENTRY.to_owned(),
        serde_json::to_vec_pretty(&manifest)?,
    );
    let entries = ordered_entries(entries);
    let bytes = write_zip(&entries)?;
    write_atomic(target, &bytes)
}

fn ordered_entries(mut entries: BTreeMap<String, Vec<u8>>) -> Vec<Entry> {
    let mut ordered = Vec::new();
    if let Some(bytes) = entries.remove("mimetype") {
        ordered.push(Entry::stored("mimetype", bytes));
    } else {
        ordered.push(Entry::stored("mimetype", MKC_MIME.as_bytes().to_vec()));
    }
    for name in [MANIFEST_ENTRY, LYRICS_ENTRY, LYRICS_TEXT_ENTRY, MIDI_ENTRY] {
        if let Some(bytes) = entries.remove(name) {
            ordered.push(Entry::deflated(name, bytes));
        }
    }
    for (name, bytes) in entries {
        ordered.push(Entry::deflated(&name, bytes));
    }
    ordered
}

/// Load an MKC package into the normalised [`Song`] model.
pub fn load_song(path: &Path, options: &LoadOptions) -> Result<Song> {
    let package = read_package(path)?;
    let manifest = package.manifest()?;
    let midi = package.midi()?;
    let doc = crate::midi::parse(midi, options.encoding)
        .with_context(|| format!("parsing song.mid in {}", path.display()))?;
    let mut song = crate::kar::from_doc(doc, options.encoding);
    song.format = SourceFormat::Mkc;
    if !manifest.title.trim().is_empty() {
        song.title = Some(manifest.title.clone());
    }
    if !manifest.artist.trim().is_empty() {
        song.artist = Some(manifest.artist.clone());
    }
    if let Some(key) = manifest.key.clone().filter(|key| !key.trim().is_empty()) {
        song.key = Some(key);
    }
    song.language = manifest
        .language
        .as_deref()
        .and_then(crate::script::canonical_language)
        .map(str::to_owned)
        .or_else(|| {
            let detected = crate::script::detect_song_language(
                song.lyric_lines.iter().map(|line| line.text.as_str()),
                song.title.as_deref().unwrap_or(""),
            );
            Some(detected.to_owned())
        });
    song.vocal_channel = manifest.vocal_channel;
    song.duration_us = song
        .duration_us
        .max(manifest.duration_ms.saturating_mul(1_000));
    if let Ok(lyrics) = package.lyrics()
        && !lyrics.lines.is_empty()
    {
        song.lyric_lines = lyrics
            .lines
            .into_iter()
            .map(|line| LyricLine {
                start_us: line.start_us,
                end_us: line.end_us.max(line.start_us),
                text: line.text,
                syllables: line
                    .syllables
                    .into_iter()
                    .map(|syllable| Syllable {
                        start_us: syllable.start_us,
                        end_us: syllable.end_us.max(syllable.start_us),
                        text: syllable.text,
                    })
                    .collect(),
            })
            .collect();
    }
    song.info = vec![
        "MKC (MIDI Karaoke Container)".to_owned(),
        format!("file: {}", path.display()),
        format!("source: {}", manifest.source.format),
        format!("code: {}", manifest.code),
    ];
    Ok(song)
}

/// Top-level folder used for converted files.
pub const MKC_PREFIX: &str = "MKC";

/// Prefix folder for each supported source format.
pub fn format_prefix(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Ncn => "NCN",
        SourceFormat::Kmid => "KMID",
        SourceFormat::Kar => "KAR",
        SourceFormat::Midi => "MIDI",
        SourceFormat::Mkc => MKC_PREFIX,
    }
}

/// Lower-case format name used in the manifest.
pub fn format_name(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Ncn => "ncn",
        SourceFormat::Kmid => "kmid",
        SourceFormat::Kar => "kar",
        SourceFormat::Midi => "midi",
        SourceFormat::Mkc => "mkc",
    }
}

/// Find the format prefix folder (`NCN`, `KMID`, `KAR`, `MIDI`, `MKC`) that
/// owns a path, searching from the path upwards.
pub fn format_root(path: &Path) -> Option<(PathBuf, SourceFormat)> {
    for ancestor in path.ancestors() {
        let Some(name) = ancestor.file_name() else {
            continue;
        };
        let name = name.to_string_lossy().to_ascii_uppercase();
        let format = match name.as_str() {
            "NCN" => SourceFormat::Ncn,
            "KMID" => SourceFormat::Kmid,
            "KAR" => SourceFormat::Kar,
            "MIDI" => SourceFormat::Midi,
            "MKC" => SourceFormat::Mkc,
            _ => continue,
        };
        return Some((ancestor.to_path_buf(), format));
    }
    None
}

/// Default base folder where the `MKC` prefix folder should be created.
pub fn default_output_root(source: &Path) -> PathBuf {
    if let Some((root, _)) = format_root(source) {
        return root.parent().unwrap_or(&root).to_path_buf();
    }
    if source.is_dir() {
        source.to_path_buf()
    } else {
        source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    }
}

/// Destination of one source song under a base folder.
pub fn output_path(source: &Path, output_root: &Path) -> PathBuf {
    let relative = if let Some((root, format)) = format_root(source) {
        let relative = source
            .strip_prefix(&root)
            .unwrap_or_else(|_| Path::new(source.file_name().unwrap_or_default()));
        if format == SourceFormat::Ncn {
            strip_song_component(relative)
        } else {
            relative.to_path_buf()
        }
    } else {
        PathBuf::from(source.file_name().unwrap_or_default())
    };
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let stem = relative
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "song".to_owned());
    output_root
        .join(MKC_PREFIX)
        .join(parent)
        .join(format!("{stem}.{MKC_EXTENSION}"))
}

fn strip_song_component(relative: &Path) -> PathBuf {
    let mut components = relative.components();
    match components.next() {
        Some(component)
            if component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("song") =>
        {
            components.collect()
        }
        _ => relative.to_path_buf(),
    }
}

/// Find karaoke songs under a file or folder.
pub fn find_karaoke_songs(path: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
    find_karaoke_songs_with_report(path, recursive, &mut |_| {})
}

/// Find karaoke songs, reporting how many have been found so far.
///
/// The report runs during the walk, which lets a GUI show progress while a
/// large library is being scanned.
pub fn find_karaoke_songs_with_report(
    path: &Path,
    recursive: bool,
    report: &mut dyn FnMut(usize),
) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        // A file the user picked explicitly gets the full check, so a MIDI
        // whose lyrics live in plain text events still converts. Folder walks
        // use the cheap sniff below instead of parsing every song.
        let is_song = load(path, &LoadOptions::default())
            .map(|song| song.format != SourceFormat::Midi || !song.lyric_lines.is_empty())
            .unwrap_or(false);
        if !is_song {
            bail!("{} is not a karaoke song", path.display());
        }
        report(1);
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        bail!("{} does not exist", path.display());
    }
    let mut songs = Vec::new();
    collect_karaoke_songs(path, recursive, &mut songs, report)?;
    songs.sort();
    songs.dedup();
    if songs.is_empty() {
        bail!("no karaoke songs found in {}", path.display());
    }
    Ok(songs)
}

fn collect_karaoke_songs(
    folder: &Path,
    recursive: bool,
    songs: &mut Vec<PathBuf>,
    report: &mut dyn FnMut(usize),
) -> Result<()> {
    for entry in
        std::fs::read_dir(folder).with_context(|| format!("reading folder {}", folder.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            if recursive {
                collect_karaoke_songs(&path, recursive, songs, report)?;
            }
        } else if is_karaoke_file(&path) {
            songs.push(path);
            report(songs.len());
        }
    }
    Ok(())
}

/// Whether one file should be converted.
///
/// This must stay cheap: a library folder can hold thousands of songs, and
/// parsing every MIDI file just to list it is what made the old scan freeze
/// the converter for minutes. NCN companions are found with a few stat calls;
/// otherwise the bytes are sniffed for the KMID magic or a MIDI lyric event.
fn is_karaoke_file(path: &Path) -> bool {
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "kar" => true,
        "mid" | "midi" => {
            if crate::ncn::locate(path, &LoadOptions::default()).is_some() {
                return true;
            }
            has_embedded_lyrics(path)
        }
        _ => false,
    }
}

/// Sniff for the KMID `LyrHdr` payload or a MIDI `Lyric` (0xFF 0x05) event.
///
/// A rare false positive is harmless: the file is still a valid MIDI song and
/// packs with an empty lyric list.
fn has_embedded_lyrics(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    const MAGIC: &[u8] = b"LyrHdr";
    if bytes.windows(MAGIC.len()).any(|window| window == MAGIC) {
        return true;
    }
    bytes
        .windows(2)
        .any(|pair| pair[0] == 0xFF && pair[1] == 0x05)
}

/// Convert any supported karaoke file to an `.mkc` file.
pub fn pack_song(source: &Path, output: &Path, overwrite: bool) -> Result<PackResult> {
    pack_song_with(source, output, overwrite, &LoadOptions::default())
}

/// Convert a song using explicit [`LoadOptions`].
///
/// This is how the CLI asks for a legacy text encoding (Shift-JIS, GB18030,
/// EUC-KR, Windows-1258, ...) when the lyrics are not UTF-8.
pub fn pack_song_with(
    source: &Path,
    output: &Path,
    overwrite: bool,
    options: &LoadOptions,
) -> Result<PackResult> {
    pack_song_inner(source, output, overwrite, false, options)
}

/// Convert an NCN `.mid` (plus its `.lyr`/`.cur` companions) to an `.mkc` file.
pub fn pack_ncn(midi_path: &Path, output: &Path, overwrite: bool) -> Result<PackResult> {
    pack_song_inner(midi_path, output, overwrite, true, &LoadOptions::default())
}

fn pack_song_inner(
    source: &Path,
    output: &Path,
    overwrite: bool,
    require_ncn: bool,
    options: &LoadOptions,
) -> Result<PackResult> {
    if output.exists() && !overwrite {
        bail!("{} already exists", output.display());
    }
    let song = load(source, options)
        .with_context(|| format!("reading karaoke song {}", source.display()))?;
    if require_ncn && song.format != SourceFormat::Ncn {
        bail!("{} is not an NCN song", source.display());
    }
    let (lyric_file, cursor_file, lyric_encoding) = if song.format == SourceFormat::Ncn {
        let (lyric_path, cursor_path) = crate::ncn::locate(source, options).ok_or_else(|| {
            anyhow::anyhow!(
                "could not find the .lyr/.cur companions of {}",
                source.display()
            )
        })?;
        let encoding = if std::fs::read(&lyric_path)
            .ok()
            .is_some_and(|bytes| std::str::from_utf8(&bytes).is_ok())
        {
            "utf-8"
        } else {
            "windows-874"
        };
        (
            file_name(&lyric_path),
            file_name(&cursor_path),
            encoding.to_owned(),
        )
    } else if song.lyric_lines.is_empty() {
        (String::new(), String::new(), "none".to_owned())
    } else {
        (String::new(), String::new(), "embedded".to_owned())
    };
    let midi_bytes =
        std::fs::read(source).with_context(|| format!("reading {}", source.display()))?;
    let code = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = song.title.clone().unwrap_or_default();
    let artist = song.artist.clone().unwrap_or_default();
    // MKC is UTF-8, so it can describe songs in any of the bundled languages.
    // Record which one this is so readers can pick the right font pack
    // without scanning the lyric text.
    let language = song
        .language
        .as_deref()
        .and_then(crate::script::canonical_language)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            crate::script::detect_song_language(
                song.lyric_lines.iter().map(|line| line.text.as_str()),
                &title,
            )
            .to_owned()
        });
    let lyrics = build_lyrics(&song);
    let manifest = MkcManifest {
        format: MKC_FORMAT.to_owned(),
        version: MKC_VERSION,
        code,
        title,
        artist,
        key: song.key.clone(),
        language: Some(language),
        tempo: song.bpm,
        duration_ms: (song.duration_us + 500) / 1_000,
        ticks_per_quarter: song.ticks_per_quarter,
        track_count: song.track_count,
        vocal_channel: song.vocal_channel,
        created_by: format!("DontreeCrab MKC {}", env!("CARGO_PKG_VERSION")),
        sections: MkcSections {
            midi: MIDI_ENTRY.to_owned(),
            lyrics: LYRICS_ENTRY.to_owned(),
            lyrics_text: LYRICS_TEXT_ENTRY.to_owned(),
        },
        source: MkcSource {
            format: format_name(song.format).to_owned(),
            midi_file: file_name(source),
            lyric_file,
            cursor_file,
            lyric_encoding,
        },
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    let lyrics_json = serde_json::to_vec_pretty(&lyrics)?;
    let lyrics_text = plain_lyrics(&song);
    let lyric_lines = lyrics.lines.len();

    let entries = vec![
        Entry::stored("mimetype", MKC_MIME.as_bytes().to_vec()),
        Entry::deflated(MANIFEST_ENTRY, manifest_json),
        Entry::deflated(LYRICS_ENTRY, lyrics_json),
        Entry::deflated(LYRICS_TEXT_ENTRY, lyrics_text.into_bytes()),
        Entry::deflated(MIDI_ENTRY, midi_bytes),
    ];
    let bytes = write_zip(&entries)?;
    write_atomic(output, &bytes)?;
    Ok(PackResult {
        output: output.to_path_buf(),
        bytes: bytes.len() as u64,
        manifest,
        lyric_lines,
    })
}

fn build_lyrics(song: &crate::Song) -> MkcLyrics {
    let lines = song
        .lyric_lines
        .iter()
        .map(|line| MkcLyricLine {
            start_us: line.start_us,
            end_us: line.end_us,
            text: line.text.clone(),
            syllables: line
                .syllables
                .iter()
                .map(|syllable| MkcSyllable {
                    start_us: syllable.start_us,
                    end_us: syllable.end_us,
                    text: syllable.text.clone(),
                })
                .collect(),
        })
        .collect();
    MkcLyrics {
        format: "mkc-lyrics".to_owned(),
        version: MKC_VERSION,
        time_unit: "us".to_owned(),
        lines,
    }
}

fn plain_lyrics(song: &crate::Song) -> String {
    plain_lyrics_from_parts(
        song.title.as_deref().unwrap_or(""),
        song.artist.as_deref().unwrap_or(""),
        song.key.as_deref().unwrap_or(""),
        &song.lyric_lines,
    )
}

fn plain_lyrics_from_parts(
    title: &str,
    artist: &str,
    key: &str,
    lines: &[crate::model::LyricLine],
) -> String {
    let mut out = String::new();
    out.push_str(title);
    out.push('\n');
    out.push_str(artist);
    out.push('\n');
    out.push_str(key);
    out.push_str("\n\n");
    for line in lines {
        out.push_str(&line.text);
        out.push('\n');
    }
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "song.mkc".to_owned());
    let temp = path.with_file_name(format!(".{file_name}.tmp"));
    std::fs::write(&temp, bytes).with_context(|| format!("writing {}", temp.display()))?;
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("replacing {}", path.display()))?;
    }
    std::fs::rename(&temp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

struct Entry {
    name: String,
    data: Vec<u8>,
    store: bool,
}

impl Entry {
    fn stored(name: &str, data: Vec<u8>) -> Self {
        Self {
            name: name.to_owned(),
            data,
            store: true,
        }
    }

    fn deflated(name: &str, data: Vec<u8>) -> Self {
        Self {
            name: name.to_owned(),
            data,
            store: false,
        }
    }
}

struct ZipEntry {
    name: String,
    data: Vec<u8>,
    crc: u32,
    method: u16,
    offset: u32,
}

/// Minimal ZIP writer used by MKC.
fn write_zip(entries: &[Entry]) -> Result<Vec<u8>> {
    if entries.len() > u16::MAX as usize {
        bail!("too many MKC entries");
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut records = Vec::with_capacity(entries.len());

    for entry in entries {
        if entry.name.len() > u16::MAX as usize {
            bail!("MKC entry name is too long");
        }
        let mut crc = Crc::new();
        crc.update(&entry.data);
        let crc = crc.sum();
        let (method, compressed) = if entry.store {
            (0u16, entry.data.clone())
        } else {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&entry.data)?;
            let compressed = encoder.finish()?;
            if compressed.len() < entry.data.len() {
                (8u16, compressed)
            } else {
                (0u16, entry.data.clone())
            }
        };
        if entry.data.len() > u32::MAX as usize || compressed.len() > u32::MAX as usize {
            bail!("MKC entry is too large for the ZIP32 format");
        }
        let offset = out.len() as u32;
        let name = entry.name.as_bytes();
        push_u32(&mut out, LOCAL_HEADER);
        push_u16(&mut out, 20);
        push_u16(&mut out, 0);
        push_u16(&mut out, method);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0x0021);
        push_u32(&mut out, crc);
        push_u32(&mut out, compressed.len() as u32);
        push_u32(&mut out, entry.data.len() as u32);
        push_u16(&mut out, name.len() as u16);
        push_u16(&mut out, 0);
        out.extend_from_slice(name);
        out.extend_from_slice(&compressed);
        records.push(ZipEntry {
            name: entry.name.clone(),
            data: entry.data.clone(),
            crc,
            method,
            offset,
        });
    }

    let central_offset = out.len() as u32;
    for record in &records {
        let name = record.name.as_bytes();
        let (compressed_size, uncompressed_size, method) = if record.method == 0 {
            (record.data.len(), record.data.len(), 0u16)
        } else {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&record.data)?;
            let compressed = encoder.finish()?;
            (compressed.len(), record.data.len(), 8u16)
        };
        if compressed_size > u32::MAX as usize || uncompressed_size > u32::MAX as usize {
            bail!("MKC entry is too large for the ZIP32 format");
        }
        push_u32(&mut central, CENTRAL_HEADER);
        push_u16(&mut central, 20);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0);
        push_u16(&mut central, method);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0x0021);
        push_u32(&mut central, record.crc);
        push_u32(&mut central, compressed_size as u32);
        push_u32(&mut central, uncompressed_size as u32);
        push_u16(&mut central, name.len() as u16);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, 0);
        push_u32(&mut central, record.offset);
        central.extend_from_slice(name);
    }
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    push_u32(&mut out, END_OF_CENTRAL);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, records.len() as u16);
    push_u16(&mut out, records.len() as u16);
    push_u32(&mut out, central_size);
    push_u32(&mut out, central_offset);
    push_u16(&mut out, 0);
    Ok(out)
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Entries read from an MKC/ZIP package.
#[derive(Debug, Clone, Default)]
pub struct MkcPackage {
    pub entries: BTreeMap<String, Vec<u8>>,
}

impl MkcPackage {
    pub fn manifest(&self) -> Result<MkcManifest> {
        let bytes = self
            .entries
            .get(MANIFEST_ENTRY)
            .ok_or_else(|| anyhow::anyhow!("MKC is missing {MANIFEST_ENTRY}"))?;
        let manifest: MkcManifest = serde_json::from_slice(bytes)?;
        if manifest.format != MKC_FORMAT {
            bail!("not an MKC manifest");
        }
        if manifest.version > MKC_VERSION {
            bail!("MKC version {} is newer than this reader", manifest.version);
        }
        Ok(manifest)
    }

    pub fn midi(&self) -> Result<&[u8]> {
        let name = self.manifest()?.sections.midi;
        self.entries
            .get(&name)
            .map(Vec::as_slice)
            .ok_or_else(|| anyhow::anyhow!("MKC is missing {name}"))
    }

    pub fn lyrics(&self) -> Result<MkcLyrics> {
        let name = self.manifest()?.sections.lyrics;
        let bytes = self
            .entries
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("MKC is missing {name}"))?;
        Ok(serde_json::from_slice(bytes)?)
    }
}

/// Read an MKC file, verifying each entry's CRC.
pub fn read_package(path: &Path) -> Result<MkcPackage> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    read_package_from_bytes(&bytes)
}

pub fn read_package_from_bytes(bytes: &[u8]) -> Result<MkcPackage> {
    let mut package = MkcPackage::default();
    let mut pos = 0usize;
    while pos + 4 <= bytes.len() {
        let signature = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
        if signature != LOCAL_HEADER {
            break;
        }
        if pos + 30 > bytes.len() {
            bail!("truncated ZIP local header");
        }
        let method = u16::from_le_bytes(bytes[pos + 8..pos + 10].try_into().unwrap());
        let crc = u32::from_le_bytes(bytes[pos + 14..pos + 18].try_into().unwrap());
        let compressed_size =
            u32::from_le_bytes(bytes[pos + 18..pos + 22].try_into().unwrap()) as usize;
        let uncompressed_size =
            u32::from_le_bytes(bytes[pos + 22..pos + 26].try_into().unwrap()) as usize;
        let name_len = u16::from_le_bytes(bytes[pos + 26..pos + 28].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(bytes[pos + 28..pos + 30].try_into().unwrap()) as usize;
        let name_start = pos + 30;
        let name_end = name_start + name_len;
        let data_start = name_end + extra_len;
        let data_end = data_start + compressed_size;
        if data_end > bytes.len() {
            bail!("truncated ZIP entry data");
        }
        let name = String::from_utf8(bytes[name_start..name_end].to_vec())?;
        let compressed = &bytes[data_start..data_end];
        let data = match method {
            0 => compressed.to_vec(),
            8 => {
                let mut decoder = DeflateDecoder::new(compressed);
                let mut out = Vec::with_capacity(uncompressed_size);
                decoder.read_to_end(&mut out)?;
                out
            }
            other => bail!("unsupported ZIP compression method {other}"),
        };
        if data.len() != uncompressed_size {
            bail!("MKC entry {name} has the wrong uncompressed size");
        }
        let mut check = Crc::new();
        check.update(&data);
        if check.sum() != crc {
            bail!("MKC entry {name} failed its CRC check");
        }
        package.entries.insert(name, data);
        pos = data_end;
    }
    if package.entries.is_empty() {
        bail!("not an MKC/ZIP file");
    }
    if package.entries.get("mimetype").map(Vec::as_slice) != Some(MKC_MIME.as_bytes()) {
        bail!("not an MKC package (bad mimetype)");
    }
    Ok(package)
}

/// Extract the package's files into `directory`.
pub fn unpack_to(path: &Path, directory: &Path) -> Result<Vec<PathBuf>> {
    let package = read_package(path)?;
    std::fs::create_dir_all(directory)?;
    let mut written = Vec::new();
    for (name, data) in &package.entries {
        let relative = Path::new(name);
        if relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            bail!("unsafe MKC entry name: {name}");
        }
        let target = directory.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, data)?;
        written.push(target);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanning_skips_plain_midi_and_reports_progress() {
        let root = std::env::temp_dir().join(format!(
            "dontreecrab-mkc-scan-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ncn = root.join("NCN");
        for folder in ["Song/S", "Lyrics/S", "Cursor/S"] {
            std::fs::create_dir_all(ncn.join(folder)).unwrap();
        }
        std::fs::write(ncn.join("Song/S/SONG0001.mid"), b"MThd").unwrap();
        std::fs::write(ncn.join("Lyrics/S/SONG0001.lyr"), b"x").unwrap();
        std::fs::write(ncn.join("Cursor/S/SONG0001.cur"), b"x").unwrap();

        // A plain MIDI file without lyric events must not be converted.
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(
            plain.join("instrumental.mid"),
            b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x00\x78",
        )
        .unwrap();

        // A KMID payload is found by sniffing, without parsing the MIDI.
        let kmid = root.join("kmid");
        std::fs::create_dir_all(&kmid).unwrap();
        std::fs::write(kmid.join("embedded.mid"), b"MThdLyrHdr1payload").unwrap();

        // KAR files are karaoke by extension.
        let kar = root.join("kar");
        std::fs::create_dir_all(&kar).unwrap();
        std::fs::write(kar.join("one.kar"), b"MThd").unwrap();

        let mut counts: Vec<usize> = Vec::new();
        let songs = find_karaoke_songs_with_report(&root, true, &mut |found| counts.push(found))
            .expect("scan");
        assert_eq!(songs.len(), 3, "{songs:?}");
        assert!(songs.iter().any(|path| path.ends_with("SONG0001.mid")));
        assert!(songs.iter().any(|path| path.ends_with("embedded.mid")));
        assert!(songs.iter().any(|path| path.ends_with("one.kar")));
        assert!(!songs.iter().any(|path| path.ends_with("instrumental.mid")));
        // The report only ever grows and ends at the final count.
        assert!(counts.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(counts.last().copied(), Some(3));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scanning_a_large_ncn_library_stays_fast() {
        let root = std::env::temp_dir().join(format!(
            "dontreecrab-mkc-scan-many-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for folder in ["Song", "Lyrics", "Cursor"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        // 500 NCN trios: the old filter parsed every MIDI file here.
        for index in 0..500 {
            let code = format!("{index:05}");
            std::fs::write(root.join("Song").join(format!("{code}.mid")), b"MThd").unwrap();
            std::fs::write(root.join("Lyrics").join(format!("{code}.lyr")), b"x").unwrap();
            std::fs::write(root.join("Cursor").join(format!("{code}.cur")), b"x").unwrap();
        }
        let started = std::time::Instant::now();
        let songs = find_karaoke_songs(&root, true).expect("scan");
        assert_eq!(songs.len(), 500);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "scan took {:?}",
            started.elapsed()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_selected_file_with_text_lyrics_still_converts() {
        let root = std::env::temp_dir().join(format!(
            "dontreecrab-mkc-text-lyrics-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("text.mid");
        // One Text event ("Hello") instead of a Lyric event.
        let track: &[u8] = &[
            0x00, 0xFF, 0x01, 0x05, b'H', b'e', b'l', b'l', b'o', 0x00, 0xFF, 0x2F, 0x00,
        ];
        let mut bytes = b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x01\xE0".to_vec();
        bytes.extend_from_slice(b"MTrk");
        bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
        bytes.extend_from_slice(track);
        std::fs::write(&file, &bytes).unwrap();

        let songs = find_karaoke_songs(&file, false).expect("single file");
        assert_eq!(songs, vec![file.clone()]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn zip_round_trips_entries() {
        let bytes = write_zip(&[
            Entry::stored("mimetype", MKC_MIME.as_bytes().to_vec()),
            Entry::deflated("manifest.json", b"{\"format\":\"MKC\"}".to_vec()),
            Entry::deflated("song.mid", vec![0x4d, 0x54, 0x68, 0x64, 0, 0, 0, 6]),
        ])
        .unwrap();
        let package = read_package_from_bytes(&bytes).unwrap();
        assert_eq!(
            package.entries.get("mimetype").map(Vec::as_slice),
            Some(MKC_MIME.as_bytes())
        );
        assert_eq!(
            package.entries.get("manifest.json").map(Vec::as_slice),
            Some(b"{\"format\":\"MKC\"}".as_slice())
        );
        assert_eq!(
            package.entries.get("song.mid").map(Vec::as_slice),
            Some([0x4d, 0x54, 0x68, 0x64, 0, 0, 0, 6].as_slice())
        );
    }

    #[test]
    fn outputs_follow_the_mkc_prefix() {
        let base = Path::new("D:/Songs");
        assert_eq!(
            output_path(Path::new("D:/Songs/NCN/Song/S/SONG0001.mid"), base),
            base.join("MKC").join("S").join("SONG0001.mkc")
        );
        assert_eq!(
            output_path(Path::new("D:/Songs/NCN/S/SONG0001.mid"), base),
            base.join("MKC").join("S").join("SONG0001.mkc")
        );
        assert_eq!(
            output_path(Path::new("D:/Songs/KMID/S/SONG0002.mid"), base),
            base.join("MKC").join("S").join("SONG0002.mkc")
        );
        assert_eq!(
            output_path(Path::new("D:/Songs/KAR/S/SONG0003.kar"), base),
            base.join("MKC").join("S").join("SONG0003.mkc")
        );
        assert_eq!(
            default_output_root(Path::new("D:/Songs/NCN/Song/S/SONG0001.mid")),
            PathBuf::from("D:/Songs")
        );
    }

    #[test]
    fn packs_a_synthetic_ncn_song() {
        let root = std::env::temp_dir().join(format!(
            "dontreecrab-mkc-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let midi = root.join("1001.mid");
        let mut midi_bytes = b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x00\x78".to_vec();
        let track: &[u8] = &[
            0x00, 0xff, 0x51, 0x03, 0x06, 0x1a, 0x80, 0x00, 0xff, 0x2f, 0x00,
        ];
        midi_bytes.extend_from_slice(b"MTrk");
        midi_bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
        midi_bytes.extend_from_slice(track);
        std::fs::write(&midi, &midi_bytes).unwrap();

        let lyr = root.join("1001.lyr");
        std::fs::write(&lyr, b"Test Song\r\nTest Artist\r\nC\r\n\r\nHello\r\n").unwrap();
        let cur = root.join("1001.cur");
        let mut cur_bytes = Vec::new();
        for value in [0u16, 10, 20, 30, 40, 50, 60] {
            cur_bytes.extend_from_slice(&value.to_le_bytes());
        }
        cur_bytes.extend_from_slice(&0xffffu16.to_le_bytes());
        std::fs::write(&cur, cur_bytes).unwrap();

        let output = root.join("1001.mkc");
        let result = pack_ncn(&midi, &output, true).unwrap();
        assert_eq!(result.manifest.title, "Test Song");
        assert_eq!(result.manifest.artist, "Test Artist");
        assert_eq!(result.manifest.key.as_deref(), Some("C"));
        // English lyrics are detected and recorded for the font fallback.
        assert_eq!(result.manifest.language.as_deref(), Some("en"));
        assert!(result.bytes > 0);

        let package = read_package(&output).unwrap();
        assert_eq!(
            package.entries.get("mimetype").map(Vec::as_slice),
            Some(MKC_MIME.as_bytes())
        );
        assert_eq!(package.midi().unwrap(), midi_bytes.as_slice());
        let lyrics = package.lyrics().unwrap();
        assert_eq!(lyrics.format, "mkc-lyrics");
        assert!(lyrics.lines.iter().any(|line| line.text == "Hello"));

        let loaded = load_song(&output, &LoadOptions::default()).unwrap();
        assert_eq!(loaded.format, SourceFormat::Mkc);
        assert_eq!(loaded.title.as_deref(), Some("Test Song"));
        assert_eq!(loaded.artist.as_deref(), Some("Test Artist"));
        assert_eq!(loaded.key.as_deref(), Some("C"));
        assert!(loaded.lyric_lines.iter().any(|line| line.text == "Hello"));
        let record = crate::library::record_from_song(&output, &loaded, "mkc");
        assert_eq!(record.format, "mkc");
        assert_eq!(record.code, "1001");

        let outcome =
            crate::library::scan_folders(&[("mkc".to_owned(), root.clone())], &[], &|_, _| {});
        assert_eq!(outcome.songs.len(), 1);
        assert_eq!(outcome.songs[0].format, "mkc");

        rewrite_midi(
            &output,
            &output,
            &midi_bytes,
            Some(&MkcEdit {
                title: Some("Renamed".to_owned()),
                ..Default::default()
            }),
            true,
        )
        .unwrap();
        let updated = read_package(&output).unwrap().manifest().unwrap();
        assert_eq!(updated.title, "Renamed");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
