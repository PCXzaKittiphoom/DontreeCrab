use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use anyhow::Result;
use eframe::egui;
use egui::{Align, Color32, FontFamily, FontId, Layout, Pos2, Rect, RichText};
use karaoke_player::audio::{self, AudioHandle, Command, Player};
use karaoke_player::i18n::{self, tr, tr_args};
use karaoke_player::meter::MeterMap;
use karaoke_player::model::{LyricLine, Song};
use karaoke_player::playlist::{PlaylistEntry, PlaylistFile};
use karaoke_player::settings::{CursorVisibility, FontStyle, Settings, TextVisibility};
use karaoke_player::soundfont;
use karaoke_player::{LoadOptions, load};
use rustysynth::SoundFont;

use crate::icons::{self, Icon};

const BG: Color32 = Color32::BLACK;
const FG: Color32 = Color32::WHITE;
const DIM: Color32 = Color32::from_gray(120);
const ERROR: Color32 = Color32::from_rgb(230, 90, 90);

/// MIDI channel 9 (1-based) is where Thai karaoke files put the melody.
const MELODY_CHANNEL: u8 = 8;

const GITHUB_URL: &str = "https://github.com/PXCza/DontreeCrab";
const APP_VERSION_LABEL: &str = concat!(env!("CARGO_PKG_VERSION"), " Beta");
const LICENSE_TEXT: &str = include_str!("../LICENSE");

/// Which About popup is open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AboutPopup {
    License,
    Created,
}

/// Actions offered by the right-click menus.
#[derive(Clone, Copy)]
enum MenuAction {
    Open,
    AddFiles,
    AddFolder,
    ImportPlaylist,
    ExportPlaylist,
    TogglePlaylist,
    ToggleFullscreen,
    Minimize,
    ToggleCursor,
    ToggleRepeat,
    ClearPlaylist,
    Settings,
    Exit,
    Remove(usize),
}

/// Which context menu is open.
#[derive(Clone, Copy)]
enum MenuKind {
    Central,
    Row(usize),
    Background,
}

/// A custom context menu that can fade out during its closing animation.
struct OpenMenu {
    kind: MenuKind,
    pos: Pos2,
    alpha: f32,
    closing: bool,
    just_opened: bool,
    /// Whether the "Song Player" submenu is expanded.
    submenu_open: bool,
}

/// Which native file dialog is running on the worker thread.
#[derive(Clone, Copy)]
enum DialogKind {
    Open,
    AddFiles,
    AddFolder,
    ImportPlaylist,
    ExportPlaylist,
    SoundFont,
    LyricFont,
}

/// A native dialog that runs on its own thread so the player keeps drawing
/// and the song keeps playing while the dialog is open.
struct PendingDialog {
    kind: DialogKind,
    receiver: Receiver<Option<Vec<PathBuf>>>,
}

struct Playback {
    _handle: AudioHandle,
    commands: Sender<Command>,
    position: Arc<AtomicU64>,
}

/// Small on-screen readout shown for a few seconds after changing KEY or VOL.
struct Indicator {
    text: String,
    until: f64,
    /// The BPM readout is recomputed every frame so it follows tempo changes
    /// in the song while it is visible.
    bpm: bool,
}

/// Cached metadata for a playlist entry.
#[derive(Clone)]
struct SongInfo {
    title: String,
    artist: String,
    file_type: String,
}

pub struct KaraokeApp {
    song: Option<Song>,
    path: Option<PathBuf>,
    playback: Option<Playback>,
    sound_font: Option<Arc<SoundFont>>,
    sound_font_path: Option<PathBuf>,
    volume: f32,
    transpose: i32,
    paused: bool,
    scrubbing: bool,
    scrub_value: f64,
    error: Option<String>,
    settings: Settings,
    playlist: Vec<PathBuf>,
    playlist_index: usize,
    titles: std::collections::HashMap<PathBuf, SongInfo>,
    indicator: Option<Indicator>,
    /// Set after a song opens; the card timer starts on the next UI frame.
    song_card_pending: bool,
    /// While in the future, the song-info card is shown in the lyrics area.
    song_card_until: Option<f64>,
    playlist_rect: Rect,
    last_key_shortcut: f64,
    last_bpm_shortcut: f64,
    bpm: f64,
    /// Playback speed last sent to the audio thread.
    applied_speed: f64,
    /// Bar line targeted by a recent F7/F8 press. Keeps repeated presses
    /// moving one bar at a time before the audio position catches up.
    bar_seek_anchor: Option<(u64, f64)>,
    meter: MeterMap,
    /// Song time at the end of the final bar; playback stops here.
    song_end_us: u64,
    playlist_selection: Option<usize>,
    temp_playlist_until: Option<f64>,
    temp_playlist_hidden: bool,
    /// The modal settings window.
    settings_open: bool,
    /// True for the frame in which the dialog was opened, so the click that
    /// opened it cannot immediately close it through the backdrop.
    settings_just_opened: bool,
    /// Which settings page is selected (0 = General, 1 = Text,
    /// 2 = SoundFont, 3 = Player, 4 = Background, 5 = About).
    settings_section: usize,
    /// Which About popup is open, if any.
    about_popup: Option<AboutPopup>,
    /// Logo texture created on first use by the About page.
    about_logo: Option<egui::TextureHandle>,
    /// Set when a control changes; settings are written shortly afterwards.
    settings_save_at: Option<f64>,
    /// Debounce for rebuilding the audio stream when the buffer size changes.
    buffer_rebuild_at: Option<f64>,
    /// True when the external lyric font loaded successfully.
    lyric_font_loaded: bool,
    /// Set when the Font "..." button is clicked.
    lyric_font_browse: bool,
    /// `egui` time of the first clock frame; drives the boot animation.
    clock_anim_start: Option<f64>,
    /// Clicks on the clock in the current series (Easter egg counter).
    clock_clicks: u32,
    /// Time of the last clock click, used to reset the series when idle.
    clock_last_click: f64,
    /// While set, the clock shows the development start date instead.
    clock_easter_egg_until: Option<f64>,
    /// Time at which the next playlist entry should start (musical interval).
    next_song_at: Option<f64>,
    /// True while the "exit player?" confirmation is visible.
    exit_confirm_open: bool,
    /// True for the frame the confirmation was opened, so the click that
    /// opened it cannot dismiss it immediately.
    exit_confirm_just_opened: bool,
    /// Set once the user confirms; lets the pending close through.
    exit_confirmed: bool,
    /// Fade-out state for the settings modal.
    settings_closing: bool,
    settings_fade: f32,
    /// Fade-out state for the exit confirmation.
    exit_confirm_fade: f32,
    exit_confirm_closing: bool,
    /// The currently open custom context menu.
    menu: Option<OpenMenu>,
    /// The native file dialog currently running on a worker thread.
    pending_dialog: Option<PendingDialog>,
}

impl Default for KaraokeApp {
    fn default() -> Self {
        Self {
            song: None,
            path: None,
            playback: None,
            sound_font: None,
            sound_font_path: None,
            volume: 0.7,
            transpose: 0,
            paused: true,
            scrubbing: false,
            scrub_value: 0.0,
            error: None,
            settings: Settings::default(),
            playlist: Vec::new(),
            playlist_index: usize::MAX,
            titles: std::collections::HashMap::new(),
            indicator: None,
            song_card_pending: false,
            song_card_until: None,
            playlist_rect: Rect::NOTHING,
            last_key_shortcut: 0.0,
            last_bpm_shortcut: 0.0,
            bpm: 120.0,
            applied_speed: 1.0,
            bar_seek_anchor: None,
            meter: MeterMap::new(96, Vec::new()),
            song_end_us: 0,
            playlist_selection: None,
            temp_playlist_until: None,
            temp_playlist_hidden: false,
            settings_open: false,
            settings_just_opened: false,
            settings_section: 0,
            about_popup: None,
            about_logo: None,
            settings_save_at: None,
            buffer_rebuild_at: None,
            lyric_font_loaded: false,
            lyric_font_browse: false,
            clock_anim_start: None,
            clock_clicks: 0,
            clock_last_click: 0.0,
            clock_easter_egg_until: None,
            next_song_at: None,
            exit_confirm_open: false,
            exit_confirm_just_opened: false,
            exit_confirmed: false,
            settings_closing: false,
            settings_fade: 1.0,
            exit_confirm_fade: 1.0,
            exit_confirm_closing: false,
            menu: None,
            pending_dialog: None,
        }
    }
}

pub fn run(initial: Vec<PathBuf>) -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../logo/DontreeCrab_icon.png"))
        .expect("valid DontreeCrab icon");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icon)
            .with_inner_size([1024.0, 640.0])
            // Keep the default size as the minimum: the top bar, lyrics and
            // transport need this much room and the window cannot shrink
            // below it.
            .with_min_inner_size([1024.0, 640.0])
            .with_title("DontreeCrab"),
        ..Default::default()
    };

    eframe::run_native(
        "DontreeCrab",
        options,
        Box::new(move |cc| Ok(Box::new(KaraokeApp::new(cc, initial)))),
    )
}

impl KaraokeApp {
    fn new(cc: &eframe::CreationContext<'_>, initial: Vec<PathBuf>) -> Self {
        install_style(&cc.egui_ctx);

        let mut app = Self::default();
        app.settings = Settings::load();
        i18n::init();
        i18n::set_language(&app.settings.language);
        app.lyric_font_loaded = install_fonts(&cc.egui_ctx, app.settings.lyric_font.as_deref());
        app.volume = app.settings.volume;
        app.transpose = app.settings.transpose;

        let stored_font = app.settings.soundfont.clone().filter(|path| path.is_file());
        app.reload_sound_font(stored_font);

        // Nothing is auto-played and the playlist always starts empty.
        // Only files passed on the command line (or by a file association)
        // are opened; the first one plays and the rest join the playlist.
        if let Some((first, rest)) = initial.split_first() {
            app.open(first.clone());
            let _ = app.extend_playlist(rest.to_vec());
        }

        app
    }

    fn save_settings(&mut self) {
        self.settings.volume = self.volume;
        self.settings.transpose = self.transpose;
        // A temporary arrow-key reveal must not become the persisted state.
        let mut settings = self.settings.clone();
        if self.temp_playlist_hidden {
            settings.show_playlist = false;
        }
        if let Err(err) = settings.save() {
            self.error = Some(format!("settings: {err:#}"));
        }
    }

