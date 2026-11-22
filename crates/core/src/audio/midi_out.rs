//! Sending the song to an external MIDI output port.
//!
//! The internal SoundFont synthesizer is used when no port is selected. When
//! the user picks a port, the player forwards the same messages it would have
//! sent to the synthesizer (after the voice locks and transposition) and stays
//! silent, so the external device is the only sound source.

use std::fmt;

use anyhow::{Context, Result, anyhow};
use midir::{MidiOutput, MidiOutputConnection, MidiOutputPort};

/// Client name reported to the operating system.
const CLIENT_NAME: &str = "DontreeCrab";

/// Names of the MIDI output ports available on this machine.
///
/// The list is empty when the platform offers no MIDI output or no ports are
/// installed; callers should then keep using the SoundFont.
pub fn output_ports() -> Vec<String> {
    let Ok(output) = MidiOutput::new(CLIENT_NAME) else {
        return Vec::new();
    };
    output
        .ports()
        .iter()
        .filter_map(|port| output.port_name(port).ok())
        .collect()
}

/// An open connection to an external MIDI device.
pub struct MidiOut {
    connection: MidiOutputConnection,
    name: String,
}

impl fmt::Debug for MidiOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MidiOut").field("port", &self.name).finish()
    }
}

impl MidiOut {
    /// Open the output port whose name matches `name`.
    ///
    /// An exact match wins; otherwise the first port whose name contains the
    /// request is used, which keeps saved settings working when the operating
    /// system appends a device index to the name.
    pub fn open(name: &str) -> Result<Self> {
        let output = MidiOutput::new(CLIENT_NAME).context("MIDI output is not available")?;
        let ports = output.ports();
        let mut names = Vec::with_capacity(ports.len());
        for port in &ports {
            names.push(output.port_name(port).unwrap_or_default());
        }
        let index = names
            .iter()
            .position(|candidate| candidate == name)
            .or_else(|| {
                names
                    .iter()
                    .position(|candidate| !name.is_empty() && candidate.contains(name))
            })
            .ok_or_else(|| anyhow!("MIDI output \"{name}\" is not available"))?;
        let port: MidiOutputPort = ports
            .into_iter()
            .nth(index)
            .ok_or_else(|| anyhow!("MIDI output \"{name}\" disappeared"))?;
        let connection = output
            .connect(&port, CLIENT_NAME)
            .map_err(|err| anyhow!("could not open MIDI output \"{name}\": {err}"))?;
        Ok(Self {
            connection,
            name: names[index].clone(),
        })
    }

    /// The port name as reported by the operating system.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Send a raw MIDI message. A disconnected device is ignored so playback
    /// keeps running instead of stopping the song.
    pub fn send(&mut self, message: &[u8]) {
        let _ = self.connection.send(message);
    }

    /// Put every channel in a clean state: no notes, default controllers and
    /// a centred pitch bend. Used before replaying a song's state into a
    /// device that may still hold values from an earlier song.
    pub fn reset_channels(&mut self) {
        for channel in 0u8..16 {
            let status = 0xB0 | (channel & 0x0F);
            self.send(&[status, 64, 0]); // sustain off
            self.send(&[status, 120, 0]); // all sound off
            self.send(&[status, 123, 0]); // all notes off
            self.send(&[status, 121, 0]); // reset all controllers
            self.send(&[0xE0 | (channel & 0x0F), 0x00, 0x40]); // centre bend
        }
    }

    /// Stop the notes sounding on one channel (used when it is muted).
    pub fn channel_off(&mut self, channel: u8) {
        let status = 0xB0 | (channel & 0x0F);
        self.send(&[status, 123, 0]); // all notes off
        self.send(&[status, 120, 0]); // all sound off
    }

    /// Stop every sounding note; used on pause, stop, seek and at the end.
    pub fn panic(&mut self) {
        for channel in 0u8..16 {
            self.channel_off(channel);
        }
    }
}

/// Number of General MIDI instrument groups used by the Split MIDI window.
pub const GROUPS: usize = 20;

