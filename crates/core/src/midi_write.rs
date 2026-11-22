//! Rewrite a Standard MIDI File with the mix, tempo and key edits made in the
//! MIDI Editor.
//!
//! The original file is parsed with `midly` and every track event is kept, so
//! lyrics, markers and anything else the player does not understand survive a
//! save. Only the requested values change:
//!
//! * Channel mix (bank, program, volume, pan, reverb, chorus) is written once
//!   at tick 0 for the channel's primary track. Existing tick-0 values for the
//!   same channel are replaced, later automation is left alone.
//! * Every `Set Tempo` event is scaled by the same ratio, so the whole tempo
//!   map keeps its shape.
//! * Notes on channels other than 10 are shifted by the key offset; drums are
//!   never transposed.
//! * Track names are replaced by the names typed in the editor.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use midly::num::{u4, u7, u24, u28};
use midly::{MetaMessage, MidiMessage, Smf, TrackEvent, TrackEventKind};

use crate::midi;

/// One channel's mix, written at tick 0 of `track`.
#[derive(Debug, Clone, Copy)]
pub struct ChannelMix {
    pub channel: u8,
    pub track: usize,
    /// Bank select MSB (controller 0).
    pub bank: u8,
    pub program: u8,
    pub volume: u8,
    pub pan: u8,
    pub reverb: u8,
    pub chorus: u8,
}

/// Everything the MIDI Editor wants to change in the file.
#[derive(Debug, Clone, Default)]
pub struct SmfEdits {
    pub channels: Vec<ChannelMix>,
    /// `(track index, new name)`.
    pub track_names: Vec<(usize, String)>,
    /// Multiplier for every tempo event: `original_bpm / target_bpm`.
    pub tempo_scale: f64,
    /// Semitones added to every non-drum note.
    pub transpose: i32,
}

/// Channel controllers that make up the saved mix.
const MIX_CONTROLLERS: [u8; 6] = [0, 32, 7, 10, 91, 93];

impl ChannelMix {
    fn clamp(mut self) -> Self {
        self.channel = self.channel.min(15);
        self.bank = self.bank.min(127);
        self.program = self.program.min(127);
        self.volume = self.volume.min(127);
        self.pan = self.pan.min(127);
        self.reverb = self.reverb.min(127);
        self.chorus = self.chorus.min(127);
        self
    }
}

/// Rewrite `path` in place through a temporary file so a failure cannot leave
/// a half-written song behind.
pub fn edit_file(path: &Path, edits: &SmfEdits) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let rewritten = edit_bytes(&bytes, edits)?;
    write_atomic(path, &rewritten)
}

/// Read `source`, apply the edits and write the result to `target`.
///
/// `target` may be a new file (Save As) or the source itself.
pub fn edit_file_to(source: &Path, target: &Path, edits: &SmfEdits) -> Result<()> {
    let bytes = std::fs::read(source).with_context(|| format!("reading {}", source.display()))?;
    let rewritten = edit_bytes(&bytes, edits)?;
    write_atomic(target, &rewritten)
}

/// Replace `path` with `bytes` through a temporary file in the same folder.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("song.mid");
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&temp, bytes).with_context(|| format!("writing {}", temp.display()))?;
    if let Err(err) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

