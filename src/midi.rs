use anyhow::{Result, bail};
use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

use crate::model::{EventKind, MetaTextKind, TimeSignatureChange, TimedEvent};
use crate::tempo::TempoMap;
use crate::text::{EncodingChoice, decode};

/// A decoded text/meta event together with its raw bytes.
///
/// The raw bytes are kept so the caller can re-decode with an explicit
/// encoding after auto-detection has been attempted.
#[derive(Debug, Clone)]
pub struct TextEvent {
    pub tick: u64,
    pub at_us: u64,
    pub track: u16,
    pub order: u32,
    pub kind: MetaTextKind,
    pub raw: Vec<u8>,
}

/// The raw result of parsing a Standard MIDI File.
#[derive(Debug, Clone)]
pub struct MidiDoc {
    pub ticks_per_quarter: u16,
    pub track_count: usize,
    pub tempo_map: TempoMap,
    /// Sorted by tick; at most one change per tick.
    pub time_signatures: Vec<TimeSignatureChange>,
    pub max_tick: u64,
    pub duration_us: u64,
    pub events: Vec<TimedEvent>,
    pub text_events: Vec<TextEvent>,
    pub has_lyrics: bool,
}

impl MidiDoc {
    pub fn bpm(&self) -> f64 {
        self.tempo_map.bpm_at(0)
    }
}

struct Pending {
    tick: u64,
    track: u16,
    order: u32,
    kind: EventKind,
}

/// Repair the magic bytes of an SMF whose `MThd` header was deliberately
/// replaced by another four-byte marker (an old karaoke copy-protection
/// trick). Only files that still have a valid-looking header and a first
/// `MTrk` chunk are patched; anything else is left untouched for the real
/// parser to reject.
fn repair_altered_header(bytes: &[u8]) -> Option<Vec<u8>> {
    const MAGIC: &[u8] = b"MThd";
    if bytes.len() < 14 || &bytes[..4] == MAGIC {
        return None;
    }

    let header_len = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if !(6..=64).contains(&header_len) {
        return None;
    }

    let track_at = 8 + header_len as usize;
    if bytes.len() < track_at + 4 || &bytes[track_at..track_at + 4] != b"MTrk" {
        return None;
    }

    // Format 0/1/2 with at least one track: the rest of the header is intact.
    let format = u16::from_be_bytes([bytes[8], bytes[9]]);
    let tracks = u16::from_be_bytes([bytes[10], bytes[11]]);
    if format > 2 || tracks == 0 {
        return None;
    }

    let mut repaired = bytes.to_vec();
    repaired[..4].copy_from_slice(MAGIC);
    Some(repaired)
}