    fn position_us(&self) -> u64 {
        self.playback
            .as_ref()
            .map(|p| p.position.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// User adjustment in BPM, relative to the song's own starting tempo.
    fn bpm_adjust(&self) -> i32 {
        self.song
            .as_ref()
            .map(|song| (self.bpm - song.bpm).round() as i32)
            .unwrap_or(0)
    }

    /// The song's own tempo at the current playhead, before the user's
    /// adjustment.
    fn tempo_bpm_at_position(&self) -> f64 {
        match &self.song {
            Some(song) => {
                let tick = song.tempo_map.us_to_tick(self.position_us());
                song.tempo_map.bpm_at(tick)
            }
            None => self.bpm,
        }
    }

    /// Tempo actually being heard: the tempo-map value at the playhead plus
    /// the user's BPM adjustment.
    fn current_bpm(&self) -> f64 {
        adjusted_bpm(self.tempo_bpm_at_position(), self.bpm_adjust())
    }

    /// Keep the audio speed in sync with tempo changes. The user adjustment
    /// is a constant BPM offset, so a song that moves from 90 to 132 plays at
    /// `90 + n` and later `132 + n`.
    fn update_playback_speed(&mut self) {
        if self.song.is_none() || self.playback.is_none() {
            return;
        }

        let base = self.tempo_bpm_at_position().max(1.0);
        let speed = (self.current_bpm() / base).clamp(0.1, 10.0);
        if (speed - self.applied_speed).abs() <= 0.0001 {
            return;
        }

        if let Some(playback) = &self.playback {
            let _ = playback.commands.send(Command::SetSpeed(speed));
        }
        self.applied_speed = speed;
    }

    fn reload_sound_font(&mut self, explicit: Option<PathBuf>) {
        self.sound_font = None;
        self.sound_font_path = None;

        let resolved = match soundfont::resolve(explicit.as_deref()) {
            Ok(path) => std::path::absolute(&path).unwrap_or(path),
            Err(err) => {
                self.error = Some(format!("{err:#}"));
                return;
            }
        };

        match soundfont::load(&resolved) {
            Ok(font) => {
                self.sound_font = Some(font);
                self.sound_font_path = Some(resolved);
                if explicit.is_some() {
                    self.settings.soundfont = self.sound_font_path.clone();
                    self.save_settings();
                }
            }
            Err(err) => self.error = Some(format!("{err:#}")),
        }
    }

    // ---------------------------------------------------------------- loading

    fn open(&mut self, path: PathBuf) {
        // Any explicit/open-next action cancels a scheduled interval.
        self.next_song_at = None;
        // Store absolute paths so the playlist still works after the working
        // directory changes (e.g. relaunching from another folder).
        let path = std::path::absolute(&path).unwrap_or(path);
        self.error = None;
        // Dropping the handle stops the previous song.
        self.playback = None;
        self.scrubbing = false;
        self.paused = true;
        self.bar_seek_anchor = None;

        let song = match load(&path, &LoadOptions::default()) {
            Ok(song) => song,
            Err(err) => {
                self.error = Some(format!("{err:#}"));
                return;
            }
        };

        // Every song starts with its own tempo and no transposition.
        self.transpose = 0;
        self.bpm = song.bpm.clamp(20.0, 200.0);
        self.meter = MeterMap::new(song.ticks_per_quarter, song.time_signatures.clone());
        // Like the reference player, stop on the bar line after the last
        // musical event instead of playing an arbitrary silence tail.
        let end_tick = song.tempo_map.us_to_tick(song.duration_us);
        self.song_end_us = song.tempo_map.tick_to_us(self.meter.bar_end_tick(end_tick));

        if let Err(err) = self.start_audio(&song) {
            self.error = Some(format!("audio: {err:#}"));
        }

        self.song = Some(song);
        self.song_card_pending = true;

        if let Some(song) = &self.song {
            let title = song.title.clone().unwrap_or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            let file_type = match song.format {
                karaoke_player::SourceFormat::Kar => "kar",
                karaoke_player::SourceFormat::Ncn => "ncn",
                karaoke_player::SourceFormat::Kmid => "kmid",
                karaoke_player::SourceFormat::Midi => "midi",
            };
            self.titles.insert(
                path.clone(),
                SongInfo {
                    title,
                    artist: song.artist.clone().unwrap_or_default(),
                    file_type: file_type.to_owned(),
                },
            );
        }

        if !self.playlist.iter().any(|item| item == &path) {
            self.playlist.push(path.clone());
        }
        self.playlist_index = self
            .playlist
            .iter()
            .position(|item| item == &path)
            .unwrap_or(usize::MAX);
        self.playlist_selection = Some(self.playlist_index);

        self.path = Some(path);
        self.save_settings();
    }

    fn start_audio(&mut self, song: &Song) -> Result<()> {
        let Some(font) = self.sound_font.clone() else {
            anyhow::bail!(tr("No SoundFont loaded"));
        };

        let position = Arc::new(AtomicU64::new(0));
        let transpose = self.transpose;
        let volume = self.volume;
        let melo_muted = self.settings.melo_muted;
        let buffer_ms = self.settings.sf2_buffer_ms;
        let max_polyphony = self.settings.max_polyphony as usize;
        let eq_enabled = self.settings.eq_enabled;
        let eq_low = self.settings.eq_low_db;
        let eq_mid = self.settings.eq_mid_db;
        let eq_high = self.settings.eq_high_db;
        let midi_effects = self.settings.midi_effects;
        let rev_inst = self.settings.rev_inst;
        let cho_inst = self.settings.cho_inst;
        let rev_drum = self.settings.rev_drum;
        let cho_drum = self.settings.cho_drum;
        let speed = if song.bpm > 1.0 {
            (self.bpm / song.bpm).clamp(0.1, 10.0)
        } else {
            1.0
        };

        let handle = audio::start(
            |rate| {
                let mut player =
                    Player::new_with_polyphony(song, font.clone(), rate, transpose, max_polyphony)?;
                player.set_volume(volume);
                player.set_channel_muted(MELODY_CHANNEL, melo_muted);
                player.set_speed(speed);
                player.set_eq(eq_enabled, eq_low, eq_mid, eq_high);
                player.set_effect_sends(midi_effects, rev_inst, cho_inst, rev_drum, cho_drum);
                Ok(player)
            },
            position.clone(),
            buffer_ms,
        )?;

        if buffer_ms > 0 && handle.buffer_frames.is_none() {
            self.error = Some(tr_args(
                "SoundFont buffer of {} ms is not supported; using the driver default",
                &[&buffer_ms as &dyn std::fmt::Display],
            ));
        }

        let commands = handle.commands.clone();
        let _ = commands.send(Command::Play);
        self.playback = Some(Playback {
            _handle: handle,
            commands,
            position,
        });
        self.applied_speed = speed;
        self.paused = false;
        Ok(())
    }

    /// Rebuild the audio stream (new SoundFont or buffer size) while keeping
    /// the current position and paused state.
    fn rebuild_audio(&mut self) {
        let Some(song) = self.song.clone() else {
            return;
        };
        let was_paused = self.paused;
        let position = self.position_us();

        self.playback = None;
        if let Err(err) = self.start_audio(&song) {
            self.error = Some(format!("audio: {err:#}"));
            return;
        }

        if let Some(playback) = &self.playback {
            if position > 0 {
                let _ = playback.commands.send(Command::Seek(position));
            }
            if was_paused {
                let _ = playback.commands.send(Command::Pause);
            }
        }
        self.paused = was_paused;
    }

    /// Send the current equalizer settings to the audio thread.
    fn send_eq(&self) {
        if let Some(playback) = &self.playback {
            let _ = playback.commands.send(Command::SetEq {
                enabled: self.settings.eq_enabled,
                low_db: self.settings.eq_low_db,
                mid_db: self.settings.eq_mid_db,
                high_db: self.settings.eq_high_db,
            });
        }
    }

    /// Send the current reverb/chorus send multipliers to the audio thread.
    fn send_effects(&self) {
        if let Some(playback) = &self.playback {
            let _ = playback.commands.send(Command::SetEffectSends {
                midi_effects: self.settings.midi_effects,
                rev_inst: self.settings.rev_inst,
                cho_inst: self.settings.cho_inst,
                rev_drum: self.settings.rev_drum,
                cho_drum: self.settings.cho_drum,
            });
        }
    }

    fn open_dialog(&mut self) {
        self.start_dialog(DialogKind::Open);
    }

    /// Run a native dialog on a worker thread. `poll_dialog` picks the result
    /// up on a later frame, so the player never freezes while it is open.
    fn start_dialog(&mut self, kind: DialogKind) {
        if self.pending_dialog.is_some() {
            return;
        }

        let (sender, receiver) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("file-dialog".to_owned())
            .spawn(move || {
                let _ = sender.send(run_dialog(kind));
            });
        match spawned {
            Ok(_) => {
                self.pending_dialog = Some(PendingDialog { kind, receiver });
            }
            Err(err) => self.error = Some(format!("dialog: {err}")),
        }
    }

    /// Check whether a worker-thread dialog has finished and apply its result.
    fn poll_dialog(&mut self, ctx: &egui::Context) {
        let outcome = match self.pending_dialog.as_ref() {
            None => return,
            Some(pending) => match pending.receiver.try_recv() {
                Ok(result) => Some((pending.kind, result)),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            },
        };

        let Some((kind, result)) = outcome else {
            self.pending_dialog = None;
            return;
        };
        self.pending_dialog = None;
        self.finish_dialog(kind, result, ctx);
    }

    fn finish_dialog(
        &mut self,
        kind: DialogKind,
        result: Option<Vec<PathBuf>>,
        ctx: &egui::Context,
    ) {
        let Some(mut paths) = result else {
            // Cancelling the lyric font picker falls back to the built-in
            // font when no external font is configured yet.
            if matches!(kind, DialogKind::LyricFont) && self.settings.lyric_font.is_none() {
                self.settings.lyric_font_style = FontStyle::Regular;
            }
            return;
        };

        match kind {
            DialogKind::Open => {
                if let Some(path) = paths.pop() {
                    if is_playlist_json(&path) {
                        let added = self.import_playlist_file(&path);
                        self.play_first_added(added);
                    } else {
                        self.open(path);
                    }
                }
            }
            DialogKind::AddFiles => {
                let added = self.extend_playlist(paths);
                self.play_first_added(added);
            }
            DialogKind::AddFolder => {
                if let Some(folder) = paths.pop() {
                    let mut found = Vec::new();
                    collect_songs(&folder, 0, &mut found);
                    found.sort();
                    let added = self.extend_playlist(found);
                    self.play_first_added(added);
                }
            }
            DialogKind::ImportPlaylist => {
                if let Some(source) = paths.pop() {
                    let added = self.import_playlist_file(&source);
                    self.play_first_added(added);
                }
            }
            DialogKind::ExportPlaylist => {
                if let Some(target) = paths.pop() {
                    self.save_playlist_to(&target);
                }
            }
            DialogKind::SoundFont => {
                if let Some(path) = paths.pop() {
                    self.reload_sound_font(Some(path));
                    self.rebuild_audio();
                    self.settings_save_at = None;
                }
            }
            DialogKind::LyricFont => {
                if let Some(path) = paths.pop() {
                    self.settings.lyric_font = Some(path);
                    self.settings.lyric_font_style = FontStyle::Custom;
                    self.lyric_font_loaded =
                        install_fonts(ctx, self.settings.lyric_font.as_deref());
                    self.save_settings();
                }
            }
        }
    }

    // --------------------------------------------------------------- playlist

    fn add_files(&mut self) {
        self.start_dialog(DialogKind::AddFiles);
    }

    fn add_folder(&mut self) {
        self.start_dialog(DialogKind::AddFolder);
    }

    fn extend_playlist(&mut self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut added = Vec::new();
        for path in paths {
            let path = std::path::absolute(&path).unwrap_or(path);
            if path.is_file() && !self.playlist.contains(&path) {
                self.playlist.push(path.clone());
                added.push(path);
            }
        }
        self.save_settings();
        added
    }

    /// When "Play immediately" is enabled, start the first new playlist entry.
    fn play_first_added(&mut self, added: Vec<PathBuf>) {
        if self.settings.play_immediately {
            if let Some(path) = added.into_iter().next() {
                self.open(path);
            }
        }
    }

    /// Metadata for one playlist entry, loading it on demand for export.
    fn song_info(&self, path: &Path) -> SongInfo {
        if let Some(info) = self.titles.get(path) {
            return info.clone();
        }

        if let Ok(song) = load(path, &LoadOptions::default()) {
            let title = song.title.clone().unwrap_or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            let file_type = match song.format {
                karaoke_player::SourceFormat::Kar => "kar",
                karaoke_player::SourceFormat::Ncn => "ncn",
                karaoke_player::SourceFormat::Kmid => "kmid",
                karaoke_player::SourceFormat::Midi => "midi",
            };
            return SongInfo {
                title,
                artist: song.artist.clone().unwrap_or_default(),
                file_type: file_type.to_owned(),
            };
        }

        SongInfo {
            title: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_default(),
            artist: String::new(),
            file_type: file_type_guess(path),
        }
    }

    fn export_playlist(&mut self) {
        self.start_dialog(DialogKind::ExportPlaylist);
    }

    fn save_playlist_to(&mut self, target: &Path) {
        let songs = self
            .playlist
            .iter()
            .map(|path| {
                let info = self.song_info(path);
                PlaylistEntry {
                    path: path.clone(),
                    file_type: info.file_type,
                    title: info.title,
                    artist: info.artist,
                }
            })
            .collect();

        match PlaylistFile::new(songs).save(target) {
            Ok(()) => self.error = None,
            Err(err) => self.error = Some(format!("export: {err:#}")),
        }
    }

    fn import_playlist(&mut self) {
        self.start_dialog(DialogKind::ImportPlaylist);
    }

    fn import_playlist_file(&mut self, source: &Path) -> Vec<PathBuf> {
        let mut added = Vec::new();
        match PlaylistFile::load(source) {
            Ok(file) => {
                for entry in file.songs {
                    let path = std::path::absolute(&entry.path).unwrap_or(entry.path);
                    if !path.is_file() {
                        continue;
                    }
                    if !self.playlist.contains(&path) {
                        self.playlist.push(path.clone());
                        added.push(path.clone());
                    }
                    self.titles.entry(path).or_insert(SongInfo {
                        title: entry.title,
                        artist: entry.artist,
                        file_type: entry.file_type,
                    });
                }
            }
            Err(err) => self.error = Some(format!("import: {err:#}")),
        }
        added
    }

    /// Arrow-key navigation: reveal the playlist temporarily and move the
    /// highlight without starting playback.
    fn playlist_step(&mut self, delta: isize, ctx: &egui::Context) {
        let len = self.playlist.len();

        // Browsing the playlist dismisses the song card.
        self.song_card_until = None;

        // The panel should appear even when the playlist is empty, so the
        // user can right-click it to add songs.
        let was_hidden = !self.settings.show_playlist;
        if was_hidden {
            self.settings.show_playlist = true;
            self.temp_playlist_hidden = true;
        }
        self.temp_playlist_until =
            Some(ctx.input(|i| i.time) + self.settings.playlist_seconds as f64);

        // The first press only brings the layout on screen; the next one
        // starts moving the highlight.
        if was_hidden {
            if len == 0 {
                self.playlist_selection = None;
            } else if self.playlist_selection.is_none() {
                // Nothing is highlighted yet (e.g. right after importing a
                // JSON playlist): start on the playing entry, or the first
                // one when no song is loaded, so Enter works immediately.
                self.playlist_selection = Some(if self.playlist_index < len {
                    self.playlist_index
                } else {
                    0
                });
            }
            return;
        }

        if len == 0 {
            self.playlist_selection = None;
            return;
        }

        let current = match self.playlist_selection {
            Some(selection) => selection,
            None => {
                if self.playlist_index < len {
                    self.playlist_index
                } else {
                    // Nothing is playing: the first navigation selects the
                    // first entry instead of skipping it.
                    self.playlist_selection = Some(0);
                    return;
                }
            }
        };
        let next = (current as isize + delta).clamp(0, len as isize - 1) as usize;
        self.playlist_selection = Some(next);
    }

    /// Remove one entry from the playlist and keep the selection/index valid.
    fn remove_playlist_entry(&mut self, index: usize) {
        if index >= self.playlist.len() {
            return;
        }
        self.playlist.remove(index);

        if index == self.playlist_index {
            self.playlist_index = usize::MAX;
        } else if index < self.playlist_index {
            self.playlist_index -= 1;
        }

        self.playlist_selection = match self.playlist_selection {
            Some(selected) if selected == index => {
                if self.playlist.is_empty() {
                    None
                } else {
                    Some(index.min(self.playlist.len() - 1))
                }
            }
            Some(selected) if selected > index => Some(selected - 1),
            other => other,
        };

        self.save_settings();
    }

    /// Hide the playlist, whether it was opened permanently or revealed
    /// temporarily by the arrow keys.
    fn close_playlist(&mut self) {
        if self.settings.show_playlist {
            self.settings.show_playlist = false;
            self.temp_playlist_until = None;
            self.temp_playlist_hidden = false;
            self.playlist_selection = None;
            self.save_settings();
        }
    }

    fn advance(&mut self, delta: isize) {
        let len = self.playlist.len();
        if len == 0 {
            return;
        }

        let current = if self.playlist_index < len {
            self.playlist_index as isize
        } else {
            -1
        };
        let mut next = current + delta;

        if self.settings.repeat {
            next = next.rem_euclid(len as isize);
        } else if next < 0 || next >= len as isize {
            self.paused = true;
            // End of the playlist: reset the transport so the time and the
            // bar/beat display go back to the beginning.
            if let Some(playback) = &self.playback {
                let _ = playback.commands.send(Command::Stop);
            }
            // Moving past the last entry means nothing is playing any more;
            // clear the finished song so its info leaves the top bar.
            if delta > 0 {
                self.clear_played_song();
            }
            return;
        }

        let path = self.playlist[next as usize].clone();
        self.open(path);
    }

    /// Unload the finished song at the end of the playlist while keeping the
    /// playlist itself intact.
    fn clear_played_song(&mut self) {
        self.playback = None;
        self.song = None;
        self.path = None;
        self.playlist_index = usize::MAX;
        self.playlist_selection = None;
        self.paused = true;
        self.scrubbing = false;
        self.next_song_at = None;
        self.bar_seek_anchor = None;
        self.song_card_pending = false;
        self.song_card_until = None;
        self.song_end_us = 0;
    }

    /// Start the next entry after a song finishes, optionally deleting the
    /// entry that just finished.
    fn on_song_finished(&mut self) {
        self.paused = true;

        if self.settings.delete_played_songs && !self.settings.repeat {
            if self.playlist_index < self.playlist.len() {
                let index = self.playlist_index;
                self.playlist.remove(index);
                if index < self.playlist.len() {
                    let path = self.playlist[index].clone();
                    self.open(path);
                    return;
                }
                self.playlist_index = usize::MAX;
                self.playlist_selection = None;
                if let Some(playback) = &self.playback {
                    let _ = playback.commands.send(Command::Stop);
                }
                self.clear_played_song();
                return;
            }
        }

        self.advance(1);
    }

    fn playlist_panel(&mut self, ui: &mut egui::Ui) {
        let mut menu_request: Option<(MenuKind, Pos2)> = None;

        let panel = egui::Panel::left("playlist")
            .resizable(true)
            .default_size(230.0)
            .min_size(170.0)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_black_alpha(165))
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(ui, |ui| {
                // Right-clicking anywhere in the panel opens this menu.
                let background = ui.interact(
                    ui.max_rect(),
                    ui.scope_id().with("playlist_bg"),
                    egui::Sense::click(),
                );

                ui.horizontal(|ui| {
                    if ui
                        .button(tr("Prev"))
                        .on_hover_text(tr("Previous playlist entry"))
                        .clicked()
                    {
                        self.advance(-1);
                    }
                    if ui
                        .button(tr("Next"))
                        .on_hover_text(tr("Next playlist entry"))
                        .clicked()
                    {
                        self.advance(1);
                    }
                    let repeat = self.settings.repeat;
                    if ui
                        .selectable_label(repeat, tr("Repeat"))
                        .on_hover_text(tr("Repeat playlist (Ctrl+R)"))
                        .clicked()
                    {
                        self.settings.repeat = !repeat;
                        self.save_settings();
                    }
                    if ui.button(tr("Clear")).clicked() {
                        self.clear_playlist();
                    }
                });

                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.playlist.is_empty() {
                        ui.label(tr("Playlist is empty\nright-click to add songs"));
                    }

                    let mut clicked = None;

                    for index in 0..self.playlist.len() {
                        let path = &self.playlist[index];
                        let name = match self.titles.get(path) {
                            Some(info) if !info.artist.is_empty() => {
                                format!("{} — {}", info.title, info.artist)
                            }
                            Some(info) => info.title.clone(),
                            None => path
                                .file_stem()
                                .map(|stem| stem.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.display().to_string()),
                        };
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 26.0),
                            egui::Sense::click(),
                        );

                        let highlighted = self.playlist_selection.unwrap_or(self.playlist_index);
                        let selected = index == highlighted;
                        if selected || response.hovered() {
                            let fill = if selected {
                                Color32::from_gray(76)
                            } else {
                                Color32::from_gray(36)
                            };
                            ui.painter().rect_filled(rect, 4.0, fill);
                        }
                        ui.painter().text(
                            rect.left_center() + egui::vec2(8.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &name,
                            FontId::proportional(14.0),
                            FG,
                        );
                        let response = response.on_hover_text(path.display().to_string());

                        if response.clicked() {
                            clicked = Some(index);
                        }

                        if response.secondary_clicked() {
                            if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                                menu_request = Some((MenuKind::Row(index), pos));
                            }
                        }
                    }

                    if let Some(index) = clicked {
                        let path = self.playlist[index].clone();
                        self.open(path);
                    }
                });

