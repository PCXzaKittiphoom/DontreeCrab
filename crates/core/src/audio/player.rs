use std::sync::Arc;

use anyhow::Result;
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use crate::meter::MeterMap;
use crate::model::{EventKind, Song};

/// Highest voice limit rustysynth accepts.
pub const MAXIMUM_POLYPHONY: usize = 256;

/// Commands the UI thread can send to the audio thread.
#[derive(Debug, Clone)]
pub enum Command {
    Play,
    Pause,
    Stop,
    /// Seek to an absolute position in microseconds.
    Seek(u64),
    SetVolume(f32),
    /// Transpose in semitones, e.g. `-2` or `+3`.
    SetTranspose(i32),
    /// Playback speed multiplier (1.0 = the song's own tempo).
    SetSpeed(f64),
    /// Mute or unmute a single MIDI channel (Thailand's karaoke melody track
    /// lives on MIDI channel 9, i.e. index 8).
    SetChannelMuted {
        channel: u8,
        muted: bool,
    },
    /// 3-band equalizer gains in dB.
    SetEq {
        enabled: bool,
        low_db: f32,
        mid_db: f32,
        high_db: f32,
    },
    /// Reverb/chorus send multipliers (1.0 = keep the MIDI file's values).
    SetEffectSends {
        midi_effects: bool,
        rev_inst: f32,
        cho_inst: f32,
        rev_drum: f32,
        cho_drum: f32,
    },
}

#[derive(Debug, Clone)]
enum PlayKind {
    NoteOn {
        channel: i32,
        key: i32,
        velocity: i32,
    },
    NoteOff {
        channel: i32,
        key: i32,
    },
    Control {
        channel: i32,
        controller: i32,
        value: i32,
    },
    Program {
        channel: i32,
        program: i32,
    },
    Bend {
        channel: i32,
        value: i32,
    },
    ChannelPressure {
        channel: i32,
        value: i32,
    },
    KeyPressure {
        channel: i32,
        key: i32,
        value: i32,
    },
}

impl PlayKind {
    /// Events that describe persistent channel state, replayed after a seek.
    fn is_state(&self) -> bool {
        matches!(
            self,
            PlayKind::Control { .. }
                | PlayKind::Program { .. }
                | PlayKind::Bend { .. }
                | PlayKind::ChannelPressure { .. }
        )
    }
}

#[derive(Debug, Clone)]
struct PlayEvent {
    at_us: u64,
    kind: PlayKind,
}

/// Block-based renderer. Owns the synth and the event schedule.
///
/// The song position is kept in microseconds of *song time*, so changing the
/// playback speed only changes how fast that position advances — the audio
/// clock (samples rendered) can never drift.
pub struct Player {
    synth: Synthesizer,
    sample_rate: u32,
    events: Vec<PlayEvent>,
    next_event: usize,
    position: f64,
    end_us: f64,
    speed: f64,
    playing: bool,
    finished: bool,
    transpose: i32,
    muted_channels: u16,
    eq_enabled: bool,
    eq_left: [Biquad; 3],
    eq_right: [Biquad; 3],
    midi_effects: bool,
    rev_inst: f32,
    cho_inst: f32,
    rev_drum: f32,
    cho_drum: f32,
    /// Last raw CC91 / CC93 values per channel, kept so the sliders can
    /// rescale the sends without losing the values written in the file.
    cc_reverb: [i32; 16],
    cc_chorus: [i32; 16],
}

impl Player {
    /// `duration_us` is the musical length of the song; the renderer keeps
    /// going through the end of the final bar (plus two seconds for reverb
    /// tails), so the UI can stop on the next bar line.
    pub fn new(
        song: &Song,
        sound_font: Arc<SoundFont>,
        sample_rate: u32,
        transpose: i32,
    ) -> Result<Self> {
        Self::new_with_polyphony(song, sound_font, sample_rate, transpose, MAXIMUM_POLYPHONY)
    }

