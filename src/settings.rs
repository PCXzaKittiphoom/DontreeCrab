use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Vertical placement of the karaoke lyric block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TextVisibility {
    #[default]
    Center,
    Upper,
    Lower,
}

/// How the moving karaoke cursor is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CursorVisibility {
    #[default]
    ModernUpper,
    LegacyCursor,
}

/// Font used for the karaoke lyric text (never for the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    /// Use the external font stored in [`Settings::lyric_font`].
    Custom,
}

/// User preferences, stored as JSON in the platform config directory
/// (`%APPDATA%\DontreeCrab\DontreeCrab\config\settings.json` on Windows).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub transpose: i32,
    /// UI language code, or `auto` to follow the operating system.
    pub language: String,
    pub soundfont: Option<PathBuf>,
    /// Loop the playlist when the last song finishes.
    pub repeat: bool,
    /// Show the playlist panel.
    pub show_playlist: bool,
    /// Mute the karaoke melody track (MIDI channel 9, index 8).
    pub melo_muted: bool,
    /// Scale factor for the karaoke lyric font (1.0 = default).
    pub font_scale: f32,
    /// Outline thickness multiplier for the lyrics (1.0 = 100%).
    pub lyric_edge_size: f32,
    /// Gap between the two lyric lines (1.0 = 100%).
    pub lyric_distance: f32,
    pub text_visibility: TextVisibility,
    pub cursor_visibility: CursorVisibility,
    pub lyric_font_style: FontStyle,
    /// External font file used when `lyric_font_style` is `Custom`.
    pub lyric_font: Option<PathBuf>,
    /// Dimming of the upcoming lyric line in percent (0 = fully bright,
    /// 50 = dimmest allowed).
    pub lyric_depth: f32,
    /// `#RRGGBB` colour behind the karaoke lyrics.
    pub background_color: String,
    /// Seconds the action readouts (VOL/KEY/BPM/UI/FONT) stay visible.
    /// 0 hides them immediately.
    pub indicator_seconds: u32,
    /// Seconds the playlist stays visible after arrow-key browsing.
    /// 0 hides it immediately.
    pub playlist_seconds: u32,
    /// Show the song title and artist in the top bar.
    pub show_song_info: bool,
    /// Show the bar/beat indicator in the top bar.
    pub show_bar_beat: bool,
    /// Show the 24-hour clock in the top bar.
    pub show_clock: bool,
    /// Remove a song from the playlist after it finishes.
    pub delete_played_songs: bool,
    /// Start the first newly added song immediately.
    pub play_immediately: bool,
    /// Advance to the next playlist entry when a song finishes.
    pub auto_play: bool,
    /// Seconds to wait after a song before starting the next one (0–10).
    pub music_interval_seconds: u32,
    /// Hide the mouse cursor while it is over the lyric display.
    pub hide_cursor: bool,
    /// Ask for confirmation before closing the player.
    pub confirm_exit: bool,
    /// `#RRGGBB` colours for the unsung lyrics and the karaoke cursor.
    pub lyric_text_color: String,
    pub lyric_edge_color: String,
    pub cursor_text_color: String,
    pub cursor_edge_color: String,
    /// Enable the 3-band equalizer.
    pub eq_enabled: bool,
    /// Equalizer gains in dB (low shelf / peak / high shelf).
    pub eq_low_db: f32,
    pub eq_mid_db: f32,
    pub eq_high_db: f32,
    /// Enable MIDI reverb/chorus sends.
    pub midi_effects: bool,
    /// Requested SoundFont buffer size in milliseconds (0 = driver default).
    pub sf2_buffer_ms: u32,
    /// Maximum simultaneous voices (8–256). Large SoundFonts need more.
    pub max_polyphony: u32,
    /// Effect-send multipliers: 1.0 keeps the values written in the MIDI file.
    pub rev_inst: f32,
    pub cho_inst: f32,
    pub rev_drum: f32,
    pub cho_drum: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.7,
            transpose: 0,
            language: "auto".to_owned(),
            soundfont: None,
            repeat: true,
            show_playlist: true,
            melo_muted: false,
            font_scale: 1.0,
            lyric_edge_size: 1.0,
            lyric_distance: 1.0,
            text_visibility: TextVisibility::Center,
            cursor_visibility: CursorVisibility::ModernUpper,
            lyric_font_style: FontStyle::Regular,
            lyric_font: None,
            lyric_depth: 0.0,
            background_color: "#000000".to_owned(),
            indicator_seconds: 5,
            playlist_seconds: 10,
            show_song_info: true,
            show_bar_beat: true,
            show_clock: true,
            delete_played_songs: false,
            play_immediately: false,
            auto_play: true,
            music_interval_seconds: 0,
            hide_cursor: false,
            confirm_exit: true,
            lyric_text_color: "#FFFFFF".to_owned(),
            lyric_edge_color: "#000000".to_owned(),
            cursor_text_color: "#0000FF".to_owned(),
            cursor_edge_color: "#FFFFFF".to_owned(),
            eq_enabled: true,
            eq_low_db: 0.0,
            eq_mid_db: 0.0,
            eq_high_db: 0.0,
            midi_effects: true,
            sf2_buffer_ms: 0,
            max_polyphony: 256,
            rev_inst: 0.5,
            cho_inst: 0.5,
            rev_drum: 0.5,
            cho_drum: 0.5,
        }
    }
}