/// General MIDI instrument group for a program number (0-127).
///
/// The ten GM families map to eight groups each, except the guitar family
/// (programs 24-31), which the player splits into five groups.
pub fn program_group(program: u8) -> usize {
    match program {
        0..=23 => usize::from(program) / 8,
        24..=25 => 3, // Acoustic Guitar
        26 => 5,      // Jazz Guitar
        27..=28 => 4, // Clean Guitar
        29 => 6,      // Overdrive Guitar
        30..=31 => 7, // Distortion Guitar
        _ => 8 + usize::from(program - 32) / 8,
    }
}

/// Number of General MIDI drum groups used by the Split MIDI window.
pub const DRUM_GROUPS: usize = 9;

/// Drum group for a General MIDI percussion note (channel 10).
///
/// The notes follow the standard GM percussion map. Notes that are not listed
/// here keep following the drum channel's own output.
pub fn drum_group(note: u8) -> Option<usize> {
    match note {
        // Kick
        35 | 36 => Some(0),
        // Snare
        31 | 34 | 37 | 38 | 40 => Some(1),
        // Tom
        41 | 43 | 45 | 47 | 48 | 50 => Some(2),
        // Hi-hat
        42 | 44 | 46 => Some(3),
        // Cowbell
        56 => Some(4),
        // Cymbal
        49 | 51 | 52 | 53 | 55 | 57 | 59 => Some(5),
        // Hand percussion
        60..=66 | 86 | 87 => Some(6),
        // Misc percussion
        27..=30 | 32 | 33 | 39 | 54 | 58 | 67..=74 | 77..=85 => Some(7),
        // Metronome
        75 | 76 => Some(8),
        _ => None,
    }
}

/// Settings value that pins a channel to the built-in synthesizer.
///
/// An empty string means "follow the default output"; this marker keeps a
/// channel on the synth even when the default is a device.
pub const INTERNAL_PORT: &str = "rustysynth";

/// Where one channel is sent.
#[derive(Clone, Copy, Debug)]
enum ChannelRoute {
    /// Follow the default output.
    Default,
    /// The built-in synthesizer, even when the default is a device.
    Internal,
    /// One of the open outputs.
    Output(usize),
}

/// One or more open outputs plus the routing that selects them per channel.
///
/// The default output is the "MIDI Out" choice from the settings; a channel can
/// override it, and an empty name means the built-in synthesizer.
pub struct MidiRouting {
    outputs: Vec<MidiOut>,
    /// Index of the default output (`None` = the built-in synthesizer).
    default: Option<usize>,
    /// Per-channel choice.
    channels: [ChannelRoute; 16],
    /// Per-group choice (`None` = follow the channel).
    groups: [Option<usize>; GROUPS],
    /// Per-drum-group choice (`None` = follow the drum channel).
    drums: [Option<usize>; DRUM_GROUPS],
}

impl fmt::Debug for MidiRouting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.outputs.iter().map(MidiOut::name).collect();
        f.debug_struct("MidiRouting")
            .field("outputs", &names)
            .field("default", &self.default)
            .field("channels", &self.channels)
            .field("groups", &self.groups)
            .field("drums", &self.drums)
            .finish()
    }
}

impl MidiRouting {
    /// Everything plays on the built-in synthesizer.
    pub fn internal() -> Self {
        Self {
            outputs: Vec::new(),
            default: None,
            channels: [ChannelRoute::Default; 16],
            groups: [None; GROUPS],
            drums: [None; DRUM_GROUPS],
        }
    }

