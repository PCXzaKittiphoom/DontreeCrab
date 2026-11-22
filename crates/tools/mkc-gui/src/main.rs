//! Standalone GUI for converting karaoke songs to MKC files.

// The converter is a window-only program: release builds must not open a
// console window behind it. Debug builds keep the console for diagnostics.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui;
use egui::{Align, Color32, Layout, RichText};
use karaoke_player::i18n::{self, tr, tr_args};
use karaoke_player::text::EncodingChoice;
use karaoke_player::{LoadOptions, mkc};

const ACCENT: Color32 = Color32::from_rgb(64, 148, 255);
const FG: Color32 = Color32::from_gray(228);
const DIM: Color32 = Color32::from_gray(132);
const ERROR: Color32 = Color32::from_rgb(255, 120, 120);
const SUCCESS: Color32 = Color32::from_rgb(128, 210, 158);

/// Encodings offered for the source lyrics, in menu order.
const ENCODINGS: &[EncodingChoice] = &[
    EncodingChoice::Auto,
    EncodingChoice::Utf8,
    EncodingChoice::Cp874,
    EncodingChoice::ShiftJis,
    EncodingChoice::Gb18030,
    EncodingChoice::Big5,
    EncodingChoice::EucKr,
    EncodingChoice::Cp1258,
    EncodingChoice::Cp1251,
    EncodingChoice::Latin1,
];

pub struct MkcWindow {
    pub input: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub recursive: bool,
    pub overwrite: bool,
    pub encoding: EncodingChoice,
    pub busy: bool,
    pub progress: f32,
    pub done: usize,
    pub total: usize,
    pub status: Option<String>,
    pub error: Option<String>,
    pub log: Vec<LogLine>,
    pub intents: Vec<MkcIntent>,
    job: Option<Receiver<MkcMessage>>,
}

#[derive(Clone)]
pub struct LogLine {
    pub text: String,
    pub error: bool,
}

pub enum MkcIntent {
    PickInputFile,
    PickInputFolder,
    PickOutputFolder,
}

enum MkcMessage {
    /// The folder walk found `found` songs so far.
    Scanning(usize),
    /// The walk finished and conversion is about to start.
    Start {
        total: usize,
    },
    /// The folder could not be read.
    ScanFailed(String),
    Progress {
        done: usize,
        total: usize,
    },
    Line {
        text: String,
        error: bool,
    },
    Done {
        converted: usize,
        failed: usize,
    },
}

impl Default for MkcWindow {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            recursive: true,
            overwrite: false,
            encoding: EncodingChoice::Auto,
            busy: false,
            progress: 0.0,
            done: 0,
            total: 0,
            status: None,
            error: None,
            log: Vec::new(),
            intents: Vec::new(),
            job: None,
        }
    }
}

impl MkcWindow {
    pub fn set_input(&mut self, path: PathBuf) {
        self.error = None;
        self.status = None;
        self.input = Some(path.clone());
        if self.output.is_none() {
            self.output = Some(mkc::default_output_root(&path));
        }
    }

    pub fn set_output(&mut self, path: PathBuf) {
        self.error = None;
        self.output = Some(path);
    }

