//! Game state and the fixed-timestep update, shared by the rendered and
//! headless paths so physics behaviour cannot diverge between them.

use crate::track::Track;
use crate::vehicle::{Controls, Vehicle};

/// Physics runs at a fixed rate independent of frame rate. 120 Hz keeps the
/// stiff suspension stable without costing much on a 6-core Zen 2.
pub const TICK_RATE: f32 = 120.0;
pub const TICK_DT: f32 = 1.0 / TICK_RATE;

pub struct Sim {
    pub track: Track,
    pub player: Vehicle,
    pub time: f32,
    pub lap: u32,
    pub last_lap_time: f32,
    pub best_lap_time: f32,
    /// Unwrapped distance travelled along the track, signed and continuous
    /// across the start line. Raw arc length jumps between ~length and ~0 at the
    /// seam, which is ambiguous; accumulating deltas removes that ambiguity.
    pub progress: f32,
    lap_start: f32,
    prev_distance: f32,
    accumulator: f32,
}

impl Sim {
    pub fn new(seed: u64) -> Sim {
        let track = Track::generate(seed);
        let player = Vehicle::new(&track, 0.0);
        let prev_distance = player.distance;
        Sim {
            track,
            player,
            time: 0.0,
            lap: 0,
            last_lap_time: 0.0,
            best_lap_time: f32::INFINITY,
            progress: 0.0,
            lap_start: 0.0,
            prev_distance,
            accumulator: 0.0,
        }
    }

    /// Advance by real elapsed time, consuming it in fixed ticks.
    pub fn update(&mut self, controls: &Controls, frame_dt: f32) {
        // Cap the catch-up so a hitch cannot spiral into a long stall.
        self.accumulator = (self.accumulator + frame_dt).min(0.25);
        while self.accumulator >= TICK_DT {
            self.tick(controls, TICK_DT);
            self.accumulator -= TICK_DT;
        }
    }

    pub fn tick(&mut self, controls: &Controls, dt: f32) {
        self.player.step(&self.track, controls, dt);
        self.time += dt;

        // Unwrap the per-tick change in arc length into continuous progress. A
        // step larger than half the track is the seam wrapping, not real travel.
        let d = self.player.distance;
        let len = self.track.length;
        let mut delta = d - self.prev_distance;
        if delta > len * 0.5 {
            delta -= len;
        } else if delta < -len * 0.5 {
            delta += len;
        }
        self.progress += delta;
        self.prev_distance = d;

        let completed = (self.progress / len).floor().max(0.0) as u32;
        if completed > self.lap {
            self.lap = completed;
            self.last_lap_time = self.time - self.lap_start;
            if self.last_lap_time < self.best_lap_time {
                self.best_lap_time = self.last_lap_time;
            }
            self.lap_start = self.time;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vehicle::autopilot;

    /// Pads have to be reachable by a car driving normally, and have to actually
    /// add speed. A pad that is never hit, or hit without effect, is scenery.
    #[test]
    fn boost_pads_fire_and_add_speed() {
        for seed in [1u64, 7, 42] {
            let mut sim = Sim::new(seed);
            assert!(sim.track.boost_pads.len() >= 8, "seed {seed}: too few pads");

            let mut fired = 0usize;
            let mut gained = 0.0f32;
            for _ in 0..(90.0 / TICK_DT) as usize {
                let controls = autopilot(&sim.track, &sim.player, 26.0);
                let before = sim.player.speed();
                sim.tick(&controls, TICK_DT);
                if sim.player.pad_triggered {
                    fired += 1;
                }
                if sim.player.pad_boost > 0.0 {
                    gained += sim.player.speed() - before;
                }
            }
            assert!(fired > 0, "seed {seed}: autopilot never hit a boost pad");
            assert!(gained > 0.0, "seed {seed}: pads fired but added no speed");
        }
    }

    /// Every lap must contain both kinds of stretch, and the open ones must
    /// actually hold the car near the floor. If gravity there still followed the
    /// tube, the car could run the walls straight through them and the section
    /// would be pointless.
    #[test]
    fn open_stretches_pin_the_car_to_the_floor() {
        let track = Track::generate(7);
        let open = track.frames.iter().filter(|f| f.gravity_blend < 0.1).count();
        let tube = track.frames.iter().filter(|f| f.gravity_blend > 0.9).count();
        assert!(open > 20 && tube > 20, "expected both zone types, got {open} open / {tube} tube");

        let mut sim = Sim::new(7);
        let mut samples = 0usize;
        let mut on_floor = 0usize;
        for _ in 0..(120.0 / TICK_DT) as usize {
            let controls = autopilot(&sim.track, &sim.player, 26.0);
            sim.tick(&controls, TICK_DT);

            let surf = sim.track.surface(sim.player.pos, sim.player.hint);
            if surf.gravity.length() > 0.5 && sim.track.frames[surf.index].gravity_blend < 0.1 {
                samples += 1;
                // In an open stretch the outward direction at the car should be
                // world down, i.e. the car is on the bottom of the tube.
                if surf.down.dot(glam::Vec3::NEG_Y) > 0.5 {
                    on_floor += 1;
                }
            }
        }
        assert!(samples > 100, "autopilot never reached an open stretch");
        let ratio = on_floor as f32 / samples as f32;
        assert!(ratio > 0.8, "car was only on the floor {:.0}% of an open stretch", ratio * 100.0);
    }

    /// Drive each seed on autopilot and check the car behaves. This guards the
    /// two failures that have actually happened: a mirrored spawn rotation, and
    /// a righting torque too weak to beat the suspension holding a flipped car
    /// on its wheels.
    #[test]
    fn autopilot_laps_without_getting_stuck_inverted() {
        for seed in [1u64, 7, 42, 1337, 99999] {
            let mut sim = Sim::new(seed);
            // Long enough to cover a standing start plus a full lap.
            let ticks = (90.0 / TICK_DT) as usize;
            let mut inverted = 0usize;
            let mut escaped = 0usize;

            for _ in 0..ticks {
                let controls = autopilot(&sim.track, &sim.player, 26.0);
                sim.tick(&controls, TICK_DT);

                assert!(
                    sim.player.pos.is_finite() && sim.player.speed().is_finite(),
                    "seed {seed}: simulation diverged"
                );
                let surf = sim.track.surface(sim.player.pos, sim.player.hint);
                if surf.gap < -1.0 {
                    escaped += 1;
                }
                if sim.player.up().dot(-surf.down) < 0.0 {
                    inverted += 1;
                }
            }

            assert_eq!(escaped, 0, "seed {seed}: car left the tube");
            assert!(sim.lap >= 1, "seed {seed}: no lap completed in 90s");
            let inverted_pct = inverted as f32 / ticks as f32 * 100.0;
            assert!(
                inverted_pct < 20.0,
                "seed {seed}: inverted {inverted_pct:.1}% of the time, righting is too weak"
            );
        }
    }
}