    /// Open every distinct port named by `default` and `channels`.
    ///
    /// An empty name means the built-in synthesizer. A port that cannot be
    /// opened fails the whole routing so the caller can report it.
    pub fn open(
        default: &str,
        channels: &[String; 16],
        groups: &[String; GROUPS],
        drums: &[String; DRUM_GROUPS],
    ) -> std::result::Result<Self, String> {
        let mut outputs = Vec::new();
        let mut default_index = None;
        let default = default.trim();
        if !default.is_empty() {
            default_index = Some(index_or_open(&mut outputs, default)?);
        }
        let mut channel_routes = [ChannelRoute::Default; 16];
        for (channel, name) in channels.iter().enumerate() {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            channel_routes[channel] = if name.eq_ignore_ascii_case(INTERNAL_PORT) {
                ChannelRoute::Internal
            } else {
                ChannelRoute::Output(index_or_open(&mut outputs, name)?)
            };
        }
        let mut group_routes = [None; GROUPS];
        for (group, name) in groups.iter().enumerate() {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            group_routes[group] = if name.eq_ignore_ascii_case(INTERNAL_PORT) {
                // Pinned to the synth: keep the group off every output.
                Some(usize::MAX)
            } else {
                Some(index_or_open(&mut outputs, name)?)
            };
        }
        let mut drum_routes = [None; DRUM_GROUPS];
        for (group, name) in drums.iter().enumerate() {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            drum_routes[group] = if name.eq_ignore_ascii_case(INTERNAL_PORT) {
                Some(usize::MAX)
            } else {
                Some(index_or_open(&mut outputs, name)?)
            };
        }
        Ok(Self {
            outputs,
            default: default_index,
            channels: channel_routes,
            groups: group_routes,
            drums: drum_routes,
        })
    }

    /// Route for one melodic channel playing `program`.
    ///
    /// An instrument group wins over the channel table; the channel table wins
    /// over the default output.
    pub fn route_for(&self, channel: usize, program: u8) -> Option<usize> {
        if channel != 9 {
            match self.groups.get(program_group(program)).copied().flatten() {
                Some(index) if index == usize::MAX => return None,
                Some(index) => return Some(index),
                None => {}
            }
        }
        self.route(channel)
    }

    /// Route for one drum note on channel 10.
    ///
    /// A drum group wins over the channel table; unlisted notes keep following
    /// the channel's own output.
    pub fn route_for_drum(&self, note: u8) -> Option<usize> {
        if let Some(group) = drum_group(note)
            && let Some(index) = self.drums.get(group).copied().flatten()
        {
            return if index == usize::MAX {
                None
            } else {
                Some(index)
            };
        }
        self.route(9)
    }

    /// True when no output port is open at all.
    pub fn is_internal(&self) -> bool {
        self.outputs.is_empty()
    }

    /// Index of the default output.
    pub fn default_index(&self) -> Option<usize> {
        self.default
    }

    /// Output index that plays `channel` (`None` = the built-in synth).
    pub fn route(&self, channel: usize) -> Option<usize> {
        match self.channels.get(channel) {
            Some(ChannelRoute::Output(index)) => Some(*index),
            Some(ChannelRoute::Internal) => None,
            Some(ChannelRoute::Default) | None => self.default,
        }
    }

    /// Send one message to the output selected by `route`.
    pub fn send(&mut self, route: Option<usize>, message: &[u8]) {
        if let Some(index) = route {
            if let Some(out) = self.outputs.get_mut(index) {
                out.send(message);
            }
        }
    }

    /// Send one message to every open output.
    pub fn send_all(&mut self, message: &[u8]) {
        for out in &mut self.outputs {
            out.send(message);
        }
    }

    /// Stop the notes sounding on one channel of one output.
    pub fn channel_off(&mut self, route: Option<usize>, channel: u8) {
        if let Some(index) = route {
            if let Some(out) = self.outputs.get_mut(index) {
                out.channel_off(channel);
            }
        }
    }

    /// Put every output in a clean state.
    pub fn reset_all(&mut self) {
        for out in &mut self.outputs {
            out.reset_channels();
        }
    }

    /// Stop every note on every output.
    pub fn panic_all(&mut self) {
        for out in &mut self.outputs {
            out.panic();
        }
    }
}

