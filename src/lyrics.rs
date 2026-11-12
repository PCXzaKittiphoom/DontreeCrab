use crate::model::{LyricLine, Syllable};

/// `@INTRO`, `@SOLO` and `@OUTTRO` are player control lines, not lyrics.
///
/// The match is case-insensitive and must start the line (after optional
/// whitespace); `@INTROX` or a marker in the middle of a lyric does not match.
pub fn is_control_marker(text: &str) -> bool {
    let text = text.trim_start();
    for token in ["@INTRO", "@SOLO", "@OUTTRO"] {
        let Some(prefix) = text.get(..token.len()) else {
            continue;
        };
        if !prefix.eq_ignore_ascii_case(token) {
            continue;
        }
        let rest = &text[token.len()..];
        if !rest
            .chars()
            .next()
            .map_or(false, |ch| ch.is_ascii_alphanumeric())
        {
            return true;
        }
    }
    false
}

/// A syllable is never highlighted for longer than this, even if the next one
/// only arrives much later (instrumental breaks, count-ins, ...).
const MAX_HOLD_US: u64 = 3_000_000;
/// How long the last syllable of a line stays highlighted while the wipe
/// finishes, before the line becomes fully sung.
const LINE_TAIL_US: u64 = 800_000;
/// How long the very last syllable of a song stays highlighted at most.
const TAIL_US: u64 = 1_000_000;

/// Convert `(line, syllables)` into the final [`LyricLine`] list.
///
/// `raw` is a list of lines, each holding `(text, start_us)` pairs already in
/// playback order.
pub fn build_lines(raw: Vec<Vec<(String, u64)>>, song_end_us: u64) -> Vec<LyricLine> {
    let mut flat: Vec<(usize, String, u64)> = Vec::new();
    for (line_index, line) in raw.into_iter().enumerate() {
        for (text, start_us) in line {
            if text.is_empty() {
                continue;
            }
            flat.push((line_index, text, start_us));
        }
    }

    // Defensive: never let a timestamp go backwards.
    let mut previous = 0u64;
    for item in &mut flat {
        if item.2 < previous {
            item.2 = previous;
        }
        previous = item.2;
    }

    let count = flat.len();
    let mut lines: Vec<LyricLine> = Vec::new();
    let mut i = 0;

    while i < count {
        let line_index = flat[i].0;
        let mut j = i;
        while j < count && flat[j].0 == line_index {
            j += 1;
        }

        let mut syllables = Vec::with_capacity(j - i);
        for k in i..j {
            let start_us = flat[k].2;
            let last_in_line = k + 1 == j;
            let end_us = if k + 1 < count {
                let next = flat[k + 1].2;
                let max_hold = if last_in_line {
                    LINE_TAIL_US
                } else {
                    MAX_HOLD_US
                };
                next.max(start_us + 1).min(start_us + max_hold)
            } else {
                song_end_us.max(start_us + 1).min(start_us + TAIL_US)
            };
            syllables.push(Syllable {
                start_us,
                end_us,
                text: flat[k].1.clone(),
            });
        }

        let text = syllables
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>();
        let start_us = syllables.first().map(|s| s.start_us).unwrap_or(0);
        let end_us = syllables.last().map(|s| s.end_us).unwrap_or(start_us);
        lines.push(LyricLine {
            start_us,
            end_us,
            text,
            syllables,
        });

        i = j;
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_only_the_three_control_markers() {
        assert!(is_control_marker("@INTRO"));
        assert!(is_control_marker("  @intro  "));
        assert!(is_control_marker("@SOLO"));
        assert!(is_control_marker("@Outtro"));
        assert!(!is_control_marker("@INTROX"));
        assert!(!is_control_marker("เนื้อร้อง @INTRO"));
        assert!(!is_control_marker("..นิค คาราโอเกะ.."));
        assert!(!is_control_marker("Lyrcs By ARM"));
        assert!(!is_control_marker("Karaoke Professional"));
    }
}
