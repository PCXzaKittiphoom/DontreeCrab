use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// How a key change (transpose) treats the notes that are already sounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransposeMode {
    /// One command transposes every instrument except the drums, including
    /// the notes that are already playing (Roland "All Tuner").
    #[default]
    All,
    /// The same result, but the player sends one command per channel
    /// ("Wolf Tuner").
    Wolf,
    /// Transpose only the notes that start after the change; sounding notes
    /// are cut. Works with devices that ignore the bulk messages
    /// ("Recovery Tuner").
    Recovery,
}

/// MIDI reset command used before a song and by the "Reset MIDI" button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MidiReset {
    /// General MIDI reset (`F0 7E 7F 09 01 F7`).
    #[default]
    Gm,
    /// Roland GS reset (`F0 41 10 42 12 40 00 7F 00 41 F7`).
    Gs,
    /// Yamaha XG reset (`F0 43 10 4C 00 00 7E 00 F7`).
    Xg,
    /// Roland-specific reset: GS reset plus a controller reset.
    Roland,
    /// Software synthesizer: no SysEx, just the standard controllers
    /// (CC120/121/123 and sustain off).
    SoundFont,
}

impl MidiReset {
    /// SysEx message this reset sends to an external device, if any.
    pub fn sysex(self) -> Option<&'static [u8]> {
        match self {
            Self::Gm => Some(&[0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7]),
            Self::Gs | Self::Roland => Some(&[
                0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x00, 0x41, 0xF7,
            ]),
            Self::Xg => Some(&[0xF0, 0x43, 0x10, 0x4C, 0x00, 0x00, 0x7E, 0x00, 0xF7]),
            Self::SoundFont => None,
        }
    }
}

/// Vertical placement of the karaoke lyric block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TextVisibility {
    /// Existing settings that still say `center` migrate to the lower block.
    #[default]
    #[serde(alias = "center")]
    Lower,
    Upper,
}

