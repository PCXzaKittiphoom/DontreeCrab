//! Import/export format for the playlist (`.json`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistEntry {
    /// Absolute path to the song file.
    pub path: PathBuf,
    /// `kar`, `ncn`, `midi` or `unknown`.
    #[serde(rename = "type")]
    pub file_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistFile {
    pub version: u32,
    pub songs: Vec<PlaylistEntry>,
}

impl PlaylistFile {
    pub fn new(songs: Vec<PlaylistEntry>) -> Self {
        Self { version: 1, songs }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let text = text.trim_start_matches('\u{feff}');
        Ok(serde_json::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let file = PlaylistFile::new(vec![PlaylistEntry {
            path: PathBuf::from(r"D:\songs\a.kar"),
            file_type: "kar".to_owned(),
            title: "เพลงทดสอบ".to_owned(),
            artist: "ศิลปิน".to_owned(),
        }]);

        let text = serde_json::to_string(&file).unwrap();
        assert!(text.contains("\"type\":\"kar\""));

        let back: PlaylistFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.songs.len(), 1);
        assert_eq!(back.songs[0].title, "เพลงทดสอบ");
        assert_eq!(back.songs[0].path, file.songs[0].path);
    }

    #[test]
    fn saves_and_loads_a_file() {
        let path = std::env::temp_dir().join("karaoke-playlist-test.json");
        let file = PlaylistFile::new(vec![PlaylistEntry {
            path: PathBuf::from(r"D:\songs\b.mid"),
            file_type: "ncn".to_owned(),
            title: "ทดสอบ".to_owned(),
            artist: "ศิลปิน".to_owned(),
        }]);

        file.save(&path).unwrap();
        let back = PlaylistFile::load(&path).unwrap();
        assert_eq!(back.songs.len(), 1);
        assert_eq!(back.songs[0].file_type, "ncn");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_optional_fields_are_allowed() {
        let text = r#"{"version":1,"songs":[{"path":"C:\\a.mid","type":"ncn"}]}"#;
        let file: PlaylistFile = serde_json::from_str(text).unwrap();
        assert_eq!(file.songs[0].title, "");
        assert_eq!(file.songs[0].artist, "");
    }
}