/// Apply the edits to an in-memory SMF and return the encoded bytes.
pub fn edit_bytes(bytes: &[u8], edits: &SmfEdits) -> Result<Vec<u8>> {
    // Declared before the parsed SMF so the new track names outlive the events
    // that borrow them.
    let name_store: Vec<Vec<u8>> = edits
        .track_names
        .iter()
        .map(|(_, name)| name.as_bytes().to_vec())
        .collect();

    let repaired = midi::repair_altered_header(bytes);
    let bytes = repaired.as_deref().unwrap_or(bytes);
    let mut smf = Smf::parse(bytes).context("parsing the MIDI file for editing")?;
    let track_count = smf.tracks.len();

    for (track, _) in &edits.track_names {
        if *track >= track_count {
            bail!("track {track} is outside the file (it has {track_count} tracks)");
        }
    }
    for mix in &edits.channels {
        if mix.track >= track_count {
            bail!(
                "track {} for channel {} is outside the file",
                mix.track,
                mix.channel
            );
        }
    }

    let mixes: Vec<ChannelMix> = edits
        .channels
        .iter()
        .copied()
        .map(ChannelMix::clamp)
        .collect();
    let mix_by_channel: HashMap<u8, ChannelMix> = {
        let mut map = HashMap::new();
        for mix in &mixes {
            // If several rows share a channel the last edit wins.
            map.insert(mix.channel, *mix);
        }
        map
    };
    let names: HashMap<usize, &[u8]> = edits
        .track_names
        .iter()
        .enumerate()
        .map(|(index, (track, _))| (*track, name_store[index].as_slice()))
        .collect();

    let transpose = edits.transpose.clamp(-24, 24);
    let tempo_scale = edits.tempo_scale;
    let scale_tempo = tempo_scale.is_finite() && (tempo_scale - 1.0).abs() > 1e-9;

    for (track_index, track) in smf.tracks.iter_mut().enumerate() {
        let old = std::mem::take(track);
        let mut kept: Vec<TrackEvent<'_>> = Vec::with_capacity(old.len() + 12);
        let mut pending_delta = 0u32;
        let mut absolute_tick = 0u64;
        let mut name_written = false;

        for mut event in old {
            let original_delta = event.delta.as_int();
            absolute_tick += u64::from(original_delta);
            let at_start = absolute_tick == 0;

            if at_start {
                // Replace rather than append: our values must win over the
                // file's own tick-0 mix for the edited channels.
                if let TrackEventKind::Midi { channel, message } = &event.kind {
                    let channel = channel.as_int();
                    let replaces_mix = match message {
                        MidiMessage::ProgramChange { .. } => true,
                        MidiMessage::Controller { controller, .. } => {
                            MIX_CONTROLLERS.contains(&controller.as_int())
                        }
                        _ => false,
                    };
                    if mix_by_channel.contains_key(&channel) && replaces_mix {
                        // Drop it; its time span merges into the next event.
                        pending_delta += original_delta;
                        continue;
                    }
                }
            }

            event.delta = u28::new((pending_delta + original_delta).min(0x0FFF_FFFF));
            pending_delta = 0;

            match &mut event.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    if scale_tempo {
                        let scaled = (f64::from(us.as_int()) * tempo_scale).round();
                        *us = u24_from(scaled);
                    }
                }
                TrackEventKind::Meta(MetaMessage::TrackName(raw)) => {
                    if let Some(name) = names.get(&track_index) {
                        *raw = name;
                        name_written = true;
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    // The editor's mix and patch are a deliberate override for
                    // the whole song, not just tick 0: later automation on the
                    // edited channel must not snap back to the old values.
                    if let Some(mix) = mix_by_channel.get(&channel.as_int()) {
                        match message {
                            MidiMessage::Controller { controller, value } => {
                                let replacement = match controller.as_int() {
                                    0 => Some(mix.bank),
                                    32 => Some(0),
                                    7 => Some(mix.volume),
                                    10 => Some(mix.pan),
                                    91 => Some(mix.reverb),
                                    93 => Some(mix.chorus),
                                    _ => None,
                                };
                                if let Some(replacement) = replacement {
                                    *value = u7::new(replacement);
                                }
                            }
                            MidiMessage::ProgramChange { program } => {
                                *program = u7::new(mix.program);
                            }
                            _ => {}
                        }
                    }
                    if channel.as_int() != 9 && transpose != 0 {
                        match message {
                            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                                let shifted = (i32::from(key.as_int()) + transpose).clamp(0, 127);
                                *key = u7::new(shifted as u8);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            kept.push(event);
        }

        // Channel mix belongs at the very start of the channel's primary track.
        let mut prefix: Vec<TrackEvent<'_>> = Vec::new();
        if let Some(name) = names.get(&track_index)
            && !name_written
        {
            prefix.push(TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Meta(MetaMessage::TrackName(name)),
            });
        }
        for mix in mixes.iter().filter(|mix| mix.track == track_index) {
            let channel = u4::new(mix.channel);
            for (controller, value) in [
                (0u8, mix.bank),
                (32, 0),
                (7, mix.volume),
                (10, mix.pan),
                (91, mix.reverb),
                (93, mix.chorus),
            ] {
                prefix.push(TrackEvent {
                    delta: u28::new(0),
                    kind: TrackEventKind::Midi {
                        channel,
                        message: MidiMessage::Controller {
                            controller: u7::new(controller),
                            value: u7::new(value),
                        },
                    },
                });
            }
            prefix.push(TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::ProgramChange {
                        program: u7::new(mix.program),
                    },
                },
            });
        }
        if !prefix.is_empty() {
            prefix.extend(kept);
            kept = prefix;
        }

        *track = kept;
    }

    let mut out = Vec::new();
    smf.write_std(&mut out)
        .context("encoding the edited MIDI file")?;
    Ok(out)
}