/// What fills the area behind the lyrics.
/// Artwork behind the lyrics.
///
/// `Default` only exists so settings written before the background picker was
/// simplified still load; [`Settings::sanitise`] migrates it to [`Self::Image`],
/// which shows the artwork shipped with the player until an external image is
/// chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundStyle {
    /// Legacy value: the artwork shipped with the player.
    Default,
    /// The flat `background_color` fill.
    Color,
    /// An external .png/.jpg scaled to the window, or the shipped artwork
    /// when no file is selected.
    #[default]
    Image,
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
    /// Keep the in-window MIDI Controller panel open across restarts.
    pub controller_open: bool,
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
    /// Colour fill or an external image behind the lyrics.
    pub background_style: BackgroundStyle,
    /// External image drawn behind the lyrics when `background_style` is
    /// `Image`; it is scaled to the window.
    pub background_image: Option<PathBuf>,
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
    /// Hide the transport bar until the pointer reaches the bottom edge.
    pub hide_transport: bool,
    /// Ask for confirmation before closing the player.
    pub confirm_exit: bool,
    /// Open the main window in fullscreen mode.
    pub fullscreen: bool,
    /// Folder roots scanned for each supported format.
    pub ncn_folder: Option<PathBuf>,
    pub kmid_folder: Option<PathBuf>,
    pub kar_folder: Option<PathBuf>,
    /// Folder root for `.mkc` files.
    pub mkc_folder: Option<PathBuf>,
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
    /// Seconds the song search popup stays visible after the last key press.
    pub popup_seconds: u32,
    /// Send the selected MIDI reset SysEx when a song starts.
    pub midi_sysex: bool,
    /// How a key change affects the notes that are already sounding.
    pub transpose_mode: TransposeMode,
    /// MIDI reset command used before a song and by the Reset MIDI button.
    pub midi_reset: MidiReset,
    /// External MIDI output port name; empty means the built-in SoundFont.
    pub midi_out_port: String,
    /// Per-channel MIDI output override (16 entries). An empty string follows
    /// the default port above.
    pub split_ports: Vec<String>,
    /// Per-instrument-group MIDI output override (20 entries).
    pub group_ports: Vec<String>,
    /// Per-group volume switches (20 entries).
    pub group_volume_on: Vec<bool>,
    /// Per-group MIDI volume (0-127, 100 = unchanged).
    pub group_volume: Vec<u32>,
    /// Per-group velocity switches (20 entries).
    pub group_velocity_on: Vec<bool>,
    /// Per-group minimum note velocity (0-127).
    pub group_velocity_min: Vec<u32>,
    /// Per-group maximum note velocity (0-127).
    pub group_velocity_max: Vec<u32>,
    /// Per-drum-group MIDI output override (9 entries).
    pub drum_ports: Vec<String>,
    /// Per-drum-group MIDI volume (0-127, 100 = unchanged).
    pub drum_volume: Vec<u32>,
    /// Per-drum-group minimum note velocity (0-127).
    pub drum_velocity_min: Vec<u32>,
    /// Per-drum-group maximum note velocity (0-127).
    pub drum_velocity_max: Vec<u32>,
    /// Per-drum-group volume switches (9 entries).
    pub drum_volume_on: Vec<bool>,
    /// Per-drum-group velocity switches (9 entries).
    pub drum_velocity_on: Vec<bool>,
    /// Force every bass program to `lock_bass_program`.
    pub lock_bass: bool,
    /// General MIDI program (1-based, 34 = Electric Bass finger).
    pub lock_bass_program: u32,
    /// Force the drum channel to one GM2 kit.
    pub lock_drum_kit: bool,
    /// GM2 drum kit number (1-based).
    pub lock_drum_program: u32,
    /// Force kick notes to `lock_kick_note`.
    pub lock_kick: bool,
    pub lock_kick_note: u32,
    /// Force snare notes to `lock_snare_note`.
    pub lock_snare: bool,
    pub lock_snare_note: u32,
    /// Click track: first beat and the other beats use these GM notes.
    pub metronome: bool,
    pub metronome_hihat: bool,
    pub metronome_first: u32,
    pub metronome_other: u32,
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
            controller_open: false,
            melo_muted: false,
            font_scale: 1.0,
            lyric_edge_size: 1.0,
            lyric_distance: 1.0,
            text_visibility: TextVisibility::Lower,
            cursor_visibility: CursorVisibility::ModernUpper,
            lyric_font_style: FontStyle::Regular,
            lyric_font: None,
            lyric_depth: 0.0,
            background_color: "#000000".to_owned(),
            background_style: BackgroundStyle::Image,
            background_image: None,
            indicator_seconds: 5,
            playlist_seconds: 10,
            show_song_info: true,
            show_bar_beat: true,
            show_clock: true,
            delete_played_songs: true,
            play_immediately: true,
            auto_play: true,
            music_interval_seconds: 0,
            hide_cursor: false,
            hide_transport: false,
            confirm_exit: true,
            fullscreen: false,
            ncn_folder: None,
            kmid_folder: None,
            kar_folder: None,
            mkc_folder: None,
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
            popup_seconds: 5,
            midi_sysex: false,
            transpose_mode: TransposeMode::default(),
            midi_reset: MidiReset::default(),
            midi_out_port: String::new(),
            split_ports: vec![String::new(); 16],
            group_ports: vec![String::new(); 20],
            group_volume_on: vec![false; 20],
            group_volume: vec![100; 20],
            group_velocity_on: vec![false; 20],
            group_velocity_min: vec![0; 20],
            group_velocity_max: vec![127; 20],
            drum_ports: vec![String::new(); 9],
            drum_volume: vec![100; 9],
            drum_velocity_min: vec![0; 9],
            drum_velocity_max: vec![127; 9],
            drum_volume_on: vec![false; 9],
            drum_velocity_on: vec![false; 9],
            lock_bass: false,
            lock_bass_program: 34,
            lock_drum_kit: false,
            lock_drum_program: 1,
            lock_kick: false,
            lock_kick_note: 36,
            lock_snare: false,
            lock_snare_note: 38,
            metronome: false,
            metronome_hihat: false,
            metronome_first: 75,
            metronome_other: 76,
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
        let mut settings: Self = match serde_json::from_str(text) {
            Ok(settings) => settings,
            Err(_) => {
                // Keep the unreadable file for inspection instead of silently
                // overwriting it on the next save.
                let _ = std::fs::copy(&path, path.with_extension("json.broken"));
                Self::default()
            }
        };
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
        // Write a temporary file first: a crash mid-write must not leave a
        // half-written settings file behind.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        // Windows refuses to rename onto an existing file.
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    pub fn sanitise(&mut self) {
        self.split_ports.resize(16, String::new());
        self.split_ports.truncate(16);
        self.group_ports.resize(20, String::new());
        self.group_ports.truncate(20);
        self.group_volume.resize(20, 100);
        self.group_volume.truncate(20);
        self.group_volume_on.resize(20, false);
        self.group_volume_on.truncate(20);
        self.group_velocity_on.resize(20, false);
        self.group_velocity_on.truncate(20);
        self.group_velocity_min.resize(20, 0);
        self.group_velocity_min.truncate(20);
        self.group_velocity_max.resize(20, 127);
        self.group_velocity_max.truncate(20);
        for value in &mut self.group_volume {
            *value = (*value).min(127);
        }
        for (index, value) in self.group_velocity_min.iter_mut().enumerate() {
            *value = (*value).min(127);
            // Keep the window ordered even after a hand-edited settings file.
            let max = self.group_velocity_max[index];
            *value = (*value).min(max);
        }
        for value in &mut self.group_velocity_max {
            *value = (*value).min(127);
        }
        self.drum_ports.resize(9, String::new());
        self.drum_ports.truncate(9);
        self.drum_volume.resize(9, 100);
        self.drum_volume.truncate(9);
        self.drum_volume_on.resize(9, false);
        self.drum_volume_on.truncate(9);
        self.drum_velocity_on.resize(9, false);
        self.drum_velocity_on.truncate(9);
        self.drum_velocity_min.resize(9, 0);
        self.drum_velocity_min.truncate(9);
        self.drum_velocity_max.resize(9, 127);
        self.drum_velocity_max.truncate(9);
        for value in &mut self.drum_volume {
            *value = (*value).min(127);
        }
        for (index, value) in self.drum_velocity_min.iter_mut().enumerate() {
            *value = (*value).min(127);
            let max = self.drum_velocity_max[index];
            *value = (*value).min(max);
        }
        for value in &mut self.drum_velocity_max {
            *value = (*value).min(127);
        }
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
        self.popup_seconds = self.popup_seconds.min(60);
        self.music_interval_seconds = self.music_interval_seconds.min(10);

        normalise_hex(&mut self.background_color, "#000000");
        // The picker now offers only Image and Color. Older settings used
        // "default" for the shipped artwork, which is what Image shows while
        // no external file is selected.
        if self.background_style == BackgroundStyle::Default {
            self.background_style = BackgroundStyle::Image;
        }
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

        for path in [
            &mut self.ncn_folder,
            &mut self.kmid_folder,
            &mut self.kar_folder,
        ] {
            sanitise_optional_path(path);
        }
    }
}