    /// Poll the background conversion job. Returns true when a repaint is due.
    pub fn poll(&mut self) -> bool {
        let Some(receiver) = self.job.as_ref() else {
            return false;
        };
        let mut messages = Vec::new();
        let mut disconnected = false;
        loop {
            match receiver.try_recv() {
                Ok(message) => messages.push(message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let mut finished = false;
        let mut scan_failed = false;
        for message in messages {
            match message {
                MkcMessage::Scanning(found) => {
                    self.status = Some(tr_args(
                        "Scanning… {} song(s) found",
                        &[&found as &dyn std::fmt::Display],
                    ));
                }
                MkcMessage::Start { total } => {
                    self.total = total;
                    self.done = 0;
                    self.progress = 0.0;
                    self.status = Some(tr("Converting…"));
                }
                MkcMessage::ScanFailed(error) => {
                    finished = true;
                    scan_failed = true;
                    self.error = Some(error);
                    self.status = None;
                }
                MkcMessage::Progress { done, total } => {
                    self.done = done;
                    self.total = total;
                    self.progress = if total == 0 {
                        0.0
                    } else {
                        done as f32 / total as f32
                    };
                }
                MkcMessage::Line { text, error } => {
                    self.log.push(LogLine { text, error });
                }
                MkcMessage::Done { converted, failed } => {
                    finished = true;
                    self.busy = false;
                    self.progress = 1.0;
                    self.status = if failed == 0 {
                        Some(tr_args(
                            "Converted {} song(s)",
                            &[&converted as &dyn std::fmt::Display],
                        ))
                    } else {
                        Some(tr_args(
                            "Converted {} song(s), {} failed",
                            &[
                                &converted as &dyn std::fmt::Display,
                                &failed as &dyn std::fmt::Display,
                            ],
                        ))
                    };
                }
            }
        }
        if finished || disconnected {
            self.job = None;
            self.busy = false;
            if disconnected && self.status.is_none() && !scan_failed {
                self.status = Some(tr("Conversion stopped unexpectedly"));
            }
        }
        true
    }

    pub fn start(&mut self) {
        if self.busy {
            return;
        }
        let Some(input) = self.input.clone() else {
            self.error = Some(tr("Choose a karaoke file or folder first"));
            return;
        };
        let Some(output) = self.output.clone() else {
            self.error = Some(tr("Choose an output base folder first"));
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        self.job = Some(receiver);
        self.busy = true;
        self.total = 0;
        self.done = 0;
        self.progress = 0.0;
        self.log.clear();
        self.error = None;
        // The folder walk also runs on the worker thread: a library can hold
        // thousands of songs and must never block the window.
        self.status = Some(tr("Looking for karaoke songs…"));
        let overwrite = self.overwrite;
        let recursive = self.recursive;
        let options = LoadOptions {
            encoding: self.encoding,
            ..Default::default()
        };
        std::thread::spawn(move || {
            let songs = match mkc::find_karaoke_songs_with_report(&input, recursive, &mut |found| {
                let _ = sender.send(MkcMessage::Scanning(found));
            }) {
                Ok(songs) => songs,
                Err(error) => {
                    let _ = sender.send(MkcMessage::ScanFailed(format!("{error:#}")));
                    return;
                }
            };
            let _ = sender.send(MkcMessage::Start { total: songs.len() });
            if let Err(error) = std::fs::create_dir_all(&output) {
                let _ = sender.send(MkcMessage::Line {
                    text: format!("{}: {error}", output.display()),
                    error: true,
                });
            }
            let mut converted = 0usize;
            let mut failed = 0usize;
            for (index, song) in songs.iter().enumerate() {
                let target = mkc::output_path(song, &output);
                match mkc::pack_song_with(song, &target, overwrite, &options) {
                    Ok(result) => {
                        converted += 1;
                        let _ = sender.send(MkcMessage::Line {
                            text: tr_args(
                                "{}  →  {} ({} bytes)",
                                &[
                                    &file_label(song) as &dyn std::fmt::Display,
                                    &file_label(&result.output),
                                    &result.bytes,
                                ],
                            ),
                            error: false,
                        });
                    }
                    Err(error) => {
                        failed += 1;
                        let _ = sender.send(MkcMessage::Line {
                            text: format!("{}: {error:#}", file_label(song)),
                            error: true,
                        });
                    }
                }
                let _ = sender.send(MkcMessage::Progress {
                    done: index + 1,
                    total: songs.len(),
                });
            }
            let _ = sender.send(MkcMessage::Done { converted, failed });
        });
    }
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Localised name of one source text encoding.
fn encoding_label(choice: EncodingChoice) -> String {
    match choice {
        EncodingChoice::Auto => tr("Automatic"),
        EncodingChoice::Utf8 => "UTF-8".to_owned(),
        EncodingChoice::Cp874 => tr("Thai (Windows-874)"),
        EncodingChoice::ShiftJis => tr("Japanese (Shift-JIS)"),
        EncodingChoice::Gb18030 => tr("Chinese (GB18030)"),
        EncodingChoice::Big5 => tr("Chinese (Big5)"),
        EncodingChoice::EucKr => tr("Korean (EUC-KR)"),
        EncodingChoice::Cp1258 => tr("Vietnamese (Windows-1258)"),
        EncodingChoice::Cp1251 => tr("Cyrillic (Windows-1251)"),
        EncodingChoice::Latin1 => tr("Latin-1"),
    }
}

pub fn mkc_window_ui(ui: &mut egui::Ui, w: &mut MkcWindow) {
    ui.set_width(ui.available_width());
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);

    ui.horizontal(|ui| {
        ui.label(RichText::new("MKC").size(28.0).strong().color(ACCENT));
        ui.add_space(2.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(tr("Karaoke to MKC")).size(16.0).strong());
            ui.label(
                RichText::new("ZIP + Deflate  ·  no encryption  ·  v1")
                    .size(10.5)
                    .color(DIM),
            );
        });
    });
    ui.add_space(4.0);

    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr("Source karaoke files"))
                    .size(12.5)
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(!w.busy, egui::Button::new(tr("Choose folder")))
                    .clicked()
                {
                    w.intents.push(MkcIntent::PickInputFolder);
                }
                if ui
                    .add_enabled(!w.busy, egui::Button::new(tr("Choose file")))
                    .clicked()
                {
                    w.intents.push(MkcIntent::PickInputFile);
                }
            });
        });
        path_field(
            ui,
            w.input.as_deref(),
            &tr("No karaoke file or folder selected"),
        );
        // Legacy source files (Shift-JIS, GBK, EUC-KR, ...) need the right
        // decoder before their lyrics can be stored as UTF-8 in the package.
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Text encoding")).size(11.5).color(DIM));
            ui.add_enabled_ui(!w.busy, |ui| {
                egui::ComboBox::from_id_salt("mkc_source_encoding")
                    .selected_text(RichText::new(encoding_label(w.encoding)).size(11.5))
                    .width(204.0)
                    .show_ui(ui, |ui| {
                        for choice in ENCODINGS {
                            ui.selectable_value(&mut w.encoding, *choice, encoding_label(*choice));
                        }
                    });
            })
            .response
            .on_hover_text(tr(
                "Auto tries UTF-8, then Shift-JIS, then Thai (Windows-874)",
            ));
        });
        if w.input.as_ref().is_some_and(|path| path.is_dir()) {
            ui.checkbox(&mut w.recursive, tr("Include subfolders"));
        }
    });

    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Output base folder")).size(12.5).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(!w.busy, egui::Button::new(tr("Browse")))
                    .clicked()
                {
                    w.intents.push(MkcIntent::PickOutputFolder);
                }
            });
        });
        path_field(
            ui,
            w.output.as_deref(),
            &tr("Files will be created under the MKC prefix folder"),
        );
        ui.checkbox(&mut w.overwrite, tr("Overwrite existing .mkc files"));
    });

    ui.horizontal(|ui| {
        let ready = !w.busy && w.input.is_some() && w.output.is_some();
        let convert = egui::Button::new(
            RichText::new(tr("Convert to MKC"))
                .size(14.0)
                .strong()
                .color(Color32::from_gray(12)),
        )
        .fill(ACCENT)
        .corner_radius(8)
        .min_size(egui::vec2(166.0, 36.0));
        if ui.add_enabled(ready, convert).clicked() {
            w.start();
        }
        if w.busy {
            ui.add(
                egui::ProgressBar::new(w.progress)
                    .desired_width(ui.available_width() - 8.0)
                    .show_percentage(),
            );
        } else if let Some(status) = &w.status {
            ui.label(RichText::new(status).size(11.5).color(DIM));
        }
    });

    if let Some(error) = &w.error {
        ui.label(RichText::new(error).size(11.0).color(ERROR));
    }

    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Log")).size(12.5).strong());
            if !w.log.is_empty() && !w.busy {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(tr("Clear")).clicked() {
                        w.log.clear();
                    }
                });
            }
        });
        let height = (ui.available_height() - 10.0).max(90.0);
        // Virtualised rows: a 4 GB library can produce thousands of log lines,
        // and laying them all out every frame froze the window.
        let rows = w.log.len().max(1);
        egui::ScrollArea::vertical()
            .id_salt("mkc_log")
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, 16.0, rows, |ui, range| {
                if w.log.is_empty() {
                    ui.label(
                        RichText::new(tr("Ready when you are."))
                            .size(11.0)
                            .color(DIM),
                    );
                    return;
                }
                for line in &w.log[range] {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&line.text)
                                .monospace()
                                .size(10.5)
                                .color(if line.error { ERROR } else { SUCCESS }),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&line.text);
                }
            });
    });

    ui.label(
        RichText::new("MKC v1  ·  UTF-8 manifest  ·  song.mid + lyrics.json")
            .size(10.0)
            .color(DIM),
    );
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(Color32::from_gray(17))
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(52)))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn path_field(ui: &mut egui::Ui, path: Option<&Path>, placeholder: &str) {
    let (text, color) = match path {
        Some(path) => (path.display().to_string(), FG),
        None => (placeholder.to_owned(), DIM),
    };
    let full = text.clone();
    egui::Frame::NONE
        .fill(Color32::from_gray(11))
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(48)))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).monospace().size(11.0).color(color))
                .on_hover_text(full);
        });
}