                if background.secondary_clicked() {
                    if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                        menu_request = Some((MenuKind::Background, pos));
                    }
                }
            });

        // Remember where the panel is so the wheel can scroll it instead of
        // changing the volume.
        self.playlist_rect = panel.response.rect;

        if let Some((kind, pos)) = menu_request {
            self.open_menu(kind, pos);
        }
    }

    fn open_settings(&mut self) {
        self.settings_open = true;
        self.settings_just_opened = true;
        self.settings_section = 0;
        self.about_popup = None;
        self.settings_closing = false;
        self.settings_fade = 1.0;
    }

    fn pick_sound_font(&mut self) {
        self.start_dialog(DialogKind::SoundFont);
    }

    /// Choose an external font for the karaoke lyrics (the UI never changes).
    fn pick_lyric_font(&mut self) {
        self.start_dialog(DialogKind::LyricFont);
    }

    // ---------------------------------------------------------------- settings

    fn settings_modal(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut browse = false;
        let mut eq_changed = false;
        let mut sends_changed = false;
        let mut buffer_changed = false;
        let mut text_changed = false;

        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        if self.settings_closing {
            self.settings_fade = (self.settings_fade - dt / 0.15).max(0.0);
            if self.settings_fade <= 0.001 {
                self.settings_open = false;
                self.settings_closing = false;
                self.settings_fade = 1.0;
                return;
            }
        }
        let fade = self.settings_fade;

        // The Background page (colour picker plus the 4:3 and 16:9 previews)
        // is the tallest page, so its content plus the fixed footer decides
        // the size of the whole settings dialog. Every page uses the same
        // frame and keeps Close pinned to the bottom-right corner.
        const PAGE_WIDTH: f32 = 564.0;
        const PAGE_HEIGHT: f32 = 400.0;
        let modal_size = egui::vec2(
            148.0 + 1.0 + PAGE_WIDTH + 22.0 + 22.0,
            PAGE_HEIGHT + 16.0 + 14.0,
        );
        let modal_pos = ctx.content_rect().center() - modal_size * 0.5;
        let modal_area = egui::Area::new(egui::Id::unique("settings_modal"))
            .kind(egui::UiKind::Modal)
            .sense(egui::Sense::hover())
            .fixed_pos(modal_pos)
            .order(egui::Order::Foreground)
            .interactable(true);
        let modal = egui::Modal::new(egui::Id::unique("settings_modal"))
            .area(modal_area)
            .backdrop_color(Color32::from_black_alpha((100.0 * fade) as u8))
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(
                        8,
                        8,
                        8,
                        (255.0 * fade) as u8,
                    ))
                    .stroke(egui::Stroke::new(
                        1.5,
                        Color32::from_rgba_unmultiplied(120, 120, 120, (255.0 * fade) as u8),
                    ))
                    .corner_radius(egui::CornerRadius::same(12)),
            )
            .show(ctx, |ui| {
                ui.set_min_size(modal_size);
                ui.set_max_size(modal_size);
                ui.set_opacity(fade);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                    // Left navigation column: reserved for future sections.
                    ui.allocate_ui_with_layout(
                        egui::vec2(148.0, modal_size.y),
                        Layout::top_down(Align::Min),
                        |ui| {
                            let rect = ui.max_rect();
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius {
                                    nw: 12,
                                    sw: 12,
                                    ne: 0,
                                    se: 0,
                                },
                                Color32::from_gray(16),
                            );
                            ui.add_space(16.0);
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(RichText::new(tr("SETTINGS")).size(10.0).color(DIM));
                            });
                            ui.add_space(10.0);
                            if nav_row(
                                ui,
                                &tr("General"),
                                self.settings_section == 0,
                                Icon::Settings,
                            )
                            .clicked()
                            {
                                self.settings_section = 0;
                            }
                            if nav_row(ui, &tr("Text"), self.settings_section == 1, Icon::Type)
                                .clicked()
                            {
                                self.settings_section = 1;
                            }
                            if nav_row(
                                ui,
                                &tr("SoundFont"),
                                self.settings_section == 2,
                                Icon::AudioLines,
                            )
                            .clicked()
                            {
                                self.settings_section = 2;
                            }
                            if nav_row(ui, &tr("Player"), self.settings_section == 3, Icon::Play)
                                .clicked()
                            {
                                self.settings_section = 3;
                            }
                            if nav_row(
                                ui,
                                &tr("Background"),
                                self.settings_section == 4,
                                Icon::Image,
                            )
                            .clicked()
                            {
                                self.settings_section = 4;
                            }

                            // About is pinned to the bottom of the column.
                            let remaining = ui.available_height() - 44.0;
                            if remaining > 0.0 {
                                ui.add_space(remaining);
                            }
                            if nav_row(ui, &tr("About"), self.settings_section == 5, Icon::Info)
                                .clicked()
                            {
                                self.settings_section = 5;
                            }
                        },
                    );

                    let (divider, _) =
                        ui.allocate_exact_size(egui::vec2(1.0, modal_size.y), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(divider, 0.0, Color32::from_gray(70));

                    // Main page: SoundFont.
                    egui::Frame::NONE
                        .inner_margin(egui::Margin {
                            left: 22,
                            right: 22,
                            top: 16,
                            bottom: 14,
                        })
                        .show(ui, |ui| {
                            // Frame inherits the parent's layout; force a
                            // vertical stack for the page content.
                            ui.set_min_size(egui::vec2(PAGE_WIDTH, PAGE_HEIGHT));
                            ui.set_max_size(egui::vec2(PAGE_WIDTH, PAGE_HEIGHT));
                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);

                                if self.settings_section == 2 {
                                    ui.add_space(14.0);
                                    let button_width = 36.0f32;
                                    let field_width =
                                        (ui.available_width() - button_width - 10.0 - 48.0)
                                            .max(120.0);
                                    let row_width = field_width + 10.0 + button_width;
                                    let left_offset =
                                        ((ui.available_width() - row_width) * 0.5).max(0.0);

                                    // The title sits at the top-right corner of the
                                    // SoundFont box.
                                    let (heading, _) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), 16.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().text(
                                        Pos2::new(heading.left() + left_offset, heading.top()),
                                        egui::Align2::LEFT_TOP,
                                        tr("Currently active SoundFont"),
                                        FontId::proportional(13.0),
                                        FG,
                                    );
                                    ui.add_space(2.0);
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 0.0;
                                        // Match the browse button height to the field.
                                        ui.spacing_mut().button_padding.y = 4.0;
                                        ui.spacing_mut().interact_size.y = 16.0;
                                        ui.add_space(left_offset);
                                        let name = self
                                            .sound_font_path
                                            .as_ref()
                                            .and_then(|path| path.file_name())
                                            .map(|name| name.to_string_lossy().to_string())
                                            .unwrap_or_else(|| tr("No SoundFont"));
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(field_width, 25.0),
                                            egui::Sense::hover(),
                                        );
                                        let painter = ui.painter();
                                        painter.rect_filled(rect, 6.0, Color32::from_gray(26));
                                        painter.rect_stroke(
                                            rect,
                                            6.0,
                                            egui::Stroke::new(1.0, Color32::from_gray(64)),
                                            egui::StrokeKind::Inside,
                                        );
                                        painter
                                            .with_clip_rect(rect.shrink2(egui::vec2(8.0, 0.0)))
                                            .text(
                                                Pos2::new(rect.left() + 9.0, rect.center().y),
                                                egui::Align2::LEFT_CENTER,
                                                name,
                                                FontId::proportional(14.0),
                                                if self.sound_font_path.is_some() {
                                                    FG
                                                } else {
                                                    DIM
                                                },
                                            );
                                        let _ = response.on_hover_text(
                                            self.sound_font_path
                                                .as_ref()
                                                .map(|path| path.display().to_string())
                                                .unwrap_or_else(|| tr("No SoundFont loaded")),
                                        );

                                        ui.add_space(10.0);
                                        if ui
                                            .add_sized(
                                                [button_width, 24.0],
                                                egui::Button::new(Icon::Ellipsis.rich(16.0)),
                                            )
                                            .on_hover_text(tr("Browse for a SoundFont"))
                                            .clicked()
                                        {
                                            browse = true;
                                        }
                                    });

                                    ui.add_space(2.0);
                                    // The checkboxes sit on the same line as the buffer
                                    // field; only the buffer label is above it.
                                    ui.horizontal_top(|ui| {
                                        ui.vertical(|ui| {
                                            ui.add_space(25.0);
                                            ui.horizontal(|ui| {
                                                ui.add_space(24.0);
                                                if ui
                                                    .checkbox(
                                                        &mut self.settings.eq_enabled,
                                                        tr("Equalizer"),
                                                    )
                                                    .changed()
                                                {
                                                    eq_changed = true;
                                                }
                                                ui.add_space(44.0);
                                                if ui
                                                    .checkbox(
                                                        &mut self.settings.midi_effects,
                                                        tr("MIDI Effects"),
                                                    )
                                                    .changed()
                                                {
                                                    sends_changed = true;
                                                }
                                            });
                                        });

                                        // Right-aligned polyphony and buffer block.
                                        let remaining = ui.available_width();
                                        let block_width = 250.0;
                                        if remaining > block_width {
                                            ui.add_space(remaining - block_width);
                                        }
                                        ui.horizontal_top(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new(tr("Polyphony"))
                                                        .size(12.0)
                                                        .color(DIM),
                                                );
                                                ui.horizontal(|ui| {
                                                    let response = ui.add_sized(
                                                        [64.0, 24.0],
                                                        egui::DragValue::new(
                                                            &mut self.settings.max_polyphony,
                                                        )
                                                        .range(8..=256u32)
                                                        .speed(8.0),
                                                    );
                                                    if response.changed() {
                                                        buffer_changed = true;
                                                    }
                                                    ui.label(
                                                        RichText::new(tr("voices"))
                                                            .size(11.0)
                                                            .color(DIM),
                                                    );
                                                });
                                            });
                                            ui.add_space(18.0);
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new(tr("SoundFont Buffer"))
                                                        .size(12.0)
                                                        .color(DIM),
                                                );
                                                ui.horizontal(|ui| {
                                                    let response = ui
                                                        .add_sized(
                                                            [58.0, 24.0],
                                                            egui::DragValue::new(
                                                                &mut self.settings.sf2_buffer_ms,
                                                            )
                                                            .range(0..=100u32)
                                                            .speed(1.0),
                                                        )
                                                        .on_hover_text(tr(
                                                            "0 = auto, 10–100 ms manual",
                                                        ));
                                                    if response.changed() {
                                                        buffer_changed = true;
                                                    }
                                                    ui.label(
                                                        RichText::new("ms").size(11.0).color(DIM),
                                                    );
                                                });
                                            });
                                        });
                                    });

                                    ui.add_space(28.0);
                                    ui.spacing_mut().slider_width = 100.0;
                                    ui.spacing_mut().slider_rail_height = 3.0;
                                    ui.horizontal(|ui| {
                                        // Small leading gap, matching the reference.
                                        ui.add_space(15.0);

                                        // Equalizer.
                                        slider_group(ui, &tr("Equalizer"), 182.0, |ui| {
                                            slider_column(
                                                ui,
                                                "LOW",
                                                &mut self.settings.eq_low_db,
                                                -12.0..=12.0,
                                                0.0,
                                                &mut eq_changed,
                                            );
                                            slider_column(
                                                ui,
                                                "MID",
                                                &mut self.settings.eq_mid_db,
                                                -12.0..=12.0,
                                                0.0,
                                                &mut eq_changed,
                                            );
                                            slider_column(
                                                ui,
                                                "HIGH",
                                                &mut self.settings.eq_high_db,
                                                -12.0..=12.0,
                                                0.0,
                                                &mut eq_changed,
                                            );
                                        });

                                        ui.add_space(72.0);

                                        // Instrument sends.
                                        slider_group(ui, "INST", 118.0, |ui| {
                                            slider_column(
                                                ui,
                                                "REV",
                                                &mut self.settings.rev_inst,
                                                0.0..=1.0,
                                                0.5,
                                                &mut sends_changed,
                                            );
                                            slider_column(
                                                ui,
                                                "CHO",
                                                &mut self.settings.cho_inst,
                                                0.0..=1.0,
                                                0.5,
                                                &mut sends_changed,
                                            );
                                        });

                                        ui.add_space(20.0);

                                        // Drum (channel 10) sends.
                                        slider_group(ui, "DRUM", 118.0, |ui| {
                                            slider_column(
                                                ui,
                                                "REV",
                                                &mut self.settings.rev_drum,
                                                0.0..=1.0,
                                                0.5,
                                                &mut sends_changed,
                                            );
                                            slider_column(
                                                ui,
                                                "CHO",
                                                &mut self.settings.cho_drum,
                                                0.0..=1.0,
                                                0.5,
                                                &mut sends_changed,
                                            );
                                        });
                                    });
                                } else if self.settings_section == 1 {
                                    text_changed = self.text_page(ui);
                                } else if self.settings_section == 3 {
                                    text_changed |= self.player_page(ui);
                                } else if self.settings_section == 4 {
                                    text_changed |= self.background_page(ui);
                                } else if self.settings_section == 5 {
                                    self.about_page(ui);
                                } else {
                                    text_changed |= self.general_page(ui);
                                }

                                // Pin Close to the bottom-right corner of the
                                // page, independently of the section content.
                                let button_size = egui::vec2(88.0, 26.0);
                                let page_rect = ui.max_rect();
                                let button_rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        page_rect.right() - button_size.x,
                                        page_rect.bottom() - button_size.y - 14.0,
                                    ),
                                    button_size,
                                );
                                if ui
                                    .put(button_rect, egui::Button::new(tr("Close")))
                                    .clicked()
                                {
                                    close = true;
                                }
                            });
                        });
                });
            });

        let just_opened = std::mem::take(&mut self.settings_just_opened);
        if modal.should_close() && !just_opened && self.about_popup.is_none() {
            close = true;
        }

        let now = ctx.input(|i| i.time);

        if browse {
            self.pick_sound_font();
        }

        if self.lyric_font_browse {
            self.lyric_font_browse = false;
            self.pick_lyric_font();
            text_changed = true;
        }

        if eq_changed {
            self.send_eq();
            self.settings_save_at = Some(now + 0.6);
        }

        if sends_changed {
            self.send_effects();
            self.settings_save_at = Some(now + 0.6);
        }

        if buffer_changed {
            self.settings_save_at = Some(now + 0.6);
            self.buffer_rebuild_at = Some(now + 0.7);
        }

        if text_changed {
            self.settings_save_at = Some(now + 0.6);
        }

        if close {
            self.settings_closing = true;
            self.about_popup = None;
            self.settings_save_at = None;
            self.save_settings();
        }
    }

    /// The "Text" settings page: karaoke font size, edge, distance and colours.
    fn text_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.add_space(16.0);

        // Two equally sized columns. The explicit margins and gap keep the
        // block centred in the 564 px page and make every control line up.
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(0.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                changed |= percent_row(
                    ui,
                    &tr("Font size"),
                    &mut self.settings.font_scale,
                    40.0,
                    250.0,
                );
                changed |= percent_row(
                    ui,
                    &tr("Edge size"),
                    &mut self.settings.lyric_edge_size,
                    0.0,
                    200.0,
                );
                changed |= percent_row(
                    ui,
                    &tr("Distance"),
                    &mut self.settings.lyric_distance,
                    50.0,
                    300.0,
                );
            });
            ui.add_space(45.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                changed |= text_visibility_row(ui, &mut self.settings.text_visibility);
                changed |= cursor_visibility_row(ui, &mut self.settings.cursor_visibility);
                changed |= lyric_depth_row(ui, &mut self.settings.lyric_depth);
            });
            ui.add_space(32.0);
        });

        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(24.0);
            ui.label(RichText::new(tr("Font")).size(13.0));
        });
        ui.add_space(4.0);
        let font_name = match self.settings.lyric_font_style {
            FontStyle::Regular => "Sarabun-Regular.ttf".to_owned(),
            FontStyle::Bold => "Sarabun-Bold.ttf".to_owned(),
            FontStyle::Custom => self
                .settings
                .lyric_font
                .as_ref()
                .filter(|_| self.lyric_font_loaded)
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| tr("No external font")),
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(24.0);
            ui.spacing_mut().item_spacing.x = 10.0;

            // Keep the field, style combo and browse button the same height
            // as the "..." button.
            ui.spacing_mut().button_padding.y = 4.0;
            ui.spacing_mut().interact_size.y = 16.0;

            let style_width = 96.0f32;
            let button_width = 36.0f32;
            let row_width = 500.0f32;
            let field_width = (row_width - style_width - button_width - 20.0).max(160.0);
            display_field(ui, &font_name, field_width);

            egui::ComboBox::from_id_salt("lyric_font_style")
                .width(style_width)
                .selected_text(font_style_label(self.settings.lyric_font_style))
                .show_ui(ui, |ui| {
                    for style in [FontStyle::Regular, FontStyle::Bold, FontStyle::Custom] {
                        if ui
                            .selectable_value(
                                &mut self.settings.lyric_font_style,
                                style,
                                font_style_label(style),
                            )
                            .changed()
                        {
                            changed = true;
                            if style == FontStyle::Custom && !self.lyric_font_loaded {
                                self.lyric_font_browse = true;
                            }
                        }
                    }
                });

            if ui
                .add_sized(
                    [button_width, 24.0],
                    egui::Button::new(Icon::Ellipsis.rich(16.0)),
                )
                .on_hover_text(tr("Choose an external lyric font"))
                .clicked()
            {
                self.lyric_font_browse = true;
            }
        });

        ui.add_space(18.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(32.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                ui.label(RichText::new(tr("Lyric")).size(13.0));
                ui.add_space(6.0);
                changed |= color_row(
                    ui,
                    &tr("Text"),
                    &mut self.settings.lyric_text_color,
                    "#FFFFFF",
                );
                changed |= color_row(
                    ui,
                    &tr("Edge"),
                    &mut self.settings.lyric_edge_color,
                    "#000000",
                );
            });
            ui.add_space(40.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                ui.label(RichText::new(tr("Cursor")).size(13.0));
                ui.add_space(6.0);
                changed |= color_row(
                    ui,
                    &tr("Text"),
                    &mut self.settings.cursor_text_color,
                    "#0000FF",
                );
                changed |= color_row(
                    ui,
                    &tr("Edge"),
                    &mut self.settings.cursor_edge_color,
                    "#FFFFFF",
                );
            });
            ui.add_space(12.0);
        });

        changed
    }

    /// The "General" settings page: display durations and top-bar visibility.
    fn general_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.add_space(14.0);

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                ui.label(RichText::new(tr("Display Time")).size(13.0));
                ui.add_space(6.0);
                changed |= seconds_row(
                    ui,
                    &tr("Indicator"),
                    &mut self.settings.indicator_seconds,
                    60,
                );
                changed |=
                    seconds_row(ui, &tr("Playlist"), &mut self.settings.playlist_seconds, 60);

                ui.add_space(40.0);
                ui.label(RichText::new(tr("Language")).size(13.0));
                ui.add_space(6.0);
                let languages = i18n::available_languages();
                let auto = tr("System default");
                let selected_text = if self.settings.language == "auto" {
                    auto.clone()
                } else {
                    languages
                        .iter()
                        .find(|language| {
                            language.code.eq_ignore_ascii_case(&self.settings.language)
                        })
                        .map(|language| language.name.clone())
                        .unwrap_or_else(|| self.settings.language.clone())
                };
                let mut selected = self.settings.language.clone();
                egui::ComboBox::from_id_salt("ui_language")
                    .width(240.0)
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut selected, "auto".to_owned(), auto);
                        for language in &languages {
                            ui.selectable_value(
                                &mut selected,
                                language.code.clone(),
                                language.name.clone(),
                            );
                        }
                    });
                if selected != self.settings.language {
                    self.settings.language = selected;
                    i18n::set_language(&self.settings.language);
                    changed = true;
                }
            });
            ui.add_space(52.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                ui.label(RichText::new(tr("Setting")).size(13.0));
                ui.add_space(6.0);
                changed |= check_row(ui, &tr("Song info"), &mut self.settings.show_song_info);
                changed |= check_row(ui, &tr("Bar & beat"), &mut self.settings.show_bar_beat);
                changed |= check_row(ui, &tr("Clock"), &mut self.settings.show_clock);
                changed |= check_row(ui, &tr("Confirm exit"), &mut self.settings.confirm_exit);
            });
            ui.add_space(14.0);
        });

        changed
    }

    /// The "Player" settings page: playlist playback behaviour.
    fn player_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.add_space(14.0);

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                ui.label(RichText::new(tr("Playlist")).size(13.0));
                ui.add_space(6.0);
                changed |= check_row(
                    ui,
                    &tr("Delete played songs"),
                    &mut self.settings.delete_played_songs,
                );
                changed |= check_row(
                    ui,
                    &tr("Play immediately"),
                    &mut self.settings.play_immediately,
                );
                changed |= check_row(ui, &tr("Automatically play"), &mut self.settings.auto_play);
                ui.add_space(8.0);
                changed |= seconds_row(
                    ui,
                    &tr("Musical interval"),
                    &mut self.settings.music_interval_seconds,
                    10,
                );
            });
        });

        changed
    }

    /// The "Background" settings page: colour picker plus 4:3 / 16:9 previews.
    fn background_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.add_space(14.0);

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(220.0);
                ui.label(RichText::new(tr("Background Color")).size(13.0));
                ui.add_space(6.0);
                changed |= background_color_row(ui, &mut self.settings.background_color);
            });
            ui.add_space(24.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(268.0);
                ui.label(RichText::new(tr("Preview monitor")).size(13.0));
                ui.add_space(6.0);
                background_preview(ui, color_or(&self.settings.background_color, [0, 0, 0]));
            });
            ui.add_space(24.0);
        });

        changed
    }

    /// The About page: logo, version, composer, tools, GitHub link and two
    /// popups for the licence and credits.
    fn about_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                let texture = self.about_logo.get_or_insert_with(|| {
                    let icon = eframe::icon_data::from_png_bytes(include_bytes!(
                        "../logo/DontreeCrab_icon.png"
                    ))
                    .expect("valid DontreeCrab logo");
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [icon.width as usize, icon.height as usize],
                        &icon.rgba,
                    );
                    ui.ctx()
                        .load_texture("about_logo", image, egui::TextureOptions::LINEAR)
                });
                egui::Frame::NONE
                    .fill(Color32::WHITE)
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.add(egui::Image::new((texture.id(), egui::vec2(96.0, 96.0))));
                    });
                ui.add_space(8.0);
                about_github_link(ui);
            });
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("DontreeCrab").size(20.0).strong());
                ui.label(RichText::new(APP_VERSION_LABEL).size(12.0).color(DIM));
                ui.add_space(14.0);
                ui.label(
                    RichText::new(format!("{} :", tr("Software Composer")))
                        .size(13.0)
                        .strong(),
                );
                ui.label(RichText::new("PXCza").size(13.0));
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("{} :", tr("Tools Used")))
                        .size(13.0)
                        .strong(),
                );
                ui.label(RichText::new("DeepSeek V4.1 Flash 1017").size(13.0));
                ui.label(RichText::new("OpenCode 2.2.7").size(13.0));
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.vertical(|ui| {
                    if ui
                        .add_sized([120.0, 26.0], egui::Button::new(tr("License")))
                        .clicked()
                    {
                        self.about_popup = Some(AboutPopup::License);
                    }
                    if ui
                        .add_sized([120.0, 26.0], egui::Button::new(tr("Created")))
                        .clicked()
                    {
                        self.about_popup = Some(AboutPopup::Created);
                    }
                });
            });
        });

        if let Some(popup) = self.about_popup {
            if draw_about_popup(ui.ctx(), popup) {
                self.about_popup = None;
            }
        }
    }

    // -------------------------------------------------------------- transport

    fn toggle_play(&mut self) {
        let Some(playback) = &self.playback else {
            return;
        };
        if self.paused {
            let _ = playback.commands.send(Command::Play);
            self.paused = false;
        } else {
            let _ = playback.commands.send(Command::Pause);
            self.paused = true;
        }
    }

    fn stop(&mut self) {
        // Stop also unloads the song: the transport, bar/beat and Song info
        // all go back to the idle state.
        self.clear_played_song();
    }

    /// Move to the next or previous bar line and show `NEXT >>>` / `BACK <<<`.
    fn seek_bar(&mut self, forward: bool, ctx: &egui::Context) {
        let Some(song) = self.song.as_ref() else {
            return;
        };
        let Some(playback) = &self.playback else {
            return;
        };

        let now = ctx.input(|i| i.time);
        let tick = song.tempo_map.us_to_tick(self.position_us());
        // Rapid presses happen before the audio thread has moved the playhead,
        // so continue from the last requested bar for a short window.
        let base = match self.bar_seek_anchor {
            Some((anchor, at)) if now - at <= 1.0 => anchor,
            _ => tick,
        };
        let target_tick = if forward {
            self.meter.next_bar_tick(base)
        } else {
            self.meter.previous_bar_tick(base)
        };

        let target_us = song.tempo_map.tick_to_us(target_tick);
        let _ = playback.commands.send(Command::Seek(target_us));
        self.bar_seek_anchor = Some((target_tick, now));
        self.paused = false;
        self.show_indicator(
            if forward {
                tr("NEXT >>>")
            } else {
                tr("BACK <<<")
            },
            ctx,
        );
    }

    fn change_transpose(&mut self, delta: i32, ctx: &egui::Context) {
        self.transpose = (self.transpose + delta).clamp(-12, 12);
        if let Some(playback) = &self.playback {
            let _ = playback
                .commands
                .send(Command::SetTranspose(self.transpose));
        }

        let text = match self
            .song
            .as_ref()
            .and_then(|song| song.key.as_deref())
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            Some(key) => {
                format!("KEY : {}", key_with_offset(key, self.transpose))
            }
            None => format!("KEY [{}]", signed_offset(self.transpose)),
        };
        self.show_indicator(text, ctx);
        self.save_settings();
    }

    fn show_indicator(&mut self, text: String, ctx: &egui::Context) {
        self.indicator = Some(Indicator {
            text,
            until: ctx.input(|i| i.time) + self.settings.indicator_seconds as f64,
            bpm: false,
        });
    }

    /// Show the BPM readout; its text is rebuilt from the playhead each frame.
    fn show_bpm_indicator(&mut self, ctx: &egui::Context) {
        self.indicator = Some(Indicator {
            text: String::new(),
            until: ctx.input(|i| i.time) + self.settings.indicator_seconds as f64,
            bpm: true,
        });
    }

    // ------------------------------------------------------------------ input

    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            // Dropping files is an "add" action, so it follows the same
            // Play immediately setting as the Add files/folder dialogs.
            let mut added = Vec::new();
            for path in dropped {
                if is_playlist_json(&path) {
                    added.extend(self.import_playlist_file(&path));
                } else {
                    added.extend(self.extend_playlist(vec![path]));
                }
            }
            self.play_first_added(added);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            if self.playback.is_some() {
                self.toggle_play();
                let state = if self.paused { tr("PAUSE") } else { tr("PLAY") };
                self.show_indicator(state, ctx);
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F6)) {
            if self.playback.is_some() {
                self.stop();
                self.show_indicator(tr("STOP"), ctx);
            }
        }
        // Esc dismisses the song card first, then closes the playlist.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ctx) {
            let now = ctx.input(|i| i.time);
            let card_visible = !self.settings.show_song_info
                && self.song.is_some()
                && self.song_card_until.is_some_and(|until| now < until);
            if card_visible {
                self.song_card_until = None;
            } else {
                self.close_playlist();
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F7)) {
            self.seek_bar(false, ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F8)) {
            self.seek_bar(true, ctx);
        }
        // Space re-shows the song card when the top bar info is hidden. It is
        // reserved for the playlist while that panel is open.
        if ctx.input(|i| i.key_pressed(egui::Key::Space))
            && !self.settings.show_song_info
            && !self.settings.show_playlist
            && self.song.is_some()
        {
            let now = ctx.input(|i| i.time);
            self.song_card_until = Some(now + self.settings.indicator_seconds as f64);
        }
        // Up/down browse the playlist; Enter plays the highlighted entry.
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            self.playlist_step(-1, ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            self.playlist_step(1, ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && self.settings.show_playlist {
            if let Some(index) = self.playlist_selection {
                if let Some(path) = self.playlist.get(index).cloned() {
                    self.open(path);
                }
            }
        }
        // Delete removes the highlighted (or currently playing) entry.
        if ctx.input(|i| i.key_pressed(egui::Key::Delete)) && self.settings.show_playlist {
            let index = self
                .playlist_selection
                .filter(|&index| index < self.playlist.len())
                .or_else(|| {
                    (self.playlist_index < self.playlist.len()).then_some(self.playlist_index)
                });
            if let Some(index) = index {
                self.remove_playlist_entry(index);
            }
        }
        // Key +/-: Ctrl+, and Ctrl+.  A short debounce keeps duplicated key
        // events (some keyboard drivers send several) to one step per press.
        let now = ctx.input(|i| i.time);
        let comma = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Comma));
        let period = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Period));
        if (comma || period) && now - self.last_key_shortcut >= 0.12 {
            self.last_key_shortcut = now;
            // Ctrl+, lowers and Ctrl+. raises the key.
            self.change_transpose(if comma { -1 } else { 1 }, ctx);
        }

        // Ctrl+\ mutes / unmutes the melody track (MIDI channel 9).
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Backslash)) {
            self.settings.melo_muted = !self.settings.melo_muted;
            if let Some(playback) = &self.playback {
                let _ = playback.commands.send(Command::SetChannelMuted {
                    channel: MELODY_CHANNEL,
                    muted: self.settings.melo_muted,
                });
            }
            let state = if self.settings.melo_muted {
                tr("OFF")
            } else {
                tr("ON")
            };
            self.show_indicator(
                tr_args("MELO [{}]", &[&state as &dyn std::fmt::Display]),
                ctx,
            );
            self.save_settings();
        }

        // BPM: Ctrl+' faster, Ctrl+; slower (20..200, one step = 1 BPM).
        let bpm_up = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Quote));
        let bpm_down = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Semicolon));
        if (bpm_up || bpm_down) && now - self.last_bpm_shortcut >= 0.12 {
            self.last_bpm_shortcut = now;
            self.bpm = (self.bpm + if bpm_up { 1.0 } else { -1.0 }).clamp(20.0, 200.0);
            self.update_playback_speed();
            self.show_bpm_indicator(ctx);
        }

        // UI zoom is handled by egui itself (Ctrl+- / Ctrl++ / Ctrl+0); show
        // the resulting level in the same readout.
        let (zoom_in, zoom_out, zoom_reset) = ctx.input(|i| {
            let command = i.modifiers.command;
            (
                command && (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals)),
                command && i.key_pressed(egui::Key::Minus),
                command && i.key_pressed(egui::Key::Num0),
            )
        });
        if zoom_in || zoom_out || zoom_reset {
            let mut zoom = ctx.zoom_factor();
            if zoom_reset {
                zoom = 1.0;
            } else {
                if zoom_in {
                    zoom += 0.1;
                }
                if zoom_out {
                    zoom -= 0.1;
                }
                zoom = (zoom.clamp(0.2, 5.0) * 10.0).round() / 10.0;
            }
            let percent = (zoom * 100.0).round() as i32;
            self.show_indicator(
                tr_args("UI [{}%]", &[&percent as &dyn std::fmt::Display]),
                ctx,
            );
        }

        // Alt+- / Alt++ resize the lyric font.
        let font_down = ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::Minus));
        let font_up = ctx.input(|i| {
            i.modifiers.alt && (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals))
        });
        if font_down || font_up {
            let before = self.settings.font_scale;
            self.settings.font_scale = (before + if font_up { 0.1 } else { -0.1 }).clamp(0.4, 2.5);
            if (self.settings.font_scale - before).abs() > 1e-4 {
                let percent = (self.settings.font_scale * 100.0).round() as i32;
                self.show_indicator(
                    tr_args("FONT [{}%]", &[&percent as &dyn std::fmt::Display]),
                    ctx,
                );
                self.save_settings();
            }
        }

        // Volume: mouse wheel, except while the pointer is over the playlist
        // (there the wheel has to scroll the list instead).
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
        let ctrl = ctx.input(|i| i.modifiers.ctrl);
        let over_playlist = pointer
            .map(|pos| self.playlist_rect.contains(pos))
            .unwrap_or(false);

        // Ctrl+wheel is egui's zoom gesture, so leave that alone.
        if scroll.abs() > 0.01 && !over_playlist && !ctrl {
            let before = self.volume;
            self.volume = (self.volume + scroll * 0.0012).clamp(0.0, 1.0);
            if (self.volume - before).abs() > 1e-5 {
                if let Some(playback) = &self.playback {
                    let _ = playback.commands.send(Command::SetVolume(self.volume));
                }
                let percent = (self.volume * 100.0).round() as i32;
                self.show_indicator(
                    tr_args("VOL [{}]", &[&percent as &dyn std::fmt::Display]),
                    ctx,
                );
            }
        }

        // Ctrl+O opens a karaoke file or imports a playlist JSON.
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            self.open_dialog();
        }
        // Ctrl+R toggles playlist repeat.
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::R)) {
            self.settings.repeat = !self.settings.repeat;
            self.save_settings();
        }
    }

    // ----------------------------------------------------------------- layout

    fn time_signature_widget(&self, ui: &mut egui::Ui) {
        let position_us = self.position_us();

        // Before playback (and after the playlist ends) show the neutral 4/4.
        if position_us == 0 {
            draw_time_signature(ui, 1, 1, 4, None);
            return;
        }

        let Some(song) = &self.song else {
            draw_time_signature(ui, 1, 1, 4, None);
            return;
        };

        let tick = song.tempo_map.us_to_tick(position_us);
        let position = self.meter.position_at_tick(tick);
        draw_time_signature(
            ui,
            position.bar,
            position.beat,
            position.beats_per_bar,
            Some(position.beat_fraction),
        );
    }

    /// 24-hour digital clock shown just left of the time signature.
    ///
    /// The first time it is drawn it plays a short boot animation: the six
    /// zeros appear left-to-right, hold, blink once, then the real time is
    /// shown.
    fn clock_widget(&mut self, ui: &mut egui::Ui) {
        let now = ui.ctx().input(|i| i.time);

        let font = FontId::new(20.0, FontFamily::Name("clock".into()));
        // Reserve the full display width so the widget does not move while
        // the characters appear.
        let full = ui
            .painter()
            .layout_no_wrap("00:00:00".to_owned(), font.clone(), FG);
        let size = egui::vec2(full.size().x + 20.0, 36.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

        let mut easter_egg = self.clock_easter_egg_until.is_some_and(|until| now < until);
        if self.clock_easter_egg_until.is_some() && !easter_egg {
            self.clock_easter_egg_until = None;
        }

        // Clicking the clock six times in a row reveals the date the player
        // was first worked on.
        if !easter_egg && response.clicked() {
            if register_clock_click(&mut self.clock_clicks, &mut self.clock_last_click, now) {
                self.clock_easter_egg_until = Some(now + 6.0);
                easter_egg = true;
            }
        }

        let text = if easter_egg {
            "06:11:26".to_owned()
        } else {
            let start = *self.clock_anim_start.get_or_insert(now);
            let elapsed = (now - start).max(0.0);
            let (hour, minute, second) = local_time_hms();
            let normal = format!("{hour:02}:{minute:02}:{second:02}");
            clock_animation_text(elapsed, &normal)
        };

        if !text.is_empty() {
            let galley = ui.painter().layout_no_wrap(text, font, FG);
            let pos = Pos2::new(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5);
            ui.painter()
                .galley_with_override_text_color(pos, galley, FG);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        // Hiding the clock resets its boot animation, so enabling it again
        // replays the reveal.
        if !self.settings.show_clock {
            self.clock_anim_start = None;
            self.clock_clicks = 0;
        }

        egui::Panel::top("top")
            .frame(
                egui::Frame::NONE
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(24, 10)),
            )
            .show(ui, |ui| {
                let row = ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 36.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        // Keep the bar height constant even when every
                        // readout is hidden.
                        ui.set_min_height(36.0);
                        if self.settings.show_bar_beat {
                            self.time_signature_widget(ui);
                        }
                        if self.settings.show_clock {
                            if self.settings.show_bar_beat {
                                ui.add_space(8.0);
                            }
                            self.clock_widget(ui);
                        }
                        ui.min_rect()
                    },
                );

                // Title and artist on the far left.
                let panel_rect = ui.max_rect();
                let group_rect = row.response.rect;
                let right_limit = row.inner.left() - 14.0;
                let left = panel_rect.left();
                let center_y = group_rect.center().y;
                let painter = ui.painter();

                if self.settings.show_song_info {
                    match &self.song {
                        Some(song) => {
                            let title = song
                                .title
                                .as_deref()
                                .map(str::to_owned)
                                .unwrap_or_else(|| tr("Untitled"));
                            let title_galley =
                                painter.layout_no_wrap(title, FontId::proportional(17.0), FG);
                            let artist_galley = song.artist.as_ref().map(|artist| {
                                painter.layout_no_wrap(
                                    format!("— {artist}"),
                                    FontId::proportional(14.0),
                                    DIM,
                                )
                            });

                            let gap = if artist_galley.is_some() { 8.0 } else { 0.0 };
                            let mut x = left;

                            painter.galley_with_override_text_color(
                                Pos2::new(x, center_y - title_galley.size().y * 0.5),
                                title_galley.clone(),
                                FG,
                            );
                            x += title_galley.size().x + gap;

                            if let Some(artist_galley) = artist_galley {
                                let artist_width = artist_galley.size().x;
                                painter.galley_with_override_text_color(
                                    Pos2::new(x, center_y - artist_galley.size().y * 0.5),
                                    artist_galley,
                                    DIM,
                                );
                                x += artist_width;
                            }

                            // Key box, right of the title/artist.
                            if let Some(key) = song
                                .key
                                .as_deref()
                                .map(str::trim)
                                .filter(|key| !key.is_empty())
                            {
                                let key_galley = painter.layout_no_wrap(
                                    key_label(key, self.transpose),
                                    FontId::proportional(13.0),
                                    FG,
                                );
                                let padding = egui::vec2(8.0, 4.0);
                                let box_size = key_galley.size() + padding * 2.0;
                                let box_rect = Rect::from_min_size(
                                    Pos2::new(x + 16.0, center_y - box_size.y * 0.5),
                                    box_size,
                                );
                                if box_rect.right() <= right_limit {
                                    painter.rect_filled(box_rect, 4.0, Color32::from_gray(16));
                                    painter.rect_stroke(
                                        box_rect,
                                        4.0,
                                        egui::Stroke::new(1.0, Color32::from_gray(70)),
                                        egui::StrokeKind::Inside,
                                    );
                                    painter.galley_with_override_text_color(
                                        box_rect.min + padding,
                                        key_galley,
                                        FG,
                                    );
                                }
                            }
                        }
                        None => {
                            painter.text(
                                Pos2::new(left, center_y),
                                egui::Align2::LEFT_CENTER,
                                tr("No song loaded"),
                                FontId::proportional(15.0),
                                DIM,
                            );
                        }
                    }
                }
            });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("bottom")
            .frame(
                egui::Frame::NONE
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(24, 14)),
            )
            .show(ui, |ui| {
                let duration_us = self.song.as_ref().map(|s| s.duration_us).unwrap_or(0);
                let duration_s = (duration_us as f64 / 1_000_000.0).max(0.01);
                let position = self.position_us();

                ui.horizontal(|ui| {
                    // Both transport buttons are exactly the same size.
                    let button = [46.0, 32.0];
                    let icon = if self.paused { Icon::Play } else { Icon::Pause };
                    if ui
                        .add_sized(button, egui::Button::new(icon.rich(16.0)))
                        .on_hover_text(tr("Play/Pause (F5)"))
                        .clicked()
                    {
                        self.toggle_play();
                    }
                    if ui
                        .add_sized(button, egui::Button::new(Icon::Stop.rich(16.0)))
                        .on_hover_text(tr("Stop (F6)"))
                        .clicked()
                    {
                        self.stop();
                    }

                    let shown = if self.scrubbing {
                        self.scrub_value
                    } else {
                        position as f64 / 1_000_000.0
                    };
                    ui.label(
                        RichText::new(fmt_time((shown * 1_000_000.0) as u64))
                            .size(12.0)
                            .color(DIM)
                            .monospace(),
                    );

                    // Leave room for the status text on the right.
                    let status_width = 210.0;
                    let mut seconds = shown;
                    let width = (ui.available_width() - status_width - 12.0).max(120.0);
                    ui.spacing_mut().slider_width = width;
                    let response =
                        ui.add(egui::Slider::new(&mut seconds, 0.0..=duration_s).show_value(false));

                    if response.dragged() {
                        self.scrubbing = true;
                        self.scrub_value = seconds;
                        self.bar_seek_anchor = None;
                    }
                    if response.drag_stopped() {
                        self.bar_seek_anchor = None;
                        if let Some(playback) = &self.playback {
                            let _ = playback
                                .commands
                                .send(Command::Seek((self.scrub_value * 1_000_000.0) as u64));
                        }
                        self.scrubbing = false;
                        self.paused = false;
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(song) = &self.song {
                            // An unidentified MIDI is labelled "MIDI Common";
                            // the karaoke formats also show the tempo at the
                            // playhead and the lyric-line count.
                            let current_bpm = self.current_bpm();
                            let bpm_text = format!("{:.0}", current_bpm);
                            let lines = song.lyric_lines.len().to_string();
                            let label = match song.format {
                                karaoke_player::SourceFormat::Midi => tr("MIDI Common"),
                                karaoke_player::SourceFormat::Kar => tr_args(
                                    "KAR  ·  {} BPM  ·  {} lines",
                                    &[
                                        &bpm_text as &dyn std::fmt::Display,
                                        &lines as &dyn std::fmt::Display,
                                    ],
                                ),
                                karaoke_player::SourceFormat::Ncn => tr_args(
                                    "NCN  ·  {} BPM  ·  {} lines",
                                    &[
                                        &bpm_text as &dyn std::fmt::Display,
                                        &lines as &dyn std::fmt::Display,
                                    ],
                                ),
                                karaoke_player::SourceFormat::Kmid => tr_args(
                                    "KMID  ·  {} BPM  ·  {} lines",
                                    &[
                                        &bpm_text as &dyn std::fmt::Display,
                                        &lines as &dyn std::fmt::Display,
                                    ],
                                ),
                            };
                            ui.label(RichText::new(label).size(11.0).color(DIM));
                        }
                    });
                });
            });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let background = color_or(&self.settings.background_color, [0, 0, 0]);
        let central = egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(background))
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let response = ui.allocate_rect(rect, egui::Sense::click());

                if response.secondary_clicked() {
                    if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                        self.open_menu(MenuKind::Central, pos);
                    }
                }

                let painter = ui.painter_at(rect);

                let Some(song) = &self.song else {
                    return;
                };

                let now = self.position_us();

                let index = song.line_at(now);
                // Keep the lyric size independent of the playlist panel: use
                // the full window width and only shift the text right when the
                // panel is open.
                let window_width = ui.ctx().content_rect().width();
                let base_size =
                    (window_width * 0.045 * self.settings.font_scale).clamp(12.0, 140.0);

                // Both lines use the same size; shrink only when a line would
                // not fit the window.
                let mut texts: Vec<&str> = Vec::new();
                match index {
                    Some(i) => {
                        texts.push(song.lyric_lines[i].text.as_str());
                        if let Some(next) = song.lyric_lines.get(i + 1) {
                            texts.push(next.text.as_str());
                        }
                    }
                    None => {
                        if let Some(first) = song.lyric_lines.first() {
                            texts.push(first.text.as_str());
                        }
                    }
                }
                let family =
                    lyric_font_family(self.settings.lyric_font_style, self.lyric_font_loaded);
                let size = fit_size(&painter, window_width, base_size, &texts, &family);
                let font = FontId::new(size, family);
                let colors = lyric_colors(&self.settings);
                let edge = size * 0.05 * self.settings.lyric_edge_size.clamp(0.0, 2.0);

                let gap = size * 1.4 * self.settings.lyric_distance.clamp(0.5, 3.0);
                let raw_center = match self.settings.text_visibility {
                    TextVisibility::Center => rect.center().y,
                    TextVisibility::Upper => rect.top() + rect.height() * 0.22,
                    TextVisibility::Lower => rect.top() + rect.height() * 0.78,
                };
                let max_half = rect.height() * 0.5;
                let half_block = (gap * 0.5 + size * 0.7).min(max_half);
                let center_y =
                    raw_center.clamp(rect.top() + half_block, rect.bottom() - half_block);
                let rows = [center_y - gap * 0.5, center_y + gap * 0.5];

                // Lyrics stay hidden until the song has actually started and
                // disappear again when it ends.
                let finished = song.duration_us > 0 && now >= song.duration_us;
                if now > 0 && !finished {
                    if let Some(i) = index {
                        let (current_row, next_row) =
                            lyric_rows(self.settings.cursor_visibility, i);
                        draw_wipe(
                            &painter,
                            rect,
                            &font,
                            rows[current_row],
                            &song.lyric_lines[i],
                            now,
                            colors,
                            edge,
                        );
                        if let Some(next) = song.lyric_lines.get(i + 1) {
                            draw_next_lyric(
                                &painter,
                                rect,
                                &font,
                                rows[next_row],
                                &next.text,
                                colors,
                                edge,
                                self.settings.lyric_depth,
                            );
                        }
                    }
                }

                if let Some(error) = &self.error {
                    draw_plain(
                        &painter,
                        rect,
                        &FontId::proportional(13.0),
                        rect.max.y - 24.0,
                        error,
                        ERROR,
                    );
                }

                if let Some(indicator) = &self.indicator {
                    let now = ui.ctx().input(|i| i.time);
                    if now <= indicator.until {
                        let text = if indicator.bpm {
                            bpm_readout(self.bpm_adjust(), self.current_bpm())
                        } else {
                            indicator.text.clone()
                        };
                        draw_indicator(&painter, rect, &text);
                    }
                }

                // When the top-bar song info is hidden, show the translucent
                // song card for the first five seconds (and on Space).
                if !self.settings.show_song_info {
                    let now = ui.ctx().input(|i| i.time);
                    if let (Some(song), Some(until)) = (self.song.as_ref(), self.song_card_until) {
                        if now < until {
                            draw_song_card(
                                &painter,
                                rect,
                                song,
                                self.path.as_deref(),
                                self.transpose,
                                self.current_bpm(),
                                self.bpm_adjust(),
                            );
                        }
                    }
                }
            });

        // Hide the pointer while it is over the lyric display; the context
        // menu and the settings dialog always keep it visible.
        let pointer_over_lyrics = ui
            .ctx()
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|pos| central.response.rect.contains(pos));
        if self.settings.hide_cursor
            && !self.settings_open
            && self.menu.is_none()
            && !egui::Popup::is_any_open(ui.ctx())
            && pointer_over_lyrics
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        }
    }

    fn clear_playlist(&mut self) {
        self.playlist.clear();
        self.playlist_index = usize::MAX;
        self.playlist_selection = None;
        self.save_settings();
    }

    fn open_menu(&mut self, kind: MenuKind, pos: Pos2) {
        self.menu = Some(OpenMenu {
            kind,
            pos,
            alpha: 1.0,
            closing: false,
            just_opened: true,
            submenu_open: false,
        });
    }

    fn apply_menu_action(&mut self, action: MenuAction, ctx: &egui::Context) {
        match action {
            MenuAction::Open => self.open_dialog(),
            MenuAction::AddFiles => self.add_files(),
            MenuAction::AddFolder => self.add_folder(),
            MenuAction::ImportPlaylist => self.import_playlist(),
            MenuAction::ExportPlaylist => self.export_playlist(),
            MenuAction::TogglePlaylist => {
                self.settings.show_playlist = !self.settings.show_playlist;
                self.temp_playlist_until = None;
                self.temp_playlist_hidden = false;
                self.save_settings();
            }
            MenuAction::Remove(index) => self.remove_playlist_entry(index),
            MenuAction::Settings => self.open_settings(),
            MenuAction::ToggleFullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            MenuAction::Minimize => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            MenuAction::ToggleCursor => {
                self.settings.hide_cursor = !self.settings.hide_cursor;
                self.save_settings();
            }
            MenuAction::ToggleRepeat => {
                self.settings.repeat = !self.settings.repeat;
                self.save_settings();
            }
            MenuAction::ClearPlaylist => self.clear_playlist(),
            MenuAction::Exit => {
                if self.settings.confirm_exit {
                    self.exit_confirm_open = true;
                    self.exit_confirm_just_opened = true;
                    self.exit_confirm_fade = 1.0;
                    self.exit_confirm_closing = false;
                } else {
                    self.exit_confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    /// Draw the custom context menu and animate its closing fade.
    fn draw_menu(&mut self, ctx: &egui::Context) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let kind = menu.kind;
        let pos = menu.pos;
        let alpha = menu.alpha;
        let closing = menu.closing;
        let submenu_open = menu.submenu_open;

        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let next_alpha = if closing {
            (alpha - dt / 0.15).max(0.0)
        } else {
            alpha
        };
        if closing && next_alpha <= 0.001 {
            self.menu = None;
            return;
        }

        let screen = ctx.content_rect();
        let (width, height) = match kind {
            MenuKind::Row(_) => (188.0, 90.0),
            MenuKind::Background => (188.0, 190.0),
            MenuKind::Central => (188.0, 224.0),
        };
        let pos = Pos2::new(
            pos.x.min(screen.right() - width).max(screen.left()),
            pos.y.min(screen.bottom() - height).max(screen.top()),
        );

        let mut action = None;
        let mut song_row: Option<Rect> = None;
        let mut song_hovered = false;
        let mut song_clicked = false;
        let area = egui::Area::new(egui::Id::unique("context_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                let fill = Color32::from_rgba_unmultiplied(8, 8, 8, (255.0 * alpha) as u8);
                let stroke = Color32::from_rgba_unmultiplied(70, 70, 70, (255.0 * alpha) as u8);
                egui::Frame::NONE
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, stroke))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(6, 6))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 2.0);
                        ui.set_opacity(alpha);
                        match kind {
                            MenuKind::Central => {
                                let row =
                                    menu_item_response(ui, Icon::Music, &tr("Song Player  ›"));
                                song_hovered = row.hovered();
                                song_clicked = row.clicked();
                                song_row = Some(row.rect);
                                ui.separator();
                                let fullscreen =
                                    ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                                let fullscreen_label = if fullscreen {
                                    tr("Exit fullscreen")
                                } else {
                                    tr("Fullscreen")
                                };
                                let fullscreen_icon = if fullscreen {
                                    Icon::Minimize2
                                } else {
                                    Icon::Maximize
                                };
                                if menu_item(ui, fullscreen_icon, &fullscreen_label) {
                                    action = Some(MenuAction::ToggleFullscreen);
                                }
                                if menu_item(ui, Icon::Minus, &tr("Minimize")) {
                                    action = Some(MenuAction::Minimize);
                                }
                                let cursor_label = if self.settings.hide_cursor {
                                    tr("Show cursor")
                                } else {
                                    tr("Hide cursor")
                                };
                                let cursor_icon = if self.settings.hide_cursor {
                                    Icon::MousePointer
                                } else {
                                    Icon::MousePointerBan
                                };
                                if menu_item(ui, cursor_icon, &cursor_label) {
                                    action = Some(MenuAction::ToggleCursor);
                                }
                                ui.separator();
                                let playlist = if self.settings.show_playlist {
                                    tr("Hide playlist")
                                } else {
                                    tr("Show playlist")
                                };
                                if menu_item(ui, Icon::ListMusic, &playlist) {
                                    action = Some(MenuAction::TogglePlaylist);
                                }
                                if menu_item(ui, Icon::Settings, &tr("Setting…")) {
                                    action = Some(MenuAction::Settings);
                                }
                                ui.separator();
                                if menu_item(ui, Icon::LogOut, &tr("Exit")) {
                                    action = Some(MenuAction::Exit);
                                }
                            }
                            MenuKind::Row(index) => {
                                if menu_item(ui, Icon::X, &tr("Remove from playlist")) {
                                    action = Some(MenuAction::Remove(index));
                                }
                                ui.separator();
                                if menu_item(ui, Icon::ListMusic, &tr("Hide playlist")) {
                                    action = Some(MenuAction::TogglePlaylist);
                                }
                            }
                            MenuKind::Background => {
                                if menu_item(ui, Icon::FilePlus, &tr("Add files…")) {
                                    action = Some(MenuAction::AddFiles);
                                }
                                if menu_item(ui, Icon::FolderPlus, &tr("Add folder…")) {
                                    action = Some(MenuAction::AddFolder);
                                }
                                ui.separator();
                                if menu_item(ui, Icon::Download, &tr("Import playlist…")) {
                                    action = Some(MenuAction::ImportPlaylist);
                                }
                                if menu_item(ui, Icon::Upload, &tr("Export playlist…")) {
                                    action = Some(MenuAction::ExportPlaylist);
                                }
                                ui.separator();
                                if menu_item(ui, Icon::ListMusic, &tr("Hide playlist")) {
                                    action = Some(MenuAction::TogglePlaylist);
                                }
                            }
                        }
                    });
            });

        // The "Song Player" flyout opens on hover and stays open while the
        // pointer crosses the small gap towards it.
        let mut sub_action = None;
        let mut submenu_rect = None;
        if matches!(kind, MenuKind::Central) {
            if let Some(row) = song_row {
                let pointer = ctx.input(|i| i.pointer.hover_pos());
                let over_main = pointer.is_some_and(|p| area.response.rect.contains(p));
                let show = submenu_open || song_hovered || song_clicked;
                if show {
                    let sub_width = 188.0;
                    let x = if row.right() + 2.0 + sub_width <= screen.right() {
                        row.right() + 2.0
                    } else {
                        area.response.rect.left() - sub_width - 2.0
                    };
                    let (act, rect) = song_player_submenu(
                        ctx,
                        Pos2::new(x, row.top() - 6.0),
                        alpha,
                        self.settings.repeat,
                    );
                    sub_action = act;
                    submenu_rect = Some(rect);
                }

                let pointer_over_sub =
                    submenu_rect.is_some_and(|r| pointer.is_some_and(|p| r.contains(p)));
                if song_hovered || song_clicked || pointer_over_sub {
                    self.menu.as_mut().unwrap().submenu_open = true;
                } else if over_main {
                    self.menu.as_mut().unwrap().submenu_open = false;
                }
            }
        }

        if !closing {
            if let Some(action) = action {
                self.apply_menu_action(action, ctx);
                if let Some(menu) = &mut self.menu {
                    menu.closing = true;
                }
            }
            if let Some(action) = sub_action {
                self.apply_menu_action(action, ctx);
                if let Some(menu) = &mut self.menu {
                    menu.closing = true;
                }
            }
        }

        if let Some(menu) = &mut self.menu {
            if menu.just_opened {
                menu.just_opened = false;
            } else {
                let clicked = ctx.input(|i| i.pointer.any_click());
                let inside = ctx
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|pos| area.response.rect.contains(pos));
                if !menu.closing
                    && ((clicked && !inside) || ctx.input(|i| i.key_pressed(egui::Key::Escape)))
                {
                    menu.closing = true;
                }
            }
            if menu.closing {
                menu.alpha = next_alpha;
            }
        }
    }

    /// Confirm before leaving the player.
    fn exit_confirm(&mut self, ctx: &egui::Context) {
        let mut cancel = false;
        let mut confirm = false;
        let just_opened = std::mem::take(&mut self.exit_confirm_just_opened);

        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        if self.exit_confirm_closing {
            self.exit_confirm_fade = (self.exit_confirm_fade - dt / 0.15).max(0.0);
            if self.exit_confirm_fade <= 0.001 {
                self.exit_confirm_open = false;
                self.exit_confirm_closing = false;
                self.exit_confirm_fade = 1.0;
                return;
            }
        }
        let fade = self.exit_confirm_fade;

        let modal = egui::Modal::new(egui::Id::unique("exit_confirm"))
            .backdrop_color(Color32::from_black_alpha((100.0 * fade) as u8))
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(
                        8,
                        8,
                        8,
                        (255.0 * fade) as u8,
                    ))
                    .stroke(egui::Stroke::new(
                        1.5,
                        Color32::from_rgba_unmultiplied(120, 120, 120, (255.0 * fade) as u8),
                    ))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(22, 18)),
            )
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.set_opacity(fade);
                ui.label(RichText::new(tr("Exit DontreeCrab?")).size(15.0));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(tr("Are you sure you want to close the player?"))
                        .size(12.0)
                        .color(DIM),
                );
                ui.add_space(18.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_sized([84.0, 28.0], egui::Button::new(tr("Exit")))
                        .clicked()
                    {
                        confirm = true;
                    }
                    if ui
                        .add_sized([84.0, 28.0], egui::Button::new(tr("Cancel")))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });

        if confirm {
            self.exit_confirmed = true;
            self.exit_confirm_open = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if cancel || (!just_opened && modal.should_close()) {
            self.exit_confirm_closing = true;
        }
    }
}

