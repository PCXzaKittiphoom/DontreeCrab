//! Live MIDI state shared between the audio thread and the controller UI.
//!
//! The audio callback writes with relaxed atomics; the UI reads a snapshot at
//! its own pace. Nothing here locks, so the render callback never waits.

use std::sync::atomic::{AtomicI16, AtomicU8, AtomicU16, Ordering};

pub const CHANNELS: usize = 16;
pub const NOTES: usize = 128;

/// Per-channel state shown by the MIDI Controller.
pub struct MidiMonitor {
    volume: [AtomicU8; CHANNELS],
    program: [AtomicU8; CHANNELS],
    bank: [AtomicU8; CHANNELS],
    pan: [AtomicU8; CHANNELS],
    reverb: [AtomicU8; CHANNELS],
    chorus: [AtomicU8; CHANNELS],
    bend: [AtomicI16; CHANNELS],
    modulation: [AtomicU8; CHANNELS],
    velocity: [AtomicU8; CHANNELS],
    muted: AtomicU16,
    /// For each note, a bitmask of the channels currently sounding it.
    notes: [AtomicU16; NOTES],
}

impl Default for MidiMonitor {
    fn default() -> Self {
        Self {
            volume: std::array::from_fn(|_| AtomicU8::new(100)),
            program: std::array::from_fn(|_| AtomicU8::new(0)),
            bank: std::array::from_fn(|_| AtomicU8::new(0)),
            pan: std::array::from_fn(|_| AtomicU8::new(64)),
            reverb: std::array::from_fn(|_| AtomicU8::new(40)),
            chorus: std::array::from_fn(|_| AtomicU8::new(0)),
            bend: std::array::from_fn(|_| AtomicI16::new(0)),
            modulation: std::array::from_fn(|_| AtomicU8::new(0)),
            velocity: std::array::from_fn(|_| AtomicU8::new(0)),
            muted: AtomicU16::new(0),
            notes: std::array::from_fn(|_| AtomicU16::new(0)),
        }
    }
}

/// A copy of the monitor for one UI frame.
#[derive(Clone)]
pub struct MidiMonitorSnapshot {
    pub volume: [u8; CHANNELS],
    pub program: [u8; CHANNELS],
    pub bank: [u8; CHANNELS],
    pub pan: [u8; CHANNELS],
    pub reverb: [u8; CHANNELS],
    pub chorus: [u8; CHANNELS],
    pub bend: [i16; CHANNELS],
    pub modulation: [u8; CHANNELS],
    pub velocity: [u8; CHANNELS],
    pub muted: u16,
    pub notes: [u16; NOTES],
}

impl Default for MidiMonitorSnapshot {
    fn default() -> Self {
        Self {
            volume: [100; CHANNELS],
            program: [0; CHANNELS],
            bank: [0; CHANNELS],
            pan: [64; CHANNELS],
            reverb: [40; CHANNELS],
            chorus: [0; CHANNELS],
            bend: [0; CHANNELS],
            modulation: [0; CHANNELS],
            velocity: [0; CHANNELS],
            muted: 0,
            notes: [0; NOTES],
        }
    }
}

impl MidiMonitor {
    pub fn note_on(&self, channel: u8, note: u8, velocity: u8) {
        if channel < CHANNELS as u8 && note < NOTES as u8 {
            self.notes[note as usize].fetch_or(1 << channel, Ordering::Relaxed);
            self.velocity[channel as usize].store(velocity.min(127), Ordering::Relaxed);
        }
    }

    pub fn note_off(&self, channel: u8, note: u8) {
        if channel < CHANNELS as u8 && note < NOTES as u8 {
            self.notes[note as usize].fetch_and(!(1 << channel), Ordering::Relaxed);
        }
    }

    pub fn set_volume(&self, channel: u8, value: u8) {
        if channel < CHANNELS as u8 {
            self.volume[channel as usize].store(value.min(127), Ordering::Relaxed);
        }
    }

    pub fn set_program(&self, channel: u8, program: u8) {
        if channel < CHANNELS as u8 {
            self.program[channel as usize].store(program.min(127), Ordering::Relaxed);
        }
    }

    pub fn set_bank(&self, channel: u8, value: u8) {
        if channel < CHANNELS as u8 {
            self.bank[channel as usize].store(value.min(127), Ordering::Relaxed);
        }
    }

    pub fn set_pan(&self, channel: u8, value: u8) {
        if channel < CHANNELS as u8 {
            self.pan[channel as usize].store(value.min(127), Ordering::Relaxed);
        }
    }

    pub fn set_reverb(&self, channel: u8, value: u8) {
        if channel < CHANNELS as u8 {
            self.reverb[channel as usize].store(value.min(127), Ordering::Relaxed);
        }
    }

    pub fn set_chorus(&self, channel: u8, value: u8) {
        if channel < CHANNELS as u8 {
            self.chorus[channel as usize].store(value.min(127), Ordering::Relaxed);
        }
    }

    pub fn set_bend(&self, channel: u8, value: i16) {
        if channel < CHANNELS as u8 {
            self.bend[channel as usize].store(value.clamp(-8192, 8191), Ordering::Relaxed);
        }
    }

    pub fn set_modulation(&self, channel: u8, value: u8) {
        if channel < CHANNELS as u8 {
            self.modulation[channel as usize].store(value.min(127), Ordering::Relaxed);
        }
    }