/// Index of `name` in `outputs`, opening it when it is not connected yet.
fn index_or_open(outputs: &mut Vec<MidiOut>, name: &str) -> std::result::Result<usize, String> {
    if let Some(index) = outputs.iter().position(|out| out.name() == name) {
        return Ok(index);
    }
    let out = MidiOut::open(name).map_err(|err| format!("{err:#}"))?;
    outputs.push(out);
    Ok(outputs.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_programs_to_the_twenty_groups() {
        assert_eq!(program_group(0), 0);
        assert_eq!(program_group(7), 0);
        assert_eq!(program_group(8), 1);
        assert_eq!(program_group(23), 2);
        // The guitar family is split into five groups.
        assert_eq!(program_group(24), 3);
        assert_eq!(program_group(25), 3);
        assert_eq!(program_group(26), 5);
        assert_eq!(program_group(27), 4);
        assert_eq!(program_group(28), 4);
        assert_eq!(program_group(29), 6);
        assert_eq!(program_group(30), 7);
        assert_eq!(program_group(31), 7);
        assert_eq!(program_group(32), 8);
        assert_eq!(program_group(39), 8);
        assert_eq!(program_group(40), 9);
        assert_eq!(program_group(120), 19);
        assert_eq!(program_group(127), 19);
    }

    fn routing() -> MidiRouting {
        MidiRouting {
            outputs: Vec::new(),
            default: None,
            channels: [ChannelRoute::Output(0); 16],
            groups: [None; GROUPS],
            drums: [None; DRUM_GROUPS],
        }
    }

    #[test]
    fn maps_drum_notes_to_the_nine_groups() {
        for note in [35, 36] {
            assert_eq!(drum_group(note), Some(0), "kick {note}");
        }
        for note in [31, 34, 37, 38, 40] {
            assert_eq!(drum_group(note), Some(1), "snare {note}");
        }
        for note in [41, 43, 45, 47, 48, 50] {
            assert_eq!(drum_group(note), Some(2), "tom {note}");
        }
        for note in [42, 44, 46] {
            assert_eq!(drum_group(note), Some(3), "hi-hat {note}");
        }
        assert_eq!(drum_group(56), Some(4));
        for note in [49, 51, 52, 53, 55, 57, 59] {
            assert_eq!(drum_group(note), Some(5), "cymbal {note}");
        }
        for note in [60, 61, 62, 63, 64, 65, 66, 86, 87] {
            assert_eq!(drum_group(note), Some(6), "hand percussion {note}");
        }
        for note in [
            27, 28, 29, 30, 32, 33, 39, 54, 58, 67, 68, 69, 70, 71, 72, 73, 74, 77, 78, 79, 80, 81,
            82, 83, 84, 85,
        ] {
            assert_eq!(drum_group(note), Some(7), "misc percussion {note}");
        }
        assert_eq!(drum_group(75), Some(8));
        assert_eq!(drum_group(76), Some(8));
        // Notes outside the list keep following the channel.
        assert_eq!(drum_group(0), None);
        assert_eq!(drum_group(88), None);
    }

    #[test]
    fn a_drum_group_wins_over_the_drum_channel() {
        let mut routing = routing();
        routing.drums[0] = Some(1);
        assert_eq!(routing.route_for_drum(35), Some(1));
        assert_eq!(routing.route_for_drum(36), Some(1));
        // A snare still follows the channel.
        assert_eq!(routing.route_for_drum(38), Some(0));
    }

    #[test]
    fn a_drum_group_can_force_the_built_in_synth() {
        let mut routing = routing();
        routing.drums[3] = Some(usize::MAX);
        assert_eq!(routing.route_for_drum(42), None);
    }

    #[test]
    fn unlisted_drum_notes_follow_the_channel() {
        let mut routing = routing();
        routing.drums[0] = Some(1);
        routing.default = Some(2);
        routing.channels[9] = ChannelRoute::Default;
        assert_eq!(routing.route_for_drum(88), Some(2));
        assert_eq!(routing.route_for_drum(35), Some(1));
    }

    #[test]
    fn a_group_wins_over_the_channel() {
        let mut routing = routing();
        routing.groups[0] = Some(1);
        assert_eq!(routing.route_for(0, 0), Some(1));
        // Programs outside that group still follow the channel.
        assert_eq!(routing.route_for(0, 8), Some(0));
    }

    #[test]
    fn a_group_can_force_the_built_in_synth() {
        let mut routing = routing();
        routing.groups[0] = Some(usize::MAX);
        assert_eq!(routing.route_for(0, 0), None);
    }

    #[test]
    fn the_drum_channel_ignores_the_groups() {
        let mut routing = routing();
        routing.groups[0] = Some(1);
        assert_eq!(routing.route_for(9, 0), Some(0));
    }
}