impl eframe::App for KaraokeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Start the song card timer on the first frame after a song opens.
        if self.song_card_pending {
            self.song_card_pending = false;
            if !self.settings.show_song_info {
                let now = ctx.input(|i| i.time);
                self.song_card_until = Some(now + self.settings.indicator_seconds as f64);
            }
        }
        // Paint the lyric background across the whole viewport first, so the
        // translucent playlist panel can show it through.
        let background = color_or(&self.settings.background_color, [0, 0, 0]);
        ui.painter().rect_filled(ui.max_rect(), 0.0, background);
        // Apply the result of a native dialog that finished on its worker
        // thread. Doing this every frame keeps the UI live while it is open.
        self.poll_dialog(&ctx);
        // Ask before closing through the window button or Alt+F4.
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_confirmed {
            if self.settings.confirm_exit {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.exit_confirm_open = true;
                self.exit_confirm_just_opened = true;
                self.exit_confirm_fade = 1.0;
                self.exit_confirm_closing = false;
            } else {
                self.exit_confirmed = true;
            }
        }
        // While the settings dialog or the exit confirmation is open it owns
        // the keyboard.
        if !self.settings_open && !self.exit_confirm_open && self.menu.is_none() {
            self.handle_input(&ctx);
        }
        // Follow tempo changes in the song while playback moves through the
        // tempo map, keeping the user's BPM adjustment constant.
        self.update_playback_speed();

        // A temporarily revealed playlist hides itself after 10 seconds.
        if let Some(until) = self.temp_playlist_until {
            if ctx.input(|i| i.time) >= until {
                self.temp_playlist_until = None;
                if self.temp_playlist_hidden {
                    self.settings.show_playlist = false;
                    self.temp_playlist_hidden = false;
                    self.playlist_selection = None;
                }
            }
        }

        // Keep timers (save debounce, buffer rebuild) ticking.
        ctx.request_repaint();

        // Debounced buffer change: rebuilding the stream is expensive, so do
        // it once the user has stopped moving the value.
        if let Some(at) = self.buffer_rebuild_at {
            if ctx.input(|i| i.time) >= at {
                self.buffer_rebuild_at = None;
                self.rebuild_audio();
            }
        }

        // Debounced settings save.
        if let Some(at) = self.settings_save_at {
            if ctx.input(|i| i.time) >= at {
                self.settings_save_at = None;
                self.save_settings();
            }
        }

        // Scheduled gap between two songs (Musical interval).
        if let Some(at) = self.next_song_at {
            if ctx.input(|i| i.time) >= at {
                self.next_song_at = None;
                if self.settings.auto_play {
                    self.on_song_finished();
                } else {
                    self.clear_played_song();
                }
            }
        }

        // Song finished: stop on the bar line after the last event.
        let finished =
            self.song.is_some() && self.song_end_us > 0 && self.position_us() >= self.song_end_us;
        if finished && !self.paused {
            self.paused = true;
            if self.settings.auto_play {
                if self.settings.music_interval_seconds > 0 {
                    let now = ctx.input(|i| i.time);
                    self.next_song_at = Some(now + self.settings.music_interval_seconds as f64);
                } else {
                    self.next_song_at = None;
                    self.on_song_finished();
                }
            } else {
                self.clear_played_song();
            }
        }

        self.top_bar(ui);
        self.bottom_bar(ui);
        if self.settings.show_playlist {
            self.playlist_panel(ui);
        }
        self.central(ui);
        self.draw_menu(&ctx);

        if self.settings_open {
            self.settings_modal(&ctx);
        }

        if self.exit_confirm_open {
            self.exit_confirm(&ctx);
        }
    }
}

