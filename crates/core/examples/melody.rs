use karaoke_player::audio::Player;
use karaoke_player::model::EventKind;
use karaoke_player::{LoadOptions, load};
use rustysynth::SoundFont;
use std::path::PathBuf;
use std::sync::Arc;

fn goertzel(x: &[f32], freq: f32, sr: f32) -> f32 {
    let w = 2.0 * std::f32::consts::PI * freq / sr;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = v as f64 + coeff as f64 * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    ((s1 * s1 + s2 * s2 - coeff as f64 * s1 * s2).sqrt() / x.len() as f64) as f32
}

fn estimate(x: &[f32], expected: f32, sr: f32) -> f32 {
    let mut best = (expected, 0.0f32);
    let mut f = expected * 0.8;
    while f <= expected * 1.2 {
        let m = goertzel(x, f, sr);
        if m > best.1 {
            best = (f, m);
        }
        f += 0.5;
    }
    best.0
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let channel: u8 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let start_s: f64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(28.0);
    let dur_s: f64 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20.0);
    let sr = 44100u32;

    let mut song = load(std::path::Path::new(&path), &LoadOptions::default()).unwrap();
    song.events.retain(|e| match &e.kind {
        EventKind::NoteOn { channel: c, .. } | EventKind::NoteOff { channel: c, .. } => {
            *c == channel
        }
        EventKind::ProgramChange { channel: c, .. }
        | EventKind::ControlChange { channel: c, .. }
        | EventKind::PitchBend { channel: c, .. } => *c == channel,
        _ => false,
    });

    let sf_path = std::env::var_os("KARAOKE_SF2")
        .map(PathBuf::from)
        .or_else(|| {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../soundfonts");
            std::fs::read_dir(dir).ok().and_then(|entries| {
                entries.flatten().map(|entry| entry.path()).find(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("sf2"))
                })
            })
        })
        .expect("set KARAOKE_SF2 or put a .sf2 in soundfonts/");
    let mut f = std::fs::File::open(&sf_path).unwrap();
    let sf = Arc::new(SoundFont::new(&mut std::io::BufReader::new(&mut f)).unwrap());
    let mut player = Player::new(&song, sf, sr, 0).unwrap();
    player.set_volume(0.5);
    player.seek((start_s * 1e6) as u64);
    player.play();

    let frames = (dur_s * sr as f64) as usize;
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    player.fill(&mut l, &mut r);

    // collect notes in the window
    let base = (start_s * 1e6) as u64;
    let mut open: Vec<(u8, u64)> = Vec::new();
    let mut notes = Vec::new();
    for e in &song.events {
        if e.at_us < base || e.at_us > base + (dur_s * 1e6) as u64 {
            continue;
        }
        match &e.kind {
            EventKind::NoteOn { key, .. } => open.push((*key, e.at_us - base)),
            EventKind::NoteOff { key, .. } => {
                if let Some(pos) = open.iter().rposition(|(k, _)| k == key) {
                    let (k, st) = open.remove(pos);
                    notes.push((k, st, e.at_us - base));
                }
            }
            _ => {}
        }
    }
    notes.sort_by_key(|n| n.1);
    println!("channel {channel}, {} notes in window", notes.len());
    let mut bad = 0;
    let mut n = 0;
    for (key, st, en) in notes.iter() {
        let len = en.saturating_sub(*st);
        if len < 150_000 {
            continue;
        }
        let a = ((st + len / 2 - 40_000) as usize * sr as usize) / 1_000_000;
        let b = a + 4096;
        if b >= frames {
            continue;
        }
        let expected = 440.0 * 2f32.powf((*key as f32 - 69.0) / 12.0);
        let measured = estimate(&l[a..b], expected, sr as f32);
        let cents = 1200.0 * (measured / expected).log2();
        n += 1;
        if cents.abs() > 25.0 {
            bad += 1;
        }
        if n <= 25 {
            println!(
                "  key {key:3} t={:7.2}s  expect {expected:7.2}  measured {measured:7.2}  {cents:+6.1} cents",
                *st as f64 / 1e6
            );
        }
    }
    println!("checked {n} notes, {bad} more than 25 cents off");
}
