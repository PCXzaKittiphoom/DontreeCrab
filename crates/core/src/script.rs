//! Script detection for karaoke lyrics.
//!
//! MKC stores text as UTF-8, so a package can hold any language. The player
//! uses these helpers to pick the right font pack and to label a song in the
//! song database.
//!
//! Only the scripts the bundled fonts cover are classified: Thai, Lao,
//! Japanese, Korean, Chinese (Han), Vietnamese and the Latin default.

/// Canonical short language codes written to `manifest.json`.
pub const SUPPORTED_LANGUAGES: &[&str] = &["th", "en", "ja", "zh", "ko", "vi", "lo"];

/// Map a manifest language tag onto a canonical code.
///
/// Accepts the forms karaoke files use in the wild: `th`, `TH`, `th-TH`,
/// `ja`, `jp`, `zh`, `zh-CN`, `zh-TW`, `ko`, `kr`, `vi`, `lo`, `lao`, `en`.
pub fn canonical_language(tag: &str) -> Option<&'static str> {
    let tag = tag.trim().to_ascii_lowercase();
    if tag.is_empty() {
        return None;
    }
    let primary = tag.split(['-', '_']).next().unwrap_or(&tag);
    Some(match primary {
        "th" | "tha" => "th",
        "en" | "eng" => "en",
        "ja" | "jp" | "jpn" => "ja",
        "zh" | "cn" | "chi" | "zho" | "chs" | "cht" | "tw" => "zh",
        "ko" | "kr" | "kor" => "ko",
        "vi" | "vie" => "vi",
        "lo" | "lao" => "lo",
        _ => return None,
    })
}

/// Detect the language of a lyric or metadata text.
///
/// The result is one of [`SUPPORTED_LANGUAGES`]. Latin-only text reports
/// `"en"`, which is also the fallback for empty input.
pub fn detect_language(text: &str) -> &'static str {
    let mut thai = 0usize;
    let mut lao = 0usize;
    let mut hangul = 0usize;
    let mut kana = 0usize;
    let mut han = 0usize;
    let mut vietnamese = false;
    let mut latin = 0usize;

    for ch in text.chars() {
        let code = ch as u32;
        match code {
            0x0E00..=0x0E7F => thai += 1,
            0x0E80..=0x0EFF => lao += 1,
            0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7FF => {
                hangul += 1;
            }
            0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => kana += 1,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F => {
                han += 1;
            }
            0x1EA0..=0x1EF9 => {
                vietnamese = true;
                latin += 1;
            }
            0x0102..=0x0103
            | 0x0110..=0x0111
            | 0x0128..=0x0129
            | 0x0168..=0x0169
            | 0x01A0..=0x01A1
            | 0x01AF..=0x01B0 => {
                vietnamese = true;
                latin += 1;
            }
            0x0041..=0x005A | 0x0061..=0x007A | 0x00C0..=0x024F => latin += 1,
            _ => {}
        }
    }

    // Kana and Hangul are unambiguous, and they win over the Han count.
    if kana > 0 && kana >= hangul {
        return "ja";
    }
    if hangul > 0 {
        return "ko";
    }
    if han > 0 {
        // Karaoke files rarely mix scripts; Han without kana is Chinese, but
        // Japanese titles written only in kanji are common enough that the
        // manifest language (when present) stays authoritative.
        return if thai > 0 && thai > han { "th" } else { "zh" };
    }
    let max = thai.max(lao).max(latin);
    if max == 0 {
        return "en";
    }
    if thai == max {
        "th"
    } else if lao == max {
        "lo"
    } else if vietnamese {
        "vi"
    } else {
        "en"
    }
}

/// Detect the language of a song from its title, artist and lyric text.
///
/// The lyric body is authoritative; the title and artist are only used when
/// the body has no recognisable script (instrumental tracks).
pub fn detect_song_language<'a>(
    lyrics: impl IntoIterator<Item = &'a str>,
    title: &str,
) -> &'static str {
    let body: String = lyrics.into_iter().collect::<Vec<_>>().join(" ");
    let body_language = detect_language(&body);
    if !body.trim().is_empty() && body_language != "en" {
        return body_language;
    }
    let header = detect_language(&format!("{title} {}", body));
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_supported_script() {
        assert_eq!(detect_language("สวัสดีชาวโลก"), "th");
        assert_eq!(detect_language("Hello world"), "en");
        assert_eq!(detect_language("こんにちは世界"), "ja");
        assert_eq!(detect_language("日本語の歌"), "ja");
        assert_eq!(detect_language("中文歌曲"), "zh");
        assert_eq!(detect_language("한국 노래"), "ko");
        assert_eq!(detect_language("Tiếng Việt"), "vi");
        assert_eq!(detect_language("ສະບາຍດີ"), "lo");
    }

    #[test]
    fn manifest_tags_are_canonicalised() {
        assert_eq!(canonical_language("th-TH"), Some("th"));
        assert_eq!(canonical_language("JP"), Some("ja"));
        assert_eq!(canonical_language("zh-CN"), Some("zh"));
        assert_eq!(canonical_language("zh-TW"), Some("zh"));
        assert_eq!(canonical_language("ko"), Some("ko"));
        assert_eq!(canonical_language(""), None);
        assert_eq!(canonical_language("xx"), None);
    }

    #[test]
    fn song_language_prefers_the_lyric_body() {
        assert_eq!(detect_song_language(["สวัสดี", "Hello"], "Title"), "th");
        // Instrumental: fall back to the title.
        assert_eq!(detect_song_language([], "日本語タイトル"), "ja");
        assert_eq!(detect_song_language([], ""), "en");
    }
}