impl Drop for KaraokeApp {
    fn drop(&mut self) {
        // Never write the user's real config while running tests.
        if cfg!(test) {
            return;
        }
        self.settings.volume = self.volume;
        self.settings.transpose = self.transpose;
        self.settings.soundfont = self.sound_font_path.clone();
        let _ = self.settings.save();
    }
}

/// Recursively collect playable files, with a small depth limit so picking a
/// huge folder by mistake cannot freeze the UI for long.
fn collect_songs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 || out.len() >= 5000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_songs(&path, depth + 1, out);
        } else if is_song_file(&path) {
            out.push(path);
        }
    }
}

fn is_song_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("kar") | Some("mid") | Some("midi")
    )
}

/// Best-effort file type for playlist export (`kar`, `ncn`, `midi`).
fn file_type_guess(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match extension.as_str() {
        "kar" => "kar".to_owned(),
        "mid" | "midi" => {
            if karaoke_player::ncn::locate(path, &LoadOptions::default()).is_some() {
                "ncn".to_owned()
            } else {
                "midi".to_owned()
            }
        }
        _ => "unknown".to_owned(),
    }
}

/// A clickable GitHub link with a Lucide external-link icon.
fn about_github_link(ui: &mut egui::Ui) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(140.0, 24.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let color = if response.hovered() { FG } else { DIM };
    icons::paint(
        ui.painter(),
        Pos2::new(rect.left() + 8.0, rect.center().y),
        Icon::ExternalLink,
        16.0,
        color,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 28.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Github",
        FontId::proportional(13.0),
        color,
    );
    if response.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(GITHUB_URL));
    }
}