struct MkcApp {
    window: MkcWindow,
}

impl Default for MkcApp {
    fn default() -> Self {
        Self {
            window: MkcWindow::default(),
        }
    }
}

impl eframe::App for MkcApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.window.poll();
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, Color32::from_rgb(8, 8, 8));
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(20, 18))
            .show(ui, |ui| mkc_window_ui(ui, &mut self.window));
        for intent in std::mem::take(&mut self.window.intents) {
            match intent {
                MkcIntent::PickInputFile => {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title(tr("Choose a karaoke file"))
                        .add_filter("Karaoke", &["kar", "mid", "midi"])
                        .pick_file()
                    {
                        self.window.set_input(path);
                    }
                }
                MkcIntent::PickInputFolder => {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title(tr("Choose folder"))
                        .pick_folder()
                    {
                        self.window.set_input(path);
                    }
                }
                MkcIntent::PickOutputFolder => {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title(tr("Choose output base folder"))
                        .pick_folder()
                    {
                        self.window.set_output(path);
                    }
                }
            }
        }
        if self.window.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

fn main() -> eframe::Result<()> {
    i18n::init();
    let language = karaoke_player::settings::Settings::load().language;
    i18n::set_language(&language);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(tr("MKC Converter"))
            .with_inner_size([760.0, 600.0])
            .with_min_inner_size([760.0, 600.0])
            .with_max_inner_size([760.0, 600.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_icon(Arc::new(load_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "MKC Converter",
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            install_style(&cc.egui_ctx);
            Ok(Box::new(MkcApp::default()))
        }),
    )
}

fn load_icon() -> egui::IconData {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../logo/MKC_LOGO.ico"
    ));
    if let Ok(image) = image::load_from_memory_with_format(bytes, image::ImageFormat::Ico) {
        let image = image.into_rgba8();
        return egui::IconData {
            rgba: image.as_raw().clone(),
            width: image.width(),
            height: image.height(),
        };
    }
    egui::IconData {
        rgba: vec![0, 0, 0, 0].repeat(16 * 16),
        width: 16,
        height: 16,
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "sarabun".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../assets/fonts/Sarabun-Regular.ttf"
        )))),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "sarabun".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "sarabun".to_owned());
    ctx.set_fonts(fonts);
}