    pub fn set_muted_mask(&self, mask: u16) {
        self.muted.store(mask, Ordering::Relaxed);
    }

    /// Clear one channel's sounding notes and velocity.
    pub fn clear_channel(&self, channel: u8) {
        if channel >= CHANNELS as u8 {
            return;
        }
        for note in &self.notes {
            note.fetch_and(!(1 << channel), Ordering::Relaxed);
        }
        self.velocity[channel as usize].store(0, Ordering::Relaxed);
    }

    /// Clear sounding notes and their velocities (seek, stop, new song).
    pub fn clear_notes(&self) {
        for note in &self.notes {
            note.store(0, Ordering::Relaxed);
        }
        for velocity in &self.velocity {
            velocity.store(0, Ordering::Relaxed);
        }
    }

    /// Reset the channel state for a fresh song.
    pub fn reset(&self) {
        self.clear_notes();
        for volume in &self.volume {
            volume.store(100, Ordering::Relaxed);
        }
        for program in &self.program {
            program.store(0, Ordering::Relaxed);
        }
        for bank in &self.bank {
            bank.store(0, Ordering::Relaxed);
        }
        for pan in &self.pan {
            pan.store(64, Ordering::Relaxed);
        }
        for reverb in &self.reverb {
            reverb.store(40, Ordering::Relaxed);
        }
        for chorus in &self.chorus {
            chorus.store(0, Ordering::Relaxed);
        }
        for bend in &self.bend {
            bend.store(0, Ordering::Relaxed);
        }
        for modulation in &self.modulation {
            modulation.store(0, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> MidiMonitorSnapshot {
        let mut snapshot = MidiMonitorSnapshot {
            volume: [100; CHANNELS],
            program: [0; CHANNELS],
            bank: [0; CHANNELS],
            pan: [64; CHANNELS],
            reverb: [40; CHANNELS],
            chorus: [0; CHANNELS],
            bend: [0; CHANNELS],
            modulation: [0; CHANNELS],
            velocity: [0; CHANNELS],
            muted: self.muted.load(Ordering::Relaxed),
            notes: [0; NOTES],
        };
        for channel in 0..CHANNELS {
            snapshot.volume[channel] = self.volume[channel].load(Ordering::Relaxed);
            snapshot.program[channel] = self.program[channel].load(Ordering::Relaxed);
            snapshot.bank[channel] = self.bank[channel].load(Ordering::Relaxed);
            snapshot.pan[channel] = self.pan[channel].load(Ordering::Relaxed);
            snapshot.reverb[channel] = self.reverb[channel].load(Ordering::Relaxed);
            snapshot.chorus[channel] = self.chorus[channel].load(Ordering::Relaxed);
            snapshot.bend[channel] = self.bend[channel].load(Ordering::Relaxed);
            snapshot.modulation[channel] = self.modulation[channel].load(Ordering::Relaxed);
            snapshot.velocity[channel] = self.velocity[channel].load(Ordering::Relaxed);
        }
        for (note, value) in snapshot.notes.iter_mut().enumerate() {
            *value = self.notes[note].load(Ordering::Relaxed);
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_tracked_per_channel() {
        let monitor = MidiMonitor::default();
        monitor.note_on(0, 60, 100);
        monitor.note_on(2, 60, 80);
        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.notes[60] & 0b101, 0b101);
        assert_eq!(snapshot.velocity[0], 100);
        assert_eq!(snapshot.velocity[2], 80);

        monitor.note_off(0, 60);
        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.notes[60] & 0b101, 0b100);
    }

    #[test]
    fn volume_program_bend_and_modulation_round_trip() {
        let monitor = MidiMonitor::default();
        monitor.set_volume(1, 42);
        monitor.set_program(1, 33);
        monitor.set_bank(1, 5);
        monitor.set_pan(1, 96);
        monitor.set_reverb(1, 80);
        monitor.set_chorus(1, 12);
        monitor.set_bend(1, -2048);
        monitor.set_modulation(1, 64);
        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.volume[1], 42);
        assert_eq!(snapshot.program[1], 33);
        assert_eq!(snapshot.bank[1], 5);
        assert_eq!(snapshot.pan[1], 96);
        assert_eq!(snapshot.reverb[1], 80);
        assert_eq!(snapshot.chorus[1], 12);
        assert_eq!(snapshot.bend[1], -2048);
        assert_eq!(snapshot.modulation[1], 64);
    }

    #[test]
    fn reset_restores_the_channel_detail_defaults() {
        let monitor = MidiMonitor::default();
        monitor.set_volume(2, 10);
        monitor.set_program(2, 40);
        monitor.set_bank(2, 9);
        monitor.set_pan(2, 5);
        monitor.set_reverb(2, 100);
        monitor.set_chorus(2, 100);
        monitor.reset();
        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.volume[2], 100);
        assert_eq!(snapshot.program[2], 0);
        assert_eq!(snapshot.bank[2], 0);
        assert_eq!(snapshot.pan[2], 64);
        assert_eq!(snapshot.reverb[2], 40);
        assert_eq!(snapshot.chorus[2], 0);
    }

    #[test]
    fn clearing_notes_keeps_the_channel_settings() {
        let monitor = MidiMonitor::default();
        monitor.set_volume(3, 90);
        monitor.note_on(3, 64, 110);
        monitor.clear_notes();
        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.notes[64], 0);
        assert_eq!(snapshot.velocity[3], 0);
        assert_eq!(snapshot.volume[3], 90);
    }
}