/// Draw the Licence or Created popup. Returns true when it should close.
fn draw_about_popup(ctx: &egui::Context, popup: AboutPopup) -> bool {
    let mut close = false;
    let title = match popup {
        AboutPopup::License => tr("License"),
        AboutPopup::Created => tr("Created"),
    };

    let modal = egui::Modal::new(egui::Id::unique("about_popup"))
        .backdrop_color(Color32::from_black_alpha(150))
        .frame(
            egui::Frame::NONE
                .fill(Color32::from_rgb(8, 8, 8))
                .stroke(egui::Stroke::new(1.5, Color32::from_gray(120)))
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::symmetric(22, 18)),
        )
        .show(ctx, |ui| {
            ui.set_width(560.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(16.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_sized([84.0, 26.0], egui::Button::new(tr("Close")))
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            ui.add_space(8.0);
            match popup {
                AboutPopup::License => draw_license(ui),
                AboutPopup::Created => draw_created(ui),
            }
        });

    close || modal.should_close()
}

fn draw_license(ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .max_height(310.0)
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(LICENSE_TEXT).monospace().size(10.5)).wrap());
        });
    ui.add_space(8.0);
    ui.label(
        RichText::new(tr(
            "Bundled fonts: Sarabun/E1234 OFL-1.1, Lucide ISC. See THIRD_PARTY_NOTICES.md.",
        ))
        .size(11.0)
        .color(DIM),
    );
}

fn draw_created(ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            about_section(ui, &tr("Software Composer"), "PXCza");
            about_section(
                ui,
                &tr("Tools Used"),
                "DeepSeek V4.1 Flash 1017\nOpenCode 2.2.7",
            );
            about_section(
                ui,
                &tr("Built with"),
                "Rust · eframe/egui · cpal · rustysynth · midly · serde",
            );
            let os = match std::env::consts::OS {
                "windows" => "Windows PC",
                "linux" => "Linux PC",
                "macos" => "macOS",
                other => other,
            };
            about_section(
                ui,
                &tr("Hardware"),
                &format!(
                    "{os} · {} · 44.1 kHz / 2 ch / f32",
                    std::env::consts::ARCH
                ),
            );
            about_section(
                ui,
                &tr("Licenses"),
                "Application: MIT\nSarabun / E1234: SIL OFL 1.1\nLucide: ISC\negui fonts: MIT / OFL-1.1 / Ubuntu Font Licence 1.0",
            );
            about_section(ui, &tr("Privacy"), &tr("No telemetry. Works offline."));
        });
}

fn about_section(ui: &mut egui::Ui, title: &str, body: &str) {
    ui.label(RichText::new(title).size(13.0).strong());
    for line in body.lines() {
        ui.label(RichText::new(line).size(12.0).color(DIM));
    }
    ui.add_space(8.0);
}

/// One entry in the settings dialog's left navigation column.
fn nav_row(ui: &mut egui::Ui, label: &str, selected: bool, icon: Icon) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());

    let painter = ui.painter();
    if selected || response.hovered() {
        let fill = if selected {
            Color32::from_gray(44)
        } else {
            Color32::from_gray(28)
        };
        painter.rect_filled(rect, 6.0, fill);
    }
    if selected {
        painter.rect_filled(
            Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            2.0,
            FG,
        );
    }

    // Lucide icon column.
    icons::paint(
        painter,
        Pos2::new(rect.left() + 22.0, rect.center().y),
        icon,
        18.0,
        if selected { FG } else { DIM },
    );

    painter.text(
        Pos2::new(rect.left() + 40.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        if selected { FG } else { DIM },
    );

    response
}

/// A titled group of vertical sliders. The title sits above the left edge of
/// the group, matching the reference design.
fn slider_group(
    ui: &mut egui::Ui,
    title: &str,
    width: f32,
    add_sliders: impl FnOnce(&mut egui::Ui),
) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 190.0),
        Layout::top_down(Align::Center),
        |ui| {
            // The outer row may have zeroed horizontal spacing to distribute
            // the groups; restore normal spacing between the slider columns.
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.allocate_ui_with_layout(
                egui::vec2(width, 16.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.add_space(20.0);
                    ui.label(RichText::new(title).size(13.0));
                },
            );
            ui.add_space(2.0);
            let pad = ((ui.available_width() - width) * 0.5).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                add_sliders(ui);
            });
        },
    );
}

/// One vertical slider with its label and live readout. Double-clicking the
/// slider restores `default`.
fn slider_column(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    changed: &mut bool,
) {
    let unit = range.end().to_owned() <= 1.0 && range.start().to_owned() >= 0.0;
    ui.allocate_ui_with_layout(
        egui::vec2(54.0, 152.0),
        Layout::top_down(Align::Center),
        |ui| {
            // A vertical layout left-aligns widgets, so put the slider in a
            // fixed-width row and centre it manually.
            ui.allocate_ui_with_layout(
                egui::vec2(54.0, 100.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(17.0);
                    let response = ui.add(
                        egui::Slider::new(value, range.clone())
                            .vertical()
                            .show_value(false)
                            .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.55 }),
                    );
                    // Sliders only sense dragging, so `response.double_clicked()`
                    // is never true; check the pointer events instead.
                    let rect = response.rect;
                    let double_clicked = ui.input(|i| {
                        i.pointer
                            .button_double_clicked(egui::PointerButton::Primary)
                            && i.pointer
                                .interact_pos()
                                .map_or(false, |pos| rect.contains(pos))
                    });
                    if double_clicked {
                        *value = default;
                        *changed = true;
                    } else if response.changed() {
                        *changed = true;
                    }

                    // Small scale marks on both sides of the rail.
                    let painter = ui.painter();
                    let center_x = rect.center().x;
                    let handle_radius = rect.width() / 2.5;
                    let track_top = rect.top() + handle_radius;
                    let track_bottom = rect.bottom() - handle_radius;
                    let span = (range.end() - range.start()).max(f32::EPSILON);
                    let t = ((*value - range.start()) / span).clamp(0.0, 1.0);
                    let handle_y = track_bottom - t * (track_bottom - track_top);
                    let tick = Color32::from_gray(95);
                    for index in 0..7 {
                        let y = track_top + (track_bottom - track_top) * index as f32 / 6.0;
                        if (y - handle_y).abs() < 7.0 {
                            continue;
                        }
                        painter.line_segment(
                            [Pos2::new(center_x - 16.0, y), Pos2::new(center_x - 9.0, y)],
                            egui::Stroke::new(1.0, tick),
                        );
                        painter.line_segment(
                            [Pos2::new(center_x + 9.0, y), Pos2::new(center_x + 16.0, y)],
                            egui::Stroke::new(1.0, tick),
                        );
                    }
                },
            );

            ui.add_space(2.0);
            ui.label(RichText::new(label).size(11.0).color(FG));
            let text = if unit {
                format!("{:.0}%", *value * 100.0)
            } else if value.abs() < 0.05 {
                "0.0 dB".to_owned()
            } else {
                format!("{value:+.1} dB")
            };
            ui.label(RichText::new(text).size(10.0).color(DIM));
        },
    );
}

// ------------------------------------------------------------------- drawing

/// Bar/beat readout: one cell per beat, the current beat flashing white, plus
/// `bar : beat` underneath. No frame — it sits directly on the black bar.
fn draw_time_signature(ui: &mut egui::Ui, bar: u32, beat: u32, beats: u32, active: Option<f32>) {
    let beats = beats.clamp(1, 12);
    let gap = 4.0;
    let cell_h = 10.0;
    // Reserve the width of a 4/4 bar so the top bar never shifts when the
    // time signature changes; the cells stretch to fill it.
    let inner_w = 4.0 * 19.0 + 3.0 * gap;
    let cell_w = ((inner_w - beats.saturating_sub(1) as f32 * gap) / beats as f32).max(4.0);
    let total_w = beats as f32 * cell_w + beats.saturating_sub(1) as f32 * gap;
    let width = inner_w + 8.0;
    let height = 36.0;

    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter();

    let start_x = rect.center().x - total_w * 0.5;
    let y = rect.top() + 3.0;

    for index in 0..beats {
        let x = start_x + index as f32 * (cell_w + gap);
        let cell = Rect::from_min_size(Pos2::new(x, y), egui::vec2(cell_w, cell_h));

        let fill = if index + 1 == beat {
            match active {
                // Flash white on the beat, then fade out like a light.
                Some(phase) => {
                    let level = (1.0 - phase * 2.0).clamp(0.0, 1.0);
                    Color32::from_gray((85.0 + 170.0 * level) as u8)
                }
                // Song has not started yet: no highlight.
                None => Color32::from_gray(32),
            }
        } else {
            Color32::from_gray(32)
        };

        painter.rect_filled(cell, 2.0, fill);
    }

    painter.text(
        Pos2::new(rect.center().x, rect.top() + 25.0),
        egui::Align2::CENTER_CENTER,
        format!("{bar} : {beat}"),
        FontId::new(15.0, FontFamily::Monospace),
        FG,
    );
}

/// Top-right readout used for KEY / VOL / MELO changes.
///
/// The box is locked to the size of the widest readout (`BPM : 200 [-180]`),
/// while the text itself is drawn a little smaller and centred inside it.
fn draw_indicator(painter: &egui::Painter, rect: Rect, text: &str) {
    // Every readout uses the size of the widest one, so the box is locked and
    // never jumps when the text changes.
    let size = painter
        .layout_no_wrap(
            "BPM : 200 [-180]".to_owned(),
            FontId::new(22.0, FontFamily::Monospace),
            FG,
        )
        .size()
        + egui::vec2(36.0, 22.0);
    let box_rect = Rect::from_min_size(
        Pos2::new(rect.max.x - size.x - 20.0, rect.min.y + 18.0),
        size,
    );

    painter.rect_filled(box_rect, 7.0, Color32::from_black_alpha(215));
    painter.rect_stroke(
        box_rect,
        7.0,
        egui::Stroke::new(1.8, FG),
        egui::StrokeKind::Inside,
    );

    let text_galley = painter.layout_no_wrap(
        text.to_owned(),
        FontId::new(17.0, FontFamily::Monospace),
        FG,
    );
    let text_pos = box_rect.center() - text_galley.size() * 0.5;
    painter.galley_with_override_text_color(text_pos, text_galley, FG);
}

/// Translucent card with the file code, song, artist, BPM and key.
///
/// Shown in the lyrics area when the top-bar song info is disabled: once for
/// five seconds when a song opens, and again whenever Space is pressed.
fn draw_song_card(
    painter: &egui::Painter,
    rect: Rect,
    song: &Song,
    path: Option<&Path>,
    transpose: i32,
    bpm: f64,
    bpm_adjust: i32,
) {
    let code = path
        .and_then(|path| path.file_stem())
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "-".to_owned());
    let song_title = song.title.clone().unwrap_or_else(|| "-".to_owned());
    let artist = song.artist.clone().unwrap_or_else(|| "-".to_owned());
    let bpm = card_bpm(bpm_adjust, bpm);
    let key = song
        .key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(|key| card_key(key, transpose))
        .unwrap_or_else(|| "-".to_owned());

    let labels = [tr("Code"), tr("Song"), tr("Artist"), tr("BPM"), tr("Key")];
    let values = [code, song_title, artist, bpm, key];

    // Keep the card proportional to the lyrics area. Its height follows the
    // width (the 420 x 220 design aspect) so tall 4:3 windows do not stretch
    // the row spacing.
    let width = (rect.width() * 0.30)
        .clamp(280.0, 460.0)
        .min(rect.width() - 56.0);
    let height = (width * (220.0 / 420.0)).min(rect.height() - 44.0);
    let scale = (width / 420.0).min(height / 220.0).clamp(0.68, 1.15);
    let padding = 20.0 * scale;
    let font_size = 17.0 * scale;

    let label_font = FontId::new(font_size, FontFamily::Name("bold".into()));
    let value_font = FontId::proportional(font_size);
    let label_galleys: Vec<_> = labels
        .iter()
        .map(|label| painter.layout_no_wrap((*label).to_owned(), label_font.clone(), FG))
        .collect();
    let value_galleys: Vec<_> = values
        .iter()
        .map(|value| painter.layout_no_wrap(value.clone(), value_font.clone(), FG))
        .collect();
    let colon = painter.layout_no_wrap(":".to_owned(), value_font.clone(), DIM);

    let row_height = (height - padding * 2.0) / labels.len() as f32;
    let label_width = label_galleys.iter().map(|g| g.size().x).fold(0.0, f32::max);
    let box_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 28.0 * scale, rect.top() + 22.0 * scale),
        egui::vec2(width, height),
    );

    painter.rect_filled(box_rect, 6.0, Color32::from_black_alpha(178));
    painter.rect_stroke(
        box_rect,
        6.0,
        egui::Stroke::new(1.5, Color32::from_rgba_unmultiplied(210, 210, 210, 220)),
        egui::StrokeKind::Inside,
    );

    let clip = painter.with_clip_rect(box_rect.shrink(6.0));
    let label_x = box_rect.left() + padding;
    let colon_x = label_x + label_width + 10.0 * scale;
    let value_x = colon_x + colon.size().x + 10.0 * scale;
    for (row, (label, value)) in label_galleys.iter().zip(value_galleys.iter()).enumerate() {
        let center_y = box_rect.top() + padding + row_height * (row as f32 + 0.5);
        clip.galley_with_override_text_color(
            Pos2::new(label_x, center_y - label.size().y * 0.5),
            label.clone(),
            FG,
        );
        clip.galley_with_override_text_color(
            Pos2::new(colon_x, center_y - colon.size().y * 0.5),
            colon.clone(),
            DIM,
        );
        clip.galley_with_override_text_color(
            Pos2::new(value_x, center_y - value.size().y * 0.5),
            value.clone(),
            FG,
        );
    }
}

fn draw_plain(
    painter: &egui::Painter,
    rect: Rect,
    font: &FontId,
    y_center: f32,
    text: &str,
    color: Color32,
) {
    if text.is_empty() {
        return;
    }
    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
    let x = rect.center().x - galley.size().x * 0.5;
    let y = y_center - galley.size().y * 0.5;
    painter.galley_with_override_text_color(Pos2::new(x, y), galley, color);
}

/// Build and show one native dialog. This runs on the worker thread so the
/// UI thread stays responsive.
fn run_dialog(kind: DialogKind) -> Option<Vec<PathBuf>> {
    match kind {
        DialogKind::Open => rfd::FileDialog::new()
            .add_filter(&tr("Karaoke"), &["kar", "mid", "midi"])
            .add_filter(&tr("Playlist JSON"), &["json"])
            .add_filter(&tr("All files"), &["*"])
            .pick_file()
            .map(|path| vec![path]),
        DialogKind::AddFiles => rfd::FileDialog::new()
            .add_filter(&tr("Karaoke"), &["kar", "mid", "midi"])
            .pick_files(),
        DialogKind::AddFolder => rfd::FileDialog::new().pick_folder().map(|path| vec![path]),
        DialogKind::ImportPlaylist => rfd::FileDialog::new()
            .add_filter(&tr("Playlist JSON"), &["json"])
            .pick_file()
            .map(|path| vec![path]),
        DialogKind::ExportPlaylist => rfd::FileDialog::new()
            .add_filter(&tr("Playlist JSON"), &["json"])
            .set_file_name("playlist.json")
            .save_file()
            .map(|path| vec![path]),
        DialogKind::SoundFont => rfd::FileDialog::new()
            .add_filter(&tr("SoundFont"), &["sf2"])
            .pick_file()
            .map(|path| vec![path]),
        DialogKind::LyricFont => rfd::FileDialog::new()
            .add_filter(&tr("Font"), &["ttf", "otf"])
            .pick_file()
            .map(|path| vec![path]),
    }
}

/// One percentage row on the Text settings page.
fn percent_row(ui: &mut egui::Ui, label: &str, scale: &mut f32, min: f32, max: f32) -> bool {
    let mut changed = false;
    let mut percent = (*scale * 100.0).round();
    ui.horizontal(|ui| {
        ui.add_sized(
            [110.0, 22.0],
            egui::Label::new(RichText::new(label).size(12.0)),
        );
        let response = ui.add_sized(
            [120.0, 24.0],
            egui::DragValue::new(&mut percent)
                .range(min..=max)
                .speed(1.0)
                .fixed_decimals(0)
                .suffix("%"),
        );
        if response.changed() {
            *scale = percent / 100.0;
            changed = true;
        }
    });
    changed
}

