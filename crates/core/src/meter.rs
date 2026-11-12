//! Bar / beat position from MIDI time signatures.

use crate::model::TimeSignatureChange;

/// Where we are in the song, musically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarBeat {
    /// 1-based bar number.
    pub bar: u32,
    /// 1-based beat inside the bar.
    pub beat: u32,
    /// Numerator of the active time signature (4 for 4/4, 3 for 3/4, ...).
    pub beats_per_bar: u32,
    /// 0.0 at the start of the current beat, approaching 1.0 at the next one.
    pub beat_fraction: f32,
}

/// Time-signature map: converts a MIDI tick into bar/beat.
///
/// The default when a file declares no time signature is 4/4, which is what
/// practically every karaoke MIDI uses.
#[derive(Debug, Clone)]
pub struct MeterMap {
    pub division: u16,
    /// Sorted by tick; at most one change per tick.
    pub changes: Vec<TimeSignatureChange>,
}

impl MeterMap {
    pub fn new(division: u16, mut changes: Vec<TimeSignatureChange>) -> Self {
        changes.sort_by_key(|change| change.tick);
        Self {
            division: division.max(1),
            changes,
        }
    }

    pub fn position_at_tick(&self, tick: u64) -> BarBeat {
        let mut current = TimeSignatureChange {
            tick: 0,
            numerator: 4,
            denominator: 4,
        };
        let mut segment_start = 0u64;
        let mut bar_offset = 0u64;

        for change in &self.changes {
            if change.tick > tick {
                break;
            }
            if change.tick > segment_start {
                bar_offset += self.bars_between(segment_start, change.tick, &current);
            }
            current = *change;
            segment_start = change.tick;
        }

        let beats_per_bar = u64::from(current.numerator.max(1));
        let beat_ticks = self.beat_ticks(&current);
        let bar_ticks = beat_ticks * beats_per_bar;
        let elapsed = tick.saturating_sub(segment_start);

        BarBeat {
            bar: (bar_offset + elapsed / bar_ticks + 1) as u32,
            beat: ((elapsed % bar_ticks) / beat_ticks + 1) as u32,
            beats_per_bar: beats_per_bar as u32,
            beat_fraction: (elapsed % beat_ticks) as f32 / beat_ticks as f32,
        }
    }

    /// Tick at which the bar containing `tick` ends.
    ///
    /// If `tick` is exactly on a bar line it is returned unchanged. A time
    /// signature change inside the bar also counts as the end of that bar.
    pub fn bar_end_tick(&self, tick: u64) -> u64 {
        let mut current = TimeSignatureChange {
            tick: 0,
            numerator: 4,
            denominator: 4,
        };
        let mut segment_start = 0u64;

        for change in &self.changes {
            if change.tick > tick {
                break;
            }
            current = *change;
            segment_start = change.tick;
        }

        let beats = u64::from(current.numerator.max(1));
        let bar_ticks = (self.beat_ticks(&current) * beats).max(1);
        let elapsed = tick.saturating_sub(segment_start);
        let bar_start = segment_start + (elapsed / bar_ticks) * bar_ticks;
        let bar_end = if elapsed % bar_ticks == 0 {
            bar_start
        } else {
            bar_start + bar_ticks
        };

        match self
            .changes
            .iter()
            .find(|change| change.tick > tick)
            .map(|change| change.tick)
        {
            Some(change_tick) if change_tick < bar_end => change_tick,
            _ => bar_end,
        }
    }

    /// Tick at which the bar containing `tick` starts.
    ///
    /// A time signature change inside the bar also counts as the start of a
    /// new bar, matching [`Self::bar_end_tick`].
    pub fn bar_start_tick(&self, tick: u64) -> u64 {
        let mut current = TimeSignatureChange {
            tick: 0,
            numerator: 4,
            denominator: 4,
        };
        let mut segment_start = 0u64;

        for change in &self.changes {
            if change.tick > tick {
                break;
            }
            current = *change;
            segment_start = change.tick;
        }

        let beats = u64::from(current.numerator.max(1));
        let bar_ticks = (self.beat_ticks(&current) * beats).max(1);
        let elapsed = tick.saturating_sub(segment_start);
        segment_start + (elapsed / bar_ticks) * bar_ticks
    }

    /// Tick of the next bar line strictly after `tick`.
    pub fn next_bar_tick(&self, tick: u64) -> u64 {
        let end = self.bar_end_tick(tick);
        if end > tick {
            end
        } else {
            self.bar_end_tick(tick.saturating_add(1))
        }
    }

    /// Tick of the start of the previous bar. At the start of the song this
    /// is 0, so the bar number decreases by one per call.
    pub fn previous_bar_tick(&self, tick: u64) -> u64 {
        let start = self.bar_start_tick(tick);
        if start == 0 {
            0
        } else {
            self.bar_start_tick(start - 1)
        }
    }

