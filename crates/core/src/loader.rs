use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::kar;
use crate::kmid;
use crate::midi;
use crate::model::{Song, SourceFormat};
use crate::ncn;
use crate::text::{EncodingChoice, decode};

#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    pub encoding: EncodingChoice,
    /// Force a container instead of sniffing it.
    pub format: Option<SourceFormat>,
    /// Explicit `.lyr` path (NCN only).
    pub lyrics: Option<PathBuf>,
    /// Explicit `.cur` path (NCN only).
    pub cursor: Option<PathBuf>,
}

/// Load any supported song file and normalise it into a [`Song`].
///
/// `.kar` is always KAR. A `.mid` is treated as KMID when it carries an
/// embedded `LyrHdr` lyric payload, then as NCN when `Lyrics/` and `Cursor/`
/// siblings can be found, and otherwise as an ordinary MIDI/KAR file.
pub fn load(path: &Path, opts: &LoadOptions) -> Result<Song> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let doc = midi::parse(&bytes, opts.encoding)
        .with_context(|| format!("parsing MIDI data in {}", path.display()))?;

    let forced_kmid = opts.format == Some(SourceFormat::Kmid);
    let forced_midi = opts.format == Some(SourceFormat::Midi);
    if !matches!(
        opts.format,
        Some(SourceFormat::Kar | SourceFormat::Ncn | SourceFormat::Midi)
    ) {
        match kmid::try_build(&doc) {
            Ok(Some(song)) => return Ok(song),
            Ok(None) => {
                if forced_kmid {
                    bail!(
                        "{}: no embedded KMID lyric payload (LyrHdr) found",
                        path.display()
                    );
                }
            }
            Err(err) => {
                if forced_kmid {
                    return Err(err)
                        .with_context(|| format!("decoding KMID lyrics in {}", path.display()));
                }
                // A broken lyric payload should not make a playable MIDI file
                // unusable, so fall through to the ordinary paths.
                eprintln!("kmid: {err:#} ({} will play as plain MIDI)", path.display());
            }
        }
    }

    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let is_ncn = match opts.format {
        Some(SourceFormat::Kar) => false,
        Some(SourceFormat::Ncn) => true,
        Some(SourceFormat::Kmid) => false,
        Some(SourceFormat::Midi) => false,
        None => extension != "kar" && !doc.has_lyrics && ncn::locate(path, opts).is_some(),
    };

    if is_ncn {
        let (lyr, cur) = ncn::locate(path, opts).with_context(|| {
            format!(
                "could not find the .lyr/.cur companions of {}",
                path.display()
            )
        })?;
        return ncn::build(path, doc, &lyr, &cur, opts.encoding);
    }

    let has_kar_marker = doc.text_events.iter().any(|event| {
        decode(&event.raw, opts.encoding)
            .trim_start()
            .to_ascii_uppercase()
            .starts_with("@KMIDI KARAOKE FILE")
    });

    let mut song = kar::from_doc(doc, opts.encoding);
    if forced_midi {
        song.format = SourceFormat::Midi;
    } else if opts.format.is_none() {
        // If nothing identifies this as a karaoke file, show it as a plain
        // MIDI: no `.kar` extension, no KAR marker and no lyric lines.
        let identified = extension == "kar" || has_kar_marker || !song.lyric_lines.is_empty();
        if !identified {
            song.format = SourceFormat::Midi;
        }
    }
    Ok(song)
}
