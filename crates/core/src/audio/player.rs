use std::sync::Arc;

use anyhow::Result;
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use super::midi_out::MidiRouting;
use super::monitor::MidiMonitor;
use crate::meter::MeterMap;
use crate::model::{EventKind, Song};
use crate::settings::{MidiReset, TransposeMode};

/// Highest voice limit rustysynth accepts.
pub const MAXIMUM_POLYPHONY: usize = 256;

/// Voice and drum fixes applied while a song plays.
///
/// All note/program numbers are General MIDI names, 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiOptions {
    /// Force every bass program (GM 33-40) to `bass_program`.
    pub lock_bass: bool,
    pub bass_program: u8,
    /// Force the drum channel to one kit.
    pub lock_drum_kit: bool,
    pub drum_program: u8,
    /// Force kick notes (35/36) to `kick_note`.
    pub lock_kick: bool,
    pub kick_note: u8,
    /// Force snare notes (31 sticks, 38-40) to `snare_note`.
    ///
    /// 37 (side stick) is deliberately left alone: it is a different
    /// instrument and songs use it as a rim click.
    pub lock_snare: bool,
    pub snare_note: u8,
    /// Click track on the drum channel.
    pub metronome: bool,
    /// Use a closed hi-hat (42) for every beat instead of the two notes below.
    pub metronome_hihat: bool,
    pub metronome_first: u8,
    pub metronome_other: u8,
    /// Send a General MIDI reset when the options are applied (`MIDI SysEx`).
    pub sysex: bool,
    /// Per-instrument-group MIDI volume (100 = unchanged, 127 = maximum).
    pub group_volume: [u8; crate::audio::GROUPS],
    /// Per-instrument-group note-velocity window.
    pub group_velocity_min: [u8; crate::audio::GROUPS],
    pub group_velocity_max: [u8; crate::audio::GROUPS],
    /// Per-drum-group MIDI volume (100 = unchanged, 127 = maximum).
    pub drum_volume: [u8; crate::audio::DRUM_GROUPS],
    /// Per-drum-group note-velocity window.
    pub drum_velocity_min: [u8; crate::audio::DRUM_GROUPS],
    pub drum_velocity_max: [u8; crate::audio::DRUM_GROUPS],
    /// How a key change treats the notes that are already sounding.
    pub transpose_mode: TransposeMode,
    /// Reset command sent when starting a song (`MIDI SysEx`).
    pub reset: MidiReset,
}

impl Default for MidiOptions {
    fn default() -> Self {
        Self {
            lock_bass: false,
            bass_program: 33,
            lock_drum_kit: false,
            drum_program: 0,
            lock_kick: false,
            kick_note: 36,
            lock_snare: false,
            snare_note: 38,
            metronome: false,
            metronome_hihat: false,
            metronome_first: 75,
            metronome_other: 76,
            group_volume: [100; crate::audio::GROUPS],
            group_velocity_min: [0; crate::audio::GROUPS],
            group_velocity_max: [127; crate::audio::GROUPS],
            drum_volume: [100; crate::audio::DRUM_GROUPS],
            drum_velocity_min: [0; crate::audio::DRUM_GROUPS],
            drum_velocity_max: [127; crate::audio::DRUM_GROUPS],
            sysex: false,
            transpose_mode: TransposeMode::default(),
            reset: MidiReset::default(),
        }
    }
}