    /// Like [`Self::new`], but with a configurable voice limit (8–256).
    ///
    /// rustysynth defaults to 64 voices. Large SoundFonts layer several samples
    /// per note (velocity layers, long release tails), so a dense karaoke
    /// arrangement can exceed 64 even with only ~20 keys held at once and
    /// rustysynth starts stealing notes.
    pub fn new_with_polyphony(
        song: &Song,
        sound_font: Arc<SoundFont>,
        sample_rate: u32,
        transpose: i32,
        maximum_polyphony: usize,
    ) -> Result<Self> {
        let mut settings = SynthesizerSettings::new(sample_rate as i32);
        settings.maximum_polyphony = maximum_polyphony.clamp(8, MAXIMUM_POLYPHONY);
        let mut synth = Synthesizer::new(&sound_font, &settings)?;
        synth.set_master_volume(0.8);

        let mut events = Vec::with_capacity(song.events.len());
        for event in &song.events {
            if let Some(kind) = to_play_kind(&event.kind) {
                events.push(PlayEvent {
                    at_us: event.at_us,
                    kind,
                });
            }
        }
        events.sort_by_key(|e| e.at_us);

        let end_tick = song.tempo_map.us_to_tick(song.duration_us);
        let meter = MeterMap::new(song.ticks_per_quarter, song.time_signatures.clone());
        let end_us = song
            .tempo_map
            .tick_to_us(meter.bar_end_tick(end_tick))
            .saturating_add(2_000_000);

        Ok(Self {
            synth,
            sample_rate,
            events,
            next_event: 0,
            position: 0.0,
            end_us: end_us as f64,
            speed: 1.0,
            playing: false,
            finished: false,
            transpose: transpose.clamp(-24, 24),
            muted_channels: 0,
            eq_enabled: true,
            eq_left: [Biquad::default(); 3],
            eq_right: [Biquad::default(); 3],
            midi_effects: true,
            rev_inst: 1.0,
            cho_inst: 1.0,
            rev_drum: 1.0,
            cho_drum: 1.0,
            // rustysynth resets the GM sends to these values.
            cc_reverb: [40; 16],
            cc_chorus: [0; 16],
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Maximum simultaneous voices configured on the synthesizer.
    pub fn maximum_polyphony(&self) -> usize {
        self.synth.get_maximum_polyphony()
    }

    pub fn position_us(&self) -> u64 {
        self.position.max(0.0) as u64
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn play(&mut self) {
        if self.finished {
            self.seek(0);
        }
        self.playing = true;
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.synth.note_off_all(true);
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.synth.reset();
        self.cc_reverb = [40; 16];
        self.cc_chorus = [0; 16];
        self.resend_effects();
        self.position = 0.0;
        self.next_event = 0;
        self.finished = false;
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.synth.set_master_volume(volume.clamp(0.0, 2.0));
    }

    pub fn set_transpose(&mut self, semitones: i32) {
        self.synth.note_off_all(true);
        self.transpose = semitones.clamp(-24, 24);
    }

    /// Change the playback speed. 1.0 keeps the song's own tempo; 1.5 plays it
    /// half again as fast.
    pub fn set_speed(&mut self, speed: f64) {
        if speed.is_finite() {
            self.speed = speed.clamp(0.1, 10.0);
        }
    }

    /// Silence one MIDI channel without stopping the song. Notes that are
    /// already sounding are cut immediately; future note-ons on that channel
    /// are skipped until it is unmuted again.
    pub fn set_channel_muted(&mut self, channel: u8, muted: bool) {
        let bit = 1u16 << (channel & 15);
        if muted {
            self.muted_channels |= bit;
            self.synth.note_off_all_channel(channel as i32, true);
        } else {
            self.muted_channels &= !bit;
        }
    }

    fn is_muted(&self, channel: i32) -> bool {
        (0..16).contains(&channel) && (self.muted_channels & (1u16 << channel)) != 0
    }

    /// Configure the 3-band equalizer. Bands: low shelf (200 Hz),
    /// peaking mid (1 kHz) and high shelf (4 kHz), all ±12 dB.
    pub fn set_eq(&mut self, enabled: bool, low_db: f32, mid_db: f32, high_db: f32) {
        self.eq_enabled = enabled;
        set_eq_bands(&mut self.eq_left, self.sample_rate, low_db, mid_db, high_db);
        set_eq_bands(
            &mut self.eq_right,
            self.sample_rate,
            low_db,
            mid_db,
            high_db,
        );
    }

    /// Set the reverb/chorus send multipliers and immediately resend the
    /// scaled controller values to every channel.
    pub fn set_effect_sends(
        &mut self,
        midi_effects: bool,
        rev_inst: f32,
        cho_inst: f32,
        rev_drum: f32,
        cho_drum: f32,
    ) {
        self.midi_effects = midi_effects;
        self.rev_inst = rev_inst.clamp(0.0, 1.0);
        self.cho_inst = cho_inst.clamp(0.0, 1.0);
        self.rev_drum = rev_drum.clamp(0.0, 1.0);
        self.cho_drum = cho_drum.clamp(0.0, 1.0);
        self.resend_effects();
    }

    fn effect_send(&self, raw: i32, multiplier: f32) -> i32 {
        scale_effect(raw, multiplier, self.midi_effects)
    }

    fn resend_effects(&mut self) {
        for channel in 0..16usize {
            let (rev_mult, cho_mult) = if channel == 9 {
                (self.rev_drum, self.cho_drum)
            } else {
                (self.rev_inst, self.cho_inst)
            };
            let reverb = self.effect_send(self.cc_reverb[channel], rev_mult);
            let chorus = self.effect_send(self.cc_chorus[channel], cho_mult);
            self.synth
                .process_midi_message(channel as i32, 0xB0, 91, reverb);
            self.synth
                .process_midi_message(channel as i32, 0xB0, 93, chorus);
        }
    }

    pub fn apply(&mut self, command: Command) {
        match command {
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::Stop => self.stop(),
            Command::Seek(us) => {
                self.seek(us);
                self.playing = true;
            }
            Command::SetVolume(volume) => self.set_volume(volume),
            Command::SetTranspose(semitones) => self.set_transpose(semitones),
            Command::SetSpeed(speed) => self.set_speed(speed),
            Command::SetChannelMuted { channel, muted } => self.set_channel_muted(channel, muted),
            Command::SetEq {
                enabled,
                low_db,
                mid_db,
                high_db,
            } => self.set_eq(enabled, low_db, mid_db, high_db),
            Command::SetEffectSends {
                midi_effects,
                rev_inst,
                cho_inst,
                rev_drum,
                cho_drum,
            } => self.set_effect_sends(midi_effects, rev_inst, cho_inst, rev_drum, cho_drum),
        }
    }

    /// Jump to `us`, restoring the synth state (programs, controllers) that
    /// would be in effect at that point. Notes that were already sounding are
    /// not restarted.
    pub fn seek(&mut self, us: u64) {
        let target = (us as f64).min(self.end_us);
        self.synth.reset();
        self.cc_reverb = [40; 16];
        self.cc_chorus = [0; 16];
        self.next_event = self
            .events
            .partition_point(|event| (event.at_us as f64) < target);
        for index in 0..self.next_event {
            if self.events[index].kind.is_state() {
                self.dispatch(index);
            }
        }
        self.resend_effects();
        self.position = target;
        self.finished = false;
    }

    /// Render the next `left.len()` frames. Extra frames beyond the song end
    /// are silence.
    pub fn fill(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());

        if !self.playing || self.finished {
            left[..frames].fill(0.0);
            right[..frames].fill(0.0);
            return;
        }

        let us_per_frame = 1_000_000.0 / self.sample_rate as f64;
        let mut done = 0usize;

        while done < frames {
            if self.position >= self.end_us {
                left[done..frames].fill(0.0);
                right[done..frames].fill(0.0);
                break;
            }

            while self.next_event < self.events.len()
                && (self.events[self.next_event].at_us as f64) <= self.position
            {
                self.dispatch(self.next_event);
                self.next_event += 1;
            }

            // Render up to the next event or the end of the song. With speed
            // != 1.0, one output frame advances `speed` frames of song time.
            let mut len = frames - done;

            if let Some(next) = self.events.get(self.next_event) {
                let delta_us = (next.at_us as f64 - self.position).max(0.0);
                let frames_to_event = (delta_us / us_per_frame / self.speed).floor() as usize;
                if frames_to_event < len {
                    len = frames_to_event.max(1);
                }
            }

            let frames_to_end =
                ((self.end_us - self.position) / us_per_frame / self.speed).ceil() as usize;
            if frames_to_end < len {
                len = frames_to_end.max(1);
            }

            len = len.max(1).min(frames - done);

            self.synth
                .render(&mut left[done..done + len], &mut right[done..done + len]);

            if self.eq_enabled {
                for i in done..done + len {
                    let mut l = left[i];
                    let mut r = right[i];
                    for band in 0..3 {
                        l = self.eq_left[band].process(l);
                        r = self.eq_right[band].process(r);
                    }
                    left[i] = l;
                    right[i] = r;
                }
            }

            // Safety net: a hot mix must not hard-clip (that sounds "mushy").
            for i in done..done + len {
                left[i] = soft_limit(left[i]);
                right[i] = soft_limit(right[i]);
            }

            done += len;
            self.position += len as f64 * us_per_frame * self.speed;
        }

        if self.position >= self.end_us {
            self.finished = true;
        }
    }

    fn dispatch(&mut self, index: usize) {
        match self.events[index].kind.clone() {
            PlayKind::NoteOn {
                channel,
                key,
                velocity,
            } => {
                if !self.is_muted(channel) {
                    self.synth.note_on(
                        channel,
                        transpose_key(channel, key, self.transpose),
                        velocity,
                    );
                }
            }
            PlayKind::NoteOff { channel, key } => self
                .synth
                .note_off(channel, transpose_key(channel, key, self.transpose)),
            PlayKind::Control {
                channel,
                controller,
                value,
            } => match controller {
                // Reverb (91) and chorus (93) sends are scaled per group.
                91 => {
                    if (0..16).contains(&channel) {
                        self.cc_reverb[channel as usize] = value;
                    }
                    let multiplier = if channel == 9 {
                        self.rev_drum
                    } else {
                        self.rev_inst
                    };
                    let send = self.effect_send(value, multiplier);
                    self.synth.process_midi_message(channel, 0xB0, 91, send);
                }
                93 => {
                    if (0..16).contains(&channel) {
                        self.cc_chorus[channel as usize] = value;
                    }
                    let multiplier = if channel == 9 {
                        self.cho_drum
                    } else {
                        self.cho_inst
                    };
                    let send = self.effect_send(value, multiplier);
                    self.synth.process_midi_message(channel, 0xB0, 93, send);
                }
                _ => self
                    .synth
                    .process_midi_message(channel, 0xB0, controller, value),
            },
            PlayKind::Program { channel, program } => {
                self.synth.process_midi_message(channel, 0xC0, program, 0)
            }
            PlayKind::Bend { channel, value } => {
                self.synth
                    .process_midi_message(channel, 0xE0, value & 0x7F, (value >> 7) & 0x7F)
            }
            PlayKind::ChannelPressure { channel, value } => {
                self.synth.process_midi_message(channel, 0xD0, value, 0)
            }
            PlayKind::KeyPressure {
                channel,
                key,
                value,
            } => self.synth.process_midi_message(channel, 0xA0, key, value),
        }
    }
}

/// General MIDI channel 10 (index 9) is a percussion channel: shifting its
/// note numbers selects different drum sounds instead of transposing a pitch,
/// so percussion is never transposed.
fn transpose_key(channel: i32, key: i32, semitones: i32) -> i32 {
    if channel == 9 {
        key
    } else {
        (key + semitones).clamp(0, 127)
    }
}

// ------------------------------------------------------------------------ EQ

/// One second-order filter (biquad) in transposed direct form II.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        // Identity filter.
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }
}

impl Biquad {
    fn set_coefficients(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        let inverse = 1.0 / a0;
        self.b0 = b0 * inverse;
        self.b1 = b1 * inverse;
        self.b2 = b2 * inverse;
        self.a1 = a1 * inverse;
        self.a2 = a2 * inverse;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn set_low_shelf(&mut self, sample_rate: f64, frequency: f64, gain_db: f32) {
        let (b0, b1, b2, a0, a1, a2) = low_shelf(sample_rate, frequency, gain_db);
        self.set_coefficients(b0, b1, b2, a0, a1, a2);
    }

    fn set_high_shelf(&mut self, sample_rate: f64, frequency: f64, gain_db: f32) {
        let (b0, b1, b2, a0, a1, a2) = high_shelf(sample_rate, frequency, gain_db);
        self.set_coefficients(b0, b1, b2, a0, a1, a2);
    }

    fn set_peaking(&mut self, sample_rate: f64, frequency: f64, gain_db: f32, q: f64) {
        let (b0, b1, b2, a0, a1, a2) = peaking(sample_rate, frequency, gain_db, q);
        self.set_coefficients(b0, b1, b2, a0, a1, a2);
    }
}

/// Configure the three bands: low shelf 200 Hz, peak 1 kHz, high shelf 4 kHz.
fn set_eq_bands(bands: &mut [Biquad; 3], sample_rate: u32, low_db: f32, mid_db: f32, high_db: f32) {
    let rate = sample_rate.max(1) as f64;
    bands[0].set_low_shelf(rate, 200.0, low_db);
    bands[1].set_peaking(rate, 1_000.0, mid_db, 0.9);
    bands[2].set_high_shelf(rate, 4_000.0, high_db);
}

// RBJ audio EQ cookbook coefficients (S = 1 for the shelves).

fn shelf_a(gain_db: f32) -> f64 {
    10f64.powf(gain_db as f64 / 40.0)
}

fn low_shelf(sample_rate: f64, frequency: f64, gain_db: f32) -> (f32, f32, f32, f32, f32, f32) {
    let a = shelf_a(gain_db);
    let w0 = 2.0 * std::f64::consts::PI * frequency / sample_rate;
    let cos = w0.cos();
    let sin = w0.sin();
    let alpha = 0.5 * sin * std::f64::consts::SQRT_2;
    let k = 2.0 * a.sqrt() * alpha;

    let b0 = a * ((a + 1.0) - (a - 1.0) * cos + k);
    let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos);
    let b2 = a * ((a + 1.0) - (a - 1.0) * cos - k);
    let a0 = (a + 1.0) + (a - 1.0) * cos + k;
    let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos);
    let a2 = (a + 1.0) + (a - 1.0) * cos - k;