pub fn parse(bytes: &[u8], enc: EncodingChoice) -> Result<MidiDoc> {
    // Some old karaoke tools replaced the `MThd` magic with arbitrary text
    // (for example "Lock") to keep the file from opening in sequencers.
    // Repair the header in memory when the rest is still a normal SMF.
    let repaired = repair_altered_header(bytes);
    let bytes = repaired.as_deref().unwrap_or(bytes);
    let smf = Smf::parse(bytes)?;

    let ticks_per_quarter = match smf.header.timing {
        Timing::Metrical(t) => t.as_int(),
        Timing::Timecode(_, _) => bail!("SMPTE time-code MIDI timing is not supported yet"),
    };
    let track_count = smf.tracks.len();

    let mut pending: Vec<Pending> = Vec::new();
    let mut tempos: Vec<(u64, u16, u32, u32)> = Vec::new();
    let mut time_sigs: Vec<(u64, u16, u32, u8, u8)> = Vec::new();
    let mut texts: Vec<TextEvent> = Vec::new();
    let mut has_lyrics = false;
    let mut max_tick = 0u64;

    for (ti, track) in smf.tracks.iter().enumerate() {
        let track_index = ti as u16;
        let mut tick = 0u64;

        for (ei, ev) in track.iter().enumerate() {
            tick += ev.delta.as_int() as u64;
            max_tick = max_tick.max(tick);
            let order = ei as u32;

            match ev.kind {
                TrackEventKind::Midi { channel, message } => {
                    let channel = channel.as_int();
                    let kind = match message {
                        MidiMessage::NoteOn { key, vel } => {
                            if vel.as_int() == 0 {
                                EventKind::NoteOff {
                                    channel,
                                    key: key.as_int(),
                                    velocity: 0,
                                }
                            } else {
                                EventKind::NoteOn {
                                    channel,
                                    key: key.as_int(),
                                    velocity: vel.as_int(),
                                }
                            }
                        }
                        MidiMessage::NoteOff { key, vel } => EventKind::NoteOff {
                            channel,
                            key: key.as_int(),
                            velocity: vel.as_int(),
                        },
                        MidiMessage::Controller { controller, value } => EventKind::ControlChange {
                            channel,
                            controller: controller.as_int(),
                            value: value.as_int(),
                        },
                        MidiMessage::ProgramChange { program } => EventKind::ProgramChange {
                            channel,
                            program: program.as_int(),
                        },
                        MidiMessage::PitchBend { bend } => EventKind::PitchBend {
                            channel,
                            // `midly` already gives a signed value in -8192..=8191
                            // where 0 is centre. Do NOT subtract 8192 again.
                            value: bend.as_int(),
                        },
                        MidiMessage::ChannelAftertouch { vel } => EventKind::ChannelPressure {
                            channel,
                            value: vel.as_int(),
                        },
                        MidiMessage::Aftertouch { key, vel } => EventKind::KeyPressure {
                            channel,
                            key: key.as_int(),
                            value: vel.as_int(),
                        },
                    };
                    pending.push(Pending {
                        tick,
                        track: track_index,
                        order,
                        kind,
                    });
                }
                TrackEventKind::Meta(meta) => match meta {
                    MetaMessage::Tempo(us) => {
                        let us_per_quarter = us.as_int();
                        tempos.push((tick, track_index, order, us_per_quarter));
                        pending.push(Pending {
                            tick,
                            track: track_index,
                            order,
                            kind: EventKind::Tempo {
                                us_per_quarter,
                                bpm: 60_000_000.0 / us_per_quarter as f64,
                            },
                        });
                    }
                    MetaMessage::TimeSignature(numerator, denominator_pow, _, _) => {
                        // The second byte is log2(denominator): 2 -> 1/4.
                        let denominator = 1u8 << (denominator_pow.min(7) as u32);
                        time_sigs.push((tick, track_index, order, numerator, denominator));
                    }
                    MetaMessage::Text(b) => {
                        push_text(
                            &mut texts,
                            &mut pending,
                            tick,
                            track_index,
                            order,
                            MetaTextKind::Text,
                            b,
                            enc,
                        );
                    }
                    MetaMessage::Lyric(b) => {
                        has_lyrics = true;
                        push_text(
                            &mut texts,
                            &mut pending,
                            tick,
                            track_index,
                            order,
                            MetaTextKind::Lyric,
                            b,
                            enc,
                        );
                    }
                    MetaMessage::Marker(b) => {
                        push_text(
                            &mut texts,
                            &mut pending,
                            tick,
                            track_index,
                            order,
                            MetaTextKind::Marker,
                            b,
                            enc,
                        );
                    }
                    MetaMessage::CuePoint(b) => {
                        push_text(
                            &mut texts,
                            &mut pending,
                            tick,
                            track_index,
                            order,
                            MetaTextKind::CuePoint,
                            b,
                            enc,
                        );
                    }
                    MetaMessage::TrackName(b) => {
                        push_text(
                            &mut texts,
                            &mut pending,
                            tick,
                            track_index,
                            order,
                            MetaTextKind::TrackName,
                            b,
                            enc,
                        );
                    }
                    MetaMessage::InstrumentName(b) => {
                        push_text(
                            &mut texts,
                            &mut pending,
                            tick,
                            track_index,
                            order,
                            MetaTextKind::InstrumentName,
                            b,
                            enc,
                        );
                    }
                    _ => {}
                },
                TrackEventKind::SysEx(_) | TrackEventKind::Escape(_) => pending.push(Pending {
                    tick,
                    track: track_index,
                    order,
                    kind: EventKind::SysEx,
                }),
            }
        }
    }

    // Duplicate tempo events at the same tick: last one (in track/event order)
    // wins, exactly like a sequencer replaying the file.
    tempos.sort_by_key(|(tick, track, order, _)| (*tick, *track, *order));
    let mut tempo_map = TempoMap::new(ticks_per_quarter);
    for &(tick, _, _, us) in &tempos {
        tempo_map.push(tick, us);
    }

    // Time signatures: last event at a tick wins, like tempo.
    time_sigs.sort_by_key(|(tick, track, order, _, _)| (*tick, *track, *order));
    let mut time_signatures: Vec<TimeSignatureChange> = Vec::new();
    for &(tick, _, _, numerator, denominator) in &time_sigs {
        if let Some(last) = time_signatures.last_mut() {
            if last.tick == tick {
                last.numerator = numerator;
                last.denominator = denominator;
                continue;
            }
        }
        time_signatures.push(TimeSignatureChange {
            tick,
            numerator,
            denominator,
        });
    }

    pending.sort_by_key(|p| (p.tick, p.track, p.order));
    let events = pending
        .into_iter()
        .map(|p| TimedEvent {
            at_us: tempo_map.tick_to_us(p.tick),
            tick: p.tick,
            track: p.track,
            kind: p.kind,
        })
        .collect();

    texts.sort_by_key(|t| (t.tick, t.track, t.order));
    for t in &mut texts {
        t.at_us = tempo_map.tick_to_us(t.tick);
    }

    Ok(MidiDoc {
        ticks_per_quarter,
        track_count,
        duration_us: tempo_map.tick_to_us(max_tick),
        tempo_map,
        time_signatures,
        max_tick,
        events,
        text_events: texts,
        has_lyrics,
    })
}

