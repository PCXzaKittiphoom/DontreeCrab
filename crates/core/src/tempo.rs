use serde::Serialize;

/// One `Set Tempo` (0x51) change.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TempoSegment {
    pub tick: u64,
    /// Microseconds per quarter note.
    pub us_per_quarter: u32,
}

/// Converts MIDI ticks to absolute time, honouring tempo changes.
///
/// All maths is done in `u128` so long songs at high tick resolutions cannot
/// overflow, and the result is truncated to whole microseconds.
#[derive(Debug, Clone, Serialize)]
pub struct TempoMap {
    pub division: u16,
    /// Sorted by `tick`, at most one entry per tick.
    pub segments: Vec<TempoSegment>,
}

impl TempoMap {
    pub fn new(division: u16) -> Self {
        Self {
            division: division.max(1),
            segments: vec![TempoSegment {
                tick: 0,
                us_per_quarter: 500_000,
            }],
        }
    }

    /// Add a tempo change. Must be called in non-decreasing `tick` order;
    /// several events at the same tick keep the last one, which matches how
    /// sequencers interpret duplicate `Set Tempo` events.
    pub fn push(&mut self, tick: u64, us_per_quarter: u32) {
        let us = us_per_quarter.clamp(1, 0x0FFF_FFFF);
        if let Some(last) = self.segments.last_mut() {
            if last.tick == tick {
                last.us_per_quarter = us;
            } else {
                debug_assert!(last.tick < tick, "tempo segments must be sorted");
                self.segments.push(TempoSegment {
                    tick,
                    us_per_quarter: us,
                });
            }
        }
    }

    pub fn us_per_quarter_at(&self, tick: u64) -> u32 {
        let mut cur = 500_000;
        for seg in &self.segments {
            if seg.tick > tick {
                break;
            }
            cur = seg.us_per_quarter;
        }
        cur
    }

    pub fn bpm_at(&self, tick: u64) -> f64 {
        60_000_000.0 / self.us_per_quarter_at(tick) as f64
    }

    pub fn tick_to_us(&self, tick: u64) -> u64 {
        let div = self.division as u128;
        let mut us: u128 = 0;
        let mut last_tick: u64 = 0;
        let mut cur: u128 = 500_000;

        for seg in &self.segments {
            if tick <= seg.tick {
                return (us + (tick - last_tick) as u128 * cur / div) as u64;
            }
            us += (seg.tick - last_tick) as u128 * cur / div;
            last_tick = seg.tick;
            cur = seg.us_per_quarter as u128;
        }

        (us + (tick - last_tick) as u128 * cur / div) as u64
    }

    /// Inverse of [`Self::tick_to_us`]. Used to turn the playback position
    /// (song time) back into a musical tick for bar/beat display.
    pub fn us_to_tick(&self, us: u64) -> u64 {
        let div = self.division as u128;
        let mut accumulated_us: u128 = 0;
        let mut last_tick: u64 = 0;
        let mut cur: u128 = 500_000;

        for segment in &self.segments {
            let segment_us = (segment.tick - last_tick) as u128 * cur / div;
            if (us as u128) < accumulated_us + segment_us {
                let remainder = (us as u128).saturating_sub(accumulated_us);
                return last_tick + (remainder * div / cur) as u64;
            }
            accumulated_us += segment_us;
            last_tick = segment.tick;
            cur = segment.us_per_quarter as u128;
        }

        let remainder = (us as u128).saturating_sub(accumulated_us);
        last_tick + (remainder * div / cur) as u64
    }
}