/// One whole-second row on the General/Player settings pages.
fn seconds_row(ui: &mut egui::Ui, label: &str, seconds: &mut u32, max: u32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let response = ui.add_sized(
            [120.0, 24.0],
            egui::DragValue::new(seconds)
                .range(0..=max)
                .speed(1.0)
                .suffix(" s"),
        );
        if response.changed() {
            changed = true;
        }
        ui.add_sized(
            [110.0, 22.0],
            egui::Label::new(RichText::new(label).size(12.0)),
        );
    });
    changed
}

/// One visibility checkbox on the General settings page.
fn check_row(ui: &mut egui::Ui, label: &str, value: &mut bool) -> bool {
    ui.add(egui::Checkbox::new(value, RichText::new(label).size(12.0)))
        .changed()
}

/// A fixed-width context menu row with a monochrome icon column.
///
/// All rows share the same width, so the labels line up and the hover
/// highlight covers the whole menu consistently.
/// The "Song Player" flyout of the central context menu.
fn song_player_submenu(
    ctx: &egui::Context,
    pos: Pos2,
    alpha: f32,
    repeat: bool,
) -> (Option<MenuAction>, Rect) {
    const WIDTH: f32 = 188.0;
    const HEIGHT: f32 = 205.0;
    let screen = ctx.content_rect();
    let pos = Pos2::new(
        pos.x.min(screen.right() - WIDTH).max(screen.left()),
        pos.y.min(screen.bottom() - HEIGHT).max(screen.top()),
    );

    let mut action = None;
    let area = egui::Area::new(egui::Id::unique("song_player_submenu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            let fill = Color32::from_rgba_unmultiplied(8, 8, 8, (255.0 * alpha) as u8);
            let stroke = Color32::from_rgba_unmultiplied(70, 70, 70, (255.0 * alpha) as u8);
            egui::Frame::NONE
                .fill(fill)
                .stroke(egui::Stroke::new(1.0, stroke))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(6, 6))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 2.0);
                    ui.set_opacity(alpha);
                    if menu_item(ui, Icon::File, &tr("Open file…")) {
                        action = Some(MenuAction::Open);
                    }
                    if menu_item(ui, Icon::FilePlus, &tr("Add files…")) {
                        action = Some(MenuAction::AddFiles);
                    }
                    if menu_item(ui, Icon::FolderPlus, &tr("Add folder…")) {
                        action = Some(MenuAction::AddFolder);
                    }
                    ui.separator();
                    if menu_item(ui, Icon::Download, &tr("Import playlist…")) {
                        action = Some(MenuAction::ImportPlaylist);
                    }
                    if menu_item(ui, Icon::Upload, &tr("Export playlist…")) {
                        action = Some(MenuAction::ExportPlaylist);
                    }
                    ui.separator();
                    let repeat_label = if repeat {
                        tr("Repeat: On")
                    } else {
                        tr("Repeat: Off")
                    };
                    if menu_item(ui, Icon::Repeat2, &repeat_label) {
                        action = Some(MenuAction::ToggleRepeat);
                    }
                    if menu_item(ui, Icon::Trash2, &tr("Clear playlist")) {
                        action = Some(MenuAction::ClearPlaylist);
                    }
                });
        });

    (action, area.response.rect)
}

fn menu_item(ui: &mut egui::Ui, icon: Icon, label: &str) -> bool {
    menu_item_response(ui, icon, label).clicked()
}

/// A fixed-width context menu row with a monochrome icon column.
fn menu_item_response(ui: &mut egui::Ui, icon: Icon, label: &str) -> egui::Response {
    const WIDTH: f32 = 176.0;
    const HEIGHT: f32 = 20.0;

    let (rect, response) = ui.allocate_exact_size(egui::vec2(WIDTH, HEIGHT), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    let visuals = ui.visuals();
    let text_color = if ui.is_enabled() {
        visuals.text_color()
    } else {
        visuals.widgets.noninteractive.fg_stroke.color
    };
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
    }

    let painter = ui.painter();
    icons::paint(
        painter,
        Pos2::new(rect.left() + 15.0, rect.center().y),
        icon,
        16.0,
        text_color,
    );
    painter.text(
        Pos2::new(rect.left() + 28.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        text_color,
    );

    response
}

fn text_visibility_row(ui: &mut egui::Ui, value: &mut TextVisibility) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [110.0, 22.0],
            egui::Label::new(RichText::new(tr("Text visibility")).size(12.0)),
        );
        egui::ComboBox::from_id_salt("text_visibility")
            .width(120.0)
            .selected_text(text_visibility_label(*value))
            .show_ui(ui, |ui| {
                for option in [
                    TextVisibility::Center,
                    TextVisibility::Upper,
                    TextVisibility::Lower,
                ] {
                    if ui
                        .selectable_value(value, option, text_visibility_label(option))
                        .changed()
                    {
                        changed = true;
                    }
                }
            });
    });
    changed
}

fn cursor_visibility_row(ui: &mut egui::Ui, value: &mut CursorVisibility) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [110.0, 22.0],
            egui::Label::new(RichText::new(tr("Cursor visibility")).size(12.0)),
        );
        egui::ComboBox::from_id_salt("cursor_visibility")
            .width(120.0)
            .selected_text(cursor_visibility_label(*value))
            .show_ui(ui, |ui| {
                for option in [
                    CursorVisibility::ModernUpper,
                    CursorVisibility::LegacyCursor,
                ] {
                    if ui
                        .selectable_value(value, option, cursor_visibility_label(option))
                        .changed()
                    {
                        changed = true;
                    }
                }
            });
    });
    changed
}

/// The upcoming-line depth control: 0% is fully bright, 50% is the dimmest.
fn lyric_depth_row(ui: &mut egui::Ui, depth: &mut f32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [110.0, 22.0],
            egui::Label::new(RichText::new(tr("Lyric depth")).size(12.0)),
        );
        let response = ui.add_sized(
            [120.0, 24.0],
            egui::DragValue::new(depth)
                .range(0.0..=50.0)
                .speed(1.0)
                .fixed_decimals(0)
                .suffix("%"),
        );
        if response.changed() {
            changed = true;
        }
        let _ = response.on_hover_text(tr("0% = brightest, 50% = dimmest"));
    });
    changed
}

fn text_visibility_label(value: TextVisibility) -> String {
    match value {
        TextVisibility::Center => tr("Center"),
        TextVisibility::Upper => tr("Upper"),
        TextVisibility::Lower => tr("Lower"),
    }
}

fn cursor_visibility_label(value: CursorVisibility) -> String {
    match value {
        CursorVisibility::ModernUpper => tr("Modern Upper"),
        CursorVisibility::LegacyCursor => tr("Legacy Cursor"),
    }
}

fn font_style_label(value: FontStyle) -> String {
    match value {
        FontStyle::Regular => tr("Regular"),
        FontStyle::Bold => tr("Bold"),
        FontStyle::Custom => tr("Custom"),
    }
}

/// Text shown in the key box next to the song info. The offset is not shown
/// here: the box only displays the sounding key.
fn key_label(key: &str, transpose: i32) -> String {
    let sounding = sounding_key(key, transpose);
    tr_args(" KEY : {} ", &[&sounding as &dyn std::fmt::Display])
}

/// The sounding key after transposition, keeping the original spelling when
/// the offset is zero. Unknown keys are returned unchanged.
fn sounding_key(key: &str, transpose: i32) -> String {
    let original = key.trim();
    transposed_key(original, transpose).unwrap_or_else(|| original.to_owned())
}

/// Key shown on the song card: the original spelling while untransposed, and
/// `A# [+3]` / `Bbm [-2]` once the key has been moved.
fn card_key(key: &str, transpose: i32) -> String {
    if transpose == 0 {
        key.trim().to_owned()
    } else {
        key_with_offset(key, transpose)
    }
}

/// Format the sounding key and its offset: `G` with +3 becomes `A# [+3]`,
/// `Cm` with -2 becomes `Bbm [-2]` and an untransposed key keeps `[0]`.
///
/// Upward transposition uses sharps and downward transposition uses flats; at
/// zero the original spelling is preserved. Keys the parser does not
/// understand are shown unchanged with the offset appended.
fn key_with_offset(key: &str, transpose: i32) -> String {
    let shown = sounding_key(key, transpose);
    format!("{shown} [{}]", signed_offset(transpose))
}

/// `+3`, `-2` or `0`, with the sign in front of the number.
fn signed_offset(value: i32) -> String {
    if value > 0 {
        format!("+{value}")
    } else {
        value.to_string()
    }
}

/// Apply the user's BPM adjustment to a tempo-map value.
fn adjusted_bpm(base: f64, adjust: i32) -> f64 {
    (base + adjust as f64).max(1.0)
}

/// BPM value and the user's adjustment. The bracket is omitted when
/// `show_zero` is false and the tempo has not been changed.
fn bpm_text(adjust: i32, current: f64, show_zero: bool) -> String {
    let current = current.round() as i32;
    if adjust == 0 && !show_zero {
        current.to_string()
    } else {
        format!("{current} [{}]", signed_offset(adjust))
    }
}

/// Top-right readout: `BPM : 110 [+10]` (always shows the offset).
fn bpm_readout(adjust: i32, current: f64) -> String {
    format!("BPM : {}", bpm_text(adjust, current, true))
}

/// Song-card value: `110 [+10]` after a change, or `100` when unchanged.
fn card_bpm(adjust: i32, current: f64) -> String {
    bpm_text(adjust, current, false)
}

fn transposed_key(key: &str, transpose: i32) -> Option<String> {
    let (pitch, minor) = parse_key(key)?;
    if transpose == 0 {
        return Some(key.to_owned());
    }

    const SHARPS: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    const FLATS: [&str; 12] = [
        "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
    ];

    let names = if transpose > 0 { SHARPS } else { FLATS };
    let index = (pitch as i32 + transpose).rem_euclid(12) as usize;
    let mut name = names[index].to_owned();
    if minor {
        name.push('m');
    }
    Some(name)
}

/// Parse a key line such as `G`, `F#m` or `Bbm` into a pitch class (C = 0)
/// and whether it is minor. Returns `None` for anything else.
fn parse_key(key: &str) -> Option<(u8, bool)> {
    let mut chars = key.trim().chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };

    let rest: String = chars.collect();
    let rest = rest.trim_start();
    let (accidental, rest) = if let Some(rest) = rest.strip_prefix('#') {
        (1, rest)
    } else if let Some(rest) = rest.strip_prefix('♯') {
        (1, rest)
    } else if let Some(rest) = rest.strip_prefix('b') {
        (-1, rest)
    } else if let Some(rest) = rest.strip_prefix('♭') {
        (-1, rest)
    } else {
        (0, rest)
    };

    let minor = match rest.trim() {
        "" => false,
        "m" => true,
        _ => return None,
    };

    Some(((base + accidental).rem_euclid(12) as u8, minor))
}

fn display_field(ui: &mut egui::Ui, text: &str, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 25.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, Color32::from_gray(26));
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, Color32::from_gray(64)),
        egui::StrokeKind::Inside,
    );
    painter
        .with_clip_rect(rect.shrink2(egui::vec2(8.0, 0.0)))
        .text(
            Pos2::new(rect.left() + 9.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            FontId::proportional(13.0),
            FG,
        );
}

fn color_row(ui: &mut egui::Ui, label: &str, hex: &mut String, fallback: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [38.0, 22.0],
            egui::Label::new(RichText::new(label).size(12.0)),
        );
        if ui
            .add_sized([82.0, 24.0], egui::TextEdit::singleline(hex))
            .changed()
        {
            changed = true;
        }
        let mut rgb = parse_hex(hex)
            .or_else(|| parse_hex(fallback))
            .unwrap_or([255, 255, 255]);
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            *hex = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
            changed = true;
        }
    });
    changed
}

/// Hex field plus swatch for the background colour (no row label).
fn background_color_row(ui: &mut egui::Ui, hex: &mut String) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui
            .add_sized([120.0, 24.0], egui::TextEdit::singleline(hex))
            .changed()
        {
            changed = true;
        }
        let mut rgb = parse_hex(hex).unwrap_or([0, 0, 0]);
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            *hex = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
            changed = true;
        }
    });
    changed
}

/// Bordered preview containing one monitor per common aspect ratio.
fn background_preview(ui: &mut egui::Ui, color: Color32) {
    egui::Frame::NONE
        .fill(BG)
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(70)))
        .corner_radius(6.0)
        .inner_margin(egui::Margin {
            left: 14,
            right: 14,
            top: 6,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(240.0);
            preview_monitor(ui, "4:3", 4.0 / 3.0, color, 156.0);
            ui.add_space(8.0);
            preview_monitor(ui, "16:9", 16.0 / 9.0, color, 200.0);
        });
}

fn preview_monitor(ui: &mut egui::Ui, label: &str, aspect: f32, color: Color32, width: f32) {
    let height = (width / aspect).round();
    let label_height = 20.0;
    let full_width = ui.available_width().max(width);
    let (outer, _) = ui.allocate_exact_size(
        egui::vec2(full_width, height + label_height),
        egui::Sense::hover(),
    );

    // Centre the monitor in the preview box and keep the label pinned to its
    // top-left corner.
    let x = outer.center().x - width * 0.5;
    let painter = ui.painter();
    painter.text(
        Pos2::new(x, outer.top()),
        egui::Align2::LEFT_TOP,
        label,
        FontId::proportional(12.0),
        DIM,
    );

    let screen = Rect::from_min_size(
        Pos2::new(x, outer.top() + label_height),
        egui::vec2(width, height),
    );
    painter.rect_filled(screen, 2.0, color);
    painter.rect_stroke(
        screen,
        2.0,
        egui::Stroke::new(1.5, FG),
        egui::StrokeKind::Inside,
    );
}

fn parse_hex(value: &str) -> Option<[u8; 3]> {
    let digits = value.trim().strip_prefix('#')?;
    let bytes = digits.as_bytes();
    if bytes.len() != 6 || !bytes.iter().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let parse = |index: usize| {
        u8::from_str_radix(std::str::from_utf8(&bytes[index..index + 2]).ok()?, 16).ok()
    };
    Some([parse(0)?, parse(2)?, parse(4)?])
}

#[derive(Clone, Copy)]
struct LyricColors {
    lyric_text: Color32,
    lyric_edge: Color32,
    cursor_text: Color32,
    cursor_edge: Color32,
}

fn lyric_colors(settings: &Settings) -> LyricColors {
    LyricColors {
        lyric_text: color_or(&settings.lyric_text_color, [255, 255, 255]),
        lyric_edge: color_or(&settings.lyric_edge_color, [0, 0, 0]),
        cursor_text: color_or(&settings.cursor_text_color, [0, 0, 255]),
        cursor_edge: color_or(&settings.cursor_edge_color, [255, 255, 255]),
    }
}

fn color_or(value: &str, fallback: [u8; 3]) -> Color32 {
    let [red, green, blue] = parse_hex(value).unwrap_or(fallback);
    Color32::from_rgb(red, green, blue)
}

fn with_alpha(color: Color32, factor: f32) -> Color32 {
    let alpha = (color.a() as f32 * factor.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn lyric_font_family(style: FontStyle, custom_loaded: bool) -> FontFamily {
    match style {
        FontStyle::Regular => FontFamily::Proportional,
        FontStyle::Bold => FontFamily::Name("bold".into()),
        FontStyle::Custom if custom_loaded => FontFamily::Name("lyric".into()),
        FontStyle::Custom => FontFamily::Proportional,
    }
}

/// Which fixed row the current and next lyric lines occupy.
///
/// Row 0 is the upper row and row 1 is the lower row. Modern Upper always
/// puts the current line on top; Legacy Cursor alternates the current row so
/// the next line replaces the other row (the classic Thai karaoke style).
fn lyric_rows(mode: CursorVisibility, index: usize) -> (usize, usize) {
    match mode {
        CursorVisibility::ModernUpper => (0, 1),
        CursorVisibility::LegacyCursor => (index % 2, (index + 1) % 2),
    }
}

/// Draw the outline of a text galley behind its fill.
fn draw_outline(
    painter: &egui::Painter,
    pos: Pos2,
    galley: Arc<egui::Galley>,
    edge: Color32,
    thickness: f32,
) {
    if thickness > 0.05 && edge.a() > 0 {
        for (dx, dy) in [
            (-1.0f32, 0.0f32),
            (1.0, 0.0),
            (0.0, -1.0),
            (0.0, 1.0),
            (-0.7, -0.7),
            (0.7, -0.7),
            (-0.7, 0.7),
            (0.7, 0.7),
        ] {
            painter.galley_with_override_text_color(
                pos + egui::vec2(dx * thickness, dy * thickness),
                galley.clone(),
                edge,
            );
        }
    }
}

/// Draw a text galley with a solid coloured outline behind the fill.
fn draw_outlined_galley(
    painter: &egui::Painter,
    pos: Pos2,
    galley: Arc<egui::Galley>,
    fill: Color32,
    edge: Color32,
    thickness: f32,
) {
    draw_outline(painter, pos, galley.clone(), edge, thickness);
    painter.galley_with_override_text_color(pos, galley, fill);
}

/// Opacity of the upcoming line from the depth percentage: 0% keeps it fully
/// bright, 50% halves it.
fn next_line_opacity(depth: f32) -> f32 {
    (1.0 - depth.clamp(0.0, 50.0) / 100.0).clamp(0.5, 1.0)
}

fn draw_next_lyric(
    painter: &egui::Painter,
    rect: Rect,
    font: &FontId,
    y_center: f32,
    text: &str,
    colors: LyricColors,
    edge: f32,
    depth: f32,
) {
    if text.is_empty() {
        return;
    }
    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), colors.lyric_text);
    let pos = Pos2::new(
        rect.center().x - galley.size().x * 0.5,
        y_center - galley.size().y * 0.5,
    );
    let opacity = next_line_opacity(depth);
    draw_outlined_galley(
        painter,
        pos,
        galley,
        with_alpha(colors.lyric_text, opacity),
        with_alpha(colors.lyric_edge, opacity),
        edge,
    );
}

/// Largest font size (<= `base`) at which every line still fits `width`.
fn fit_size(
    painter: &egui::Painter,
    width: f32,
    base: f32,
    texts: &[&str],
    family: &FontFamily,
) -> f32 {
    let max_width = width * 0.88;
    let mut size = base;

    for _ in 0..3 {
        let font = FontId::new(size, family.clone());
        let widest = texts
            .iter()
            .filter(|text| !text.is_empty())
            .map(|text| {
                painter
                    .layout_no_wrap((*text).to_owned(), font.clone(), FG)
                    .size()
                    .x
            })
            .fold(0.0f32, f32::max);

        if widest <= max_width || size <= 16.0 {
            break;
        }
        size = (size * (max_width / widest)).max(16.0);
    }

    size
}

