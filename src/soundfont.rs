use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use rustysynth::SoundFont;

/// Find a SoundFont to use.
///
/// Order of preference:
/// 1. the explicit path,
/// 2. `$KARAOKE_SF2`,
/// 3. the first `.sf2` in `./soundfonts`, then in the working directory.
pub fn resolve(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.exists() {
            return Ok(path.to_path_buf());
        }
        bail!("soundfont not found: {}", path.display());
    }

    if let Ok(value) = std::env::var("KARAOKE_SF2") {
        let path = PathBuf::from(value);
        if path.exists() {
            return Ok(path);
        }
    }

    let mut found: Vec<PathBuf> = Vec::new();
    for dir in ["soundfonts", "."] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_sf2 = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("sf2"))
                .unwrap_or(false);
            if is_sf2 {
                found.push(path);
            }
        }
    }
    found.sort();

    found
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no .sf2 soundfont found; pass --sf2 or put one in ./soundfonts"))
}

pub fn load(path: &Path) -> Result<Arc<SoundFont>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening soundfont {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let sound_font = SoundFont::new(&mut reader)
        .map_err(|e| anyhow!("reading soundfont {}: {e}", path.display()))?;
    Ok(Arc::new(sound_font))
}
