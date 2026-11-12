use clap::ValueEnum;
use encoding_rs::WINDOWS_874;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum EncodingChoice {
    /// Try UTF-8, fall back to Windows-874 (Thai).
    #[default]
    Auto,
    Utf8,
    /// Windows-874 / TIS-620, the encoding used by Thai karaoke files.
    Cp874,
    /// Raw byte per character, for old Latin-1/ASCII files.
    Latin1,
}

/// Decode a text blob from a MIDI meta event or a `.lyr` file.
pub fn decode(bytes: &[u8], choice: EncodingChoice) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match choice {
        EncodingChoice::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        EncodingChoice::Cp874 => WINDOWS_874.decode(bytes).0.into_owned(),
        EncodingChoice::Latin1 => bytes.iter().map(|&b| b as char).collect(),
        EncodingChoice::Auto => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => WINDOWS_874.decode(bytes).0.into_owned(),
        },
    }
}
