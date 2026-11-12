use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SupportedStreamConfig};

use super::player::{Command, Player};

/// Preferred output format: 44.1 kHz, stereo.
const PREFERRED_RATE: u32 = 44_100;

pub struct AudioHandle {
    /// Dropping this stops the audio stream.
    _stream: cpal::Stream,
    /// Commands for the audio thread.
    pub commands: Sender<Command>,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: SampleFormat,
    /// Fixed buffer size in frames that the device accepted (`None` means the
    /// driver default, which is also what `buffer_ms == 0` requests).
    pub buffer_frames: Option<u32>,
}

/// Open the default audio device and start pulling from `player`.
///
/// `make_player` is called with the sample rate the device actually accepted,
/// which is normally 44.1 kHz but may be the system mix rate.
///
/// `buffer_ms` requests a fixed buffer size in milliseconds; `0` uses the
/// driver default. If the device rejects the requested size, the stream is
/// rebuilt with the default size instead of failing.
pub fn start<F>(make_player: F, position: Arc<AtomicU64>, buffer_ms: u32) -> Result<AudioHandle>
where
    F: Fn(u32) -> Result<Player>,
{
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no default audio output device")?;

    let config = choose_config(&device)?;
    let sample_rate = config.sample_rate();

    let build = |buffer_frames: Option<u32>| -> Result<AudioHandle> {
        let player = make_player(sample_rate)?;
        let (commands, receiver) = mpsc::channel();
        let stream = build_stream(
            &device,
            &config,
            player,
            receiver,
            position.clone(),
            buffer_frames,
        )?;
        Ok(AudioHandle {
            _stream: stream,
            commands,
            sample_rate,
            channels: config.channels(),
            sample_format: config.sample_format(),
            buffer_frames,
        })
    };

    if buffer_ms > 0 {
        let frames = ((sample_rate as u64 * buffer_ms as u64) / 1_000).max(32) as u32;
        match build(Some(frames)) {
            Ok(handle) => return Ok(handle),
            Err(err) => eprintln!(
                "requested {buffer_ms} ms SoundFont buffer was rejected ({err:#}); \
                 using the driver default"
            ),
        }
    }

    build(None)
}

/// Prefer 44.1 kHz stereo; fall back to whatever the device offers.
fn choose_config(device: &cpal::Device) -> Result<SupportedStreamConfig> {
    let mut best: Option<(u8, SupportedStreamConfig)> = None;

    if let Ok(ranges) = device.supported_output_configs() {
        for range in ranges {
            if range.channels() != 2 {
                continue;
            }
            let min_rate = range.min_sample_rate();
            let max_rate = range.max_sample_rate();
            if min_rate > PREFERRED_RATE || max_rate < PREFERRED_RATE {
                continue;
            }
            // Higher score = nicer format for us.
            let score = match range.sample_format() {
                SampleFormat::F32 => 4,
                SampleFormat::I16 => 3,
                SampleFormat::U16 => 2,
                SampleFormat::I32 => 1,
                _ => 0,
            };
            if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                best = Some((score, range.with_sample_rate(PREFERRED_RATE)));
            }
        }
    }

    if let Some((_, config)) = best {
        return Ok(config);
    }

    device
        .default_output_config()
        .context("no supported audio output configuration")
}

fn build_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    player: Player,
    commands: Receiver<Command>,
    position: Arc<AtomicU64>,
    buffer_frames: Option<u32>,
) -> Result<cpal::Stream> {
    let mut stream_config = config.config();
    if let Some(frames) = buffer_frames {
        stream_config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    let channels = stream_config.channels as usize;

    let stream = match config.sample_format() {
        SampleFormat::F32 => make_stream::<f32, _>(
            device,
            stream_config,
            channels,
            player,
            commands,
            position,
            |sample| sample,
        )?,
        SampleFormat::I16 => make_stream::<i16, _>(
            device,
            stream_config,
            channels,
            player,
            commands,
            position,
            |sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16,
        )?,
        SampleFormat::U16 => make_stream::<u16, _>(
            device,
            stream_config,
            channels,
            player,
            commands,
            position,
            |sample| ((sample.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16,
        )?,
        SampleFormat::I32 => make_stream::<i32, _>(
            device,
            stream_config,
            channels,
            player,
            commands,
            position,
            |sample| (sample.clamp(-1.0, 1.0) * i32::MAX as f32) as i32,
        )?,
        SampleFormat::U8 => make_stream::<u8, _>(
            device,
            stream_config,
            channels,
            player,
            commands,
            position,
            |sample| ((sample.clamp(-1.0, 1.0) * 0.5 + 0.5) * u8::MAX as f32) as u8,
        )?,
        other => bail!("unsupported output sample format: {other:?}"),
    };

    Ok(stream)
}

fn make_stream<T, F>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut player: Player,
    commands: Receiver<Command>,
    position: Arc<AtomicU64>,
    convert: F,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    F: Fn(f32) -> T + Send + 'static,
{
    let mut left = vec![0.0f32; 4096];
    let mut right = vec![0.0f32; 4096];

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            // Commands are drained without blocking; the callback never waits.
            while let Ok(command) = commands.try_recv() {
                player.apply(command);
            }

            let frames = data.len() / channels;
            if left.len() < frames {
                left.resize(frames, 0.0);
                right.resize(frames, 0.0);
            }
            player.fill(&mut left[..frames], &mut right[..frames]);
            position.store(player.position_us(), Ordering::Relaxed);

            for (index, frame) in data.chunks_exact_mut(channels).enumerate() {
                if channels == 1 {
                    frame[0] = convert(0.5 * (left[index] + right[index]));
                } else {
                    frame[0] = convert(left[index]);
                    frame[1] = convert(right[index]);
                    for sample in &mut frame[2..] {
                        *sample = convert(0.0);
                    }
                }
            }
        },
        |error| eprintln!("audio output error: {error}"),
        None,
    )?;

    stream.play()?;
    Ok(stream)
}