fn install_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = Color32::from_rgb(8, 8, 8);
        style.visuals.window_fill = Color32::from_rgb(12, 12, 12);
        style.visuals.faint_bg_color = Color32::from_gray(22);
        style.visuals.extreme_bg_color = Color32::from_gray(8);
        style.visuals.widgets.inactive.bg_fill = Color32::from_gray(26);
        style.visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(26);
        style.visuals.widgets.hovered.bg_fill = Color32::from_gray(42);
        style.visuals.widgets.hovered.weak_bg_fill = Color32::from_gray(42);
        style.visuals.widgets.active.bg_fill = Color32::from_gray(54);
        style.visuals.widgets.active.weak_bg_fill = Color32::from_gray(54);
        style.visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(64, 148, 255, 96);
        style.visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(10.5, egui::FontFamily::Proportional),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_menu_lists_every_choice_once() {
        assert_eq!(ENCODINGS.len(), 10);
        let mut seen: Vec<EncodingChoice> = Vec::new();
        for choice in ENCODINGS {
            assert!(!seen.contains(choice), "duplicate encoding {choice:?}");
            assert!(!encoding_label(*choice).trim().is_empty());
            seen.push(*choice);
        }
        assert_eq!(ENCODINGS[0], EncodingChoice::Auto);
        assert!(ENCODINGS.contains(&EncodingChoice::ShiftJis));
        assert!(ENCODINGS.contains(&EncodingChoice::Gb18030));
        assert!(ENCODINGS.contains(&EncodingChoice::EucKr));
        assert!(ENCODINGS.contains(&EncodingChoice::Cp1258));
    }

    #[test]
    fn the_chosen_encoding_reaches_the_converter() {
        let root = std::env::temp_dir().join(format!(
            "dontreecrab-mkc-gui-encoding-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let midi = root.join("2001.mid");
        let mut midi_bytes = b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x00\x78".to_vec();
        let track: &[u8] = &[
            0x00, 0xff, 0x51, 0x03, 0x06, 0x1a, 0x80, 0x00, 0xff, 0x2f, 0x00,
        ];
        midi_bytes.extend_from_slice(b"MTrk");
        midi_bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
        midi_bytes.extend_from_slice(track);
        std::fs::write(&midi, &midi_bytes).unwrap();

        // Windows-874 Thai header: "ทดสอบ" / "ศิลปิน" / "C".
        let mut lyr = vec![0xB7, 0xB4, 0xCA, 0xCD, 0xBA];
        lyr.extend_from_slice(b"\r\n");
        lyr.extend_from_slice(&[0xC8, 0xD4, 0xC5, 0xBB, 0xD4, 0xB9]);
        lyr.extend_from_slice(b"\r\nC\r\n\r\nHello\r\n");
        std::fs::write(root.join("2001.lyr"), &lyr).unwrap();
        let mut cursor = Vec::new();
        for value in [0u16, 10, 20, 30, 40, 50, 60] {
            cursor.extend_from_slice(&value.to_le_bytes());
        }
        cursor.extend_from_slice(&0xffffu16.to_le_bytes());
        std::fs::write(root.join("2001.cur"), cursor).unwrap();

        let output = root.join("out");
        let mut window = MkcWindow::default();
        assert_eq!(window.encoding, EncodingChoice::Auto);
        window.encoding = EncodingChoice::Cp874;
        window.set_input(midi);
        window.set_output(output.clone());
        window.start();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while window.busy && std::time::Instant::now() < deadline {
            window.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        window.poll();
        assert!(!window.busy, "conversion did not finish");
        assert!(window.error.is_none(), "{:?}", window.error);

        let package = mkc::read_package(&output.join("MKC").join("2001.mkc")).unwrap();
        let manifest = package.manifest().unwrap();
        assert_eq!(manifest.title, "ทดสอบ");
        assert_eq!(manifest.artist, "ศิลปิน");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn converts_a_synthetic_ncn_song_in_the_background() {
        let root = std::env::temp_dir().join(format!(
            "dontreecrab-mkc-gui-{}",
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
        std::fs::write(
            root.join("1001.lyr"),
            b"Test Song\r\nTest Artist\r\nC\r\n\r\nHello\r\n",
        )
        .unwrap();
        let mut cursor = Vec::new();
        for value in [0u16, 10, 20, 30, 40, 50, 60] {
            cursor.extend_from_slice(&value.to_le_bytes());
        }
        cursor.extend_from_slice(&0xffffu16.to_le_bytes());
        std::fs::write(root.join("1001.cur"), cursor).unwrap();

        let output = root.join("out");
        let mut window = MkcWindow::default();
        window.set_input(midi);
        window.set_output(output.clone());
        window.start();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while window.busy && std::time::Instant::now() < deadline {
            window.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        window.poll();
        assert!(!window.busy, "conversion did not finish");
        assert!(output.join("MKC").join("1001.mkc").is_file());
        assert!(window.error.is_none(), "{:?}", window.error);

        let _ = std::fs::remove_dir_all(&root);
    }
}