    (
        b0 as f32, b1 as f32, b2 as f32, a0 as f32, a1 as f32, a2 as f32,
    )
}

fn high_shelf(sample_rate: f64, frequency: f64, gain_db: f32) -> (f32, f32, f32, f32, f32, f32) {
    let a = shelf_a(gain_db);
    let w0 = 2.0 * std::f64::consts::PI * frequency / sample_rate;
    let cos = w0.cos();
    let sin = w0.sin();
    let alpha = 0.5 * sin * std::f64::consts::SQRT_2;
    let k = 2.0 * a.sqrt() * alpha;

    let b0 = a * ((a + 1.0) + (a - 1.0) * cos + k);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos);
    let b2 = a * ((a + 1.0) + (a - 1.0) * cos - k);
    let a0 = (a + 1.0) - (a - 1.0) * cos + k;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos);
    let a2 = (a + 1.0) - (a - 1.0) * cos - k;

    (
        b0 as f32, b1 as f32, b2 as f32, a0 as f32, a1 as f32, a2 as f32,
    )
}

fn peaking(
    sample_rate: f64,
    frequency: f64,
    gain_db: f32,
    q: f64,
) -> (f32, f32, f32, f32, f32, f32) {
    let a = shelf_a(gain_db);
    let w0 = 2.0 * std::f64::consts::PI * frequency / sample_rate;
    let cos = w0.cos();
    let alpha = w0.sin() / (2.0 * q);

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos;
    let a2 = 1.0 - alpha / a;

    (
        b0 as f32, b1 as f32, b2 as f32, a0 as f32, a1 as f32, a2 as f32,
    )
}