/// Draw the current line: unsung text uses the lyric colours, the sung part
/// uses the cursor colours.
///
/// The leading edge of the wipe is feathered over a few dozen pixels so the
/// highlight glides instead of snapping from one pixel column to the next.
#[allow(clippy::too_many_arguments)]
fn draw_wipe(
    painter: &egui::Painter,
    rect: Rect,
    font: &FontId,
    y_center: f32,
    line: &LyricLine,
    now: u64,
    colors: LyricColors,
    edge: f32,
) {
    if line.text.is_empty() {
        return;
    }

    let galley = painter.layout_no_wrap(line.text.clone(), font.clone(), colors.lyric_text);
    let x = rect.center().x - galley.size().x * 0.5;
    let pos = Pos2::new(x, y_center - galley.size().y * 0.5);
    let full_width = galley.size().x;

    // Clip horizontally only: Thai upper vowels and tone marks can rise above
    // the galley's own height, so the vertical range must be the whole panel.
    //
    // Thai leading vowels (ไ ใ โ) have a negative left side bearing in
    // Sarabun, so their ink starts a little left of the galley origin. Pad the
    // left edge of the sung band, otherwise that sliver stays in the unsung
    // colours until the whole line finishes.
    let left_overhang = font.size * 0.2;
    let band = |from: f32, to: f32| {
        let from = if from <= 0.0 {
            from - left_overhang
        } else {
            from
        };
        Rect::from_min_max(
            Pos2::new(pos.x + from, rect.top()),
            Pos2::new(pos.x + to, rect.bottom()),
        )
        .intersect(rect)
    };

    // Unsung text (base lyric colours).
    draw_outlined_galley(
        painter,
        pos,
        galley.clone(),
        colors.lyric_text,
        colors.lyric_edge,
        edge,
    );

    let width = wipe_width(painter, font, line, now);
    if width <= 0.0 {
        return;
    }

    // Once the line is finished, show it completely in the cursor colours.
    if width >= full_width - 0.5 {
        draw_outlined_galley(
            painter,
            pos,
            galley,
            colors.cursor_text,
            colors.cursor_edge,
            edge,
        );
        return;
    }

    // Solid part of the sung text.
    let feather = (font.size * 0.35).max(6.0);
    let half = feather * 0.5;
    let hard_end = (width - half).max(0.0);
    let soft_end = (width + half).min(full_width);

    if hard_end > 0.0 {
        let clip = band(0.0, hard_end);
        if clip.width() > 0.0 {
            let clipped = painter.with_clip_rect(clip);
            draw_outlined_galley(
                &clipped,
                pos,
                galley.clone(),
                colors.cursor_text,
                colors.cursor_edge,
                edge,
            );
        }
    }

    // Feathered edge so the cursor dissolves into the unsung text.
    if soft_end > hard_end {
        const STEPS: usize = 12;
        let span = soft_end - hard_end;

        for step in 0..STEPS {
            let t0 = step as f32 / STEPS as f32;
            let t1 = (step + 1) as f32 / STEPS as f32;
            let a = hard_end + span * t0;
            let b = hard_end + span * t1;
            let alpha = 1.0 - (t0 + t1) * 0.5;
            let fill = with_alpha(colors.cursor_text, alpha);
            let edge_color = with_alpha(colors.cursor_edge, alpha);

            let clip = band(a, b);
            if clip.width() > 0.0 {
                let clipped = painter.with_clip_rect(clip);
                draw_outlined_galley(&clipped, pos, galley.clone(), fill, edge_color, edge);
            }
        }
    }
}

/// X offset of the wipe edge inside the current line.
///
/// Base characters are grouped with their combining marks so a Thai vowel or
/// tone mark is highlighted together with the character it belongs to. Widths
/// are measured from the text prefix: egui's `pos_from_cursor` indexes glyphs
/// rather than characters, which is wrong for scripts with combining marks.
fn wipe_width(painter: &egui::Painter, font: &FontId, line: &LyricLine, now: u64) -> f32 {
    let syllables = &line.syllables;
    let mut prefix = String::new();
    let mut i = 0usize;

    while i < syllables.len() {
        let mut j = i + 1;
        while j < syllables.len() && is_combining(&syllables[j].text) {
            j += 1;
        }

        let start_us = syllables[i].start_us;
        let end_us = syllables[j - 1].end_us;

        if now < start_us {
            return measure(painter, font, &prefix);
        }
        if now < end_us {
            let x0 = measure(painter, font, &prefix);
            let mut with_cluster = prefix.clone();
            for syllable in &syllables[i..j] {
                with_cluster.push_str(&syllable.text);
            }
            let x1 = measure(painter, font, &with_cluster);
            let span = (end_us - start_us).max(1) as f32;
            let fraction = ((now - start_us) as f32 / span).clamp(0.0, 1.0);
            return x0 + (x1 - x0) * fraction;
        }

        for syllable in &syllables[i..j] {
            prefix.push_str(&syllable.text);
        }
        i = j;
    }

    measure(painter, font, &prefix)
}

fn measure(painter: &egui::Painter, font: &FontId, text: &str) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    painter
        .layout_no_wrap(text.to_owned(), font.clone(), FG)
        .size()
        .x
}

fn is_combining(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_combining_char)
}

/// Combining marks used by Thai karaoke lyrics (plus the common Unicode
/// combining blocks, in case a file contains Latin or other scripts).
fn is_combining_char(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F
            | 0x0E31
            | 0x0E34..=0x0E3A
            | 0x0E47..=0x0E4E
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
    )
}

fn fmt_time(us: u64) -> String {
    let total_ms = us / 1_000;
    let ms = total_ms % 1_000;
    let seconds = total_ms / 1_000 % 60;
    let minutes = total_ms / 60_000;
    format!("{minutes:02}:{seconds:02}.{ms:03}")
}

/// Text shown by the clock during its boot animation.
///
/// Register a click on the clock and report whether the sixth click in a
/// short series triggers the Easter egg.
fn register_clock_click(clicks: &mut u32, last_click: &mut f64, now: f64) -> bool {
    const CLICK_WINDOW: f64 = 2.0;
    const CLICKS_TO_TRIGGER: u32 = 6;

    if now - *last_click > CLICK_WINDOW {
        *clicks = 0;
    }
    *last_click = now;
    *clicks += 1;
    if *clicks >= CLICKS_TO_TRIGGER {
        *clicks = 0;
        true
    } else {
        false
    }
}

/// `elapsed` is seconds since the first clock frame. The six zeros appear
/// one character at a time, hold for a moment, blink once (off, then on),
/// and finally the normal `HH:MM:SS` text takes over.
fn clock_animation_text(elapsed: f64, normal: &str) -> String {
    const ZEROS: &str = "00:00:00";
    const REVEAL: f64 = 0.72;
    const HOLD: f64 = 0.50;
    const BLINK: f64 = 0.40;

    if elapsed < REVEAL {
        let shown = ((elapsed / REVEAL) * ZEROS.len() as f64).floor() as usize;
        ZEROS[..shown.min(ZEROS.len())].to_owned()
    } else if elapsed < REVEAL + HOLD {
        ZEROS.to_owned()
    } else if elapsed < REVEAL + HOLD + BLINK {
        let blink_elapsed = elapsed - REVEAL - HOLD;
        if blink_elapsed < BLINK * 0.5 {
            String::new()
        } else {
            ZEROS.to_owned()
        }
    } else {
        normal.to_owned()
    }
}

/// True when `path` names a playlist JSON file.
fn is_playlist_json(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

/// Local wall-clock time as `(hour, minute, second)` in 24-hour form.
fn local_time_hms() -> (u32, u32, u32) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;

        let mut time = unsafe { std::mem::zeroed::<SYSTEMTIME>() };
        unsafe { GetLocalTime(&mut time) };
        (time.wHour as u32, time.wMinute as u32, time.wSecond as u32)
    }

    // Fallback for non-Windows builds: UTC.
    #[cfg(not(windows))]
    {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
            % 86_400;
        (
            (seconds / 3_600) as u32,
            (seconds / 60 % 60) as u32,
            (seconds % 60) as u32,
        )
    }
}

// -------------------------------------------------------------- fonts & style

/// Install the UI fonts plus, when provided, an external karaoke font.
///
/// Returns `true` when the external font was read successfully and the
/// `lyric` family is available.
fn install_fonts(ctx: &egui::Context, custom: Option<&Path>) -> bool {
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "sarabun".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/Sarabun-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "sarabun-bold".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/Sarabun-Bold.ttf"
        ))),
    );
    fonts.font_data.insert(
        "e1234".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/E1234-Regular.ttf"
        ))),
    );
    icons::install(&mut fonts);

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&family) {
            list.insert(0, "sarabun".to_owned());
        }
    }
    fonts.families.insert(
        FontFamily::Name("bold".into()),
        vec!["sarabun-bold".to_owned(), "sarabun".to_owned()],
    );
    fonts.families.insert(
        FontFamily::Name("clock".into()),
        vec!["e1234".to_owned(), "sarabun".to_owned()],
    );

    let mut custom_loaded = false;
    if let Some(path) = custom {
        if let Ok(bytes) = std::fs::read(path) {
            if looks_like_font(&bytes) {
                fonts.font_data.insert(
                    "lyric-custom".to_owned(),
                    Arc::new(egui::FontData::from_owned(bytes)),
                );
                // Sarabun stays as a fallback for glyphs missing from the
                // external font.
                fonts.families.insert(
                    FontFamily::Name("lyric".into()),
                    vec!["lyric-custom".to_owned(), "sarabun".to_owned()],
                );
                custom_loaded = true;
            }
        }
    }

    ctx.set_fonts(fonts);
    custom_loaded
}

/// Accept the TrueType/OpenType container signatures egui can parse.
fn looks_like_font(bytes: &[u8]) -> bool {
    let Some(head) = bytes.get(..4) else {
        return false;
    };
    head.starts_with(b"\x00\x01\x00\x00")
        || head.starts_with(b"OTTO")
        || head.starts_with(b"true")
        || head.starts_with(b"typ1")
}

fn install_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = BG;
    visuals.extreme_bg_color = Color32::from_gray(16);
    visuals.faint_bg_color = Color32::from_gray(20);
    visuals.override_text_color = Some(FG);

    visuals.widgets.noninteractive.bg_fill = BG;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, Color32::from_gray(40));
    visuals.widgets.inactive.bg_fill = Color32::from_gray(26);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(26);
    visuals.widgets.hovered.bg_fill = Color32::from_gray(48);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_gray(48);
    visuals.widgets.active.bg_fill = Color32::from_gray(72);
    visuals.widgets.active.weak_bg_fill = Color32::from_gray(72);
    visuals.selection.bg_fill = Color32::from_gray(80);
    visuals.selection.stroke = egui::Stroke::new(1.0, FG);

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_rows_match_the_selected_mode() {
        assert_eq!(lyric_rows(CursorVisibility::ModernUpper, 0), (0, 1));
        assert_eq!(lyric_rows(CursorVisibility::ModernUpper, 7), (0, 1));

        assert_eq!(lyric_rows(CursorVisibility::LegacyCursor, 0), (0, 1));
        assert_eq!(lyric_rows(CursorVisibility::LegacyCursor, 1), (1, 0));
        assert_eq!(lyric_rows(CursorVisibility::LegacyCursor, 2), (0, 1));
        assert_eq!(lyric_rows(CursorVisibility::LegacyCursor, 3), (1, 0));
    }

    #[test]
    fn next_line_depth_is_brightest_at_zero_and_capped() {
        assert!((next_line_opacity(0.0) - 1.0).abs() < 1e-6);
        assert!((next_line_opacity(25.0) - 0.75).abs() < 1e-6);
        assert!((next_line_opacity(50.0) - 0.5).abs() < 1e-6);
        assert!((next_line_opacity(90.0) - 0.5).abs() < 1e-6);
        assert!((next_line_opacity(-5.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn recognises_supported_font_headers() {
        assert!(looks_like_font(&[0, 1, 0, 0, 9, 9]));
        assert!(looks_like_font(b"OTTO...."));
        assert!(looks_like_font(b"true...."));
        assert!(looks_like_font(b"typ1...."));
        assert!(!looks_like_font(b"RIFF...."));
        assert!(!looks_like_font(b""));
    }

    #[test]
    fn local_clock_fields_are_in_range() {
        let (hour, minute, second) = local_time_hms();
        assert!(hour < 24);
        assert!(minute < 60);
        assert!(second < 60);
    }

    #[test]
    fn clock_easter_egg_needs_six_quick_clicks() {
        let mut clicks = 0;
        let mut last = 0.0;
        for step in 0..5 {
            assert!(!register_clock_click(
                &mut clicks,
                &mut last,
                10.0 + step as f64 * 0.2
            ));
        }
        assert!(register_clock_click(&mut clicks, &mut last, 11.0));
        assert_eq!(clicks, 0);

        // Slow clicks start a new series instead of accumulating.
        let mut clicks = 0;
        let mut last = 0.0;
        assert!(!register_clock_click(&mut clicks, &mut last, 10.0));
        assert!(!register_clock_click(&mut clicks, &mut last, 13.0));
        assert_eq!(clicks, 1);
    }

    #[test]
    fn clock_boot_animation_reveals_then_blinks() {
        let normal = "12:34:56";
        assert_eq!(clock_animation_text(0.0, normal), "");
        assert_eq!(clock_animation_text(0.2, normal), "00");
        assert_eq!(clock_animation_text(0.63, normal), "00:00:0");
        assert_eq!(clock_animation_text(0.9, normal), "00:00:00");
        assert_eq!(clock_animation_text(1.4, normal), "");
        assert_eq!(clock_animation_text(1.5, normal), "00:00:00");
        assert_eq!(clock_animation_text(1.7, normal), normal);
    }

    #[test]
    fn custom_font_family_falls_back_when_not_loaded() {
        assert!(matches!(
            lyric_font_family(FontStyle::Custom, false),
            FontFamily::Proportional
        ));
        assert!(matches!(
            lyric_font_family(FontStyle::Custom, true),
            FontFamily::Name(name) if name.as_ref() == "lyric"
        ));
    }

    #[test]
    fn key_label_matches_the_song_info_box() {
        assert_eq!(key_label("Cm", 0), " KEY : Cm ");
        assert_eq!(key_label("  F#m ", 0), " KEY : F#m ");
        assert_eq!(key_label("G", 3), " KEY : A# ");
        assert_eq!(key_label("Cm", -2), " KEY : Bbm ");
        assert_eq!(key_label("?", 1), " KEY : ? ");
    }

    #[test]
    fn song_card_hides_the_zero_offset() {
        assert_eq!(card_key("Cm", 0), "Cm");
        assert_eq!(card_key("  G ", 0), "G");
        assert_eq!(card_key("G", 3), "A# [+3]");
        assert_eq!(card_key("Cm", -2), "Bbm [-2]");
        assert_eq!(card_key("?", 0), "?");
    }

    #[test]
    fn bpm_adjustment_is_a_constant_offset() {
        assert_eq!(adjusted_bpm(90.0, 10), 100.0);
        assert_eq!(adjusted_bpm(132.0, 10), 142.0);
        assert_eq!(adjusted_bpm(132.0, 0), 132.0);
        assert_eq!(adjusted_bpm(10.0, -20), 1.0);
    }

    #[test]
    fn bpm_readout_shows_the_current_tempo_and_offset() {
        assert_eq!(bpm_readout(0, 100.0), "BPM : 100 [0]");
        assert_eq!(bpm_readout(10, 110.0), "BPM : 110 [+10]");
        assert_eq!(bpm_readout(-5, 95.0), "BPM : 95 [-5]");
        assert_eq!(bpm_readout(0, 132.4), "BPM : 132 [0]");
        assert_eq!(bpm_readout(10, 142.0), "BPM : 142 [+10]");
    }

    #[test]
    fn song_card_omits_the_zero_bpm_offset() {
        assert_eq!(card_bpm(0, 100.0), "100");
        assert_eq!(card_bpm(10, 110.0), "110 [+10]");
        assert_eq!(card_bpm(-5, 95.0), "95 [-5]");
        assert_eq!(card_bpm(0, 67.4), "67");
        assert_eq!(card_bpm(10, 142.0), "142 [+10]");
    }

    #[test]
    fn transposed_keys_use_sharps_up_and_flats_down() {
        assert_eq!(key_with_offset("G", 0), "G [0]");
        assert_eq!(key_with_offset("G", 3), "A# [+3]");
        assert_eq!(key_with_offset("Cm", -2), "Bbm [-2]");
        assert_eq!(key_with_offset("C", 1), "C# [+1]");
        assert_eq!(key_with_offset("C", -1), "B [-1]");
        assert_eq!(key_with_offset("B", 1), "C [+1]");
        assert_eq!(key_with_offset("A", -3), "Gb [-3]");
        assert_eq!(key_with_offset("Bb", 2), "C [+2]");
        assert_eq!(key_with_offset("F#m", 2), "G#m [+2]");
        assert_eq!(key_with_offset("Ebm", -1), "Dm [-1]");
    }

    #[test]
    fn unknown_keys_keep_their_spelling_with_the_offset() {
        assert_eq!(key_with_offset("?", 3), "? [+3]");
        assert_eq!(key_with_offset("C major", 1), "C major [+1]");
        assert_eq!(parse_key("H"), None);
        assert_eq!(parse_key("C7"), None);
    }

    #[test]
    fn detects_playlist_json_files() {
        assert!(is_playlist_json(Path::new("list.json")));
        assert!(is_playlist_json(Path::new("LIST.JSON")));
        assert!(!is_playlist_json(Path::new("song.kar")));
        assert!(!is_playlist_json(Path::new("playlist.json.bak")));
    }

    #[test]
    fn imports_playlist_json_entries() {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/sample.kar");
        let dir =
            std::env::temp_dir().join(format!("karaoke-playlist-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let file = dir.join("playlist.json");
        PlaylistFile::new(vec![PlaylistEntry {
            path: sample.clone(),
            file_type: "kar".to_owned(),
            title: "Test".to_owned(),
            artist: "Artist".to_owned(),
        }])
        .save(&file)
        .expect("save playlist");

        let mut app = KaraokeApp::default();
        app.import_playlist_file(&file);
        assert_eq!(app.playlist, vec![sample]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
