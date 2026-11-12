use std::path::{Path, PathBuf};

use karaoke_player::tempo::TempoMap;
use karaoke_player::text::EncodingChoice;
use karaoke_player::{LoadOptions, SourceFormat, kar, load, midi, ncn};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name)
}

fn assert_monotonic(song: &karaoke_player::Song) {
    assert!(!song.lyric_lines.is_empty(), "song has no lyric lines");
    for line in &song.lyric_lines {
        assert!(
            !line.text.trim().is_empty(),
            "empty lyric line at {}",
            line.start_us
        );
        assert!(!line.syllables.is_empty());
        for syllable in &line.syllables {
            assert!(
                syllable.end_us >= syllable.start_us,
                "syllable ends before it starts: {syllable:?}"
            );
        }
    }
    for pair in song.lyric_lines.windows(2) {
        assert!(
            pair[0].start_us <= pair[1].start_us,
            "lyric lines are not sorted"
        );
    }
}

#[test]
fn kar_sample_parses() {
    let song = load(&sample("sample.kar"), &LoadOptions::default()).unwrap();

    assert_eq!(song.format, SourceFormat::Kar);
    assert_eq!(song.title.as_deref(), Some("Test Song"));
    assert_eq!(song.artist.as_deref(), Some("DontreeCrab Tests"));
    assert!(song.duration_us > 30_000_000);
    assert!(song.lyric_lines.len() >= 4);
    assert_monotonic(&song);
}

#[test]
fn ncn_sample_parses() {
    let song = load(&sample("ncn/Song/1/test.mid"), &LoadOptions::default()).unwrap();

    assert_eq!(song.format, SourceFormat::Ncn);
    assert_eq!(song.title.as_deref(), Some("Test Song"));
    assert_eq!(song.artist.as_deref(), Some("DontreeCrab"));
    assert_eq!(song.key.as_deref(), Some("C"));
    assert_eq!(song.lyric_lines.len(), 2);
    assert_eq!(song.lyric_lines[0].text, "hello");
    assert_eq!(song.lyric_lines[1].text, "world");
    assert_monotonic(&song);
}

#[test]
fn plain_midi_is_labelled_midi() {
    let song = load(&sample("plain.mid"), &LoadOptions::default()).unwrap();
    assert_eq!(song.format, SourceFormat::Midi);
    assert!(song.lyric_lines.is_empty());
}

#[test]
fn forcing_kmid_requires_a_payload() {
    let mut opts = LoadOptions::default();
    opts.format = Some(SourceFormat::Kmid);
    assert!(load(&sample("plain.mid"), &opts).is_err());
}

#[test]
fn kar_control_markers_are_hidden() {
    let bytes: Vec<u8> = vec![
        b'M', b'T', b'h', b'd', 0, 0, 0, 6, 0, 0, 0, 1, 0, 96, b'M', b'T', b'r', b'k', 0, 0, 0, 39,
        0x00, 0xFF, 0x05, 0x07, b'@', b'I', b'N', b'T', b'R', b'O', b'\\', 0x83, 0x60, 0xFF, 0x05,
        0x06, b'h', b'e', b'l', b'l', b'o', b'\\', 0x83, 0x60, 0xFF, 0x05, 0x08, b'@', b'O', b'U',
        b'T', b'T', b'R', b'O', b'\\', 0x00, 0xFF, 0x2F, 0x00,
    ];

    let doc = midi::parse(&bytes, EncodingChoice::Auto).unwrap();
    let song = kar::from_doc(doc, EncodingChoice::Auto);

    assert_eq!(song.lyric_lines.len(), 1);
    assert_eq!(song.lyric_lines[0].text, "hello");
}

#[test]
fn ncn_control_markers_are_hidden() {
    let dir = std::env::temp_dir().join(format!("dontreecrab-ncn-markers-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lyr = dir.join("song.lyr");
    let cur = dir.join("song.cur");

    std::fs::write(
        &lyr,
        b"Title\nArtist\nKey\n\n@INTRO\nhello\n@SOLO\nworld\n@OUTTRO\n",
    )
    .unwrap();

    // One u16 per displayed character plus the leading/each-line separators.
    let mut cursor = Vec::new();
    for value in 0..34u16 {
        cursor.extend_from_slice(&(value * 5).to_le_bytes());
    }
    cursor.extend_from_slice(&0xFFFFu16.to_le_bytes());
    std::fs::write(&cur, cursor).unwrap();

    let bytes: Vec<u8> = vec![
        b'M', b'T', b'h', b'd', 0, 0, 0, 6, 0, 0, 0, 1, 0, 96, b'M', b'T', b'r', b'k', 0, 0, 0, 13,
        0x00, 0x90, 0x3C, 0x64, 0x87, 0x40, 0x80, 0x3C, 0x00, 0x00, 0xFF, 0x2F, 0x00,
    ];
    let doc = midi::parse(&bytes, EncodingChoice::Auto).unwrap();
    let song = ncn::build(Path::new("song.mid"), doc, &lyr, &cur, EncodingChoice::Auto).unwrap();

    assert_eq!(song.format, SourceFormat::Ncn);
    assert_eq!(song.lyric_lines.len(), 2);
    assert_eq!(song.lyric_lines[0].text, "hello");
    assert_eq!(song.lyric_lines[1].text, "world");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tempo_map_handles_changes() {
    let mut map = TempoMap::new(480);
    // 500000 us/quarter from tick 0 (= 120 BPM), then 250000 from tick 480.
    map.push(480, 250_000);

    assert_eq!(map.tick_to_us(0), 0);
    assert_eq!(map.tick_to_us(480), 500_000);
    assert_eq!(map.tick_to_us(960), 750_000);
    assert!((map.bpm_at(0) - 120.0).abs() < 1e-9);
    assert!((map.bpm_at(960) - 240.0).abs() < 1e-9);
}

#[test]
fn duplicate_tempo_at_same_tick_keeps_last() {
    let mut map = TempoMap::new(96);
    map.push(0, 1953);
    map.push(0, 645_161);
    assert!((map.bpm_at(0) - 93.0).abs() < 0.001);
}

#[test]
fn time_signatures_are_parsed() {
    for name in ["sample.kar", "ncn/Song/1/test.mid", "plain.mid"] {
        let song = load(&sample(name), &LoadOptions::default()).unwrap();
        let first = song
            .time_signatures
            .first()
            .unwrap_or_else(|| panic!("{name}: no time signature"));
        assert_eq!(
            (first.numerator, first.denominator),
            (4, 4),
            "{name}: expected 4/4"
        );
    }
}

#[test]
fn us_to_tick_inverts_tick_to_us() {
    let mut map = TempoMap::new(480);
    map.push(960, 250_000);
    map.push(2400, 600_000);

    for tick in [0u64, 1, 480, 960, 1440, 2400, 4000] {
        let us = map.tick_to_us(tick);
        let back = map.us_to_tick(us);
        assert!(back.abs_diff(tick) <= 1, "tick {tick} -> {us} us -> {back}");
    }
}