    fn beat_ticks(&self, signature: &TimeSignatureChange) -> u64 {
        let denominator = u64::from(signature.denominator.max(1));
        (u64::from(self.division) * 4 / denominator).max(1)
    }

    fn bars_between(&self, from: u64, to: u64, signature: &TimeSignatureChange) -> u64 {
        let bar_ticks = self.beat_ticks(signature) * u64::from(signature.numerator.max(1));
        to.saturating_sub(from) / bar_ticks.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(tick: u64, numerator: u8, denominator: u8) -> TimeSignatureChange {
        TimeSignatureChange {
            tick,
            numerator,
            denominator,
        }
    }

    fn pos(map: &MeterMap, tick: u64) -> (u32, u32, u32) {
        let position = map.position_at_tick(tick);
        (position.bar, position.beat, position.beats_per_bar)
    }

    #[test]
    fn defaults_to_four_four() {
        let map = MeterMap::new(480, Vec::new());
        assert_eq!(pos(&map, 0), (1, 1, 4));
        assert_eq!(pos(&map, 480), (1, 2, 4));
        assert_eq!(pos(&map, 1919), (1, 4, 4));
        assert_eq!(pos(&map, 1920), (2, 1, 4));
    }

    #[test]
    fn three_four() {
        let map = MeterMap::new(480, vec![change(0, 3, 4)]);
        assert_eq!(pos(&map, 480), (1, 2, 3));
        assert_eq!(pos(&map, 1440), (2, 1, 3));
    }

    #[test]
    fn six_eight_uses_eighth_note_beats() {
        let map = MeterMap::new(480, vec![change(0, 6, 8)]);
        assert_eq!(pos(&map, 240), (1, 2, 6));
        assert_eq!(pos(&map, 1440), (2, 1, 6));
    }

    #[test]
    fn changes_continue_the_bar_count() {
        let map = MeterMap::new(480, vec![change(0, 4, 4), change(1920, 3, 4)]);
        assert_eq!(pos(&map, 1920), (2, 1, 3));
        assert_eq!(pos(&map, 1920 + 1440), (3, 1, 3));
    }

    #[test]
    fn beat_fraction_tracks_the_beat() {
        let map = MeterMap::new(480, Vec::new());
        assert_eq!(map.position_at_tick(0).beat_fraction, 0.0);
        assert!((map.position_at_tick(240).beat_fraction - 0.5).abs() < 1e-6);
        assert!((map.position_at_tick(479).beat_fraction - 479.0 / 480.0).abs() < 1e-6);
    }

    #[test]
    fn bar_end_rounds_to_the_next_bar_line() {
        let map = MeterMap::new(480, Vec::new());
        assert_eq!(map.bar_end_tick(0), 0);
        assert_eq!(map.bar_end_tick(1), 1920);
        assert_eq!(map.bar_end_tick(1919), 1920);
        assert_eq!(map.bar_end_tick(1920), 1920);
        assert_eq!(map.bar_end_tick(1921), 3840);
    }

    #[test]
    fn bar_navigation_moves_one_bar_at_a_time() {
        let map = MeterMap::new(480, Vec::new());
        assert_eq!(map.next_bar_tick(0), 1920);
        assert_eq!(map.next_bar_tick(1), 1920);
        assert_eq!(map.next_bar_tick(1919), 1920);
        assert_eq!(map.next_bar_tick(1920), 3840);
        assert_eq!(map.previous_bar_tick(0), 0);
        assert_eq!(map.previous_bar_tick(1), 0);
        assert_eq!(map.previous_bar_tick(1920), 0);
        assert_eq!(map.previous_bar_tick(1921), 0);
        assert_eq!(map.previous_bar_tick(3840), 1920);
        assert_eq!(map.previous_bar_tick(3841), 1920);
    }

    #[test]
    fn bar_navigation_respects_signature_changes() {
        let map = MeterMap::new(480, vec![change(0, 4, 4), change(1000, 3, 4)]);
        assert_eq!(map.next_bar_tick(500), 1000);
        assert_eq!(map.next_bar_tick(1000), 1000 + 1440);
        assert_eq!(map.previous_bar_tick(1001), 0);
        assert_eq!(map.previous_bar_tick(1000), 0);
        assert_eq!(map.previous_bar_tick(2441), 1000);
    }

    #[test]
    fn bar_end_respects_a_signature_change() {
        let map = MeterMap::new(480, vec![change(0, 4, 4), change(1000, 3, 4)]);
        assert_eq!(map.bar_end_tick(500), 1000);
        assert_eq!(map.bar_end_tick(1000), 1000);
        assert_eq!(map.bar_end_tick(1001), 1000 + 1440);
    }
}
