//! Lucide icon-font glyphs used by the GUI.
//!
//! The font is embedded from `assets/fonts/lucide.ttf` and registered as the
//! `lucide` font family by `install_fonts`. Codepoints come from
//! `lucide-static` 1.72.0's CSS mapping.

use eframe::egui::{self, Color32, FontFamily, FontId, Painter, Pos2, Rect, RichText};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    Music,
    Maximize,
    Minimize2,
    Minus,
    MousePointer,
    MousePointerBan,
    ListMusic,
    Settings,
    LogOut,
    X,
    Ellipsis,
    File,
    FilePlus,
    FolderPlus,
    Download,
    Upload,
    Repeat2,
    Trash2,
    AudioLines,
    Image,
    Type,
    Info,
    ExternalLink,
}

impl Icon {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Play => "\u{e13c}",
            Self::Pause => "\u{e12e}",
            Self::Stop => "\u{e167}",
            Self::Music => "\u{e122}",
            Self::Maximize => "\u{e112}",
            Self::Minimize2 => "\u{e11b}",
            Self::Minus => "\u{e11c}",
            Self::MousePointer => "\u{e11f}",
            Self::MousePointerBan => "\u{e5e7}",
            Self::ListMusic => "\u{e2e0}",
            Self::Settings => "\u{e154}",
            Self::LogOut => "\u{e10e}",
            Self::X => "\u{e1b2}",
            Self::Ellipsis => "\u{e0b6}",
            Self::File => "\u{e0c0}",
            Self::FilePlus => "\u{e0c9}",
            Self::FolderPlus => "\u{e0d9}",
            Self::Download => "\u{e0b2}",
            Self::Upload => "\u{e19e}",
            Self::Repeat2 => "\u{e411}",
            Self::Trash2 => "\u{e18e}",
            Self::AudioLines => "\u{e55a}",
            Self::Image => "\u{e0f6}",
            Self::Type => "\u{e198}",
            Self::Info => "\u{e0f9}",
            Self::ExternalLink => "\u{e0b9}",
        }
    }

    pub fn font(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name("lucide".into()))
    }

    /// A `RichText` glyph for use inside an egui button.
    pub fn rich(self, size: f32) -> RichText {
        RichText::new(self.glyph()).font(Self::font(size))
    }
}

/// Draw a Lucide glyph centred on `center`.
pub fn paint(painter: &Painter, center: Pos2, icon: Icon, size: f32, color: Color32) {
    let galley = painter.layout_no_wrap(icon.glyph().to_owned(), Icon::font(size), color);
    let pos = center - galley.size() * 0.5;
    painter.galley_with_override_text_color(pos, galley, color);
}

/// Draw a Lucide glyph centred in `rect`.
#[allow(dead_code)]
pub fn paint_rect(painter: &Painter, rect: Rect, icon: Icon, size: f32, color: Color32) {
    paint(painter, rect.center(), icon, size, color);
}

/// Register the Lucide font family used by [`Icon`].
pub fn install(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        "lucide".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/lucide.ttf"
        )))),
    );
    fonts
        .families
        .insert(FontFamily::Name("lucide".into()), vec!["lucide".to_owned()]);
}
