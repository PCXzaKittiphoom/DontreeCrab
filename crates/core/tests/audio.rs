use std::path::{Path, PathBuf};
use std::sync::Arc;

use karaoke_player::audio::Player;
use karaoke_player::{LoadOptions, load};
use rustysynth::SoundFont;

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("testdata")
        .join(name)
}

fn soundfont() -> Option<Arc<SoundFont>> {
    let path = std::env::var_os("KARAOKE_SF2")
        .map(PathBuf::from)
        .or_else(|| {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("soundfonts");
            std::fs::read_dir(dir)
                .ok()?
                .flatten()
                .map(|entry| entry.path())
                .find(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("sf2"))
                })
        })?;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    SoundFont::new(&mut reader).ok().map(Arc::new)
}

fn rms(left: &[f32], right: &[f32]) -> f32 {
    let sum: f32 = left.iter().chain(right.iter()).map(|s| s * s).sum::<f32>();
    (sum / (left.len() + right.len()) as f32).sqrt()
}

#[test]
fn renders_audible_audio_at_44k_stereo() {
    let Some(sf) = soundfont() else {
        eprintln!("skipping: no .sf2 found in soundfonts/ or KARAOKE_SF2");
        return;
    };

    let song = load(&sample("sample.kar"), &LoadOptions::default()).unwrap();
    let mut player = Player::new(&song, sf.clone(), 44_100, 0).unwrap();
    assert_eq!(player.sample_rate(), 44_100);
    assert_eq!(player.maximum_polyphony(), 256);
    assert_eq!(
        Player::new_with_polyphony(&song, sf, 44_100, 0, 64)
            .unwrap()
            .maximum_polyphony(),
        64
    );

    player.play();
    player.seek(30_000_000);

    let mut left = vec![0.0f32; 44_100];
    let mut right = vec![0.0f32; 44_100];
    player.fill(&mut left, &mut right);

    let level = rms(&left, &right);
    assert!(level > 0.001, "output is silent (rms={level})");
    assert!(
        left.iter()
            .chain(right.iter())
            .all(|s| s.is_finite() && s.abs() < 2.0),
        "output contains invalid samples"
    );
}

#[test]
fn eq_and_effect_settings_keep_the_mix_audible() {
    let Some(sf) = soundfont() else {
        eprintln!("skipping: no .sf2 found in soundfonts/ or KARAOKE_SF2");
        return;
    };

    let song = load(&sample("sample.kar"), &LoadOptions::default()).unwrap();
    let mut player = Player::new(&song, sf, 44_100, 0).unwrap();
    player.set_eq(true, 9.0, -6.0, 6.0);
    player.set_effect_sends(true, 0.5, 0.0, 0.25, 1.0);
    player.play();
    player.seek(30_000_000);

    let mut left = vec![0.0f32; 44_100];
    let mut right = vec![0.0f32; 44_100];
    player.fill(&mut left, &mut right);

    let level = rms(&left, &right);
    assert!(level > 0.001, "EQ made the output silent (rms={level})");
    assert!(
        left.iter()
            .chain(right.iter())
            .all(|s| s.is_finite() && s.abs() < 2.0),
        "EQ/effects produced invalid samples"
    );

    // Turning the effects off must not silence the music itself.
    player.set_effect_sends(false, 0.0, 0.0, 0.0, 0.0);
    player.seek(30_000_000);
    player.play();
    player.fill(&mut left, &mut right);
    let level = rms(&left, &right);
    assert!(
        level > 0.001,
        "Muting reverb/chorus silenced the mix (rms={level})"
    );
}

#[test]
fn seek_moves_the_clock() {
    let Some(sf) = soundfont() else {
        eprintln!("skipping: no .sf2 found in soundfonts/ or KARAOKE_SF2");
        return;
    };

    let song = load(&sample("sample.kar"), &LoadOptions::default()).unwrap();
    let mut player = Player::new(&song, sf, 44_100, 0).unwrap();

    player.seek(60_000_000);
    assert!((player.position_us() as i64 - 60_000_000).abs() < 1_000);

    player.play();
    let mut left = vec![0.0f32; 4_410];
    let mut right = vec![0.0f32; 4_410];
    player.fill(&mut left, &mut right);
    assert!((player.position_us() as i64 - 60_100_000).abs() < 2_000);

    player.seek(0);
    assert_eq!(player.position_us(), 0);
}

#[test]
fn pause_emits_silence_and_freezes_the_clock() {
    let Some(sf) = soundfont() else {
        eprintln!("skipping: no .sf2 found in soundfonts/ or KARAOKE_SF2");
        return;
    };

    let song = load(&sample("sample.kar"), &LoadOptions::default()).unwrap();
    let mut player = Player::new(&song, sf, 44_100, 0).unwrap();

    player.play();
    let mut left = vec![0.0f32; 1_024];
    let mut right = vec![0.0f32; 1_024];
    player.fill(&mut left, &mut right);
    let before = player.position_us();

    player.pause();
    player.fill(&mut left, &mut right);
    assert_eq!(player.position_us(), before);
    assert!(left.iter().chain(right.iter()).all(|s| *s == 0.0));
}

#[test]
fn muting_the_melody_channel_silences_it() {
    let Some(sf) = soundfont() else {
        eprintln!("skipping: no .sf2 found in soundfonts/ or KARAOKE_SF2");
        return;
    };

    let mut song = load(&sample("sample.kar"), &LoadOptions::default()).unwrap();
    // Keep only the melody channel (MIDI channel 9, index 8).
    song.events.retain(|event| match &event.kind {
        karaoke_player::EventKind::NoteOn { channel, .. }
        | karaoke_player::EventKind::NoteOff { channel, .. }
        | karaoke_player::EventKind::ProgramChange { channel, .. }
        | karaoke_player::EventKind::ControlChange { channel, .. }
        | karaoke_player::EventKind::PitchBend { channel, .. } => *channel == 8,
        _ => false,
    });

    let mut player = Player::new(&song, sf, 44_100, 0).unwrap();
    player.set_volume(1.0);

    let render = |player: &mut Player| {
        player.seek(30_000_000);
        player.play();
        let mut left = vec![0.0f32; 44_100];
        let mut right = vec![0.0f32; 44_100];
        player.fill(&mut left, &mut right);
        rms(&left, &right)
    };

    let unmuted = render(&mut player);
    assert!(unmuted > 0.001, "melody channel produced no sound");

    player.set_channel_muted(8, true);
    let muted = render(&mut player);

    assert!(
        muted < unmuted * 0.05,
        "muted channel is still audible: {muted} vs {unmuted}"
    );
}

#[test]
fn speed_changes_the_clock_rate() {
    let Some(sf) = soundfont() else {
        eprintln!("skipping: no .sf2 found in soundfonts/ or KARAOKE_SF2");
        return;
    };

    let song = load(&sample("sample.kar"), &LoadOptions::default()).unwrap();

    for (speed, expected_us) in [(2.0_f64, 2_000_000_i64), (0.5, 500_000), (1.0, 1_000_000)] {
        let mut player = Player::new(&song, sf.clone(), 44_100, 0).unwrap();
        player.play();
        player.set_speed(speed);

        let mut left = vec![0.0f32; 44_100];
        let mut right = vec![0.0f32; 44_100];
        player.fill(&mut left, &mut right);

        let position = player.position_us() as i64;
        assert!(
            (position - expected_us).abs() < 40_000,
            "speed {speed}: position {position} us, expected ~{expected_us} us"
        );
    }
}
