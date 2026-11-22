use clap::ValueEnum;
use encoding_rs::{BIG5, EUC_KR, GB18030, SHIFT_JIS, WINDOWS_874, WINDOWS_1251, WINDOWS_1258};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum EncodingChoice {
    /// Try UTF-8, fall back to Windows-874 (Thai).
    #[default]
    Auto,
    Utf8,
    /// Windows-874 / TIS-620, the encoding used by Thai karaoke files.
    Cp874,
    /// Shift-JIS (Japanese).
    ShiftJis,
    /// GB18030 / GBK (Simplified Chinese).
    Gb18030,
    /// Big5 (Traditional Chinese).
    Big5,
    /// EUC-KR / Windows-949 (Korean).
    EucKr,
    /// Windows-1258 (Vietnamese).
    Cp1258,
    /// Windows-1251 (Cyrillic).
    Cp1251,
    /// Raw byte per character, for old Latin-1/ASCII files.
    Latin1,
}

/// Decode a text blob from a MIDI meta event or a `.lyr` file.
///
/// The result is always normalised to Unicode NFC so that decomposed
/// sequences (Vietnamese, Lao, Korean jamo, Latin accents) compare and render
/// as one unit.
pub fn decode(bytes: &[u8], choice: EncodingChoice) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = match choice {
        EncodingChoice::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        EncodingChoice::Cp874 => WINDOWS_874.decode(bytes).0.into_owned(),
        EncodingChoice::ShiftJis => SHIFT_JIS.decode(bytes).0.into_owned(),
        EncodingChoice::Gb18030 => GB18030.decode(bytes).0.into_owned(),
        EncodingChoice::Big5 => BIG5.decode(bytes).0.into_owned(),
        EncodingChoice::EucKr => EUC_KR.decode(bytes).0.into_owned(),
        EncodingChoice::Cp1258 => WINDOWS_1258.decode(bytes).0.into_owned(),
        EncodingChoice::Cp1251 => WINDOWS_1251.decode(bytes).0.into_owned(),
        EncodingChoice::Latin1 => bytes.iter().map(|&b| b as char).collect(),
        EncodingChoice::Auto => auto_decode(bytes),
    };
    normalize(&text)
}

/// Best-effort decoding for legacy karaoke files.
///
/// UTF-8 wins when the bytes are valid. Otherwise Shift-JIS is recognised by
/// its lead bytes (Thai Windows-874 text never uses `0x81..=0x9F`), and
/// everything else falls back to Windows-874, the Thai default.
fn auto_decode(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    if looks_like_shift_jis(bytes) {
        return SHIFT_JIS.decode(bytes).0.into_owned();
    }
    WINDOWS_874.decode(bytes).0.into_owned()
}

/// A conservative Shift-JIS probe: at least one double-byte sequence whose
/// lead is `0x81..=0x9F` (every Japanese kana/kanji stream has one), and no
/// byte that both Shift-JIS and Windows-874 leave undefined.
///
/// Thai Windows-874 text only uses `0xA1..=0xFB`, so it never triggers the
/// double-byte branch.
fn looks_like_shift_jis(bytes: &[u8]) -> bool {
    let mut i = 0usize;
    let mut double_byte = false;
    while i < bytes.len() {
        let byte = bytes[i];
        match byte {
            0x00..=0x7F => i += 1,
            0x81..=0x9F => {
                let Some(trail) = bytes.get(i + 1) else {
                    return false;
                };
                if !matches!(*trail, 0x40..=0x7E | 0x80..=0xFC) {
                    return false;
                }
                double_byte = true;
                i += 2;
            }
            0xA1..=0xFB => i += 1,
            _ => return false,
        }
    }
    double_byte
}

/// Normalise a string to Unicode NFC.
pub fn normalize(text: &str) -> String {
    text.nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_utf8_and_normalises() {
        // "ế" written as "e" + circumflex + acute must become one code point.
        let decomposed = "e\u{0302}\u{0301}";
        assert_eq!(decode(decomposed.as_bytes(), EncodingChoice::Auto), "ế");
    }

    #[test]
    fn legacy_encodings_decode_their_own_script() {
        // Shift-JIS: あいう
        let japanese = [0x82, 0xA0, 0x82, 0xA2, 0x82, 0xA4];
        assert_eq!(decode(&japanese, EncodingChoice::ShiftJis), "あいう");

        // GBK / GB18030: 中文
        let chinese = [0xD6, 0xD0, 0xCE, 0xC4];
        assert_eq!(decode(&chinese, EncodingChoice::Gb18030), "中文");

        // EUC-KR: 한국
        let korean = [0xC7, 0xD1, 0xB1, 0xB9];
        assert_eq!(decode(&korean, EncodingChoice::EucKr), "한국");

        // Windows-1258 (Vietnamese): "ê" + combining acute, normalised to "ế".
        let vietnamese = [0xEA, 0xEC];
        assert_eq!(decode(&vietnamese, EncodingChoice::Cp1258), "ế");

        // Windows-874 (Thai): ก
        assert_eq!(decode(&[0xA1], EncodingChoice::Cp874), "ก");
    }

    #[test]
    fn utf8_bom_is_ignored() {
        let bytes = b"\xEF\xBB\xBFHello";
        assert_eq!(decode(bytes, EncodingChoice::Auto), "Hello");
    }

    #[test]
    fn auto_recognises_shift_jis_but_keeps_thai() {
        // Shift-JIS あいう
        let japanese = [0x82, 0xA0, 0x82, 0xA2, 0x82, 0xA4];
        assert_eq!(decode(&japanese, EncodingChoice::Auto), "あいう");

        // Windows-874 Thai must stay Thai: "สวัสดี" (no 0x81..=0x9F bytes).
        let thai = [0xCA, 0xC7, 0xD1, 0xCA, 0xB4, 0xD5];
        assert_eq!(decode(&thai, EncodingChoice::Auto), "สวัสดี");
    }
}