/// Scale a raw MIDI controller value by a send multiplier.
fn scale_effect(raw: i32, multiplier: f32, enabled: bool) -> i32 {
    if !enabled {
        return 0;
    }
    let raw = raw.clamp(0, 127) as f32;
    (raw * multiplier.clamp(0.0, 1.0)).round().clamp(0.0, 127.0) as i32
}

fn to_play_kind(kind: &EventKind) -> Option<PlayKind> {
    let play = match kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
        } => PlayKind::NoteOn {
            channel: *channel as i32,
            key: *key as i32,
            velocity: *velocity as i32,
        },
        EventKind::NoteOff {
            channel,
            key,
            velocity: _,
        } => PlayKind::NoteOff {
            channel: *channel as i32,
            key: *key as i32,
        },
        EventKind::ControlChange {
            channel,
            controller,
            value,
        } => PlayKind::Control {
            channel: *channel as i32,
            controller: *controller as i32,
            value: *value as i32,
        },
        EventKind::ProgramChange { channel, program } => PlayKind::Program {
            channel: *channel as i32,
            program: *program as i32,
        },
        EventKind::PitchBend { channel, value } => PlayKind::Bend {
            channel: *channel as i32,
            value: *value as i32 + 8192,
        },
        EventKind::ChannelPressure { channel, value } => PlayKind::ChannelPressure {
            channel: *channel as i32,
            value: *value as i32,
        },
        EventKind::KeyPressure {
            channel,
            key,
            value,
        } => PlayKind::KeyPressure {
            channel: *channel as i32,
            key: *key as i32,
            value: *value as i32,
        },
        _ => return None,
    };
    Some(play)
}