impl Settings {
    pub fn config_path() -> Option<PathBuf> {
        ProjectDirs::from("dev", "DontreeCrab", "DontreeCrab")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }

    /// The path used by the previous "KaraokePlayer" name, read once so an
    /// existing installation keeps its settings after the rename.
    fn legacy_config_path() -> Option<PathBuf> {
        ProjectDirs::from("dev", "KaraokePlayer", "KaraokePlayer")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }

    /// Load the settings, falling back to defaults on any problem. A broken
    /// config must never stop the player from starting.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        let text = std::fs::read_to_string(&path).ok().or_else(|| {
            Self::legacy_config_path().and_then(|legacy| std::fs::read_to_string(legacy).ok())
        });
        let Some(text) = text else {
            return Self::default();
        };
        // Tolerate a UTF-8 BOM (some editors add one).
        let text = text.trim_start_matches('\u{feff}');
        let mut settings: Self = serde_json::from_str(text).unwrap_or_default();
        settings.sanitise();
        settings
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = Self::config_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn sanitise(&mut self) {
        if !self.volume.is_finite() {
            self.volume = 0.7;
        }
        self.volume = self.volume.clamp(0.0, 1.0);
        self.transpose = self.transpose.clamp(-12, 12);
        if !self.font_scale.is_finite() {
            self.font_scale = 1.0;
        }
        self.font_scale = self.font_scale.clamp(0.4, 2.5);

        if !self.lyric_edge_size.is_finite() {
            self.lyric_edge_size = 1.0;
        }
        self.lyric_edge_size = self.lyric_edge_size.clamp(0.0, 2.0);

        if !self.lyric_distance.is_finite() {
            self.lyric_distance = 1.0;
        }
        self.lyric_distance = self.lyric_distance.clamp(0.5, 3.0);

        if !self.lyric_depth.is_finite() {
            self.lyric_depth = 0.0;
        }
        self.lyric_depth = self.lyric_depth.clamp(0.0, 50.0);

        if self
            .lyric_font
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            self.lyric_font = None;
        }
        if self.lyric_font.is_none() && self.lyric_font_style == FontStyle::Custom {
            self.lyric_font_style = FontStyle::Regular;
        }

        self.indicator_seconds = self.indicator_seconds.min(60);
        self.playlist_seconds = self.playlist_seconds.min(60);
        self.music_interval_seconds = self.music_interval_seconds.min(10);

        normalise_hex(&mut self.background_color, "#000000");
        normalise_hex(&mut self.lyric_text_color, "#FFFFFF");
        normalise_hex(&mut self.lyric_edge_color, "#000000");
        normalise_hex(&mut self.cursor_text_color, "#0000FF");
        normalise_hex(&mut self.cursor_edge_color, "#FFFFFF");

        for gain in [
            &mut self.eq_low_db,
            &mut self.eq_mid_db,
            &mut self.eq_high_db,
        ] {
            if !gain.is_finite() {
                *gain = 0.0;
            }
            *gain = gain.clamp(-12.0, 12.0);
        }

        for send in [
            &mut self.rev_inst,
            &mut self.cho_inst,
            &mut self.rev_drum,
            &mut self.cho_drum,
        ] {
            if !send.is_finite() {
                *send = 1.0;
            }
            *send = send.clamp(0.0, 1.0);
        }

        if self.sf2_buffer_ms > 0 {
            self.sf2_buffer_ms = self.sf2_buffer_ms.clamp(10, 100);
        }
        self.max_polyphony = self.max_polyphony.clamp(8, 256);

        self.soundfont = self
            .soundfont
            .take()
            .map(|path| std::path::absolute(&path).unwrap_or(path));
    }
}