/// Commands the UI thread can send to the audio thread.
#[derive(Debug)]
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
    /// Set the complete mute mask at once (used by the controller's Solo).
    SetChannelMutes {
        mask: u16,
    },
    /// Set one channel's CC7 volume from the controller fader.
    SetChannelVolume {
        channel: u8,
        value: u8,
    },
    /// Move one channel's pitch bend from the controller wheel.
    SetChannelBend {
        channel: u8,
        value: i16,
    },
    /// Play a note from the on-screen piano.
    NoteOn {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    /// Release a note played from the on-screen piano.
    NoteOff {
        channel: u8,
        key: u8,
    },
    /// Set one channel's modulation (CC1) from the controller wheel.
    SetChannelModulation {
        channel: u8,
        value: u8,
    },
    /// Select a program on one channel from the channel detail popup.
    SetChannelProgram {
        channel: u8,
        program: u8,
    },
    /// Select a bank (CC0, 0-127) on one channel.
    SetChannelBank {
        channel: u8,
        bank: u8,
    },
    /// Set one channel's pan (CC10, 0=left, 64=centre, 127=right).
    SetChannelPan {
        channel: u8,
        value: u8,
    },
    /// Set one channel's reverb send (CC91).
    SetChannelReverb {
        channel: u8,
        value: u8,
    },
    /// Set one channel's chorus send (CC93).
    SetChannelChorus {
        channel: u8,
        value: u8,
    },
    /// Temporarily transpose one channel (semitones, -24..24).
    SetChannelTranspose {
        channel: u8,
        semitones: i32,
    },
    /// Lock or unlock the controller values of the given channels.
    SetChannelLocks {
        mask: u16,
    },
    /// 3-band equalizer gains in dB.
    SetEq {
        enabled: bool,
        low_db: f32,
        mid_db: f32,
        high_db: f32,
    },
    /// Voice/drum fixes and the metronome.
    SetMidiOptions(MidiOptions),
    /// Route the song to external MIDI devices (per channel).
    SetMidiRouting(MidiRouting),
    /// Send a MIDI reset now (the Reset MIDI button).
    MidiReset(MidiReset),
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

/// One note that is currently sounding (used to retune held notes).
#[derive(Debug, Clone, Copy)]
struct ActiveNote {
    channel: i32,
    /// Key as written in the file, before transposition and locks.
    source_key: i32,
    velocity: i32,
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
    /// Temporary per-channel transpose, added to `transpose` (drums ignored).
    channel_transpose: [i32; 16],
    /// Channels whose volume/pan/reverb/chorus ignore song CCs.
    channel_locks: u16,
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
    midi: MidiOptions,
    /// Program the file asked for, per channel (-1 = not seen yet).
    source_program: [i32; 16],
    /// Program last sent to the synth, per channel.
    sent_program: [i32; 16],
    /// Last CC7 value seen or set, per channel.
    channel_volume: [u8; 16],
    /// Last CC0 (bank MSB) value, per channel.
    channel_bank: [u8; 16],
    /// Last CC10 pan value, per channel.
    channel_pan: [u8; 16],
    /// Live state shown by the MIDI Controller.
    monitor: Option<Arc<MidiMonitor>>,
    /// External MIDI outputs and the per-channel routing.
    routing: MidiRouting,
    /// Notes that are sounding right now, so a key change can retune them.
    active_notes: Vec<ActiveNote>,
    /// `(tick, first beat of the bar)` for the click track.
    beats: Vec<(u64, bool)>,
    tempo_map: crate::tempo::TempoMap,
    /// Song events without the click track, so it can be rebuilt.
    base_events: Vec<PlayEvent>,
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
        let mut beats = Vec::new();
        let mut tick = 0u64;
        while tick <= end_tick {
            let beat = meter.beat_ticks_at(tick).max(1);
            let bar_start = meter.bar_start_tick(tick);
            beats.push((tick, tick == bar_start));
            tick += beat;
        }
        let end_us = song
            .tempo_map
            .tick_to_us(meter.bar_end_tick(end_tick))
            .saturating_add(2_000_000);

        let base_events = events.clone();
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
            channel_transpose: [0; 16],
            channel_locks: 0,
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
            midi: MidiOptions::default(),
            source_program: [-1; 16],
            sent_program: [-1; 16],
            channel_volume: [100; 16],
            channel_bank: [0; 16],
            channel_pan: [64; 16],
            monitor: None,
            routing: MidiRouting::internal(),
            active_notes: Vec::new(),
            beats,
            tempo_map: song.tempo_map.clone(),
            base_events,
        })
    }

    /// A silent player for the on-screen keyboard when no song is open.
    ///
    /// It keeps the synthesizer alive with no scheduled events, so audition
    /// notes sent through [`Command::NoteOn`] can sound immediately.
    pub fn new_preview(
        sound_font: Arc<SoundFont>,
        sample_rate: u32,
        transpose: i32,
        maximum_polyphony: usize,
    ) -> Result<Self> {
        let mut settings = SynthesizerSettings::new(sample_rate as i32);
        settings.maximum_polyphony = maximum_polyphony.clamp(8, MAXIMUM_POLYPHONY);
        let synth = Synthesizer::new(&sound_font, &settings)?;

        Ok(Self {
            synth,
            sample_rate,
            events: Vec::new(),
            next_event: 0,
            position: 0.0,
            end_us: f64::MAX,
            speed: 1.0,
            playing: true,
            finished: false,
            transpose: transpose.clamp(-24, 24),
            channel_transpose: [0; 16],
            channel_locks: 0,
            muted_channels: 0,
            eq_enabled: true,
            eq_left: [Biquad::default(); 3],
            eq_right: [Biquad::default(); 3],
            midi_effects: true,
            rev_inst: 1.0,
            cho_inst: 1.0,
            rev_drum: 1.0,
            cho_drum: 1.0,
            cc_reverb: [40; 16],
            cc_chorus: [0; 16],
            midi: MidiOptions::default(),
            source_program: [-1; 16],
            sent_program: [-1; 16],
            channel_volume: [100; 16],
            channel_bank: [0; 16],
            channel_pan: [64; 16],
            monitor: None,
            routing: MidiRouting::internal(),
            active_notes: Vec::new(),
            beats: Vec::new(),
            tempo_map: crate::tempo::TempoMap::new(96),
            base_events: Vec::new(),
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
        self.active_notes.clear();
        self.synth.note_off_all(true);
        self.routing.panic_all();
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.active_notes.clear();
        if let Some(monitor) = &self.monitor {
            monitor.clear_notes();
        }
        self.synth.reset();
        self.routing.panic_all();
        self.source_program = [-1; 16];
        self.sent_program = [-1; 16];
        self.channel_transpose = [0; 16];
        self.channel_locks = 0;
        self.channel_volume = [100; 16];
        self.channel_bank = [0; 16];
        self.channel_pan = [64; 16];
        self.cc_reverb = [40; 16];
        self.cc_chorus = [0; 16];
        if let Some(monitor) = &self.monitor {
            monitor.reset();
        }
        self.resend_effects();
        self.position = 0.0;
        self.next_event = 0;
        self.finished = false;
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.synth.set_master_volume(volume.clamp(0.0, 2.0));
    }

    /// Voice/drum fixes for the current song, plus the click track.
    ///
    /// Everything applies immediately: programs whose mapping changed are sent
    /// again so a lock is audible without restarting the song, and the click
    /// track is rebuilt around the current playhead.
    pub fn set_midi_options(&mut self, options: MidiOptions) {
        let sysex_was = self.midi.sysex;
        // Re-mapping a drum key while it sounds would leave the note ringing,
        // because its note-off would be translated with the new mapping.
        let drum_keys_changed = self.midi.lock_kick != options.lock_kick
            || self.midi.kick_note != options.kick_note
            || self.midi.lock_snare != options.lock_snare
            || self.midi.snare_note != options.snare_note;
        if drum_keys_changed {
            self.release_remapped_drums(&options);
        }
        let metronome_changed = self.midi.metronome != options.metronome
            || self.midi.metronome_hihat != options.metronome_hihat
            || self.midi.metronome_first != options.metronome_first
            || self.midi.metronome_other != options.metronome_other;
        self.midi = options;

        for channel in 0..16 {
            let source = self.source_program[channel];
            if source < 0 {
                continue;
            }
            let mapped = map_program(&self.midi, channel as i32, source);
            if self.sent_program[channel] != mapped || (channel == 9 && self.midi.lock_drum_kit) {
                if channel == 9 && self.midi.lock_drum_kit {
                    self.pin_drum_bank();
                }
                let route = self.program_route(channel as i32, mapped);
                if route.is_none() {
                    self.synth
                        .process_midi_message(channel as i32, 0xC0, mapped, 0);
                }
                self.routing.send(
                    route,
                    &[0xC0 | channel_byte(channel as i32), midi_data(mapped)],
                );
                self.sent_program[channel] = mapped;
            }
        }

        self.apply_drum_kit();

        if self.midi.sysex && !sysex_was {
            if let Some(sysex) = self.midi.reset.sysex() {
                self.routing.send_all(sysex);
            }
        }

        if metronome_changed {
            self.rebuild_metronome();
            // Skip the clicks that are already behind the playhead.
            let position = self.position.max(0.0) as u64;
            self.next_event = self.events.partition_point(|event| event.at_us <= position);
        }
    }

    /// Replace the external outputs and the per-channel routing.
    ///
    /// The programs and controllers that are in effect at the playhead are
    /// replayed, so the devices start in sync even in the middle of a song.
    pub fn set_midi_routing(&mut self, routing: MidiRouting) {
        self.routing.panic_all();
        self.routing = routing;
        if self.midi.sysex {
            if let Some(sysex) = self.midi.reset.sysex() {
                self.routing.send_all(sysex);
            }
        }
        let position = self.position_us();
        self.seek(position);
    }

    /// Apply a MIDI reset right now.
    ///
    /// The local synthesizer is always reset, and every external device gets the
    /// controller reset plus the selected SysEx (when it has one). The song's
    /// programs and controllers are then rebuilt at the playhead so the music
    /// continues correctly.
    pub fn reset_midi(&mut self, reset: MidiReset) {
        self.active_notes.clear();
        self.routing.reset_all();
        if let Some(sysex) = reset.sysex() {
            self.routing.send_all(sysex);
        }
        let position = self.position_us();
        self.seek(position);
    }

    /// Program that is actually sounding on `channel` (0 when the file has
    /// not selected one).
    fn effective_program(&self, channel: i32) -> u8 {
        if !(0..16).contains(&channel) {
            return 0;
        }
        let channel = channel as usize;
        let program = if self.sent_program[channel] >= 0 {
            self.sent_program[channel]
        } else {
            self.source_program[channel]
        };
        program.clamp(0, 127) as u8
    }

    /// Output index for `channel` (`None` = the built-in synthesizer).
    fn route_for(&self, channel: i32) -> Option<usize> {
        if !(0..16).contains(&channel) {
            return None;
        }
        self.routing
            .route_for(channel as usize, self.effective_program(channel))
    }

    /// Output index for a program change that selects `program` on `channel`.
    fn program_route(&self, channel: i32, program: i32) -> Option<usize> {
        if !(0..16).contains(&channel) {
            return None;
        }
        self.routing
            .route_for(channel as usize, program.clamp(0, 127) as u8)
    }

    /// Apply the instrument-group volume and velocity window to one note.
    fn group_velocity(&self, channel: i32, velocity: i32) -> i32 {
        if !(0..16).contains(&channel) {
            return velocity;
        }
        group_scaled_velocity(
            &self.midi,
            channel,
            self.effective_program(channel),
            velocity,
        )
    }

    /// Apply the drum-group volume and velocity window to one note.
    fn drum_velocity(&self, key: i32, velocity: i32) -> i32 {
        if velocity <= 0 {
            return velocity;
        }
        drum_scaled_velocity(&self.midi, key, velocity)
    }

    /// True when `channel` is played by the built-in synthesizer.
    fn channel_on_synth(&self, channel: i32) -> bool {
        self.route_for(channel).is_none()
    }

    /// Forward one message to the output selected for `channel`.
    fn send_to_midi_out(&mut self, channel: i32, message: &[u8]) {
        // Drum notes follow their own group, even when the message is sent by
        // a path that only knows the channel (drum-key remapping, for example).
        let note = message
            .first()
            .is_some_and(|status| matches!(status & 0xF0, 0x80 | 0x90));
        let route = if channel == 9 && note {
            self.routing
                .route_for_drum(message.get(1).copied().unwrap_or(0))
        } else {
            self.route_for(channel)
        };
        self.routing.send(route, message);
    }

    /// Put the drum channel back in the percussion bank (128).
    ///
    /// rustysynth turns a bank select on channel 10 into `128 + CC0` and falls
    /// back to the standard kit when that bank has no such patch. Files that
    /// send CC0 = 120 (very common) would therefore ignore the locked kit.
    fn pin_drum_bank(&mut self) {
        if self.channel_on_synth(9) {
            self.synth.process_midi_message(9, 0xB0, 0, 0);
        }
        self.send_to_midi_out(9, &[0xB0 | 9, 0, 0]);
    }

    /// Send the locked drum kit when the file never selects one.
    fn apply_drum_kit(&mut self) {
        if !self.midi.lock_drum_kit || self.source_program[9] >= 0 {
            return;
        }
        let patch = i32::from(self.midi.drum_program);
        self.pin_drum_bank();
        if self.channel_on_synth(9) {
            self.synth.process_midi_message(9, 0xC0, patch, 0);
        }
        self.send_to_midi_out(9, &[0xC0 | 9, midi_data(patch)]);
        self.sent_program[9] = patch;
    }

    fn rebuild_metronome(&mut self) {
        self.events = self.base_events.clone();
        self.next_event = 0;
        if !self.midi.metronome {
            return;
        }
        let mut clicks = Vec::with_capacity(self.beats.len() * 2);
        for &(tick, first) in &self.beats {
            let at = self.tempo_map.tick_to_us(tick);
            let note = beat_note(&self.midi, first);
            // The closed hi-hat is much quieter than the claves it replaces in
            // most SoundFonts, so it is played at full velocity.
            let velocity = if self.midi.metronome_hihat { 127 } else { 100 };
            clicks.push(PlayEvent {
                at_us: at,
                kind: PlayKind::NoteOn {
                    channel: 9,
                    key: note,
                    velocity,
                },
            });
            clicks.push(PlayEvent {
                at_us: at + 40_000,
                kind: PlayKind::NoteOff {
                    channel: 9,
                    key: note,
                },
            });
        }
        self.events.extend(clicks);
        self.events.sort_by_key(|event| event.at_us);
    }

    /// Change the key.
    ///
    /// What happens to the notes that are already sounding depends on
    /// [`MidiOptions::transpose_mode`]:
    ///
    /// * `All` retunes every channel in one pass (Roland "All Tuner").
    /// * `Wolf` does the same channel by channel.
    /// * `Recovery` cuts them and only transposes what follows.
    pub fn set_transpose(&mut self, semitones: i32) {
        let previous = self.transpose;
        let next = semitones.clamp(-24, 24);
        if next == previous {
            return;
        }
        self.transpose = next;
        if let Some(monitor) = &self.monitor {
            monitor.clear_notes();
        }
        match self.midi.transpose_mode {
            TransposeMode::Recovery => {
                self.active_notes.clear();
                self.synth.note_off_all(true);
                self.routing.panic_all();
            }
            TransposeMode::All => self.retrigger_notes(previous, false),
            TransposeMode::Wolf => self.retrigger_notes(previous, true),
        }
    }

    /// Stop the sounding drum notes whose key would be remapped.
    ///
    /// Without this a note that is already playing would get its note-off
    /// translated with the new mapping and ring until the song ends.
    fn release_remapped_drums(&mut self, options: &MidiOptions) {
        let notes: Vec<ActiveNote> = self
            .active_notes
            .iter()
            .copied()
            .filter(|note| note.channel == 9)
            .collect();
        let mut released: Vec<i32> = Vec::new();
        for note in notes {
            let old_key = map_key(&self.midi, 9, note.source_key);
            if old_key == map_key(options, 9, note.source_key) {
                continue;
            }
            if self.channel_on_synth(9) {
                self.synth.note_off(9, old_key);
            }
            self.send_to_midi_out(9, &[0x80 | 9, midi_data(old_key), 0]);
            released.push(note.source_key);
        }
        self.active_notes
            .retain(|note| !(note.channel == 9 && released.contains(&note.source_key)));
    }

    /// Re-send the sounding notes so they follow the new key.
    fn retrigger_notes(&mut self, previous: i32, per_channel: bool) {
        // The drum channel is never transposed, so retriggering it would only
        // add an extra (wrong) hit.
        let notes: Vec<ActiveNote> = self
            .active_notes
            .iter()
            .copied()
            .filter(|note| note.channel != 9)
            .collect();
        if per_channel {
            for channel in 0..16 {
                for note in notes.iter().filter(|note| note.channel == channel) {
                    self.send_active_note(note, previous, false);
                }
                for note in notes.iter().filter(|note| note.channel == channel) {
                    self.send_active_note(note, self.transpose, true);
                }
            }
        } else {
            for note in &notes {
                self.send_active_note(note, previous, false);
            }
            for note in &notes {
                self.send_active_note(note, self.transpose, true);
            }
        }
    }

    /// Send one note on/off for a sounding note at `transpose`.
    fn send_active_note(&mut self, note: &ActiveNote, transpose: i32, on: bool) {
        let total = transpose + self.channel_transpose[note.channel as usize];
        let key = map_key(
            &self.midi,
            note.channel,
            transpose_key(note.channel, note.source_key, total),
        );
        if on {
            if self.channel_on_synth(note.channel) {
                self.synth.note_on(note.channel, key, note.velocity);
            }
            self.send_to_midi_out(
                note.channel,
                &[
                    0x90 | channel_byte(note.channel),
                    midi_data(key),
                    midi_data(note.velocity),
                ],
            );
        } else {
            if self.channel_on_synth(note.channel) {
                self.synth.note_off(note.channel, key);
            }
            self.send_to_midi_out(
                note.channel,
                &[0x80 | channel_byte(note.channel), midi_data(key), 0],
            );
        }
    }

    /// Forget one sounding note (the note-off was played or the note ended).
    fn forget_note(&mut self, channel: i32, source_key: i32) {
        if let Some(index) = self
            .active_notes
            .iter()
            .position(|note| note.channel == channel && note.source_key == source_key)
        {
            self.active_notes.remove(index);
        }
    }

    /// Change the playback speed. 1.0 keeps the song's own tempo; 1.5 plays it
    /// half again as fast.
    pub fn set_speed(&mut self, speed: f64) {
        if speed.is_finite() {
            self.speed = speed.clamp(0.1, 10.0);
        }
    }

    /// Attach the live monitor shown by the MIDI Controller.
    pub fn set_monitor(&mut self, monitor: Arc<MidiMonitor>) {
        monitor.reset();
        monitor.set_muted_mask(self.muted_channels);
        for channel in 0..16 {
            monitor.set_volume(channel as u8, self.channel_volume[channel]);
            if self.sent_program[channel] >= 0 {
                monitor.set_program(
                    channel as u8,
                    self.sent_program[channel].clamp(0, 127) as u8,
                );
            }
        }
        self.monitor = Some(monitor);
    }

    /// Replace the complete mute mask (used by Solo).
    pub fn set_channel_mutes(&mut self, mask: u16) {
        let changed = mask ^ self.muted_channels;
        for channel in 0..16u8 {
            if changed & (1u16 << channel) != 0 {
                self.set_channel_muted(channel, mask & (1u16 << channel) != 0);
            }
        }
        if let Some(monitor) = &self.monitor {
            monitor.set_muted_mask(mask);
        }
    }

    /// Set one channel's CC7 volume from the controller fader.
    pub fn set_channel_volume(&mut self, channel: u8, value: u8) {
        if channel >= 16 || self.channel_locked(channel as i32) {
            return;
        }
        let value = value.min(127);
        self.channel_volume[channel as usize] = value;
        if self.channel_on_synth(channel as i32) {
            self.synth
                .process_midi_message(channel as i32, 0xB0, 7, i32::from(value));
        }
        self.send_to_midi_out(
            channel as i32,
            &[0xB0 | channel_byte(channel as i32), 7, value],
        );
        if let Some(monitor) = &self.monitor {
            monitor.set_volume(channel, value);
        }
    }

    /// Move one channel's pitch bend from the controller wheel.
    pub fn set_channel_bend(&mut self, channel: u8, value: i16) {
        if channel >= 16 {
            return;
        }
        let value = value.clamp(-8192, 8191);
        let raw = i32::from(value) + 8192;
        if self.channel_on_synth(channel as i32) {
            self.synth
                .process_midi_message(channel as i32, 0xE0, raw & 0x7F, (raw >> 7) & 0x7F);
        }
        self.send_to_midi_out(
            channel as i32,
            &[
                0xE0 | channel_byte(channel as i32),
                (raw & 0x7F) as u8,
                ((raw >> 7) & 0x7F) as u8,
            ],
        );
        if let Some(monitor) = &self.monitor {
            monitor.set_bend(channel, value);
        }
    }

    /// Set one channel's modulation (CC1) from the controller wheel.
    pub fn set_channel_modulation(&mut self, channel: u8, value: u8) {
        if channel >= 16 {
            return;
        }
        let value = value.min(127);
        if self.channel_on_synth(channel as i32) {
            self.synth
                .process_midi_message(channel as i32, 0xB0, 1, i32::from(value));
        }
        self.send_to_midi_out(
            channel as i32,
            &[0xB0 | channel_byte(channel as i32), 1, value],
        );
        if let Some(monitor) = &self.monitor {
            monitor.set_modulation(channel, value);
        }
    }

    /// Select a program on one channel from the channel detail popup.
    pub fn set_channel_program(&mut self, channel: u8, program: u8) {
        if channel >= 16 {
            return;
        }
        let channel_index = channel as usize;
        let channel = channel as i32;
        let program = i32::from(program.min(127));
        if channel == 9 {
            self.pin_drum_bank();
        }
        // A manual selection wins over the MIDI fix locks; the song's next
        // program change still takes over as usual.
        self.source_program[channel_index] = program;
        self.sent_program[channel_index] = program;
        if self.channel_on_synth(channel) {
            self.synth.process_midi_message(channel, 0xC0, program, 0);
        }
        self.send_to_midi_out(channel, &[0xC0 | channel_byte(channel), midi_data(program)]);
        if let Some(monitor) = &self.monitor {
            monitor.set_program(channel as u8, program as u8);
        }
    }

    /// Select a bank (CC0) on one channel. The program is sent again so the
    /// synthesizer actually applies the new bank.
    pub fn set_channel_bank(&mut self, channel: u8, bank: u8) {
        if channel >= 16 {
            return;
        }
        let channel_index = channel as usize;
        let channel = channel as i32;
        // Channel 10 stays in the percussion bank (rustysynth adds 128).
        let bank = if channel == 9 { 0 } else { bank.min(127) };
        self.channel_bank[channel_index] = bank;
        if self.channel_on_synth(channel) {
            self.synth
                .process_midi_message(channel, 0xB0, 0, i32::from(bank));
            self.synth.process_midi_message(channel, 0xB0, 32, 0);
        }
        self.send_to_midi_out(channel, &[0xB0 | channel_byte(channel), 0, bank]);
        self.send_to_midi_out(channel, &[0xB0 | channel_byte(channel), 32, 0]);
        let program = self.sent_program[channel_index].max(0);
        if self.channel_on_synth(channel) {
            self.synth.process_midi_message(channel, 0xC0, program, 0);
        }
        self.send_to_midi_out(channel, &[0xC0 | channel_byte(channel), midi_data(program)]);
        if let Some(monitor) = &self.monitor {
            monitor.set_bank(channel as u8, bank);
        }
    }

    /// Set one channel's pan (CC10) from the channel detail popup.
    pub fn set_channel_pan(&mut self, channel: u8, value: u8) {
        if channel >= 16 || self.channel_locked(channel as i32) {
            return;
        }
        let value = value.min(127);
        self.channel_pan[channel as usize] = value;
        let channel = channel as i32;
        if self.channel_on_synth(channel) {
            self.synth
                .process_midi_message(channel, 0xB0, 10, i32::from(value));
        }
        self.send_to_midi_out(channel, &[0xB0 | channel_byte(channel), 10, value]);
        if let Some(monitor) = &self.monitor {
            monitor.set_pan(channel as u8, value);
        }
    }

    /// Set one channel's reverb send (CC91) from the channel detail popup.
    pub fn set_channel_reverb(&mut self, channel: u8, value: u8) {
        if channel >= 16 || self.channel_locked(channel as i32) {
            return;
        }
        let value = value.min(127);
        self.cc_reverb[channel as usize] = i32::from(value);
        let channel = channel as i32;
        let multiplier = if channel == 9 {
            self.rev_drum
        } else {
            self.rev_inst
        };
        let send = self.effect_send(i32::from(value), multiplier);
        if self.channel_on_synth(channel) {
            self.synth.process_midi_message(channel, 0xB0, 91, send);
        }
        self.send_to_midi_out(channel, &[0xB0 | channel_byte(channel), 91, value]);
        if let Some(monitor) = &self.monitor {
            monitor.set_reverb(channel as u8, value);
        }
    }

    /// Set one channel's chorus send (CC93) from the channel detail popup.
    pub fn set_channel_chorus(&mut self, channel: u8, value: u8) {
        if channel >= 16 || self.channel_locked(channel as i32) {
            return;
        }
        let value = value.min(127);
        self.cc_chorus[channel as usize] = i32::from(value);
        let channel = channel as i32;
        let multiplier = if channel == 9 {
            self.cho_drum
        } else {
            self.cho_inst
        };
        let send = self.effect_send(i32::from(value), multiplier);
        if self.channel_on_synth(channel) {
            self.synth.process_midi_message(channel, 0xB0, 93, send);
        }
        self.send_to_midi_out(channel, &[0xB0 | channel_byte(channel), 93, value]);
        if let Some(monitor) = &self.monitor {
            monitor.set_chorus(channel as u8, value);
        }
    }

    /// Temporarily transpose one channel from the controller's right-click menu.
    ///
    /// Percussion is never transposed. Held notes on the channel are cut and
    /// follow the new key from the next note on, like a fresh key change.
    pub fn set_channel_transpose(&mut self, channel: u8, semitones: i32) {
        if channel >= 16 || channel == 9 {
            return;
        }
        let index = channel as usize;
        let next = semitones.clamp(-24, 24);
        let previous = self.channel_transpose[index];
        if next == previous {
            return;
        }
        self.channel_transpose[index] = next;

        let old_total = self.transpose + previous;
        let notes: Vec<ActiveNote> = self
            .active_notes
            .iter()
            .copied()
            .filter(|note| note.channel == channel as i32)
            .collect();
        for note in &notes {
            let key = map_key(
                &self.midi,
                channel as i32,
                transpose_key(channel as i32, note.source_key, old_total),
            );
            if self.channel_on_synth(channel as i32) {
                self.synth.note_off(channel as i32, key);
            }
            self.send_to_midi_out(
                channel as i32,
                &[0x80 | channel_byte(channel as i32), midi_data(key), 0],
            );
        }
        self.active_notes
            .retain(|note| note.channel != channel as i32);
        if let Some(monitor) = &self.monitor {
            monitor.clear_channel(channel);
        }
    }

    /// Re-send a locked channel's stored values after the synth was reset by a
    /// seek. The state events before the seek are ignored for locked channels,
    /// so without this the channel would fall back to program 0 (piano).
    fn reapply_locked_channels(&mut self) {
        for channel in 0..16usize {
            if self.channel_locks & (1 << channel) == 0 {
                continue;
            }
            let channel_i = channel as i32;
            let bank = if channel == 9 {
                0
            } else {
                self.channel_bank[channel]
            };
            let program = self.sent_program[channel].max(0);
            if self.channel_on_synth(channel_i) {
                self.synth
                    .process_midi_message(channel_i, 0xB0, 0, i32::from(bank));
                self.synth.process_midi_message(channel_i, 0xB0, 32, 0);
                self.synth.process_midi_message(channel_i, 0xC0, program, 0);
                self.synth.process_midi_message(
                    channel_i,
                    0xB0,
                    7,
                    i32::from(self.channel_volume[channel]),
                );
                self.synth.process_midi_message(
                    channel_i,
                    0xB0,
                    10,
                    i32::from(self.channel_pan[channel]),
                );
            }
            self.send_to_midi_out(channel_i, &[0xB0 | channel_byte(channel_i), 0, bank]);
            self.send_to_midi_out(channel_i, &[0xB0 | channel_byte(channel_i), 32, 0]);
            self.send_to_midi_out(
                channel_i,
                &[0xC0 | channel_byte(channel_i), midi_data(program)],
            );
            self.send_to_midi_out(
                channel_i,
                &[
                    0xB0 | channel_byte(channel_i),
                    7,
                    self.channel_volume[channel],
                ],
            );
            self.send_to_midi_out(
                channel_i,
                &[
                    0xB0 | channel_byte(channel_i),
                    10,
                    self.channel_pan[channel],
                ],
            );
            if let Some(monitor) = &self.monitor {
                monitor.set_program(channel as u8, program as u8);
                monitor.set_bank(channel as u8, bank);
                monitor.set_volume(channel as u8, self.channel_volume[channel]);
                monitor.set_pan(channel as u8, self.channel_pan[channel]);
            }
        }
    }

    /// True when the channel's mixer values ignore song automation.
    fn channel_locked(&self, channel: i32) -> bool {
        (0..16).contains(&channel) && self.channel_locks & (1 << channel) != 0
    }

    /// Play one note from the on-screen piano on `channel`.
    pub fn audition_note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        if channel >= 16 || key >= 128 || self.is_muted(channel as i32) {
            return;
        }
        let key = key as i32;
        let velocity = velocity.clamp(1, 127) as i32;
        let route = if channel == 9 {
            self.routing.route_for_drum(key as u8)
        } else {
            self.route_for(channel as i32)
        };
        if route.is_none() {
            self.synth.note_on(channel as i32, key, velocity);
        }
        self.routing.send(
            route,
            &[
                0x90 | channel_byte(channel as i32),
                key as u8,
                velocity as u8,
            ],
        );
        if let Some(monitor) = &self.monitor {
            monitor.note_on(channel, key as u8, velocity as u8);
        }
    }

    /// Release a note played from the on-screen piano.
    pub fn audition_note_off(&mut self, channel: u8, key: u8) {
        if channel >= 16 || key >= 128 {
            return;
        }
        let key = key as i32;
        let route = if channel == 9 {
            self.routing.route_for_drum(key as u8)
        } else {
            self.route_for(channel as i32)
        };
        if route.is_none() {
            self.synth.note_off(channel as i32, key);
        }
        self.routing
            .send(route, &[0x80 | channel_byte(channel as i32), key as u8, 0]);
        if let Some(monitor) = &self.monitor {
            monitor.note_off(channel, key as u8);
        }
    }

    /// Silence one MIDI channel without stopping the song. Notes that are
    /// already sounding are cut immediately; future note-ons on that channel
    /// are skipped until it is unmuted again.
    pub fn set_channel_muted(&mut self, channel: u8, muted: bool) {
        let bit = 1u16 << (channel & 15);
        if muted {
            self.muted_channels |= bit;
            self.active_notes
                .retain(|note| note.channel != i32::from(channel));
            self.synth.note_off_all_channel(channel as i32, true);
            let route = self.route_for(channel as i32);
            self.routing.channel_off(route, channel);
        } else {
            self.muted_channels &= !bit;
        }
        if let Some(monitor) = &self.monitor {
            if muted {
                monitor.clear_channel(channel);
            }
            monitor.set_muted_mask(self.muted_channels);
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
            if !self.channel_on_synth(channel as i32) {
                continue;
            }
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
            Command::SetMidiOptions(options) => self.set_midi_options(options),
            Command::SetMidiRouting(routing) => self.set_midi_routing(routing),
            Command::MidiReset(reset) => self.reset_midi(reset),
            Command::SetSpeed(speed) => self.set_speed(speed),
            Command::SetChannelMuted { channel, muted } => self.set_channel_muted(channel, muted),
            Command::SetChannelMutes { mask } => self.set_channel_mutes(mask),
            Command::SetChannelVolume { channel, value } => self.set_channel_volume(channel, value),
            Command::SetChannelBend { channel, value } => self.set_channel_bend(channel, value),
            Command::NoteOn {
                channel,
                key,
                velocity,
            } => self.audition_note_on(channel, key, velocity),
            Command::NoteOff { channel, key } => self.audition_note_off(channel, key),
            Command::SetChannelModulation { channel, value } => {
                self.set_channel_modulation(channel, value)
            }
            Command::SetChannelProgram { channel, program } => {
                self.set_channel_program(channel, program)
            }
            Command::SetChannelBank { channel, bank } => self.set_channel_bank(channel, bank),
            Command::SetChannelPan { channel, value } => self.set_channel_pan(channel, value),
            Command::SetChannelReverb { channel, value } => self.set_channel_reverb(channel, value),
            Command::SetChannelChorus { channel, value } => self.set_channel_chorus(channel, value),
            Command::SetChannelTranspose { channel, semitones } => {
                self.set_channel_transpose(channel, semitones)
            }
            Command::SetChannelLocks { mask } => self.channel_locks = mask,
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
        self.active_notes.clear();
        if let Some(monitor) = &self.monitor {
            monitor.clear_notes();
        }
        self.synth.reset();
        self.routing.panic_all();
        let saved_reverb = self.cc_reverb;
        let saved_chorus = self.cc_chorus;
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
        for channel in 0..16usize {
            if self.channel_locks & (1 << channel) != 0 {
                self.cc_reverb[channel] = saved_reverb[channel];
                self.cc_chorus[channel] = saved_chorus[channel];
            }
        }
        self.resend_effects();
        // Files that never select a drum program lose the locked kit when the
        // synth is reset, so re-apply it after every seek.
        self.apply_drum_kit();
        self.reapply_locked_channels();
        self.position = target;
        self.finished = false;
    }

    /// Render the next `left.len()` frames. Extra frames beyond the song end
    /// are silence. While paused the song stays frozen but the synthesizer
    /// keeps rendering, so the MIDI Controller's virtual piano can be played.
    pub fn fill(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());

        if self.finished {
            left[..frames].fill(0.0);
            right[..frames].fill(0.0);
            return;
        }

        if !self.playing {
            self.synth.render(&mut left[..frames], &mut right[..frames]);
            self.apply_output_effects(&mut left[..frames], &mut right[..frames]);
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

            self.apply_output_effects(&mut left[done..done + len], &mut right[done..done + len]);

            done += len;
            self.position += len as f64 * us_per_frame * self.speed;
        }

        if self.position >= self.end_us {
            self.active_notes.clear();
            if !self.finished {
                self.routing.panic_all();
            }
            self.finished = true;
        }
    }

    /// Equalizer and limiter shared by song playback and paused auditioning.
    fn apply_output_effects(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.eq_enabled {
            for i in 0..left.len() {
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

        // Safety net: a hot mix must not hard-clip ("mushy" sound).
        for i in 0..left.len() {
            left[i] = soft_limit(left[i]);
            right[i] = soft_limit(right[i]);
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
                    let source_key = key;
                    let total = self.transpose + self.channel_transpose[channel as usize];
                    let key = map_key(&self.midi, channel, transpose_key(channel, key, total));
                    let velocity = if channel == 9 {
                        self.drum_velocity(key, velocity)
                    } else {
                        self.group_velocity(channel, velocity)
                    };
                    if velocity > 0 {
                        self.active_notes.push(ActiveNote {
                            channel,
                            source_key,
                            velocity,
                        });
                    } else {
                        self.forget_note(channel, source_key);
                    }
                    if let Some(monitor) = &self.monitor {
                        let note = key.clamp(0, 127) as u8;
                        if velocity > 0 {
                            monitor.note_on(
                                channel.clamp(0, 15) as u8,
                                note,
                                velocity.clamp(0, 127) as u8,
                            );
                        } else {
                            monitor.note_off(channel.clamp(0, 15) as u8, note);
                        }
                    }
                    let route = if channel == 9 {
                        self.routing.route_for_drum(key.clamp(0, 127) as u8)
                    } else {
                        self.route_for(channel)
                    };
                    if route.is_none() {
                        self.synth.note_on(channel, key, velocity);
                    }
                    self.routing.send(
                        route,
                        &[
                            0x90 | channel_byte(channel),
                            midi_data(key),
                            midi_data(velocity),
                        ],
                    );
                }
            }
            PlayKind::NoteOff { channel, key } => {
                self.forget_note(channel, key);
                let total = self.transpose + self.channel_transpose[channel as usize];
                let key = map_key(&self.midi, channel, transpose_key(channel, key, total));
                let route = if channel == 9 {
                    self.routing.route_for_drum(key.clamp(0, 127) as u8)
                } else {
                    self.route_for(channel)
                };
                if route.is_none() {
                    self.synth.note_off(channel, key);
                }
                if let Some(monitor) = &self.monitor {
                    monitor.note_off(channel.clamp(0, 15) as u8, key.clamp(0, 127) as u8);
                }
                self.routing
                    .send(route, &[0x80 | channel_byte(channel), midi_data(key), 0]);
            }
            PlayKind::Control {
                channel,
                controller,
                value,
            } => {
                if self.channel_locked(channel) && matches!(controller, 0 | 7 | 10 | 32 | 91 | 93) {
                    // The channel's mixer values are locked by the user.
                } else {
                    match controller {
                        // Reverb (91) and chorus (93) sends are scaled per group.
                        91 => {
                            if (0..16).contains(&channel) {
                                self.cc_reverb[channel as usize] = value;
                                if let Some(monitor) = &self.monitor {
                                    monitor.set_reverb(channel as u8, value.clamp(0, 127) as u8);
                                }
                            }
                            let multiplier = if channel == 9 {
                                self.rev_drum
                            } else {
                                self.rev_inst
                            };
                            let send = self.effect_send(value, multiplier);
                            if self.channel_on_synth(channel) {
                                self.synth.process_midi_message(channel, 0xB0, 91, send);
                            }
                            self.send_to_midi_out(
                                channel,
                                &[0xB0 | channel_byte(channel), 91, midi_data(value)],
                            );
                        }
                        93 => {
                            if (0..16).contains(&channel) {
                                self.cc_chorus[channel as usize] = value;
                                if let Some(monitor) = &self.monitor {
                                    monitor.set_chorus(channel as u8, value.clamp(0, 127) as u8);
                                }
                            }
                            let multiplier = if channel == 9 {
                                self.cho_drum
                            } else {
                                self.cho_inst
                            };
                            let send = self.effect_send(value, multiplier);
                            if self.channel_on_synth(channel) {
                                self.synth.process_midi_message(channel, 0xB0, 93, send);
                            }
                            self.send_to_midi_out(
                                channel,
                                &[0xB0 | channel_byte(channel), 93, midi_data(value)],
                            );
                        }
                        _ => {
                            if (0..16).contains(&channel) {
                                if controller == 7 {
                                    self.channel_volume[channel as usize] =
                                        value.clamp(0, 127) as u8;
                                    if let Some(monitor) = &self.monitor {
                                        monitor
                                            .set_volume(channel as u8, value.clamp(0, 127) as u8);
                                    }
                                } else if controller == 1
                                    && let Some(monitor) = &self.monitor
                                {
                                    monitor
                                        .set_modulation(channel as u8, value.clamp(0, 127) as u8);
                                } else if controller == 0 {
                                    // The drum channel always stays in the percussion bank.
                                    let bank = if channel == 9 {
                                        0
                                    } else {
                                        value.clamp(0, 127) as u8
                                    };
                                    self.channel_bank[channel as usize] = bank;
                                    if let Some(monitor) = &self.monitor {
                                        monitor.set_bank(channel as u8, bank);
                                    }
                                } else if controller == 10 {
                                    self.channel_pan[channel as usize] = value.clamp(0, 127) as u8;
                                    if let Some(monitor) = &self.monitor {
                                        monitor.set_pan(channel as u8, value.clamp(0, 127) as u8);
                                    }
                                }
                            }
                            let value = if channel == 9
                                && self.midi.lock_drum_kit
                                && matches!(controller, 0 | 32)
                            {
                                0
                            } else {
                                value
                            };
                            if self.channel_on_synth(channel) {
                                self.synth
                                    .process_midi_message(channel, 0xB0, controller, value);
                            }
                            self.send_to_midi_out(
                                channel,
                                &[
                                    0xB0 | channel_byte(channel),
                                    midi_data(controller),
                                    midi_data(value),
                                ],
                            );
                        }
                    }
                }
            }
            PlayKind::Program { channel, program } => {
                if !self.channel_locked(channel) {
                    if (0..16).contains(&channel) {
                        self.source_program[channel as usize] = program;
                    }
                    let mapped = map_program(&self.midi, channel, program);
                    if (0..16).contains(&channel) {
                        self.sent_program[channel as usize] = mapped;
                        if let Some(monitor) = &self.monitor {
                            monitor.set_program(channel as u8, mapped.clamp(0, 127) as u8);
                        }
                    }
                    if channel == 9 && self.midi.lock_drum_kit {
                        self.pin_drum_bank();
                    }
                    if self.channel_on_synth(channel) {
                        self.synth.process_midi_message(channel, 0xC0, mapped, 0);
                    }
                    self.send_to_midi_out(
                        channel,
                        &[0xC0 | channel_byte(channel), midi_data(mapped)],
                    );
                }
            }
            PlayKind::Bend { channel, value } => {
                if let Some(monitor) = &self.monitor {
                    monitor.set_bend(channel.clamp(0, 15) as u8, wire_bend_to_signed(value));
                }
                if self.channel_on_synth(channel) {
                    self.synth.process_midi_message(
                        channel,
                        0xE0,
                        value & 0x7F,
                        (value >> 7) & 0x7F,
                    );
                }
                self.send_to_midi_out(
                    channel,
                    &[
                        0xE0 | channel_byte(channel),
                        (value & 0x7F) as u8,
                        ((value >> 7) & 0x7F) as u8,
                    ],
                );
            }
            PlayKind::ChannelPressure { channel, value } => {
                if self.channel_on_synth(channel) {
                    self.synth.process_midi_message(channel, 0xD0, value, 0);
                }
                self.send_to_midi_out(channel, &[0xD0 | channel_byte(channel), midi_data(value)]);
            }
            PlayKind::KeyPressure {
                channel,
                key,
                value,
            } => {
                if self.channel_on_synth(channel) {
                    self.synth.process_midi_message(channel, 0xA0, key, value);
                }
                self.send_to_midi_out(
                    channel,
                    &[
                        0xA0 | channel_byte(channel),
                        midi_data(key),
                        midi_data(value),
                    ],
                );
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // The audio stream (and this player) is dropped when the user stops or
        // opens another song; without this the external device keeps the last
        // notes sounding.
        self.routing.panic_all();
    }
}

/// Apply one instrument group's volume and velocity window to a note.
///
/// The volume (0-127, with 100 unchanged) scales the velocity first, then the
/// group's minimum and maximum clamp the result, and finally the value stays
/// in the MIDI range. A zero volume therefore leaves the quietest audible note
/// rather than a note-on that would be silently dropped.
fn group_scaled_velocity(options: &MidiOptions, channel: i32, program: u8, velocity: i32) -> i32 {
    if velocity <= 0 || channel == 9 {
        return velocity;
    }
    let group = crate::audio::program_group(program);
    let volume = i32::from(options.group_volume[group]);
    let scaled = velocity * volume / 100;
    let min = i32::from(options.group_velocity_min[group]);
    let max = i32::from(options.group_velocity_max[group]);
    scaled.clamp(min, max).clamp(1, 127)
}

/// Apply one drum group's volume and velocity window to a note.
fn drum_scaled_velocity(options: &MidiOptions, key: i32, velocity: i32) -> i32 {
    if velocity <= 0 {
        return velocity;
    }
    let Some(group) = crate::audio::drum_group(key.clamp(0, 127) as u8) else {
        return velocity;
    };
    let volume = i32::from(options.drum_volume[group]);
    let scaled = velocity * volume / 100;
    let min = i32::from(options.drum_velocity_min[group]);
    let max = i32::from(options.drum_velocity_max[group]);
    scaled.clamp(min, max).clamp(1, 127)
}

/// Drum note used for one metronome click.
///
/// With `metronome_hihat` the closed hi-hat replaces both configured notes, so
/// every beat of the bar gets the same hat sound.
fn beat_note(options: &MidiOptions, first: bool) -> i32 {
    if options.metronome_hihat {
        42
    } else if first {
        i32::from(options.metronome_first)
    } else {
        i32::from(options.metronome_other)
    }
}

/// Low nibble of a MIDI status byte for `channel`.
fn channel_byte(channel: i32) -> u8 {
    (channel as u8) & 0x0F
}

/// Clamp a value into the 7-bit data range MIDI messages allow.
fn midi_data(value: i32) -> u8 {
    value.clamp(0, 127) as u8
}

/// The program actually sent for `channel`, after the MIDI fixes.
fn map_program(options: &MidiOptions, channel: i32, program: i32) -> i32 {
    if channel == 9 {
        return if options.lock_drum_kit {
            i32::from(options.drum_program)
        } else {
            program
        };
    }
    if options.lock_bass && (32..=39).contains(&program) {
        return i32::from(options.bass_program);
    }
    program
}

/// Kick/snare replacement for drum notes.
fn map_key(options: &MidiOptions, channel: i32, key: i32) -> i32 {
    if channel != 9 {
        return key;
    }
    if options.lock_kick && matches!(key, 35 | 36) {
        return i32::from(options.kick_note);
    }
    if options.lock_snare && matches!(key, 31 | 38..=40) {
        return i32::from(options.snare_note);
    }
    key
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

/// Convert a 14-bit pitch-bend value back to the signed range the monitor uses.
fn wire_bend_to_signed(value: i32) -> i16 {
    (value - 8192).clamp(-8192, 8191) as i16
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
    fn midi_locks_replace_programs_and_drum_notes() {
        let mut options = MidiOptions::default();
        assert_eq!(map_program(&options, 1, 32), 32);
        options.lock_bass = true;
        options.bass_program = 33;
        assert_eq!(map_program(&options, 1, 32), 33);
        assert_eq!(map_program(&options, 1, 39), 33);
        assert_eq!(map_program(&options, 1, 0), 0);

        options.lock_drum_kit = true;
        // `MidiOptions` carries the zero-based program number, so the MIDI
        // page's "25 Electronic" arrives here as 24.
        options.drum_program = 3;
        assert_eq!(map_program(&options, 9, 0), 3);
        options.drum_program = 24;
        assert_eq!(map_program(&options, 9, 0), 24);

        options.lock_kick = true;
        options.kick_note = 36;
        assert_eq!(map_key(&options, 9, 35), 36);
        assert_eq!(map_key(&options, 9, 36), 36);
        assert_eq!(map_key(&options, 9, 38), 38);

        options.lock_snare = true;
        options.snare_note = 40;
        assert_eq!(map_key(&options, 9, 38), 40);
        assert_eq!(map_key(&options, 9, 39), 40);
        assert_eq!(map_key(&options, 9, 31), 40); // Drum Sticks follow the snare
        assert_eq!(map_key(&options, 9, 37), 37); // Side Stick is left alone
        assert_eq!(map_key(&options, 9, 34), 34);
        // Melodic channels are untouched.
        assert_eq!(map_key(&options, 1, 35), 35);
    }

    #[test]
    fn instrument_groups_scale_and_limit_note_velocity() {
        let mut options = MidiOptions::default();
        // Bass is program 32, which belongs to group 8.
        options.group_volume[8] = 50;
        assert_eq!(group_scaled_velocity(&options, 1, 32, 100), 50);
        // The velocity window is applied after the volume.
        options.group_velocity_min[8] = 60;
        assert_eq!(group_scaled_velocity(&options, 1, 32, 100), 60);
        options.group_velocity_min[8] = 0;
        options.group_velocity_max[8] = 40;
        assert_eq!(group_scaled_velocity(&options, 1, 32, 100), 40);
        // Other groups keep their own settings.
        assert_eq!(group_scaled_velocity(&options, 1, 0, 100), 100);
        // Note-offs and drums pass through untouched.
        assert_eq!(group_scaled_velocity(&options, 1, 32, 0), 0);
        assert_eq!(group_scaled_velocity(&options, 9, 32, 100), 100);
        // The result never leaves the seven-bit MIDI range.
        options.group_velocity_max[8] = 127;
        options.group_volume[8] = 100;
        assert_eq!(group_scaled_velocity(&options, 1, 32, 127), 127);
        options.group_volume[8] = 127;
        assert_eq!(group_scaled_velocity(&options, 1, 32, 100), 127);
        options.group_volume[8] = 0;
        assert_eq!(group_scaled_velocity(&options, 1, 32, 100), 1);
    }

    #[test]
    fn drum_groups_scale_and_limit_note_velocity() {
        let mut options = MidiOptions::default();
        // Kick notes are drum group 0.
        options.drum_volume[0] = 50;
        assert_eq!(drum_scaled_velocity(&options, 35, 100), 50);
        options.drum_velocity_min[0] = 60;
        assert_eq!(drum_scaled_velocity(&options, 36, 100), 60);
        options.drum_velocity_min[0] = 0;
        options.drum_velocity_max[0] = 40;
        assert_eq!(drum_scaled_velocity(&options, 35, 100), 40);
        // Unlisted notes keep the velocity from the file.
        assert_eq!(drum_scaled_velocity(&options, 88, 100), 100);
        // Note-offs are never scaled.
        assert_eq!(drum_scaled_velocity(&options, 35, 0), 0);
        // The result stays in the seven-bit MIDI range.
        options.drum_velocity_max[0] = 127;
        options.drum_volume[0] = 127;
        assert_eq!(drum_scaled_velocity(&options, 35, 100), 127);
    }

    #[test]
    fn hihat_replaces_both_metronome_notes() {
        let mut options = MidiOptions {
            metronome: true,
            metronome_first: 75,
            metronome_other: 76,
            ..Default::default()
        };
        assert_eq!(beat_note(&options, true), 75);
        assert_eq!(beat_note(&options, false), 76);
        options.metronome_hihat = true;
        assert_eq!(beat_note(&options, true), 42);
        assert_eq!(beat_note(&options, false), 42);
    }

    #[test]
    fn midi_messages_stay_inside_seven_bits() {
        assert_eq!(channel_byte(0), 0);
        assert_eq!(channel_byte(9), 9);
        assert_eq!(channel_byte(15), 15);
        assert_eq!(midi_data(-1), 0);
        assert_eq!(midi_data(0), 0);
        assert_eq!(midi_data(127), 127);
        assert_eq!(midi_data(200), 127);
        assert_eq!(0x90 | channel_byte(9), 0x99);
        assert_eq!(0xB0 | channel_byte(0), 0xB0);
    }

    #[test]
    fn song_pitch_bend_keeps_its_sign_for_the_monitor() {
        assert_eq!(wire_bend_to_signed(8192), 0);
        assert_eq!(wire_bend_to_signed(0), -8192);
        assert_eq!(wire_bend_to_signed(16383), 8191);
        assert_eq!(wire_bend_to_signed(4096), -4096);
        assert_eq!(wire_bend_to_signed(12288), 4096);
    }

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
