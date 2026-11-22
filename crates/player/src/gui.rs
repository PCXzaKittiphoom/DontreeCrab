use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use eframe::egui;
use egui::{Align, Color32, FontFamily, FontId, Layout, Pos2, Rect, RichText};
use karaoke_player::audio::{
    self, AudioHandle, Command, MidiMonitor, MidiMonitorSnapshot, MidiOptions, Player,
};
use karaoke_player::i18n::{self, tr, tr_args};
use karaoke_player::library::{self, Library, LyricHit, ScanOutcome, SongRecord};
use karaoke_player::meter::MeterMap;
use karaoke_player::model::{EventKind, LyricLine, Song};
use karaoke_player::ncn;
use karaoke_player::playlist::{PlaylistEntry, PlaylistFile};
use karaoke_player::settings::{
    BackgroundStyle, CursorVisibility, FontStyle, MidiReset, Settings, TextVisibility,
    TransposeMode,
};
use karaoke_player::soundfont;
use karaoke_player::text::{EncodingChoice, decode};
use karaoke_player::{LoadOptions, load};
use rustysynth::SoundFont;
use unicode_segmentation::UnicodeSegmentation;

use crate::icons::{self, Icon};

const BG: Color32 = Color32::BLACK;
const FG: Color32 = Color32::WHITE;
const DIM: Color32 = Color32::from_gray(120);
const ERROR: Color32 = Color32::from_rgb(230, 90, 90);

/// MIDI channel 9 (1-based) is where Thai karaoke files put the melody.
const MELODY_CHANNEL: u8 = 8;

/// Fixed scale of the song search popup (120% of the fitted size).
const SEARCH_POPUP_SCALE: f32 = 1.2;

const GITHUB_URL: &str = "https://github.com/PCXzaKittiphoom/DontreeCrab";
const APP_VERSION_LABEL: &str = concat!(env!("CARGO_PKG_VERSION"), " Beta");
const LICENSE_TEXT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../LICENSE"));

/// Which About popup is open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AboutPopup {
    License,
    Created,
}

/// Windows that follow the main window when it is minimized.
fn child_viewports() -> [egui::ViewportId; 13] {
    [
        egui::ViewportId::from_hash_of("settings_window"),
        egui::ViewportId::from_hash_of("split_midi_window"),
        egui::ViewportId::from_hash_of("midi_editor_window"),
        egui::ViewportId::from_hash_of("midi_editor_popup_window"),
        egui::ViewportId::from_hash_of("midi_editor_confirm_window"),
        egui::ViewportId::from_hash_of("midi_editor_save_as_window"),
        egui::ViewportId::from_hash_of("song_data_window"),
        egui::ViewportId::from_hash_of("import_window"),
        egui::ViewportId::from_hash_of("about_license_window"),
        egui::ViewportId::from_hash_of("about_created_window"),
        egui::ViewportId::from_hash_of("exit_confirm_window"),
        egui::ViewportId::from_hash_of("song_restore_confirm_window"),
        egui::ViewportId::from_hash_of("library_delete_confirm_window"),
    ]
}

/// Separate confirmation window for deleting songs from the library.
fn library_delete_confirm_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("library_delete_confirm_window")
}

/// Confirmation shown before the Song Data page clears its folders.
fn song_restore_confirm_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("song_restore_confirm_window")
}

/// Actions offered by the right-click menus.
/// Background artwork shipped with the player; the default until the user
/// picks a colour or their own picture.
const DEFAULT_BACKGROUND: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/backgrounds/DontreeCrab_Background.png"
));

/// A command sent from the import window back to the player.
enum ImportIntent {
    OpenFolder,
    Add,
    Selected(usize, bool),
    SelectAll(bool),
    Filter(LibraryFilter),
}

/// State of the "Add a song" window, shared with its own OS viewport.
struct ImportWindow {
    open: bool,
    shown: bool,
    filter: LibraryFilter,
    items: Arc<Vec<ImportItem>>,
    view: Arc<Vec<usize>>,
    progress: f32,
    note: Option<String>,
    error: Option<String>,
    busy: bool,
    generation: u64,
    intents: Vec<ImportIntent>,
}

impl Default for ImportWindow {
    fn default() -> Self {
        Self {
            open: false,
            shown: false,
            filter: LibraryFilter::All,
            items: Arc::new(Vec::new()),
            view: Arc::new(Vec::new()),
            progress: 0.0,
            note: None,
            error: None,
            busy: false,
            generation: 0,
            intents: Vec::new(),
        }
    }
}

/// A command sent from the Song Data window back to the player.
enum LibraryIntent {
    QueryChanged,
    FilterChanged,
    Create,
    CheckMissing,
    Compact,
    Backup,
    AddSong,
    /// Delete songs from the database, and optionally their files.
    Delete {
        songs: Vec<(PathBuf, String)>,
        delete_files: bool,
    },
}

/// State of the Song Data window, shared with its own OS viewport.
struct LibraryWindow {
    open: bool,
    /// Last visibility sent to the OS window.
    shown: bool,
    query: String,
    filter: LibraryFilter,
    songs: Arc<Vec<SongRecord>>,
    view: Arc<Vec<usize>>,
    note: Option<String>,
    error: Option<String>,
    progress: f32,
    busy: bool,
    busy_label: Option<String>,
    loading: bool,
    songs_gen: u64,
    view_gen: u64,
    focus_query: bool,
    /// Song indices selected in the table (indices into `songs`).
    selected: HashSet<usize>,
    /// View position where the last click started, for Shift ranges.
    selection_anchor: Option<usize>,
    /// Songs waiting for the delete confirmation popup.
    delete_confirm: Option<Vec<(PathBuf, String)>>,
    /// Last visibility sent to the delete confirmation viewport.
    delete_confirm_shown: bool,
    intents: Vec<LibraryIntent>,
}

impl Default for LibraryWindow {
    fn default() -> Self {
        Self {
            open: false,
            shown: false,
            query: String::new(),
            filter: LibraryFilter::All,
            songs: Arc::new(Vec::new()),
            view: Arc::new(Vec::new()),
            note: None,
            error: None,
            progress: 0.0,
            busy: false,
            busy_label: None,
            loading: false,
            songs_gen: 0,
            view_gen: 0,
            focus_query: false,
            selected: HashSet::new(),
            selection_anchor: None,
            delete_confirm: None,
            delete_confirm_shown: false,
            intents: Vec::new(),
        }
    }
}

/// General MIDI program names, indexed by program number (0 = program 1).
const GM_PROGRAM_NAMES: [&str; 128] = [
    "Acoustic Grand Piano",
    "Bright Acoustic Piano",
    "Electric Grand Piano",
    "Honky-tonk Piano",
    "Electric Piano 1",
    "Electric Piano 2",
    "Harpsichord",
    "Clavinet",
    "Celesta",
    "Glockenspiel",
    "Music Box",
    "Vibraphone",
    "Marimba",
    "Xylophone",
    "Tubular Bells",
    "Dulcimer",
    "Drawbar Organ",
    "Percussive Organ",
    "Rock Organ",
    "Church Organ",
    "Reed Organ",
    "Accordion",
    "Harmonica",
    "Tango Accordion",
    "Acoustic Guitar (nylon)",
    "Acoustic Guitar (steel)",
    "Electric Guitar (jazz)",
    "Electric Guitar (clean)",
    "Electric Guitar (muted)",
    "Overdriven Guitar",
    "Distortion Guitar",
    "Guitar Harmonics",
    "Acoustic Bass",
    "Electric Bass (finger)",
    "Electric Bass (pick)",
    "Fretless Bass",
    "Slap Bass 1",
    "Slap Bass 2",
    "Synth Bass 1",
    "Synth Bass 2",
    "Violin",
    "Viola",
    "Cello",
    "Contrabass",
    "Tremolo Strings",
    "Pizzicato Strings",
    "Orchestral Harp",
    "Timpani",
    "String Ensemble 1",
    "String Ensemble 2",
    "SynthStrings 1",
    "SynthStrings 2",
    "Choir Aahs",
    "Voice Oohs",
    "Synth Voice",
    "Orchestra Hit",
    "Trumpet",
    "Trombone",
    "Tuba",
    "Muted Trumpet",
    "French Horn",
    "Brass Section",
    "SynthBrass 1",
    "SynthBrass 2",
    "Soprano Sax",
    "Alto Sax",
    "Tenor Sax",
    "Baritone Sax",
    "Oboe",
    "English Horn",
    "Bassoon",
    "Clarinet",
    "Piccolo",
    "Flute",
    "Recorder",
    "Pan Flute",
    "Blown Bottle",
    "Shakuhachi",
    "Whistle",
    "Ocarina",
    "Lead 1 (square)",
    "Lead 2 (sawtooth)",
    "Lead 3 (calliope)",
    "Lead 4 (chiff)",
    "Lead 5 (charang)",
    "Lead 6 (voice)",
    "Lead 7 (fifths)",
    "Lead 8 (bass + lead)",
    "Pad 1 (new age)",
    "Pad 2 (warm)",
    "Pad 3 (polysynth)",
    "Pad 4 (choir)",
    "Pad 5 (bowed)",
    "Pad 6 (metallic)",
    "Pad 7 (halo)",
    "Pad 8 (sweep)",
    "FX 1 (rain)",
    "FX 2 (soundtrack)",
    "FX 3 (crystal)",
    "FX 4 (atmosphere)",
    "FX 5 (brightness)",
    "FX 6 (goblins)",
    "FX 7 (echoes)",
    "FX 8 (sci-fi)",
    "Sitar",
    "Banjo",
    "Shamisen",
    "Koto",
    "Kalimba",
    "Bag pipe",
    "Fiddle",
    "Shanai",
    "Tinkle Bell",
    "Agogo",
    "Steel Drums",
    "Woodblock",
    "Taiko Drum",
    "Melodic Tom",
    "Synth Drum",
    "Reverse Cymbal",
    "Guitar Fret Noise",
    "Breath Noise",
    "Seashore",
    "Bird Tweet",
    "Telephone Ring",
    "Helicopter",
    "Applause",
    "Gunshot",
];
/// Size of the in-window MIDI Controller panel (top-left of the lyrics area).
const CONTROLLER_WIDTH: f32 = 700.0;
const CONTROLLER_HEIGHT: f32 = 203.0;
const CONTROLLER_KEYBOARD_WIDTH: f32 = 634.0;
const CONTROLLER_OFFSET: f32 = CONTROLLER_HEIGHT + 40.0;
/// Size of the per-channel detail popup (slightly smaller than the panel).
/// The layout below is authored on a 372x264 canvas and scaled to this size.
const CONTROLLER_DETAIL_SCALE: f32 = 0.8;
const CONTROLLER_DETAIL_WIDTH: f32 = 372.0 * CONTROLLER_DETAIL_SCALE;
const CONTROLLER_DETAIL_HEIGHT: f32 = 264.0 * CONTROLLER_DETAIL_SCALE;
const CONTROLLER_STRIP_WIDTH: f32 = 40.0;
const CONTROLLER_STRIP_GAP: f32 =
    ((CONTROLLER_WIDTH - 16.0) - 16.0 * CONTROLLER_STRIP_WIDTH) / 15.0;
const CONTROLLER_STRIP_PITCH: f32 = CONTROLLER_STRIP_WIDTH + CONTROLLER_STRIP_GAP;
/// Thin, slightly softened white outline for the two floating panels.
const CONTROLLER_BORDER: Color32 = Color32::from_rgb(220, 220, 220);
/// White keys of the 61-key C2..C7 keyboard.
const CONTROLLER_WHITE_NOTES: [u8; 36] = [
    36, 38, 40, 41, 43, 45, 47, 48, 50, 52, 53, 55, 57, 59, 60, 62, 64, 65, 67, 69, 71, 72, 74, 76,
    77, 79, 81, 83, 84, 86, 88, 89, 91, 93, 95, 96,
];

/// Controller position: top-left normally, or centred horizontally along the
/// bottom of the lyric area when the lyric lines have been moved to the top.
fn controller_panel_position(lyric: Rect, lyrics_upper: bool) -> Pos2 {
    if lyrics_upper {
        Pos2::new(
            lyric.center().x - CONTROLLER_WIDTH * 0.5,
            (lyric.bottom() - CONTROLLER_HEIGHT - 12.0).max(lyric.top() + 12.0),
        )
    } else {
        Pos2::new(lyric.left() + 12.0, lyric.top() + 12.0)
    }
}

/// Top-left of the channel detail popup: under the selected channel, clamped
/// to the visible lyric area. With the controller centred (lyrics on top) the
/// popup opens above the channel header instead.
fn controller_detail_position(panel: Pos2, lyric: Rect, channel: usize, above: bool) -> Pos2 {
    let strip_left = panel.x + 8.0 + channel as f32 * CONTROLLER_STRIP_PITCH;
    let x = strip_left
        .min(lyric.right() - CONTROLLER_DETAIL_WIDTH - 8.0)
        .max(lyric.left() + 8.0);
    let y = if above {
        (panel.y + 8.0 - CONTROLLER_DETAIL_HEIGHT - 4.0).max(lyric.top() + 4.0)
    } else {
        panel.y + 36.0
    };
    Pos2::new(x, y)
}

/// Draw one label box and horizontal fader in the channel detail popup.
fn paint_controller_detail_slider(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    origin: Pos2,
    top: f32,
    label: &str,
    value: &mut u8,
    id: egui::Id,
    enabled: bool,
    locked: bool,
) -> bool {
    let s = CONTROLLER_DETAIL_SCALE;
    let label_rect = Rect::from_min_size(
        origin + egui::vec2(6.0 * s, top * s),
        egui::vec2(72.0 * s, 36.0 * s),
    );
    let track_left = origin.x + 101.0 * s;
    let track_right = origin.x + 348.0 * s;
    let center_y = origin.y + (top + 18.0) * s;
    let track = Rect::from_min_max(
        Pos2::new(track_left, center_y - 4.0 * s),
        Pos2::new(track_right, center_y + 4.0 * s),
    );
    let hit = Rect::from_min_size(
        origin + egui::vec2(95.0 * s, top * s),
        egui::vec2(266.0 * s, 36.0 * s),
    );

    let mut changed = false;
    if enabled && !locked {
        let response = ui.interact(hit, id, egui::Sense::click_and_drag());
        if response.dragged() || response.clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let t = ((pointer.x - track_left) / (track_right - track_left)).clamp(0.0, 1.0);
                let next = (t * 127.0).round() as u8;
                if next != *value {
                    *value = next;
                    changed = true;
                }
            }
        }
    }

    painter.rect_filled(label_rect, 4.0, Color32::from_gray(127));
    painter.text(
        label_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.0 * s),
        if enabled {
            Color32::WHITE
        } else {
            Color32::from_gray(90)
        },
    );
    // Horizontal version of the controller's vertical channel fader: dark rail,
    // grey fill and a white pill handle.
    painter.rect_filled(track, 4.0 * s, Color32::from_gray(26));
    let knob_x = track_left + (track_right - track_left) * (*value as f32 / 127.0);
    if knob_x > track_left {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(track_left, track.top()),
                Pos2::new(knob_x, track.bottom()),
            ),
            4.0 * s,
            if enabled {
                Color32::from_gray(90)
            } else {
                Color32::from_gray(50)
            },
        );
    }
    painter.rect_filled(
        Rect::from_center_size(Pos2::new(knob_x, center_y), egui::vec2(10.0 * s, 22.0 * s)),
        4.0 * s,
        if locked {
            Color32::from_rgb(90, 225, 140)
        } else if enabled {
            Color32::WHITE
        } else {
            Color32::from_gray(160)
        },
    );
    changed
}

/// Draw one of the two bank spinner arrows in the channel detail popup.
fn paint_controller_detail_spinner(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    rect: Rect,
    up: bool,
    enabled: bool,
    id: egui::Id,
) -> bool {
    let response = ui.interact(
        rect,
        id,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let fill = if !enabled {
        Color32::from_gray(70)
    } else if response.hovered() {
        Color32::from_gray(145)
    } else {
        Color32::from_gray(127)
    };
    painter.rect_filled(rect, 3.0, fill);
    let s = CONTROLLER_DETAIL_SCALE;
    let center = rect.center();
    let points = if up {
        vec![
            Pos2::new(center.x - 7.0 * s, center.y + 3.0 * s),
            Pos2::new(center.x + 7.0 * s, center.y + 3.0 * s),
            Pos2::new(center.x, center.y - 4.0 * s),
        ]
    } else {
        vec![
            Pos2::new(center.x - 7.0 * s, center.y - 3.0 * s),
            Pos2::new(center.x + 7.0 * s, center.y - 3.0 * s),
            Pos2::new(center.x, center.y + 4.0 * s),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        points,
        if enabled {
            Color32::WHITE
        } else {
            Color32::from_gray(95)
        },
        egui::Stroke::NONE,
    ));
    response.clicked()
}

/// Drum kit name for a zero-based program number.
fn drum_kit_name(program: u8) -> String {
    let number = u32::from(program) + 1;
    DRUM_KITS
        .iter()
        .find(|(kit, _)| *kit == number)
        .map(|(_, name)| format!("Drum Kit {name}"))
        .unwrap_or_else(|| format!("Drum Kit {number}"))
}

/// "1 Standard" becomes "Standard" for the editor's patch list.
fn drum_kit_label(program: u8) -> String {
    let number = u32::from(program) + 1;
    DRUM_KITS
        .iter()
        .find(|(kit, _)| *kit == number)
        .and_then(|(_, name)| name.split_once(' ').map(|(_, label)| (*label).to_owned()))
        .unwrap_or_else(|| format!("Drum Kit {number}"))
}

fn midi_event_channel(kind: &EventKind) -> Option<u8> {
    match kind {
        EventKind::NoteOn { channel, .. }
        | EventKind::NoteOff { channel, .. }
        | EventKind::ControlChange { channel, .. }
        | EventKind::ProgramChange { channel, .. }
        | EventKind::PitchBend { channel, .. }
        | EventKind::ChannelPressure { channel, .. }
        | EventKind::KeyPressure { channel, .. } => Some(*channel),
        _ => None,
    }
}

fn editor_values(snapshot: &MidiMonitorSnapshot, channel: u8) -> MidiEditorValues {
    let channel = channel as usize;
    let channel = channel.min(15);
    MidiEditorValues {
        program: snapshot.program[channel],
        bank: snapshot.bank[channel],
        volume: snapshot.volume[channel],
        pan: snapshot.pan[channel],
        reverb: snapshot.reverb[channel],
        chorus: snapshot.chorus[channel],
    }
}

/// First ProgramChange and bank select (CC0) for every channel in the file.
/// Playback locks (bass program / drum kit) never touch `Song.events`.
fn file_instruments(song: &Song) -> ([Option<u8>; 16], [Option<u8>; 16]) {
    let mut programs = [None; 16];
    let mut banks = [None; 16];
    for event in &song.events {
        match &event.kind {
            EventKind::ProgramChange { channel, program } => {
                let channel = *channel as usize;
                if channel < 16 && programs[channel].is_none() {
                    programs[channel] = Some(*program);
                }
            }
            EventKind::ControlChange {
                channel,
                controller: 0,
                value,
            } => {
                let channel = *channel as usize;
                if channel < 16 && banks[channel].is_none() {
                    banks[channel] = Some(*value);
                }
            }
            _ => {}
        }
    }
    (programs, banks)
}

fn editor_program_label(program: u8, channel: Option<u8>) -> String {
    let number = u32::from(program) + 1;
    let name = if channel == Some(9) {
        drum_kit_label(program)
    } else {
        GM_PROGRAM_NAMES[program as usize].to_owned()
    };
    format!("{number:03} - {name}")
}

struct EditorColumns {
    order: f32,
    name: f32,
    channel: f32,
    instrument: f32,
    bank: f32,
    vol: f32,
    pan: f32,
    rev: f32,
    cho: f32,
}

fn editor_columns(left: f32, width: f32) -> EditorColumns {
    EditorColumns {
        order: left + width * 0.02,
        name: left + width * 0.065,
        channel: left + width * 0.285,
        instrument: left + width * 0.365,
        bank: left + width * 0.645,
        vol: left + width * 0.72,
        pan: left + width * 0.795,
        rev: left + width * 0.87,
        cho: left + width * 0.945,
    }
}

fn truncate_editor_text(text: &str, max_graphemes: usize) -> String {
    let count = text.graphemes(true).count();
    if count <= max_graphemes {
        return text.to_owned();
    }
    let mut out: String = text
        .graphemes(true)
        .take(max_graphemes.saturating_sub(1))
        .collect();
    out.push('\u{2026}');
    out
}

fn draw_editor_header(ui: &egui::Ui, rect: Rect, width: f32) {
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, Color32::from_rgb(52, 52, 52));
    let cols = editor_columns(rect.left(), width);
    let color = Color32::from_rgb(235, 235, 235);
    let font = FontId::proportional(12.0);
    let y = rect.center().y;
    for (x, text, centered) in [
        (cols.name, tr("Track Name"), false),
        (cols.channel, tr("Channel"), false),
        (cols.instrument, tr("Instrument"), false),
        (cols.bank, "BANK".to_owned(), true),
        (cols.vol, "VOL".to_owned(), true),
        (cols.pan, "PAN".to_owned(), true),
        (cols.rev, "REV".to_owned(), true),
        (cols.cho, "CHO".to_owned(), true),
    ] {
        painter.text(
            Pos2::new(x, y),
            if centered {
                egui::Align2::CENTER_CENTER
            } else {
                egui::Align2::LEFT_CENTER
            },
            text,
            font.clone(),
            color,
        );
    }
}

fn draw_editor_row(
    ui: &egui::Ui,
    rect: Rect,
    row: &MidiEditorRow,
    index: usize,
    width: f32,
    snapshot: Option<&MidiMonitorSnapshot>,
    edited: Option<MidiEditorValues>,
    hovered: bool,
) {
    let painter = ui.painter();
    let background = if hovered {
        Color32::from_rgb(56, 56, 56)
    } else if index % 2 == 0 {
        Color32::from_rgb(30, 30, 30)
    } else {
        Color32::from_rgb(24, 24, 24)
    };
    painter.rect_filled(rect, 0.0, background);
    painter.line_segment(
        [
            Pos2::new(rect.left(), rect.bottom() - 0.5),
            Pos2::new(rect.right(), rect.bottom() - 0.5),
        ],
        egui::Stroke::new(1.0, Color32::from_gray(46)),
    );

    let cols = editor_columns(rect.left(), width);
    let y = rect.center().y;
    let value_color = Color32::from_rgb(232, 232, 232);
    let muted_color = Color32::from_rgb(120, 120, 120);
    let font = FontId::proportional(12.5);

    painter.text(
        Pos2::new(cols.order, y),
        egui::Align2::CENTER_CENTER,
        (index + 1).to_string(),
        font.clone(),
        value_color,
    );
    painter.text(
        Pos2::new(cols.name, y),
        egui::Align2::LEFT_CENTER,
        truncate_editor_text(&row.name, 26),
        font.clone(),
        value_color,
    );

    let Some(channel) = row.channel else {
        painter.text(
            Pos2::new(cols.channel, y),
            egui::Align2::LEFT_CENTER,
            "-",
            font.clone(),
            muted_color,
        );
        return;
    };

    let values = if let Some(edited) = edited {
        edited
    } else {
        let mut values = snapshot
            .map(|snapshot| editor_values(snapshot, channel))
            .unwrap_or(row.initial);
        // Instrument and bank are the file's values, so the bass/drum
        // playback locks cannot leak into the editor.
        values.program = row.program;
        values.bank = row.bank;
        values
    };
    painter.text(
        Pos2::new(cols.channel, y),
        egui::Align2::LEFT_CENTER,
        (u32::from(channel) + 1).to_string(),
        font.clone(),
        value_color,
    );
    painter.text(
        Pos2::new(cols.instrument, y),
        egui::Align2::LEFT_CENTER,
        editor_program_label(values.program, Some(channel)),
        font.clone(),
        value_color,
    );
    for (x, value) in [
        (cols.bank, values.bank),
        (cols.vol, values.volume),
        (cols.pan, values.pan),
        (cols.rev, values.reverb),
        (cols.cho, values.chorus),
    ] {
        painter.text(
            Pos2::new(x, y),
            egui::Align2::CENTER_CENTER,
            value.to_string(),
            font.clone(),
            value_color,
        );
    }
}

fn open_midi_editor_popup(w: &mut MidiEditorWindow, index: usize) {
    let Some(row) = w.rows.get(index) else {
        return;
    };
    let mut values = match (row.channel, w.monitor.as_ref()) {
        (Some(channel), Some(monitor)) => editor_values(&monitor.snapshot(), channel),
        _ => row.initial,
    };
    if let Some(channel) = row.channel {
        values.program = row.program;
        values.bank = row.bank;
        if let Some((_, edited)) = w.edits.iter().find(|(edited, _)| *edited == channel) {
            values = *edited;
        }
    }
    w.popup = Some(MidiEditorPopup {
        row: index,
        name: row.name.clone(),
        values,
    });
}

/// Commit the values currently shown in an open track popup, so Save and
/// Save As never discard a slider the user changed but did not confirm first.
fn commit_open_editor_popup(w: &mut MidiEditorWindow) {
    let Some(popup) = w.popup.take() else {
        return;
    };
    if let Some(row) = w.rows.get(popup.row).cloned() {
        apply_midi_editor_popup(w, &row, popup);
    }
}

fn apply_midi_editor_popup(w: &mut MidiEditorWindow, row: &MidiEditorRow, popup: MidiEditorPopup) {
    let name = popup.name.trim().to_owned();
    let original = w.base_names.get(popup.row).cloned().unwrap_or_default();
    if name != original {
        let display = if name.is_empty() {
            tr_args("Track {}", &[&(popup.row + 1) as &dyn std::fmt::Display])
        } else {
            name.clone()
        };
        // A format-0 file has one SMF track for every channel, so a real
        // rename applies to all of its channel rows. An unchanged name must
        // leave the per-row fallbacks alone.
        for target in w.rows.iter_mut().filter(|target| target.track == row.track) {
            target.name = display.clone();
        }
        if let Some(entry) = w
            .name_edits
            .iter_mut()
            .find(|(track, _)| *track == row.track)
        {
            entry.1 = name;
        } else {
            w.name_edits.push((row.track, name));
        }
    } else {
        w.name_edits.retain(|(track, _)| *track != row.track);
    }

    let Some(channel) = row.channel else {
        return;
    };
    let mut values = popup.values;
    // Channel 10 always uses the percussion bank; keep its saved bank at 0 so
    // the file cannot select an invalid kit bank.
    if channel == 9 {
        values.bank = 0;
    }
    if let Some(entry) = w.edits.iter_mut().find(|(edited, _)| *edited == channel) {
        entry.1 = values;
    } else {
        w.edits.push((channel, values));
    }
}

fn editor_number_box_at(
    ui: &mut egui::Ui,
    rect: Rect,
    value: &mut i32,
    min: i32,
    max: i32,
    salt: &str,
) {
    let box_rect = Rect::from_min_size(rect.min, egui::vec2(rect.width() - 18.0, rect.height()));
    ui.put(
        box_rect,
        egui::DragValue::new(value).range(min..=max).speed(1.0),
    );

    let half = rect.height() * 0.5;
    let up_rect = Rect::from_min_size(
        Pos2::new(box_rect.right() + 2.0, rect.top()),
        egui::vec2(16.0, half),
    );
    let down_rect = Rect::from_min_size(
        Pos2::new(box_rect.right() + 2.0, rect.top() + half),
        egui::vec2(16.0, half),
    );
    for (step, target, id) in [(1, up_rect, "up"), (-1, down_rect, "down")] {
        let response = ui.interact(
            target,
            ui.scope_id().with(("editor_step", salt, id)),
            egui::Sense::click(),
        );
        let fill = if response.hovered() {
            Color32::from_gray(78)
        } else {
            Color32::from_gray(40)
        };
        ui.painter().rect_filled(target, 2.0, fill);
        let center = target.center();
        let triangle = if step > 0 {
            vec![
                Pos2::new(center.x, center.y - 3.5),
                Pos2::new(center.x - 4.0, center.y + 2.0),
                Pos2::new(center.x + 4.0, center.y + 2.0),
            ]
        } else {
            vec![
                Pos2::new(center.x, center.y + 3.5),
                Pos2::new(center.x - 4.0, center.y - 2.0),
                Pos2::new(center.x + 4.0, center.y - 2.0),
            ]
        };
        ui.painter().add(egui::Shape::convex_polygon(
            triangle,
            Color32::WHITE,
            egui::Stroke::NONE,
        ));
        if response.clicked() {
            *value = (*value + step).clamp(min, max);
        }
    }
}

fn editor_number_box(
    ui: &mut egui::Ui,
    value: &mut i32,
    min: i32,
    max: i32,
    width: f32,
    salt: &str,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width + 18.0, 24.0), egui::Sense::hover());
    editor_number_box_at(ui, rect, value, min, max, salt);
}

fn editor_number_u8_at(ui: &mut egui::Ui, rect: Rect, value: &mut u8, salt: &str) {
    let mut current = i32::from(*value);
    editor_number_box_at(ui, rect, &mut current, 0, 127, salt);
    *value = current.clamp(0, 127) as u8;
}

fn editor_number_i32(
    ui: &mut egui::Ui,
    value: &mut i32,
    min: i32,
    max: i32,
    width: f32,
    salt: &str,
) {
    editor_number_box(ui, value, min, max, width, salt);
}

fn editor_number_f64(
    ui: &mut egui::Ui,
    value: &mut f64,
    min: f64,
    max: f64,
    width: f32,
    salt: &str,
) {
    let mut current = value.round() as i32;
    editor_number_box(
        ui,
        &mut current,
        min.round() as i32,
        max.round() as i32,
        width,
        salt,
    );
    *value = (current as f64).clamp(min, max);
}

fn editor_slider_row_at(
    ui: &mut egui::Ui,
    left: f32,
    top: f32,
    label: &str,
    value: &mut u8,
    salt: &str,
) {
    let painter = ui.painter().clone();
    painter.text(
        Pos2::new(left + 16.0, top + 10.0),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(14.0),
        Color32::WHITE,
    );
    // Centre the numeric box and slider on the label row so the four
    // left-hand labels line up with their controls.
    let box_rect = Rect::from_min_size(Pos2::new(left + 84.0, top - 3.0), egui::vec2(62.0, 26.0));
    editor_number_u8_at(ui, box_rect, value, salt);
    let slider_rect = Rect::from_min_size(Pos2::new(left + 158.0, top), egui::vec2(242.0, 20.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(slider_rect), |ui| {
        ui.spacing_mut().slider_width = slider_rect.width();
        ui.add(egui::Slider::new(value, 0..=127).show_value(false));
    });
}

fn midi_editor_popup_content(ui: &mut egui::Ui, w: &mut MidiEditorWindow) {
    let Some(mut popup) = w.popup.take() else {
        return;
    };
    let Some(row) = w.rows.get(popup.row).cloned() else {
        w.popup = None;
        return;
    };
    let origin = ui.max_rect().min;
    let left = origin.x;
    let top = origin.y;
    let painter = ui.painter().clone();
    let font = FontId::proportional(14.0);

    painter.text(
        Pos2::new(left + 16.0, top + 15.0),
        egui::Align2::LEFT_TOP,
        tr("Track Name"),
        font.clone(),
        Color32::WHITE,
    );
    let field_rect =
        Rect::from_min_size(Pos2::new(left + 16.0, top + 36.0), egui::vec2(398.0, 28.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(field_rect), |ui| {
        ui.spacing_mut().interact_size.y = 28.0;
        ui.add_sized(
            field_rect.size(),
            egui::TextEdit::singleline(&mut popup.name)
                .desired_width(field_rect.width())
                .hint_text(tr("Track Name")),
        );
    });

    if row.channel.is_some() {
        painter.text(
            Pos2::new(left + 16.0, top + 83.0),
            egui::Align2::LEFT_TOP,
            tr("Patch Name"),
            font.clone(),
            Color32::WHITE,
        );
        let combo_rect =
            Rect::from_min_size(Pos2::new(left + 16.0, top + 106.0), egui::vec2(264.0, 30.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(combo_rect), |ui| {
            ui.spacing_mut().interact_size.y = 30.0;
            egui::ComboBox::from_id_salt(("midi_editor_patch", popup.row))
                .selected_text(
                    RichText::new(editor_program_label(popup.values.program, row.channel))
                        .size(14.0),
                )
                .width(combo_rect.width())
                .show_ui(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for value in 0..=127u8 {
                                let text = editor_program_label(value, row.channel);
                                ui.selectable_value(&mut popup.values.program, value, text);
                            }
                        });
                });
        });
        if row.channel != Some(9) {
            painter.text(
                Pos2::new(left + 290.0, top + 121.0),
                egui::Align2::LEFT_CENTER,
                "BANK",
                FontId::proportional(13.0),
                Color32::WHITE,
            );
            let bank_rect =
                Rect::from_min_size(Pos2::new(left + 330.0, top + 106.0), egui::vec2(80.0, 30.0));
            editor_number_u8_at(ui, bank_rect, &mut popup.values.bank, "bank");
        }
        editor_slider_row_at(
            ui,
            left,
            top + 154.0,
            &tr("Volume"),
            &mut popup.values.volume,
            "volume",
        );
        editor_slider_row_at(
            ui,
            left,
            top + 200.0,
            &tr("Pan"),
            &mut popup.values.pan,
            "pan",
        );
        editor_slider_row_at(
            ui,
            left,
            top + 246.0,
            &tr("Reverb"),
            &mut popup.values.reverb,
            "reverb",
        );
        editor_slider_row_at(
            ui,
            left,
            top + 292.0,
            &tr("Chorus"),
            &mut popup.values.chorus,
            "chorus",
        );
    } else {
        painter.text(
            Pos2::new(left + 16.0, top + 106.0),
            egui::Align2::LEFT_TOP,
            tr("This track has no MIDI channel"),
            font,
            Color32::from_gray(150),
        );
    }

    let ok_rect = Rect::from_min_size(
        Pos2::new(left + 155.0, top + 348.0),
        egui::vec2(120.0, 30.0),
    );
    let apply = ui
        .put(
            ok_rect,
            egui::Button::new(RichText::new(tr("OK")).size(15.0)).min_size(egui::vec2(120.0, 30.0)),
        )
        .clicked();

    if apply {
        apply_midi_editor_popup(w, &row, popup);
    } else {
        w.popup = Some(popup);
    }
}

fn midi_editor_confirm_content(ui: &mut egui::Ui, w: &mut MidiEditorWindow) {
    let save_as = w.save_as_confirm;
    let content_height = 64.0;
    let top = ((ui.available_height() - content_height) * 0.5).max(0.0);
    ui.add_space(top);
    ui.vertical_centered(|ui| {
        let message = if save_as {
            tr("An existing file will be overwritten. Continue?")
        } else {
            tr("Overwrite the original file?")
        };
        ui.label(RichText::new(message).size(14.0));
    });
    ui.add_space(22.0);
    ui.horizontal(|ui| {
        let gap = 12.0;
        let total = 100.0 + gap + 100.0;
        ui.add_space((ui.available_width() - total).max(0.0) * 0.5);
        if ui
            .add_sized([100.0, 30.0], egui::Button::new(tr("Cancel")))
            .clicked()
        {
            if save_as {
                w.save_as_confirm = false;
                w.save_as_pending = None;
            } else {
                w.save_confirm = false;
            }
        }
        ui.add_space(gap);
        if ui
            .add_sized([100.0, 30.0], egui::Button::new(tr("Save")))
            .clicked()
        {
            if save_as {
                w.save_as_confirm = false;
                w.save_as_requested = w.save_as_pending.take();
                w.save_as = None;
            } else {
                w.save_confirm = false;
                w.save_requested = true;
                w.status = None;
            }
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    });
}

fn sanitize_save_as_name(input: &str, extension: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let mut stem = trimmed;
    for ext in [".mid", ".midi", ".kar", ".mkc"] {
        if lower.ends_with(ext) {
            stem = &trimmed[..trimmed.len() - ext.len()];
            break;
        }
    }
    let mut clean = String::new();
    for ch in stem.chars() {
        if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
            clean.push('_');
        } else {
            clean.push(ch);
        }
    }
    let clean = clean
        .trim()
        .trim_end_matches(|ch| ch == '.' || ch == ' ')
        .trim();
    if clean.is_empty() {
        return None;
    }
    let extension = extension.trim_start_matches('.');
    Some(if extension.is_empty() {
        clean.to_owned()
    } else {
        format!("{clean}.{extension}")
    })
}

fn midi_editor_save_as_content(ui: &mut egui::Ui, w: &mut MidiEditorWindow) {
    let Some(mut form) = w.save_as.take() else {
        return;
    };
    let mut submit = false;
    let mut cancel = false;
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.add_space(2.0);
    ui.label(RichText::new(tr("File name")).size(13.0));
    if ui
        .add(
            egui::TextEdit::singleline(&mut form.file_name)
                .desired_width(f32::INFINITY)
                .hint_text(tr("File name")),
        )
        .changed()
    {
        // A new name no longer targets the file we warned about.
        form.confirm_overwrite = false;
    }
    ui.add_space(2.0);
    ui.label(RichText::new(tr("Song title")).size(13.0));
    ui.add(
        egui::TextEdit::singleline(&mut form.title)
            .desired_width(f32::INFINITY)
            .hint_text(tr("Song title")),
    );
    ui.add_space(2.0);
    ui.label(RichText::new(tr("Artist")).size(13.0));
    ui.add(
        egui::TextEdit::singleline(&mut form.artist)
            .desired_width(f32::INFINITY)
            .hint_text(tr("Artist")),
    );
    ui.add_space(2.0);
    ui.label(RichText::new(tr("Key")).size(13.0));
    ui.add(
        egui::TextEdit::singleline(&mut form.key)
            .desired_width(f32::INFINITY)
            .hint_text(tr("Key")),
    );
    ui.add_space(2.0);
    ui.label(RichText::new(tr("Folder")).size(13.0));
    ui.label(
        RichText::new(form.folder.display().to_string())
            .size(11.5)
            .color(Color32::from_gray(165)),
    );
    if form.outside_library {
        ui.label(
            RichText::new(tr("This file is outside the song folders."))
                .size(11.0)
                .color(Color32::from_gray(160)),
        );
    }
    if form.confirm_overwrite {
        let same_name = form.file_name.eq_ignore_ascii_case(&form.source_name);
        ui.label(
            RichText::new(tr(save_as_overwrite_message(
                form.outside_library,
                same_name,
            )))
            .size(12.5)
            .color(Color32::from_rgb(240, 190, 100)),
        );
    }
    if let Some(error) = &form.error {
        ui.label(
            RichText::new(error)
                .size(12.0)
                .color(Color32::from_rgb(235, 120, 120)),
        );
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let gap = 12.0;
        let total = 100.0 + gap + 100.0;
        ui.add_space((ui.available_width() - total).max(0.0) * 0.5);
        if ui
            .add_sized([100.0, 30.0], egui::Button::new(tr("Cancel")))
            .clicked()
        {
            cancel = true;
        }
        ui.add_space(gap);
        let action = if form.confirm_overwrite {
            tr("Overwrite")
        } else {
            tr("OK")
        };
        if ui
            .add_sized([100.0, 30.0], egui::Button::new(action))
            .clicked()
        {
            submit = true;
        }
    });

    if cancel {
        w.save_as = None;
        w.save_as_confirm = false;
        w.save_as_pending = None;
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        return;
    }
    if submit {
        if form.confirm_overwrite {
            // The warning was accepted in this same window, so nothing can be
            // hidden behind it while the user decides.
            w.save_as_requested = Some(form);
            w.save_as = None;
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            return;
        }
        let extension = w
            .path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("mid");
        match sanitize_save_as_name(&form.file_name, extension) {
            Some(name) => {
                form.file_name = name;
                let target = form.folder.join(&form.file_name);
                if target.exists() {
                    form.confirm_overwrite = true;
                    w.save_as = Some(form);
                } else {
                    w.save_as_requested = Some(form);
                    w.save_as = None;
                }
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
            None => {
                form.error = Some(tr("Enter a file name").to_owned());
                w.save_as = Some(form);
            }
        }
    } else {
        w.save_as = Some(form);
    }
}

/// Warning shown when Save As is about to replace a file.
///
/// The stronger wording is used when the source file itself lives outside the
/// configured song folders and its name was left unchanged, because that means
/// the original file is replaced in place.
fn save_as_overwrite_message(outside_library: bool, same_name: bool) -> &'static str {
    if outside_library && same_name {
        "The original file outside the song folders will be overwritten. Continue?"
    } else {
        "An existing file will be overwritten. Continue?"
    }
}

fn midi_editor_ui(ui: &mut egui::Ui, w: &mut MidiEditorWindow) {
    let ctx = ui.ctx().clone();
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        if w.save_confirm {
            w.save_confirm = false;
        } else if w.popup.is_some() {
            w.popup = None;
        } else {
            w.open = false;
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        return;
    }

    let width = ui.available_width().max(760.0);
    ui.set_width(width);
    ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);

    const HEADER_HEIGHT: f32 = 30.0;
    const ROW_HEIGHT: f32 = 24.0;
    const VISIBLE_ROWS: f32 = 8.0;

    let (header_rect, _) =
        ui.allocate_exact_size(egui::vec2(width, HEADER_HEIGHT), egui::Sense::hover());
    draw_editor_header(ui, header_rect, width);

    let snapshot = w.monitor.as_ref().map(|monitor| monitor.snapshot());
    let area_height = ROW_HEIGHT * VISIBLE_ROWS;
    let mut open_popup = None;
    egui::ScrollArea::vertical()
        .id_salt("midi_editor_rows")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .max_height(area_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (index, row) in w.rows.iter().enumerate() {
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), ROW_HEIGHT),
                    egui::Sense::click(),
                );
                let edited = row.channel.and_then(|channel| {
                    w.edits
                        .iter()
                        .find(|(edited, _)| *edited == channel)
                        .map(|(_, values)| *values)
                });
                draw_editor_row(
                    ui,
                    rect,
                    row,
                    index,
                    width,
                    snapshot.as_ref(),
                    edited,
                    response.hovered(),
                );
                if response.double_clicked() {
                    open_popup = Some(index);
                }
            }
            let used = w.rows.len() as f32 * ROW_HEIGHT;
            if used < area_height {
                ui.allocate_space(egui::vec2(0.0, area_height - used));
            }
        });
    if let Some(index) = open_popup {
        open_midi_editor_popup(w, index);
    }

    ui.add_space(6.0);
    let now = ctx.input(|i| i.time);
    if w.status.is_some() && now > w.status_until {
        w.status = None;
    }
    ui.horizontal(|ui| {
        if let Some(status) = &w.status {
            let color = if w.status_error {
                Color32::from_rgb(235, 120, 120)
            } else {
                Color32::from_rgb(150, 220, 150)
            };
            ui.label(RichText::new(status).size(12.0).color(color));
        }
    });
    ui.add_space(4.0);

    // Keep the action row on the bottom edge when the table is short.
    let gap = (ui.available_height() - 34.0).max(0.0);
    ui.add_space(gap);

    ui.horizontal(|ui| {
        if ui
            .add_sized(
                [96.0, 30.0],
                egui::Button::new(RichText::new(tr("Save")).size(14.0)),
            )
            .clicked()
        {
            commit_open_editor_popup(w);
            w.save_confirm = true;
        }
        let save_as = egui::Button::new(RichText::new(tr("Save As")).size(14.0))
            .min_size(egui::vec2(96.0, 30.0));
        if ui.add(save_as).clicked() {
            commit_open_editor_popup(w);
            w.save_as_open_requested = true;
        }

        // Flush the tempo/key group to the same right edge as the table and
        // lift it slightly so it sits optically centred with the buttons.
        let start = ui.cursor().min;
        let group_area = Rect::from_min_size(
            Pos2::new(start.x, start.y - 5.0),
            egui::vec2(ui.available_width(), 30.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(group_area), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                editor_number_i32(ui, &mut w.key, w.key_min, w.key_max, 56.0, "key");
                if let Some(key) = w.key_name.clone() {
                    let live = (w.key_base + w.key).clamp(-12, 12);
                    let sounding = sounding_key(&key, live);
                    ui.label(
                        RichText::new(format!("{}: {sounding}", tr("Key")))
                            .size(14.0)
                            .strong(),
                    );
                } else {
                    ui.add_sized(
                        [34.0, 20.0],
                        egui::Label::new(RichText::new("KEY").size(14.0).strong()),
                    );
                }
                ui.add_space(22.0);
                editor_number_f64(ui, &mut w.bpm, 20.0, 300.0, 56.0, "bpm");
                ui.add_sized(
                    [34.0, 20.0],
                    egui::Label::new(RichText::new("BPM").size(14.0).strong()),
                );
            });
        });
    });
}

/// Paint one of the inert controller placeholders and allow a hover tooltip.
fn paint_controller_placeholder_button(ui: &egui::Ui, rect: Rect, label: &str) -> egui::Response {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(17));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.5),
        Color32::from_gray(136),
    );
    ui.interact(
        rect,
        ui.scope_id().with((
            "controller_placeholder",
            rect.min.x as i32,
            rect.min.y as i32,
        )),
        egui::Sense::hover(),
    )
}

/// Small up/down triangle button used by the channel selector.
fn controller_spin_button_at(ui: &mut egui::Ui, rect: Rect, up: bool, enabled: bool) -> bool {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("controller_spin", up)),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        3.0,
        if enabled && response.hovered() {
            Color32::from_gray(48)
        } else {
            Color32::from_gray(26)
        },
    );
    let center = rect.center();
    let points = if up {
        vec![
            Pos2::new(center.x - 4.0, center.y + 2.0),
            Pos2::new(center.x + 4.0, center.y + 2.0),
            Pos2::new(center.x, center.y - 3.0),
        ]
    } else {
        vec![
            Pos2::new(center.x - 4.0, center.y - 2.0),
            Pos2::new(center.x + 4.0, center.y - 2.0),
            Pos2::new(center.x, center.y + 3.0),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        points,
        if enabled {
            Color32::WHITE
        } else {
            Color32::from_gray(110)
        },
        egui::Stroke::NONE,
    ));
    response.clicked()
}

/// Draw a fixed-height volume fader.
fn paint_controller_fader(
    painter: &egui::Painter,
    rect: Rect,
    value: u8,
    enabled: bool,
    locked: bool,
) {
    // Sit the rail a couple of pixels lower without changing the strip layout.
    let track = Rect::from_min_size(
        Pos2::new(rect.center().x - 4.0, rect.top() + 4.0),
        egui::vec2(8.0, rect.height() - 8.0),
    );
    painter.rect_filled(track, 4.0, Color32::from_gray(26));
    if !enabled {
        return;
    }
    let t = value.min(127) as f32 / 127.0;
    let y = (track.bottom() - track.height() * t).clamp(track.top() + 4.5, track.bottom() - 4.5);
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(track.left(), y), track.right_bottom()),
        4.0,
        Color32::from_gray(90),
    );
    painter.rect_filled(
        Rect::from_center_size(Pos2::new(track.center().x, y), egui::vec2(20.0, 9.0)),
        4.0,
        if locked {
            Color32::from_rgb(90, 225, 140)
        } else {
            Color32::WHITE
        },
    );
}

/// Draw one channel's segmented level meter.
fn paint_controller_meter(painter: &egui::Painter, rect: Rect, level: f32) {
    // Match the fader's small downward shift; the bottom stays put.
    let rect = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + 4.0), rect.max);
    const SEGMENTS: usize = 11;
    let gap = 1.0;
    let height = (rect.height() - gap * (SEGMENTS as f32 - 1.0)) / SEGMENTS as f32;
    for index in 0..SEGMENTS {
        let t = index as f32 / (SEGMENTS as f32 - 1.0);
        let active = level > 0.001 && level >= t - 0.001;
        let y = rect.bottom() - height * (index as f32 + 1.0) - gap * index as f32;
        let color = if active {
            // The top four of the eleven segments form the red peak zone.
            if t >= 0.7 {
                Color32::from_rgb(235, 70, 70)
            } else {
                Color32::from_rgb(30, 150, 235)
            }
        } else {
            Color32::from_gray(28)
        };
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(rect.left(), y), egui::vec2(rect.width(), height)),
            0.0,
            color,
        );
    }
}

/// Paint the 61-key keyboard (C2..C7) with the notes of one channel lit.
fn paint_controller_keyboard(
    painter: &egui::Painter,
    rect: Rect,
    notes: &[u16; 128],
    channel: usize,
) {
    let white_width = rect.width() / CONTROLLER_WHITE_NOTES.len() as f32;
    for (index, note) in CONTROLLER_WHITE_NOTES.iter().enumerate() {
        let x = rect.left() + index as f32 * white_width;
        let key = Rect::from_min_max(
            Pos2::new(x, rect.top()),
            Pos2::new(x + white_width, rect.bottom()),
        );
        let active = notes[*note as usize] & (1 << channel) != 0;
        painter.rect_filled(
            key,
            0.0,
            if active {
                Color32::from_rgb(30, 150, 235)
            } else {
                Color32::WHITE
            },
        );
        painter.rect_stroke(
            key,
            0.0,
            egui::Stroke::new(0.8, Color32::from_gray(190)),
            egui::StrokeKind::Inside,
        );
        if *note == 60 {
            painter.rect_filled(
                Rect::from_center_size(
                    Pos2::new(key.center().x, key.bottom() - 5.0),
                    egui::vec2(4.0, 4.0),
                ),
                0.0,
                Color32::from_gray(20),
            );
        }
    }
    for note in 36..=96u8 {
        if !matches!(note % 12, 1 | 3 | 6 | 8 | 10) {
            continue;
        }
        let index = CONTROLLER_WHITE_NOTES.iter().filter(|n| **n < note).count();
        let center = rect.left() + index as f32 * white_width;
        let width = white_width * 0.62;
        let height = rect.height() * 0.62;
        let key = Rect::from_min_size(
            Pos2::new(center - width * 0.5, rect.top()),
            egui::vec2(width, height),
        );
        let active = notes[note as usize] & (1 << channel) != 0;
        painter.rect_filled(
            key,
            0.0,
            if active {
                Color32::from_rgb(90, 185, 255)
            } else {
                Color32::from_gray(8)
            },
        );
    }
}

/// MIDI note under `pos` on the 61-key keyboard.
fn controller_key_at(rect: Rect, pos: Pos2) -> Option<u8> {
    if !rect.contains(pos) {
        return None;
    }
    let white_width = rect.width() / CONTROLLER_WHITE_NOTES.len() as f32;
    for note in 36..=96u8 {
        if !matches!(note % 12, 1 | 3 | 6 | 8 | 10) {
            continue;
        }
        let index = CONTROLLER_WHITE_NOTES.iter().filter(|n| **n < note).count();
        let center = rect.left() + index as f32 * white_width;
        let width = white_width * 0.62;
        let height = rect.height() * 0.62;
        let key = Rect::from_min_size(
            Pos2::new(center - width * 0.5, rect.top()),
            egui::vec2(width, height),
        );
        if key.contains(pos) {
            return Some(note);
        }
    }
    let index = ((pos.x - rect.left()) / white_width).floor() as usize;
    CONTROLLER_WHITE_NOTES.get(index).copied()
}

/// Draw a vertical performance wheel with its handle at `t` (0..1).
fn paint_controller_wheel(painter: &egui::Painter, rect: Rect, t: f32) {
    painter.rect_filled(rect, 2.0, Color32::from_gray(26));
    let y = rect.bottom() - rect.height() * t.clamp(0.0, 1.0);
    painter.line_segment(
        [
            Pos2::new(rect.left() + 2.0, y),
            Pos2::new(rect.right() - 2.0, y),
        ],
        egui::Stroke::new(2.0, Color32::WHITE),
    );
}

/// Display names of the twenty General MIDI instrument groups.
const GROUP_NAMES: [&str; audio::GROUPS] = [
    "Piano",
    "Chromatic Percussion",
    "Organ",
    "Acoustic Guitar",
    "Clean Guitar",
    "Jazz Guitar",
    "Overdrive Guitar",
    "Distortion Guitar",
    "Bass",
    "Strings",
    "Ensemble",
    "Brass",
    "Reed",
    "Pipe",
    "Synth Lead",
    "Synth Pad",
    "Synth Effects",
    "Ethnic",
    "Percussive",
    "Sound Effects",
];

/// General MIDI programs that belong to each instrument group (tooltip text).
const GROUP_INSTRUMENTS: [&str; audio::GROUPS] = [
    "1 Acoustic Grand Piano\n2 Bright Acoustic Piano\n3 Electric Grand Piano\n4 Honky-tonk Piano\n5 Electric Piano 1\n6 Electric Piano 2\n7 Harpsichord\n8 Clavinet",
    "9 Celesta\n10 Glockenspiel\n11 Music Box\n12 Vibraphone\n13 Marimba\n14 Xylophone\n15 Tubular Bells\n16 Dulcimer",
    "17 Drawbar Organ\n18 Percussive Organ\n19 Rock Organ\n20 Church Organ\n21 Reed Organ\n22 Accordion\n23 Harmonica\n24 Tango Accordion",
    "25 Acoustic Guitar (nylon)\n26 Acoustic Guitar (steel)",
    "28 Electric Guitar (clean)\n29 Electric Guitar (muted)",
    "27 Electric Guitar (jazz)",
    "30 Overdriven Guitar",
    "31 Distortion Guitar\n32 Guitar Harmonics",
    "33 Acoustic Bass\n34 Electric Bass (finger)\n35 Electric Bass (pick)\n36 Fretless Bass\n37 Slap Bass 1\n38 Slap Bass 2\n39 Synth Bass 1\n40 Synth Bass 2",
    "41 Violin\n42 Viola\n43 Cello\n44 Contrabass\n45 Tremolo Strings\n46 Pizzicato Strings\n47 Orchestral Harp\n48 Timpani",
    "49 String Ensemble 1\n50 String Ensemble 2\n51 SynthStrings 1\n52 SynthStrings 2\n53 Choir Aahs\n54 Voice Oohs\n55 Synth Voice\n56 Orchestra Hit",
    "57 Trumpet\n58 Trombone\n59 Tuba\n60 Muted Trumpet\n61 French Horn\n62 Brass Section\n63 SynthBrass 1\n64 SynthBrass 2",
    "65 Soprano Sax\n66 Alto Sax\n67 Tenor Sax\n68 Baritone Sax\n69 Oboe\n70 English Horn\n71 Bassoon\n72 Clarinet",
    "73 Piccolo\n74 Flute\n75 Recorder\n76 Pan Flute\n77 Blown Bottle\n78 Shakuhachi\n79 Whistle\n80 Ocarina",
    "81 Lead 1 (square)\n82 Lead 2 (sawtooth)\n83 Lead 3 (calliope)\n84 Lead 4 (chiff)\n85 Lead 5 (charang)\n86 Lead 6 (voice)\n87 Lead 7 (fifths)\n88 Lead 8 (bass + lead)",
    "89 Pad 1 (new age)\n90 Pad 2 (warm)\n91 Pad 3 (polysynth)\n92 Pad 4 (choir)\n93 Pad 5 (bowed)\n94 Pad 6 (metallic)\n95 Pad 7 (halo)\n96 Pad 8 (sweep)",
    "97 FX 1 (rain)\n98 FX 2 (soundtrack)\n99 FX 3 (crystal)\n100 FX 4 (atmosphere)\n101 FX 5 (brightness)\n102 FX 6 (goblins)\n103 FX 7 (echoes)\n104 FX 8 (sci-fi)",
    "105 Sitar\n106 Banjo\n107 Shamisen\n108 Koto\n109 Kalimba\n110 Bag pipe\n111 Fiddle\n112 Shanai",
    "113 Tinkle Bell\n114 Agogo\n115 Steel Drums\n116 Woodblock\n117 Taiko Drum\n118 Melodic Tom\n119 Synth Drum\n120 Reverse Cymbal",
    "121 Guitar Fret Noise\n122 Breath Noise\n123 Seashore\n124 Bird Tweet\n125 Telephone Ring\n126 Helicopter\n127 Applause\n128 Gunshot",
];

/// Preview artwork for each group, embedded in the executable.
const GROUP_IMAGES: [&[u8]; audio::GROUPS] = [
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/1.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/2.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/3.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/4.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/5.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/6.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/7.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/8.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/9.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/10.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/11.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/12.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/13.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/14.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/15.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/16.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/17.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/18.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/19.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/instruments/20.png"
    )),
];

/// Display names of the nine General MIDI drum groups.
const DRUM_NAMES: [&str; audio::DRUM_GROUPS] = [
    "Kick",
    "Snare",
    "Tom",
    "Hi-hat",
    "Cowbell",
    "Cymbal",
    "Hand Percussion",
    "Misc Percussion",
    "Metronome",
];

/// Notes that belong to each drum group (shown as a tooltip on the row).
const DRUM_NOTES: [&str; audio::DRUM_GROUPS] = [
    "35 Acoustic Bass Drum\n36 Electric Bass Drum",
    "31 Drum Sticks\n34 Metronome Bell\n37 Side Stick\n38 Acoustic Snare\n40 Electric Snare",
    "41 Low Floor Tom\n43 High Floor Tom\n45 Low Tom\n47 Low-Mid Tom\n48 High-Mid Tom\n50 High Tom",
    "42 Closed Hi-hat\n44 Pedal Hi-hat\n46 Open Hi-hat",
    "56 Cowbell",
    "49 Crash Cymbal 1\n51 Ride Cymbal 1\n52 Chinese Cymbal\n53 Ride Bell\n55 Splash Cymbal\n57 Crash Cymbal 2\n59 Ride Cymbal 2",
    "60 High Bongo\n61 Low Bongo\n62 Mute High Conga\n63 Open High Conga\n64 Low Conga\n65 High Timbale\n66 Low Timbale\n86 Mute Surdo\n87 Open Surdo",
    "27 High Q\n28 Slap Noise\n29 Scratch Push\n30 Scratch Pull\n32 Square Click\n33 Metronome Click\n39 Hand Clap\n54 Tambourine\n58 Vibraslap\n67 High Agogo\n68 Low Agogo\n69 Cabasa\n70 Maracas\n71 Short Whistle\n72 Long Whistle\n73 Short Guiro\n74 Long Guiro\n77 Low Woodblock\n78 Mute Cuica\n79 Open Cuica\n80 Mute Triangle\n81 Open Triangle\n82 Shaker\n83 Jingle Bell\n84 Belltree\n85 Castanets",
    "75 Claves\n76 High Woodblock",
];

/// Preview artwork for each drum group, embedded in the executable.
const DRUM_IMAGES: [&[u8]; audio::DRUM_GROUPS] = [
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/1.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/2.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/3.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/4.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/5.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/6.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/7.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/8.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/drums/9.png"
    )),
];

/// Preview artwork for the channel page: 1 = the drum channel, 2 = the rest.
const CHANNEL_IMAGES: [&[u8]; 2] = [
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/midich/1.png"
    )),
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/midich/2.png"
    )),
];

/// State of the Split MIDI window, shared with its own OS viewport.
#[derive(Default)]
struct SplitWindow {
    open: bool,
    /// Last visibility sent to the OS window.
    shown: bool,
    ports: Vec<String>,
    default_label: String,
    channels: Vec<String>,
    /// Channel whose artwork is shown (channel 10 uses a different image).
    channel_selected: Option<usize>,
    channel_texture: Option<(usize, egui::TextureHandle)>,
    selection: Vec<usize>,
    drag_anchor: Option<usize>,
    error: Option<String>,
    changed: bool,
    /// 0 = channel table, 1 = instrument groups, 2 = drum groups.
    page: usize,
    /// Group shown in the preview and edited by the sliders.
    group_selected: Option<usize>,
    /// Cached preview artwork (`usize` = the group it belongs to).
    group_texture: Option<(usize, egui::TextureHandle)>,
    group_ports: Vec<String>,
    group_volume: Vec<u32>,
    group_volume_on: Vec<bool>,
    /// Rows selected in the group table (drag or Ctrl/Shift click).
    group_selection: Vec<usize>,
    group_drag_anchor: Option<usize>,
    group_velocity_min: Vec<u32>,
    group_velocity_max: Vec<u32>,
    group_velocity_on: Vec<bool>,
    /// Drum group shown in the preview and edited by the sliders.
    drum_selected: Option<usize>,
    drum_texture: Option<(usize, egui::TextureHandle)>,
    drum_ports: Vec<String>,
    drum_volume: Vec<u32>,
    drum_volume_on: Vec<bool>,
    drum_velocity_min: Vec<u32>,
    drum_velocity_max: Vec<u32>,
    drum_velocity_on: Vec<bool>,
    drum_selection: Vec<usize>,
    drum_drag_anchor: Option<usize>,
}

/// One channel's editable mix values.
#[derive(Clone, Copy, Debug)]
struct MidiEditorValues {
    program: u8,
    bank: u8,
    volume: u8,
    pan: u8,
    reverb: u8,
    chorus: u8,
}

impl Default for MidiEditorValues {
    fn default() -> Self {
        Self {
            program: 0,
            bank: 0,
            volume: 100,
            pan: 64,
            reverb: 40,
            chorus: 0,
        }
    }
}

/// One table row: a MIDI track and the channel its notes use.
#[derive(Clone)]
struct MidiEditorRow {
    track: usize,
    name: String,
    channel: Option<u8>,
    /// Program and bank taken from the file's own events, not the playback
    /// locks, so bass and drum edits refer to the original MIDI.
    program: u8,
    bank: u8,
    /// Mix values read when the editor opened, used when no monitor is
    /// available.
    initial: MidiEditorValues,
}

/// Fields of the Save As dialog.
#[derive(Clone)]
struct MidiSaveAsForm {
    file_name: String,
    title: String,
    artist: String,
    key: String,
    folder: PathBuf,
    error: Option<String>,
    /// The source file lives outside the configured song folders.
    outside_library: bool,
    /// Original name of the source file, to detect an unchanged name.
    source_name: String,
    /// Waiting for the user to accept overwriting an existing file.
    confirm_overwrite: bool,
}

/// The per-track edit dialog above the table.
struct MidiEditorPopup {
    row: usize,
    name: String,
    values: MidiEditorValues,
}

/// State of the MIDI Editor window, shared with its own OS viewport.
struct MidiEditorWindow {
    open: bool,
    /// Last visibility sent to the OS window.
    shown: bool,
    path: PathBuf,
    rows: Vec<MidiEditorRow>,
    /// Track names as they were when the editor opened (or last saved).
    base_names: Vec<String>,
    /// `(track, new name)` entries waiting for a save.
    name_edits: Vec<(usize, String)>,
    /// `(channel, values)` entries waiting for a save.
    edits: Vec<(u8, MidiEditorValues)>,
    monitor: Option<Arc<MidiMonitor>>,
    popup: Option<MidiEditorPopup>,
    save_confirm: bool,
    save_requested: bool,
    /// Save As form, its OS window state and the overwrite confirmation.
    save_as: Option<MidiSaveAsForm>,
    save_as_open_requested: bool,
    save_as_shown: bool,
    save_as_fonts_ready: bool,
    save_as_confirm: bool,
    save_as_pending: Option<MidiSaveAsForm>,
    save_as_requested: Option<MidiSaveAsForm>,
    status: Option<String>,
    status_error: bool,
    status_until: f64,
    /// Target tempo shown in the BPM box.
    bpm: f64,
    /// Tempo of the file as it is on disk (changes after a save).
    bpm_base: f64,
    /// Song key shown beside the KEY box (NCN/KMID only).
    key_name: Option<String>,
    /// Additional key offset shown in the KEY box.
    key: i32,
    /// Key offset already baked into the file by earlier saves.
    key_base: i32,
    key_min: i32,
    key_max: i32,
    /// Last visibility sent to the popup OS window.
    popup_shown: bool,
    /// Last visibility sent to the save-confirmation OS window.
    confirm_shown: bool,
    /// Fonts/styles are installed once on the editor's own context.
    fonts_ready: bool,
    /// Fonts/styles for the popup's own context.
    popup_fonts_ready: bool,
    /// Fonts/styles for the confirmation's own context.
    confirm_fonts_ready: bool,
}

impl Default for MidiEditorWindow {
    fn default() -> Self {
        Self {
            open: false,
            shown: false,
            path: PathBuf::new(),
            rows: Vec::new(),
            base_names: Vec::new(),
            name_edits: Vec::new(),
            edits: Vec::new(),
            monitor: None,
            popup: None,
            save_confirm: false,
            save_requested: false,
            save_as: None,
            save_as_open_requested: false,
            save_as_shown: false,
            save_as_fonts_ready: false,
            save_as_confirm: false,
            save_as_pending: None,
            save_as_requested: None,
            status: None,
            status_error: false,
            status_until: 0.0,
            bpm: 120.0,
            bpm_base: 120.0,
            key_name: None,
            key: 0,
            key_base: 0,
            key_min: -12,
            key_max: 12,
            popup_shown: false,
            confirm_shown: false,
            fonts_ready: false,
            popup_fonts_ready: false,
            confirm_fonts_ready: false,
        }
    }
}

fn midi_editor_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("midi_editor_window")
}

fn midi_editor_popup_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("midi_editor_popup_window")
}

fn midi_editor_confirm_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("midi_editor_confirm_window")
}

fn midi_editor_save_as_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("midi_editor_save_as_window")
}

#[derive(Clone)]
enum MenuAction {
    Open,
    AddFiles,
    AddFolder,
    ImportPlaylist,
    ExportPlaylist,
    TogglePlaylist,
    /// Open the song database window.
    SongData,
    /// Switch the MIDI output (empty = the built-in SoundFont).
    SetMidiOut(String),
    /// Switch the lyric background between image and colour.
    SetBackground(BackgroundStyle),
    /// Open the per-channel output window.
    SplitVoice,
    /// Show or hide the in-window MIDI Controller.
    MidiController,
    /// Open the MIDI Editor for the playing song.
    MidiEditor,
    /// Hide or show the transport bar.
    ToggleTransport,
    ToggleFullscreen,
    Minimize,
    ToggleCursor,
    ToggleRepeat,
    ClearPlaylist,
    PlaylistPrev,
    PlaylistNext,
    Settings,
    About,
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
    BackgroundImage,
    AddFiles,
    AddFolder,
    ImportPlaylist,
    ExportPlaylist,
    SoundFont,
    LyricFont,
    NcnFolder,
    KmidFolder,
    KarFolder,
    MkcFolder,
    ImportFolder,
}

/// A native dialog that runs on its own thread so the player keeps drawing
/// and the song keeps playing while the dialog is open.
struct PendingDialog {
    kind: DialogKind,
    receiver: Receiver<Option<Vec<PathBuf>>>,
}

enum LibraryScanMessage {
    Progress(usize, usize),
    Done(ScanOutcome),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LibraryFilter {
    All,
    Ncn,
    Kmid,
    Kar,
    Mkc,
}

impl LibraryFilter {
    fn label(self) -> String {
        match self {
            Self::All => tr("All"),
            Self::Ncn => tr("NCN"),
            Self::Kmid => tr("KMID"),
            Self::Kar => tr("KAR"),
            Self::Mkc => tr("MKC"),
        }
    }

    fn format(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Ncn => Some("ncn"),
            Self::Kmid => Some("kmid"),
            Self::Kar => Some("kar"),
            Self::Mkc => Some("mkc"),
        }
    }
}

/// A database maintenance action started from the Song Data window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LibraryOp {
    CheckMissing,
    Compact,
}

impl LibraryOp {
    fn busy_label(self) -> String {
        match self {
            Self::CheckMissing => tr("Checking files…"),
            Self::Compact => tr("Compacting…"),
        }
    }
}

/// Raw numbers reported by a maintenance action.
enum LibraryOpResult {
    Missing { missing: usize, restored: usize },
    Compact { before: u64, after: u64 },
}

struct LibraryScan {
    receiver: Receiver<LibraryScanMessage>,
    filter: LibraryFilter,
    progress: f32,
    done: usize,
    total: usize,
}

#[derive(Clone)]
struct ImportItem {
    record: SongRecord,
    selected: bool,
}

struct ImportScan {
    receiver: Receiver<LibraryScanMessage>,
    progress: f32,
}

enum ImportAddMessage {
    Progress(usize, usize),
    Done(ImportAddOutcome),
}

struct ImportAddOutcome {
    /// Source path, the copied record, and the files written for it.
    added: Vec<(PathBuf, SongRecord, Vec<PathBuf>)>,
    skipped: usize,
}

struct ImportAdd {
    receiver: Receiver<ImportAddMessage>,
    progress: f32,
}

/// Arrow-key browsing of every song in the database.
struct LyricBrowse {
    index: usize,
    total: usize,
    hit: LyricHit,
}

struct Playback {
    _handle: AudioHandle,
    commands: Sender<Command>,
    position: Arc<AtomicU64>,
    monitor: Arc<MidiMonitor>,
}

/// A silent audio stream for the on-screen keyboard when no song is open.
struct Preview {
    _handle: AudioHandle,
    commands: Sender<Command>,
    monitor: Arc<MidiMonitor>,
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
    code: String,
    title: String,
    artist: String,
    file_type: String,
    bpm: f64,
    key: String,
    bars: u32,
    loaded: bool,
}

/// File-type badge shown in the black bar of the song search popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SongFileIcon {
    Ncn,
    Kmid,
    Midi,
    Mkc,
}

/// Which artwork belongs to a song format.
///
/// KAR and plain MIDI share the generic MIDI badge.
fn song_file_icon(format: &str) -> SongFileIcon {
    match format.to_ascii_lowercase().as_str() {
        "ncn" => SongFileIcon::Ncn,
        "kmid" => SongFileIcon::Kmid,
        "mkc" => SongFileIcon::Mkc,
        _ => SongFileIcon::Midi,
    }
}

/// The four file-type badges, decoded once per egui context.
struct SongFileIcons {
    ncn: egui::TextureHandle,
    kmid: egui::TextureHandle,
    midi: egui::TextureHandle,
    mkc: egui::TextureHandle,
}

impl SongFileIcons {
    fn load(ctx: &egui::Context) -> Option<Self> {
        fn make(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
            let image = load_background_bytes(bytes).ok()?;
            Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
        }

        Some(Self {
            ncn: make(
                ctx,
                "songfile-ncn",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../assets/songfile/ncn.png"
                )),
            )?,
            kmid: make(
                ctx,
                "songfile-kmid",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../assets/songfile/kmid.png"
                )),
            )?,
            midi: make(
                ctx,
                "songfile-midi",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../assets/songfile/midi.png"
                )),
            )?,
            mkc: make(
                ctx,
                "songfile-mkc",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../assets/songfile/mkc.png"
                )),
            )?,
        })
    }

    fn get(&self, icon: SongFileIcon) -> &egui::TextureHandle {
        match icon {
            SongFileIcon::Ncn => &self.ncn,
            SongFileIcon::Kmid => &self.kmid,
            SongFileIcon::Midi => &self.midi,
            SongFileIcon::Mkc => &self.mkc,
        }
    }
}

pub struct KaraokeApp {
    song: Option<Song>,
    path: Option<PathBuf>,
    playback: Option<Playback>,
    /// Audio kept alive for the virtual piano when no song is loaded.
    preview: Option<Preview>,
    preview_attempted: bool,
    startup_volume_shown: bool,
    sound_font: Option<Arc<SoundFont>>,
    sound_font_path: Option<PathBuf>,
    /// True while the opening splash is shown.
    startup_active: bool,
    startup_status: String,
    startup_detail: String,
    startup_started: f64,
    startup_ready_at: Option<f64>,
    startup_receiver: Option<Receiver<Result<Arc<SoundFont>, String>>>,
    startup_path: Option<PathBuf>,
    startup_texture: Option<egui::TextureHandle>,
    /// The splash window has been centred once on its monitor.
    splash_centered: bool,
    /// Frames left before the expanded main window is centred.
    center_main_pending: u8,
    /// Frames left before the saved fullscreen state is applied.
    fullscreen_pending: u8,
    pending_open: Option<PathBuf>,
    pending_playlist: Vec<PathBuf>,
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
    /// Metadata from the song database, keyed by file path.
    library_meta: std::collections::HashMap<PathBuf, SongInfo>,
    indicator: Option<Indicator>,
    /// Set after a song opens; the card timer starts on the next UI frame.
    song_card_pending: bool,
    /// While in the future, the song-info card is shown in the lyrics area.
    song_card_until: Option<f64>,
    playlist_rect: Rect,
    central_rect: Rect,
    lyric_search_rect: Rect,
    /// Full rect of the song search popup, so the readout can stay clear of it.
    lyric_overlay_rect: Rect,
    /// Decoded background image, keyed by the path it was loaded from.
    background_texture: Option<(PathBuf, egui::TextureHandle)>,
    /// Decoded copy of the artwork shipped with the player.
    default_background: Option<egui::TextureHandle>,
    /// Why the last SoundFont could not be loaded, shown on the SoundFont page.
    soundfont_error: Option<String>,
    /// Why the last background image could not be loaded.
    background_error: Option<String>,
    /// The full area between the top and bottom bars, ignoring the playlist.
    /// Lyric anchors use this so the playlist never moves the lyrics.
    lyric_area: Rect,
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
    /// Text typed into the always-visible lyric search bar.
    lyric_search: String,
    /// Focus the search field when the overlay opens.
    lyric_search_focus: bool,
    lyric_search_id: Option<egui::Id>,
    /// Rect of the MIDI Out flyout in the context menu (NOTHING when closed).
    midi_menu_rect: Rect,
    /// Rect of the Tools flyout in the context menu (NOTHING when closed).
    tools_menu_rect: Rect,
    /// Rect of the Background flyout in the context menu (NOTHING when closed).
    background_menu_rect: Rect,
    /// Artwork for the file-type badge in the song search popup.
    song_file_icons: Option<SongFileIcons>,
    /// The search overlay is a temporary 5-second popup.
    lyric_overlay_visible: bool,
    lyric_overlay_until: f64,
    /// Set when the popup closed on its own: the next Left/Right shows the
    /// remembered song instead of moving.
    lyric_overlay_remembered: bool,
    lyric_browse: Option<LyricBrowse>,
    /// Restore the playlist when the search overlay closes.
    search_restore_playlist: bool,
    /// Songs matching the lyric query, best match first.
    lyric_results: Vec<LyricHit>,
    lyric_result_index: usize,
    lyric_query_cache: String,
    lyric_last_query: String,
    lyric_db: Option<Library>,
    temp_playlist_until: Option<f64>,
    temp_playlist_hidden: bool,
    /// The modal settings window.
    settings_open: bool,
    /// True for the frame in which the dialog was opened, so the click that
    /// opened it cannot immediately close it through the backdrop.
    settings_just_opened: bool,
    /// Playlist entry the last Stop interrupted, so Play can resume it.
    stopped_index: Option<usize>,
    /// MIDI output ports found the last time the list was refreshed.
    midi_ports: Vec<String>,
    /// True once the port list has been read at least once.
    midi_ports_loaded: bool,
    /// Last error from choosing a MIDI output, shown under the combo.
    midi_page_error: Option<String>,
    /// The Split MIDI window (per-channel MIDI routing).
    split_open: bool,
    /// Routing table shared with the window's own viewport.
    split_window: Arc<Mutex<SplitWindow>>,
    /// True while the MIDI Editor window is up.
    midi_editor_open: bool,
    /// Table and pending edits shared with the editor's own viewport.
    midi_editor_window: Arc<Mutex<MidiEditorWindow>>,
    /// Song Data table shared with its own viewport.
    library_window: Arc<Mutex<LibraryWindow>>,
    /// Settings pages shared with the settings window's viewport.
    settings_pages: Arc<Mutex<SettingsPages>>,
    /// "Add a song" window shared with its own viewport.
    import_window: Arc<Mutex<ImportWindow>>,
    /// Bumped when the import list changes.
    import_gen: u64,
    /// Bumped when the song list is reloaded.
    library_songs_gen: u64,
    /// Bumped when the filtered view is rebuilt.
    library_view_gen: u64,
    /// Which settings page is selected (0 = General, 1 = Text, 2 = MIDI,
    /// 3 = SoundFont, 4 = Player, 5 = Background, 6 = Song Data, 7 = About).
    settings_section: usize,
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
    exit_confirm_open: Arc<AtomicBool>,
    /// Set once the user confirms; lets the pending close through.
    exit_confirmed: Arc<AtomicBool>,
    /// Last minimized state seen for the main window.
    main_minimized: bool,
    /// True while the in-window MIDI Controller panel is shown.
    controller_open: bool,
    /// True while the per-channel detail popup is shown.
    controller_detail_open: bool,
    /// True while the program list covers the detail popup.
    controller_detail_program_open: bool,
    /// Channel whose keys and names are shown in the controller.
    controller_channel: usize,
    /// Temporary key offsets for the controller's right-click menu.
    controller_channel_transpose: [i32; 16],
    /// Channels whose mixer values are locked against song automation.
    controller_locks: u16,
    /// Persistent per-channel mute mask (channel 9 follows `melo_muted`).
    controller_mutes: u16,
    /// Temporary solo channel; reset on a new song and at the song's end.
    controller_solo: Option<u8>,
    /// Channels that the current song uses.
    controller_used: u16,
    /// Initial program for each channel in the current song.
    controller_programs: [u8; 16],
    /// Last monitor snapshot, kept while playback is stopped.
    controller_monitor: MidiMonitorSnapshot,
    /// Meter levels and peak hold for the 16 strips.
    controller_meter: [f32; 16],
    controller_peak: [f32; 16],
    controller_peak_until: [f64; 16],
    controller_last_time: f64,
    /// Bend value currently drawn on the wheel (springs back to centre).
    controller_bend_display: i16,
    controller_bend_target: i16,
    controller_bend_channel: usize,
    controller_bend_changed_at: f64,
    controller_bend_manual_until: f64,
    /// Note currently held by the mouse on the virtual piano.
    controller_playing: Option<(u8, u8)>,
    /// Fade-out state for the settings modal.
    settings_closing: bool,
    settings_fade: f32,
    /// True while the Song Data folder reset confirmation is open.
    song_restore_confirm: Arc<AtomicBool>,
    /// Set when the user confirms clearing the song data folders.
    song_restore_confirmed: Arc<AtomicBool>,
    /// The currently open custom context menu.
    menu: Option<OpenMenu>,
    /// The native file dialog currently running on a worker thread.
    pending_dialog: Option<PendingDialog>,
    /// The Song Database window.
    library_open: bool,
    library_songs: Vec<SongRecord>,
    library_view: Vec<usize>,
    library_query: String,
    library_search_focus: bool,
    /// Lowercased text used by the in-window filter, one row per song.
    library_keys: Vec<String>,
    library_scan: Option<LibraryScan>,
    library_filter: LibraryFilter,
    library_progress: f32,
    library_error: Option<String>,
    library_note: Option<String>,
    /// Background result of the startup missing-file check.
    missing_check: Option<Receiver<std::result::Result<(usize, usize), String>>>,
    missing_checked: bool,
    /// Background load of the browse list.
    library_load: Option<Receiver<std::result::Result<Vec<SongRecord>, String>>>,
    /// Background check or compaction of the database file.
    library_op: Option<(
        LibraryOp,
        Receiver<std::result::Result<LibraryOpResult, String>>,
    )>,
    import_open: bool,
    import_source: Option<PathBuf>,
    import_filter: LibraryFilter,
    import_items: Vec<ImportItem>,
    import_view: Vec<usize>,
    import_scan: Option<ImportScan>,
    import_add: Option<ImportAdd>,
    import_progress: f32,
    import_note: Option<String>,
    import_error: Option<String>,
}

impl Default for KaraokeApp {
    fn default() -> Self {
        Self {
            song: None,
            path: None,
            playback: None,
            preview: None,
            preview_attempted: false,
            startup_volume_shown: false,
            sound_font: None,
            sound_font_path: None,
            startup_active: false,
            startup_status: String::new(),
            startup_detail: String::new(),
            startup_started: 0.0,
            startup_ready_at: None,
            startup_receiver: None,
            startup_path: None,
            startup_texture: None,
            splash_centered: false,
            center_main_pending: 0,
            fullscreen_pending: 0,
            pending_open: None,
            pending_playlist: Vec::new(),
            volume: 0.7,
            transpose: 0,
            paused: true,
            scrubbing: false,
            scrub_value: 0.0,
            error: None,
            settings: Settings::default(),
            stopped_index: None,
            midi_ports: Vec::new(),
            midi_ports_loaded: false,
            midi_page_error: None,
            split_open: false,
            split_window: Arc::new(Mutex::new(SplitWindow::default())),
            midi_editor_open: false,
            midi_editor_window: Arc::new(Mutex::new(MidiEditorWindow::default())),
            library_window: Arc::new(Mutex::new(LibraryWindow::default())),
            settings_pages: Arc::new(Mutex::new(SettingsPages::default())),
            import_window: Arc::new(Mutex::new(ImportWindow::default())),
            import_gen: 0,
            library_songs_gen: 0,
            library_view_gen: 0,
            playlist: Vec::new(),
            playlist_index: usize::MAX,
            titles: std::collections::HashMap::new(),
            library_meta: std::collections::HashMap::new(),
            indicator: None,
            song_card_pending: false,
            song_card_until: None,
            playlist_rect: Rect::NOTHING,
            central_rect: Rect::NOTHING,
            lyric_search_rect: Rect::NOTHING,
            lyric_overlay_rect: Rect::NOTHING,
            background_texture: None,
            default_background: None,
            soundfont_error: None,
            background_error: None,
            lyric_area: Rect::NOTHING,
            last_key_shortcut: 0.0,
            last_bpm_shortcut: 0.0,
            bpm: 120.0,
            applied_speed: 1.0,
            bar_seek_anchor: None,
            meter: MeterMap::new(96, Vec::new()),
            song_end_us: 0,
            playlist_selection: None,
            lyric_search: String::new(),
            lyric_search_focus: false,
            lyric_search_id: None,
            midi_menu_rect: Rect::NOTHING,
            tools_menu_rect: Rect::NOTHING,
            background_menu_rect: Rect::NOTHING,
            song_file_icons: None,
            lyric_overlay_visible: false,
            lyric_overlay_until: 0.0,
            lyric_overlay_remembered: false,
            lyric_browse: None,
            search_restore_playlist: false,
            lyric_results: Vec::new(),
            lyric_result_index: 0,
            lyric_query_cache: String::new(),
            lyric_last_query: String::new(),
            lyric_db: None,
            temp_playlist_until: None,
            temp_playlist_hidden: false,
            settings_open: false,
            settings_just_opened: false,
            settings_section: 0,
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
            exit_confirm_open: Arc::new(AtomicBool::new(false)),
            exit_confirmed: Arc::new(AtomicBool::new(false)),
            main_minimized: false,
            controller_open: false,
            controller_detail_open: false,
            controller_detail_program_open: false,
            controller_channel: 0,
            controller_channel_transpose: [0; 16],
            controller_locks: 0,
            controller_mutes: 0,
            controller_solo: None,
            controller_used: 0,
            controller_programs: [0; 16],
            controller_monitor: MidiMonitorSnapshot::default(),
            controller_meter: [0.0; 16],
            controller_peak: [0.0; 16],
            controller_peak_until: [0.0; 16],
            controller_last_time: 0.0,
            controller_bend_display: 0,
            controller_bend_target: 0,
            controller_bend_channel: 0,
            controller_bend_changed_at: 0.0,
            controller_bend_manual_until: 0.0,
            controller_playing: None,
            settings_closing: false,
            settings_fade: 1.0,
            song_restore_confirm: Arc::new(AtomicBool::new(false)),
            song_restore_confirmed: Arc::new(AtomicBool::new(false)),
            menu: None,
            pending_dialog: None,
            library_open: false,
            library_songs: Vec::new(),
            library_view: Vec::new(),
            library_query: String::new(),
            library_search_focus: false,
            library_keys: Vec::new(),
            library_scan: None,
            library_filter: LibraryFilter::All,
            library_progress: 0.0,
            library_error: None,
            library_note: None,
            missing_check: None,
            missing_checked: false,
            library_load: None,
            library_op: None,
            import_open: false,
            import_source: None,
            import_filter: LibraryFilter::All,
            import_items: Vec::new(),
            import_view: Vec::new(),
            import_scan: None,
            import_add: None,
            import_progress: 0.0,
            import_note: None,
            import_error: None,
        }
    }
}

pub fn run(initial: Vec<PathBuf>) -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../logo/DontreeCrab_icon.png"
    )))
    .expect("valid DontreeCrab icon");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icon)
            // The window opens as a small, borderless splash and is expanded
            // to the real size once the SoundFont is ready.
            .with_inner_size([480.0, 324.0])
            .with_min_inner_size([480.0, 324.0])
            .with_resizable(false)
            .with_decorations(false)
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
        // The player handles Ctrl +/- itself so it can stop the auto-fit from
        // overriding a manual zoom level.
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);
        ensure_program_folders();

        let mut app = Self::default();
        app.settings = Settings::load();
        app.controller_open = app.settings.controller_open;
        i18n::init();
        i18n::set_language(&app.settings.language);
        app.refresh_library_meta();
        app.lyric_font_loaded = install_fonts(&cc.egui_ctx, app.settings.lyric_font.as_deref());
        app.volume = app.settings.volume;
        app.transpose = app.settings.transpose;

        let stored_font = app.settings.soundfont.clone().filter(|path| path.is_file());
        app.begin_startup(stored_font);
        app.start_missing_check();

        // Files passed on the command line are opened once the startup splash
        // has finished and the SoundFont is ready.
        if let Some((first, rest)) = initial.split_first() {
            app.pending_open = Some(first.clone());
            app.pending_playlist = rest.to_vec();
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
        if self.search_restore_playlist {
            settings.show_playlist = true;
        }
        if let Err(err) = settings.save() {
            self.error = Some(format!("settings: {err:#}"));
        }
    }

    /// Reset the player-side fields of a settings page.
    ///
    /// The window sends its own settings copy on Save, but the folders and
    /// the lyric font are merged from the player so a stale window copy cannot
    /// undo a file dialog. Applying the page reset here keeps the reset from
    /// being reverted by that merge.
    fn reset_settings_page(&mut self, section: usize, ctx: &egui::Context) {
        let d = Settings::default();
        match section {
            1 => {
                self.settings.lyric_font = d.lyric_font.clone();
                self.settings.lyric_font_style = d.lyric_font_style;
                self.lyric_font_loaded = install_fonts(ctx, self.settings.lyric_font.as_deref());
            }
            6 => {
                self.settings.ncn_folder = d.ncn_folder.clone();
                self.settings.kmid_folder = d.kmid_folder.clone();
                self.settings.kar_folder = d.kar_folder.clone();
                self.settings.mkc_folder = d.mkc_folder.clone();
            }
            _ => {}
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

    /// Resolve the SoundFont and start loading it in the background. The
    /// opening splash stays visible until the worker finishes.
    fn begin_startup(&mut self, stored: Option<PathBuf>) {
        self.startup_active = true;
        self.splash_centered = false;
        self.startup_started = 0.0;
        self.startup_ready_at = None;
        self.startup_status = tr("Starting DontreeCrab");
        self.startup_detail.clear();

        let resolved = match soundfont::resolve(stored.as_deref()) {
            Ok(path) => std::path::absolute(&path).unwrap_or(path),
            Err(err) => {
                self.error = Some(format!("{err:#}"));
                self.startup_status = tr("No SoundFont found");
                self.startup_detail = tr("Open Settings → SoundFont to choose one");
                return;
            }
        };

        self.startup_detail = resolved
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.startup_path = Some(resolved.clone());

        let (sender, receiver) = std::sync::mpsc::channel();
        self.startup_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = soundfont::load(&resolved).map_err(|err| format!("{err:#}"));
            let _ = sender.send(result);
        });
    }

    fn poll_startup(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if self.startup_started == 0.0 {
            self.startup_started = now;
        }

        if let Some(receiver) = &self.startup_receiver {
            match receiver.try_recv() {
                Ok(Ok(font)) => {
                    self.sound_font = Some(font);
                    self.sound_font_path = self.startup_path.clone();
                    self.startup_receiver = None;
                    self.startup_status = tr("Ready");
                    self.startup_detail.clear();
                    self.startup_ready_at = Some(now + 0.6);
                }
                Ok(Err(err)) => {
                    self.startup_receiver = None;
                    self.error = Some(err);
                    self.startup_status = tr("SoundFont not loaded");
                    self.startup_detail = tr("Open Settings → SoundFont to choose one");
                    self.startup_ready_at = Some(now + 1.0);
                }
                Err(TryRecvError::Empty) => {
                    if now - self.startup_started > 0.2 {
                        self.startup_status = tr("Loading SoundFont");
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.startup_receiver = None;
                    self.error = Some(tr("SoundFont loader stopped unexpectedly"));
                    self.startup_status = tr("SoundFont not loaded");
                    self.startup_ready_at = Some(now + 1.0);
                }
            }
        }

        if self.startup_receiver.is_none() && self.startup_ready_at.is_none() {
            self.startup_ready_at = Some(now + 0.9);
        }
        if let Some(until) = self.startup_ready_at {
            if now >= until {
                self.finish_startup();
            }
        }
    }

    fn finish_startup(&mut self) {
        self.startup_active = false;
        self.startup_ready_at = None;
        if let Some(first) = self.pending_open.take() {
            self.open(first);
            let rest = std::mem::take(&mut self.pending_playlist);
            let _ = self.extend_playlist(rest);
        }
    }

    fn draw_splash(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, BG);

        let texture = self.startup_texture.get_or_insert_with(|| {
            let icon = eframe::icon_data::from_png_bytes(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../logo/DontreeCrab_Splash.png"
            )))
            .expect("valid DontreeCrab splash");
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            ui.ctx()
                .load_texture("startup_splash", image, egui::TextureOptions::LINEAR)
        });

        // Cover the window without stretching the artwork.
        let image_size = texture.size_vec2();
        let scale = (rect.width() / image_size.x).max(rect.height() / image_size.y);
        let draw_rect = Rect::from_center_size(rect.center(), image_size * scale);
        painter.image(
            texture.id(),
            draw_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        let ink = Color32::from_rgb(18, 24, 16);
        painter.text(
            Pos2::new(rect.right() - 24.0, rect.top() + 20.0),
            egui::Align2::RIGHT_TOP,
            APP_VERSION_LABEL,
            FontId::proportional(14.0),
            ink,
        );

        let now = ui.ctx().input(|i| i.time);
        let dots = if self.startup_receiver.is_some() {
            ["", ".", "..", "..."][((now * 2.0) as usize) % 4]
        } else {
            ""
        };
        let status = format!("{}{}", self.startup_status, dots);
        painter.text(
            Pos2::new(rect.left() + 24.0, rect.bottom() - 24.0),
            egui::Align2::LEFT_BOTTOM,
            status,
            FontId::proportional(13.0),
            ink,
        );
        if !self.startup_detail.is_empty() {
            painter.text(
                Pos2::new(rect.left() + 24.0, rect.bottom() - 44.0),
                egui::Align2::LEFT_BOTTOM,
                &self.startup_detail,
                FontId::proportional(11.0),
                Color32::from_gray(55),
            );
        }
    }

    fn reload_sound_font(&mut self, explicit: Option<PathBuf>) {
        self.preview = None;
        self.preview_attempted = false;
        self.sound_font = None;
        self.sound_font_path = None;

        let resolved = match soundfont::resolve(explicit.as_deref()) {
            Ok(path) => std::path::absolute(&path).unwrap_or(path),
            Err(err) => {
                self.soundfont_error = Some(format!("{err:#}"));
                self.error = Some(format!("{err:#}"));
                return;
            }
        };

        match soundfont::load(&resolved) {
            Ok(font) => {
                self.sound_font = Some(font);
                self.sound_font_path = Some(resolved);
                self.soundfont_error = None;
                if explicit.is_some() {
                    self.settings.soundfont = self.sound_font_path.clone();
                    self.save_settings();
                }
            }
            Err(err) => {
                self.soundfont_error = Some(format!("{err:#}"));
                self.error = Some(format!("{err:#}"));
            }
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
        self.prepare_controller_for_song(&song);
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
                karaoke_player::SourceFormat::Mkc => "mkc",
            };
            self.titles.insert(
                path.clone(),
                SongInfo {
                    code: self
                        .library_meta
                        .get(&path)
                        .map(|info| info.code.clone())
                        .unwrap_or_else(|| file_stem(&path)),
                    title,
                    artist: song.artist.clone().unwrap_or_default(),
                    file_type: file_type.to_owned(),
                    bpm: song.bpm,
                    key: song
                        .key
                        .clone()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| "C".to_owned()),
                    bars: library::bar_count(song).unwrap_or(0),
                    loaded: true,
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
        self.stopped_index = None;
        self.save_settings();
    }

    /// The MIDI fixes described by the settings (0-based GM numbers).
    fn midi_options(&self) -> MidiOptions {
        MidiOptions {
            lock_bass: self.settings.lock_bass,
            bass_program: (self.settings.lock_bass_program.clamp(1, 128) - 1) as u8,
            lock_drum_kit: self.settings.lock_drum_kit,
            drum_program: (self.settings.lock_drum_program.clamp(1, 128) - 1) as u8,
            lock_kick: self.settings.lock_kick,
            kick_note: self.settings.lock_kick_note.min(127) as u8,
            lock_snare: self.settings.lock_snare,
            snare_note: self.settings.lock_snare_note.min(127) as u8,
            metronome: self.settings.metronome,
            metronome_hihat: self.settings.metronome_hihat,
            metronome_first: self.settings.metronome_first.min(127) as u8,
            metronome_other: self.settings.metronome_other.min(127) as u8,
            group_volume: gated_group_values(
                &self.settings.group_volume,
                &self.settings.group_volume_on,
                100,
            ),
            group_velocity_min: gated_group_values(
                &self.settings.group_velocity_min,
                &self.settings.group_velocity_on,
                0,
            ),
            group_velocity_max: gated_group_values(
                &self.settings.group_velocity_max,
                &self.settings.group_velocity_on,
                127,
            ),
            drum_volume: gated_drum_values(
                &self.settings.drum_volume,
                &self.settings.drum_volume_on,
                100,
            ),
            drum_velocity_min: gated_drum_values(
                &self.settings.drum_velocity_min,
                &self.settings.drum_velocity_on,
                0,
            ),
            drum_velocity_max: gated_drum_values(
                &self.settings.drum_velocity_max,
                &self.settings.drum_velocity_on,
                127,
            ),
            sysex: self.settings.midi_sysex,
            transpose_mode: self.settings.transpose_mode,
            reset: self.settings.midi_reset,
        }
    }

    /// Effective mute mask sent to the player (Solo wins over the table).
    fn controller_mute_mask(&self) -> u16 {
        if let Some(solo) = self.controller_solo {
            return !(1u16 << (solo & 15)) & 0xFFFF;
        }
        let mut mask = self.controller_mutes & !(1u16 << MELODY_CHANNEL);
        if self.settings.melo_muted {
            mask |= 1u16 << MELODY_CHANNEL;
        }
        mask
    }

    /// Send the current mute/solo state to the audio thread.
    fn apply_controller_mutes(&self) {
        let mask = self.controller_mute_mask();
        if let Some(playback) = &self.playback {
            let _ = playback.commands.send(Command::SetChannelMutes { mask });
        } else if let Some(preview) = &self.preview {
            let _ = preview.commands.send(Command::SetChannelMutes { mask });
        }
    }

    /// Toggle one channel's mute; channel 9 is the existing melody mute.
    fn toggle_controller_mute(&mut self, channel: usize) {
        if channel == MELODY_CHANNEL as usize {
            self.settings.melo_muted = !self.settings.melo_muted;
            self.save_settings();
        } else if channel < 16 {
            self.controller_mutes ^= 1u16 << channel;
        }
        self.apply_controller_mutes();
    }

    /// Release the note held by the virtual piano, if any.
    fn stop_controller_note(&mut self) {
        if let Some((channel, key)) = self.controller_playing.take()
            && let Some(playback) = &self.playback
        {
            let _ = playback.commands.send(Command::NoteOff { channel, key });
        }
    }

    /// Solo one channel, or leave Solo when it is already selected.
    fn toggle_controller_solo(&mut self, channel: usize) {
        if channel >= 16 {
            return;
        }
        let channel = channel as u8;
        self.controller_solo = if self.controller_solo == Some(channel) {
            None
        } else {
            if self.controller_channel != channel as usize {
                self.stop_controller_note();
                self.controller_channel = channel as usize;
            }
            Some(channel)
        };
        self.apply_controller_mutes();
    }

    /// Solo is temporary: clear it on a new song and when a song ends.
    fn reset_controller_solo(&mut self) {
        if self.controller_solo.take().is_some() {
            self.apply_controller_mutes();
        }
    }

    /// Scan the loaded song for the channels and initial programs it uses.
    fn prepare_controller_for_song(&mut self, song: &Song) {
        let mut used = 0u16;
        let mut programs = [0u8; 16];
        let mut seen = [false; 16];
        for event in &song.events {
            let channel = match &event.kind {
                EventKind::NoteOn { channel, .. }
                | EventKind::NoteOff { channel, .. }
                | EventKind::ControlChange { channel, .. }
                | EventKind::ProgramChange { channel, .. }
                | EventKind::PitchBend { channel, .. }
                | EventKind::ChannelPressure { channel, .. }
                | EventKind::KeyPressure { channel, .. } => Some(*channel),
                _ => None,
            };
            if let Some(channel) = channel
                && (channel as usize) < 16
            {
                used |= 1u16 << channel;
            }
            if let EventKind::ProgramChange { channel, program } = &event.kind
                && (*channel as usize) < 16
                && !seen[*channel as usize]
            {
                programs[*channel as usize] = *program;
                seen[*channel as usize] = true;
            }
        }
        self.controller_playing = None;
        self.controller_used = used;
        self.controller_programs = programs;
        self.controller_channel_transpose = [0; 16];
        self.controller_locks = 0;
        self.controller_solo = None;
        self.controller_meter = [0.0; 16];
        self.controller_peak = [0.0; 16];
        self.controller_peak_until = [0.0; 16];
    }

    /// Re-read the list of MIDI output ports.
    fn refresh_midi_ports(&mut self) {
        self.midi_ports = audio::output_ports();
        self.midi_ports_loaded = true;
    }

    /// Per-channel output names from the settings (always 16 entries).
    fn split_port_names(&self) -> [String; 16] {
        let mut names: [String; 16] = Default::default();
        for (index, name) in names.iter_mut().enumerate() {
            *name = self
                .settings
                .split_ports
                .get(index)
                .cloned()
                .unwrap_or_default();
        }
        names
    }

    /// Per-instrument-group output names from the settings (always 20).
    fn group_port_names(&self) -> [String; 20] {
        let mut names: [String; 20] = Default::default();
        for (index, name) in names.iter_mut().enumerate() {
            *name = self
                .settings
                .group_ports
                .get(index)
                .cloned()
                .unwrap_or_default();
        }
        names
    }

    /// Per-drum-group output names from the settings (always 9).
    fn drum_port_names(&self) -> [String; audio::DRUM_GROUPS] {
        let mut names: [String; audio::DRUM_GROUPS] = Default::default();
        for (index, name) in names.iter_mut().enumerate() {
            *name = self
                .settings
                .drum_ports
                .get(index)
                .cloned()
                .unwrap_or_default();
        }
        names
    }

    /// Build the routing described by the settings and send it to the player.
    fn apply_midi_routing(&mut self) -> Result<(), String> {
        let channels = self.split_port_names();
        let groups = self.group_port_names();
        let drums = self.drum_port_names();
        let routing =
            audio::MidiRouting::open(&self.settings.midi_out_port, &channels, &groups, &drums)?;
        if let Some(playback) = &self.playback {
            let _ = playback.commands.send(Command::SetMidiRouting(routing));
        }
        Ok(())
    }

    /// Switch the default output (`""` = the built-in SoundFont).
    fn apply_midi_out(&mut self, port: String) {
        // Re-selecting the current device must not rebuild the routing.
        if port == self.settings.midi_out_port {
            return;
        }
        let previous = self.settings.midi_out_port.clone();
        self.settings.midi_out_port = port;
        match self.apply_midi_routing() {
            Ok(()) => self.midi_page_error = None,
            Err(err) => {
                self.settings.midi_out_port = previous;
                self.midi_page_error = Some(tr_args(
                    "Could not open MIDI output: {}",
                    &[&err as &dyn std::fmt::Display],
                ));
                let _ = self.apply_midi_routing();
            }
        }
        self.save_settings();
    }

    /// Send the selected MIDI reset (Reset MIDI button).
    fn send_midi_reset(&mut self) {
        if let Some(playback) = &self.playback {
            let _ = playback
                .commands
                .send(Command::MidiReset(self.settings.midi_reset));
            return;
        }
        // No song is loaded, so only an external device can be reset.
        let port = self.settings.midi_out_port.clone();
        if port.is_empty() {
            return;
        }
        match audio::MidiOut::open(&port) {
            Ok(mut out) => {
                out.reset_channels();
                if let Some(sysex) = self.settings.midi_reset.sysex() {
                    out.send(sysex);
                }
                self.midi_page_error = None;
            }
            Err(err) => {
                self.midi_page_error = Some(tr_args(
                    "Could not open MIDI output: {}",
                    &[&format!("{err:#}") as &dyn std::fmt::Display],
                ));
            }
        }
    }

    /// Push the current MIDI fixes to the audio thread.
    fn send_midi_options(&self) {
        if let Some(playback) = &self.playback {
            let _ = playback
                .commands
                .send(Command::SetMidiOptions(self.midi_options()));
        }
    }

    /// Start a silent audio stream for the virtual piano when no song is open.
    ///
    /// It uses the same SoundFont, master volume and channel mutes as the song
    /// playback, and is dropped as soon as a song starts or the controller
    /// closes so only one device stream is ever open.
    fn ensure_preview(&mut self) {
        if self.preview.is_some() || self.preview_attempted {
            return;
        }
        let Some(font) = self.sound_font.clone() else {
            return;
        };
        self.preview_attempted = true;

        let monitor = Arc::new(MidiMonitor::default());
        let monitor_for_player = Arc::clone(&monitor);
        let volume = self.volume;
        let transpose = self.transpose;
        let mutes = self.controller_mute_mask();
        let options = self.midi_options();
        let buffer_ms = self.settings.sf2_buffer_ms;
        let polyphony = self.settings.max_polyphony as usize;

        let handle = audio::start(
            move |rate| {
                let mut player = Player::new_preview(font.clone(), rate, transpose, polyphony)?;
                player.set_monitor(Arc::clone(&monitor_for_player));
                player.set_volume(volume);
                player.set_channel_mutes(mutes);
                player.set_midi_options(options);
                Ok(player)
            },
            Arc::new(AtomicU64::new(0)),
            buffer_ms,
        );

        if let Ok(handle) = handle {
            self.preview = Some(Preview {
                commands: handle.commands.clone(),
                _handle: handle,
                monitor,
            });
        }
    }

    fn start_audio(&mut self, song: &Song) -> Result<()> {
        // The preview stream is only for the idle controller; free the device
        // before opening the song playback.
        self.preview = None;
        self.preview_attempted = false;
        let Some(font) = self.sound_font.clone() else {
            anyhow::bail!(tr("No SoundFont loaded"));
        };

        let position = Arc::new(AtomicU64::new(0));
        let transpose = self.transpose;
        let volume = self.volume;
        let controller_mutes = self.controller_mute_mask();
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

        // Validate the saved output first so a missing device falls back to
        // the SoundFont with a visible message instead of failing playback.
        let options = self.midi_options();
        let routing_default = self.settings.midi_out_port.clone();
        let routing_channels = self.split_port_names();
        let routing_groups = self.group_port_names();
        let routing_drums = self.drum_port_names();
        let mut pending_routing = match audio::MidiRouting::open(
            &routing_default,
            &routing_channels,
            &routing_groups,
            &routing_drums,
        ) {
            Ok(routing) => Some(routing),
            Err(err) => {
                // Keep the saved devices: they may just be busy right now, so
                // the next song should try them again.
                let message = tr_args(
                    "Could not open MIDI output: {}",
                    &[&err as &dyn std::fmt::Display],
                );
                self.error = Some(message.clone());
                self.midi_page_error = Some(message);
                None
            }
        };

        let monitor = Arc::new(MidiMonitor::default());
        let monitor_for_player = Arc::clone(&monitor);
        let handle = audio::start(
            move |rate| {
                let mut player =
                    Player::new_with_polyphony(song, font.clone(), rate, transpose, max_polyphony)?;
                player.set_monitor(Arc::clone(&monitor_for_player));
                player.set_midi_options(options);
                player.set_channel_mutes(controller_mutes);
                // The first attempt uses the routing opened above; a retry
                // (rejected buffer size) opens the ports again.
                let routing = match pending_routing.take() {
                    Some(routing) => routing,
                    None => audio::MidiRouting::open(
                        &routing_default,
                        &routing_channels,
                        &routing_groups,
                        &routing_drums,
                    )
                    .unwrap_or_else(|_| audio::MidiRouting::internal()),
                };
                player.set_midi_routing(routing);
                player.set_volume(volume);
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
            monitor,
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
                    } else if self.song.is_some() {
                        self.queue_song(path);
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
            DialogKind::BackgroundImage => {
                if let Some(path) = paths.pop() {
                    self.settings.background_image = Some(path);
                    self.settings.background_style = BackgroundStyle::Image;
                    // Force a reload even when the same file is picked again.
                    self.background_texture = None;
                    self.save_settings();
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
            DialogKind::NcnFolder => {
                if let Some(path) = paths.pop() {
                    self.settings.ncn_folder = Some(path);
                    self.save_settings();
                }
            }
            DialogKind::KmidFolder => {
                if let Some(path) = paths.pop() {
                    self.settings.kmid_folder = Some(path);
                    self.save_settings();
                }
            }
            DialogKind::KarFolder => {
                if let Some(path) = paths.pop() {
                    self.settings.kar_folder = Some(path);
                    self.save_settings();
                }
            }
            DialogKind::MkcFolder => {
                if let Some(path) = paths.pop() {
                    self.settings.mkc_folder = Some(path);
                    self.save_settings();
                }
            }
            DialogKind::ImportFolder => {
                if let Some(folder) = paths.pop() {
                    self.start_import_scan(folder);
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
            if path.is_file() {
                self.playlist.push(path.clone());
                added.push(path);
            }
        }
        self.save_settings();
        added
    }

    /// Append a song to the playlist without touching playback.
    fn queue_song(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        if !path.is_file() {
            return;
        }
        self.playlist.push(path.clone());
        if !self.titles.contains_key(&path) {
            let info = self.load_song_info(&path);
            self.titles.insert(path, info);
        }
        self.save_settings();
    }

    /// The path currently shown in the search overlay.
    fn selected_lyric_path(&self) -> Option<PathBuf> {
        if self.lyric_search.trim().is_empty() {
            if let Some(browse) = &self.lyric_browse {
                return Some(browse.hit.path.clone());
            }
        }
        self.lyric_results
            .get(self.lyric_result_index)
            .map(|hit| hit.path.clone())
    }

    /// Enter in the search overlay: play when idle, otherwise queue the song
    /// so the current performance is not interrupted.
    fn select_search_song(&mut self, path: PathBuf, ctx: &egui::Context) {
        // "Play immediately" starts a song when idle, but a loaded song is
        // never interrupted: the selection waits in the playlist instead.
        if self.settings.play_immediately && self.song.is_none() {
            self.open(path);
        } else {
            self.queue_song(path);
        }
        self.hide_lyric_overlay_preserve_browse(ctx);
    }

    /// When "Play immediately" is enabled, start the first new playlist entry.
    fn play_first_added(&mut self, added: Vec<PathBuf>) {
        if self.settings.play_immediately && self.song.is_none() {
            if let Some(path) = added.into_iter().next() {
                self.open(path.clone());
                if let Some(index) = self.playlist.iter().rposition(|item| item == &path) {
                    self.playlist_index = index;
                    self.playlist_selection = Some(index);
                }
            }
        }
    }

    /// Metadata for one playlist entry, loading it on demand for export.
    fn song_info(&self, path: &Path) -> SongInfo {
        if let Some(info) = self.titles.get(path) {
            return info.clone();
        }
        self.load_song_info(path)
    }

    /// Metadata for one playlist row. The song database wins, then the
    /// session cache, and only then is the file parsed.
    fn playlist_info(&mut self, path: &Path) -> SongInfo {
        if let Some(info) = self.library_meta.get(path) {
            return info.clone();
        }
        if let Some(info) = self.titles.get(path) {
            if info.loaded {
                return info.clone();
            }
        }
        let info = self.load_song_info(path);
        self.titles.insert(path.to_owned(), info.clone());
        info
    }

    /// Start loading the playlist metadata cache from the song database.
    fn refresh_library_meta(&mut self) {
        self.start_library_load();
    }

    /// Rebuild the playlist metadata cache from the loaded library rows.
    fn rebuild_library_meta(&mut self) {
        self.library_meta.clear();
        // A scan or import may have stored new lyrics.
        self.lyric_query_cache.clear();
        self.library_keys.clear();
        self.library_keys.reserve(self.library_songs.len());
        for song in &self.library_songs {
            let path = std::path::absolute(&song.path).unwrap_or_else(|_| song.path.clone());
            self.library_keys.push(format!(
                "{}\n{}\n{}\n{}",
                song.code.to_lowercase(),
                song.file_name.to_lowercase(),
                song.title.to_lowercase(),
                song.artist.to_lowercase()
            ));
            self.library_meta.insert(
                path,
                SongInfo {
                    code: song.code.clone(),
                    title: song.title.clone(),
                    artist: song.artist.clone(),
                    file_type: song.format.clone(),
                    bpm: song.tempo.unwrap_or(0.0),
                    key: song
                        .key
                        .clone()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| "C".to_owned()),
                    bars: song.bars.unwrap_or(0),
                    loaded: true,
                },
            );
        }
    }

    /// Parse one file into playlist metadata without touching the cache.
    fn load_song_info(&self, path: &Path) -> SongInfo {
        if let Ok(song) = load(path, &LoadOptions::default()) {
            let title = song.title.clone().unwrap_or_else(|| file_stem(path));
            let file_type = match song.format {
                karaoke_player::SourceFormat::Kar => "kar",
                karaoke_player::SourceFormat::Ncn => "ncn",
                karaoke_player::SourceFormat::Kmid => "kmid",
                karaoke_player::SourceFormat::Midi => "midi",
                karaoke_player::SourceFormat::Mkc => "mkc",
            };
            return SongInfo {
                code: file_stem(path),
                title,
                artist: song.artist.clone().unwrap_or_default(),
                file_type: file_type.to_owned(),
                bpm: song.bpm,
                key: song
                    .key
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "C".to_owned()),
                bars: library::bar_count(&song).unwrap_or(0),
                loaded: true,
            };
        }

        SongInfo {
            code: file_stem(path),
            title: file_stem(path),
            artist: String::new(),
            file_type: file_type_guess(path),
            bpm: 0.0,
            key: "C".to_owned(),
            bars: 0,
            loaded: true,
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
                    self.playlist.push(path.clone());
                    added.push(path.clone());
                    self.titles.entry(path).or_insert(SongInfo {
                        code: String::new(),
                        title: entry.title,
                        artist: entry.artist,
                        file_type: entry.file_type,
                        bpm: 0.0,
                        key: "C".to_owned(),
                        bars: 0,
                        loaded: false,
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
        self.dismiss_song_card();
        if self.lyric_overlay_visible {
            self.hide_lyric_overlay(ctx);
        }
        self.search_restore_playlist = false;
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

        let index = next as usize;
        let path = self.playlist[index].clone();
        self.open(path);
        self.playlist_index = index;
        self.playlist_selection = Some(index);
    }

    /// Unload the finished song at the end of the playlist while keeping the
    /// playlist itself intact.
    fn clear_played_song(&mut self) {
        self.reset_controller_solo();
        self.controller_playing = None;
        self.controller_detail_program_open = false;
        self.controller_used = 0;
        self.controller_programs = [0; 16];
        self.controller_channel_transpose = [0; 16];
        self.controller_locks = 0;
        self.controller_monitor = MidiMonitorSnapshot::default();
        self.controller_meter = [0.0; 16];
        self.controller_peak = [0.0; 16];
        self.controller_peak_until = [0.0; 16];
        if let Some(playback) = &self.playback {
            // Stop first so the device gets an immediate all-notes-off; the
            // dropped stream below also sends one.
            let _ = playback.commands.send(Command::Stop);
        }
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
        self.reset_controller_solo();
        self.paused = true;

        if self.settings.delete_played_songs && !self.settings.repeat {
            if self.playlist_index < self.playlist.len() {
                let index = self.playlist_index;
                self.playlist.remove(index);
                if index < self.playlist.len() {
                    let path = self.playlist[index].clone();
                    self.open(path);
                    self.playlist_index = index;
                    self.playlist_selection = Some(index);
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

    /// In-window MIDI Controller: 16 strips, virtual piano and performance wheels.
    fn midi_controller_panel(&mut self, ctx: &egui::Context) {
        if !self.controller_open {
            return;
        }
        if self.playback.is_none() {
            self.ensure_preview();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
        let now = ctx.input(|i| i.time);
        let dt = (now - self.controller_last_time).clamp(0.0, 0.1);
        self.controller_last_time = now;
        if let Some(playback) = &self.playback {
            self.controller_monitor = playback.monitor.snapshot();
        } else if let Some(preview) = &self.preview {
            self.controller_monitor = preview.monitor.snapshot();
        }
        let snapshot = self.controller_monitor.clone();
        let mute_mask = self.controller_mute_mask();
        let used = self.controller_used;
        let selected = self.controller_channel.min(15);
        let commands = self
            .playback
            .as_ref()
            .map(|p| p.commands.clone())
            .or_else(|| self.preview.as_ref().map(|p| p.commands.clone()));

        // The song's pitch bend is held until the file sends centre. The wheel
        // springs back visually after a short pause so it never looks stuck.
        if now >= self.controller_bend_manual_until {
            let target = snapshot.bend[selected];
            if selected != self.controller_bend_channel || target != self.controller_bend_target {
                self.controller_bend_channel = selected;
                self.controller_bend_target = target;
                self.controller_bend_display = target;
                self.controller_bend_changed_at = now;
            } else if now - self.controller_bend_changed_at > 0.25 {
                let decay = (dt as f32 * 6.0).clamp(0.0, 1.0);
                let shown = self.controller_bend_display as f32 * (1.0 - decay);
                self.controller_bend_display = if shown.abs() < 16.0 { 0 } else { shown as i16 };
            }
        }

        for channel in 0..16 {
            let used_bit = used & (1u16 << channel) != 0;
            let audible = used_bit && mute_mask & (1u16 << channel) == 0;
            let active = (0..128).any(|note| snapshot.notes[note] & (1u16 << channel) != 0);
            let target = if audible && active {
                (snapshot.velocity[channel] as f32 / 127.0).max(0.12)
            } else {
                0.0
            };
            self.controller_meter[channel] = if target > self.controller_meter[channel] {
                target
            } else {
                (self.controller_meter[channel] - dt as f32 * 1.8).max(0.0)
            };
            if self.controller_meter[channel] > self.controller_peak[channel] {
                self.controller_peak[channel] = self.controller_meter[channel];
                self.controller_peak_until[channel] = now + 0.8;
            } else if now > self.controller_peak_until[channel] {
                self.controller_peak[channel] =
                    (self.controller_peak[channel] - dt as f32 * 0.7).max(0.0);
            }
        }

        let lyrics_upper = matches!(self.settings.text_visibility, TextVisibility::Upper);
        let pos = controller_panel_position(self.lyric_area, lyrics_upper);
        let mut mute_action = None;
        let mut solo_action = None;
        let mut channel_action = None;
        let mut detail_action = None;
        let mut transpose_action = None;
        let mut lock_action = None;
        let mut melo_action = false;

        egui::Area::new(egui::Id::unique("midi_controller_panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                ui.set_width(CONTROLLER_WIDTH);
                egui::Frame::NONE
                    .fill(Color32::from_gray(8))
                    .stroke(egui::Stroke::new(1.0, CONTROLLER_BORDER))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);

                        ui.horizontal_top(|ui| {
                            // 16 strips of 40 px with the leftover shared as gaps.
                            ui.spacing_mut().item_spacing.x = CONTROLLER_STRIP_GAP;
                            for channel in 0..16 {
                                let used_bit = used & (1u16 << channel) != 0;
                                let muted = mute_mask & (1u16 << channel) != 0;
                                let soloed = self.controller_solo == Some(channel as u8);
                                let key_offset = self.controller_channel_transpose[channel];
                                let locked = self.controller_locks & (1u16 << channel) != 0;
                                ui.allocate_ui_with_layout(
                                    egui::vec2(CONTROLLER_STRIP_WIDTH, 96.0),
                                    Layout::top_down(Align::Min),
                                    |ui| {
                                        let fill = if !used_bit {
                                            Color32::from_gray(55)
                                        } else if muted {
                                            Color32::from_rgb(190, 45, 45)
                                        } else {
                                            Color32::from_rgb(0, 125, 210)
                                        };
                                        let header = ui.add_sized(
                                            [40.0, 26.0],
                                            egui::Button::new(
                                                RichText::new(format!("{}", channel + 1))
                                                    .size(10.5)
                                                    .color(Color32::WHITE),
                                            )
                                            .fill(fill)
                                            .stroke(egui::Stroke::NONE),
                                        );
                                        if selected == channel {
                                            ui.painter().rect_stroke(
                                                header.rect.expand(1.0),
                                                2.0,
                                                egui::Stroke::new(1.0, Color32::WHITE),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
                                        if header.clicked() {
                                            channel_action = Some(channel);
                                            detail_action = Some(channel);
                                        } else if header.secondary_clicked() {
                                            channel_action = Some(channel);
                                            self.controller_detail_open = false;
                                            self.controller_detail_program_open = false;
                                        }
                                        let mut toggle_mute = false;
                                        let mut toggle_solo = false;
                                        header.context_menu(|ui| {
                                            if ui
                                                .button(if muted {
                                                    tr("Unmute")
                                                } else {
                                                    tr("Mute")
                                                })
                                                .clicked()
                                            {
                                                toggle_mute = true;
                                                ui.close();
                                            }
                                            if soloed {
                                                if ui.button(tr("Unsolo")).clicked() {
                                                    toggle_solo = true;
                                                    ui.close();
                                                }
                                            } else if ui.button(tr("Solo")).clicked() {
                                                toggle_solo = true;
                                                ui.close();
                                            }
                                            if commands.is_some() {
                                                let label = if locked {
                                                    tr("Unlock MIDI values")
                                                } else {
                                                    tr("Lock MIDI values")
                                                };
                                                if ui.button(label).clicked() {
                                                    lock_action = Some((channel, !locked));
                                                    ui.close();
                                                }
                                            }
                                            if channel != 9 && commands.is_some() {
                                                let label = if key_offset == 0 {
                                                    tr("Change key")
                                                } else {
                                                    format!(
                                                        "{} [{:+}]",
                                                        tr("Change key"),
                                                        key_offset
                                                    )
                                                };
                                                ui.menu_button(label, |ui| {
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{} [{:+}]",
                                                            tr("Change key"),
                                                            key_offset
                                                        ))
                                                        .size(10.0)
                                                        .color(Color32::from_gray(160)),
                                                    );
                                                    ui.separator();
                                                    if ui
                                                        .button(tr("Raise semitone [1+]"))
                                                        .clicked()
                                                    {
                                                        transpose_action = Some((
                                                            channel,
                                                            (key_offset + 1).clamp(-24, 24),
                                                        ));
                                                        ui.close();
                                                    }
                                                    if ui
                                                        .button(tr("Lower semitone [1-]"))
                                                        .clicked()
                                                    {
                                                        transpose_action = Some((
                                                            channel,
                                                            (key_offset - 1).clamp(-24, 24),
                                                        ));
                                                        ui.close();
                                                    }
                                                    if ui.button(tr("Raise octave [12+]")).clicked()
                                                    {
                                                        transpose_action = Some((
                                                            channel,
                                                            (key_offset + 12).clamp(-24, 24),
                                                        ));
                                                        ui.close();
                                                    }
                                                    if ui.button(tr("Lower octave [12-]")).clicked()
                                                    {
                                                        transpose_action = Some((
                                                            channel,
                                                            (key_offset - 12).clamp(-24, 24),
                                                        ));
                                                        ui.close();
                                                    }
                                                    ui.separator();
                                                    if ui.button(tr("Reset key")).clicked() {
                                                        transpose_action = Some((channel, 0));
                                                        ui.close();
                                                    }
                                                });
                                            }
                                        });
                                        if toggle_mute {
                                            mute_action = Some(channel);
                                        }
                                        if toggle_solo {
                                            solo_action = Some(channel);
                                        }

                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 1.0;
                                            let mut volume = snapshot.volume[channel];
                                            let (fader_rect, fader_response) = ui
                                                .allocate_exact_size(
                                                    egui::vec2(20.0, 66.0),
                                                    egui::Sense::click_and_drag(),
                                                );
                                            paint_controller_fader(
                                                ui.painter(),
                                                fader_rect,
                                                volume,
                                                used_bit,
                                                locked,
                                            );
                                            if used_bit
                                                && !locked
                                                && !self.controller_detail_open
                                                && (fader_response.dragged()
                                                    || fader_response.clicked())
                                                && let Some(pos) =
                                                    fader_response.interact_pointer_pos()
                                            {
                                                let t = ((fader_rect.bottom() - pos.y)
                                                    / fader_rect.height())
                                                .clamp(0.0, 1.0);
                                                volume = (t * 127.0).round() as u8;
                                                if let Some(commands) = &commands {
                                                    let _ =
                                                        commands.send(Command::SetChannelVolume {
                                                            channel: channel as u8,
                                                            value: volume,
                                                        });
                                                }
                                                self.controller_monitor.volume[channel] = volume;
                                            }
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(14.0, 66.0),
                                                egui::Sense::hover(),
                                            );
                                            paint_controller_meter(
                                                ui.painter(),
                                                rect,
                                                self.controller_meter[channel],
                                            );
                                        });
                                    },
                                );
                            }
                        });

                        ui.add_space(4.0);
                        let (piano_rect, _) = ui.allocate_exact_size(
                            egui::vec2(CONTROLLER_WIDTH - 16.0, 54.0),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter_at(piano_rect);
                        let wheel_w = 12.0;
                        let bend_rect = Rect::from_min_size(
                            Pos2::new(piano_rect.left() + 3.0, piano_rect.top()),
                            egui::vec2(wheel_w, piano_rect.height()),
                        );
                        let mod_rect = Rect::from_min_size(
                            Pos2::new(piano_rect.left() + 23.0, piano_rect.top()),
                            egui::vec2(wheel_w, piano_rect.height()),
                        );
                        let keyboard_rect = Rect::from_min_size(
                            Pos2::new(piano_rect.left() + 44.0, piano_rect.top()),
                            egui::vec2(CONTROLLER_KEYBOARD_WIDTH, piano_rect.height()),
                        );
                        paint_controller_keyboard(
                            &painter,
                            keyboard_rect,
                            &snapshot.notes,
                            selected,
                        );

                        if !self.controller_detail_open {
                            let keyboard_response = ui.interact(
                                keyboard_rect,
                                egui::Id::unique("controller_keyboard"),
                                egui::Sense::click_and_drag(),
                            );
                            let down = keyboard_response.is_pointer_button_down_on()
                                || keyboard_response.dragged();
                            let desired = if down {
                                keyboard_response
                                    .interact_pointer_pos()
                                    .and_then(|pos| controller_key_at(keyboard_rect, pos))
                                    .map(|key| (selected as u8, key))
                            } else {
                                None
                            };
                            if commands.is_some() && desired != self.controller_playing {
                                if let Some((channel, key)) = self.controller_playing
                                    && let Some(commands) = &commands
                                {
                                    let _ = commands.send(Command::NoteOff { channel, key });
                                }
                                if let Some((channel, key)) = desired
                                    && let Some(commands) = &commands
                                {
                                    let _ = commands.send(Command::NoteOn {
                                        channel,
                                        key,
                                        velocity: 100,
                                    });
                                }
                                self.controller_playing = desired;
                            } else if commands.is_none() {
                                self.controller_playing = None;
                            }
                        }

                        let mut bend = self.controller_bend_display;
                        if !self.controller_detail_open {
                            let bend_response = ui.interact(
                                bend_rect,
                                egui::Id::unique(("controller_bend", selected)),
                                egui::Sense::drag(),
                            );
                            if bend_response.dragged() {
                                if let Some(pos) = bend_response.interact_pointer_pos() {
                                    let t = ((bend_rect.bottom() - pos.y) / bend_rect.height())
                                        .clamp(0.0, 1.0);
                                    bend = (t * 16383.0 - 8192.0).round() as i16;
                                    if let Some(commands) = &commands {
                                        let _ = commands.send(Command::SetChannelBend {
                                            channel: selected as u8,
                                            value: bend,
                                        });
                                    }
                                    self.controller_monitor.bend[selected] = bend;
                                    self.controller_bend_display = bend;
                                    self.controller_bend_target = bend;
                                    self.controller_bend_changed_at = now;
                                    self.controller_bend_manual_until = now + 0.4;
                                }
                            }
                            if bend_response.drag_stopped() {
                                if let Some(commands) = &commands {
                                    let _ = commands.send(Command::SetChannelBend {
                                        channel: selected as u8,
                                        value: 0,
                                    });
                                }
                                self.controller_monitor.bend[selected] = 0;
                                self.controller_bend_display = 0;
                                self.controller_bend_target = 0;
                                self.controller_bend_changed_at = now;
                                self.controller_bend_manual_until = now + 0.4;
                                bend = 0;
                            }
                        }
                        paint_controller_wheel(
                            &painter,
                            bend_rect,
                            (bend as f32 + 8192.0) / 16383.0,
                        );

                        let mut modulation = snapshot.modulation[selected];
                        if !self.controller_detail_open {
                            let mod_response = ui.interact(
                                mod_rect,
                                egui::Id::unique(("controller_mod", selected)),
                                egui::Sense::drag(),
                            );
                            if mod_response.dragged() {
                                if let Some(pos) = mod_response.interact_pointer_pos() {
                                    let t = ((mod_rect.bottom() - pos.y) / mod_rect.height())
                                        .clamp(0.0, 1.0);
                                    modulation = (t * 127.0).round() as u8;
                                    if let Some(commands) = &commands {
                                        let _ = commands.send(Command::SetChannelModulation {
                                            channel: selected as u8,
                                            value: modulation,
                                        });
                                    }
                                    self.controller_monitor.modulation[selected] = modulation;
                                }
                            }
                        }
                        paint_controller_wheel(&painter, mod_rect, modulation as f32 / 127.0);

                        ui.add_space(5.0);
                        let (bar_rect, _) = ui.allocate_exact_size(
                            egui::vec2(CONTROLLER_WIDTH - 16.0, 26.0),
                            egui::Sense::hover(),
                        );
                        let used_bit = used & (1u16 << selected) != 0;
                        let program = if used_bit || self.preview.is_some() {
                            snapshot.program[selected]
                        } else {
                            self.controller_programs[selected]
                        };
                        let name = if selected == 9 && (used_bit || self.preview.is_some()) {
                            format!("Drum Kit {}", program as u32 + 1)
                        } else {
                            GM_PROGRAM_NAMES[program as usize].to_owned()
                        };
                        let label_area =
                            Rect::from_min_size(bar_rect.min, egui::vec2(150.0, bar_rect.height()));
                        ui.painter_at(label_area).text(
                            Pos2::new(label_area.left(), label_area.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("{:02} {}", selected + 1, name),
                            FontId::proportional(11.5),
                            if used_bit {
                                Color32::from_rgb(90, 225, 140)
                            } else {
                                Color32::WHITE
                            },
                        );
                        let spinner = Rect::from_min_size(
                            Pos2::new(bar_rect.left() + 152.0, bar_rect.center().y - 12.0),
                            egui::vec2(20.0, 24.0),
                        );
                        let spin_up = controller_spin_button_at(
                            ui,
                            Rect::from_min_size(spinner.min, egui::vec2(20.0, 12.0)),
                            true,
                            true,
                        );
                        if spin_up {
                            channel_action = Some((selected + 15) % 16);
                        }
                        let spin_down = controller_spin_button_at(
                            ui,
                            Rect::from_min_size(
                                Pos2::new(spinner.left(), spinner.top() + 12.0),
                                egui::vec2(20.0, 12.0),
                            ),
                            false,
                            true,
                        );
                        if spin_down {
                            channel_action = Some((selected + 1) % 16);
                        }

                        for index in 0..8 {
                            let rect = Rect::from_min_size(
                                Pos2::new(
                                    bar_rect.left() + 178.0 + index as f32 * 28.0,
                                    bar_rect.top(),
                                ),
                                egui::vec2(26.0, 26.0),
                            );
                            paint_controller_placeholder_button(
                                ui,
                                rect,
                                &format!("{}", index + 1),
                            )
                            .on_hover_text(tr("Effects will be enabled in the next phase"));
                        }
                        let stop_rect = Rect::from_min_size(
                            Pos2::new(bar_rect.left() + 402.0, bar_rect.top()),
                            egui::vec2(46.0, 26.0),
                        );
                        paint_controller_placeholder_button(ui, stop_rect, "STOP");
                        let mute_rect = Rect::from_min_size(
                            Pos2::new(bar_rect.left() + 554.0, bar_rect.top()),
                            egui::vec2(58.0, 26.0),
                        );
                        paint_controller_placeholder_button(ui, mute_rect, &tr("Mixer"))
                            .on_hover_text(tr("Coming soon"));
                        let melo_rect = Rect::from_min_size(
                            Pos2::new(bar_rect.left() + 622.0, bar_rect.top()),
                            egui::vec2(58.0, 26.0),
                        );
                        let melo_on = !self.settings.melo_muted;
                        let melo_clicked = ui
                            .put(
                                melo_rect,
                                egui::Button::new(
                                    RichText::new("MELO").size(10.5).color(Color32::WHITE),
                                )
                                .min_size(melo_rect.size())
                                .fill(if melo_on {
                                    Color32::from_rgb(0, 125, 210)
                                } else {
                                    Color32::from_gray(40)
                                }),
                            )
                            .clicked();
                        if melo_clicked {
                            melo_action = true;
                        }
                    });
            });

        // The click that opened or switched the popup must not also count as an
        // outside click in the same frame.
        let detail_was_same = detail_action.is_some_and(|channel| {
            self.controller_detail_open && self.controller_channel == channel
        });
        if let Some(channel) = channel_action {
            if channel != self.controller_channel {
                self.stop_controller_note();
                self.controller_channel = channel;
            }
        }
        if let Some(channel) = detail_action {
            self.controller_detail_program_open = false;
            if detail_was_same {
                self.controller_detail_open = false;
            } else {
                self.stop_controller_note();
                self.controller_channel = channel;
                self.controller_detail_open = true;
            }
        }

        if self.controller_detail_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.controller_detail_program_open {
                self.controller_detail_program_open = false;
            } else {
                self.controller_detail_open = false;
            }
        }

        let detail_pos = controller_detail_position(
            pos,
            self.lyric_area,
            self.controller_channel.min(15),
            lyrics_upper,
        );
        let detail_rect = Rect::from_min_size(
            detail_pos,
            egui::vec2(CONTROLLER_DETAIL_WIDTH, CONTROLLER_DETAIL_HEIGHT),
        );
        let controller_bar = Rect::from_min_max(
            Pos2::new(pos.x, pos.y + 160.0),
            Pos2::new(pos.x + CONTROLLER_WIDTH, pos.y + CONTROLLER_HEIGHT),
        );
        let clicked_outside = self.controller_detail_open
            && detail_action.is_none()
            && ctx.input(|i| i.pointer.any_click())
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|pointer| {
                    !detail_rect.contains(pointer) && !controller_bar.contains(pointer)
                });

        if self.controller_detail_open {
            let selected = self.controller_channel.min(15);
            let used_bit = used & (1u16 << selected) != 0;
            let muted = mute_mask & (1u16 << selected) != 0;
            let soloed = self.controller_solo == Some(selected as u8);
            let locked = self.controller_locks & (1u16 << selected) != 0;
            let can_edit = commands.is_some();
            let program = if used_bit || self.preview.is_some() {
                snapshot.program[selected]
            } else {
                self.controller_programs[selected]
            };
            let bank = snapshot.bank[selected];
            let mut volume = snapshot.volume[selected];
            let mut pan = snapshot.pan[selected];
            let mut reverb = snapshot.reverb[selected];
            let mut chorus = snapshot.chorus[selected];
            let mut program_choice = None;
            let mut instrument_clicked = false;
            let mut bank_step = 0i8;
            let s = CONTROLLER_DETAIL_SCALE;
            let program_text = if selected == 9 {
                format!("{:03} - {}", u32::from(program) + 1, drum_kit_name(program))
            } else {
                format!(
                    "{:03} - {}",
                    u32::from(program) + 1,
                    GM_PROGRAM_NAMES[program as usize]
                )
            };

            // Any click on the controller raises its layer, so keep the popup
            // on top for every frame while it is open. Its program list lives
            // inside the same area, so there is no nested popup to worry about.
            ctx.move_to_top(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique("midi_controller_detail"),
            ));
            egui::Area::new(egui::Id::unique("midi_controller_detail"))
                .order(egui::Order::Foreground)
                .fixed_pos(detail_pos)
                .show(ctx, |ui| {
                    ui.set_width(CONTROLLER_DETAIL_WIDTH);
                    let (popup_rect, _) = ui.allocate_exact_size(
                        egui::vec2(CONTROLLER_DETAIL_WIDTH, CONTROLLER_DETAIL_HEIGHT),
                        egui::Sense::hover(),
                    );
                    let origin = popup_rect.min;
                    ui.interact(
                        popup_rect,
                        ui.scope_id().with("controller_detail_bg"),
                        egui::Sense::click_and_drag(),
                    );
                    let painter = ui.painter().clone();
                    painter.rect_filled(popup_rect, 8.0, Color32::BLACK);
                    painter.rect_stroke(
                        popup_rect,
                        8.0,
                        egui::Stroke::new(1.0, CONTROLLER_BORDER),
                        egui::StrokeKind::Inside,
                    );

                    let rel = |x: f32, y: f32, w: f32, h: f32| {
                        Rect::from_min_size(
                            origin + egui::vec2(x * s, y * s),
                            egui::vec2(w * s, h * s),
                        )
                    };

                    // Mute / Solo buttons.
                    let mute_rect = rel(55.0, 9.0, 102.0, 33.0);
                    let mute_response = ui.interact(
                        mute_rect,
                        ui.scope_id().with("controller_detail_mute"),
                        egui::Sense::click(),
                    );
                    let mute_fill = if muted {
                        Color32::from_rgb(190, 45, 45)
                    } else if mute_response.hovered() {
                        Color32::from_gray(145)
                    } else {
                        Color32::from_gray(127)
                    };
                    painter.rect_filled(mute_rect, 4.0, mute_fill);
                    painter.text(
                        mute_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if muted { tr("Unmute") } else { tr("Mute") },
                        FontId::proportional(14.0 * s),
                        Color32::WHITE,
                    );
                    if mute_response.clicked() {
                        mute_action = Some(selected);
                    }

                    let solo_rect = rel(211.0, 9.0, 104.0, 33.0);
                    let solo_response = ui.interact(
                        solo_rect,
                        ui.scope_id().with("controller_detail_solo"),
                        egui::Sense::click(),
                    );
                    let solo_fill = if soloed {
                        Color32::from_rgb(0, 125, 210)
                    } else if solo_response.hovered() {
                        Color32::from_gray(145)
                    } else {
                        Color32::from_gray(127)
                    };
                    painter.rect_filled(solo_rect, 4.0, solo_fill);
                    painter.text(
                        solo_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if soloed { tr("Unsolo") } else { tr("Solo") },
                        FontId::proportional(14.0 * s),
                        Color32::WHITE,
                    );
                    if solo_response.clicked() {
                        solo_action = Some(selected);
                    }

                    // Instrument display; the whole box opens the program list.
                    let instrument_rect = rel(6.0, 52.0, 276.0, 33.0);
                    let instrument_response = ui.interact(
                        instrument_rect,
                        ui.scope_id().with("controller_detail_instrument"),
                        if can_edit {
                            egui::Sense::click()
                        } else {
                            egui::Sense::hover()
                        },
                    );
                    let instrument_fill = if can_edit && instrument_response.hovered() {
                        Color32::from_gray(145)
                    } else {
                        Color32::from_gray(127)
                    };
                    painter.rect_filled(instrument_rect, 4.0, instrument_fill);
                    painter.text(
                        Pos2::new(instrument_rect.left() + 7.0, instrument_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        &program_text,
                        FontId::proportional(12.5),
                        if can_edit {
                            Color32::WHITE
                        } else {
                            Color32::from_gray(90)
                        },
                    );
                    if can_edit && instrument_response.clicked() {
                        self.controller_detail_program_open = !self.controller_detail_program_open;
                        instrument_clicked = true;
                    }

                    // Bank number and spinner.
                    let bank_rect = rel(288.0, 52.0, 46.0, 33.0);
                    painter.rect_filled(bank_rect, 4.0, Color32::from_gray(127));
                    painter.text(
                        bank_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("{bank}"),
                        FontId::proportional(16.0 * s),
                        Color32::WHITE,
                    );
                    let bank_enabled = can_edit && selected != 9;
                    let up_rect = rel(340.0, 52.0, 26.0, 16.0);
                    let down_rect = rel(340.0, 69.0, 26.0, 17.0);
                    if paint_controller_detail_spinner(
                        ui,
                        &painter,
                        up_rect,
                        true,
                        bank_enabled,
                        ui.scope_id().with("controller_detail_bank_up"),
                    ) {
                        bank_step = 1;
                    }
                    if paint_controller_detail_spinner(
                        ui,
                        &painter,
                        down_rect,
                        false,
                        bank_enabled,
                        ui.scope_id().with("controller_detail_bank_down"),
                    ) {
                        bank_step = -1;
                    }

                    // Four labelled sliders.
                    if paint_controller_detail_slider(
                        ui,
                        &painter,
                        origin,
                        93.0,
                        &tr("Volume"),
                        &mut volume,
                        ui.scope_id().with("controller_detail_volume"),
                        can_edit,
                        locked,
                    ) {
                        if let Some(commands) = &commands {
                            let _ = commands.send(Command::SetChannelVolume {
                                channel: selected as u8,
                                value: volume,
                            });
                        }
                        self.controller_monitor.volume[selected] = volume;
                    }
                    if paint_controller_detail_slider(
                        ui,
                        &painter,
                        origin,
                        135.0,
                        &tr("Pan"),
                        &mut pan,
                        ui.scope_id().with("controller_detail_pan"),
                        can_edit,
                        locked,
                    ) {
                        if let Some(commands) = &commands {
                            let _ = commands.send(Command::SetChannelPan {
                                channel: selected as u8,
                                value: pan,
                            });
                        }
                        self.controller_monitor.pan[selected] = pan;
                    }
                    if paint_controller_detail_slider(
                        ui,
                        &painter,
                        origin,
                        177.0,
                        "REV",
                        &mut reverb,
                        ui.scope_id().with("controller_detail_reverb"),
                        can_edit,
                        locked,
                    ) {
                        if let Some(commands) = &commands {
                            let _ = commands.send(Command::SetChannelReverb {
                                channel: selected as u8,
                                value: reverb,
                            });
                        }
                        self.controller_monitor.reverb[selected] = reverb;
                    }
                    if paint_controller_detail_slider(
                        ui,
                        &painter,
                        origin,
                        219.0,
                        "CHO",
                        &mut chorus,
                        ui.scope_id().with("controller_detail_chorus"),
                        can_edit,
                        locked,
                    ) {
                        if let Some(commands) = &commands {
                            let _ = commands.send(Command::SetChannelChorus {
                                channel: selected as u8,
                                value: chorus,
                            });
                        }
                        self.controller_monitor.chorus[selected] = chorus;
                    }

                    // Program list covers the slider area while it is open.
                    if self.controller_detail_program_open {
                        let list_rect = rel(6.0, 91.0, 360.0, 167.0);
                        ui.interact(
                            list_rect,
                            ui.scope_id().with("controller_detail_list_bg"),
                            egui::Sense::click_and_drag(),
                        );
                        painter.rect_filled(list_rect, 6.0, Color32::from_gray(18));
                        painter.rect_stroke(
                            list_rect,
                            6.0,
                            egui::Stroke::new(1.0, Color32::from_gray(70)),
                            egui::StrokeKind::Inside,
                        );
                        let inner = list_rect.shrink(2.0);
                        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
                            ui.style_mut().spacing.item_spacing = egui::vec2(0.0, 0.0);
                            ui.style_mut().spacing.button_padding = egui::vec2(5.0, 2.0);
                            ui.style_mut().spacing.interact_size.y = 18.0;
                            let visuals = &mut ui.style_mut().visuals;
                            visuals.selection.bg_fill = Color32::from_gray(100);
                            visuals.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
                            visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                            visuals.widgets.hovered.weak_bg_fill = Color32::from_gray(60);
                            visuals.widgets.active.weak_bg_fill = Color32::from_gray(80);
                            visuals.widgets.inactive.fg_stroke =
                                egui::Stroke::new(1.0, Color32::WHITE);
                            visuals.widgets.hovered.fg_stroke =
                                egui::Stroke::new(1.0, Color32::WHITE);
                            visuals.widgets.active.fg_stroke =
                                egui::Stroke::new(1.0, Color32::WHITE);
                            egui::ScrollArea::vertical()
                                .id_salt(("controller_detail_programs", selected))
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_width(inner.width());
                                    if selected == 9 {
                                        for (kit, kit_name) in DRUM_KITS {
                                            let value = (kit - 1) as u8;
                                            if ui
                                                .selectable_label(
                                                    value == program,
                                                    RichText::new(format!(
                                                        "{:03} - Drum Kit {kit_name}",
                                                        value as u32 + 1
                                                    ))
                                                    .size(12.5),
                                                )
                                                .clicked()
                                            {
                                                program_choice = Some(value);
                                            }
                                        }
                                    } else {
                                        for value in 0..=127u8 {
                                            if ui
                                                .selectable_label(
                                                    value == program,
                                                    RichText::new(format!(
                                                        "{:03} - {}",
                                                        value as u32 + 1,
                                                        GM_PROGRAM_NAMES[value as usize]
                                                    ))
                                                    .size(12.5),
                                                )
                                                .clicked()
                                            {
                                                program_choice = Some(value);
                                            }
                                        }
                                    }
                                });
                        });
                        // Clicking outside the list (but not on the instrument
                        // box) closes it.
                        let click = ctx.input(|i| i.pointer.any_click());
                        let pointer = ctx.input(|i| i.pointer.interact_pos());
                        if click
                            && !instrument_clicked
                            && pointer.is_some_and(|pointer| {
                                !list_rect.contains(pointer) && !instrument_rect.contains(pointer)
                            })
                        {
                            self.controller_detail_program_open = false;
                        }
                    }
                });

            if let Some(value) = program_choice {
                if let Some(commands) = &commands {
                    let _ = commands.send(Command::SetChannelProgram {
                        channel: selected as u8,
                        program: value,
                    });
                }
                self.controller_monitor.program[selected] = value;
                self.controller_detail_program_open = false;
            }
            if bank_step != 0 {
                let bank = if bank_step > 0 {
                    (bank + 1).min(127)
                } else {
                    bank.saturating_sub(1)
                };
                if let Some(commands) = &commands {
                    let _ = commands.send(Command::SetChannelBank {
                        channel: selected as u8,
                        bank,
                    });
                }
                self.controller_monitor.bank[selected] = bank;
            }
        }

        if clicked_outside {
            self.controller_detail_open = false;
        }
        if !self.controller_detail_open {
            self.controller_detail_program_open = false;
        }

        if let Some((channel, locked)) = lock_action {
            if locked {
                self.controller_locks |= 1u16 << channel;
            } else {
                self.controller_locks &= !(1u16 << channel);
            }
            if let Some(commands) = &commands {
                let _ = commands.send(Command::SetChannelLocks {
                    mask: self.controller_locks,
                });
            }
            self.show_indicator(
                format!(
                    "CH {:02} {}",
                    channel + 1,
                    if locked { "LOCKED" } else { "UNLOCKED" }
                ),
                ctx,
            );
        }
        if let Some((channel, semitones)) = transpose_action {
            self.controller_channel_transpose[channel] = semitones;
            if let Some(commands) = &commands {
                let _ = commands.send(Command::SetChannelTranspose {
                    channel: channel as u8,
                    semitones,
                });
            }
            self.show_indicator(format!("CH {:02} KEY [{:+}]", channel + 1, semitones), ctx);
        }
        if let Some(channel) = mute_action {
            self.toggle_controller_mute(channel);
        }
        if let Some(channel) = solo_action {
            self.toggle_controller_solo(channel);
        }
        if melo_action {
            self.toggle_controller_mute(MELODY_CHANNEL as usize);
        }
    }

    fn playlist_panel(&mut self, ui: &mut egui::Ui) {
        // Fit the playlist to the window: fullscreen keeps the normal size,
        // while a smaller windowed player gets a smaller card.
        let scale = window_fit_scale(ui.ctx());
        let row_height = 50.0 * scale;
        let no_width = 82.0 * scale;
        let type_width = 104.0 * scale;
        let pad = 14.0 * scale;

        let mut menu_request: Option<(MenuKind, Pos2)> = None;
        let mut clicked: Option<usize> = None;

        // Keep the card on the opposite side of the lyrics. When the lyrics
        // are on top and the controller is docked at the bottom, the card
        // sits directly above the controller instead of under it.
        let lyrics_upper = matches!(
            self.settings.text_visibility,
            karaoke_player::settings::TextVisibility::Upper
        );
        let at_bottom = lyrics_upper;
        // A bottom card keeps a wider gap so it never touches the transport bar.
        let margin_top = if at_bottom { 14.0 } else { 18.0 };
        let margin_bottom = if at_bottom { 32.0 } else { 14.0 };
        // Three rows per page, with the panel sized so they fit exactly.
        let panel_height = (188.0 + 32.0 + margin_top + margin_bottom) * scale;
        // Float the playlist over the lyrics instead of taking layout space,
        // so the background keeps its full size while the card is open.
        let area = ui.available_rect_before_wrap();
        let controller_offset = if self.controller_open {
            CONTROLLER_OFFSET
        } else {
            0.0
        };
        let card_rect = if at_bottom && self.controller_open {
            let controller = controller_panel_position(area, true);
            let top = (controller.y - 10.0 - panel_height).max(area.top());
            Rect::from_min_max(
                Pos2::new(area.left(), top),
                Pos2::new(area.right(), top + panel_height),
            )
        } else if at_bottom {
            Rect::from_min_max(
                Pos2::new(area.left(), area.bottom() - panel_height),
                area.max,
            )
        } else {
            Rect::from_min_max(
                Pos2::new(area.left(), area.top() + controller_offset),
                Pos2::new(area.right(), area.top() + panel_height + controller_offset),
            )
        };
        let _ = egui::Area::new(egui::Id::unique("playlist"))
            .order(egui::Order::Middle)
            .fixed_pos(card_rect.min)
            .show(ui.ctx(), |ui| {
                ui.set_width(card_rect.width());
                ui.set_height(card_rect.height());
                egui::Frame::NONE
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(egui::Margin {
                        left: (34.0 * scale).round() as i8,
                        right: (34.0 * scale).round() as i8,
                        top: (margin_top * scale).round() as i8,
                        bottom: (margin_bottom * scale).round() as i8,
                    })
                    .show(ui, |ui| {
                        // A floating card with a gap around it, so the playlist reads
                        // as a panel instead of being glued to the window edges.
                        egui::Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(10, 10, 13, 195))
                            .stroke(egui::Stroke::new(1.0 * scale, Color32::from_gray(72)))
                            .corner_radius(egui::CornerRadius::same((16.0 * scale).round() as u8))
                            .inner_margin(egui::Margin::symmetric(
                                (24.0 * scale).round() as i8,
                                (16.0 * scale).round() as i8,
                            ))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.set_min_height(ui.available_height());

                                // Right-clicking anywhere in the panel opens this menu.
                                let background = ui.interact(
                                    ui.max_rect(),
                                    ui.scope_id().with("playlist_bg"),
                                    egui::Sense::click(),
                                );

                                let width = ui.available_width();
                                let (header, _) = ui.allocate_exact_size(
                                    egui::vec2(width, 36.0 * scale),
                                    egui::Sense::hover(),
                                );
                                let painter = ui.painter_at(header);
                                let header_font = FontId::proportional(15.0 * scale);
                                painter.text(
                                    Pos2::new(header.left() + pad, header.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    "NO.",
                                    header_font.clone(),
                                    FG,
                                );
                                painter.text(
                                    Pos2::new(header.left() + no_width + pad, header.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    "SONGS",
                                    header_font.clone(),
                                    FG,
                                );
                                painter.text(
                                    Pos2::new(header.right() - pad, header.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    "TYPE",
                                    header_font,
                                    FG,
                                );
                                painter.line_segment(
                                    [
                                        Pos2::new(header.left(), header.bottom() - 0.5),
                                        Pos2::new(header.right(), header.bottom() - 0.5),
                                    ],
                                    egui::Stroke::new(1.0 * scale, Color32::from_gray(70)),
                                );

                                ui.add_space(2.0 * scale);

                                // Exactly one page of rows: moving past the last row
                                // of a page brings the next page on screen.
                                let rows_per_page = 3usize;
                                let page_height = row_height * rows_per_page as f32;
                                let highlighted =
                                    self.playlist_selection.unwrap_or(self.playlist_index);
                                let mut page = if highlighted == usize::MAX {
                                    0
                                } else {
                                    highlighted / rows_per_page
                                };
                                // After removing songs the page may sit past the end.
                                if !self.playlist.is_empty() {
                                    page = page.min((self.playlist.len() - 1) / rows_per_page);
                                }

                                if self.playlist.is_empty() {
                                    let (empty, _) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), page_height),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter_at(empty).text(
                                        empty.center(),
                                        egui::Align2::CENTER_CENTER,
                                        tr("Playlist is empty\nright-click to add songs"),
                                        FontId::proportional(12.0 * scale),
                                        FG,
                                    );
                                } else {
                                    // Draw exactly one page; the arrow keys move the
                                    // page when the highlight leaves it.
                                    let first = page * rows_per_page;
                                    let last = (first + rows_per_page).min(self.playlist.len());
                                    let (list_rect, _) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), page_height),
                                        egui::Sense::hover(),
                                    );

                                    for index in first..last {
                                        let path = self.playlist[index].clone();
                                        let info = self.playlist_info(&path);
                                        let row = Rect::from_min_size(
                                            Pos2::new(
                                                list_rect.left(),
                                                list_rect.top()
                                                    + (index - first) as f32 * row_height,
                                            ),
                                            egui::vec2(list_rect.width(), row_height),
                                        );
                                        let response = ui.interact(
                                            row,
                                            ui.scope_id().with(("playlist_row", index)),
                                            egui::Sense::click(),
                                        );

                                        let selected = index == highlighted;
                                        if selected || response.hovered() {
                                            let fill = if selected {
                                                Color32::from_gray(72)
                                            } else {
                                                Color32::from_gray(34)
                                            };
                                            ui.painter().rect_filled(row, 3.0 * scale, fill);
                                        }

                                        let painter = ui.painter_at(row);
                                        let font = FontId::proportional(21.0 * scale);
                                        painter.text(
                                            Pos2::new(row.left() + pad, row.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            format!("{:02}", index + 1),
                                            font.clone(),
                                            FG,
                                        );
                                        painter.text(
                                            Pos2::new(row.right() - pad, row.center().y),
                                            egui::Align2::RIGHT_CENTER,
                                            info.file_type.to_uppercase(),
                                            font.clone(),
                                            FG,
                                        );

                                        let songs_left = row.left() + no_width + pad;
                                        let songs_right = row.right() - type_width;
                                        let songs = painter.with_clip_rect(Rect::from_min_max(
                                            Pos2::new(songs_left, row.top()),
                                            Pos2::new(songs_right, row.bottom()),
                                        ));
                                        // A highlighted row uses one solid white so every
                                        // column reads the same.
                                        let secondary = FG;
                                        let mut x = songs_left;
                                        let code = if info.code.is_empty() {
                                            file_stem(&path)
                                        } else {
                                            info.code.clone()
                                        };
                                        draw_playlist_segment(
                                            &songs,
                                            &mut x,
                                            row.center().y,
                                            &code,
                                            &font,
                                            secondary,
                                            14.0 * scale,
                                        );
                                        // Plain MIDI rows fall back to the file name as
                                        // the title; do not print it twice.
                                        if !info.title.is_empty()
                                            && !info.title.trim().eq_ignore_ascii_case(code.trim())
                                        {
                                            draw_playlist_segment(
                                                &songs,
                                                &mut x,
                                                row.center().y,
                                                &info.title,
                                                &font,
                                                FG,
                                                10.0 * scale,
                                            );
                                        }
                                        let meta = format!(
                                            "{:.0}-({})-[{}]",
                                            info.bpm, info.key, info.bars
                                        );
                                        draw_playlist_segment(
                                            &songs,
                                            &mut x,
                                            row.center().y,
                                            &meta,
                                            &font,
                                            secondary,
                                            8.0 * scale,
                                        );
                                        if !info.artist.is_empty() {
                                            draw_playlist_segment(
                                                &songs,
                                                &mut x,
                                                row.center().y,
                                                &format!("-> {}", info.artist),
                                                &font,
                                                secondary,
                                                0.0,
                                            );
                                        }

                                        if response.clicked() {
                                            clicked = Some(index);
                                        }
                                        if response.secondary_clicked() {
                                            if let Some(pos) =
                                                ui.ctx().input(|i| i.pointer.interact_pos())
                                            {
                                                menu_request = Some((MenuKind::Row(index), pos));
                                            }
                                        }
                                    }
                                }

                                if background.secondary_clicked() {
                                    if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos())
                                    {
                                        menu_request = Some((MenuKind::Background, pos));
                                    }
                                }
                            });
                    });
            });

        // Remember where the card is so the wheel can scroll it instead of
        // changing the volume.
        self.playlist_rect = card_rect;

        if let Some(index) = clicked {
            if let Some(path) = self.playlist.get(index).cloned() {
                self.open(path);
                self.playlist_index = index;
                self.playlist_selection = Some(index);
            }
        }
        if let Some((kind, pos)) = menu_request {
            self.open_menu(kind, pos);
        }
    }

    fn dismiss_song_card(&mut self) {
        self.song_card_until = None;
        self.song_card_pending = false;
    }

    fn show_lyric_overlay(&mut self, ctx: &egui::Context) {
        self.dismiss_song_card();
        let now = ctx.input(|i| i.time);
        self.search_restore_playlist = self.settings.show_playlist && !self.temp_playlist_hidden;
        if self.settings.show_playlist {
            self.settings.show_playlist = false;
        }
        self.temp_playlist_until = None;
        self.temp_playlist_hidden = false;
        self.lyric_overlay_visible = true;
        self.lyric_overlay_until = now + self.settings.popup_seconds as f64;
        self.lyric_search_focus = true;
    }

    fn hide_lyric_overlay(&mut self, ctx: &egui::Context) {
        self.lyric_overlay_visible = false;
        self.lyric_overlay_until = 0.0;
        self.lyric_overlay_remembered = false;
        if self.search_restore_playlist {
            self.settings.show_playlist = true;
            self.search_restore_playlist = false;
        }
        self.lyric_search.clear();
        self.lyric_results.clear();
        self.lyric_result_index = 0;
        self.lyric_query_cache.clear();
        self.lyric_last_query.clear();
        self.lyric_browse = None;
        self.lyric_search_rect = Rect::NOTHING;
        if let Some(id) = self.lyric_search_id.take() {
            ctx.data_mut(|data| data.remove::<egui::widgets::text_edit::TextEditState>(id));
        }
        ctx.memory_mut(|memory| memory.stop_text_input());
    }

    fn hide_lyric_overlay_preserve_browse(&mut self, ctx: &egui::Context) {
        let browse = self.lyric_browse.take();
        self.hide_lyric_overlay(ctx);
        self.lyric_browse = browse;
    }

    /// Timeout close: the popup hides but the query and its results stay, so
    /// Left/Right reopens on the same song.
    fn hide_lyric_overlay_keep_search(&mut self, ctx: &egui::Context) {
        self.lyric_overlay_visible = false;
        self.lyric_overlay_until = 0.0;
        if self.search_restore_playlist {
            self.settings.show_playlist = true;
            self.search_restore_playlist = false;
        }
        self.lyric_search_rect = Rect::NOTHING;
        if let Some(id) = self.lyric_search_id.take() {
            ctx.data_mut(|data| data.remove::<egui::widgets::text_edit::TextEditState>(id));
        }
        ctx.memory_mut(|memory| memory.stop_text_input());
        self.lyric_overlay_remembered = true;
    }

    fn update_lyric_overlay(&mut self, ctx: &egui::Context) {
        if self.lyric_overlay_visible && ctx.input(|i| i.time) > self.lyric_overlay_until {
            self.hide_lyric_overlay_keep_search(ctx);
        }
    }

    /// Move through the current search results, or through every song in the
    /// database when the search box is empty.
    fn step_lyric_selection(&mut self, delta: isize) {
        let query = self.lyric_search.trim().to_owned();
        if !query.is_empty() || (self.lyric_browse.is_none() && !self.lyric_results.is_empty()) {
            let len = self.lyric_results.len();
            if len == 0 {
                return;
            }
            let index = (self.lyric_result_index as isize + delta).rem_euclid(len as isize);
            self.lyric_result_index = index as usize;
            return;
        }

        if self.lyric_db.is_none() {
            self.lyric_db = Library::open_default().ok();
        }
        let total = match &self.lyric_browse {
            Some(browse) => browse.total,
            None => {
                let Some(library) = self.lyric_db.as_ref() else {
                    return;
                };
                library.active_song_count().unwrap_or(0)
            }
        };
        if total == 0 {
            self.lyric_browse = None;
            return;
        }
        let index = match &self.lyric_browse {
            Some(browse) => (browse.index as isize + delta).rem_euclid(total as isize) as usize,
            None => {
                if delta < 0 {
                    total - 1
                } else {
                    0
                }
            }
        };
        let hit = {
            let Some(library) = self.lyric_db.as_ref() else {
                return;
            };
            match library.song_hit_at(index) {
                Ok(Some(hit)) => hit,
                _ => return,
            }
        };
        self.lyric_browse = Some(LyricBrowse { index, total, hit });
    }

    fn load_lyric_query(&mut self, query: &str) {
        if self.lyric_db.is_none() {
            self.lyric_db = Library::open_default().ok();
        }
        let Some(library) = self.lyric_db.as_ref() else {
            return;
        };
        let mut matches = library.search(query, 50).unwrap_or_default();
        // If the exact text has no hits, fall back to the longest prefix that
        // still matches, so a half-typed phrase still shows useful songs.
        if matches.is_empty() {
            let mut fallback = query.to_owned();
            while fallback.chars().count() > 1 {
                fallback.pop();
                if let Ok(found) = library.search(&fallback, 50) {
                    if !found.is_empty() {
                        matches = found;
                        break;
                    }
                }
            }
        }
        // After the matches, list the rest of the library so Left/Right can
        // always browse on to the remaining songs.
        let mut seen: HashSet<PathBuf> = matches.iter().map(|hit| hit.path.clone()).collect();
        for record in &self.library_songs {
            if matches.len() >= 500 {
                break;
            }
            if record.missing || seen.contains(&record.path) {
                continue;
            }
            seen.insert(record.path.clone());
            let preview = record.lyr_preview.clone();
            matches.push(LyricHit {
                code: record.code.clone(),
                format: record.format.clone(),
                path: record.path.clone(),
                title: record.title.clone(),
                artist: record.artist.clone(),
                key: record.key.clone(),
                tempo: record.tempo,
                bars: record.bars,
                preview: preview.unwrap_or_default(),
            });
        }
        self.lyric_results = matches;
    }

    fn refresh_lyric_results(&mut self) {
        let query = self.lyric_search.trim().to_owned();
        if query == self.lyric_query_cache {
            return;
        }
        let previous = std::mem::replace(&mut self.lyric_query_cache, query.clone());
        self.lyric_result_index = 0;
        self.lyric_results.clear();
        if query.is_empty() {
            // After deleting the whole query, keep showing results based on
            // the last character that was typed.
            if previous.is_empty() {
                return;
            }
            let last = self
                .lyric_last_query
                .chars()
                .last()
                .map(|ch| ch.to_string())
                .unwrap_or_default();
            if !last.is_empty() {
                self.load_lyric_query(&last);
            }
            return;
        }
        self.lyric_last_query = query.clone();
        self.lyric_browse = None;
        self.load_lyric_query(&query);
    }

    /// Geometry of the song search popup background.
    ///
    /// The bar covers the info header and the white band fills everything below it
    /// up to the popup border. Keeping the band bottom at the popup bottom is what
    /// prevents a strip of the background showing under the white half.
    fn search_popup_bands(rect: Rect, bar_height: f32) -> (Rect, Rect) {
        let bar = Rect::from_min_size(rect.min, egui::vec2(rect.width(), bar_height));
        let lower = Rect::from_min_max(Pos2::new(rect.left(), bar.bottom()), rect.max);
        (bar, lower)
    }

    fn lyric_search_overlay(&mut self, ctx: &egui::Context) {
        self.refresh_lyric_results();

        let scale = window_fit_scale(ctx) * SEARCH_POPUP_SCALE;
        let band_alpha = 200u8;
        let bar_alpha = 120u8;
        // Proportions follow the reference song list: a black info bar on top
        // and white boxes underneath. The search box matches the bar, while the
        // lyric preview keeps its smaller height and text.
        // Both boxes are the same size and sit centred in the white half.
        let box_height = 34.0 * scale;
        // The bar is tall enough for Thai vowels that hang below the line.
        let header_height = 42.0 * scale;
        let gap = 8.0 * scale;
        let white_gap = 10.0 * scale;
        // The boxes sit near the bottom edge, like the reference song list.
        let bottom_gap = 4.0 * scale;
        let margin = 10.0 * scale;
        // Keep the text and boxes away from the panel edges.
        let padding = (20.0 * scale).round();
        // Keep the panel clear of the window edges and rounded corners.
        let inset = (16.0 * scale).round();
        let height = margin + header_height + white_gap + bottom_gap + box_height;

        let query = self.lyric_search.trim().to_owned();
        self.lyric_result_index = self
            .lyric_result_index
            .min(self.lyric_results.len().saturating_sub(1));
        let display = if query.is_empty() {
            self.lyric_browse
                .as_ref()
                .map(|browse| browse.hit.clone())
                .or_else(|| self.lyric_results.get(self.lyric_result_index).cloned())
        } else {
            self.lyric_results.get(self.lyric_result_index).cloned()
        };

        let mut code = String::new();
        let mut title = String::new();
        let mut artist = String::new();
        let mut meta = String::new();
        let mut file_type = String::new();
        let mut file_format = String::new();
        let mut preview: Option<String> = None;
        let mut message: Option<String> = None;

        if let Some(hit) = &display {
            code = hit.code.clone();
            title = hit.title.clone();
            artist = hit.artist.clone();
            let key = hit_key(
                hit.key.as_deref().unwrap_or("C"),
                &hit.path,
                self.path.as_deref(),
                self.transpose,
            );
            meta = format!(
                "{:.0}-({})-[{}]",
                hit.tempo.unwrap_or(0.0),
                key,
                hit.bars.unwrap_or(0)
            );
            file_type = hit.format.to_uppercase();
            file_format = hit.format.clone();
            if hit.preview.trim().is_empty() {
                message = Some(tr("No lyrics in this song"));
            } else {
                preview = Some(hit.preview.clone());
            }
        } else if query.is_empty() {
            message = Some(tr("Type to find a song"));
        } else {
            message = Some(tr("No songs match that search"));
        }

        // Sit opposite the lyric lines: they are at the bottom by default, so
        // the popup takes the top; with the lines moved up it drops below them.
        let top_edge = self.central_rect.top() + inset;
        let bottom_edge =
            (self.central_rect.bottom() - inset - height).max(self.central_rect.top() + inset);
        // Sit opposite the lyric lines: they are at the bottom by default, so
        // the popup takes the top; with the lines moved up it drops below them.
        let top = match self.settings.text_visibility {
            TextVisibility::Upper => {
                if self.controller_open {
                    let controller = controller_panel_position(self.lyric_area, true);
                    (controller.y - 10.0 - height).max(top_edge)
                } else {
                    bottom_edge
                }
            }
            _ => {
                let top = top_edge;
                if self.controller_open {
                    (top + CONTROLLER_OFFSET).min(bottom_edge - height)
                } else {
                    top
                }
            }
        };
        let rect = Rect::from_min_size(
            Pos2::new(self.central_rect.left() + inset, top),
            egui::vec2((self.central_rect.width() - inset * 2.0).max(240.0), height),
        );
        self.lyric_overlay_rect = rect;
        if display.is_some() && self.song_file_icons.is_none() {
            self.song_file_icons = SongFileIcons::load(ctx);
        }

        egui::Area::new(egui::Id::unique("lyric_search_overlay"))
            .order(egui::Order::Middle)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                ui.set_width(rect.width());
                ui.set_height(rect.height());
                // Paint the bar and the white half before the frame: the frame's
                // content clip ends one `bottom_gap` above the border, so
                // painting the band inside the frame left a strip of the
                // background showing between the white band and the border.
                let radius = (6.0 * scale) as u8;
                let (bar, lower) = Self::search_popup_bands(rect, margin + header_height);
                let background = ui.painter().clone();
                background.rect_filled(
                    bar,
                    egui::CornerRadius {
                        nw: radius,
                        ne: radius,
                        sw: 0,
                        se: 0,
                    },
                    Color32::from_rgba_unmultiplied(0, 0, 0, bar_alpha),
                );
                background.rect_filled(
                    lower,
                    egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: radius,
                        se: radius,
                    },
                    Color32::from_rgba_unmultiplied(255, 255, 255, band_alpha),
                );
                egui::Frame::NONE
                    // Transparent and borderless: the background shapes blend
                    // with the lyrics behind them, and the border is stroked
                    // after the content so nothing can leave a gap above it.
                    .fill(Color32::TRANSPARENT)
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(egui::CornerRadius::same((6.0 * scale) as u8))
                    .inner_margin(egui::Margin::ZERO)
                    .show(ui, |ui| {
                        // The gaps below are exact: no automatic item spacing,
                        // so the boxes stay centred in the white half.
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                        let width = ui.available_width();

                        let (header, _) = ui.allocate_exact_size(
                            egui::vec2(width, margin + header_height),
                            egui::Sense::hover(),
                        );
                        // Black info bar across the whole panel width.
                        let header = Rect::from_min_size(
                            Pos2::new(header.left() + padding, header.top() + margin),
                            egui::vec2((header.width() - padding * 2.0).max(0.0), header_height),
                        );
                        // File-type badge in the top-right corner of the bar.
                        let icon_size = (header_height * 0.85).min(44.0 * scale);
                        let icon_center = Pos2::new(
                            rect.right() - 12.0 * scale - icon_size * 0.5,
                            header.center().y,
                        );
                        let icon = if file_format.is_empty() {
                            None
                        } else {
                            self.song_file_icons
                                .as_ref()
                                .map(|icons| icons.get(song_file_icon(&file_format)).clone())
                        };
                        let text_right = if icon.is_some() {
                            icon_center.x - icon_size * 0.5 - 10.0 * scale
                        } else {
                            header.right()
                        };
                        let text_rect = Rect::from_min_max(
                            header.min,
                            Pos2::new(text_right.max(header.left()), header.bottom()),
                        );
                        let painter = ui.painter_at(text_rect);
                        let header_font = FontId::proportional(26.0 * scale);
                        let mut x = text_rect.left();
                        draw_playlist_segment(
                            &painter,
                            &mut x,
                            header.center().y,
                            &code,
                            &header_font,
                            FG,
                            18.0 * scale,
                        );
                        draw_playlist_segment(
                            &painter,
                            &mut x,
                            header.center().y,
                            &title,
                            &header_font,
                            FG,
                            16.0 * scale,
                        );
                        draw_playlist_segment(
                            &painter,
                            &mut x,
                            header.center().y,
                            &meta,
                            &header_font,
                            FG,
                            16.0 * scale,
                        );
                        if !artist.is_empty() {
                            draw_playlist_segment(
                                &painter,
                                &mut x,
                                header.center().y,
                                &format!("-> {}", artist),
                                &header_font,
                                FG,
                                14.0 * scale,
                            );
                        }
                        if !file_type.is_empty() {
                            draw_playlist_segment(
                                &painter,
                                &mut x,
                                header.center().y,
                                &format!("({})", file_type),
                                &header_font,
                                FG,
                                0.0,
                            );
                        }
                        if let Some(texture) = icon {
                            let icon_rect = Rect::from_center_size(
                                icon_center,
                                egui::vec2(icon_size, icon_size),
                            );
                            ui.painter().image(
                                texture.id(),
                                icon_rect,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        ui.add_space(white_gap);

                        let (row, _) = ui.allocate_exact_size(
                            egui::vec2(width, box_height),
                            egui::Sense::hover(),
                        );
                        // Inset the boxes so they never touch the panel edge.
                        let row = Rect::from_min_max(
                            Pos2::new(row.left() + padding, row.top()),
                            Pos2::new(row.right() - padding, row.bottom()),
                        );
                        let input_width = (row.width() * 0.22)
                            .clamp(140.0 * scale, 260.0 * scale)
                            .min(row.width() * 0.4);
                        let input_rect =
                            Rect::from_min_size(row.min, egui::vec2(input_width, box_height));
                        let preview_rect = Rect::from_min_size(
                            Pos2::new(input_rect.right() + gap, row.top()),
                            egui::vec2(
                                (row.right() - (input_rect.right() + gap)).max(0.0),
                                box_height,
                            ),
                        );
                        let painter = ui.painter_at(row);
                        for rect in [input_rect, preview_rect] {
                            // The white band shows through: only the border is
                            // painted, so the boxes match the panel exactly.
                            painter.rect_filled(rect, 0.0, Color32::TRANSPARENT);
                            painter.rect_stroke(
                                rect,
                                0.0,
                                egui::Stroke::new(1.0 * scale, BG),
                                egui::StrokeKind::Inside,
                            );
                        }

                        let search_id = ui.make_persistent_id("lyric_search_input");
                        self.lyric_search_id = Some(search_id);
                        if self.lyric_search_focus {
                            // Start typing at the end of any seeded text, even
                            // if a stale cursor was persisted from last time.
                            ui.ctx().data_mut(|data| {
                                data.remove::<egui::widgets::text_edit::TextEditState>(search_id);
                            });
                            ui.memory_mut(|memory| memory.request_focus(search_id));
                            self.lyric_search_focus = false;
                        }
                        // Keep the insertion point at the end so the seeded
                        // text and anything typed afterwards stay in order.
                        let end = egui::text::CCursor::new(self.lyric_search.chars().count());
                        let mut state =
                            egui::widgets::text_edit::TextEditState::load(ui.ctx(), search_id)
                                .unwrap_or_default();
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(end)));
                        state.store(ui.ctx(), search_id);
                        let edit = egui::TextEdit::singleline(&mut self.lyric_search)
                            .id(search_id)
                            .frame(egui::Frame::NONE)
                            .text_color(BG)
                            .font(FontId::proportional(header_font.size))
                            .align(egui::Align2::LEFT_CENTER)
                            .cursor_at_end(true)
                            .margin(egui::Margin::symmetric(
                                (14.0 * scale).round() as i8,
                                (4.0 * scale).round() as i8,
                            ));
                        let edit_response = ui.put(input_rect.shrink(2.0 * scale), edit);
                        if edit_response.changed() {
                            self.lyric_overlay_until =
                                ui.ctx().input(|i| i.time) + self.settings.popup_seconds as f64;
                        }
                        if let Some(text) = &preview {
                            // Clip left and right only: clipping the top and
                            // bottom would cut Thai vowels above and below.
                            let clip = Rect::from_min_max(
                                Pos2::new(preview_rect.left() + 12.0 * scale, preview_rect.top()),
                                Pos2::new(
                                    preview_rect.right() - 12.0 * scale,
                                    preview_rect.bottom(),
                                ),
                            );
                            painter.with_clip_rect(clip).text(
                                Pos2::new(
                                    preview_rect.left() + 14.0 * scale,
                                    preview_rect.center().y,
                                ),
                                egui::Align2::LEFT_CENTER,
                                text,
                                FontId::proportional(14.0 * scale),
                                BG,
                            );
                        } else if let Some(text) = &message {
                            painter.with_clip_rect(preview_rect).text(
                                Pos2::new(
                                    preview_rect.left() + 14.0 * scale,
                                    preview_rect.center().y,
                                ),
                                egui::Align2::LEFT_CENTER,
                                text,
                                FontId::proportional(12.5 * scale),
                                Color32::from_gray(90),
                            );
                        }

                        // Leave only a thin strip under the boxes.
                        ui.add_space(bottom_gap);
                    });
                // Stroke last so the border sits over the background and the
                // content, with no unpainted strip above it.
                ui.painter().rect_stroke(
                    rect,
                    egui::CornerRadius::same(radius),
                    egui::Stroke::new(1.0 * scale, Color32::from_gray(40)),
                    egui::StrokeKind::Inside,
                );
            });

        self.lyric_search_rect = rect;
    }

    fn open_settings(&mut self) {
        self.open_settings_at(0);
    }

    fn open_settings_at(&mut self, section: usize) {
        self.settings_section = section;
        self.settings_just_opened = true;
        self.song_restore_confirm.store(false, Ordering::Relaxed);
        self.song_restore_confirmed.store(false, Ordering::Relaxed);
        self.settings_closing = false;
        self.settings_fade = 1.0;
        self.open_settings_window();
    }

    /// Open the "Add a song" window as its own OS viewport.
    fn open_import_window(&mut self) {
        {
            let mut w = self.import_window.lock().unwrap();
            w.open = true;
            w.intents.clear();
        }
        self.import_open = true;
        self.sync_import_window(true);
    }

    /// Push the importer state into the window.
    fn sync_import_window(&mut self, force: bool) {
        let mut w = self.import_window.lock().unwrap();
        if !w.open {
            return;
        }
        w.filter = self.import_filter;
        w.progress = self.import_progress;
        w.note = self.import_note.clone();
        w.error = self.import_error.clone();
        w.busy = self.import_scan.is_some() || self.import_add.is_some();
        if force || w.generation != self.import_gen {
            w.items = Arc::new(self.import_items.clone());
            w.view = Arc::new(self.import_view.clone());
            w.generation = self.import_gen;
        }
    }

    /// Keep the import window alive while it is open.
    fn draw_import_window(&mut self, ctx: &egui::Context) {
        let state = Arc::clone(&self.import_window);
        if !self.import_open && !state.lock().unwrap().open {
            return;
        }
        let title = tr("Add a song");
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("import_window"),
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([660.0, 320.0])
                .with_min_inner_size([660.0, 320.0])
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_always_on_top(),
            move |ctx, _class| {
                let mut w = state.lock().unwrap();
                if w.open != w.shown {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(w.open));
                    w.shown = w.open;
                }
                if !w.open {
                    return;
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(Color32::from_rgb(8, 8, 8)))
                    .show(ctx, |ui| {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(14, 12))
                            .show(ui, |ui| import_window_ui(ui, &mut w));
                    });
                if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || ctx.input(|i| i.viewport().close_requested())
                {
                    w.open = false;
                }
            },
        );
    }

    /// Run the commands the import window sent.
    fn poll_import_window(&mut self, ctx: &egui::Context) {
        self.poll_import();
        let (open, intents) = {
            let mut w = self.import_window.lock().unwrap();
            (w.open, std::mem::take(&mut w.intents))
        };
        if !open {
            if self.import_open {
                let id = egui::ViewportId::from_hash_of("import_window");
                ctx.request_repaint_of(id);
            }
            self.import_open = false;
            self.import_scan = None;
            self.import_add = None;
            return;
        }
        self.import_open = true;
        for intent in intents {
            match intent {
                ImportIntent::OpenFolder => self.start_dialog(DialogKind::ImportFolder),
                ImportIntent::Add => self.start_import_add(),
                ImportIntent::Selected(index, selected) => {
                    if let Some(item) = self.import_items.get_mut(index) {
                        item.selected = selected;
                        self.import_gen = self.import_gen.wrapping_add(1);
                    }
                }
                ImportIntent::SelectAll(selected) => {
                    for &index in &self.import_view {
                        if let Some(item) = self.import_items.get_mut(index) {
                            item.selected = selected;
                        }
                    }
                    self.import_gen = self.import_gen.wrapping_add(1);
                }
                ImportIntent::Filter(filter) => {
                    self.import_filter = filter;
                    self.rebuild_import_view();
                }
            }
        }
        if self.import_scan.is_some() || self.import_add.is_some() {
            ctx.request_repaint();
        }
        self.sync_import_window(false);
        ctx.request_repaint_of(egui::ViewportId::from_hash_of("import_window"));
    }

    /// Open the settings window as its own OS viewport.
    fn open_settings_window(&mut self) {
        {
            let mut p = self.settings_pages.lock().unwrap();
            p.open = true;
            p.section = self.settings_section;
            p.settings = self.settings.clone();
            p.sound_font_path = self.sound_font_path.clone();
            p.lyric_font_loaded = self.lyric_font_loaded;
            p.midi_ports = self.midi_ports.clone();
            p.midi_ports_loaded = self.midi_ports_loaded;
            p.midi_error = self.midi_page_error.clone();
            p.background_texture = self.background_texture.clone();
            p.default_background = self.default_background.clone();
            p.background_error = self.background_error.clone();
            p.about_logo = self.about_logo.clone();
            p.intents.clear();
        }
        self.settings_open = true;
    }

    /// Push the player's live state into the settings window.
    fn sync_settings_pages(&mut self) {
        let mut p = self.settings_pages.lock().unwrap();
        if !p.open {
            return;
        }
        p.midi_ports = self.midi_ports.clone();
        p.midi_ports_loaded = self.midi_ports_loaded;
        p.background_texture = self.background_texture.clone();
        p.default_background = self.default_background.clone();
        p.background_error = self.background_error.clone();
        p.about_logo = self.about_logo.clone();
        p.lyric_font_loaded = self.lyric_font_loaded;
        p.sound_font_path = self.sound_font_path.clone();
        p.midi_error = self.midi_page_error.clone();
        p.soundfont_error = self.soundfont_error.clone();
        // Fields the player changes outside the window (dialogs, routing) are
        // copied back so a later save from the window cannot undo them.
        p.settings.midi_out_port = self.settings.midi_out_port.clone();
        p.settings.split_ports = self.settings.split_ports.clone();
        p.settings.group_ports = self.settings.group_ports.clone();
        p.settings.group_volume = self.settings.group_volume.clone();
        p.settings.group_volume_on = self.settings.group_volume_on.clone();
        p.settings.group_velocity_on = self.settings.group_velocity_on.clone();
        p.settings.group_velocity_min = self.settings.group_velocity_min.clone();
        p.settings.group_velocity_max = self.settings.group_velocity_max.clone();
        p.settings.drum_ports = self.settings.drum_ports.clone();
        p.settings.drum_volume = self.settings.drum_volume.clone();
        p.settings.drum_volume_on = self.settings.drum_volume_on.clone();
        p.settings.drum_velocity_on = self.settings.drum_velocity_on.clone();
        p.settings.drum_velocity_min = self.settings.drum_velocity_min.clone();
        p.settings.drum_velocity_max = self.settings.drum_velocity_max.clone();
        p.settings.soundfont = self.settings.soundfont.clone();
        p.settings.lyric_font = self.settings.lyric_font.clone();
        p.settings.lyric_font_style = self.settings.lyric_font_style;
        p.settings.ncn_folder = self.settings.ncn_folder.clone();
        p.settings.kmid_folder = self.settings.kmid_folder.clone();
        p.settings.kar_folder = self.settings.kar_folder.clone();
        p.settings.mkc_folder = self.settings.mkc_folder.clone();
    }

    /// Keep the settings window alive while it is open.
    fn draw_settings_window(&mut self, ctx: &egui::Context) {
        let state = Arc::clone(&self.settings_pages);
        if !self.settings_open && !state.lock().unwrap().open {
            return;
        }
        let title = tr("Settings");
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("settings_window"),
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([760.0, 470.0])
                .with_min_inner_size([760.0, 470.0])
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_always_on_top(),
            move |ctx, _class| {
                let mut p = state.lock().unwrap();
                if p.open != p.shown {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(p.open));
                    p.shown = p.open;
                }
                if !p.open {
                    return;
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(Color32::from_rgb(8, 8, 8)))
                    .show(ctx, |ui| p.ui(ui));
                if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || ctx.input(|i| i.viewport().close_requested())
                {
                    p.open = false;
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            },
        );
    }

    /// Show the About license/credits popup in its own OS window.
    fn draw_about_popup_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let popup = {
            let pages = self.settings_pages.lock().unwrap();
            (pages.section == 7).then_some(pages.about_popup).flatten()
        };
        let Some(popup) = popup else {
            // A page change or a closed Settings window hides the popup.
            self.settings_pages.lock().unwrap().about_popup = None;
            return;
        };
        let (id, title, size) = match popup {
            AboutPopup::License => (
                egui::ViewportId::from_hash_of("about_license_window"),
                tr("License"),
                [720.0, 490.0],
            ),
            AboutPopup::Created => (
                egui::ViewportId::from_hash_of("about_created_window"),
                tr("Created"),
                [700.0, 480.0],
            ),
        };
        let state = Arc::clone(&self.settings_pages);
        ctx.show_viewport_deferred(
            id,
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size(size)
                .with_min_inner_size(size)
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_always_on_top(),
            move |ctx, _class| {
                let mut pages = state.lock().unwrap();
                if pages.about_popup != Some(popup) {
                    return;
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || ctx.input(|i| i.viewport().close_requested())
                {
                    pages.about_popup = None;
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    return;
                }
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(8, 8, 8))
                            .inner_margin(egui::Margin::same(16)),
                    )
                    .show(ctx, |ui| match popup {
                        AboutPopup::License => draw_license(ui),
                        AboutPopup::Created => draw_created(ui),
                    });
            },
        );
    }

    /// Run the commands the settings window sent and keep it in sync.
    fn poll_settings_window(&mut self, ctx: &egui::Context) {
        let (open, intents, settings, section) = {
            let mut p = self.settings_pages.lock().unwrap();
            (
                p.open,
                std::mem::take(&mut p.intents),
                p.settings.clone(),
                p.section,
            )
        };
        if !open {
            if self.settings_open {
                let id = egui::ViewportId::from_hash_of("settings_window");
                ctx.request_repaint_of(id);
                // Flush anything still waiting for the debounce timer.
                self.settings_save_at = None;
                self.save_settings();
            }
            if self.song_restore_confirm.swap(false, Ordering::Relaxed) {
                self.song_restore_confirmed.store(false, Ordering::Relaxed);
                ctx.send_viewport_cmd_to(
                    song_restore_confirm_viewport(),
                    egui::ViewportCommand::Visible(false),
                );
            }
            self.settings_pages.lock().unwrap().about_popup = None;
            self.settings_open = false;
            return;
        }
        self.settings_open = true;
        self.settings_section = section;

        // Apply the edits first: the send intents below read `self.settings`,
        // and the page pushes them before the shell pushes Save.
        let save = intents
            .iter()
            .any(|intent| matches!(intent, SettingsIntent::Save));
        if save {
            if let Some(section) = intents.iter().find_map(|intent| match intent {
                SettingsIntent::ResetPage(section) => Some(*section),
                _ => None,
            }) {
                self.reset_settings_page(section, ctx);
            }
            // Keep the fields the player owns (dialogs, routing) instead of
            // overwriting them with the window's older copy.
            let language_changed = settings.language != self.settings.language;
            let mut merged = settings;
            merged.midi_out_port = self.settings.midi_out_port.clone();
            merged.split_ports = self.settings.split_ports.clone();
            merged.group_ports = self.settings.group_ports.clone();
            merged.group_volume = self.settings.group_volume.clone();
            merged.group_volume_on = self.settings.group_volume_on.clone();
            merged.group_velocity_on = self.settings.group_velocity_on.clone();
            merged.group_velocity_min = self.settings.group_velocity_min.clone();
            merged.group_velocity_max = self.settings.group_velocity_max.clone();
            merged.drum_ports = self.settings.drum_ports.clone();
            merged.drum_volume = self.settings.drum_volume.clone();
            merged.drum_volume_on = self.settings.drum_volume_on.clone();
            merged.drum_velocity_on = self.settings.drum_velocity_on.clone();
            merged.drum_velocity_min = self.settings.drum_velocity_min.clone();
            merged.drum_velocity_max = self.settings.drum_velocity_max.clone();
            merged.soundfont = self.settings.soundfont.clone();
            merged.lyric_font = self.settings.lyric_font.clone();
            merged.lyric_font_style = self.settings.lyric_font_style;
            merged.ncn_folder = self.settings.ncn_folder.clone();
            merged.kmid_folder = self.settings.kmid_folder.clone();
            merged.kar_folder = self.settings.kar_folder.clone();
            merged.mkc_folder = self.settings.mkc_folder.clone();
            self.settings = merged;
            if language_changed {
                i18n::set_language(&self.settings.language);
            }
            self.settings_pages.lock().unwrap().settings = self.settings.clone();
            // Debounce the write: sliders change settings many times a second.
            let now = ctx.input(|i| i.time);
            self.settings_save_at = Some(now + 0.5);
        }

        for intent in intents {
            match intent {
                SettingsIntent::Save | SettingsIntent::ResetPage(_) => {}
                SettingsIntent::ConfirmSongReset => {
                    self.song_restore_confirm.store(true, Ordering::Relaxed);
                    ctx.request_repaint_of(song_restore_confirm_viewport());
                }
                SettingsIntent::PickSoundFont => self.pick_sound_font(),
                SettingsIntent::PickLyricFont => {
                    self.lyric_font_browse = false;
                    self.pick_lyric_font();
                }
                SettingsIntent::PickBackgroundImage => {
                    self.start_dialog(DialogKind::BackgroundImage);
                }
                SettingsIntent::PickFolder(kind) => self.start_dialog(kind),
                SettingsIntent::OpenLibrary => self.open_library(),
                SettingsIntent::OpenSplitWindow => self.open_split_window(),
                SettingsIntent::SetMidiOut(port) => self.apply_midi_out(port),
                SettingsIntent::ResetMidi => self.send_midi_reset(),
                SettingsIntent::SendMidiOptions => self.send_midi_options(),
                SettingsIntent::SendEq => self.send_eq(),
                SettingsIntent::SendEffects => self.send_effects(),
                SettingsIntent::RefreshMidiPorts => self.refresh_midi_ports(),
                SettingsIntent::RebuildBuffer => {
                    let now = ctx.input(|i| i.time);
                    self.settings_save_at = Some(now + 0.6);
                    self.buffer_rebuild_at = Some(now + 0.7);
                }
            }
        }
        self.sync_settings_pages();
        // The child viewport only repaints on input, so ask for a frame after
        // the player changed what the window shows.
        ctx.request_repaint_of(egui::ViewportId::from_hash_of("settings_window"));
    }

    fn pick_sound_font(&mut self) {
        self.start_dialog(DialogKind::SoundFont);
    }

    /// Choose an external font for the karaoke lyrics (the UI never changes).
    fn pick_lyric_font(&mut self) {
        self.start_dialog(DialogKind::LyricFont);
    }

    // ---------------------------------------------------------------- settings

    fn open_split_window(&mut self) {
        self.refresh_midi_ports();
        let default_label = if self.settings.midi_out_port.is_empty() {
            tr("RustySynth SoundFont")
        } else {
            self.settings.midi_out_port.clone()
        };
        {
            let mut w = self.split_window.lock().unwrap();
            w.open = true;
            w.ports = self.midi_ports.clone();
            w.default_label = default_label;
            w.channels = self.split_port_names().to_vec();
            w.channel_selected = None;
            w.channel_texture = None;
            w.group_volume_on = self.settings.group_volume_on.clone();
            w.group_volume_on.resize(audio::GROUPS, false);
            w.group_velocity_on = self.settings.group_velocity_on.clone();
            w.group_velocity_on.resize(audio::GROUPS, false);
            w.group_ports = self.settings.group_ports.clone();
            w.group_ports.resize(20, String::new());
            w.group_volume = self.settings.group_volume.clone();
            w.group_volume.resize(20, 100);
            w.group_velocity_min = self.settings.group_velocity_min.clone();
            w.group_velocity_min.resize(20, 0);
            w.group_velocity_max = self.settings.group_velocity_max.clone();
            w.group_velocity_max.resize(20, 127);
            w.group_selected.get_or_insert(0);
            w.group_selection = vec![w.group_selected.unwrap_or(0)];
            w.group_drag_anchor = None;
            w.group_texture = None;
            w.drum_ports = self.settings.drum_ports.clone();
            w.drum_ports.resize(audio::DRUM_GROUPS, String::new());
            w.drum_volume = self.settings.drum_volume.clone();
            w.drum_volume.resize(audio::DRUM_GROUPS, 100);
            w.drum_velocity_min = self.settings.drum_velocity_min.clone();
            w.drum_velocity_min.resize(audio::DRUM_GROUPS, 0);
            w.drum_velocity_max = self.settings.drum_velocity_max.clone();
            w.drum_velocity_max.resize(audio::DRUM_GROUPS, 127);
            w.drum_volume_on = self.settings.drum_volume_on.clone();
            w.drum_volume_on.resize(audio::DRUM_GROUPS, false);
            w.drum_velocity_on = self.settings.drum_velocity_on.clone();
            w.drum_velocity_on.resize(audio::DRUM_GROUPS, false);
            w.drum_selected.get_or_insert(0);
            w.drum_selection = vec![w.drum_selected.unwrap_or(0)];
            w.drum_drag_anchor = None;
            w.drum_texture = None;
            w.selection.clear();
            w.drag_anchor = None;
            w.error = None;
            w.changed = false;
        }
        self.split_open = true;
    }

    /// Keep the Split MIDI window alive while it is open.
    fn draw_split_window(&mut self, ctx: &egui::Context) {
        let state = Arc::clone(&self.split_window);
        // The viewport stays registered for the session: closing hides the OS
        // window so it can be shown again without recreating it.
        if !self.split_open && !state.lock().unwrap().open {
            return;
        }
        let title = tr("Split MIDI");
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("split_midi_window"),
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([700.0, 660.0])
                .with_min_inner_size([700.0, 660.0])
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_always_on_top(),
            move |ctx, _class| {
                let mut w = state.lock().unwrap();
                if w.open != w.shown {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(w.open));
                    w.shown = w.open;
                }
                if !w.open {
                    return;
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(Color32::from_gray(14)))
                    .show(ctx, |ui| {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::same(16))
                            .show(ui, |ui| split_window_ui(ui, &mut w));
                    });
                if w.changed {
                    // The main viewport owns the player and the settings file,
                    // so wake it when this window edits something.
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || ctx.input(|i| i.viewport().close_requested())
                {
                    w.open = false;
                }
            },
        );
    }

    /// Apply the routing the window edited and follow its open state.
    fn poll_split_window(&mut self, ctx: &egui::Context) {
        let (
            open,
            changed,
            channels,
            groups,
            volume,
            velocity_min,
            velocity_max,
            drums,
            drum_volume,
            drum_velocity_min,
            drum_velocity_max,
            group_volume_on,
            group_velocity_on,
            drum_volume_on,
            drum_velocity_on,
        ) = {
            let w = self.split_window.lock().unwrap();
            (
                w.open,
                w.changed,
                w.channels.clone(),
                w.group_ports.clone(),
                w.group_volume.clone(),
                w.group_velocity_min.clone(),
                w.group_velocity_max.clone(),
                w.drum_ports.clone(),
                w.drum_volume.clone(),
                w.drum_velocity_min.clone(),
                w.drum_velocity_max.clone(),
                w.group_volume_on.clone(),
                w.group_velocity_on.clone(),
                w.drum_volume_on.clone(),
                w.drum_velocity_on.clone(),
            )
        };
        if !open {
            if self.split_open {
                // Let the child viewport repaint once so it can hide itself.
                let id = egui::ViewportId::from_hash_of("split_midi_window");
                ctx.request_repaint_of(id);
            }
            self.split_open = false;
            return;
        }
        self.split_open = true;
        if !changed {
            return;
        }
        self.settings.split_ports = channels;
        self.settings.group_ports = groups;
        self.settings.group_volume = volume;
        self.settings.group_velocity_min = velocity_min;
        self.settings.group_velocity_max = velocity_max;
        self.settings.drum_ports = drums;
        self.settings.drum_volume = drum_volume;
        self.settings.drum_velocity_min = drum_velocity_min;
        self.settings.drum_velocity_max = drum_velocity_max;
        self.settings.group_volume_on = group_volume_on;
        self.settings.group_velocity_on = group_velocity_on;
        self.settings.drum_volume_on = drum_volume_on;
        self.settings.drum_velocity_on = drum_velocity_on;
        self.send_midi_options();
        let result = self.apply_midi_routing();
        self.save_settings();
        let mut w = self.split_window.lock().unwrap();
        w.changed = false;
        w.error = result.err();
    }

    /// The editor works on the Standard MIDI File behind any loaded song
    /// (including the `.mid` of an NCN trio) while it is playing or paused;
    /// stopping or reaching the end closes it.
    fn midi_editor_available(&self) -> bool {
        if self.song.is_none() || self.playback.is_none() || self.path.is_none() {
            return false;
        }
        if self.song_end_us > 0 && self.position_us() >= self.song_end_us {
            return false;
        }
        true
    }

    /// Read the TrackName meta events straight from the file.
    fn midi_editor_track_names(path: &Path, track_count: usize) -> Vec<Option<String>> {
        let mut names = vec![None; track_count];
        let bytes = if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mkc"))
        {
            let Ok(package) = karaoke_player::mkc::read_package(path) else {
                return names;
            };
            match package.midi() {
                Ok(midi) => midi.to_vec(),
                Err(_) => return names,
            }
        } else {
            let Ok(bytes) = std::fs::read(path) else {
                return names;
            };
            bytes
        };
        let Ok(doc) = karaoke_player::midi::parse(&bytes, EncodingChoice::Auto) else {
            return names;
        };
        let mut seen = vec![false; track_count];
        for event in &doc.text_events {
            if event.kind != karaoke_player::MetaTextKind::TrackName {
                continue;
            }
            let track = event.track as usize;
            if track >= names.len() || seen[track] {
                continue;
            }
            let text = decode(&event.raw, EncodingChoice::Auto);
            let text = text.trim();
            if !text.is_empty() {
                names[track] = Some(text.to_owned());
                seen[track] = true;
            }
        }
        names
    }

    /// One row per channel used by each non-empty SMF track. A format-0 file
    /// keeps all of its instruments in one track, so splitting by channel is
    /// what makes those files editable. Tracks without any MIDI channel (the
    /// tempo/meta tracks) are skipped, matching the table design.
    fn midi_editor_rows(song: &Song, names: &[Option<String>]) -> Vec<MidiEditorRow> {
        let (programs, banks) = file_instruments(song);
        let mut rows = Vec::new();
        for track in 0..song.track_count {
            let mut channels: Vec<u8> = Vec::new();
            for event in song
                .events
                .iter()
                .filter(|event| event.track as usize == track)
            {
                let Some(channel) = midi_event_channel(&event.kind) else {
                    continue;
                };
                if !channels.contains(&channel) {
                    channels.push(channel);
                }
            }
            if channels.is_empty() {
                continue;
            }
            // Drums first, then the channels in ascending order. This keeps the
            // row order stable after a save (the writer adds tick-0 events).
            channels.sort_by_key(|channel| (if *channel == 9 { 0 } else { 1 }, *channel));
            let multi_channel = channels.len() > 1;
            for channel in channels {
                let number = rows.len() + 1;
                let real_name = if multi_channel {
                    None
                } else {
                    names.get(track).cloned().flatten()
                };
                let name = real_name
                    .unwrap_or_else(|| tr_args("Track {}", &[&number as &dyn std::fmt::Display]));
                let index = channel as usize;
                rows.push(MidiEditorRow {
                    track,
                    name,
                    channel: Some(channel),
                    program: programs.get(index).copied().flatten().unwrap_or(0),
                    bank: banks.get(index).copied().flatten().unwrap_or(0),
                    initial: MidiEditorValues::default(),
                });
            }
        }
        rows
    }

    fn open_midi_editor(&mut self) -> bool {
        if !self.midi_editor_available() {
            return false;
        }
        let Some(song) = &self.song else {
            return false;
        };
        let Some(playback) = &self.playback else {
            return false;
        };
        let Some(path) = self.path.clone() else {
            return false;
        };

        let names = Self::midi_editor_track_names(&path, song.track_count);
        let mut rows = Self::midi_editor_rows(song, &names);
        let snapshot = playback.monitor.snapshot();
        for row in &mut rows {
            if let Some(channel) = row.channel {
                let mut values = editor_values(&snapshot, channel);
                values.program = row.program;
                values.bank = row.bank;
                row.initial = values;
            }
        }
        let base_names = rows.iter().map(|row| row.name.clone()).collect();

        {
            let mut w = self.midi_editor_window.lock().unwrap();
            w.open = true;
            w.path = path;
            w.rows = rows;
            w.base_names = base_names;
            w.name_edits.clear();
            w.edits.clear();
            w.popup = None;
            w.save_confirm = false;
            w.save_requested = false;
            w.save_as = None;
            w.save_as_open_requested = false;
            w.save_as_shown = false;
            w.save_as_fonts_ready = false;
            w.save_as_confirm = false;
            w.save_as_pending = None;
            w.save_as_requested = None;
            w.status = None;
            w.status_error = false;
            w.status_until = 0.0;
            w.monitor = Some(Arc::clone(&playback.monitor));
            w.bpm = song.bpm.max(1.0);
            w.bpm_base = song.bpm.max(1.0);
            w.key_name = if matches!(
                song.format,
                karaoke_player::SourceFormat::Ncn
                    | karaoke_player::SourceFormat::Kmid
                    | karaoke_player::SourceFormat::Mkc
            ) {
                song.key.clone().filter(|key| !key.trim().is_empty())
            } else {
                None
            };
            w.key = 0;
            w.key_base = 0;
            w.key_min = -12;
            w.key_max = 12;
            w.popup_shown = false;
            w.confirm_shown = false;
            w.fonts_ready = false;
            w.popup_fonts_ready = false;
            w.confirm_fonts_ready = false;
        }
        self.midi_editor_open = true;
        true
    }

    /// True when the song belongs to the configured song library (or is
    /// already in the database). Files opened from elsewhere stay out of the
    /// database when they are saved.
    fn midi_song_is_in_library(&self, path: &Path, format: karaoke_player::SourceFormat) -> bool {
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if self.library_meta.contains_key(&absolute)
            || self.library_songs.iter().any(|row| row.path == absolute)
        {
            return true;
        }
        let configured = match format {
            karaoke_player::SourceFormat::Ncn => self.settings.ncn_folder.as_deref(),
            karaoke_player::SourceFormat::Kmid => self.settings.kmid_folder.as_deref(),
            karaoke_player::SourceFormat::Kar | karaoke_player::SourceFormat::Midi => {
                self.settings.kar_folder.as_deref()
            }
            karaoke_player::SourceFormat::Mkc => self.settings.mkc_folder.as_deref(),
        };
        configured.is_some_and(|root| absolute.starts_with(root))
    }

    /// Folder that Save As should write into for this format.
    fn midi_save_as_folder(&self, source: &Path, format: karaoke_player::SourceFormat) -> PathBuf {
        let configured = match format {
            karaoke_player::SourceFormat::Ncn => self.settings.ncn_folder.as_deref(),
            karaoke_player::SourceFormat::Kmid => self.settings.kmid_folder.as_deref(),
            karaoke_player::SourceFormat::Kar | karaoke_player::SourceFormat::Midi => {
                self.settings.kar_folder.as_deref()
            }
            karaoke_player::SourceFormat::Mkc => self.settings.mkc_folder.as_deref(),
        };
        let parent = source.parent().unwrap_or_else(|| Path::new("."));
        if let Some(root) = configured {
            if let Ok(relative) = parent.strip_prefix(root) {
                root.join(relative)
            } else {
                // Files opened from outside the configured song folders stay
                // beside their current file.
                parent.to_path_buf()
            }
        } else {
            parent.to_path_buf()
        }
    }

    /// Fill the Save As form from the loaded song and the editor state.
    fn open_midi_save_as(&mut self) {
        let Some(song) = &self.song else {
            return;
        };
        let Some(path) = &self.path else {
            return;
        };
        let folder = self.midi_save_as_folder(path, song.format);
        let outside_library = !self.midi_song_is_in_library(path, song.format);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "song.mid".to_owned());
        let title = song.title.clone().unwrap_or_else(|| file_stem(path));
        let artist = song.artist.clone().unwrap_or_default();
        let key = {
            let w = self.midi_editor_window.lock().unwrap();
            if matches!(
                song.format,
                karaoke_player::SourceFormat::Ncn
                    | karaoke_player::SourceFormat::Kmid
                    | karaoke_player::SourceFormat::Mkc
            ) {
                let base = w
                    .key_name
                    .clone()
                    .or_else(|| song.key.clone())
                    .unwrap_or_else(|| "C".to_owned());
                sounding_key(&base, w.key_base + w.key)
            } else {
                song.key.clone().unwrap_or_else(|| "C".to_owned())
            }
        };
        let mut w = self.midi_editor_window.lock().unwrap();
        w.save_as = Some(MidiSaveAsForm {
            source_name: file_name.clone(),
            outside_library,
            confirm_overwrite: false,
            file_name,
            title,
            artist,
            key,
            folder,
            error: None,
        });
        w.save_as_confirm = false;
        w.save_as_pending = None;
    }

    /// Keep the editor viewport alive and apply the values it changes.
    fn draw_midi_editor_window(&mut self, ctx: &egui::Context) {
        let state = Arc::clone(&self.midi_editor_window);
        if !self.midi_editor_open && !state.lock().unwrap().open {
            return;
        }
        let title = tr("MIDI Editor");
        ctx.show_viewport_deferred(
            midi_editor_viewport(),
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([800.0, 300.0])
                .with_min_inner_size([800.0, 300.0])
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_always_on_top(),
            move |ctx, _class| {
                let mut w = state.lock().unwrap();
                if w.open != w.shown {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(w.open));
                    w.shown = w.open;
                }
                if !w.open {
                    return;
                }
                if !w.fonts_ready {
                    install_fonts(ctx, None);
                    install_style(ctx);
                    w.fonts_ready = true;
                }
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(8, 8, 8))
                            .inner_margin(egui::Margin::same(10)),
                    )
                    .show(ctx, |ui| midi_editor_ui(ui, &mut w));
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
                if ctx.input(|i| i.viewport().close_requested()) {
                    w.open = false;
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            },
        );
    }

    /// The per-track editor is its own small OS dialog so it never clips
    /// inside the table window.
    fn draw_midi_editor_popup_window(&mut self, ctx: &egui::Context) {
        let state = Arc::clone(&self.midi_editor_window);
        let (popup_open, shown) = {
            let w = state.lock().unwrap();
            (w.open && w.popup.is_some(), w.popup_shown)
        };
        if !popup_open && !shown {
            return;
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title(tr("Midi Track Editor"))
            .with_inner_size([430.0, 400.0])
            .with_min_inner_size([430.0, 400.0])
            .with_resizable(false)
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_always_on_top();
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            builder = builder.with_position([rect.center().x - 215.0, rect.center().y - 200.0]);
        }
        ctx.show_viewport_deferred(midi_editor_popup_viewport(), builder, move |ctx, _class| {
            let mut w = state.lock().unwrap();
            let visible = w.open && w.popup.is_some();
            if visible != w.popup_shown {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
                w.popup_shown = visible;
            }
            if !visible {
                return;
            }
            if !w.popup_fonts_ready {
                install_fonts(ctx, None);
                install_style(ctx);
                w.popup_fonts_ready = true;
            }
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(10, 10, 10))
                        .inner_margin(egui::Margin::same(0)),
                )
                .show(ctx, |ui| midi_editor_popup_content(ui, &mut w));
            if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                || ctx.input(|i| i.viewport().close_requested())
            {
                w.popup = None;
                w.popup_shown = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                ctx.request_repaint_of(egui::ViewportId::ROOT);
                ctx.request_repaint_of(midi_editor_viewport());
            }
        });
    }

    /// The overwrite question is a small OS dialog of its own, not a window
    /// drawn inside the table.
    fn draw_midi_editor_confirm_window(&mut self, ctx: &egui::Context) {
        let state = Arc::clone(&self.midi_editor_window);
        let (wanted, shown, save_as) = {
            let w = state.lock().unwrap();
            (
                w.open && (w.save_confirm || w.save_as_confirm),
                w.confirm_shown,
                w.save_as_confirm,
            )
        };
        if !wanted && !shown {
            return;
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title(if save_as {
                tr("Save As")
            } else {
                tr("Save MIDI")
            })
            .with_inner_size([360.0, 130.0])
            .with_min_inner_size([360.0, 130.0])
            .with_resizable(false)
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_always_on_top();
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            builder = builder.with_position([rect.center().x - 180.0, rect.center().y - 65.0]);
        }
        ctx.show_viewport_deferred(
            midi_editor_confirm_viewport(),
            builder,
            move |ctx, _class| {
                let mut w = state.lock().unwrap();
                let visible = w.open && (w.save_confirm || w.save_as_confirm);
                if visible != w.confirm_shown {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
                    w.confirm_shown = visible;
                }
                if !visible {
                    return;
                }
                if !w.confirm_fonts_ready {
                    install_fonts(ctx, None);
                    install_style(ctx);
                    w.confirm_fonts_ready = true;
                }
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(10, 10, 10))
                            .inner_margin(egui::Margin::same(16)),
                    )
                    .show(ctx, |ui| midi_editor_confirm_content(ui, &mut w));
                if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || ctx.input(|i| i.viewport().close_requested())
                {
                    if w.save_as_confirm {
                        w.save_as_confirm = false;
                        w.save_as_pending = None;
                    } else {
                        w.save_confirm = false;
                    }
                    w.confirm_shown = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.request_repaint_of(midi_editor_viewport());
                    ctx.request_repaint_of(midi_editor_save_as_viewport());
                }
            },
        );
    }

    /// The Save As fields live in their own OS dialog.
    fn draw_midi_editor_save_as_window(&mut self, ctx: &egui::Context) {
        let state = Arc::clone(&self.midi_editor_window);
        let (wanted, shown) = {
            let w = state.lock().unwrap();
            (w.open && w.save_as.is_some(), w.save_as_shown)
        };
        if !wanted && !shown {
            return;
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title(tr("Save As"))
            .with_inner_size([440.0, 404.0])
            .with_min_inner_size([440.0, 404.0])
            .with_resizable(false)
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_always_on_top();
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            builder = builder.with_position([rect.center().x - 220.0, rect.center().y - 202.0]);
        }
        ctx.show_viewport_deferred(
            midi_editor_save_as_viewport(),
            builder,
            move |ctx, _class| {
                let mut w = state.lock().unwrap();
                let visible = w.open && w.save_as.is_some();
                if visible != w.save_as_shown {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
                    w.save_as_shown = visible;
                }
                if !visible {
                    return;
                }
                if !w.save_as_fonts_ready {
                    install_fonts(ctx, None);
                    install_style(ctx);
                    w.save_as_fonts_ready = true;
                }
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(10, 10, 10))
                            .inner_margin(egui::Margin::same(16)),
                    )
                    .show(ctx, |ui| midi_editor_save_as_content(ui, &mut w));
                if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || ctx.input(|i| i.viewport().close_requested())
                {
                    w.save_as = None;
                    w.save_as_confirm = false;
                    w.save_as_pending = None;
                    w.save_as_shown = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.request_repaint_of(midi_editor_viewport());
                    ctx.request_repaint_of(midi_editor_confirm_viewport());
                }
            },
        );
    }

    /// Write the editor's current values and metadata to the Save As target.
    fn commit_midi_save_as(
        &mut self,
        form: &MidiSaveAsForm,
    ) -> std::result::Result<String, String> {
        let Some(source) = self.path.clone() else {
            return Err(tr("No song is loaded").to_owned());
        };
        let Some(song) = self.song.as_ref() else {
            return Err(tr("No song is loaded").to_owned());
        };
        let format = song.format;
        let target = form.folder.join(&form.file_name);
        let (smf_edits, bpm, bpm_base, key, key_base, edits) = {
            let w = self.midi_editor_window.lock().unwrap();
            let mut channels = Vec::new();
            for (channel, values) in &w.edits {
                let track = w
                    .rows
                    .iter()
                    .find(|row| row.channel == Some(*channel))
                    .map(|row| row.track)
                    .unwrap_or(0);
                channels.push(karaoke_player::midi_write::ChannelMix {
                    channel: *channel,
                    track,
                    bank: values.bank,
                    program: values.program,
                    volume: values.volume,
                    pan: values.pan,
                    reverb: values.reverb,
                    chorus: values.chorus,
                });
            }
            let tempo_scale = if w.bpm_base > 1.0 && w.bpm > 1.0 {
                w.bpm_base / w.bpm
            } else {
                1.0
            };
            let smf = karaoke_player::midi_write::SmfEdits {
                channels,
                track_names: w.name_edits.clone(),
                tempo_scale,
                transpose: w.key,
            };
            (smf, w.bpm, w.bpm_base, w.key, w.key_base, w.edits.clone())
        };

        if format == karaoke_player::SourceFormat::Mkc {
            let package =
                karaoke_player::mkc::read_package(&source).map_err(|err| format!("{err:#}"))?;
            let edited = karaoke_player::midi_write::edit_bytes(
                package.midi().map_err(|err| format!("{err:#}"))?,
                &smf_edits,
            )
            .map_err(|err| format!("{err:#}"))?;
            let time_scale = if bpm_base > 1.0 && bpm > 1.0 {
                bpm_base / bpm
            } else {
                1.0
            };
            let edit = karaoke_player::mkc::MkcEdit {
                code: target
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned()),
                title: Some(form.title.clone()),
                artist: Some(form.artist.clone()),
                key: Some(form.key.clone()),
                tempo: Some(bpm),
                time_scale,
            };
            karaoke_player::mkc::rewrite_midi(&source, &target, &edited, Some(&edit), true)
                .map_err(|err| format!("{err:#}"))?;
        } else {
            karaoke_player::midi_write::edit_file_to(&source, &target, &smf_edits)
                .map_err(|err| format!("{err:#}"))?;
        }

        if matches!(format, karaoke_player::SourceFormat::Ncn) {
            let (lyr, cur) = karaoke_player::ncn::locate(&source, &LoadOptions::default())
                .ok_or_else(|| tr("The NCN lyric/cursor files are missing").to_owned())?;
            let overwrite = target == source;
            let (target_lyr, target_cur) = if overwrite {
                (lyr.clone(), cur.clone())
            } else {
                let stem = target
                    .file_stem()
                    .map(|value| value.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let root = self.settings.ncn_folder.as_deref();
                let map = |path: &Path, extension: &str| {
                    if let Some(root) = root
                        && let Ok(relative) = path.strip_prefix(root)
                    {
                        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
                        root.join(parent).join(format!("{stem}.{extension}"))
                    } else {
                        target
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join(format!("{stem}.{extension}"))
                    }
                };
                (map(&lyr, "lyr"), map(&cur, "cur"))
            };
            for (from, to) in [(&cur, &target_cur), (&lyr, &target_lyr)] {
                if from != to {
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent).map_err(|err| format!("{err:#}"))?;
                    }
                    std::fs::copy(from, to).map_err(|err| format!("{err:#}"))?;
                }
            }
            karaoke_player::ncn::rewrite_lyr_metadata(
                &target_lyr,
                &form.title,
                &form.artist,
                &form.key,
            )
            .map_err(|err| format!("{err:#}"))?;
        }

        if self.midi_song_is_in_library(&source, format) {
            self.register_saved_file(
                &target,
                &form.title,
                &form.artist,
                &form.key,
                target != source,
            )?;
        }

        if target == source {
            self.finish_save_as_overwrite(bpm, bpm_base, key, key_base, &edits, &form.key);
        }
        Ok(form.file_name.clone())
    }

    /// Insert or update the target song's library row and playlist entry.
    fn register_saved_file(
        &mut self,
        path: &Path,
        title: &str,
        artist: &str,
        key: &str,
        add_playlist: bool,
    ) -> std::result::Result<(), String> {
        let song = load(path, &LoadOptions::default()).map_err(|err| format!("{err:#}"))?;
        let mut record = library::record_from_song(path, &song, "");
        let title = title.trim();
        if !title.is_empty() {
            record.title = title.to_owned();
        } else if record.title.trim().is_empty() {
            record.title = record.code.clone();
        }
        record.artist = artist.trim().to_owned();
        let key = key.trim();
        if !key.is_empty() {
            record.key = Some(key.to_owned());
        }
        let mut library = Library::open_default().map_err(|err| format!("{err:#}"))?;
        library
            .insert_records(std::slice::from_ref(&record))
            .map_err(|err| format!("{err:#}"))?;

        if let Some(existing) = self.library_songs.iter_mut().find(|row| {
            row.path == record.path || (row.code == record.code && row.format == record.format)
        }) {
            *existing = record.clone();
        } else {
            self.library_songs.push(record.clone());
        }
        self.library_songs_gen = self.library_songs_gen.wrapping_add(1);
        self.rebuild_library_meta();
        self.rebuild_library_view();
        self.titles.insert(
            record.path.clone(),
            SongInfo {
                code: record.code.clone(),
                title: record.title.clone(),
                artist: record.artist.clone(),
                file_type: record.format.clone(),
                bpm: record.tempo.unwrap_or(0.0),
                key: record.key.clone().unwrap_or_else(|| "C".to_owned()),
                bars: record.bars.unwrap_or(0),
                loaded: true,
            },
        );
        if add_playlist && !self.playlist.iter().any(|entry| entry == &record.path) {
            self.playlist.push(record.path.clone());
            self.save_settings();
        }
        Ok(())
    }

    /// Follow an overwritten file with the live playback and reset the
    /// editor's save baselines.
    fn finish_save_as_overwrite(
        &mut self,
        bpm: f64,
        bpm_base: f64,
        key: i32,
        key_base: i32,
        edits: &[(u8, MidiEditorValues)],
        metadata_key: &str,
    ) {
        let bpm_changed = (bpm - bpm_base).abs() > 0.001;
        let key_changed = key != 0;
        let live_key = (key_base + key).clamp(-12, 12);
        {
            let mut w = self.midi_editor_window.lock().unwrap();
            w.bpm_base = bpm.max(1.0);
            w.key_base = 0;
            w.key = 0;
            w.key_min = -12;
            w.key_max = 12;
            let meta = metadata_key.trim();
            w.key_name = if meta.is_empty() {
                None
            } else {
                Some(meta.to_owned())
            };
            for (channel, values) in edits {
                for row in w.rows.iter_mut() {
                    if row.channel == Some(*channel) {
                        row.program = values.program;
                        row.bank = values.bank;
                    }
                }
            }
            w.name_edits.clear();
            w.base_names = w.rows.iter().map(|row| row.name.clone()).collect();
        }
        if bpm_changed {
            self.bpm = bpm.clamp(20.0, 300.0);
            self.update_playback_speed();
        }
        if key_changed {
            self.transpose = live_key;
            if let Some(playback) = &self.playback {
                let _ = playback
                    .commands
                    .send(Command::SetTranspose(self.transpose));
            }
        }
        if let Some(playback) = &self.playback {
            for (channel, values) in edits {
                let _ = playback.commands.send(Command::SetChannelBank {
                    channel: *channel,
                    bank: values.bank,
                });
                let _ = playback.commands.send(Command::SetChannelProgram {
                    channel: *channel,
                    program: values.program,
                });
                let _ = playback.commands.send(Command::SetChannelVolume {
                    channel: *channel,
                    value: values.volume,
                });
                let _ = playback.commands.send(Command::SetChannelPan {
                    channel: *channel,
                    value: values.pan,
                });
                let _ = playback.commands.send(Command::SetChannelReverb {
                    channel: *channel,
                    value: values.reverb,
                });
                let _ = playback.commands.send(Command::SetChannelChorus {
                    channel: *channel,
                    value: values.chorus,
                });
            }
        }
    }

    /// Keep the editor viewport alive and run a requested save. The editor is
    /// deliberately edit-only: BPM/KEY and channel changes never touch the
    /// song that is currently playing, only the file written by Save.
    fn poll_midi_editor_window(&mut self, ctx: &egui::Context) {
        let id = midi_editor_viewport();
        let (open, path, save_requested, save_as_request, save_as_open) = {
            let w = self.midi_editor_window.lock().unwrap();
            (
                w.open,
                w.path.clone(),
                w.save_requested,
                w.save_as_requested.clone(),
                w.save_as_open_requested,
            )
        };
        if !open {
            if self.midi_editor_open {
                // Let the child viewport hide itself.
                ctx.request_repaint_of(id);
            }
            self.midi_editor_open = false;
            return;
        }
        if !self.midi_editor_available() || self.path.as_ref() != Some(&path) {
            let mut w = self.midi_editor_window.lock().unwrap();
            w.open = false;
            w.popup = None;
            w.save_confirm = false;
            w.save_as = None;
            w.save_as_confirm = false;
            w.save_as_pending = None;
            w.save_as_requested = None;
            w.save_as_open_requested = false;
            ctx.request_repaint_of(id);
            ctx.request_repaint_of(midi_editor_popup_viewport());
            ctx.request_repaint_of(midi_editor_confirm_viewport());
            ctx.request_repaint_of(midi_editor_save_as_viewport());
            return;
        }
        self.midi_editor_open = true;

        if save_as_open {
            self.midi_editor_window
                .lock()
                .unwrap()
                .save_as_open_requested = false;
            self.open_midi_save_as();
            ctx.request_repaint_of(midi_editor_save_as_viewport());
        }

        if save_requested {
            let (edits, names, bpm, bpm_base, key) = {
                let w = self.midi_editor_window.lock().unwrap();
                (
                    w.edits.clone(),
                    w.name_edits.clone(),
                    w.bpm,
                    w.bpm_base,
                    w.key,
                )
            };
            let result = self.save_midi_editor(&edits, &names, bpm, bpm_base, key);
            let now = ctx.input(|i| i.time);
            let mut apply_bpm = None;
            let mut apply_key = None;
            let mut apply_mix = Vec::new();
            let mut saved_key_offset = None;
            {
                let mut w = self.midi_editor_window.lock().unwrap();
                w.save_requested = false;
                match result {
                    Ok(()) => {
                        let bpm_changed = (w.bpm - w.bpm_base).abs() > 0.001;
                        let key_changed = w.key != 0;
                        if bpm_changed {
                            apply_bpm = Some(w.bpm);
                        }
                        w.status = Some(tr("Saved").to_owned());
                        w.status_error = false;
                        w.status_until = now + 4.0;
                        // The file now contains these values. Keep the editor
                        // display on them and measure the next save from the
                        // new file baseline.
                        w.bpm_base = w.bpm.max(1.0);
                        w.key_base += w.key;
                        saved_key_offset = Some(w.key_base);
                        if key_changed {
                            apply_key = Some(w.key_base);
                        }
                        w.key = 0;
                        w.key_min = -12;
                        w.key_max = 12;
                        apply_mix = w.edits.clone();
                        let saved = apply_mix.clone();
                        for (channel, values) in &saved {
                            for row in w.rows.iter_mut() {
                                if row.channel == Some(*channel) {
                                    row.program = values.program;
                                    row.bank = values.bank;
                                }
                            }
                        }
                        w.name_edits.clear();
                        w.base_names = w.rows.iter().map(|row| row.name.clone()).collect();
                    }
                    Err(err) => {
                        w.status = Some(tr_args(
                            "Could not save the MIDI file: {}",
                            &[&err as &dyn std::fmt::Display],
                        ));
                        w.status_error = true;
                        w.status_until = now + 8.0;
                    }
                }
            }
            if let Some(key_offset) = saved_key_offset
                && let Err(err) = self.update_library_after_save(&path, key_offset)
            {
                let mut w = self.midi_editor_window.lock().unwrap();
                w.status = Some(tr_args(
                    "Saved, but the song database could not be updated: {}",
                    &[&err as &dyn std::fmt::Display],
                ));
                w.status_error = true;
                w.status_until = now + 8.0;
            }
            // The editor is isolated while editing; once the file is written,
            // let the current playback follow what was saved.
            if let Some(bpm) = apply_bpm {
                self.bpm = bpm.clamp(20.0, 300.0);
                self.update_playback_speed();
            }
            if let Some(key) = apply_key {
                self.transpose = key.clamp(-12, 12);
                if let Some(playback) = &self.playback {
                    let _ = playback
                        .commands
                        .send(Command::SetTranspose(self.transpose));
                }
            }
            if let Some(playback) = &self.playback {
                for (channel, values) in &apply_mix {
                    let _ = playback.commands.send(Command::SetChannelBank {
                        channel: *channel,
                        bank: values.bank,
                    });
                    let _ = playback.commands.send(Command::SetChannelProgram {
                        channel: *channel,
                        program: values.program,
                    });
                    let _ = playback.commands.send(Command::SetChannelVolume {
                        channel: *channel,
                        value: values.volume,
                    });
                    let _ = playback.commands.send(Command::SetChannelPan {
                        channel: *channel,
                        value: values.pan,
                    });
                    let _ = playback.commands.send(Command::SetChannelReverb {
                        channel: *channel,
                        value: values.reverb,
                    });
                    let _ = playback.commands.send(Command::SetChannelChorus {
                        channel: *channel,
                        value: values.chorus,
                    });
                }
            }
            ctx.request_repaint_of(id);
        }

        if let Some(form) = save_as_request {
            let result = self.commit_midi_save_as(&form);
            let now = ctx.input(|i| i.time);
            let mut w = self.midi_editor_window.lock().unwrap();
            w.save_as_requested = None;
            match result {
                Ok(name) => {
                    w.status = Some(tr_args("Saved as {}", &[&name as &dyn std::fmt::Display]));
                    w.status_error = false;
                    w.status_until = now + 4.0;
                    w.save_as = None;
                    w.save_as_confirm = false;
                    w.save_as_pending = None;
                }
                Err(err) => {
                    let mut reopened = form;
                    reopened.error = Some(err);
                    w.save_as = Some(reopened);
                    w.save_as_confirm = false;
                    w.save_as_pending = None;
                    w.status = Some(tr("Could not save as").to_owned());
                    w.status_error = true;
                    w.status_until = now + 8.0;
                }
            }
            ctx.request_repaint_of(id);
            ctx.request_repaint_of(midi_editor_save_as_viewport());
            ctx.request_repaint_of(midi_editor_confirm_viewport());
        }
    }

    fn save_midi_editor(
        &mut self,
        edits: &[(u8, MidiEditorValues)],
        names: &[(usize, String)],
        bpm: f64,
        bpm_base: f64,
        key: i32,
    ) -> std::result::Result<(), String> {
        let Some(path) = self.path.clone() else {
            return Err(tr("No song is loaded").to_owned());
        };
        let rows = {
            let w = self.midi_editor_window.lock().unwrap();
            w.rows.clone()
        };
        let mut channels = Vec::with_capacity(edits.len());
        for (channel, values) in edits {
            let track = rows
                .iter()
                .find(|row| row.channel == Some(*channel))
                .map(|row| row.track)
                .unwrap_or(0);
            channels.push(karaoke_player::midi_write::ChannelMix {
                channel: *channel,
                track,
                bank: values.bank,
                program: values.program,
                volume: values.volume,
                pan: values.pan,
                reverb: values.reverb,
                chorus: values.chorus,
            });
        }
        let tempo_scale = if bpm_base > 1.0 && bpm > 1.0 {
            bpm_base / bpm
        } else {
            1.0
        };
        let smf_edits = karaoke_player::midi_write::SmfEdits {
            channels,
            track_names: names.to_vec(),
            tempo_scale,
            transpose: key,
        };
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mkc"))
        {
            let package =
                karaoke_player::mkc::read_package(&path).map_err(|err| format!("{err:#}"))?;
            let edited = karaoke_player::midi_write::edit_bytes(
                package.midi().map_err(|err| format!("{err:#}"))?,
                &smf_edits,
            )
            .map_err(|err| format!("{err:#}"))?;
            let edit = karaoke_player::mkc::MkcEdit {
                tempo: Some(bpm),
                time_scale: tempo_scale,
                ..Default::default()
            };
            karaoke_player::mkc::rewrite_midi(&path, &path, &edited, Some(&edit), true)
                .map_err(|err| format!("{err:#}"))
        } else {
            karaoke_player::midi_write::edit_file(&path, &smf_edits)
                .map_err(|err| format!("{err:#}"))
        }
    }

    /// After a successful save, refresh the library row, playlist cache and
    /// Song Data window so the new BPM/key/bars are visible without a restart.
    fn update_library_after_save(&mut self, path: &Path, key_offset: i32) -> Result<()> {
        let song = load(path, &LoadOptions::default())?;
        if !self.midi_song_is_in_library(path, song.format) {
            return Ok(());
        }
        let mut record = library::record_from_song(path, &song, "");
        if matches!(
            song.format,
            karaoke_player::SourceFormat::Ncn
                | karaoke_player::SourceFormat::Kmid
                | karaoke_player::SourceFormat::Mkc
        ) && let Some(base) = song.key.as_deref().filter(|value| !value.trim().is_empty())
        {
            record.key = Some(sounding_key(base, key_offset));
        }
        let mut library = Library::open_default()?;
        library.insert_records(std::slice::from_ref(&record))?;

        if let Some(existing) = self.library_songs.iter_mut().find(|row| {
            row.path == record.path || (row.code == record.code && row.format == record.format)
        }) {
            *existing = record.clone();
        } else {
            self.library_songs.push(record.clone());
        }
        self.library_songs_gen = self.library_songs_gen.wrapping_add(1);
        self.rebuild_library_meta();
        self.rebuild_library_view();
        self.titles.insert(
            record.path.clone(),
            SongInfo {
                code: record.code.clone(),
                title: record.title.clone(),
                artist: record.artist.clone(),
                file_type: record.format.clone(),
                bpm: record.tempo.unwrap_or(0.0),
                key: record.key.clone().unwrap_or_else(|| "C".to_owned()),
                bars: record.bars.unwrap_or(0),
                loaded: true,
            },
        );
        Ok(())
    }

    /// The "General" settings page: display durations and top-bar visibility.
    fn sync_background_image(&mut self, ctx: &egui::Context) {
        // The shipped artwork is decoded once, the first time it is needed:
        // it backs the image style whenever no external file is selected.
        if matches!(
            self.settings.background_style,
            BackgroundStyle::Default | BackgroundStyle::Image
        ) && self.default_background.is_none()
            && let Ok(image) = load_background_bytes(DEFAULT_BACKGROUND)
        {
            self.default_background =
                Some(ctx.load_texture("default_background", image, egui::TextureOptions::LINEAR));
        }
        if self.settings.background_style != BackgroundStyle::Image {
            return;
        }
        let Some(path) = self.settings.background_image.clone() else {
            return;
        };
        if self
            .background_texture
            .as_ref()
            .is_some_and(|(loaded, _)| loaded == &path)
        {
            return;
        }
        match load_background_image(&path) {
            Ok(image) => {
                self.background_texture = Some((
                    path,
                    ctx.load_texture("background_image", image, egui::TextureOptions::LINEAR),
                ));
                self.background_error = None;
            }
            Err(err) => {
                self.background_texture = None;
                self.background_error = Some(tr_args(
                    "Could not load the background image: {}",
                    &[&format!("{err:#}") as &dyn std::fmt::Display],
                ));
            }
        }
    }

    fn start_missing_check(&mut self) {
        if self.missing_checked || self.missing_check.is_some() {
            return;
        }
        self.missing_checked = true;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.missing_check = Some(receiver);
        std::thread::spawn(move || {
            let result = Library::open_default()
                .and_then(|mut library| library.mark_missing_files())
                .map_err(|err| format!("{err:#}"));
            let _ = sender.send(result);
        });
    }

    /// Run a database maintenance action off the UI thread.
    ///
    /// Checking 24k files takes a moment and VACUUM takes even longer on a
    /// large library, so the window must stay responsive while they run.
    fn start_library_op(&mut self, op: LibraryOp) {
        if self.library_op.is_some() {
            return;
        }
        self.library_note = None;
        self.library_error = None;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.library_op = Some((op, receiver));
        std::thread::spawn(move || {
            let result = match op {
                LibraryOp::CheckMissing => Library::open_default()
                    .and_then(|mut library| library.mark_missing_files())
                    .map(|(missing, restored)| LibraryOpResult::Missing { missing, restored }),
                LibraryOp::Compact => Library::open_default()
                    .and_then(|mut library| library.compact())
                    .map(|(before, after)| LibraryOpResult::Compact { before, after }),
            }
            .map_err(|err| format!("{err:#}"));
            let _ = sender.send(result);
        });
    }

    fn poll_library_op(&mut self) {
        let Some((_, receiver)) = self.library_op.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(result)) => {
                self.library_op = None;
                self.library_note = Some(match result {
                    LibraryOpResult::Missing { missing, restored } => tr_args(
                        "{} missing · {} restored",
                        &[&missing as &dyn std::fmt::Display, &restored],
                    ),
                    LibraryOpResult::Compact { before, after } => tr_args(
                        "Database {} → {}",
                        &[
                            &format_bytes(before) as &dyn std::fmt::Display,
                            &format_bytes(after),
                        ],
                    ),
                });
                self.reload_library();
            }
            Ok(Err(err)) => {
                self.library_op = None;
                self.library_error = Some(err);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.library_op = None,
        }
    }

    fn poll_missing_check(&mut self) {
        let Some(receiver) = self.missing_check.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok((0, 0))) => self.missing_check = None,
            Ok(Ok((missing, restored))) => {
                self.missing_check = None;
                self.library_note =
                    Some(tr_args("{} missing · {} restored", &[&missing, &restored]));
                self.reload_library();
            }
            Ok(Err(err)) => {
                self.missing_check = None;
                self.library_error = Some(err);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.missing_check = None,
        }
    }

    /// Load the browse list in the background. A 100k-song library takes about
    /// half a second, which is too long to block a frame.
    fn start_library_load(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.library_load = Some(receiver);
        std::thread::spawn(move || {
            let result = Library::open_default()
                .and_then(|library| library.songs())
                .map_err(|err| format!("{err:#}"));
            let _ = sender.send(result);
        });
    }

    fn poll_library_load(&mut self) {
        let Some(receiver) = self.library_load.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(songs)) => {
                self.library_load = None;
                self.library_songs = songs;
                self.library_songs_gen = self.library_songs_gen.wrapping_add(1);
                self.rebuild_library_meta();
                self.rebuild_library_view();
            }
            Ok(Err(err)) => {
                self.library_load = None;
                self.library_error = Some(err);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.library_load = None,
        }
    }

    fn open_library(&mut self) {
        self.library_open = true;
        self.library_error = None;
        self.library_search_focus = true;
        // Surface a damaged database here instead of failing silently later.
        if let Ok(library) = Library::open_default()
            && let Ok(Some(problem)) = library.quick_check()
        {
            self.library_error = Some(format!("{}: {problem}", tr("Database check failed")));
        }
        self.reload_library();
        self.open_library_window();
    }

    fn reload_library(&mut self) {
        self.library_songs_gen = self.library_songs_gen.wrapping_add(1);
        self.start_library_load();
    }

    fn rebuild_library_view(&mut self) {
        self.library_view_gen = self.library_view_gen.wrapping_add(1);
        let format = self.library_filter.format();
        let matches_format = |song: &SongRecord| format.is_none_or(|format| song.format == format);
        let query = self.library_query.trim().to_lowercase();
        if query.is_empty() {
            self.library_view = self
                .library_songs
                .iter()
                .enumerate()
                .filter(|(_, song)| matches_format(song))
                .map(|(index, _)| index)
                .collect();
            return;
        }
        if self.library_keys.len() != self.library_songs.len() {
            self.rebuild_library_meta();
        }
        // The lowercased text is prepared once per load, so typing does not
        // allocate four strings per song on every keystroke.
        let boundary = format!("\n{query}");
        let mut scored: Vec<(i32, usize)> = self
            .library_keys
            .iter()
            .enumerate()
            .filter(|(index, _)| matches_format(&self.library_songs[*index]))
            .filter_map(|(index, key)| {
                let score = if *key == query {
                    1000
                } else if key.starts_with(&query) || key.contains(&boundary) {
                    800
                } else if key.contains(&query) {
                    500
                } else {
                    0
                };
                (score > 0).then_some((score, index))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        self.library_view = scored.into_iter().map(|(_, index)| index).collect();
    }

    fn start_library_scan(&mut self) {
        if self.library_scan.is_some() {
            return;
        }
        let filter = self.library_filter;
        // When the user configured at least one folder, scan exactly those.
        // The built-in defaults are only used on a fresh install, otherwise a
        // folder the user cleared would silently come back.
        let any_configured = self.settings.ncn_folder.is_some()
            || self.settings.kmid_folder.is_some()
            || self.settings.kar_folder.is_some()
            || self.settings.mkc_folder.is_some();
        let mut folders: Vec<(String, PathBuf)> = Vec::new();
        let mut add = |name: &str, configured: Option<&Path>| {
            let path = if any_configured {
                configured.map(Path::to_path_buf)
            } else {
                effective_song_folder(configured, name)
            };
            if let Some(path) = path {
                folders.push((name.to_ascii_lowercase(), path));
            }
        };
        if matches!(filter, LibraryFilter::All | LibraryFilter::Ncn) {
            add("NCN", self.settings.ncn_folder.as_deref());
        }
        if matches!(filter, LibraryFilter::All | LibraryFilter::Kmid) {
            add("KMID", self.settings.kmid_folder.as_deref());
        }
        if matches!(filter, LibraryFilter::All | LibraryFilter::Kar) {
            add("KAR", self.settings.kar_folder.as_deref());
        }
        if matches!(filter, LibraryFilter::All | LibraryFilter::Mkc) {
            add("MKC", self.settings.mkc_folder.as_deref());
        }

        if folders.is_empty() {
            self.library_error = Some(tr("Choose at least one folder first"));
            return;
        }

        let (sender, receiver) = std::sync::mpsc::channel();
        self.library_scan = Some(LibraryScan {
            receiver,
            filter,
            progress: 0.0,
            done: 0,
            total: 0,
        });
        self.library_progress = 0.0;
        self.library_error = None;
        self.library_note = None;
        std::thread::spawn(move || {
            // Read the stored rows inside the worker: cloning a 100k-song list
            // on the UI thread would stall the frame.
            let existing = Library::open_default()
                .and_then(|library| library.songs())
                .unwrap_or_default();
            let outcome = {
                let report = |done, total| {
                    let _ = sender.send(LibraryScanMessage::Progress(done, total));
                };
                library::scan_folders(&folders, &existing, &report)
            };
            let _ = sender.send(LibraryScanMessage::Done(outcome));
        });
    }

    fn poll_library_scan(&mut self) {
        let mut finished = None;
        let mut disconnected = false;
        if let Some(scan) = self.library_scan.as_mut() {
            loop {
                match scan.receiver.try_recv() {
                    Ok(LibraryScanMessage::Progress(done, total)) => {
                        scan.done = done;
                        scan.total = total;
                        let value = if total > 0 {
                            done as f32 / total as f32
                        } else {
                            0.0
                        };
                        scan.progress = scan.progress.max(value);
                    }
                    Ok(LibraryScanMessage::Done(outcome)) => {
                        finished = Some(outcome);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if let Some(progress) = self.library_scan.as_ref().map(|scan| scan.progress) {
            self.library_progress = progress;
        }
        if disconnected {
            self.library_scan = None;
            self.library_error = Some(tr("The database scan stopped unexpectedly"));
        }
        let Some(outcome) = finished else {
            return;
        };
        let filter = self
            .library_scan
            .as_ref()
            .map(|scan| scan.filter)
            .unwrap_or(LibraryFilter::All);
        self.library_scan = None;
        self.library_progress = 1.0;
        let mut songs = outcome.songs;
        if let Some(format) = filter.format() {
            songs.retain(|song| song.format == format);
        }
        let summary = tr_args(
            "{} songs · {} unchanged · {} duplicates · {} skipped",
            &[
                &songs.len(),
                &outcome.unchanged,
                &outcome.duplicates,
                &outcome.skipped,
            ],
        );
        match Library::open_default().and_then(|mut library| library.rebuild_scan(&songs)) {
            Ok(()) => {
                self.start_library_load();
                self.library_note = Some(summary);
            }
            Err(err) => self.library_error = Some(format!("{err:#}")),
        }
    }

    // ------------------------------------------------------------ add songs

    fn start_import_scan(&mut self, folder: PathBuf) {
        if self.import_scan.is_some() || self.import_add.is_some() {
            return;
        }
        self.import_open = true;
        self.import_source = Some(folder.clone());
        self.import_items.clear();
        self.import_view.clear();
        self.import_progress = 0.0;
        self.import_note = None;
        self.import_error = None;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.import_scan = Some(ImportScan {
            receiver,
            progress: 0.0,
        });
        self.import_gen = self.import_gen.wrapping_add(1);
        self.open_import_window();
        std::thread::spawn(move || {
            let folders = vec![("all".to_owned(), folder)];
            let outcome = {
                let report = |done, total| {
                    let _ = sender.send(LibraryScanMessage::Progress(done, total));
                };
                library::scan_folders(&folders, &[], &report)
            };
            let _ = sender.send(LibraryScanMessage::Done(outcome));
        });
    }

    fn rebuild_import_view(&mut self) {
        self.import_gen = self.import_gen.wrapping_add(1);
        let format = self.import_filter.format();
        self.import_view = self
            .import_items
            .iter()
            .enumerate()
            .filter(|(_, item)| format.map_or(true, |format| item.record.format == format))
            .map(|(index, _)| index)
            .collect();
    }

    fn poll_import(&mut self) {
        let mut scan_done = None;
        let mut scan_disconnected = false;
        if let Some(scan) = self.import_scan.as_mut() {
            loop {
                match scan.receiver.try_recv() {
                    Ok(LibraryScanMessage::Progress(done, total)) => {
                        let value = if total > 0 {
                            done as f32 / total as f32
                        } else {
                            0.0
                        };
                        scan.progress = scan.progress.max(value);
                    }
                    Ok(LibraryScanMessage::Done(outcome)) => {
                        scan_done = Some(outcome);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        scan_disconnected = true;
                        break;
                    }
                }
            }
        }
        if let Some(progress) = self.import_scan.as_ref().map(|scan| scan.progress) {
            self.import_progress = self.import_progress.max(progress);
        }
        if scan_disconnected {
            self.import_scan = None;
            self.import_error = Some(tr("The folder scan stopped unexpectedly"));
        }
        if let Some(outcome) = scan_done {
            self.import_scan = None;
            self.import_progress = 1.0;
            self.import_items = outcome
                .songs
                .into_iter()
                .map(|record| ImportItem {
                    record,
                    selected: true,
                })
                .collect();
            self.rebuild_import_view();
            self.import_note = Some(tr_args("Found {} songs", &[&self.import_items.len()]));
        }

        let mut add_done = None;
        let mut add_disconnected = false;
        if let Some(add) = self.import_add.as_mut() {
            loop {
                match add.receiver.try_recv() {
                    Ok(ImportAddMessage::Progress(done, total)) => {
                        let value = if total > 0 {
                            done as f32 / total as f32
                        } else {
                            0.0
                        };
                        add.progress = add.progress.max(value);
                    }
                    Ok(ImportAddMessage::Done(outcome)) => {
                        add_done = Some(outcome);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        add_disconnected = true;
                        break;
                    }
                }
            }
        }
        if let Some(progress) = self.import_add.as_ref().map(|add| add.progress) {
            self.import_progress = self.import_progress.max(progress);
        }
        if add_disconnected {
            self.import_add = None;
            self.import_error = Some(tr("The import stopped unexpectedly"));
        }
        if let Some(outcome) = add_done {
            self.import_add = None;
            self.import_progress = 1.0;
            let mut added_records = Vec::new();
            let mut created_files = Vec::new();
            let mut added_sources = HashSet::new();
            for (source, record, files) in &outcome.added {
                added_sources.insert(source.clone());
                added_records.push(record.clone());
                created_files.extend(files.iter().cloned());
            }
            let added_count = added_records.len();
            if added_records.is_empty() {
                self.import_note = Some(tr_args(
                    "Added {} songs ({} skipped)",
                    &[&0usize, &outcome.skipped],
                ));
            } else {
                match Library::open_default()
                    .and_then(|mut library| library.insert_records(&added_records))
                {
                    Ok(()) => {
                        self.start_library_load();
                        self.import_items
                            .retain(|item| !added_sources.contains(&item.record.path));
                        self.rebuild_import_view();
                        self.import_note = Some(tr_args(
                            "Added {} songs ({} skipped)",
                            &[&added_count, &outcome.skipped],
                        ));
                    }
                    Err(err) => {
                        // The rows never reached the library, so drop the files
                        // this import copied instead of leaving them orphaned.
                        for path in &created_files {
                            let _ = std::fs::remove_file(path);
                        }
                        self.import_error = Some(format!("{err:#}"));
                    }
                }
            }
        }
    }

    fn start_import_add(&mut self) {
        if self.import_scan.is_some() || self.import_add.is_some() {
            return;
        }
        let selected: Vec<SongRecord> = self
            .import_view
            .iter()
            .filter_map(|&index| {
                self.import_items
                    .get(index)
                    .filter(|item| item.selected)
                    .map(|item| item.record.clone())
            })
            .collect();
        if selected.is_empty() {
            self.import_note = Some(tr("Select at least one song"));
            return;
        }

        let existing: HashSet<(String, String)> = self
            .library_songs
            .iter()
            .map(|song| (song.code.clone(), song.format.clone()))
            .collect();
        let mut skipped = 0usize;
        let mut work = Vec::new();
        for record in selected {
            if existing.contains(&(record.code.clone(), record.format.clone())) {
                skipped += 1;
                continue;
            }
            let destination = match record.format.as_str() {
                "ncn" => effective_song_folder(self.settings.ncn_folder.as_deref(), "NCN"),
                "kmid" => effective_song_folder(self.settings.kmid_folder.as_deref(), "KMID"),
                "kar" => effective_song_folder(self.settings.kar_folder.as_deref(), "KAR"),
                "mkc" => effective_song_folder(self.settings.mkc_folder.as_deref(), "MKC"),
                _ => None,
            };
            match destination {
                Some(destination) => work.push((record, destination)),
                None => skipped += 1,
            }
        }
        if work.is_empty() {
            self.import_note = Some(tr_args("Added {} songs ({} skipped)", &[&0usize, &skipped]));
            return;
        }

        let total = work.len();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.import_add = Some(ImportAdd {
            receiver,
            progress: 0.0,
        });
        self.import_progress = 0.0;
        self.import_error = None;
        self.import_note = None;
        std::thread::spawn(move || {
            let mut added = Vec::new();
            for (index, (record, destination)) in work.into_iter().enumerate() {
                match import_copy(&record, &destination) {
                    Ok((copied, files)) => added.push((record.path.clone(), copied, files)),
                    Err(_) => skipped += 1,
                }
                let _ = sender.send(ImportAddMessage::Progress(index + 1, total));
            }
            let _ = sender.send(ImportAddMessage::Done(ImportAddOutcome { added, skipped }));
        });
    }

    fn open_library_window(&mut self) {
        {
            let mut w = self.library_window.lock().unwrap();
            w.open = true;
            w.query = self.library_query.clone();
            w.filter = self.library_filter;
            w.focus_query = true;
            w.intents.clear();
        }
        self.sync_library_window(true);
        self.library_open = true;
    }

    /// Push the player's database state into the window.
    fn sync_library_window(&mut self, force: bool) {
        let mut w = self.library_window.lock().unwrap();
        if !w.open {
            return;
        }
        if force || w.songs_gen != self.library_songs_gen {
            w.songs = Arc::new(self.library_songs.clone());
            w.songs_gen = self.library_songs_gen;
            w.selected.clear();
            w.selection_anchor = None;
        }
        if force || w.view_gen != self.library_view_gen {
            w.view = Arc::new(self.library_view.clone());
            w.view_gen = self.library_view_gen;
            w.selected.clear();
            w.selection_anchor = None;
        }
        w.note = self.library_note.clone();
        w.error = self.library_error.clone();
        w.progress = self.library_progress;
        w.busy = self.library_scan.is_some() || self.library_op.is_some();
        w.busy_label = self.library_op.as_ref().map(|(op, _)| op.busy_label());
        w.loading = self.library_load.is_some();
    }

    /// Delete songs from the database, and optionally their files on disk.
    fn delete_library_songs(&mut self, songs: &[(PathBuf, String)], delete_files: bool) {
        self.library_note = None;
        self.library_error = None;
        let mut removed_paths = Vec::new();
        let mut failures = Vec::new();
        for (path, format) in songs {
            if delete_files {
                let mut targets = vec![path.clone()];
                if format.eq_ignore_ascii_case("ncn") {
                    targets.extend(karaoke_player::ncn::companions(path));
                }
                let mut ok = true;
                for target in targets {
                    if target.exists()
                        && let Err(err) = std::fs::remove_file(&target)
                    {
                        failures.push(format!("{}: {err}", target.display()));
                        ok = false;
                    }
                }
                if ok {
                    removed_paths.push(path.clone());
                }
            } else {
                removed_paths.push(path.clone());
            }
        }

        let mut removed = 0usize;
        if !removed_paths.is_empty() {
            match Library::open_default()
                .and_then(|mut library| library.delete_paths(&removed_paths))
            {
                Ok(count) => removed = count,
                Err(err) => failures.push(format!("{err:#}")),
            }
        }

        if delete_files {
            let deleted: HashSet<PathBuf> = removed_paths.into_iter().collect();
            let count = deleted.len();
            if !deleted.is_empty() {
                self.playlist.retain(|path| !deleted.contains(path));
                self.stopped_index = None;
                self.library_meta.retain(|path, _| !deleted.contains(path));
                for path in &deleted {
                    self.titles.remove(path);
                }
                if self
                    .path
                    .as_ref()
                    .is_some_and(|path| deleted.contains(path))
                {
                    self.clear_played_song();
                } else if let Some(path) = self.path.clone()
                    && let Some(index) = self.playlist.iter().position(|item| *item == path)
                {
                    self.playlist_index = index;
                    self.playlist_selection = Some(index);
                }
            }
            self.library_note = Some(tr_args(
                "Deleted {} songs",
                &[&count as &dyn std::fmt::Display],
            ));
        } else {
            self.library_note = Some(tr_args(
                "Removed {} songs from the database",
                &[&removed as &dyn std::fmt::Display],
            ));
        }
        if !failures.is_empty() {
            self.library_error = Some(failures.join("\n"));
        }
        self.reload_library();
    }

    /// Keep the Song Data window alive while it is open.
    fn draw_library_window(&mut self, ctx: &egui::Context) {
        let state = Arc::clone(&self.library_window);
        if !self.library_open && !state.lock().unwrap().open {
            return;
        }
        let title = tr("Song Data");
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("song_data_window"),
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([720.0, 402.0])
                .with_min_inner_size([720.0, 402.0])
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_always_on_top(),
            move |ctx, _class| {
                let mut w = state.lock().unwrap();
                if w.open != w.shown {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(w.open));
                    w.shown = w.open;
                }
                if !w.open {
                    return;
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(Color32::from_rgb(8, 8, 8)))
                    .show(ctx, |ui| {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(20, 18))
                            .show(ui, |ui| library_window_ui(ui, &mut w));
                    });
                if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || ctx.input(|i| i.viewport().close_requested())
                {
                    w.open = false;
                }
                // The delete question is its own OS window, parented to this
                // one so it always appears above the song table.
                let confirm_open = w.open && w.delete_confirm.is_some();
                let confirm_shown = w.delete_confirm_shown;
                drop(w);
                if confirm_open || confirm_shown {
                    let child_state = Arc::clone(&state);
                    ctx.show_viewport_deferred(
                        library_delete_confirm_viewport(),
                        egui::ViewportBuilder::default()
                            .with_title(tr("Delete songs"))
                            .with_inner_size([480.0, 175.0])
                            .with_min_inner_size([480.0, 175.0])
                            .with_resizable(false)
                            .with_minimize_button(false)
                            .with_maximize_button(false)
                            .with_active(true)
                            .with_always_on_top(),
                        move |ctx, _class| {
                            let mut w = child_state.lock().unwrap();
                            let wanted = w.open && w.delete_confirm.is_some();
                            if wanted != w.delete_confirm_shown {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(wanted));
                                w.delete_confirm_shown = wanted;
                            }
                            if !wanted {
                                return;
                            }
                            if ctx.input(|i| i.viewport().close_requested())
                                || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                w.delete_confirm = None;
                                w.delete_confirm_shown = false;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                                ctx.request_repaint_of(egui::ViewportId::from_hash_of(
                                    "song_data_window",
                                ));
                                return;
                            }
                            let count = w.delete_confirm.as_ref().map_or(0, Vec::len);
                            let mut cancel = false;
                            let mut choice: Option<bool> = None;
                            egui::CentralPanel::default()
                                .frame(
                                    egui::Frame::NONE
                                        .fill(Color32::from_rgb(8, 8, 8))
                                        .inner_margin(egui::Margin::symmetric(22, 18)),
                                )
                                .show(ctx, |ui| {
                                    ui.label(
                                        RichText::new(tr("Delete the selected songs?")).size(13.0),
                                    );
                                    ui.label(
                                        RichText::new(tr_args(
                                            "{} selected",
                                            &[&count as &dyn std::fmt::Display],
                                        ))
                                        .size(11.0)
                                        .color(DIM),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(tr(
                                            "Delete from folder also removes the files from disk.",
                                        ))
                                        .size(11.0)
                                        .color(ERROR),
                                    );
                                    ui.add_space(14.0);
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui
                                            .add_sized(
                                                [132.0, 28.0],
                                                egui::Button::new(tr("Delete from folder")),
                                            )
                                            .clicked()
                                        {
                                            choice = Some(true);
                                        }
                                        if ui
                                            .add_sized(
                                                [152.0, 28.0],
                                                egui::Button::new(tr("Remove from database")),
                                            )
                                            .clicked()
                                        {
                                            choice = Some(false);
                                        }
                                        if ui
                                            .add_sized(
                                                [80.0, 28.0],
                                                egui::Button::new(tr("Cancel")),
                                            )
                                            .clicked()
                                        {
                                            cancel = true;
                                        }
                                    });
                                });
                            if let Some(delete_files) = choice {
                                if let Some(songs) = w.delete_confirm.take() {
                                    w.intents.push(LibraryIntent::Delete {
                                        songs,
                                        delete_files,
                                    });
                                }
                                w.delete_confirm_shown = false;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                                ctx.request_repaint_of(egui::ViewportId::from_hash_of(
                                    "song_data_window",
                                ));
                            } else if cancel {
                                w.delete_confirm = None;
                                w.delete_confirm_shown = false;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                                ctx.request_repaint_of(egui::ViewportId::from_hash_of(
                                    "song_data_window",
                                ));
                            }
                        },
                    );
                }
            },
        );
    }

    /// Run the commands the window sent and keep it in sync.
    fn poll_library_window(&mut self, ctx: &egui::Context) {
        self.poll_library_scan();
        let (open, intents) = {
            let mut w = self.library_window.lock().unwrap();
            (w.open, std::mem::take(&mut w.intents))
        };
        if !open {
            if self.library_open {
                let id = egui::ViewportId::from_hash_of("song_data_window");
                ctx.request_repaint_of(id);
            }
            {
                let mut w = self.library_window.lock().unwrap();
                if w.delete_confirm.take().is_some() || w.delete_confirm_shown {
                    w.delete_confirm_shown = false;
                    ctx.send_viewport_cmd_to(
                        library_delete_confirm_viewport(),
                        egui::ViewportCommand::Visible(false),
                    );
                }
            }
            self.library_open = false;
            // The import window is independent, so it keeps running here.
            self.library_scan = None;
            return;
        }
        self.library_open = true;

        let (query, filter) = {
            let w = self.library_window.lock().unwrap();
            (w.query.clone(), w.filter)
        };
        let mut rebuild = false;
        for intent in intents {
            match intent {
                LibraryIntent::QueryChanged => {
                    self.library_query = query.clone();
                    rebuild = true;
                }
                LibraryIntent::FilterChanged => {
                    self.library_filter = filter;
                    rebuild = true;
                }
                LibraryIntent::Create => self.start_library_scan(),
                LibraryIntent::CheckMissing => self.start_library_op(LibraryOp::CheckMissing),
                LibraryIntent::Compact => self.start_library_op(LibraryOp::Compact),
                LibraryIntent::Backup => match backup_library() {
                    Ok((_, size)) => {
                        self.library_note = Some(tr_args(
                            "Backup saved ({})",
                            &[&format_bytes(size) as &dyn std::fmt::Display],
                        ));
                    }
                    Err(err) => self.library_error = Some(format!("{err:#}")),
                },
                LibraryIntent::Delete {
                    songs,
                    delete_files,
                } => self.delete_library_songs(&songs, delete_files),
                LibraryIntent::AddSong => {
                    self.import_source = None;
                    self.import_items.clear();
                    self.import_view.clear();
                    self.import_filter = LibraryFilter::All;
                    self.import_scan = None;
                    self.import_add = None;
                    self.import_progress = 0.0;
                    self.import_note = None;
                    self.import_error = None;
                    self.import_gen = self.import_gen.wrapping_add(1);
                    self.open_import_window();
                }
            }
        }
        if rebuild {
            self.rebuild_library_view();
        }
        if self.library_scan.is_some() {
            ctx.request_repaint();
        }
        self.sync_library_window(false);
        ctx.request_repaint_of(egui::ViewportId::from_hash_of("song_data_window"));
    }

    fn toggle_play(&mut self) {
        let Some(playback) = &self.playback else {
            self.play_from_playlist();
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

    /// Start the highlighted playlist entry (or the one Stop interrupted, or
    /// the first entry).
    fn play_from_playlist(&mut self) -> bool {
        let index = playlist_play_index(
            self.playlist_selection,
            self.stopped_index,
            self.playlist.len(),
        );
        let Some(index) = index else {
            return false;
        };
        let Some(path) = self.playlist.get(index).cloned() else {
            return false;
        };
        self.open(path);
        self.playlist_index = index;
        self.playlist_selection = Some(index);
        self.stopped_index = None;
        true
    }

    fn stop(&mut self) {
        // Stop also unloads the song: the transport, bar/beat and Song info
        // all go back to the idle state. Remember what was playing so Play
        // starts it again.
        let interrupted = self
            .playlist_selection
            .filter(|index| *index < self.playlist.len())
            .or_else(|| (self.playlist_index < self.playlist.len()).then_some(self.playlist_index));
        self.clear_played_song();
        self.stopped_index = interrupted;
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
        // The key belongs to a song: without one there is nothing to transpose.
        if self.song.is_none() {
            return;
        }
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

    /// True while a settings-style window covers the lyric area.
    ///
    /// Those windows own the keyboard: the lyric search popup and the playlist
    /// shortcuts must not react behind them.
    fn window_open(&self) -> bool {
        self.settings_open
            || self.split_open
            || self.library_open
            || self.import_open
            || self.exit_confirm_open.load(Ordering::Relaxed)
            || self.song_restore_confirm.load(Ordering::Relaxed)
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

        // The temporary search overlay owns Left/Right and Esc while open.
        let modal_open = self.window_open();
        if self.lyric_overlay_visible && !modal_open {
            // Space reopens the song-info card instead of typing a leading
            // space into the empty search box.
            if !self.settings.show_song_info
                && self.song.is_some()
                && self.lyric_search.trim().is_empty()
                && ctx.input(|i| i.key_pressed(egui::Key::Space))
            {
                self.hide_lyric_overlay_preserve_browse(ctx);
                self.song_card_until =
                    Some(ctx.input(|i| i.time) + self.settings.indicator_seconds as f64);
                ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Space);
                });
                return;
            }
            let (left, right, up, down, escape) = ctx.input(|i| {
                (
                    !i.modifiers.ctrl
                        && !i.modifiers.alt
                        && !i.modifiers.command
                        && i.key_pressed(egui::Key::ArrowLeft),
                    !i.modifiers.ctrl
                        && !i.modifiers.alt
                        && !i.modifiers.command
                        && i.key_pressed(egui::Key::ArrowRight),
                    !i.modifiers.ctrl
                        && !i.modifiers.alt
                        && !i.modifiers.command
                        && i.key_pressed(egui::Key::ArrowUp),
                    !i.modifiers.ctrl
                        && !i.modifiers.alt
                        && !i.modifiers.command
                        && i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if escape {
                self.hide_lyric_overlay(ctx);
                return;
            }
            if up || down {
                self.hide_lyric_overlay(ctx);
                self.search_restore_playlist = false;
                self.playlist_step(if down { 1 } else { -1 }, ctx);
                ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
                });
                return;
            }
            if left || right {
                self.step_lyric_selection(if right { 1 } else { -1 });
                self.lyric_overlay_until =
                    ctx.input(|i| i.time) + self.settings.popup_seconds as f64;
                ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft);
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight);
                });
                return;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some(path) = self.selected_lyric_path() {
                    self.select_search_song(path, ctx);
                }
                return;
            }
        }

        // Typing a letter or number, or pressing Left/Right, opens the popup
        // when no other text field has keyboard focus.
        if !ctx.egui_wants_keyboard_input() && !modal_open {
            let (text, left, right, modifiers) = ctx.input(|i| {
                let mut text = String::new();
                for event in &i.events {
                    if let egui::Event::Text(value) = event {
                        text.push_str(value);
                    }
                }
                (
                    text,
                    i.key_pressed(egui::Key::ArrowLeft),
                    i.key_pressed(egui::Key::ArrowRight),
                    i.modifiers,
                )
            });
            let wants_text = !modifiers.ctrl
                && !modifiers.alt
                && !modifiers.command
                && text.chars().any(|ch| ch.is_alphanumeric());
            if wants_text {
                if !self.lyric_overlay_visible {
                    self.show_lyric_overlay(ctx);
                    self.lyric_search.clear();
                    self.lyric_browse = None;
                    self.lyric_overlay_remembered = false;
                }
                self.lyric_search.push_str(&text);
                self.lyric_search_focus = true;
                self.lyric_browse = None;
                self.lyric_query_cache.clear();
                self.lyric_overlay_until =
                    ctx.input(|i| i.time) + self.settings.popup_seconds as f64;
                if let Some(id) = self.lyric_search_id {
                    ctx.data_mut(|data| {
                        data.remove::<egui::widgets::text_edit::TextEditState>(id);
                    });
                }
                ctx.input_mut(|i| {
                    i.events
                        .retain(|event| !matches!(event, egui::Event::Text(_)));
                });
                return;
            }
            if !self.lyric_overlay_visible && (left || right) {
                let remembered = self.lyric_overlay_remembered;
                self.lyric_overlay_remembered = false;
                self.show_lyric_overlay(ctx);
                // The first press reveals the remembered song; the next
                // press moves to the neighbour.
                if !remembered && self.lyric_browse.is_none() {
                    self.step_lyric_selection(if right { 1 } else { -1 });
                }
                ctx.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft);
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight);
                });
                return;
            }
        }

        // A settings-style window owns the keyboard and the transport: no
        // global shortcut may fire behind it. Its own widgets still get their
        // keys because egui handles those before this point.
        if modal_open {
            return;
        }

        // The lyric search field (and any other text input) owns the keyboard
        // while focused, so single-key shortcuts must not fire. Transport keys
        // stay available so the operator can still control playback.
        if ctx.egui_wants_keyboard_input() {
            let transport = ctx.input(|i| {
                i.key_pressed(egui::Key::F5)
                    || i.key_pressed(egui::Key::F6)
                    || i.key_pressed(egui::Key::F7)
                    || i.key_pressed(egui::Key::F8)
            });
            if !transport {
                return;
            }
        }

        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            // With nothing loaded this starts the playlist entry.
            let had_song = self.playback.is_some();
            self.toggle_play();
            if self.playback.is_some() {
                let state = if self.paused && had_song {
                    tr("PAUSE")
                } else {
                    tr("PLAY")
                };
                self.show_indicator(state, ctx);
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F6)) {
            if self.playback.is_some() {
                self.stop();
                self.show_indicator(tr("STOP"), ctx);
            }
        }
        // Ctrl+Backspace resets the MIDI device immediately.
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Backspace))
            && !ctx.egui_wants_keyboard_input()
        {
            self.send_midi_reset();
            self.show_indicator(tr("MIDI RESET"), ctx);
            ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Backspace);
            });
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ctx) {
            self.close_playlist();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F7)) {
            self.seek_bar(false, ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F8)) {
            self.seek_bar(true, ctx);
        }
        // Space toggles the song card when the top bar info is hidden. While
        // the playlist is open it closes the playlist and shows the card.
        if ctx.input(|i| i.key_pressed(egui::Key::Space))
            && !self.settings.show_song_info
            && !self.lyric_overlay_visible
            && self.song.is_some()
        {
            let now = ctx.input(|i| i.time);
            let card_visible = self.song_card_until.is_some_and(|until| now < until);
            if self.settings.show_playlist {
                self.close_playlist();
            }
            self.song_card_until = if card_visible {
                None
            } else {
                Some(now + self.settings.indicator_seconds as f64)
            };
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
                    self.playlist_index = index;
                    self.playlist_selection = Some(index);
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
            self.apply_controller_mutes();
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
            self.save_settings();
        }

        // BPM: Ctrl+' faster, Ctrl+; slower (20..200, one step = 1 BPM).
        let bpm_up = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Quote));
        let bpm_down = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Semicolon));
        if (bpm_up || bpm_down) && now - self.last_bpm_shortcut >= 0.12 && self.song.is_some() {
            self.last_bpm_shortcut = now;
            self.bpm = (self.bpm + if bpm_up { 1.0 } else { -1.0 }).clamp(20.0, 200.0);
            self.update_playback_speed();
            self.show_bpm_indicator(ctx);
        }

        // UI zoom: Ctrl+- / Ctrl++ / Ctrl+0. Applying it here (instead of
        // letting egui handle the keys) lets the auto-fit pass know the user
        // chose a level.
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
            ctx.set_zoom_factor(zoom);
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
        let over_lyric_search = pointer
            .map(|pos| self.lyric_search_rect.contains(pos))
            .unwrap_or(false);
        // The channel detail popup and its program list need the wheel for
        // scrolling instead of changing the master volume.
        let over_controller_detail = self.controller_detail_open
            && pointer.is_some_and(|pos| {
                let lyrics_upper = matches!(self.settings.text_visibility, TextVisibility::Upper);
                let panel = controller_panel_position(self.lyric_area, lyrics_upper);
                let detail = Rect::from_min_size(
                    controller_detail_position(
                        panel,
                        self.lyric_area,
                        self.controller_channel.min(15),
                        lyrics_upper,
                    ),
                    egui::vec2(CONTROLLER_DETAIL_WIDTH, CONTROLLER_DETAIL_HEIGHT),
                );
                detail.contains(pos)
            });

        let over_window = self.library_open
            || self.import_open
            || self.settings_open
            || self.split_open
            || self.exit_confirm_open.load(Ordering::Relaxed)
            || self.song_restore_confirm.load(Ordering::Relaxed);
        // Ctrl+wheel is egui's zoom gesture, so leave that alone.
        if scroll.abs() > 0.01
            && !over_playlist
            && !over_lyric_search
            && !over_controller_detail
            && !self.controller_detail_open
            && !over_window
            && !ctrl
        {
            let before = self.volume;
            self.volume = (self.volume + scroll * 0.0012).clamp(0.0, 1.0);
            if (self.volume - before).abs() > 1e-5 {
                if let Some(playback) = &self.playback {
                    let _ = playback.commands.send(Command::SetVolume(self.volume));
                } else if let Some(preview) = &self.preview {
                    let _ = preview.commands.send(Command::SetVolume(self.volume));
                }
                let percent = (self.volume * 100.0).round() as i32;
                self.show_indicator(
                    tr_args("VOL [{}]", &[&percent as &dyn std::fmt::Display]),
                    ctx,
                );
                // Persist the wheel change even without a song loaded.
                self.settings_save_at = Some(ctx.input(|i| i.time) + 0.6);
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
                            // A plain MIDI usually has no title of its own: use
                            // the file name so the bar is not empty.
                            let file_name = self.path.as_deref().map(file_stem).unwrap_or_default();
                            let title = song
                                .title
                                .clone()
                                .filter(|title| !title.trim().is_empty())
                                .or_else(|| (!file_name.is_empty()).then(|| file_name.clone()))
                                .unwrap_or_else(|| tr("Untitled"));
                            let title_galley =
                                painter.layout_no_wrap(title, FontId::proportional(17.0), FG);
                            // Files sometimes repeat their own file name as the
                            // artist; never show that twice.
                            let artist_galley = song
                                .artist
                                .as_deref()
                                .filter(|artist| !artist.trim().is_empty())
                                .filter(|artist| {
                                    !artist.trim().eq_ignore_ascii_case(file_name.trim())
                                })
                                .map(|artist| {
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
                                karaoke_player::SourceFormat::Mkc => tr_args(
                                    "MKC  ·  {} BPM  ·  {} lines",
                                    &[
                                        &bpm_text as &dyn std::fmt::Display,
                                        &lines as &dyn std::fmt::Display,
                                    ],
                                ),
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
                        // The controller has its own channel menus; don't open
                        // the window menu on top of it.
                        let lyrics_upper =
                            matches!(self.settings.text_visibility, TextVisibility::Upper);
                        let controller_rect = if self.controller_open {
                            Rect::from_min_size(
                                controller_panel_position(self.lyric_area, lyrics_upper),
                                egui::vec2(CONTROLLER_WIDTH, CONTROLLER_HEIGHT),
                            )
                        } else {
                            Rect::NOTHING
                        };
                        if !controller_rect.contains(pos) {
                            self.open_menu(MenuKind::Central, pos);
                        }
                    }
                }

                let painter = ui.painter_at(rect);

                // An image background fills the lyric area and scales with the
                // window: the user's own picture, or the shipped artwork when
                // no file is selected.
                let background_texture = match self.settings.background_style {
                    BackgroundStyle::Color => None,
                    BackgroundStyle::Default | BackgroundStyle::Image => self
                        .background_texture
                        .as_ref()
                        .map(|(_, texture)| texture)
                        .or(self.default_background.as_ref()),
                };
                if let Some(texture) = background_texture {
                    paint_cover(&painter, rect, texture);
                }

                // The readouts (VOL/KEY/BPM/FONT/UI) work without a song, so
                // they are drawn before the "no song" return below.
                if let Some(indicator) = &self.indicator {
                    let now = ui.ctx().input(|i| i.time);
                    if now <= indicator.until {
                        let text = if indicator.bpm {
                            bpm_readout(self.bpm_adjust(), self.current_bpm())
                        } else {
                            indicator.text.clone()
                        };
                        let scale = window_fit_scale(ui.ctx());
                        let mut top_offset = if self.settings.show_playlist
                            && !matches!(self.settings.text_visibility, TextVisibility::Upper)
                        {
                            8.0
                        } else {
                            18.0
                        };
                        // The search popup covers the top of the lyrics: put the
                        // readout below it instead of drawing over its text.
                        if self.lyric_overlay_visible
                            && self.lyric_overlay_rect.is_positive()
                            && self.lyric_overlay_rect.top() <= rect.top() + 40.0 * scale
                        {
                            let below =
                                self.lyric_overlay_rect.bottom() - rect.top() + 10.0 * scale;
                            top_offset = (below / scale).max(top_offset);
                        }
                        // Never let it fall out of the lyrics area.
                        top_offset = top_offset.min((rect.height() - 80.0 * scale) / scale);
                        draw_indicator(&painter, rect, top_offset, scale, &text);
                    }
                }

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
                // Upper and lower lyrics keep their position from before the
                // playlist opened.
                let raw_center = match self.settings.text_visibility {
                    TextVisibility::Upper => {
                        self.lyric_area.top() + self.lyric_area.height() * 0.22
                    }
                    TextVisibility::Lower => {
                        self.lyric_area.top() + self.lyric_area.height() * 0.78
                    }
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

                // When the top-bar song info is hidden, show the translucent
                // song card for the first five seconds (and on Space).
                if !self.settings.show_song_info
                    && !self.settings.show_playlist
                    && !self.lyric_overlay_visible
                {
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

        self.central_rect = central.response.rect;

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
        self.midi_menu_rect = Rect::NOTHING;
        self.tools_menu_rect = Rect::NOTHING;
        self.background_menu_rect = Rect::NOTHING;
        self.refresh_midi_ports();
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
                let show = !self.settings.show_playlist;
                if show {
                    self.dismiss_song_card();
                }
                if self.lyric_overlay_visible {
                    self.hide_lyric_overlay(ctx);
                }
                self.search_restore_playlist = false;
                self.settings.show_playlist = show;
                self.temp_playlist_until = None;
                self.temp_playlist_hidden = false;
                self.save_settings();
            }
            MenuAction::Remove(index) => self.remove_playlist_entry(index),
            MenuAction::Settings => self.open_settings(),
            MenuAction::SongData => self.open_library(),
            MenuAction::SetMidiOut(port) => self.apply_midi_out(port),
            MenuAction::SetBackground(style) => {
                if self.settings.background_style != style {
                    self.settings.background_style = style;
                    self.save_settings();
                }
            }
            MenuAction::SplitVoice => self.open_split_window(),
            MenuAction::MidiController => {
                if self.controller_open {
                    self.stop_controller_note();
                    self.controller_detail_open = false;
                    self.controller_detail_program_open = false;
                    self.preview = None;
                    self.preview_attempted = false;
                }
                self.controller_open = !self.controller_open;
                self.settings.controller_open = self.controller_open;
                self.save_settings();
                if self.controller_open {
                    self.controller_last_time = ctx.input(|i| i.time);
                }
            }
            MenuAction::MidiEditor => {
                if self.midi_editor_open {
                    let mut w = self.midi_editor_window.lock().unwrap();
                    w.open = false;
                    w.popup = None;
                    w.save_confirm = false;
                    ctx.request_repaint_of(midi_editor_viewport());
                    ctx.request_repaint_of(midi_editor_popup_viewport());
                    ctx.request_repaint_of(midi_editor_confirm_viewport());
                } else {
                    self.open_midi_editor();
                }
            }
            MenuAction::ToggleTransport => {
                self.settings.hide_transport = !self.settings.hide_transport;
                self.save_settings();
            }
            MenuAction::About => self.open_settings_at(7),
            MenuAction::ToggleFullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                self.settings.fullscreen = !fullscreen;
                self.save_settings();
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            MenuAction::Minimize => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                self.main_minimized = true;
                for id in child_viewports() {
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(true));
                }
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
            MenuAction::PlaylistPrev => self.advance(-1),
            MenuAction::PlaylistNext => self.advance(1),
            MenuAction::Exit => {
                if self.settings.confirm_exit {
                    self.exit_confirm_open.store(true, Ordering::Relaxed);
                } else {
                    self.exit_confirmed.store(true, Ordering::Relaxed);
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

        // Escape closes the menu like a native popup.
        if !closing && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(menu) = self.menu.as_mut() {
                menu.closing = true;
            }
        }

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
            MenuKind::Row(_) => (188.0, 192.0),
            MenuKind::Background => (188.0, 272.0),
            MenuKind::Central => (188.0, 328.0),
        };
        let pos = Pos2::new(
            pos.x.min(screen.right() - width).max(screen.left()),
            pos.y.min(screen.bottom() - height).max(screen.top()),
        );

        let mut action = None;
        let mut song_row: Option<Rect> = None;
        let mut song_hovered = false;
        let mut song_clicked = false;
        let mut midi_row_rect: Option<Rect> = None;
        let mut midi_hovered = false;
        let mut tools_row_rect: Option<Rect> = None;
        let mut tools_hovered = false;
        let mut background_row_rect: Option<Rect> = None;
        let mut background_hovered = false;
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
                                let playlist = if self.settings.show_playlist {
                                    tr("Hide playlist")
                                } else {
                                    tr("Show playlist")
                                };
                                if menu_item(ui, Icon::ListMusic, &playlist) {
                                    action = Some(MenuAction::TogglePlaylist);
                                }
                                if menu_item(ui, Icon::Database, &tr("Song Data")) {
                                    action = Some(MenuAction::SongData);
                                }
                                let background_row = menu_item_response(
                                    ui,
                                    Icon::Image,
                                    &format!("{}  \u{203a}", tr("Background")),
                                );
                                background_hovered = background_row.hovered();
                                background_row_rect = Some(background_row.rect);
                                if menu_item(ui, Icon::Music, &tr("Split MIDI")) {
                                    action = Some(MenuAction::SplitVoice);
                                }
                                let tools_row = menu_item_response(
                                    ui,
                                    Icon::Wrench,
                                    &format!("{}  \u{203a}", tr("Tools")),
                                );
                                tools_hovered = tools_row.hovered();
                                tools_row_rect = Some(tools_row.rect);
                                let can_edit_midi = self.midi_editor_available();
                                ui.add_enabled_ui(can_edit_midi, |ui| {
                                    if menu_item(ui, Icon::AudioLines, &tr("MIDI Editor")) {
                                        action = Some(MenuAction::MidiEditor);
                                    }
                                });
                                let midi_row = menu_item_response(
                                    ui,
                                    Icon::AudioLines,
                                    &format!("{}  \u{203a}", tr("MIDI Out")),
                                );
                                midi_hovered = midi_row.hovered();
                                midi_row_rect = Some(midi_row.rect);
                                if menu_item(ui, Icon::Settings, &tr("Setting…")) {
                                    action = Some(MenuAction::Settings);
                                }
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
                                let transport_label = if self.settings.hide_transport {
                                    tr("Show transport bar")
                                } else {
                                    tr("Hide transport bar")
                                };
                                let transport_icon = if self.settings.hide_transport {
                                    Icon::Maximize
                                } else {
                                    Icon::Minus
                                };
                                if menu_item(ui, transport_icon, &transport_label) {
                                    action = Some(MenuAction::ToggleTransport);
                                }
                                ui.separator();
                                if menu_item(ui, Icon::Info, &tr("About")) {
                                    action = Some(MenuAction::About);
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
                                if menu_item(ui, Icon::SkipBack, &tr("Prev")) {
                                    action = Some(MenuAction::PlaylistPrev);
                                }
                                if menu_item(ui, Icon::SkipForward, &tr("Next")) {
                                    action = Some(MenuAction::PlaylistNext);
                                }
                                let repeat_label = if self.settings.repeat {
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
                                if menu_item(ui, Icon::SkipBack, &tr("Prev")) {
                                    action = Some(MenuAction::PlaylistPrev);
                                }
                                if menu_item(ui, Icon::SkipForward, &tr("Next")) {
                                    action = Some(MenuAction::PlaylistNext);
                                }
                                let repeat_label = if self.settings.repeat {
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

        // MIDI Out flyout: opens on hover and stays while the pointer is in it.
        if matches!(kind, MenuKind::Central) {
            if let Some(row) = midi_row_rect {
                let pointer = ctx.input(|i| i.pointer.hover_pos());
                let over_flyout = pointer.is_some_and(|p| self.midi_menu_rect.contains(p));
                if midi_hovered || over_flyout {
                    // The Song Player flyout must not stay open as well.
                    if midi_hovered {
                        if let Some(menu) = &mut self.menu {
                            menu.submenu_open = false;
                        }
                        self.tools_menu_rect = Rect::NOTHING;
                    }
                    let width = 250.0;
                    let x = if row.right() + 2.0 + width <= screen.right() {
                        row.right() + 2.0
                    } else {
                        area.response.rect.left() - width - 2.0
                    };
                    let (act, rect) = midi_out_submenu(
                        ctx,
                        Pos2::new(x, row.top() - 6.0),
                        alpha,
                        &self.settings.midi_out_port,
                        &self.midi_ports,
                    );
                    sub_action = act;
                    self.midi_menu_rect = rect;
                } else {
                    self.midi_menu_rect = Rect::NOTHING;
                }
            }
        }

        // Tools flyout: opens on hover and stays while the pointer is in it.
        if matches!(kind, MenuKind::Central) {
            if let Some(row) = tools_row_rect {
                let pointer = ctx.input(|i| i.pointer.hover_pos());
                let over_flyout = pointer.is_some_and(|p| self.tools_menu_rect.contains(p));
                if tools_hovered || over_flyout {
                    // The other flyouts must not stay open as well.
                    if tools_hovered {
                        if let Some(menu) = &mut self.menu {
                            menu.submenu_open = false;
                        }
                        self.midi_menu_rect = Rect::NOTHING;
                    }
                    let width = 188.0;
                    let x = if row.right() + 2.0 + width <= screen.right() {
                        row.right() + 2.0
                    } else {
                        area.response.rect.left() - width - 2.0
                    };
                    let (act, rect) = tools_submenu(
                        ctx,
                        Pos2::new(x, row.top() - 6.0),
                        alpha,
                        self.controller_open,
                    );
                    sub_action = act;
                    self.tools_menu_rect = rect;
                } else {
                    self.tools_menu_rect = Rect::NOTHING;
                }
            }
        }

        // Background flyout: image or colour, mirroring the settings picker.
        if matches!(kind, MenuKind::Central) {
            if let Some(row) = background_row_rect {
                let pointer = ctx.input(|i| i.pointer.hover_pos());
                let over_flyout = pointer.is_some_and(|p| self.background_menu_rect.contains(p));
                if background_hovered || over_flyout {
                    let width = 188.0;
                    let x = if row.right() + 2.0 + width <= screen.right() {
                        row.right() + 2.0
                    } else {
                        area.response.rect.left() - width - 2.0
                    };
                    let (act, rect) = background_submenu(
                        ctx,
                        Pos2::new(x, row.top() - 6.0),
                        alpha,
                        self.settings.background_style,
                    );
                    sub_action = act;
                    self.background_menu_rect = rect;
                } else {
                    self.background_menu_rect = Rect::NOTHING;
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

    /// Ask before the Song Data page clears its folder list.
    ///
    /// The reset button lives in the settings window, so the question gets
    /// its own small OS window instead of covering the page.
    fn draw_song_restore_confirm(&self, ctx: &egui::Context) {
        let open = Arc::clone(&self.song_restore_confirm);
        let confirmed = Arc::clone(&self.song_restore_confirmed);
        ctx.show_viewport_deferred(
            song_restore_confirm_viewport(),
            egui::ViewportBuilder::default()
                .with_title(tr("Reset to defaults"))
                .with_inner_size([400.0, 150.0])
                .with_min_inner_size([400.0, 150.0])
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_always_on_top(),
            move |ctx, _class| {
                if !open.load(Ordering::Relaxed) {
                    return;
                }
                if ctx.input(|i| i.viewport().close_requested())
                    || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    open.store(false, Ordering::Relaxed);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    return;
                }
                let mut cancel = false;
                let mut reset = false;
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(8, 8, 8))
                            .inner_margin(egui::Margin::symmetric(22, 18)),
                    )
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(tr("Reset the song data folders to their defaults?"))
                                .size(12.5),
                        );
                        ui.label(
                            RichText::new(tr(
                                "The selected NCN, KMID, KAR and MKC folders will be cleared.",
                            ))
                            .size(11.0)
                            .color(DIM),
                        );
                        ui.add_space(18.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_sized([84.0, 28.0], egui::Button::new(tr("Reset")))
                                .clicked()
                            {
                                reset = true;
                            }
                            if ui
                                .add_sized([84.0, 28.0], egui::Button::new(tr("Cancel")))
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                    });
                if reset {
                    confirmed.store(true, Ordering::Relaxed);
                    open.store(false, Ordering::Relaxed);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                } else if cancel {
                    open.store(false, Ordering::Relaxed);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            },
        );
    }

    /// Clear the Song Data folders once the confirmation popup agrees.
    fn apply_song_data_reset(&mut self, ctx: &egui::Context) {
        self.reset_settings_page(6, ctx);
        {
            let mut pages = self.settings_pages.lock().unwrap();
            pages.settings.ncn_folder = None;
            pages.settings.kmid_folder = None;
            pages.settings.kar_folder = None;
            pages.settings.mkc_folder = None;
        }
        self.save_settings();
        ctx.request_repaint_of(egui::ViewportId::from_hash_of("settings_window"));
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    /// Confirm before leaving the player.
    /// Show the exit confirmation in its own OS window.
    fn draw_exit_confirm(&self, ctx: &egui::Context) {
        let open = Arc::clone(&self.exit_confirm_open);
        let confirmed = Arc::clone(&self.exit_confirmed);
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("exit_confirm_window"),
            egui::ViewportBuilder::default()
                .with_title(tr("Exit DontreeCrab?"))
                .with_inner_size([380.0, 130.0])
                .with_min_inner_size([380.0, 130.0])
                .with_resizable(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_always_on_top(),
            move |ctx, _class| {
                if !open.load(Ordering::Relaxed) {
                    return;
                }
                if ctx.input(|i| i.viewport().close_requested())
                    || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    open.store(false, Ordering::Relaxed);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    return;
                }
                let mut cancel = false;
                let mut exit = false;
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(8, 8, 8))
                            .inner_margin(egui::Margin::symmetric(22, 18)),
                    )
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(tr("Are you sure you want to close the player?"))
                                .size(12.5),
                        );
                        ui.add_space(18.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_sized([84.0, 28.0], egui::Button::new(tr("Exit")))
                                .clicked()
                            {
                                exit = true;
                            }
                            if ui
                                .add_sized([84.0, 28.0], egui::Button::new(tr("Cancel")))
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                    });
                if exit {
                    confirmed.store(true, Ordering::Relaxed);
                    open.store(false, Ordering::Relaxed);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
                } else if cancel {
                    open.store(false, Ordering::Relaxed);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            },
        );
    }
}

/// A command the settings window sends back to the player.
enum SettingsIntent {
    Save,
    /// Reset the page on the player side as well; the save merge would
    /// otherwise put the player-owned fields (folders, lyric font) back.
    ResetPage(usize),
    /// Ask before clearing the Song Data folders.
    ConfirmSongReset,
    PickSoundFont,
    PickLyricFont,
    PickBackgroundImage,
    PickFolder(DialogKind),
    OpenLibrary,
    OpenSplitWindow,
    SetMidiOut(String),
    ResetMidi,
    SendMidiOptions,
    SendEq,
    SendEffects,
    RefreshMidiPorts,
    RebuildBuffer,
}

/// The settings pages, shared with the settings window's own viewport.
struct SettingsPages {
    open: bool,
    shown: bool,
    section: usize,
    settings: Settings,
    sound_font_path: Option<PathBuf>,
    lyric_font_loaded: bool,
    midi_ports: Vec<String>,
    midi_ports_loaded: bool,
    midi_error: Option<String>,
    soundfont_error: Option<String>,
    background_texture: Option<(PathBuf, egui::TextureHandle)>,
    default_background: Option<egui::TextureHandle>,
    background_error: Option<String>,
    about_logo: Option<egui::TextureHandle>,
    about_popup: Option<AboutPopup>,
    intents: Vec<SettingsIntent>,
}

impl Default for SettingsPages {
    fn default() -> Self {
        Self {
            open: false,
            shown: false,
            section: 0,
            settings: Settings::default(),
            sound_font_path: None,
            lyric_font_loaded: false,
            midi_ports: Vec::new(),
            midi_ports_loaded: false,
            midi_error: None,
            soundfont_error: None,
            background_texture: None,
            default_background: None,
            background_error: None,
            about_logo: None,
            about_popup: None,
            intents: Vec::new(),
        }
    }
}

impl SettingsPages {
    /// Draw the whole settings window.
    fn ui(&mut self, ui: &mut egui::Ui) {
        const PAGE_WIDTH: f32 = 564.0;
        const PAGE_HEIGHT: f32 = 400.0;
        // Fill the window: the sidebar and divider reach the bottom edge.
        let height = ui.available_height().max(PAGE_HEIGHT + 16.0 + 14.0);
        let size = egui::vec2(148.0 + 1.0 + PAGE_WIDTH + 22.0 + 22.0, height);
        ui.set_min_size(size);
        ui.set_max_size(size);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            ui.allocate_ui_with_layout(
                egui::vec2(148.0, size.y),
                Layout::top_down(Align::Min),
                |ui| {
                    let rect = ui.max_rect();
                    ui.painter().rect_filled(
                        rect,
                        egui::CornerRadius::same(0),
                        Color32::from_gray(16),
                    );
                    ui.add_space(14.0);
                    for (section, label, icon) in [
                        (0, tr("General"), Icon::Settings),
                        (1, tr("Text"), Icon::Type),
                        (2, tr("MIDI"), Icon::Music),
                        (3, tr("SoundFont"), Icon::AudioLines),
                        (4, tr("Player"), Icon::Play),
                        (5, tr("Background"), Icon::Image),
                        (6, tr("Song Data"), Icon::Database),
                    ] {
                        if nav_row(ui, &label, self.section == section, icon).clicked() {
                            self.section = section;
                            self.about_popup = None;
                            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                        }
                    }
                    let remaining = ui.available_height() - 44.0;
                    if remaining > 0.0 {
                        ui.add_space(remaining);
                    }
                    if nav_row(ui, &tr("About"), self.section == 7, Icon::Info).clicked() {
                        self.section = 7;
                        self.about_popup = None;
                        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                    }
                },
            );
            let (divider, _) =
                ui.allocate_exact_size(egui::vec2(1.0, size.y), egui::Sense::hover());
            ui.painter()
                .rect_filled(divider, 0.0, Color32::from_gray(70));
            egui::Frame::NONE
                .inner_margin(egui::Margin {
                    left: 22,
                    right: 22,
                    top: 16,
                    bottom: 14,
                })
                .show(ui, |ui| {
                    let page_height = height - 16.0 - 14.0;
                    ui.set_min_size(egui::vec2(PAGE_WIDTH, page_height));
                    ui.set_max_size(egui::vec2(PAGE_WIDTH, page_height));
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                        let mut changed = false;
                        match self.section {
                            1 => changed = self.text_page(ui),
                            2 => changed = self.midi_page(ui),
                            3 => changed = self.soundfont_page(ui),
                            4 => changed = self.player_page(ui),
                            5 => changed = self.background_page(ui),
                            6 => changed = self.song_data_page(ui),
                            7 => self.about_page(ui),
                            _ => changed = self.general_page(ui),
                        }
                        if changed {
                            self.intents.push(SettingsIntent::Save);
                        }
                        // The window title bar already closes it, so the
                        // bottom-right button resets the current page.
                        if self.section != 7 {
                            let label = tr("Reset to defaults");
                            let width = 132.0;
                            let button_size = egui::vec2(width, 26.0);
                            let page_rect = ui.max_rect();
                            let button_rect = egui::Rect::from_min_size(
                                egui::pos2(
                                    page_rect.right() - button_size.x,
                                    page_rect.bottom() - button_size.y - 14.0,
                                ),
                                button_size,
                            );
                            if ui.put(button_rect, egui::Button::new(label)).clicked() {
                                self.reset_page();
                            }
                        }
                    });
                });
        });
    }

    /// Reset the settings shown on the current page.
    fn reset_page(&mut self) {
        // Clearing the folder list is destructive, so ask before doing it.
        if self.section == 6 {
            self.intents.push(SettingsIntent::ConfirmSongReset);
            return;
        }
        let d = Settings::default();
        match self.section {
            0 => {
                self.settings.indicator_seconds = d.indicator_seconds;
                self.settings.playlist_seconds = d.playlist_seconds;
                self.settings.popup_seconds = d.popup_seconds;
                self.settings.show_song_info = d.show_song_info;
                self.settings.show_bar_beat = d.show_bar_beat;
                self.settings.show_clock = d.show_clock;
                self.settings.confirm_exit = d.confirm_exit;
                self.settings.language = d.language;
            }
            1 => {
                self.settings.font_scale = d.font_scale;
                self.settings.lyric_edge_size = d.lyric_edge_size;
                self.settings.lyric_distance = d.lyric_distance;
                self.settings.text_visibility = d.text_visibility;
                self.settings.cursor_visibility = d.cursor_visibility;
                self.settings.lyric_font_style = d.lyric_font_style;
                self.settings.lyric_font = d.lyric_font.clone();
                self.settings.lyric_depth = d.lyric_depth;
                self.settings.lyric_text_color = d.lyric_text_color.clone();
                self.settings.lyric_edge_color = d.lyric_edge_color.clone();
                self.settings.cursor_text_color = d.cursor_text_color.clone();
                self.settings.cursor_edge_color = d.cursor_edge_color.clone();
            }
            2 => {
                self.settings.midi_out_port = d.midi_out_port.clone();
                self.settings.midi_sysex = d.midi_sysex;
                self.settings.midi_reset = d.midi_reset;
                self.settings.lock_bass = d.lock_bass;
                self.settings.lock_bass_program = d.lock_bass_program;
                self.settings.lock_drum_kit = d.lock_drum_kit;
                self.settings.lock_drum_program = d.lock_drum_program;
                self.settings.lock_kick = d.lock_kick;
                self.settings.lock_kick_note = d.lock_kick_note;
                self.settings.lock_snare = d.lock_snare;
                self.settings.lock_snare_note = d.lock_snare_note;
                self.settings.metronome = d.metronome;
                self.settings.metronome_first = d.metronome_first;
                self.settings.metronome_hihat = d.metronome_hihat;
                self.settings.metronome_other = d.metronome_other;
                self.intents.push(SettingsIntent::SendMidiOptions);
                self.intents.push(SettingsIntent::SetMidiOut(String::new()));
            }
            3 => {
                self.settings.eq_enabled = d.eq_enabled;
                self.settings.midi_effects = d.midi_effects;
                self.settings.max_polyphony = d.max_polyphony;
                self.settings.sf2_buffer_ms = d.sf2_buffer_ms;
                self.settings.eq_low_db = d.eq_low_db;
                self.settings.eq_mid_db = d.eq_mid_db;
                self.settings.eq_high_db = d.eq_high_db;
                self.settings.rev_inst = d.rev_inst;
                self.settings.cho_inst = d.cho_inst;
                self.settings.rev_drum = d.rev_drum;
                self.settings.cho_drum = d.cho_drum;
                self.intents.push(SettingsIntent::SendEq);
                self.intents.push(SettingsIntent::SendEffects);
                self.intents.push(SettingsIntent::RebuildBuffer);
            }
            4 => {
                self.settings.delete_played_songs = d.delete_played_songs;
                self.settings.play_immediately = d.play_immediately;
                self.settings.auto_play = d.auto_play;
                self.settings.music_interval_seconds = d.music_interval_seconds;
                self.settings.transpose_mode = d.transpose_mode;
                self.settings.midi_reset = d.midi_reset;
            }
            5 => {
                self.settings.background_style = d.background_style;
                self.settings.background_color = d.background_color.clone();
                self.settings.background_image = d.background_image.clone();
            }
            6 => {
                self.settings.ncn_folder = d.ncn_folder.clone();
                self.settings.kmid_folder = d.kmid_folder.clone();
                self.settings.kar_folder = d.kar_folder.clone();
                self.settings.mkc_folder = d.mkc_folder.clone();
            }
            _ => return,
        }
        self.intents.push(SettingsIntent::ResetPage(self.section));
        self.intents.push(SettingsIntent::Save);
    }

    /// The SoundFont page: active file, polyphony, buffer, EQ and sends.
    fn soundfont_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut eq_changed = false;
        let mut sends_changed = false;
        let mut buffer_changed = false;
        ui.add_space(14.0);
        let button_width = 36.0f32;
        let field_width = (ui.available_width() - button_width - 10.0 - 48.0).max(120.0);
        let row_width = field_width + 10.0 + button_width;
        let left_offset = ((ui.available_width() - row_width) * 0.5).max(0.0);

        // The title sits at the top-right corner of the
        // SoundFont box.
        let (heading, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 16.0), egui::Sense::hover());
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
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(field_width, 25.0), egui::Sense::hover());
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
                self.intents.push(SettingsIntent::PickSoundFont);
            }
        });

        if let Some(error) = &self.soundfont_error {
            ui.add_space(2.0);
            ui.label(RichText::new(error.clone()).size(10.5).color(ERROR));
        }
        ui.add_space(2.0);
        // The checkboxes sit on the same line as the buffer
        // field; only the buffer label is above it.
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.add_space(25.0);
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    if ui
                        .checkbox(&mut self.settings.eq_enabled, tr("Equalizer"))
                        .changed()
                    {
                        eq_changed = true;
                    }
                    ui.add_space(44.0);
                    if ui
                        .checkbox(&mut self.settings.midi_effects, tr("MIDI Effects"))
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
                    ui.label(RichText::new(tr("Polyphony")).size(12.0).color(DIM));
                    ui.horizontal(|ui| {
                        let response = ui.add_sized(
                            [64.0, 24.0],
                            egui::DragValue::new(&mut self.settings.max_polyphony)
                                .range(8..=256u32)
                                .speed(8.0),
                        );
                        if response.changed() {
                            buffer_changed = true;
                        }
                        ui.label(RichText::new(tr("voices")).size(11.0).color(DIM));
                    });
                });
                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(tr("SoundFont Buffer")).size(12.0).color(DIM));
                    ui.horizontal(|ui| {
                        let response = ui
                            .add_sized(
                                [58.0, 24.0],
                                egui::DragValue::new(&mut self.settings.sf2_buffer_ms)
                                    .range(0..=100u32)
                                    .speed(1.0),
                            )
                            .on_hover_text(tr("0 = auto, 10–100 ms manual"));
                        if response.changed() {
                            buffer_changed = true;
                        }
                        ui.label(RichText::new("ms").size(11.0).color(DIM));
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

            ui.add_space(28.0);

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

        if eq_changed {
            self.intents.push(SettingsIntent::SendEq);
        }
        if sends_changed {
            self.intents.push(SettingsIntent::SendEffects);
        }
        if buffer_changed {
            self.intents.push(SettingsIntent::RebuildBuffer);
        }
        eq_changed || sends_changed || buffer_changed
    }

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
                changed |= seconds_row(
                    ui,
                    &tr("Search Popup"),
                    &mut self.settings.popup_seconds,
                    60,
                );

                ui.add_space(16.0);
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
                                self.intents.push(SettingsIntent::PickLyricFont);
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
                self.intents.push(SettingsIntent::PickLyricFont);
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

    /// The "MIDI" settings page: output, voice locks and the metronome.

    fn midi_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.add_space(24.0);

        // Layout copied from the design: the output row spans the left column,
        // the buttons sit at the top right and the metronome below them.
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.set_width(344.0);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let left = ui.max_rect().left();
                    pad_to(ui, left, 30.0);
                    ui.label(RichText::new(tr("MIDI Out")).size(13.0));
                    pad_to(ui, left, 258.0);
                    changed |= ui
                        .add(egui::Checkbox::new(
                            &mut self.settings.midi_sysex,
                            RichText::new("MIDI SysEx").size(12.0),
                        ))
                        .changed();
                });

                if !self.midi_ports_loaded {
                    self.intents.push(SettingsIntent::RefreshMidiPorts);
                }
                let selection = self.settings.midi_out_port.clone();
                let selected_text = if selection.is_empty() {
                    tr("RustySynth SoundFont")
                } else {
                    selection.clone()
                };
                let mut chosen: Option<String> = None;
                let mut refresh = false;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(30.0);
                    egui::ComboBox::from_id_salt("midi_out")
                        .width(270.0)
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(selection.is_empty(), tr("RustySynth SoundFont"))
                                .clicked()
                            {
                                chosen = Some(String::new());
                            }
                            for port in &self.midi_ports {
                                if ui.selectable_label(&selection == port, port).clicked() {
                                    chosen = Some(port.clone());
                                }
                            }
                        });
                    ui.add_space(8.0);
                    if ui
                        .add_sized([26.0, 22.0], egui::Button::new(Icon::Repeat2.rich(12.0)))
                        .on_hover_text(tr("Refresh MIDI outputs"))
                        .clicked()
                    {
                        refresh = true;
                    }
                });
                if refresh {
                    self.intents.push(SettingsIntent::RefreshMidiPorts);
                }
                if let Some(port) = chosen {
                    self.intents.push(SettingsIntent::SetMidiOut(port));
                }
                if let Some(error) = &self.midi_error {
                    ui.label(
                        RichText::new(error.clone())
                            .size(11.0)
                            .color(Color32::from_rgb(0xE0, 0x6A, 0x6A)),
                    );
                }

                ui.add_space(48.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                changed |= lock_row(
                    ui,
                    &tr("Lock bass voice"),
                    &mut self.settings.lock_bass,
                    &mut self.settings.lock_bass_program,
                    &BASS_PROGRAMS,
                );
                changed |= lock_row(
                    ui,
                    &tr("Lock drum kit"),
                    &mut self.settings.lock_drum_kit,
                    &mut self.settings.lock_drum_program,
                    &DRUM_KITS,
                );
                changed |= lock_row(
                    ui,
                    &tr("Lock kick"),
                    &mut self.settings.lock_kick,
                    &mut self.settings.lock_kick_note,
                    &KICK_NOTES,
                );
                changed |= lock_row(
                    ui,
                    &tr("Lock snare"),
                    &mut self.settings.lock_snare,
                    &mut self.settings.lock_snare_note,
                    &SNARE_NOTES,
                );
            });

            ui.add_space(26.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.set_width(150.0);
                ui.add_space(9.0);
                ui.horizontal(|ui| {
                    ui.add_space(31.0);
                    if ui
                        .add_sized([125.0, 28.0], egui::Button::new(tr("Reset MIDI")))
                        .clicked()
                    {
                        self.intents.push(SettingsIntent::ResetMidi);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space(31.0);
                    if ui
                        .add_sized([125.0, 28.0], egui::Button::new(tr("Split MIDI")))
                        .clicked()
                    {
                        self.intents.push(SettingsIntent::OpenSplitWindow);
                    }
                });

                ui.add_space(29.0);
                ui.horizontal(|ui| {
                    // The metronome group sits a touch further right.
                    ui.add_space(6.0);
                    ui.label(RichText::new(tr("Metronome")).size(13.0));
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    changed |= ui
                        .add(egui::Checkbox::new(
                            &mut self.settings.metronome,
                            RichText::new(tr("Enable")).size(12.0),
                        ))
                        .changed();
                    ui.add_space(14.0);
                    changed |= ui
                        .add(egui::Checkbox::new(
                            &mut self.settings.metronome_hihat,
                            RichText::new("Hihat").size(12.0),
                        ))
                        .on_hover_text(tr("Use a closed hi-hat (42) for every beat"))
                        .changed();
                });
                ui.add_space(14.0);
                ui.spacing_mut().item_spacing.y = 12.0;
                // The closed hi-hat replaces both beat notes, so the fields are
                // disabled while it is selected.
                ui.add_enabled_ui(!self.settings.metronome_hihat, |ui| {
                    changed |= note_row(ui, &tr("First beat"), &mut self.settings.metronome_first);
                    changed |= note_row(ui, &tr("Other beats"), &mut self.settings.metronome_other);
                });
            });
        });

        if changed {
            self.intents.push(SettingsIntent::SendMidiOptions);
        }
        changed
    }

    /// The Split MIDI window: a channel table where a click or a drag
    /// selects rows so one output can be assigned to a whole group.
    /// Open the Split MIDI window as its own OS viewport.

    fn player_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.add_space(14.0);

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(220.0);
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

            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [116.0, 26.0],
                        egui::Label::new(RichText::new(tr("Change Key")).size(13.0)),
                    );
                    ui.add_space(6.0);
                    let mut mode = self.settings.transpose_mode;
                    egui::ComboBox::from_id_salt("transpose_mode")
                        .width(148.0)
                        .selected_text(transpose_mode_name(mode))
                        .truncate()
                        .show_ui(ui, |ui| {
                            for option in [
                                TransposeMode::All,
                                TransposeMode::Wolf,
                                TransposeMode::Recovery,
                            ] {
                                ui.selectable_value(&mut mode, option, transpose_mode_name(option));
                            }
                        });
                    if mode != self.settings.transpose_mode {
                        self.settings.transpose_mode = mode;
                        self.intents.push(SettingsIntent::SendMidiOptions);
                        changed = true;
                    }
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [116.0, 26.0],
                        egui::Label::new(RichText::new(tr("MIDI Reset System")).size(13.0)),
                    );
                    ui.add_space(6.0);
                    let mut reset = self.settings.midi_reset;
                    egui::ComboBox::from_id_salt("midi_reset")
                        .width(148.0)
                        .selected_text(midi_reset_name(reset))
                        .truncate()
                        .show_ui(ui, |ui| {
                            for option in [
                                MidiReset::Gm,
                                MidiReset::Gs,
                                MidiReset::Xg,
                                MidiReset::Roland,
                                MidiReset::SoundFont,
                            ] {
                                ui.selectable_value(&mut reset, option, midi_reset_name(option));
                            }
                        });
                    if reset != self.settings.midi_reset {
                        self.settings.midi_reset = reset;
                        self.intents.push(SettingsIntent::SendMidiOptions);
                        changed = true;
                    }
                });
            });
        });

        changed
    }

    /// The "Background" settings page: colour picker plus 4:3 / 16:9 previews.
    /// Load the background image when the settings point at a new file.

    fn background_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.add_space(14.0);

        let style = self.settings.background_style;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(18.0);
            // Left: the background type, then the colour or image controls.
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(240.0);
                ui.horizontal(|ui| {
                    // Left-aligned heading, level with the dropdown's box. The
                    // text is painted so it can be nudged onto the box's centre
                    // line: Thai upper vowels push the ink above the line box.
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (heading, _) =
                        ui.allocate_exact_size(egui::vec2(70.0, 28.0), egui::Sense::hover());
                    ui.painter().text(
                        Pos2::new(heading.left(), heading.center().y + 4.0),
                        egui::Align2::LEFT_CENTER,
                        tr("Background"),
                        FontId::proportional(13.0),
                        FG,
                    );
                    let selected = match style {
                        BackgroundStyle::Color => tr("Color"),
                        BackgroundStyle::Default | BackgroundStyle::Image => tr("Image"),
                    };
                    egui::ComboBox::from_id_salt("background_style")
                        .width(150.0)
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            // Only two choices: the image (shipped artwork until
                            // the user picks a file) or a flat colour.
                            for option in [BackgroundStyle::Image, BackgroundStyle::Color] {
                                let label = match option {
                                    BackgroundStyle::Color => tr("Color"),
                                    BackgroundStyle::Default | BackgroundStyle::Image => {
                                        tr("Image")
                                    }
                                };
                                if ui
                                    .selectable_value(
                                        &mut self.settings.background_style,
                                        option,
                                        label,
                                    )
                                    .clicked()
                                {
                                    changed = true;
                                }
                            }
                        });
                });
                ui.add_space(14.0);
                if style == BackgroundStyle::Color {
                    ui.label(RichText::new(tr("Background Color")).size(13.0));
                    ui.add_space(6.0);
                    changed |= background_color_row(ui, &mut self.settings.background_color);
                } else {
                    ui.label(RichText::new(tr("Background image")).size(13.0));
                    ui.add_space(6.0);
                    let has_image = self.settings.background_image.is_some();
                    ui.horizontal(|ui| {
                        if ui
                            .add_sized([120.0, 24.0], egui::Button::new(tr("Choose image\u{2026}")))
                            .clicked()
                        {
                            self.intents.push(SettingsIntent::PickBackgroundImage);
                        }
                        // The delete button stays visible but locked until the
                        // user has chosen a picture of their own.
                        if ui
                            .add_enabled(
                                has_image,
                                egui::Button::new(tr("Remove")).min_size(egui::vec2(60.0, 24.0)),
                            )
                            .clicked()
                        {
                            self.settings.background_image = None;
                            self.background_texture = None;
                            self.background_error = None;
                            changed = true;
                        }
                    });
                    ui.add_space(4.0);
                    let name = self
                        .settings
                        .background_image
                        .as_deref()
                        .map(|path| {
                            path.file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string())
                        })
                        .unwrap_or_else(|| tr("Shipped with the player"));
                    ui.label(RichText::new(name).size(11.0).color(DIM));
                    if let Some(error) = &self.background_error {
                        ui.label(RichText::new(error.clone()).size(10.5).color(ERROR));
                    }
                }
            });
            ui.add_space(8.0);
            // Right: the monitors sit at the top, beside the dropdown.
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.set_width(268.0);
                ui.label(RichText::new(tr("Preview monitor")).size(13.0));
                ui.add_space(6.0);
                let texture = match style {
                    BackgroundStyle::Color => None,
                    BackgroundStyle::Default | BackgroundStyle::Image => self
                        .background_texture
                        .as_ref()
                        .map(|(_, texture)| texture)
                        .or(self.default_background.as_ref()),
                };
                background_preview(
                    ui,
                    color_or(&self.settings.background_color, [0, 0, 0]),
                    texture,
                );
            });
            ui.add_space(24.0);
        });

        changed
    }

    fn about_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(22.0);
        let page_rect = ui.max_rect();
        let button_top = ui.cursor().top();

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(20.0);
            let page_width = page_rect.width() - 20.0;

            ui.allocate_ui_with_layout(
                egui::vec2(120.0, 190.0),
                Layout::top_down(Align::Min),
                |ui| {
                    let texture = self.about_logo.get_or_insert_with(|| {
                        let icon = eframe::icon_data::from_png_bytes(include_bytes!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/../../logo/DontreeCrab_icon.png"
                        )))
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
                        .inner_margin(egui::Margin::same(0))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Image::new((texture.id(), egui::vec2(104.0, 104.0)))
                                    .corner_radius(8),
                            );
                        });
                    ui.add_space(0.0);
                    about_github_link(ui);
                },
            );

            ui.add_space(16.0);

            ui.allocate_ui_with_layout(
                egui::vec2(page_width - 260.0, 190.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.label(RichText::new("DontreeCrab").size(18.0).strong());
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
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(format!("{} :", tr("Hardware")))
                            .size(13.0)
                            .strong(),
                    );
                    let os = match std::env::consts::OS {
                        "windows" => "Windows PC",
                        "linux" => "Linux PC",
                        "macos" => "macOS",
                        other => other,
                    };
                    ui.label(
                        RichText::new(format!(
                            "{os} · {} · 44.1 kHz / 2 ch / f32",
                            std::env::consts::ARCH
                        ))
                        .size(13.0),
                    );
                },
            );
        });

        let license_rect = Rect::from_min_size(
            Pos2::new(page_rect.right() - 132.0, button_top),
            egui::vec2(120.0, 28.0),
        );
        if ui
            .put(license_rect, egui::Button::new(tr("License")))
            .clicked()
        {
            self.about_popup = Some(AboutPopup::License);
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
        if ui
            .put(
                license_rect.translate(egui::vec2(0.0, 36.0)),
                egui::Button::new(tr("Created")),
            )
            .clicked()
        {
            self.about_popup = Some(AboutPopup::Created);
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    }

    // -------------------------------------------------------------- transport

    /// Play/pause the loaded song. With nothing loaded (right after Stop) it
    /// starts the playlist entry the operator expects instead of doing
    /// nothing.

    fn song_data_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.add_space(20.0);
        ui.horizontal_top(|ui| {
            ui.add_space(18.0);
            let width = (ui.available_width() - 18.0).max(300.0);
            ui.allocate_ui_with_layout(
                egui::vec2(width, ui.available_height()),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(width);
                    changed |= self.song_data_content(ui);
                },
            );
        });
        changed
    }

    fn song_data_content(&mut self, ui: &mut egui::Ui) -> bool {
        // Folder changes go through dialogs, so nothing changes inline here.
        let changed = false;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if ui
                .add_sized(
                    [110.0, 28.0],
                    egui::Button::new(RichText::new(tr("Song Data")).strong())
                        .selected(true)
                        .fill(Color32::from_gray(32)),
                )
                .clicked()
            {
                self.intents.push(SettingsIntent::OpenLibrary);
            }
        });

        ui.add_space(10.0);

        let table_width = (ui.available_width() - 2.0).max(200.0);
        let type_width = 110.0f32;
        let header_height = 30.0f32;
        let row_height = 30.0f32;
        let rows = 4usize;
        let (table_rect, _) = ui.allocate_exact_size(
            egui::vec2(table_width, header_height + row_height * rows as f32),
            egui::Sense::hover(),
        );

        let painter = ui.painter_at(table_rect);
        painter.rect_filled(table_rect, 0.0, Color32::from_gray(14));
        let header_rect = Rect::from_min_size(
            table_rect.min,
            egui::vec2(table_rect.width(), header_height),
        );
        painter.rect_filled(header_rect, 0.0, Color32::from_gray(34));

        let line = egui::Stroke::new(1.0, Color32::from_gray(80));
        let divider_x = table_rect.left() + type_width;
        painter.line_segment(
            [
                Pos2::new(table_rect.left(), header_rect.bottom()),
                Pos2::new(table_rect.right(), header_rect.bottom()),
            ],
            line,
        );
        for row in 1..rows {
            let y = table_rect.top() + header_height + row as f32 * row_height;
            painter.line_segment(
                [
                    Pos2::new(table_rect.left(), y),
                    Pos2::new(table_rect.right(), y),
                ],
                line,
            );
        }
        painter.line_segment(
            [
                Pos2::new(divider_x, table_rect.top()),
                Pos2::new(divider_x, table_rect.bottom()),
            ],
            line,
        );
        painter.rect_stroke(
            table_rect,
            0.0,
            egui::Stroke::new(1.0, Color32::from_gray(90)),
            egui::StrokeKind::Inside,
        );

        painter.text(
            Pos2::new(table_rect.left() + 6.0, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            tr("File type"),
            FontId::proportional(11.0),
            FG,
        );
        painter.text(
            Pos2::new(divider_x + 6.0, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            tr("Folder path"),
            FontId::proportional(11.0),
            FG,
        );

        let entries = [
            (
                tr("NCN MIDI"),
                self.settings.ncn_folder.as_deref(),
                DialogKind::NcnFolder,
            ),
            (
                tr("KMID MIDI"),
                self.settings.kmid_folder.as_deref(),
                DialogKind::KmidFolder,
            ),
            (
                tr("KAR MIDI"),
                self.settings.kar_folder.as_deref(),
                DialogKind::KarFolder,
            ),
            (
                tr("MKC MIDI"),
                self.settings.mkc_folder.as_deref(),
                DialogKind::MkcFolder,
            ),
        ];
        let mut browse: Option<DialogKind> = None;
        for (index, (label, path, kind)) in entries.iter().enumerate() {
            let row_top = table_rect.top() + header_height + index as f32 * row_height;
            let row_rect = Rect::from_min_size(
                Pos2::new(table_rect.left(), row_top),
                egui::vec2(table_rect.width(), row_height),
            );
            painter.text(
                Pos2::new(row_rect.left() + 6.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.5),
                FG,
            );

            let button_width = 38.0f32;
            let field_x = divider_x + 6.0;
            let field_right = row_rect.right() - 8.0 - button_width - 8.0;
            let field_width = (field_right - field_x).max(80.0);
            let field_rect = Rect::from_min_size(
                Pos2::new(field_x, row_rect.center().y - 10.0),
                egui::vec2(field_width, 20.0),
            );
            let button_rect = Rect::from_min_size(
                Pos2::new(
                    row_rect.right() - 8.0 - button_width,
                    row_rect.center().y - 10.0,
                ),
                egui::vec2(button_width, 20.0),
            );

            let text = path
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| tr("No folder selected"));
            let full = text.clone();
            painter.rect_filled(field_rect, 4.0, Color32::from_gray(26));
            painter.rect_stroke(
                field_rect,
                4.0,
                egui::Stroke::new(1.0, Color32::from_gray(64)),
                egui::StrokeKind::Inside,
            );
            painter
                .with_clip_rect(field_rect.shrink2(egui::vec2(6.0, 0.0)))
                .text(
                    Pos2::new(field_rect.left() + 6.0, field_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &text,
                    FontId::proportional(11.5),
                    if path.is_some() { FG } else { DIM },
                );
            ui.interact(
                field_rect,
                ui.scope_id().with(("song_folder_path", index)),
                egui::Sense::hover(),
            )
            .on_hover_text(full);

            let response = ui.interact(
                button_rect,
                ui.scope_id().with(("song_folder_browse", index)),
                egui::Sense::click(),
            );
            let fill = if response.hovered() {
                Color32::from_gray(56)
            } else {
                Color32::from_gray(34)
            };
            painter.rect_filled(button_rect, 4.0, fill);
            painter.rect_stroke(
                button_rect,
                4.0,
                egui::Stroke::new(1.0, Color32::from_gray(90)),
                egui::StrokeKind::Inside,
            );
            painter.text(
                button_rect.center(),
                egui::Align2::CENTER_CENTER,
                "...",
                FontId::proportional(12.0),
                FG,
            );
            if response.clicked() {
                browse = Some(*kind);
            }
        }
        if let Some(kind) = browse {
            self.intents.push(SettingsIntent::PickFolder(kind));
        }

        changed
    }
}

impl eframe::App for KaraokeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(minimized) = ctx.input(|i| i.viewport().minimized)
            && minimized != self.main_minimized
        {
            self.main_minimized = minimized;
            for id in child_viewports() {
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(minimized));
            }
        }
        self.sync_background_image(&ctx);
        self.poll_missing_check();
        self.poll_library_load();
        self.poll_library_op();
        if self.startup_active {
            self.poll_startup(&ctx);
            if self.startup_active {
                // Centre the borderless splash once, then let it paint.
                if !self.splash_centered {
                    if let Some(cmd) = egui::ViewportCommand::center_on_screen(&ctx) {
                        ctx.send_viewport_cmd(cmd);
                        self.splash_centered = true;
                    }
                }
                self.draw_splash(ui);
                ctx.request_repaint();
                return;
            }

            // Startup finished: turn the splash back into the main window.
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                1024.0, 640.0,
            )));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1024.0, 640.0)));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.center_main_pending = 5;
            if self.settings.fullscreen {
                // Apply fullscreen only after the normal-size window exists, so
                // leaving fullscreen later restores 1024x640 instead of the
                // splash size.
                self.fullscreen_pending = 60;
            }
            // Let the window manager apply the new size before painting the
            // main interface, so it never flashes inside the small splash.
            ctx.request_repaint();
            return;
        }
        if self.center_main_pending > 0 {
            self.center_main_pending -= 1;
            let wide = ctx.input(|i| {
                i.viewport()
                    .inner_rect
                    .map(|rect| rect.width() >= 900.0)
                    .unwrap_or(false)
            });
            if wide || self.center_main_pending == 0 {
                if let Some(cmd) = egui::ViewportCommand::center_on_screen(&ctx) {
                    ctx.send_viewport_cmd(cmd);
                }
                self.center_main_pending = 0;
            } else {
                ctx.request_repaint();
            }
        }
        if self.fullscreen_pending > 0 {
            self.fullscreen_pending -= 1;
            let wide = ctx.input(|i| {
                i.viewport()
                    .inner_rect
                    .map(|rect| rect.width() >= 900.0)
                    .unwrap_or(false)
            });
            if wide || self.fullscreen_pending == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                self.fullscreen_pending = 0;
            } else {
                ctx.request_repaint();
            }
        }
        // Start the song card timer on the first frame after a song opens.
        if self.song_card_pending {
            self.song_card_pending = false;
            if !self.settings.show_song_info
                && !self.settings.show_playlist
                && !self.lyric_overlay_visible
            {
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
        if ctx.input(|i| i.viewport().close_requested())
            && !self.exit_confirmed.load(Ordering::Relaxed)
        {
            if self.settings.confirm_exit {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.exit_confirm_open.store(true, Ordering::Relaxed);
            } else {
                self.exit_confirmed.store(true, Ordering::Relaxed);
            }
        }
        self.update_lyric_overlay(&ctx);
        // While the settings dialog or the exit confirmation is open it owns
        // the keyboard.
        if !self.settings_open
            && !self.exit_confirm_open.load(Ordering::Relaxed)
            && self.menu.is_none()
        {
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

        if !self.startup_volume_shown {
            self.startup_volume_shown = true;
            let percent = (self.volume * 100.0).round() as i32;
            self.show_indicator(
                tr_args("VOL [{}]", &[&percent as &dyn std::fmt::Display]),
                &ctx,
            );
        }
        self.top_bar(ui);
        // The transport bar is either shown or hidden; the context menu is the
        // only way in or out, so it never pops up on its own.
        if !self.settings.hide_transport {
            self.bottom_bar(ui);
        }
        self.lyric_area = ui.available_rect_before_wrap();
        if self.settings.show_playlist {
            self.playlist_panel(ui);
        }
        self.central(ui);
        if self.lyric_overlay_visible {
            self.lyric_search_overlay(&ctx);
        }
        self.midi_controller_panel(&ctx);
        self.draw_menu(&ctx);

        self.draw_settings_window(&ctx);
        self.poll_settings_window(&ctx);
        if self.song_restore_confirm.load(Ordering::Relaxed) {
            self.draw_song_restore_confirm(&ctx);
        }
        if self.song_restore_confirmed.swap(false, Ordering::Relaxed) {
            self.apply_song_data_reset(&ctx);
        }
        self.draw_about_popup_window(&ctx);

        self.draw_split_window(&ctx);
        self.poll_split_window(&ctx);

        self.draw_midi_editor_window(&ctx);
        self.draw_midi_editor_popup_window(&ctx);
        self.draw_midi_editor_confirm_window(&ctx);
        self.draw_midi_editor_save_as_window(&ctx);
        self.poll_midi_editor_window(&ctx);

        self.draw_library_window(&ctx);
        self.poll_library_window(&ctx);

        self.draw_import_window(&ctx);
        self.poll_import_window(&ctx);

        if self.exit_confirm_open.load(Ordering::Relaxed) {
            self.draw_exit_confirm(&ctx);
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
        Some("kar") | Some("mid") | Some("midi") | Some("mkc")
    )
}

/// Best-effort file type for playlist export (`kar`, `ncn`, `midi`).
/// Draw one coloured segment of a playlist row and advance the pen.
fn draw_playlist_segment(
    painter: &egui::Painter,
    x: &mut f32,
    y: f32,
    text: &str,
    font: &FontId,
    color: Color32,
    gap: f32,
) {
    if text.is_empty() {
        return;
    }
    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
    let width = galley.size().x;
    let height = galley.size().y;
    painter.galley(Pos2::new(*x, y - height * 0.5), galley, color);
    *x += width + gap;
}

/// How much the fixed-size overlays should shrink to fit the window.
fn window_fit_scale(ctx: &egui::Context) -> f32 {
    let logical = ctx.content_rect().size() * ctx.zoom_factor();
    (logical.x / 1366.0).min(logical.y / 768.0).clamp(0.70, 1.0)
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn file_type_guess(path: &Path) -> String {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match extension.as_str() {
        "kar" => "kar".to_owned(),
        "mkc" => "mkc".to_owned(),
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
    let (rect, response) = ui.allocate_exact_size(egui::vec2(104.0, 24.0), egui::Sense::click());
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

/// Shown below the MIT licence text in About -> License.
const USAGE_NOTICE: &str = "DontreeCrab is only a MIDI player, intended for MIDI content that you create yourself or are otherwise entitled to use.\n\nWe do not support or accept any use of this codebase for reading, converting, playing, copying, or supporting commercial karaoke formats that are copy-protected or restricted by law, such as EMK & XMK (eXtreme Karaoke), SIB & SIBKL (SIBSIB Karaoke) and SMK & SMKL & SMD (Sonic Karaoke), or any program that decrypts or bypasses file protection to play, convert, copy or distribute song libraries without permission from the copyright owners.\n\nIssues and pull requests requesting such support will be rejected immediately. Anyone who uses or modifies this software for those purposes is solely responsible for their own actions; the developers and contributors accept no liability.\n\nThis software is provided \"as is\", without warranty, as stated in the MIT License. This notice is not legal advice; please check the laws in your country.";

fn reflow_license(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut paragraph = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !paragraph.is_empty() {
                result.push_str(&paragraph);
                result.push_str("\n\n");
                paragraph.clear();
            }
        } else {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(line);
        }
    }
    if !paragraph.is_empty() {
        result.push_str(&paragraph);
    }
    result
}

fn draw_license(ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .id_salt("about_license_text")
        .max_height(200.0)
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(reflow_license(LICENSE_TEXT))
                        .monospace()
                        .size(10.5),
                )
                .wrap(),
            );
        });
    ui.add_space(10.0);
    egui::Frame::NONE
        .fill(Color32::from_rgb(26, 23, 14))
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(120, 92, 30)))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(
                RichText::new(tr("Important notice"))
                    .size(13.0)
                    .strong()
                    .color(Color32::from_rgb(255, 190, 90)),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("about_license_notice")
                .max_height(190.0)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(tr(USAGE_NOTICE)).size(11.0)).wrap());
                });
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
    let provider_terms = tr("Provider terms");
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "DeepSeek V4.1 Flash 1017",
            tr("AI coding assistant"),
            &provider_terms,
        ),
        ("OpenCode 2.2.7", tr("Development environment"), "MIT"),
        ("Rust", tr("Programming language"), "MIT / Apache-2.0"),
        ("anyhow", tr("Error handling"), "MIT / Apache-2.0"),
        ("eframe / egui", tr("GUI and rendering"), "MIT / Apache-2.0"),
        ("image", tr("Image decoding"), "MIT / Apache-2.0"),
        ("cpal", tr("Audio output"), "Apache-2.0"),
        ("rustysynth", tr("SoundFont synthesis"), "MIT"),
        ("hound", tr("WAV rendering"), "Apache-2.0"),
        ("midir", tr("MIDI ports"), "MIT"),
        ("midly", tr("MIDI parsing"), "Unlicense"),
        (
            "serde / serde_json",
            tr("Configuration and JSON"),
            "MIT / Apache-2.0",
        ),
        ("flate2", tr("KMID decompression"), "MIT / Apache-2.0"),
        ("roxmltree", tr("KMID XML parsing"), "MIT / Apache-2.0"),
        (
            "encoding_rs",
            tr("Text encoding"),
            "Apache-2.0 / MIT + BSD-3",
        ),
        ("rusqlite", tr("Song database"), "MIT"),
        ("SQLite", tr("Embedded database"), "Public Domain"),
        ("clap", tr("Command-line interface"), "MIT / Apache-2.0"),
        ("rfd", tr("Native file dialogs"), "MIT"),
        ("directories", tr("Config paths"), "MIT / Apache-2.0"),
        ("windows-sys", tr("Windows API"), "MIT / Apache-2.0"),
        ("winreg", tr("Windows registry"), "MIT"),
        ("winresource", tr("Windows resources"), "MIT"),
        ("Sarabun", tr("UI / lyric font"), "OFL-1.1"),
        ("E1234", tr("Clock font"), "OFL-1.1"),
        ("Lucide", tr("Icons"), "ISC"),
        (
            "egui default fonts",
            tr("UI fallback fonts"),
            "MIT / OFL-1.1 / UFL-1.0",
        ),
    ];

    egui::ScrollArea::vertical()
        .id_salt("about_created_grid_scroll")
        .max_height(395.0)
        .show(ui, |ui| {
            let available = ui.available_width();
            egui::Grid::new("about_created_grid")
                .num_columns(3)
                .spacing([16.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    let tool_width = 165.0;
                    let license_width = 175.0;
                    let purpose_width = (available - tool_width - license_width - 32.0).max(150.0);

                    ui.add_sized(
                        [tool_width, 18.0],
                        egui::Label::new(RichText::new(tr("Tool")).size(11.0).strong())
                            .halign(Align::LEFT),
                    );
                    ui.add_sized(
                        [purpose_width, 18.0],
                        egui::Label::new(RichText::new(tr("Used for")).size(11.0).strong())
                            .halign(Align::LEFT),
                    );
                    ui.add_sized(
                        [license_width, 18.0],
                        egui::Label::new(RichText::new(tr("License")).size(11.0).strong())
                            .halign(Align::LEFT),
                    );
                    ui.end_row();

                    for (name, purpose, license) in rows {
                        ui.add_sized(
                            [tool_width, 18.0],
                            egui::Label::new(RichText::new(name).size(11.5)).halign(Align::LEFT),
                        );
                        ui.add_sized(
                            [purpose_width, 18.0],
                            egui::Label::new(RichText::new(purpose).size(11.0).color(DIM))
                                .halign(Align::LEFT),
                        );
                        ui.add_sized(
                            [license_width, 18.0],
                            egui::Label::new(RichText::new(license).size(11.0).color(DIM))
                                .halign(Align::LEFT),
                        );
                        ui.end_row();
                    }
                });
        });
    ui.add_space(8.0);
    ui.separator();
    ui.label(
        RichText::new(tr("Application license: MIT"))
            .size(11.0)
            .color(DIM),
    );
    ui.label(
        RichText::new(tr("No telemetry. Works offline."))
            .size(11.0)
            .color(DIM),
    );
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
/// Contents of the "Add a song" window.
fn import_window_ui(ui: &mut egui::Ui, w: &mut ImportWindow) {
    // Lock the content width: the list must not pull the window wider when
    // the scrollbar appears or a long line is drawn.
    let width = ui.available_width();
    ui.set_width(width);
    let busy = w.busy;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy, egui::Button::new(tr("Open")))
            .clicked()
        {
            w.intents.push(ImportIntent::OpenFolder);
        }
        let selected_visible = w
            .view
            .iter()
            .filter(|&&index| w.items.get(index).is_some_and(|item| item.selected))
            .count();
        if ui
            .add_enabled(!busy && selected_visible > 0, egui::Button::new(tr("Add")))
            .clicked()
        {
            w.intents.push(ImportIntent::Add);
        }

        let all_selected = !w.view.is_empty()
            && w.view
                .iter()
                .all(|&index| w.items.get(index).is_some_and(|item| item.selected));
        let mut select_all = all_selected;
        if ui
            .add_enabled(
                !busy && !w.view.is_empty(),
                egui::Checkbox::new(&mut select_all, tr("Select all")),
            )
            .changed()
        {
            w.intents.push(ImportIntent::SelectAll(select_all));
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let before = w.filter;
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("import_filter")
                    .width(110.0)
                    .selected_text(w.filter.label())
                    .show_ui(ui, |ui| {
                        for filter in [
                            LibraryFilter::All,
                            LibraryFilter::Ncn,
                            LibraryFilter::Kmid,
                            LibraryFilter::Kar,
                            LibraryFilter::Mkc,
                        ] {
                            ui.selectable_value(&mut w.filter, filter, filter.label());
                        }
                    });
            });
            if w.filter != before {
                w.intents.push(ImportIntent::Filter(w.filter));
            }
            ui.label(tr("File type"));
        });
    });
    ui.add_space(6.0);
    import_list_ui(ui, w, busy, width);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let progress = w.progress.clamp(0.0, 1.0);
        let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(300.0, 20.0), egui::Sense::hover());
        let painter = ui.painter_at(bar_rect);
        painter.rect_filled(bar_rect, 6.0, Color32::from_gray(30));
        if progress > 0.0 {
            let fill_rect = Rect::from_min_size(
                bar_rect.min,
                egui::vec2(bar_rect.width() * progress, bar_rect.height()),
            );
            painter.rect_filled(fill_rect, 6.0, Color32::from_gray(90));
        }
        painter.text(
            bar_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{:.0}%", progress * 100.0),
            FontId::proportional(12.0),
            FG,
        );
        if let Some(error) = &w.error {
            ui.label(RichText::new(error).size(10.5).color(ERROR));
        } else if let Some(note) = &w.note {
            ui.label(RichText::new(note).size(10.5).color(DIM));
        }
    });
}

/// The scan result list: one checkbox and one line per song.
fn import_list_ui(ui: &mut egui::Ui, w: &mut ImportWindow, busy: bool, width: f32) {
    let row_height = 24.0f32;
    if w.view.is_empty() {
        let (empty, _) = ui.allocate_exact_size(egui::vec2(width, 210.0), egui::Sense::hover());
        let painter = ui.painter_at(empty);
        painter.rect_filled(empty, 0.0, Color32::from_gray(14));
        painter.rect_stroke(
            empty,
            0.0,
            egui::Stroke::new(1.0, Color32::from_gray(90)),
            egui::StrokeKind::Inside,
        );
        let message = if w.items.is_empty() {
            tr("Open a folder to add songs.")
        } else {
            tr("No songs match the filter.")
        };
        painter.text(
            empty.center(),
            egui::Align2::CENTER_CENTER,
            message,
            FontId::proportional(12.0),
            DIM,
        );
        return;
    }

    egui::Frame::NONE
        .fill(Color32::from_gray(14))
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(90)))
        .show(ui, |ui| {
            ui.set_min_height(210.0);
            ui.set_min_width(width);
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical()
                .max_height(210.0)
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show_rows(ui, row_height, w.view.len(), |ui, range| {
                    let left = ui.max_rect().left();
                    let top = ui.max_rect().top();
                    let width = ui.available_width();
                    for (offset, view_index) in range.clone().enumerate() {
                        let Some(&item_index) = w.view.get(view_index) else {
                            continue;
                        };
                        let Some(item) = w.items.get(item_index) else {
                            continue;
                        };
                        let row = Rect::from_min_size(
                            Pos2::new(left, top + offset as f32 * row_height),
                            egui::vec2(width, row_height),
                        );
                        let painter = ui.painter_at(row);
                        let fill = if view_index % 2 == 0 {
                            Color32::from_gray(14)
                        } else {
                            Color32::from_gray(11)
                        };
                        painter.rect_filled(row, 0.0, fill);
                        painter.line_segment(
                            [
                                Pos2::new(row.left(), row.bottom()),
                                Pos2::new(row.right(), row.bottom()),
                            ],
                            egui::Stroke::new(1.0, Color32::from_gray(40)),
                        );

                        let checkbox = Rect::from_center_size(
                            Pos2::new(row.left() + 16.0, row.center().y),
                            egui::vec2(16.0, 16.0),
                        );
                        let response = ui.interact(
                            checkbox,
                            ui.scope_id().with(("import_select", item_index)),
                            egui::Sense::click(),
                        );
                        if response.clicked() && !busy {
                            w.intents
                                .push(ImportIntent::Selected(item_index, !item.selected));
                        }
                        painter.rect_filled(checkbox, 3.0, Color32::from_gray(28));
                        painter.rect_stroke(
                            checkbox,
                            3.0,
                            egui::Stroke::new(1.0, Color32::from_gray(110)),
                            egui::StrokeKind::Inside,
                        );
                        if item.selected {
                            painter.line_segment(
                                [
                                    Pos2::new(checkbox.left() + 3.0, checkbox.center().y),
                                    Pos2::new(checkbox.center().x - 1.0, checkbox.bottom() - 4.0),
                                ],
                                egui::Stroke::new(2.0, FG),
                            );
                            painter.line_segment(
                                [
                                    Pos2::new(checkbox.center().x - 1.0, checkbox.bottom() - 4.0),
                                    Pos2::new(checkbox.right() - 3.0, checkbox.top() + 4.0),
                                ],
                                egui::Stroke::new(2.0, FG),
                            );
                        }

                        let key = item.record.key.as_deref().unwrap_or("C");
                        let text = format!(
                            "{} : {} {} ({}) {}",
                            view_index + 1,
                            item.record.code,
                            item.record.title,
                            key,
                            item.record.artist
                        );
                        painter
                            .with_clip_rect(Rect::from_min_max(
                                Pos2::new(row.left() + 30.0, row.top()),
                                Pos2::new(row.right() - 4.0, row.bottom()),
                            ))
                            .text(
                                Pos2::new(row.left() + 30.0, row.center().y),
                                egui::Align2::LEFT_CENTER,
                                text,
                                FontId::proportional(11.5),
                                FG,
                            );
                    }
                });
        });
}

/// Contents of the Song Data window.
fn library_window_ui(ui: &mut egui::Ui, w: &mut LibraryWindow) {
    ui.set_width(ui.available_width());
    // Ctrl+A selects every visible row unless the search box has focus.
    if !ui.ctx().egui_wants_keyboard_input()
        && ui
            .ctx()
            .input(|i| (i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::A))
    {
        w.selected = w.view.iter().copied().collect();
        w.selection_anchor = None;
    }
    // The search sits above the buttons, inset from the frame.
    ui.horizontal(|ui| {
        let width = ui.available_width();
        let response = ui.add_sized(
            [width, 24.0],
            egui::TextEdit::singleline(&mut w.query)
                .id_salt("library_query_input")
                .hint_text(tr("Code, song or artist"))
                .font(FontId::proportional(13.0)),
        );
        if w.focus_query {
            response.request_focus();
            w.focus_query = false;
        }
        if response.changed() {
            w.intents.push(LibraryIntent::QueryChanged);
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let busy = w.busy;
        if ui
            .add_enabled(!busy, egui::Button::new(tr("Add a song")))
            .clicked()
        {
            w.intents.push(LibraryIntent::AddSong);
        }
        if ui
            .add_enabled(!busy, egui::Button::new(tr("Check missing")))
            .on_hover_text(tr("Check missing files"))
            .clicked()
        {
            w.intents.push(LibraryIntent::CheckMissing);
        }
        if ui
            .add_enabled(!busy, egui::Button::new(tr("Compact")))
            .on_hover_text(tr("Rebuild the database file and reclaim space"))
            .clicked()
        {
            w.intents.push(LibraryIntent::Compact);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_enabled(!busy, egui::Button::new(tr("Create a database")))
                .clicked()
            {
                w.intents.push(LibraryIntent::Create);
            }
            let filter_before = w.filter;
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("library_filter")
                    .width(110.0)
                    .selected_text(w.filter.label())
                    .show_ui(ui, |ui| {
                        for filter in [
                            LibraryFilter::All,
                            LibraryFilter::Ncn,
                            LibraryFilter::Kmid,
                            LibraryFilter::Kar,
                            LibraryFilter::Mkc,
                        ] {
                            ui.selectable_value(&mut w.filter, filter, filter.label());
                        }
                    });
            });
            if w.filter != filter_before {
                w.intents.push(LibraryIntent::FilterChanged);
            }
            ui.label(tr("File type"));
        });
    });
    ui.add_space(6.0);
    library_table_ui(ui, w);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let busy = w.busy;
        if ui
            .add_enabled(!busy, egui::Button::new(tr("Backup")))
            .on_hover_text(tr("Save a copy of the database to the backups folder"))
            .clicked()
        {
            w.intents.push(LibraryIntent::Backup);
        }
        let progress = w.progress.clamp(0.0, 1.0);
        let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(300.0, 20.0), egui::Sense::hover());
        let painter = ui.painter_at(bar_rect);
        painter.rect_filled(bar_rect, 6.0, Color32::from_gray(30));
        if progress > 0.0 {
            let fill_rect = Rect::from_min_size(
                bar_rect.min,
                egui::vec2(bar_rect.width() * progress, bar_rect.height()),
            );
            painter.rect_filled(fill_rect, 6.0, Color32::from_gray(90));
        }
        painter.text(
            bar_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{:.0}%", progress * 100.0),
            FontId::proportional(12.0),
            FG,
        );
        if let Some(error) = &w.error {
            ui.label(RichText::new(error).size(10.5).color(ERROR));
        } else if let Some(label) = &w.busy_label {
            ui.label(RichText::new(label.clone()).size(10.5).color(DIM));
        } else if let Some(note) = &w.note {
            ui.label(RichText::new(note).size(10.5).color(DIM));
        }
    });
}

/// The song table: three columns of metadata per row, read-only.
fn library_table_ui(ui: &mut egui::Ui, w: &mut LibraryWindow) {
    let songs = Arc::clone(&w.songs);
    let view = Arc::clone(&w.view);
    let loading = w.loading;
    let width = ui.available_width();
    let header_height = 24.0f32;
    let row_height = 22.0f32;
    let code_width = 80.0f32;
    let key_width = 60.0f32;
    let type_width = 60.0f32;
    let remaining = (width - code_width - key_width - type_width).max(200.0);
    let song_width = remaining * 0.62;
    let artist_width = remaining - song_width;
    let columns = [code_width, song_width, artist_width, key_width, type_width];

    let (header, _) =
        ui.allocate_exact_size(egui::vec2(width, header_height), egui::Sense::hover());
    let painter = ui.painter_at(header);
    let line = egui::Stroke::new(1.0, Color32::from_gray(80));
    painter.rect_filled(header, 0.0, Color32::from_gray(34));
    painter.rect_stroke(
        header,
        0.0,
        egui::Stroke::new(1.0, Color32::from_gray(90)),
        egui::StrokeKind::Inside,
    );
    let mut x = header.left();
    for (index, column) in columns.iter().enumerate() {
        x += *column;
        if index + 1 < columns.len() {
            painter.line_segment(
                [Pos2::new(x, header.top()), Pos2::new(x, header.bottom())],
                line,
            );
        }
    }
    let labels = [tr("Code"), tr("Song"), tr("Artist"), tr("Key"), tr("Type")];
    let mut x = header.left();
    for (label, column) in labels.iter().zip(columns) {
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(x, header.top()),
                egui::vec2(column, header_height),
            ))
            .text(
                Pos2::new(x + 4.0, header.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                FG,
            );
        x += column;
    }

    if view.is_empty() {
        let (empty, _) = ui.allocate_exact_size(egui::vec2(width, 210.0), egui::Sense::hover());
        let painter = ui.painter_at(empty);
        painter.rect_filled(empty, 0.0, Color32::from_gray(14));
        painter.rect_stroke(
            empty,
            0.0,
            egui::Stroke::new(1.0, Color32::from_gray(90)),
            egui::StrokeKind::Inside,
        );
        let message = if loading {
            "Loading the library..."
        } else if songs.is_empty() {
            "No songs yet. Press Create a database."
        } else {
            "No songs match your search."
        };
        painter.text(
            empty.center(),
            egui::Align2::CENTER_CENTER,
            tr(message),
            FontId::proportional(12.0),
            DIM,
        );
        return;
    }

    egui::Frame::NONE
        .fill(Color32::from_gray(14))
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(90)))
        .show(ui, |ui| {
            ui.set_min_height(210.0);
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical()
                .max_height(210.0)
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show_rows(ui, row_height, view.len(), |ui, range| {
                    let left = ui.max_rect().left();
                    let top = ui.max_rect().top();
                    let width = ui.available_width();
                    for (offset, position) in range.clone().enumerate() {
                        let Some(&song_index) = view.get(position) else {
                            continue;
                        };
                        let Some(song) = songs.get(song_index) else {
                            continue;
                        };
                        let row = Rect::from_min_size(
                            Pos2::new(left, top + offset as f32 * row_height),
                            egui::vec2(width, row_height),
                        );
                        let response = ui
                            .interact(
                                row,
                                ui.scope_id().with(("library_row", song_index)),
                                egui::Sense::click(),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        let selected = w.selected.contains(&song_index);
                        let painter = ui.painter_at(row);
                        let fill = if selected {
                            Color32::from_gray(48)
                        } else if response.hovered() {
                            Color32::from_gray(22)
                        } else if position % 2 == 0 {
                            Color32::from_gray(14)
                        } else {
                            Color32::from_gray(11)
                        };
                        painter.rect_filled(row, 0.0, fill);
                        painter.line_segment(
                            [
                                Pos2::new(row.left(), row.bottom()),
                                Pos2::new(row.right(), row.bottom()),
                            ],
                            line,
                        );
                        let mut x = row.left();
                        for (column_index, column) in columns.iter().enumerate() {
                            painter.line_segment(
                                [Pos2::new(x, row.top()), Pos2::new(x, row.bottom())],
                                line,
                            );
                            if column_index + 1 == columns.len() {
                                painter.line_segment(
                                    [
                                        Pos2::new(row.right(), row.top()),
                                        Pos2::new(row.right(), row.bottom()),
                                    ],
                                    line,
                                );
                            }
                            x += *column;
                        }

                        let code = if song.missing {
                            format!("! {}", song.code)
                        } else {
                            song.code.clone()
                        };
                        let cells = [
                            (code, if song.missing { ERROR } else { FG }),
                            (song.title.clone(), if song.missing { DIM } else { FG }),
                            (song.artist.clone(), if song.missing { DIM } else { FG }),
                            (
                                song.key.clone().unwrap_or_default(),
                                if song.missing { DIM } else { FG },
                            ),
                            (song.format.to_uppercase(), FG),
                        ];
                        let mut x = row.left();
                        for ((text, color), column) in cells.iter().zip(columns) {
                            let cell = Rect::from_min_size(
                                Pos2::new(x, row.top()),
                                egui::vec2(column, row_height),
                            );
                            painter
                                .with_clip_rect(cell.shrink2(egui::vec2(4.0, 0.0)))
                                .text(
                                    Pos2::new(x + 4.0, row.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    text,
                                    FontId::proportional(11.0),
                                    *color,
                                );
                            x += column;
                        }

                        let (shift, command) = ui.input(|i| {
                            (i.modifiers.shift, i.modifiers.command || i.modifiers.ctrl)
                        });
                        if response.clicked() {
                            if shift {
                                if let Some(anchor) = w.selection_anchor {
                                    let (low, high) = if anchor <= position {
                                        (anchor, position)
                                    } else {
                                        (position, anchor)
                                    };
                                    w.selected.clear();
                                    for target in low..=high {
                                        if let Some(&song) = view.get(target) {
                                            w.selected.insert(song);
                                        }
                                    }
                                } else {
                                    w.selected.clear();
                                    w.selected.insert(song_index);
                                }
                            } else if command {
                                if !w.selected.remove(&song_index) {
                                    w.selected.insert(song_index);
                                }
                                w.selection_anchor = Some(position);
                            } else {
                                w.selected.clear();
                                w.selected.insert(song_index);
                                w.selection_anchor = Some(position);
                            }
                        }
                        if response.secondary_clicked() && !w.selected.contains(&song_index) {
                            w.selected.clear();
                            w.selected.insert(song_index);
                            w.selection_anchor = Some(position);
                        }
                        let response = if song.missing {
                            response.on_hover_text(tr("File not found"))
                        } else {
                            response
                        };
                        response.context_menu(|ui| {
                            library_row_menu(ui, w, view.as_slice());
                        });
                    }
                });
        });
}

/// Paths and formats of the currently selected library rows.
fn library_selected_songs(w: &LibraryWindow) -> Vec<(PathBuf, String)> {
    let mut indices: Vec<usize> = w.selected.iter().copied().collect();
    indices.sort_unstable();
    indices
        .into_iter()
        .filter_map(|index| w.songs.get(index))
        .map(|song| (song.path.clone(), song.format.clone()))
        .collect()
}

/// Right-click menu for a song row.
fn library_row_menu(ui: &mut egui::Ui, w: &mut LibraryWindow, view: &[usize]) {
    if ui.button(tr("Select all")).clicked() {
        w.selected = view.iter().copied().collect();
        w.selection_anchor = None;
        ui.close();
    }
    ui.separator();
    let count = w.selected.len();
    let label = if count > 1 {
        tr_args(
            "Delete {} songs\u{2026}",
            &[&count as &dyn std::fmt::Display],
        )
    } else {
        tr("Delete\u{2026}")
    };
    let delete_response = ui
        .add_enabled(count > 0, egui::Button::new(label))
        .on_hover_text(tr("Remove from the database or delete the files"));
    if delete_response.clicked() {
        let selected = library_selected_songs(w);
        if !selected.is_empty() {
            w.delete_confirm = Some(selected);
        }
        ui.close();
    }
}

/// Contents of the Split MIDI window.
fn split_window_ui(ui: &mut egui::Ui, w: &mut SplitWindow) {
    ui.set_width(ui.available_width());
    let area = ui.max_rect();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(150.0, (area.height() - 46.0).max(120.0)),
            Layout::top_down(Align::Min),
            |ui| {
                if nav_row(ui, &tr("MIDI Channel"), w.page == 0, Icon::Music).clicked() {
                    w.page = 0;
                }
                if nav_row(ui, &tr("Instrument Group"), w.page == 1, Icon::AudioLines).clicked() {
                    w.page = 1;
                }
                if nav_row(ui, &tr("Drum Channel"), w.page == 2, Icon::AudioLines).clicked() {
                    w.page = 2;
                }
            },
        );
        ui.add_space(4.0);
        let (divider, _) = ui.allocate_exact_size(
            egui::vec2(1.0, (area.height() - 58.0).max(120.0)),
            egui::Sense::hover(),
        );
        ui.painter()
            .rect_filled(divider, 0.0, Color32::from_gray(70));
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.set_width((area.width() - 178.0).max(400.0));
            if w.page == 0 {
                split_table_ui(ui, w);
                ui.add_space(8.0);
                let image = if w.channel_selected == Some(9) { 0 } else { 1 };
                group_preview(
                    ui,
                    &mut w.channel_texture,
                    &CHANNEL_IMAGES,
                    "channel_preview",
                    image,
                );
            } else if w.page == 1 {
                split_groups_ui(ui, w);
            } else {
                split_drums_ui(ui, w);
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new(tr_args(
                    "Default Port: {}",
                    &[&w.default_label as &dyn std::fmt::Display],
                ))
                .size(11.0)
                .color(DIM),
            );
            if let Some(error) = &w.error {
                ui.label(RichText::new(error.clone()).size(10.5).color(ERROR));
            }
        });
    });

    // The window title bar closes it, so the bottom-left corner holds the
    // reset, pinned to the bottom like the settings window.
    let button_size = egui::vec2(132.0, 26.0);
    let button_rect = Rect::from_min_size(
        Pos2::new(area.left(), area.bottom() - button_size.y - 12.0),
        button_size,
    );
    if ui
        .put(button_rect, egui::Button::new(tr("Reset to defaults")))
        .clicked()
    {
        for port in &mut w.channels {
            port.clear();
        }
        for value in &mut w.group_volume_on {
            *value = false;
        }
        for value in &mut w.group_velocity_on {
            *value = false;
        }
        for port in &mut w.group_ports {
            port.clear();
        }
        for value in &mut w.group_volume {
            *value = 100;
        }
        for value in &mut w.group_velocity_min {
            *value = 0;
        }
        for value in &mut w.group_velocity_max {
            *value = 127;
        }
        for value in &mut w.drum_volume_on {
            *value = false;
        }
        for value in &mut w.drum_velocity_on {
            *value = false;
        }
        for port in &mut w.drum_ports {
            port.clear();
        }
        for value in &mut w.drum_volume {
            *value = 100;
        }
        for value in &mut w.drum_velocity_min {
            *value = 0;
        }
        for value in &mut w.drum_velocity_max {
            *value = 127;
        }
        w.changed = true;
    }
}

/// One table row with the group name and its output port.
///
/// Selection works like the channel table: a click selects one row, dragging
/// selects a range, Ctrl/Shift toggles rows, and a right-click assigns the
/// chosen output to every selected group.
fn split_groups_ui(ui: &mut egui::Ui, w: &mut SplitWindow) {
    const HEADER: f32 = 22.0;
    const ROW: f32 = 20.0;
    let width = ui.available_width().min(500.0);
    let name_width = (width * 0.45).max(160.0);
    let body = ROW * audio::GROUPS as f32;

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, HEADER + body),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let header = Rect::from_min_size(rect.min, egui::vec2(width, HEADER));
    let rows_top = header.bottom();

    let row_at = |y: f32| {
        (y >= rows_top && y < rows_top + body)
            .then(|| (((y - rows_top) / ROW).floor() as usize).min(audio::GROUPS - 1))
    };
    // The press frame still has the pointer where the button went down; the
    // drag threshold would otherwise move the anchor one row along.
    let toggle = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);
    // Only the primary button selects; a right-click must keep the selection.
    let selecting = ui.input(|i| i.pointer.primary_down());
    if selecting && response.is_pointer_button_down_on() {
        match w.group_drag_anchor {
            None => {
                if let Some(row) = response
                    .interact_pointer_pos()
                    .and_then(|pos| row_at(pos.y))
                {
                    w.group_drag_anchor = Some(row);
                    w.group_selected = Some(row);
                    if toggle {
                        if let Some(index) =
                            w.group_selection.iter().position(|&group| group == row)
                        {
                            w.group_selection.remove(index);
                        } else {
                            w.group_selection.push(row);
                        }
                    } else {
                        w.group_selection = vec![row];
                    }
                }
            }
            Some(anchor) => {
                if !toggle
                    && let Some(row) = response
                        .interact_pointer_pos()
                        .and_then(|pos| row_at(pos.y))
                {
                    let (low, high) = if anchor <= row {
                        (anchor, row)
                    } else {
                        (row, anchor)
                    };
                    w.group_selection = (low..=high).collect();
                }
            }
        }
    } else {
        w.group_drag_anchor = None;
    }

    if response.secondary_clicked()
        && let Some(row) = response
            .interact_pointer_pos()
            .and_then(|pos| row_at(pos.y))
    {
        w.group_selected = Some(row);
        if !w.group_selection.contains(&row) {
            w.group_selection = vec![row];
        }
    }
    let ports = w.ports.clone();
    let mut assign: Option<String> = None;
    response.context_menu(|ui| {
        ui.set_min_width(200.0);
        if ui.button(tr("Default Port")).clicked() {
            assign = Some(String::new());
            ui.close();
        }
        if ui.button(tr("RustySynth SoundFont")).clicked() {
            assign = Some(audio::INTERNAL_PORT.to_owned());
            ui.close();
        }
        for port in &ports {
            if ui.button(port).clicked() {
                assign = Some(port.clone());
                ui.close();
            }
        }
    });
    if let Some(port) = assign {
        if w.group_ports.len() < audio::GROUPS {
            w.group_ports.resize(audio::GROUPS, String::new());
        }
        for &group in &w.group_selection {
            w.group_ports[group] = port.clone();
        }
        w.changed = true;
    }

    let hover_row = response.hover_pos().and_then(|pos| row_at(pos.y));
    let selected = |group: usize| w.group_selection.contains(&group);

    // Header.
    painter.rect_filled(header, 0.0, Color32::from_gray(34));
    painter.rect_stroke(
        header,
        0.0,
        egui::Stroke::new(1.0, Color32::from_gray(90)),
        egui::StrokeKind::Inside,
    );
    for (label, left) in [(tr("Channel"), 0.0), (tr("MIDI Output"), name_width)] {
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(header.left() + left, header.top()),
                egui::vec2(
                    if left == 0.0 {
                        name_width
                    } else {
                        width - name_width
                    },
                    HEADER,
                ),
            ))
            .text(
                Pos2::new(header.left() + left + 6.0, header.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                FG,
            );
    }

    // Rows.
    for index in 0..audio::GROUPS {
        let top = rows_top + ROW * index as f32;
        let row_rect = Rect::from_min_size(Pos2::new(rect.left(), top), egui::vec2(width, ROW));
        let fill = if selected(index) {
            Color32::from_gray(58)
        } else if hover_row == Some(index) {
            Color32::from_gray(30)
        } else if index % 2 == 0 {
            Color32::from_gray(16)
        } else {
            Color32::from_gray(20)
        };
        painter.rect_filled(row_rect, 0.0, fill);

        let color = if selected(index) {
            FG
        } else {
            Color32::from_gray(190)
        };
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(row_rect.left(), row_rect.top()),
                egui::vec2(name_width, ROW),
            ))
            .text(
                Pos2::new(row_rect.left() + 6.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                tr(GROUP_NAMES[index]),
                FontId::proportional(11.5),
                color,
            );
        let port = w.group_ports.get(index).cloned().unwrap_or_default();
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(row_rect.left() + name_width, top),
                egui::vec2(width - name_width, ROW),
            ))
            .text(
                Pos2::new(row_rect.left() + name_width + 6.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                split_port_label(&port),
                FontId::proportional(11.5),
                color,
            );
    }

    // Grid lines.
    let line = egui::Stroke::new(1.0, Color32::from_gray(70));
    painter.line_segment([header.left_bottom(), header.right_bottom()], line);
    painter.line_segment(
        [
            Pos2::new(header.left() + name_width, header.top()),
            Pos2::new(header.left() + name_width, rows_top + body),
        ],
        line,
    );
    let border = egui::Stroke::new(1.0, Color32::from_gray(90));
    painter.rect_stroke(
        Rect::from_min_size(rect.min, egui::vec2(width, HEADER + body)),
        0.0,
        border,
        egui::StrokeKind::Inside,
    );

    if let Some(row) = hover_row {
        response.on_hover_text(GROUP_INSTRUMENTS[row]);
    }

    // Preview and per-group controls.
    let index = w.group_selected.unwrap_or(0).min(audio::GROUPS - 1);
    ui.add_space(8.0);
    ui.horizontal_top(|ui| {
        group_preview(
            ui,
            &mut w.group_texture,
            &GROUP_IMAGES,
            "group_preview",
            index,
        );
        ui.add_space(20.0);
        ui.vertical(|ui| {
            ui.set_width(260.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.spacing_mut().slider_width = 250.0;
            if w.group_volume_on.len() < audio::GROUPS {
                w.group_volume_on.resize(audio::GROUPS, false);
            }
            ui.horizontal(|ui| {
                if ui
                    .checkbox(
                        &mut w.group_volume_on[index],
                        RichText::new(tr("Volume")).size(12.0),
                    )
                    .changed()
                {
                    w.changed = true;
                }
                if w.group_volume.len() < audio::GROUPS {
                    w.group_volume.resize(audio::GROUPS, 100);
                }
                let spacing = ui.spacing().item_spacing.x;
                let space = (ui.available_width() - 46.0 - spacing).max(0.0);
                ui.add_space(space);
                let volume_enabled = w.group_volume_on[index];
                ui.add_enabled_ui(volume_enabled, |ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut w.group_volume[index])
                                .range(0..=127u32)
                                .min_size(egui::vec2(46.0, 20.0)),
                        )
                        .changed()
                    {
                        w.changed = true;
                    }
                });
            });
            let volume_enabled = w.group_volume_on[index];
            ui.add_enabled_ui(volume_enabled, |ui| {
                let mut volume = w.group_volume[index] as f32;
                if ui
                    .add_sized(
                        [250.0, 18.0],
                        egui::Slider::new(&mut volume, 0.0..=127.0).show_value(false),
                    )
                    .changed()
                {
                    w.group_volume[index] = volume.round() as u32;
                    w.changed = true;
                }
            });

            ui.add_space(6.0);
            if w.group_velocity_on.len() < audio::GROUPS {
                w.group_velocity_on.resize(audio::GROUPS, false);
            }
            ui.horizontal(|ui| {
                if ui
                    .checkbox(
                        &mut w.group_velocity_on[index],
                        RichText::new(tr("Velocity")).size(12.0),
                    )
                    .changed()
                {
                    w.changed = true;
                }
                if w.group_velocity_min.len() < audio::GROUPS {
                    w.group_velocity_min.resize(audio::GROUPS, 0);
                    w.group_velocity_max.resize(audio::GROUPS, 127);
                }
                let spacing = ui.spacing().item_spacing.x;
                let space = (ui.available_width() - 92.0 - spacing * 2.0).max(0.0);
                ui.add_space(space);
                let velocity_enabled = w.group_velocity_on[index];
                ui.add_enabled_ui(velocity_enabled, |ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut w.group_velocity_min[index])
                                .range(0..=127u32)
                                .min_size(egui::vec2(46.0, 20.0)),
                        )
                        .changed()
                    {
                        w.group_velocity_min[index] =
                            w.group_velocity_min[index].min(w.group_velocity_max[index]);
                        w.changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut w.group_velocity_max[index])
                                .range(0..=127u32)
                                .min_size(egui::vec2(46.0, 20.0)),
                        )
                        .changed()
                    {
                        w.group_velocity_max[index] =
                            w.group_velocity_max[index].max(w.group_velocity_min[index]);
                        w.changed = true;
                    }
                });
            });
            let velocity_enabled = w.group_velocity_on[index];
            ui.add_enabled_ui(velocity_enabled, |ui| {
                let mut low = w.group_velocity_min[index] as f32;
                if ui
                    .add_sized(
                        [250.0, 18.0],
                        egui::Slider::new(&mut low, 0.0..=127.0).show_value(false),
                    )
                    .changed()
                {
                    w.group_velocity_min[index] =
                        (low.round() as u32).min(w.group_velocity_max[index]);
                    w.changed = true;
                }
                let mut high = w.group_velocity_max[index] as f32;
                if ui
                    .add_sized(
                        [250.0, 18.0],
                        egui::Slider::new(&mut high, 0.0..=127.0).show_value(false),
                    )
                    .changed()
                {
                    w.group_velocity_max[index] =
                        (high.round() as u32).max(w.group_velocity_min[index]);
                    w.changed = true;
                }
            });
        });
    });
}

/// One drum group row with its output port.
///
/// Selection works like the channel table: a click selects one row, dragging
/// selects a range, Ctrl/Shift toggles rows, and a right-click assigns the
/// chosen output to every selected group.
fn split_drums_ui(ui: &mut egui::Ui, w: &mut SplitWindow) {
    const HEADER: f32 = 22.0;
    const ROW: f32 = 20.0;
    let width = ui.available_width().min(500.0);
    let name_width = (width * 0.45).max(160.0);
    let body = ROW * audio::DRUM_GROUPS as f32;

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, HEADER + body),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let header = Rect::from_min_size(rect.min, egui::vec2(width, HEADER));
    let rows_top = header.bottom();

    let row_at = |y: f32| {
        (y >= rows_top && y < rows_top + body)
            .then(|| (((y - rows_top) / ROW).floor() as usize).min(audio::DRUM_GROUPS - 1))
    };
    // The press frame still has the pointer where the button went down; the
    // drag threshold would otherwise move the anchor one row along.
    let toggle = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);
    // Only the primary button selects; a right-click must keep the selection.
    let selecting = ui.input(|i| i.pointer.primary_down());
    if selecting && response.is_pointer_button_down_on() {
        match w.drum_drag_anchor {
            None => {
                if let Some(row) = response
                    .interact_pointer_pos()
                    .and_then(|pos| row_at(pos.y))
                {
                    w.drum_drag_anchor = Some(row);
                    w.drum_selected = Some(row);
                    if toggle {
                        if let Some(index) = w.drum_selection.iter().position(|&group| group == row)
                        {
                            w.drum_selection.remove(index);
                        } else {
                            w.drum_selection.push(row);
                        }
                    } else {
                        w.drum_selection = vec![row];
                    }
                }
            }
            Some(anchor) => {
                if !toggle
                    && let Some(row) = response
                        .interact_pointer_pos()
                        .and_then(|pos| row_at(pos.y))
                {
                    let (low, high) = if anchor <= row {
                        (anchor, row)
                    } else {
                        (row, anchor)
                    };
                    w.drum_selection = (low..=high).collect();
                }
            }
        }
    } else {
        w.drum_drag_anchor = None;
    }

    if response.secondary_clicked()
        && let Some(row) = response
            .interact_pointer_pos()
            .and_then(|pos| row_at(pos.y))
    {
        w.drum_selected = Some(row);
        if !w.drum_selection.contains(&row) {
            w.drum_selection = vec![row];
        }
    }
    let ports = w.ports.clone();
    let mut assign: Option<String> = None;
    response.context_menu(|ui| {
        ui.set_min_width(200.0);
        if ui.button(tr("Default Port")).clicked() {
            assign = Some(String::new());
            ui.close();
        }
        if ui.button(tr("RustySynth SoundFont")).clicked() {
            assign = Some(audio::INTERNAL_PORT.to_owned());
            ui.close();
        }
        for port in &ports {
            if ui.button(port).clicked() {
                assign = Some(port.clone());
                ui.close();
            }
        }
    });
    if let Some(port) = assign {
        if w.drum_ports.len() < audio::DRUM_GROUPS {
            w.drum_ports.resize(audio::DRUM_GROUPS, String::new());
        }
        for &group in &w.drum_selection {
            w.drum_ports[group] = port.clone();
        }
        w.changed = true;
    }

    let hover_row = response.hover_pos().and_then(|pos| row_at(pos.y));
    let selected = |group: usize| w.drum_selection.contains(&group);

    // Header.
    painter.rect_filled(header, 0.0, Color32::from_gray(34));
    painter.rect_stroke(
        header,
        0.0,
        egui::Stroke::new(1.0, Color32::from_gray(90)),
        egui::StrokeKind::Inside,
    );
    for (label, left) in [(tr("Drum"), 0.0), (tr("MIDI Output"), name_width)] {
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(header.left() + left, header.top()),
                egui::vec2(
                    if left == 0.0 {
                        name_width
                    } else {
                        width - name_width
                    },
                    HEADER,
                ),
            ))
            .text(
                Pos2::new(header.left() + left + 6.0, header.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                FG,
            );
    }

    // Rows.
    for index in 0..audio::DRUM_GROUPS {
        let top = rows_top + ROW * index as f32;
        let row_rect = Rect::from_min_size(Pos2::new(rect.left(), top), egui::vec2(width, ROW));
        let fill = if selected(index) {
            Color32::from_gray(58)
        } else if hover_row == Some(index) {
            Color32::from_gray(30)
        } else if index % 2 == 0 {
            Color32::from_gray(16)
        } else {
            Color32::from_gray(20)
        };
        painter.rect_filled(row_rect, 0.0, fill);

        let color = if selected(index) {
            FG
        } else {
            Color32::from_gray(190)
        };
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(row_rect.left(), row_rect.top()),
                egui::vec2(name_width, ROW),
            ))
            .text(
                Pos2::new(row_rect.left() + 6.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                tr(DRUM_NAMES[index]),
                FontId::proportional(11.5),
                color,
            );
        let port = w.drum_ports.get(index).cloned().unwrap_or_default();
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(row_rect.left() + name_width, top),
                egui::vec2(width - name_width, ROW),
            ))
            .text(
                Pos2::new(row_rect.left() + name_width + 6.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                split_port_label(&port),
                FontId::proportional(11.5),
                color,
            );
    }

    // Grid lines.
    let line = egui::Stroke::new(1.0, Color32::from_gray(70));
    painter.line_segment([header.left_bottom(), header.right_bottom()], line);
    painter.line_segment(
        [
            Pos2::new(header.left() + name_width, header.top()),
            Pos2::new(header.left() + name_width, rows_top + body),
        ],
        line,
    );
    let border = egui::Stroke::new(1.0, Color32::from_gray(90));
    painter.rect_stroke(
        Rect::from_min_size(rect.min, egui::vec2(width, HEADER + body)),
        0.0,
        border,
        egui::StrokeKind::Inside,
    );

    if let Some(row) = hover_row {
        response.on_hover_text(DRUM_NOTES[row]);
    }

    // Preview and per-group controls.
    let index = w.drum_selected.unwrap_or(0).min(audio::DRUM_GROUPS - 1);
    ui.add_space(8.0);
    ui.horizontal_top(|ui| {
        group_preview(ui, &mut w.drum_texture, &DRUM_IMAGES, "drum_preview", index);
        ui.add_space(20.0);
        ui.vertical(|ui| {
            ui.set_width(260.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.spacing_mut().slider_width = 250.0;
            if w.drum_volume_on.len() < audio::DRUM_GROUPS {
                w.drum_volume_on.resize(audio::DRUM_GROUPS, false);
            }
            ui.horizontal(|ui| {
                if ui
                    .checkbox(
                        &mut w.drum_volume_on[index],
                        RichText::new(tr("Volume")).size(12.0),
                    )
                    .changed()
                {
                    w.changed = true;
                }
                if w.drum_volume.len() < audio::DRUM_GROUPS {
                    w.drum_volume.resize(audio::DRUM_GROUPS, 100);
                }
                let spacing = ui.spacing().item_spacing.x;
                let space = (ui.available_width() - 46.0 - spacing).max(0.0);
                ui.add_space(space);
                let volume_enabled = w.drum_volume_on[index];
                ui.add_enabled_ui(volume_enabled, |ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut w.drum_volume[index])
                                .range(0..=127u32)
                                .min_size(egui::vec2(46.0, 20.0)),
                        )
                        .changed()
                    {
                        w.changed = true;
                    }
                });
            });
            let volume_enabled = w.drum_volume_on[index];
            ui.add_enabled_ui(volume_enabled, |ui| {
                let mut volume = w.drum_volume[index] as f32;
                if ui
                    .add_sized(
                        [250.0, 18.0],
                        egui::Slider::new(&mut volume, 0.0..=127.0).show_value(false),
                    )
                    .changed()
                {
                    w.drum_volume[index] = volume.round() as u32;
                    w.changed = true;
                }
            });

            ui.add_space(6.0);
            if w.drum_velocity_on.len() < audio::DRUM_GROUPS {
                w.drum_velocity_on.resize(audio::DRUM_GROUPS, false);
            }
            ui.horizontal(|ui| {
                if ui
                    .checkbox(
                        &mut w.drum_velocity_on[index],
                        RichText::new(tr("Velocity")).size(12.0),
                    )
                    .changed()
                {
                    w.changed = true;
                }
                if w.drum_velocity_min.len() < audio::DRUM_GROUPS {
                    w.drum_velocity_min.resize(audio::DRUM_GROUPS, 0);
                    w.drum_velocity_max.resize(audio::DRUM_GROUPS, 127);
                }
                let spacing = ui.spacing().item_spacing.x;
                let space = (ui.available_width() - 92.0 - spacing * 2.0).max(0.0);
                ui.add_space(space);
                let velocity_enabled = w.drum_velocity_on[index];
                ui.add_enabled_ui(velocity_enabled, |ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut w.drum_velocity_min[index])
                                .range(0..=127u32)
                                .min_size(egui::vec2(46.0, 20.0)),
                        )
                        .changed()
                    {
                        w.drum_velocity_min[index] =
                            w.drum_velocity_min[index].min(w.drum_velocity_max[index]);
                        w.changed = true;
                    }
                    if ui
                        .add(
                            egui::DragValue::new(&mut w.drum_velocity_max[index])
                                .range(0..=127u32)
                                .min_size(egui::vec2(46.0, 20.0)),
                        )
                        .changed()
                    {
                        w.drum_velocity_max[index] =
                            w.drum_velocity_max[index].max(w.drum_velocity_min[index]);
                        w.changed = true;
                    }
                });
            });
            let velocity_enabled = w.drum_velocity_on[index];
            ui.add_enabled_ui(velocity_enabled, |ui| {
                let mut low = w.drum_velocity_min[index] as f32;
                if ui
                    .add_sized(
                        [250.0, 18.0],
                        egui::Slider::new(&mut low, 0.0..=127.0).show_value(false),
                    )
                    .changed()
                {
                    w.drum_velocity_min[index] =
                        (low.round() as u32).min(w.drum_velocity_max[index]);
                    w.changed = true;
                }
                let mut high = w.drum_velocity_max[index] as f32;
                if ui
                    .add_sized(
                        [250.0, 18.0],
                        egui::Slider::new(&mut high, 0.0..=127.0).show_value(false),
                    )
                    .changed()
                {
                    w.drum_velocity_max[index] =
                        (high.round() as u32).max(w.drum_velocity_min[index]);
                    w.changed = true;
                }
            });
        });
    });
}

/// Preview artwork for one group or drum group.
fn group_preview(
    ui: &mut egui::Ui,
    texture: &mut Option<(usize, egui::TextureHandle)>,
    images: &'static [&'static [u8]],
    name: &str,
    index: usize,
) {
    let loaded = texture.as_ref().map(|(group, _)| *group);
    if loaded != Some(index)
        && let Some(bytes) = images.get(index)
        && let Ok(image) = load_background_bytes(bytes)
    {
        *texture = Some((
            index,
            ui.ctx().load_texture(
                format!("{name}_{index}"),
                image,
                egui::TextureOptions::LINEAR,
            ),
        ));
    }
    let size = egui::vec2(150.0, 150.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(8));
    if let Some((group, texture)) = texture
        && *group == index
    {
        painter.image(
            texture.id(),
            rect.shrink(2.0),
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, Color32::from_gray(90)),
        egui::StrokeKind::Inside,
    );
}

/// The channel table: a click selects one row, a drag selects a range and
/// Ctrl+click adds or removes single rows. Right-click assigns an output to
/// every selected channel.
fn split_table_ui(ui: &mut egui::Ui, w: &mut SplitWindow) {
    const HEADER: f32 = 22.0;
    const ROW: f32 = 20.0;
    const ROWS: usize = 16;
    let width = ui.available_width().min(500.0);
    let channel_width = (width * 0.45).max(160.0);

    let body = ROW * ROWS as f32;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, HEADER + body),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let header = Rect::from_min_size(rect.min, egui::vec2(width, HEADER));
    let rows_top = header.bottom();

    let row_at = |y: f32| {
        (y >= rows_top && y < rows_top + body)
            .then(|| (((y - rows_top) / ROW).floor() as usize).min(ROWS - 1))
    };
    // The press frame still has the pointer where the button went down; the
    // drag threshold would otherwise move the anchor one row along.
    let toggle = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);
    // Only the primary button selects; a right-click must keep the group.
    let selecting = ui.input(|i| i.pointer.primary_down());
    if selecting && response.is_pointer_button_down_on() {
        match w.drag_anchor {
            None => {
                if let Some(row) = response
                    .interact_pointer_pos()
                    .and_then(|pos| row_at(pos.y))
                {
                    w.drag_anchor = Some(row);
                    w.channel_selected = Some(row);
                    if toggle {
                        if let Some(index) = w.selection.iter().position(|&ch| ch == row) {
                            w.selection.remove(index);
                        } else {
                            w.selection.push(row);
                        }
                    } else {
                        w.selection = vec![row];
                    }
                }
            }
            Some(anchor) => {
                if !toggle
                    && let Some(row) = response
                        .interact_pointer_pos()
                        .and_then(|pos| row_at(pos.y))
                {
                    let (low, high) = if anchor <= row {
                        (anchor, row)
                    } else {
                        (row, anchor)
                    };
                    w.selection = (low..=high).collect();
                }
            }
        }
    } else {
        w.drag_anchor = None;
    }

    if response.secondary_clicked()
        && let Some(row) = response
            .interact_pointer_pos()
            .and_then(|pos| row_at(pos.y))
        && !w.selection.contains(&row)
    {
        w.channel_selected = Some(row);
        if !w.selection.contains(&row) {
            w.selection = vec![row];
        }
    }
    let ports = w.ports.clone();
    let mut assign: Option<String> = None;
    response.context_menu(|ui| {
        ui.set_min_width(200.0);
        if ui.button(tr("Default Port")).clicked() {
            assign = Some(String::new());
            ui.close();
        }
        if ui.button(tr("RustySynth SoundFont")).clicked() {
            assign = Some(audio::INTERNAL_PORT.to_owned());
            ui.close();
        }
        for port in &ports {
            if ui.button(port).clicked() {
                assign = Some(port.clone());
                ui.close();
            }
        }
    });
    if let Some(port) = assign {
        if w.channels.len() < 16 {
            w.channels.resize(16, String::new());
        }
        for &channel in &w.selection {
            w.channels[channel] = port.clone();
        }
        w.changed = true;
    }

    let hover_row = response.hover_pos().and_then(|pos| row_at(pos.y));
    let selected = |channel: usize| w.selection.contains(&channel);

    // Header.
    painter.rect_filled(header, 0.0, Color32::from_gray(34));
    painter.rect_stroke(
        header,
        0.0,
        egui::Stroke::new(1.0, Color32::from_gray(90)),
        egui::StrokeKind::Inside,
    );
    for (label, left) in [(tr("Channel"), 0.0), (tr("MIDI Output"), channel_width)] {
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(header.left() + left, header.top()),
                egui::vec2(
                    if left == 0.0 {
                        channel_width
                    } else {
                        width - channel_width
                    },
                    HEADER,
                ),
            ))
            .text(
                Pos2::new(header.left() + left + 6.0, header.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                FG,
            );
    }

    // Rows.
    for channel in 0..ROWS {
        let top = rows_top + ROW * channel as f32;
        let row_rect = Rect::from_min_size(Pos2::new(rect.left(), top), egui::vec2(width, ROW));
        let fill = if selected(channel) {
            Color32::from_gray(58)
        } else if hover_row == Some(channel) {
            Color32::from_gray(30)
        } else if channel % 2 == 0 {
            Color32::from_gray(16)
        } else {
            Color32::from_gray(20)
        };
        painter.rect_filled(row_rect, 0.0, fill);

        let port = w.channels.get(channel).cloned().unwrap_or_default();
        let color = if selected(channel) {
            FG
        } else {
            Color32::from_gray(190)
        };
        painter.text(
            Pos2::new(row_rect.left() + 8.0, row_rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}", channel + 1),
            FontId::proportional(11.5),
            color,
        );
        painter
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(row_rect.left() + channel_width, top),
                egui::vec2(width - channel_width, ROW),
            ))
            .text(
                Pos2::new(row_rect.left() + channel_width + 6.0, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
                split_port_label(&port),
                FontId::proportional(11.5),
                color,
            );
    }

    // Grid lines.
    let line = egui::Stroke::new(1.0, Color32::from_gray(70));
    painter.line_segment([header.left_bottom(), header.right_bottom()], line);
    painter.line_segment(
        [
            Pos2::new(header.left() + channel_width, header.top()),
            Pos2::new(header.left() + channel_width, rows_top + body),
        ],
        line,
    );
    let border = egui::Stroke::new(1.0, Color32::from_gray(90));
    painter.rect_stroke(
        Rect::from_min_size(rect.min, egui::vec2(width, HEADER + body)),
        0.0,
        border,
        egui::StrokeKind::Inside,
    );
}

/// How an empty output name is shown: it follows the default port.
fn split_port_label(port: &str) -> String {
    if port.is_empty() {
        tr("Default Port")
    } else if port.eq_ignore_ascii_case(audio::INTERNAL_PORT) {
        tr("RustySynth SoundFont")
    } else {
        port.to_owned()
    }
}

fn draw_indicator(painter: &egui::Painter, rect: Rect, top_offset: f32, scale: f32, text: &str) {
    // Every readout uses the size of the widest one, so the box is locked and
    // never jumps when the text changes.
    let size = painter
        .layout_no_wrap(
            "BPM : 200 [-180]".to_owned(),
            FontId::new(25.0 * scale, FontFamily::Monospace),
            FG,
        )
        .size()
        + egui::vec2(42.0, 26.0) * scale;
    let box_rect = Rect::from_min_size(
        Pos2::new(
            rect.max.x - size.x - 20.0 * scale,
            rect.min.y + top_offset * scale,
        ),
        size,
    );

    painter.rect_filled(box_rect, 7.0 * scale, Color32::from_black_alpha(150));
    painter.rect_stroke(
        box_rect,
        7.0 * scale,
        egui::Stroke::new(1.8 * scale, FG),
        egui::StrokeKind::Inside,
    );

    let text_galley = painter.layout_no_wrap(
        text.to_owned(),
        FontId::new(20.0 * scale, FontFamily::Monospace),
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
fn import_copy(
    record: &SongRecord,
    destination: &Path,
) -> anyhow::Result<(SongRecord, Vec<PathBuf>)> {
    let source = &record.path;
    if !source.is_file() {
        anyhow::bail!("source file is missing");
    }
    // Every imported song goes into the letter folder that matches the first
    // character of its code (`acb123.kar` -> `A`), created when missing, so a
    // growing library stays browsable for every format. If the user already
    // pointed the destination at that letter folder, keep it as it is.
    let song_folder = match letter_folder(&record.code) {
        Some(letter) => {
            let already_there = destination
                .file_name()
                .map(|name| name.to_string_lossy().eq_ignore_ascii_case(&letter))
                .unwrap_or(false);
            if already_there {
                destination.to_path_buf()
            } else {
                destination.join(letter)
            }
        }
        None => destination.to_path_buf(),
    };
    let mut files = Vec::new();
    match record.format.as_str() {
        "ncn" => {
            let (lyrics, cursor) = ncn::locate(source, &LoadOptions::default())
                .ok_or_else(|| anyhow::anyhow!("NCN companions are missing"))?;
            let extension = source
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("mid");
            files.push((
                source.clone(),
                song_folder.join(format!("{}.{}", record.code, extension)),
            ));
            files.push((lyrics, song_folder.join(format!("{}.lyr", record.code))));
            files.push((cursor, song_folder.join(format!("{}.cur", record.code))));
        }
        "kmid" | "kar" | "mkc" => {
            let extension = source
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or(if record.format == "kar" {
                    "kar"
                } else if record.format == "mkc" {
                    "mkc"
                } else {
                    "mid"
                });
            files.push((
                source.clone(),
                song_folder.join(format!("{}.{}", record.code, extension)),
            ));
        }
        _ => anyhow::bail!("unsupported format"),
    }
    if files.iter().any(|(_, path)| path.exists()) {
        anyhow::bail!("destination already exists");
    }
    std::fs::create_dir_all(&song_folder)?;
    let mut copied = Vec::new();
    for (from, to) in &files {
        if let Err(err) = std::fs::copy(from, to) {
            for path in copied {
                let _ = std::fs::remove_file(path);
            }
            return Err(err.into());
        }
        copied.push(to.clone());
    }
    let copied_path = files[0].1.clone();
    let mut song = record.clone();
    song.path = copied_path.clone();
    song.file_name = copied_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    song.missing = false;
    if let Ok(metadata) = std::fs::metadata(&copied_path) {
        song.file_size = metadata.len();
    }
    let copied_files = files.iter().map(|(_, to)| to.clone()).collect();
    Ok((song, copied_files))
}

/// Folder that groups a song under the first character of its code.
///
/// ASCII letters are upper-cased so `acb123.kar` joins an existing `A` folder;
/// digits and other scripts are kept as written. Codes that start with
/// punctuation are grouped under `#`.
fn letter_folder(code: &str) -> Option<String> {
    let first = code.trim().chars().next()?;
    let letter = if first.is_ascii_alphabetic() {
        first.to_ascii_uppercase()
    } else {
        first
    };
    Some(if letter.is_alphanumeric() {
        letter.to_string()
    } else {
        "#".to_owned()
    })
}

fn program_folder(name: &str) -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
}

/// Local stamp such as `20261114-235959`, used for backup file names.
fn local_time_stamp() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;

        let mut time = unsafe { std::mem::zeroed::<SYSTEMTIME>() };
        unsafe { GetLocalTime(&mut time) };
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
        )
    }

    #[cfg(not(windows))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs().to_string())
            .unwrap_or_else(|_| "0".to_owned())
    }
}

/// Write a timestamped copy of the library into `config/backups`.
fn backup_library() -> anyhow::Result<(PathBuf, u64)> {
    let folder = Library::default_path()
        .and_then(|path| path.parent().map(|dir| dir.join("backups")))
        .ok_or_else(|| anyhow::anyhow!("no per-user config directory available"))?;
    let target = folder.join(format!("library-{}.sqlite", local_time_stamp()));
    let library = Library::open_default()?;
    let size = library.backup_to(&target)?;
    Ok((target, size))
}

/// Playlist index the transport should start when nothing is loaded: the
/// highlighted entry, then the one Stop interrupted, then the first entry.
fn playlist_play_index(
    selection: Option<usize>,
    stopped: Option<usize>,
    len: usize,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    selection
        .filter(|index| *index < len)
        .or_else(|| stopped.filter(|index| *index < len))
        .or(Some(0))
}

/// Display name of a MIDI reset command.
fn midi_reset_name(reset: MidiReset) -> String {
    match reset {
        MidiReset::Gm => tr("GM Reset"),
        MidiReset::Gs => tr("GS Reset"),
        MidiReset::Xg => tr("XG Reset"),
        MidiReset::Roland => tr("Roland Reset"),
        MidiReset::SoundFont => tr("SoundFont Reset"),
    }
}

/// Display name of a key-change mode.
fn transpose_mode_name(mode: TransposeMode) -> String {
    match mode {
        TransposeMode::All => tr("All Tuner"),
        TransposeMode::Wolf => tr("Wolf Tuner"),
        TransposeMode::Recovery => tr("Recovery Tuner"),
    }
}

/// Human readable byte count for the database size readout.
fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MB", value / MIB)
    } else if value >= KIB {
        format!("{:.0} KB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn ensure_program_folders() {
    for name in ["NCN", "KMID", "KAR", "MKC"] {
        if let Some(path) = program_folder(name) {
            let _ = std::fs::create_dir_all(path);
        }
    }
}

fn effective_song_folder(configured: Option<&Path>, default_name: &str) -> Option<PathBuf> {
    configured
        .map(Path::to_path_buf)
        .or_else(|| program_folder(default_name))
}

fn run_dialog(kind: DialogKind) -> Option<Vec<PathBuf>> {
    match kind {
        DialogKind::Open => rfd::FileDialog::new()
            .add_filter(&tr("Karaoke"), &["kar", "mid", "midi", "mkc"])
            .add_filter(&tr("Playlist JSON"), &["json"])
            .add_filter(&tr("All files"), &["*"])
            .pick_file()
            .map(|path| vec![path]),
        DialogKind::AddFiles => rfd::FileDialog::new()
            .add_filter(&tr("Karaoke"), &["kar", "mid", "midi", "mkc"])
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
        DialogKind::BackgroundImage => rfd::FileDialog::new()
            .add_filter("Image", &["png", "jpg", "jpeg"])
            .pick_file()
            .map(|path| vec![path]),
        DialogKind::SoundFont => rfd::FileDialog::new()
            .add_filter(&tr("SoundFont"), &["sf2"])
            .pick_file()
            .map(|path| vec![path]),
        DialogKind::LyricFont => rfd::FileDialog::new()
            .add_filter(&tr("Font"), &["ttf", "otf"])
            .pick_file()
            .map(|path| vec![path]),
        DialogKind::NcnFolder
        | DialogKind::KmidFolder
        | DialogKind::KarFolder
        | DialogKind::MkcFolder
        | DialogKind::ImportFolder => rfd::FileDialog::new()
            .set_title(tr("Choose folder"))
            .pick_folder()
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

/// GM bass programs offered by the bass lock (1-based, as players show them).
const BASS_PROGRAMS: [(u32, &str); 8] = [
    (33, "33 Acoustic Bass"),
    (34, "34 Electric Bass (finger)"),
    (35, "35 Electric Bass (pick)"),
    (36, "36 Fretless Bass"),
    (37, "37 Slap Bass 1"),
    (38, "38 Slap Bass 2"),
    (39, "39 Synth Bass 1"),
    (40, "40 Synth Bass 2"),
];

/// The nine GM2 drum kits, shown by GM2 kit number. `midi_options` converts
/// them to the zero-based program numbers the synthesizer expects, so
/// "25 Electronic" selects program 24.
const DRUM_KITS: [(u32, &str); 9] = [
    (1, "1 Standard"),
    (9, "9 Room"),
    (17, "17 Power"),
    (25, "25 Electronic"),
    (26, "26 TR-808"),
    (33, "33 Jazz"),
    (41, "41 Brush"),
    (49, "49 Orchestra"),
    (57, "57 SFX"),
];

const KICK_NOTES: [(u32, &str); 2] = [(35, "35 Acoustic Bass Drum"), (36, "36 Bass Drum 1")];

const SNARE_NOTES: [(u32, &str); 4] = [
    (31, "31 Drum Sticks"),
    (34, "34 Metronome Bell"),
    (38, "38 Acoustic Snare"),
    (40, "40 Electric Snare"),
];

/// Move the current row's cursor to `offset` px from the row's left edge.
fn pad_to(ui: &mut egui::Ui, row_left: f32, offset: f32) {
    let target = row_left + offset;
    let current = ui.cursor().min.x;
    if target > current {
        ui.add_space(target - current);
    }
}

/// Checkbox plus a fixed-choice combo, for the MIDI voice locks.
fn lock_row(
    ui: &mut egui::Ui,
    label: &str,
    enabled: &mut bool,
    value: &mut u32,
    options: &[(u32, &str)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let left = ui.max_rect().left();
        pad_to(ui, left, 30.0);
        changed |= ui
            .add(egui::Checkbox::new(
                enabled,
                RichText::new(label).size(12.0),
            ))
            .changed();
        pad_to(ui, left, 163.0);
        let selected = options
            .iter()
            .find(|(number, _)| number == value)
            .map(|(_, name)| *name)
            .unwrap_or("--");
        ui.add_enabled_ui(*enabled, |ui| {
            // Every lock box is exactly the same width: the selected text is
            // truncated instead of widening the button.
            ui.scope(|ui| {
                ui.set_max_width(180.0);
                egui::ComboBox::from_id_salt(label)
                    .width(180.0)
                    .selected_text(selected)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for (number, name) in options {
                            if ui.selectable_value(value, *number, *name).changed() {
                                changed = true;
                            }
                        }
                    });
            });
        });
    });
    changed
}

/// A note-number row (0-127) for the metronome.
fn note_row(ui: &mut egui::Ui, label: &str, value: &mut u32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let left = ui.max_rect().left();
        pad_to(ui, left, 17.0);
        ui.label(RichText::new(label).size(12.0));
        pad_to(ui, left, 103.0);
        let response = ui.add_sized(
            [47.0, 24.0],
            egui::DragValue::new(value).range(0..=127).speed(1.0),
        );
        if response.changed() {
            changed = true;
        }
    });
    changed
}

/// Read the 20 instrument-group switches and values for the audio thread.
///
/// A group whose switch is off keeps the neutral value, so its stored volume
/// or velocity window is ignored without losing it.
fn gated_group_values(values: &[u32], on: &[bool], off: u8) -> [u8; audio::GROUPS] {
    let mut out = [off; audio::GROUPS];
    for (index, slot) in out.iter_mut().enumerate() {
        if on.get(index).copied().unwrap_or(false) {
            *slot = values
                .get(index)
                .copied()
                .unwrap_or(u32::from(off))
                .min(127) as u8;
        }
    }
    out
}

/// Read the 9 drum-group switches and values for the audio thread.
fn gated_drum_values(values: &[u32], on: &[bool], off: u8) -> [u8; audio::DRUM_GROUPS] {
    let mut out = [off; audio::DRUM_GROUPS];
    for (index, slot) in out.iter_mut().enumerate() {
        if on.get(index).copied().unwrap_or(false) {
            *slot = values
                .get(index)
                .copied()
                .unwrap_or(u32::from(off))
                .min(127) as u8;
        }
    }
    out
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
/// Flyout listing the MIDI outputs, with the active one marked.
fn midi_out_submenu(
    ctx: &egui::Context,
    pos: Pos2,
    alpha: f32,
    current: &str,
    ports: &[String],
) -> (Option<MenuAction>, Rect) {
    const WIDTH: f32 = 250.0;
    let height = 30.0 * (ports.len() + 2) as f32 + 16.0;
    let screen = ctx.content_rect();
    let pos = Pos2::new(
        pos.x.min(screen.right() - WIDTH).max(screen.left()),
        pos.y.min(screen.bottom() - height).max(screen.top()),
    );

    let mut action = None;
    let area = egui::Area::new(egui::Id::unique("midi_out_submenu"))
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
                    ui.set_width(WIDTH - 12.0);
                    let internal = if current.is_empty() {
                        format!("\u{2022} {}", tr("RustySynth SoundFont"))
                    } else {
                        tr("RustySynth SoundFont")
                    };
                    if menu_item(ui, Icon::Music, &internal) {
                        action = Some(MenuAction::SetMidiOut(String::new()));
                    }
                    ui.separator();
                    if ports.is_empty() {
                        ui.label(RichText::new(tr("No MIDI outputs")).size(11.0).color(DIM));
                    }
                    for port in ports {
                        let label = if current == *port {
                            format!("\u{2022} {port}")
                        } else {
                            port.clone()
                        };
                        if menu_item(ui, Icon::AudioLines, &label) {
                            action = Some(MenuAction::SetMidiOut(port.clone()));
                        }
                    }
                });
        });
    (action, area.response.rect)
}

fn tools_submenu(
    ctx: &egui::Context,
    pos: Pos2,
    alpha: f32,
    controller_open: bool,
) -> (Option<MenuAction>, Rect) {
    const WIDTH: f32 = 188.0;
    const HEIGHT: f32 = 38.0;
    let screen = ctx.content_rect();
    let pos = Pos2::new(
        pos.x.min(screen.right() - WIDTH).max(screen.left()),
        pos.y.min(screen.bottom() - HEIGHT).max(screen.top()),
    );

    let mut action = None;
    let area = egui::Area::new(egui::Id::unique("tools_submenu"))
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
                    ui.set_width(WIDTH - 12.0);
                    let label = if controller_open {
                        format!("\u{2022} {}", tr("MIDI Controller"))
                    } else {
                        tr("MIDI Controller")
                    };
                    if menu_item(ui, Icon::AudioLines, &label) {
                        action = Some(MenuAction::MidiController);
                    }
                });
        });
    (action, area.response.rect)
}

/// Image-or-colour picker shown from the right-click menu.
fn background_submenu(
    ctx: &egui::Context,
    pos: Pos2,
    alpha: f32,
    style: BackgroundStyle,
) -> (Option<MenuAction>, Rect) {
    const WIDTH: f32 = 188.0;
    const HEIGHT: f32 = 70.0;
    let screen = ctx.content_rect();
    let pos = Pos2::new(
        pos.x.min(screen.right() - WIDTH).max(screen.left()),
        pos.y.min(screen.bottom() - HEIGHT).max(screen.top()),
    );

    let mut action = None;
    let area = egui::Area::new(egui::Id::unique("background_submenu"))
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
                    ui.set_width(WIDTH - 12.0);
                    // The shipped artwork backs the image style, so "Image" is
                    // the current choice whenever the style is not Color.
                    let image = if style == BackgroundStyle::Color {
                        tr("Image")
                    } else {
                        format!("\u{2022} {}", tr("Image"))
                    };
                    if menu_item(ui, Icon::Image, &image) {
                        action = Some(MenuAction::SetBackground(BackgroundStyle::Image));
                    }
                    let color = if style == BackgroundStyle::Color {
                        format!("\u{2022} {}", tr("Color"))
                    } else {
                        tr("Color")
                    };
                    if menu_item(ui, Icon::Palette, &color) {
                        action = Some(MenuAction::SetBackground(BackgroundStyle::Color));
                    }
                });
        });
    (action, area.response.rect)
}

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
        // `override_text_color` would otherwise make disabled rows look active.
        Color32::from_gray(110)
    };

    let painter = ui.painter();
    let galley = painter.layout_no_wrap(label.to_owned(), FontId::proportional(13.0), text_color);
    let text_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 28.0, rect.center().y - galley.size().y * 0.5),
        galley.size(),
    );
    if response.hovered() || response.has_focus() {
        // The highlight follows the label instead of filling the whole row.
        let highlight = Rect::from_min_max(
            Pos2::new(rect.left() + 2.0, rect.top()),
            Pos2::new(
                (text_rect.right() + 8.0).min(rect.right() - 2.0),
                rect.bottom(),
            ),
        );
        painter.rect_filled(highlight, 4.0, visuals.widgets.hovered.weak_bg_fill);
    }

    icons::paint(
        painter,
        Pos2::new(rect.left() + 15.0, rect.center().y),
        icon,
        16.0,
        text_color,
    );
    painter.galley(text_rect.min, galley, text_color);

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
                for option in [TextVisibility::Upper, TextVisibility::Lower] {
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

/// Key shown for a search hit. The song that is currently playing follows the
/// transpose; every other row keeps the key stored with its file.
fn hit_key(key: &str, path: &Path, current: Option<&Path>, transpose: i32) -> String {
    let key = key.trim();
    let key = if key.is_empty() { "C" } else { key };
    if current == Some(path) {
        sounding_key(key, transpose)
    } else {
        key.to_owned()
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

/// Transpose a key name such as `G`, `F#m` or `Bbm`.
///
/// Only the note is moved: whatever follows it is kept verbatim, so untidy
/// keys found in real files (`Bdm`, `Bm7`, `(Am)`, `C major`) still change
/// with the transpose instead of staying frozen on screen.
fn transposed_key(key: &str, transpose: i32) -> Option<String> {
    let (pitch, suffix) = split_key(key)?;
    if transpose == 0 {
        return Some(key.trim().to_owned());
    }

    const SHARPS: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    const FLATS: [&str; 12] = [
        "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
    ];

    let names = if transpose > 0 { SHARPS } else { FLATS };
    let index = (pitch as i32 + transpose).rem_euclid(12) as usize;
    Some(format!("{}{suffix}", names[index]))
}

/// Split a key into its pitch class (C = 0) and the text that follows the
/// note, so `Bdm` becomes `(11, "dm")`. Returns `None` when the text does not
/// start with a note.
fn split_key(key: &str) -> Option<(u8, String)> {
    let trimmed = key.trim();
    // Some files wrap the key in brackets: `(Am)`.
    let core = trimmed.trim_matches(|ch: char| ch.is_whitespace() || "()[]{}".contains(ch));
    let mut chars = core.chars();
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

    let rest = chars.as_str();
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

    Some(((base + accidental).rem_euclid(12) as u8, rest.to_owned()))
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
            .add_sized([140.0, 24.0], egui::TextEdit::singleline(hex))
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
fn background_preview(ui: &mut egui::Ui, color: Color32, image: Option<&egui::TextureHandle>) {
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
            preview_monitor(ui, "4:3", 4.0 / 3.0, color, 156.0, image);
            ui.add_space(8.0);
            preview_monitor(ui, "16:9", 16.0 / 9.0, color, 200.0, image);
        });
}

fn preview_monitor(
    ui: &mut egui::Ui,
    label: &str,
    aspect: f32,
    color: Color32,
    width: f32,
    image: Option<&egui::TextureHandle>,
) {
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
    if let Some(texture) = image {
        paint_cover(&painter, screen, texture);
    }
    painter.rect_stroke(
        screen,
        2.0,
        egui::Stroke::new(1.5, FG),
        egui::StrokeKind::Inside,
    );
}

/// Decode a .png/.jpg file into an egui image.
fn load_background_image(path: &Path) -> anyhow::Result<egui::ColorImage> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    load_background_bytes(&bytes)
}

/// Decode PNG/JPEG bytes into an egui image.
fn load_background_bytes(bytes: &[u8]) -> anyhow::Result<egui::ColorImage> {
    let decoded = image::load_from_memory(bytes).context("decoding the image")?;
    let rgba = decoded.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        rgba.as_raw(),
    ))
}

/// Draw `texture` over `rect`, cropping the overflow so it keeps its aspect
/// ratio while covering the whole area.
fn paint_cover(painter: &egui::Painter, rect: Rect, texture: &egui::TextureHandle) {
    let size = texture.size_vec2();
    if size.x <= 0.0 || size.y <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let image_aspect = size.x / size.y;
    let rect_aspect = rect.width() / rect.height();
    let uv = if image_aspect > rect_aspect {
        let width = rect_aspect / image_aspect;
        Rect::from_min_max(
            Pos2::new(0.5 - width * 0.5, 0.0),
            Pos2::new(0.5 + width * 0.5, 1.0),
        )
    } else {
        let height = image_aspect / rect_aspect;
        Rect::from_min_max(
            Pos2::new(0.0, 0.5 - height * 0.5),
            Pos2::new(1.0, 0.5 + height * 0.5),
        )
    };
    painter.image(texture.id(), rect, uv, Color32::WHITE);
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

/// Combining marks of any script, so Thai, Lao and Vietnamese lyric timing is
/// never split away from its base character.
fn is_combining_char(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
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
        Arc::new(egui::FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/Sarabun-Regular.ttf"
        )))),
    );
    fonts.font_data.insert(
        "sarabun-bold".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/Sarabun-Bold.ttf"
        )))),
    );
    fonts.font_data.insert(
        "e1234".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/E1234-Regular.ttf"
        )))),
    );
    icons::install(&mut fonts);

    // Bundled packs cover Japanese, Korean, Chinese and Lao. Egui resolves a
    // glyph from the first family entry that has it, so appending them after
    // Sarabun gives Thai/Latin priority and a script fallback for the rest.
    let packs = crate::fonts::loaded_packs();
    for (id, bytes) in packs {
        fonts.font_data.insert(
            (*id).to_owned(),
            Arc::new(egui::FontData::from_blob(bytes.clone(), 0)),
        );
    }
    let pack_ids: Vec<String> = packs.iter().map(|(id, _)| (*id).to_owned()).collect();

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&family) {
            list.insert(0, "sarabun".to_owned());
            let mut insert_at = 1;
            for id in &pack_ids {
                list.insert(insert_at, id.clone());
                insert_at += 1;
            }
        }
    }
    let mut bold = vec!["sarabun-bold".to_owned(), "sarabun".to_owned()];
    bold.extend(pack_ids.iter().cloned());
    fonts.families.insert(FontFamily::Name("bold".into()), bold);
    let mut clock = vec!["e1234".to_owned(), "sarabun".to_owned()];
    clock.extend(pack_ids.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name("clock".into()), clock);

    let mut custom_loaded = false;
    if let Some(path) = custom {
        if let Ok(bytes) = std::fs::read(path) {
            if looks_like_font(&bytes) {
                fonts.font_data.insert(
                    "lyric-custom".to_owned(),
                    Arc::new(egui::FontData::from_owned(bytes)),
                );
                // The user font wins, then Sarabun, then the script packs.
                let mut lyric = vec!["lyric-custom".to_owned(), "sarabun".to_owned()];
                lyric.extend(pack_ids.iter().cloned());
                fonts
                    .families
                    .insert(FontFamily::Name("lyric".into()), lyric);
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
    fn search_hit_key_follows_the_transpose_only_for_the_playing_song() {
        let playing = Path::new("C:/Songs/1001.mid");
        let other = Path::new("C:/Songs/2002.mid");
        assert_eq!(hit_key("Dm", playing, Some(playing), 3), "Fm");
        assert_eq!(hit_key("Dm", other, Some(playing), 3), "Dm");
        assert_eq!(hit_key("", playing, Some(playing), 0), "C");
        assert_eq!(hit_key("Dm", playing, None, 3), "Dm");
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
    fn play_button_starts_the_expected_playlist_entry() {
        // Nothing highlighted or stopped: start at the top.
        assert_eq!(playlist_play_index(None, None, 3), Some(0));
        // The highlighted entry wins over everything else.
        assert_eq!(playlist_play_index(Some(2), Some(1), 3), Some(2));
        // After Stop nothing is highlighted, so replay what was interrupted.
        assert_eq!(playlist_play_index(None, Some(1), 3), Some(1));
        // Out-of-range values fall back to the first entry.
        assert_eq!(playlist_play_index(Some(9), Some(8), 3), Some(0));
        // An empty playlist has nothing to start.
        assert_eq!(playlist_play_index(None, None, 0), None);
        assert_eq!(playlist_play_index(Some(0), Some(0), 0), None);
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
    fn untidy_keys_still_follow_the_transpose() {
        // A key that does not start with a note is left alone.
        assert_eq!(key_with_offset("?", 3), "? [+3]");
        assert_eq!(key_with_offset("H", 1), "H [+1]");
        // Notes with extra text keep it, but the root moves.
        assert_eq!(key_with_offset("C major", 1), "C# major [+1]");
        assert_eq!(key_with_offset("Bdm", 3), "Ddm [+3]");
        assert_eq!(key_with_offset("Bm7", 2), "C#m7 [+2]");
        assert_eq!(key_with_offset("(Am)", 2), "Bm [+2]");
        assert_eq!(split_key("H"), None);
        assert_eq!(split_key("( Bm )"), Some((11, "m".to_owned())));
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
        let sample = std::path::absolute(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/sample.kar"),
        )
        .expect("absolute sample path");
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

    #[test]
    fn import_copy_reports_the_files_it_wrote() {
        let dir = std::env::temp_dir().join(format!("karaoke-import-copy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let source = dir.join("source.mid");
        std::fs::write(&source, b"MThd").expect("write source");

        let record = SongRecord {
            code: "9001".to_owned(),
            format: "kmid".to_owned(),
            path: source,
            file_name: "source.mid".to_owned(),
            title: "Test".to_owned(),
            artist: "Artist".to_owned(),
            key: Some("C".to_owned()),
            tempo: None,
            bars: None,
            duration_ms: 0,
            language: None,
            vocal_channel: None,
            year: None,
            creator: None,
            lyr_preview: None,
            file_size: 4,
            modified_at: 0,
            missing: true,
        };
        let destination = dir.join("out");
        let (copied, files) = import_copy(&record, &destination).expect("copy");
        assert_eq!(copied.path, destination.join("9").join("9001.mid"));
        assert_eq!(files, vec![destination.join("9").join("9001.mid")]);
        assert!(files[0].is_file());
        assert!(!copied.missing);

        // A second add must not overwrite the copied song.
        assert!(import_copy(&record, &destination).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_copy_handles_mkc_files() {
        let dir = std::env::temp_dir().join(format!(
            "karaoke-import-mkc-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let source = dir.join("source.mkc");
        std::fs::write(&source, b"PK").expect("write source");

        let record = SongRecord {
            code: "MKC9001".to_owned(),
            format: "mkc".to_owned(),
            path: source,
            file_name: "source.mkc".to_owned(),
            title: "Test".to_owned(),
            artist: "Artist".to_owned(),
            key: Some("C".to_owned()),
            tempo: None,
            bars: None,
            duration_ms: 0,
            language: None,
            vocal_channel: None,
            year: None,
            creator: None,
            lyr_preview: None,
            file_size: 2,
            modified_at: 0,
            missing: true,
        };
        let destination = dir.join("out");
        let (copied, files) = import_copy(&record, &destination).expect("copy");
        assert_eq!(copied.path, destination.join("M").join("MKC9001.mkc"));
        assert_eq!(files, vec![destination.join("M").join("MKC9001.mkc")]);
        assert!(files[0].is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_copy_groups_files_by_their_leading_letter() {
        let dir = std::env::temp_dir().join(format!(
            "karaoke-import-letter-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let make_record = |code: &str, source: &Path| SongRecord {
            code: code.to_owned(),
            format: "kar".to_owned(),
            path: source.to_path_buf(),
            file_name: source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            title: "Test".to_owned(),
            artist: "Artist".to_owned(),
            key: None,
            tempo: None,
            bars: None,
            duration_ms: 0,
            language: None,
            vocal_channel: None,
            year: None,
            creator: None,
            lyr_preview: None,
            file_size: 4,
            modified_at: 0,
            missing: true,
        };

        // Lower-case names join an upper-case folder, created on demand.
        let lower = dir.join("acb123.kar");
        std::fs::write(&lower, b"MThd").unwrap();
        let destination = dir.join("out");
        let record = make_record("acb123", &lower);
        let (copied, files) = import_copy(&record, &destination).expect("first copy");
        assert_eq!(copied.path, destination.join("A").join("acb123.kar"));
        assert_eq!(files, vec![destination.join("A").join("acb123.kar")]);

        // A second song with the same letter reuses the existing folder.
        let second = dir.join("abc999.kar");
        std::fs::write(&second, b"MThd").unwrap();
        let record = make_record("abc999", &second);
        let (copied, _) = import_copy(&record, &destination).expect("second copy");
        assert_eq!(copied.path, destination.join("A").join("abc999.kar"));
        assert!(destination.join("A").is_dir());

        // Numeric codes group under their digit.
        let numeric = dir.join("77.kar");
        std::fs::write(&numeric, b"MThd").unwrap();
        let record = make_record("77", &numeric);
        let (copied, _) = import_copy(&record, &destination).expect("numeric copy");
        assert_eq!(copied.path, destination.join("7").join("77.kar"));

        // Pointing the destination at the letter folder must not nest it again.
        let nested = dir.join("bee123.kar");
        std::fs::write(&nested, b"MThd").unwrap();
        let record = make_record("bee123", &nested);
        let (copied, _) = import_copy(&record, &destination.join("B")).expect("letter copy");
        assert_eq!(copied.path, destination.join("B").join("bee123.kar"));

        // NCN keeps its three files together in the letter folder.
        let ncn_mid = dir.join("ncn123.mid");
        std::fs::write(&ncn_mid, b"MThd").unwrap();
        std::fs::write(dir.join("ncn123.lyr"), b"x").unwrap();
        std::fs::write(dir.join("ncn123.cur"), b"x").unwrap();
        let record = SongRecord {
            format: "ncn".to_owned(),
            ..make_record("ncn123", &ncn_mid)
        };
        let (copied, files) = import_copy(&record, &destination).expect("ncn copy");
        assert_eq!(copied.path, destination.join("N").join("ncn123.mid"));
        assert_eq!(files.len(), 3);
        assert!(
            files
                .iter()
                .all(|path| path.parent() == Some(destination.join("N").as_path()))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mkc_files_are_recognised_by_the_playlist_scanner() {
        assert!(is_song_file(Path::new("song.mkc")));
        assert_eq!(file_type_guess(Path::new("song.mkc")), "mkc");
    }

    #[test]
    fn song_file_badges_map_each_format() {
        assert_eq!(song_file_icon("ncn"), SongFileIcon::Ncn);
        assert_eq!(song_file_icon("NCN"), SongFileIcon::Ncn);
        assert_eq!(song_file_icon("kmid"), SongFileIcon::Kmid);
        // KAR and plain MIDI share the generic badge.
        assert_eq!(song_file_icon("kar"), SongFileIcon::Midi);
        assert_eq!(song_file_icon("midi"), SongFileIcon::Midi);
        assert_eq!(song_file_icon("mkc"), SongFileIcon::Mkc);
        assert_eq!(song_file_icon("unknown"), SongFileIcon::Midi);
    }

    #[test]
    fn search_popup_band_reaches_the_bottom_border() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 60.0), egui::vec2(1000.0, 100.0));
        let (bar, lower) = KaraokeApp::search_popup_bands(rect, 52.0);
        assert_eq!(bar.top(), rect.top());
        assert_eq!(bar.width(), rect.width());
        assert_eq!(bar.bottom(), 112.0);
        assert_eq!(lower.top(), bar.bottom());
        // The white half must meet the popup border; ending at the content
        // height left a strip of the background showing.
        assert_eq!(lower.bottom(), rect.bottom());
        assert_eq!(lower.left(), rect.left());
        assert_eq!(lower.right(), rect.right());
    }

    #[test]
    fn truncation_keeps_grapheme_clusters_together() {
        // Thai marks attach to the preceding consonant: "ส" "วั" "ส" "ดี".
        let cut = truncate_editor_text("สวัสดีชาวโลก", 3);
        assert_eq!(cut.graphemes(true).count(), 3);
        assert!(cut.ends_with('…'));

        // Japanese kana are one grapheme each.
        let short = truncate_editor_text("こんにちは世界", 4);
        assert_eq!(short.graphemes(true).count(), 4);

        // Text that already fits is returned unchanged.
        assert_eq!(truncate_editor_text("abc", 5), "abc");
    }

    #[test]
    fn combining_marks_cover_every_supported_script() {
        assert!(is_combining("\u{0E31}")); // Thai mai han akat
        assert!(is_combining("\u{0EB1}")); // Lao mai kan
        assert!(is_combining("\u{0301}")); // combining acute (Vietnamese)
        assert!(is_combining("\u{3099}")); // Japanese voiced sound mark
        assert!(!is_combining("ก"));
        assert!(!is_combining("한"));
        assert!(!is_combining("a"));
    }

    #[test]
    fn song_data_page_reset_asks_for_confirmation() {
        let mut pages = SettingsPages::default();
        pages.section = 6;
        pages.settings.ncn_folder = Some(PathBuf::from(r"D:\Songs\NCN"));
        pages.settings.kmid_folder = Some(PathBuf::from(r"D:\Songs\KMID"));
        pages.settings.kar_folder = Some(PathBuf::from(r"D:\Songs\KAR"));
        pages.settings.mkc_folder = Some(PathBuf::from(r"D:\Songs\MKC"));
        pages.reset_page();
        // Nothing is cleared until the confirmation popup agrees.
        assert_eq!(
            pages.settings.ncn_folder,
            Some(PathBuf::from(r"D:\Songs\NCN"))
        );
        assert_eq!(
            pages.settings.mkc_folder,
            Some(PathBuf::from(r"D:\Songs\MKC"))
        );
        assert!(
            pages
                .intents
                .iter()
                .any(|intent| matches!(intent, SettingsIntent::ConfirmSongReset))
        );
        assert!(
            !pages
                .intents
                .iter()
                .any(|intent| matches!(intent, SettingsIntent::Save))
        );
    }

    #[test]
    fn resetting_song_data_clears_the_player_folders() {
        let mut app = KaraokeApp::default();
        app.settings.ncn_folder = Some(PathBuf::from(r"D:\Songs\NCN"));
        app.settings.kmid_folder = Some(PathBuf::from(r"D:\Songs\KMID"));
        app.settings.kar_folder = Some(PathBuf::from(r"D:\Songs\KAR"));
        app.settings.mkc_folder = Some(PathBuf::from(r"D:\Songs\MKC"));
        app.reset_settings_page(6, &egui::Context::default());
        assert_eq!(app.settings.ncn_folder, None);
        assert_eq!(app.settings.kmid_folder, None);
        assert_eq!(app.settings.kar_folder, None);
        assert_eq!(app.settings.mkc_folder, None);
    }

    #[test]
    fn save_as_keeps_external_files_beside_them() {
        let mut app = KaraokeApp::default();
        app.settings.ncn_folder = Some(PathBuf::from(r"D:\Songs\NCN"));
        let external = Path::new(r"C:\Temp\song.mid");
        assert_eq!(
            app.midi_save_as_folder(external, karaoke_player::SourceFormat::Ncn),
            PathBuf::from(r"C:\Temp")
        );
        let inside = Path::new(r"D:\Songs\NCN\Song\6\song.mid");
        assert_eq!(
            app.midi_save_as_folder(inside, karaoke_player::SourceFormat::Ncn),
            PathBuf::from(r"D:\Songs\NCN\Song\6")
        );
        assert!(!app.midi_song_is_in_library(external, karaoke_player::SourceFormat::Ncn));
        assert!(app.midi_song_is_in_library(inside, karaoke_player::SourceFormat::Ncn));
        app.settings.ncn_folder = None;
        assert_eq!(
            app.midi_save_as_folder(external, karaoke_player::SourceFormat::Ncn),
            PathBuf::from(r"C:\Temp")
        );
    }

    #[test]
    fn save_as_uses_the_stronger_warning_for_external_originals() {
        assert_eq!(
            save_as_overwrite_message(true, true),
            "The original file outside the song folders will be overwritten. Continue?"
        );
        // Renamed copy: the original is untouched.
        assert_eq!(
            save_as_overwrite_message(true, false),
            "An existing file will be overwritten. Continue?"
        );
        // Library files keep the ordinary wording.
        assert_eq!(
            save_as_overwrite_message(false, true),
            "An existing file will be overwritten. Continue?"
        );
    }

    #[test]
    fn save_as_keeps_the_package_extension_for_mkc() {
        assert_eq!(
            sanitize_save_as_name("song.mkc", "mkc"),
            Some("song.mkc".to_owned())
        );
        assert_eq!(
            sanitize_save_as_name("song", "mkc"),
            Some("song.mkc".to_owned())
        );
        assert_eq!(
            sanitize_save_as_name("SONG.MKC", "mkc"),
            Some("SONG.mkc".to_owned())
        );
        // Other formats keep working as before.
        assert_eq!(
            sanitize_save_as_name("song.mid", "mid"),
            Some("song.mid".to_owned())
        );
        assert_eq!(
            sanitize_save_as_name("song.kar", "kar"),
            Some("song.kar".to_owned())
        );
    }

    #[test]
    fn formats_database_sizes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(1024 * 1024 * 3 / 2), "1.5 MB");
    }
}
