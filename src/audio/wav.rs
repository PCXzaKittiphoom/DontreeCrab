use std::path::Path;

use anyhow::{Result, bail};

use super::player::Player;

#[derive(Debug, Clone, Copy)]
pub struct RenderStats {
    pub frames: u64,
    pub peak: f32,
    pub rms: f32,
}

/// Render the whole song to a 16-bit stereo WAV file.
///
/// `max_frames` truncates the output, which is handy for short previews.
pub fn render_to_wav(
    player: &mut Player,
    path: &Path,
    max_frames: Option<u64>,
) -> Result<RenderStats> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: player.sample_rate(),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;

    let mut left = vec![0.0f32; 1024];
    let mut right = vec![0.0f32; 1024];
    let mut frames = 0u64;
    let mut peak = 0.0f32;
    let mut sum_squares = 0.0f64;

    player.play();

    // Safety net: a zero-length song must not loop forever.
    let max_frames = max_frames
        .unwrap_or(u64::MAX)
        .min(player.sample_rate() as u64 * 60 * 60);

    while !player.is_finished() && frames < max_frames {
        player.fill(&mut left, &mut right);
        for i in 0..left.len() {
            let l = left[i].clamp(-1.0, 1.0);
            let r = right[i].clamp(-1.0, 1.0);

            peak = peak.max(l.abs()).max(r.abs());
            sum_squares += (l as f64) * (l as f64) + (r as f64) * (r as f64);

            writer.write_sample((l * i16::MAX as f32) as i16)?;
            writer.write_sample((r * i16::MAX as f32) as i16)?;
            frames += 1;
        }
    }

    writer.finalize()?;

    let samples = (frames as f64) * 2.0;
    let rms = if samples > 0.0 {
        (sum_squares / samples).sqrt() as f32
    } else {
        0.0
    };

    if frames == 0 {
        bail!("rendered zero frames");
    }

    Ok(RenderStats { frames, peak, rms })
}