#[allow(clippy::too_many_arguments)]
fn push_text(
    texts: &mut Vec<TextEvent>,
    pending: &mut Vec<Pending>,
    tick: u64,
    track: u16,
    order: u32,
    kind: MetaTextKind,
    raw: &[u8],
    enc: EncodingChoice,
) {
    texts.push(TextEvent {
        tick,
        at_us: 0,
        track,
        order,
        kind,
        raw: raw.to_vec(),
    });
    pending.push(Pending {
        tick,
        track,
        order,
        kind: EventKind::Text {
            text_kind: kind,
            text: decode(raw, enc),
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_altered_midi_headers() {
        let normal: Vec<u8> = vec![
            b'M', b'T', b'h', b'd', 0, 0, 0, 6, 0, 0, 0, 1, 0, 96, b'M', b'T', b'r', b'k', 0, 0, 0,
            4, 0x00, 0xFF, 0x2F, 0x00,
        ];

        for marker in [b"Lock", b"ABCD", b"Xtra"] {
            let mut altered = normal.clone();
            altered[..4].copy_from_slice(marker);
            let doc = parse(&altered, EncodingChoice::Auto)
                .unwrap_or_else(|err| panic!("{marker:?}: {err:#}"));
            assert_eq!(doc.track_count, 1);
        }

        // A file that does not look like an SMF after the first four bytes is
        // still rejected.
        let mut broken = normal.clone();
        broken[..4].copy_from_slice(b"RIFF");
        broken[7] = 0; // invalid header length
        assert!(parse(&broken, EncodingChoice::Auto).is_err());
    }

    /// Regression test for the bug where a centred pitch bend (0x2000) was
    /// treated as a full downward bend, making every song play 2 semitones flat.
    #[test]
    fn pitch_bend_is_centered_on_zero() {
        let bytes: Vec<u8> = vec![
            b'M', b'T', b'h', b'd', 0, 0, 0, 6, 0, 0, 0, 1, 0, 96, b'M', b'T', b'r', b'k', 0, 0, 0,
            16, 0x00, 0xE0, 0x00, 0x40, // centre
            0x00, 0xE0, 0x00, 0x00, // full down
            0x00, 0xE0, 0x7F, 0x7F, // full up
            0x00, 0xFF, 0x2F, 0x00, // end of track
        ];

        let doc = parse(&bytes, EncodingChoice::Auto).unwrap();
        let bends: Vec<i16> = doc
            .events
            .iter()
            .filter_map(|event| match event.kind {
                EventKind::PitchBend { value, .. } => Some(value),
                _ => None,
            })
            .collect();

        assert_eq!(bends, vec![0, -8192, 8191]);
    }
}