fn u24_from(value: f64) -> u24 {
    let value = value.round().clamp(1.0, 0x00FF_FFFF as f64);
    u24::new(value as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varlen(mut value: u32) -> Vec<u8> {
        let mut buf = vec![(value & 0x7f) as u8];
        value >>= 7;
        while value > 0 {
            buf.insert(0, ((value & 0x7f) | 0x80) as u8);
            value >>= 7;
        }
        buf
    }

    fn smf(tracks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        out.extend_from_slice(&96u16.to_be_bytes());
        for track in tracks {
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&(track.len() as u32).to_be_bytes());
            out.extend_from_slice(track);
        }
        out
    }

    fn meta(meta_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 0xFF, meta_type];
        out.extend_from_slice(&varlen(payload.len() as u32));
        out.extend_from_slice(payload);
        out
    }

    fn source() -> Vec<u8> {
        // Track 0: tempo 500000 (120 BPM) and a name.
        let mut meta_track = meta(0x51, &500_000u32.to_be_bytes()[1..]);
        meta_track.extend(meta(0x03, b"Old Meta"));
        meta_track.extend([0, 0xFF, 0x2F, 0]);

        // Track 1: program 0, volume 100, note on/off on channel 1 and drums.
        let mut music = vec![0, 0xC0, 0];
        music.extend([0, 0xB0, 7, 100]);
        music.extend(meta(0x03, b"Keys"));
        music.extend([0, 0x90, 60, 100]);
        music.extend([96, 0x80, 60, 0]);
        music.extend([0, 0x99, 60, 100]);
        music.extend([96, 0x89, 60, 0]);
        // Later automation on the edited channel must not undo the edit.
        music.extend([0, 0xB0, 7, 127]);
        music.extend([0, 0xC0, 5]);
        music.extend([0, 0xFF, 0x2F, 0]);
        smf(&[meta_track, music])
    }

    #[test]
    fn writes_mix_across_the_song_and_scales_tempo() {
        let edits = SmfEdits {
            channels: vec![ChannelMix {
                channel: 0,
                track: 1,
                bank: 3,
                program: 40,
                volume: 80,
                pan: 32,
                reverb: 20,
                chorus: 10,
            }],
            track_names: vec![(0, "New Meta".to_owned()), (1, "Piano".to_owned())],
            tempo_scale: 0.8,
            transpose: 0,
        };
        let out = edit_bytes(&source(), &edits).unwrap();
        let smf = Smf::parse(&out).unwrap();

        let mut tempo = None;
        let mut names = Vec::new();
        let mut cc = HashMap::new();
        let mut program = None;
        for (index, track) in smf.tracks.iter().enumerate() {
            let mut tick = 0u32;
            for event in track {
                tick += event.delta.as_int();
                match &event.kind {
                    TrackEventKind::Meta(MetaMessage::Tempo(us)) => tempo = Some(us.as_int()),
                    TrackEventKind::Meta(MetaMessage::TrackName(raw)) => {
                        names.push((index, String::from_utf8_lossy(raw).into_owned()));
                    }
                    TrackEventKind::Midi {
                        channel,
                        message: MidiMessage::Controller { controller, value },
                    } if tick == 0 && channel.as_int() == 0 => {
                        cc.insert(controller.as_int(), value.as_int());
                    }
                    TrackEventKind::Midi {
                        channel,
                        message: MidiMessage::ProgramChange { program: value },
                    } if tick == 0 && channel.as_int() == 0 => {
                        program = Some(value.as_int());
                    }
                    _ => {}
                }
            }
        }

        assert_eq!(tempo, Some(400_000));
        assert_eq!(
            names,
            vec![(0, "New Meta".to_owned()), (1, "Piano".to_owned())]
        );
        assert_eq!(program, Some(40));
        assert_eq!(cc.get(&0), Some(&3));
        assert_eq!(cc.get(&7), Some(&80));
        assert_eq!(cc.get(&10), Some(&32));
        assert_eq!(cc.get(&91), Some(&20));
        assert_eq!(cc.get(&93), Some(&10));

        let mut volumes = Vec::new();
        let mut programs = Vec::new();
        for track in &smf.tracks {
            for event in track {
                if let TrackEventKind::Midi { channel, message } = &event.kind
                    && channel.as_int() == 0
                {
                    match message {
                        MidiMessage::Controller { controller, value }
                            if controller.as_int() == 7 =>
                        {
                            volumes.push(value.as_int());
                        }
                        MidiMessage::ProgramChange { program: value } => {
                            programs.push(value.as_int());
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(volumes.len() >= 2);
        assert!(volumes.iter().all(|value| *value == 80));
        assert!(programs.len() >= 2);
        assert!(programs.iter().all(|value| *value == 40));
    }

    #[test]
    fn transposes_notes_but_not_drums() {
        let edits = SmfEdits {
            transpose: 2,
            ..Default::default()
        };
        let out = edit_bytes(&source(), &edits).unwrap();
        let smf = Smf::parse(&out).unwrap();
        let mut music_notes = Vec::new();
        let mut drum_notes = Vec::new();
        for track in &smf.tracks {
            for event in track {
                if let TrackEventKind::Midi { channel, message } = &event.kind {
                    let key = match message {
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            Some(key.as_int())
                        }
                        _ => None,
                    };
                    if let Some(key) = key {
                        if channel.as_int() == 9 {
                            drum_notes.push(key);
                        } else {
                            music_notes.push(key);
                        }
                    }
                }
            }
        }
        assert_eq!(music_notes, vec![62, 62]);
        assert_eq!(drum_notes, vec![60, 60]);
    }

    #[test]
    fn preserves_unrelated_events() {
        let edits = SmfEdits {
            transpose: 1,
            ..Default::default()
        };
        let out = edit_bytes(&source(), &edits).unwrap();
        let smf = Smf::parse(&out).unwrap();
        let has_end = smf.tracks.iter().all(|track| {
            track
                .iter()
                .any(|event| matches!(event.kind, TrackEventKind::Meta(MetaMessage::EndOfTrack)))
        });
        assert!(has_end);
        assert_eq!(smf.tracks.len(), 2);
    }
}