fn sanitise_optional_path(path: &mut Option<PathBuf>) {
    if path
        .as_ref()
        .is_some_and(|value| value.as_os_str().is_empty())
    {
        *path = None;
        return;
    }
    if let Some(value) = path.take() {
        *path = Some(std::path::absolute(&value).unwrap_or(value));
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
        settings.fullscreen = true;
        settings.controller_open = true;
        settings.group_volume_on[0] = true;
        settings.group_velocity_on[1] = true;
        settings.drum_volume_on[2] = true;
        settings.drum_velocity_on[3] = true;
        settings.ncn_folder = Some(PathBuf::from(r"C:\Songs\NCN"));
        settings.mkc_folder = Some(PathBuf::from(r"C:\Songs\MKC"));

        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert!((back.volume - 0.42).abs() < 1e-6);
        assert_eq!(back.transpose, -3);
        assert!(back.melo_muted);
        assert!(back.fullscreen);
        assert!(back.controller_open);
        assert!(back.group_volume_on[0]);
        assert!(!back.group_volume_on[1]);
        assert!(back.group_velocity_on[1]);
        assert!(back.drum_volume_on[2]);
        assert!(back.drum_velocity_on[3]);
        assert_eq!(back.ncn_folder, Some(PathBuf::from(r"C:\Songs\NCN")));
        assert_eq!(back.mkc_folder, Some(PathBuf::from(r"C:\Songs\MKC")));
    }

    #[test]
    fn midi_reset_commands_match_the_standard_sysex() {
        assert_eq!(
            MidiReset::Gm.sysex(),
            Some([0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7].as_slice())
        );
        assert_eq!(
            MidiReset::Gs.sysex(),
            Some(
                [
                    0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x00, 0x41, 0xF7
                ]
                .as_slice()
            )
        );
        assert_eq!(
            MidiReset::Xg.sysex(),
            Some([0xF0, 0x43, 0x10, 0x4C, 0x00, 0x00, 0x7E, 0x00, 0xF7].as_slice())
        );
        assert_eq!(MidiReset::Roland.sysex(), MidiReset::Gs.sysex());
        assert_eq!(MidiReset::SoundFont.sysex(), None);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let back: Settings = serde_json::from_str(r#"{"volume":0.3}"#).unwrap();
        assert!((back.volume - 0.3).abs() < 1e-6);
        assert_eq!(back.transpose, 0);
        assert!(back.repeat);
        assert!(!back.fullscreen);
        assert!(!back.controller_open);
        assert_eq!(back.mkc_folder, None);
        // Old settings files start with every group adjustment off.
        assert!(back.group_volume_on.iter().all(|value| !*value));
        assert!(back.group_velocity_on.iter().all(|value| !*value));
        assert!(back.drum_volume_on.iter().all(|value| !*value));
        assert!(back.drum_velocity_on.iter().all(|value| !*value));
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

        settings.group_volume = vec![200; 20];
        settings.group_ports = vec!["port".to_owned(); 25];
        settings.sanitise();
        assert!(settings.group_volume.iter().all(|value| *value == 127));
        assert_eq!(settings.group_ports.len(), 20);

        settings.drum_volume = vec![200; 9];
        settings.drum_ports = vec!["port".to_owned(); 12];
        settings.drum_volume_on = vec![true; 12];
        settings.sanitise();
        assert!(settings.drum_volume.iter().all(|value| *value == 127));
        assert_eq!(settings.drum_ports.len(), 9);
        assert_eq!(settings.drum_volume_on.len(), 9);
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
    fn old_centred_lyrics_migrate_to_the_lower_block() {
        assert_eq!(
            serde_json::from_str::<TextVisibility>("\"center\"").unwrap(),
            TextVisibility::Lower
        );
        assert_eq!(
            serde_json::from_str::<TextVisibility>("\"upper\"").unwrap(),
            TextVisibility::Upper
        );
    }

    #[test]
    fn lyric_display_defaults_and_hex() {
        let mut settings = Settings::default();
        assert_eq!(settings.text_visibility, TextVisibility::Lower);
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
    fn legacy_default_background_migrates_to_image() {
        let mut settings = Settings::default();
        assert_eq!(settings.background_style, BackgroundStyle::Image);

        // Settings files written before the picker was simplified still load,
        // then migrate to the image style (which shows the shipped artwork
        // until the user picks a file).
        let style: BackgroundStyle = serde_json::from_str("\"default\"").unwrap();
        assert_eq!(style, BackgroundStyle::Default);
        settings.background_style = BackgroundStyle::Default;
        settings.sanitise();
        assert_eq!(settings.background_style, BackgroundStyle::Image);
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
        assert!(settings.ncn_folder.is_none());
        assert!(settings.kmid_folder.is_none());
        assert!(settings.kar_folder.is_none());

        settings.indicator_seconds = 999;
        settings.playlist_seconds = 61;
        settings.sanitise();
        assert_eq!(settings.indicator_seconds, 60);
        assert_eq!(settings.playlist_seconds, 60);
    }

    #[test]
    fn player_defaults_and_limits() {
        let mut settings = Settings::default();
        assert!(settings.delete_played_songs);
        assert!(settings.play_immediately);
        assert!(settings.auto_play);
        assert_eq!(settings.music_interval_seconds, 0);

        settings.music_interval_seconds = 99;
        settings.sanitise();
        assert_eq!(settings.music_interval_seconds, 10);
    }
}
