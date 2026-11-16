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
}

impl fmt::Debug for MidiRouting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.outputs.iter().map(MidiOut::name).collect();
        f.debug_struct("MidiRouting")
            .field("outputs", &names)
            .field("default", &self.default)
            .field("channels", &self.channels)
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
        }
    }

    /// Open every distinct port named by `default` and `channels`.
    ///
    /// An empty name means the built-in synthesizer. A port that cannot be
    /// opened fails the whole routing so the caller can report it.
    pub fn open(default: &str, channels: &[String; 16]) -> std::result::Result<Self, String> {
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
        Ok(Self {
            outputs,
            default: default_index,
            channels: channel_routes,
        })
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
