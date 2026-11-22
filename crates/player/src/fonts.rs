//! Bundled font packs for lyric scripts other than Thai and Latin.
//!
//! Sarabun already covers Thai, Latin and Vietnamese. Japanese, Chinese,
//! Korean and Lao come from the open (SIL OFL) Noto fonts shipped in
//! `assets/fonts/packs` next to the executable. Egui walks the font family in
//! order and uses the first face that has the glyph, so adding the packs as a
//! fallback chain is enough: no per-song font switching is needed.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// One face in the fallback chain.
pub struct FontPack {
    /// Name used inside [`egui::FontDefinitions`].
    pub id: &'static str,
    /// File name inside `assets/fonts/packs`.
    pub file: &'static str,
    /// Human-readable name, used by tests and diagnostics.
    #[allow(dead_code)]
    pub label: &'static str,
    /// Language this pack is primarily for.
    #[allow(dead_code)]
    pub language: &'static str,
}

/// Fallback order for the CJK and Lao packs.
///
/// Japanese comes first because it also carries kana; Chinese simplified
/// characters that only exist in GB fonts still resolve to the SC pack.
pub const PACKS: &[FontPack] = &[
    FontPack {
        id: "pack-ja",
        file: "NotoSansJP-Regular.otf",
        label: "Noto Sans JP",
        language: "ja",
    },
    FontPack {
        id: "pack-ko",
        file: "NotoSansKR-Regular.otf",
        label: "Noto Sans KR",
        language: "ko",
    },
    FontPack {
        id: "pack-zh",
        file: "NotoSansSC-Regular.otf",
        label: "Noto Sans SC",
        language: "zh",
    },
    FontPack {
        id: "pack-lo",
        file: "NotoSansLao.ttf",
        label: "Noto Sans Lao",
        language: "lo",
    },
];

/// Directories searched for the packs, in order.
///
/// The release zip ships `assets/fonts/packs` beside the executable; a
/// development checkout falls back to the workspace assets.
fn pack_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.join("assets").join("fonts").join("packs"));
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("assets")
        .join("fonts")
        .join("packs");
    if !dirs.contains(&source) {
        dirs.push(source);
    }
    dirs
}

/// Pack bytes, loaded once and shared with every egui viewport.
///
/// Missing files are skipped, so a build without the packs still runs (the
/// lyrics simply fall back to the Thai face).
pub fn loaded_packs() -> &'static [(&'static str, Arc<Vec<u8>>)] {
    static CACHE: OnceLock<Vec<(&'static str, Arc<Vec<u8>>)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let dirs = pack_dirs();
        let mut loaded = Vec::new();
        for pack in PACKS {
            for dir in &dirs {
                let path = dir.join(pack.file);
                if let Ok(bytes) = std::fs::read(&path) {
                    loaded.push((pack.id, Arc::new(bytes)));
                    break;
                }
            }
        }
        loaded
    })
}

/// Pack labels that were found on disk (empty when the assets are missing).
#[allow(dead_code)]
pub fn available_packs() -> Vec<&'static str> {
    let dirs = pack_dirs();
    PACKS
        .iter()
        .filter(|pack| dirs.iter().any(|dir| dir.join(pack.file).is_file()))
        .map(|pack| pack.label)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_tree_ships_every_pack() {
        let labels = available_packs();
        for pack in PACKS {
            assert!(
                labels.contains(&pack.label),
                "missing font pack: {} ({})",
                pack.label,
                pack.file
            );
        }
    }

    #[test]
    fn packs_load_into_memory() {
        let loaded = loaded_packs();
        assert_eq!(loaded.len(), PACKS.len());
        for (id, bytes) in loaded {
            assert!(!bytes.is_empty(), "{id} is empty");
        }
    }
}