/// Gentle limiter so a hot mix cannot hard-clip (clipping sounds distorted).
#[inline]
fn soft_limit(sample: f32) -> f32 {
    const KNEE: f32 = 0.85;
    let magnitude = sample.abs();
    if magnitude <= KNEE {
        sample
    } else {
        let range = 1.0 - KNEE;
        let excess = (magnitude - KNEE) / range;
        sample.signum() * (KNEE + range * excess.tanh())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percussion_is_never_transposed() {
        assert_eq!(transpose_key(9, 36, 5), 36);
        assert_eq!(transpose_key(9, 36, -5), 36);
        assert_eq!(transpose_key(0, 60, 5), 65);
        assert_eq!(transpose_key(0, 60, -12), 48);
        assert_eq!(transpose_key(0, 5, -12), 0);
        assert_eq!(transpose_key(0, 120, 12), 127);
    }

    #[test]
    fn limiter_keeps_signal_in_range() {
        for sample in [-4.0, -1.0, -0.5, 0.0, 0.5, 1.0, 4.0] {
            let out = soft_limit(sample);
            assert!(out.abs() <= 1.0, "{sample} -> {out}");
            assert!(out * sample >= 0.0, "sign changed: {sample} -> {out}");
        }
        // Transparent below the knee.
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.5), -0.5);
    }

    #[test]
    fn flat_eq_is_transparent() {
        let mut bands = [Biquad::default(); 3];
        set_eq_bands(&mut bands, 44_100, 0.0, 0.0, 0.0);

        for input in [0.25, -0.5, 0.1, -0.2, 0.0, 0.7] {
            let mut output = input;
            for band in &mut bands {
                output = band.process(output);
            }
            assert!(
                (output - input).abs() < 1e-4,
                "{input} -> {output} is not transparent"
            );
        }
    }

    #[test]
    fn eq_boosts_the_expected_band() {
        // Feed a 100 Hz sine through a +12 dB low shelf and compare energy.
        let rate = 44_100u32;
        let input: Vec<f32> = (0..rate)
            .map(|i| (2.0 * std::f32::consts::PI * 100.0 * i as f32 / rate as f32).sin() * 0.25)
            .collect();

        let mut bands = [Biquad::default(); 3];
        set_eq_bands(&mut bands, rate, 12.0, 0.0, 0.0);

        let output: Vec<f32> = input
            .iter()
            .map(|&x| {
                let mut y = x;
                for band in &mut bands {
                    y = band.process(y);
                }
                y
            })
            .collect();

        // Skip the start-up transient and compare RMS.
        let measure = |data: &[f32]| {
            let slice = &data[2_000..];
            (slice.iter().map(|x| x * x).sum::<f32>() / slice.len() as f32).sqrt()
        };
        let gain = measure(&output) / measure(&input);
        // +12 dB is roughly 4x; allow for the shelf's slope at 100 Hz.
        assert!(gain > 2.5, "low band gain was only {gain:.2}x");
    }

    #[test]
    fn effect_sends_are_scaled_and_can_be_muted() {
        assert_eq!(scale_effect(64, 1.0, true), 64);
        assert_eq!(scale_effect(64, 0.5, true), 32);
        assert_eq!(scale_effect(64, 0.0, true), 0);
        assert_eq!(scale_effect(64, 1.0, false), 0);
        assert_eq!(scale_effect(200, 1.0, true), 127);
        assert_eq!(scale_effect(-5, 1.0, true), 0);
    }
}
