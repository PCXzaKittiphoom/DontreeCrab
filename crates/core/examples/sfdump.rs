use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

fn estimate_f0(x: &[f32], sr: f32) -> f32 {
    let min_lag = (sr / 1200.0) as usize;
    let max_lag = (sr / 60.0) as usize;
    let mut best_lag = min_lag;
    let mut best = f32::MIN;
    for lag in min_lag..max_lag {
        let n = x.len().saturating_sub(lag);
        if n < 1024 {
            break;
        }
        let mut sum = 0.0f64;
        let mut e1 = 0.0f64;
        let mut e2 = 0.0f64;
        for i in 0..n {
            sum += (x[i] as f64) * (x[i + lag] as f64);
            e1 += (x[i] as f64) * (x[i] as f64);
            e2 += (x[i + lag] as f64) * (x[i + lag] as f64);
        }
        let score = (sum / (e1.sqrt() * e2.sqrt() + 1e-12)) as f32;
        if score > best {
            best = score;
            best_lag = lag;
        }
    }
    sr / best_lag as f32
}

fn render_note(sf: &Arc<SoundFont>, channel: i32, key: i32, program: Option<i32>) -> f32 {
    let settings = SynthesizerSettings::new(44100);
    let mut synth = Synthesizer::new(sf, &settings).unwrap();
    synth.set_master_volume(0.8);
    if let Some(p) = program {
        synth.process_midi_message(channel, 0xC0, p, 0);
    }
    synth.note_on(channel, key, 100);
    let n = 44100usize;
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    synth.render(&mut l, &mut r);
    let seg = &l[8192..8192 + 16384];
    let peak = seg.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    if peak < 1e-6 {
        return 0.0;
    }
    estimate_f0(seg, 44100.0)
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: sfdump <file.sf2>");
    let mut file = std::fs::File::open(&path).unwrap();
    let sf = SoundFont::new(&mut std::io::BufReader::new(&mut file)).unwrap();
    println!("bits_per_sample = {}", sf.get_bits_per_sample());
    println!("info = {:?}", sf.get_info().get_bank_name());
    let presets = sf.get_presets();
    println!("presets = {}", presets.len());
    let mut banks = std::collections::BTreeMap::new();
    for p in presets.iter() {
        *banks.entry(p.get_bank_number()).or_insert(0usize) += 1;
    }
    println!("banks = {:?}", banks);
    println!("--- first 40 presets");
    for p in presets.iter().take(40) {
        println!(
            "  bank {:3} patch {:3}  {}",
            p.get_bank_number(),
            p.get_patch_number(),
            p.get_name()
        );
    }
    if let Some(drum) = presets.iter().find(|p| p.get_bank_number() == 128) {
        println!(
            "--- drum kit: bank {} patch {} {}",
            drum.get_bank_number(),
            drum.get_patch_number(),
            drum.get_name()
        );
    } else {
        println!("--- NO BANK 128 (no drum kit!)");
    }

    let sf = Arc::new(sf);
    println!("--- tuning tests (expect A4 = 440.0 Hz)");
    for (ch, key, label) in [(0, 69, "ch0 A4"), (0, 60, "ch0 C4"), (1, 69, "ch1 A4")] {
        let f = render_note(&sf, ch, key, None);
        println!("  {label:12} -> {f:7.2} Hz");
    }
    for program in [0, 24, 25, 33, 48, 80] {
        let f = render_note(&sf, 0, 69, Some(program));
        let cents = if f > 0.0 {
            1200.0 * (f / 440.0).log2()
        } else {
            0.0
        };
        println!("  program {program:3}  A4 -> {f:7.2} Hz ({cents:+8.1} cents)");
    }
    println!("--- drums on channel 9");
    for key in [35, 36, 38, 42, 46, 49] {
        let f = render_note(&sf, 9, key, None);
        println!("  drum key {key:3} -> {f:7.2} Hz");
    }
}