fn normalise_hex(value: &mut String, fallback: &str) {
    let trimmed = value.trim();
    let valid = trimmed.len() == 7
        && trimmed.starts_with('#')
        && trimmed.get(1..).map_or(false, |digits| {
            digits.chars().all(|c| c.is_ascii_hexdigit())
        });
    *value = if valid {
        trimmed.to_ascii_uppercase()
    } else {
        fallback.to_owned()
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let mut settings = Settings::default();
        settings.volume = 0.42;
        settings.transpose = -3;
        settings.melo_muted = true;

        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert!((back.volume - 0.42).abs() < 1e-6);
        assert_eq!(back.transpose, -3);
        assert!(back.melo_muted);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let back: Settings = serde_json::from_str(r#"{"volume":0.3}"#).unwrap();
        assert!((back.volume - 0.3).abs() < 1e-6);
        assert_eq!(back.transpose, 0);
        assert!(back.repeat);
    }

    #[test]
    fn sanitise_clamps_bad_values() {
        let mut settings = Settings {
            volume: 9.0,
            transpose: -99,
            ..Settings::default()
        };
        settings.sanitise();
        assert_eq!(settings.volume, 1.0);
        assert_eq!(settings.transpose, -12);

        settings.volume = f32::NAN;
        settings.sanitise();
        assert!((settings.volume - 0.7).abs() < 1e-6);
    }

    #[test]
    fn sanitise_clamps_eq_effects_and_buffer() {
        let mut settings = Settings {
            eq_low_db: 99.0,
            eq_high_db: f32::NAN,
            rev_inst: 5.0,
            cho_drum: -1.0,
            sf2_buffer_ms: 999,
            max_polyphony: 999,
            ..Settings::default()
        };
        settings.sanitise();
        assert_eq!(settings.eq_low_db, 12.0);
        assert_eq!(settings.eq_high_db, 0.0);
        assert_eq!(settings.rev_inst, 1.0);
        assert_eq!(settings.cho_drum, 0.0);
        assert_eq!(settings.sf2_buffer_ms, 100);
        assert_eq!(settings.max_polyphony, 256);

        settings.sf2_buffer_ms = 5;
        settings.sanitise();
        assert_eq!(settings.sf2_buffer_ms, 10);
        settings.sf2_buffer_ms = 0;
        settings.sanitise();
        assert_eq!(settings.sf2_buffer_ms, 0);
    }

    #[test]
    fn lyric_display_defaults_and_hex() {
        let mut settings = Settings::default();
        assert_eq!(settings.text_visibility, TextVisibility::Center);
        assert_eq!(settings.cursor_visibility, CursorVisibility::ModernUpper);
        assert_eq!(settings.lyric_font_style, FontStyle::Regular);
        assert!(settings.lyric_font.is_none());
        assert!((settings.lyric_depth - 0.0).abs() < 1e-6);
        assert_eq!(settings.background_color, "#000000");

        settings.lyric_text_color = "ffffff".to_owned();
        settings.lyric_edge_color = "nope".to_owned();
        settings.background_color = "nope".to_owned();
        settings.lyric_edge_size = 9.0;
        settings.lyric_distance = 0.1;
        settings.lyric_depth = 99.0;
        settings.sanitise();

        assert_eq!(settings.lyric_text_color, "#FFFFFF");
        assert_eq!(settings.lyric_edge_color, "#000000");
        assert_eq!(settings.background_color, "#000000");
        assert_eq!(settings.lyric_edge_size, 2.0);
        assert_eq!(settings.lyric_distance, 0.5);
        assert_eq!(settings.lyric_depth, 50.0);

        // Custom without a font path falls back to the built-in face.
        settings.lyric_font_style = FontStyle::Custom;
        settings.lyric_font = Some(PathBuf::new());
        settings.sanitise();
        assert!(settings.lyric_font.is_none());
        assert_eq!(settings.lyric_font_style, FontStyle::Regular);
    }

    #[test]
    fn general_defaults_and_limits() {
        let mut settings = Settings::default();
        assert_eq!(settings.indicator_seconds, 5);
        assert_eq!(settings.playlist_seconds, 10);
        assert!(settings.show_song_info);
        assert!(settings.show_bar_beat);
        assert!(settings.show_clock);
        assert!(!settings.hide_cursor);
        assert!(settings.confirm_exit);

        settings.indicator_seconds = 999;
        settings.playlist_seconds = 61;
        settings.sanitise();
        assert_eq!(settings.indicator_seconds, 60);
        assert_eq!(settings.playlist_seconds, 60);
    }

    #[test]
    fn player_defaults_and_limits() {
        let mut settings = Settings::default();
        assert!(!settings.delete_played_songs);
        assert!(!settings.play_immediately);
        assert!(settings.auto_play);
        assert_eq!(settings.music_interval_seconds, 0);

        settings.music_interval_seconds = 99;
        settings.sanitise();
        assert_eq!(settings.music_interval_seconds, 10);
    }
}
