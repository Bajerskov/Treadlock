//! Game state and the fixed-timestep update, shared by the rendered and
//! headless paths so physics behaviour cannot diverge between them.

use crate::track::Track;
use crate::vehicle::{self, Controls, Vehicle};

/// Physics runs at a fixed rate independent of frame rate. 120 Hz keeps the
/// stiff suspension stable without costing much on a 6-core Zen 2.
pub const TICK_RATE: f32 = 120.0;
pub const TICK_DT: f32 = 1.0 / TICK_RATE;

/// How many AI cars line up alongside the player.
pub const OPPONENT_COUNT: usize = 5;
/// Cars closer than this push each other apart.
const CONTACT_RADIUS: f32 = 3.4;

pub struct Opponent {
    pub car: Vehicle,
    /// Sideways offset this driver holds from the centre line, in metres, so
    /// the field spreads across the track instead of driving in single file.
    pub lane: f32,
    /// 0..1. Scales how hard they push and how far ahead they look.
    pub skill: f32,
    pub progress: f32,
    pub lap: u32,
    prev_distance: f32,
    pub tint: glam::Vec3,
}

pub struct Sim {
    pub track: Track,
    pub player: Vehicle,
    pub opponents: Vec<Opponent>,
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
        let player = Vehicle::new(&track, 0);
        let prev_distance = player.distance;

        // Spread the field across the track and vary how hard each driver
        // pushes, so they do not move as one block.
        let opponents = (0..OPPONENT_COUNT)
            .map(|i| {
                let car = Vehicle::new(&track, i + 1);
                let spread = (i as f32 / OPPONENT_COUNT.max(1) as f32) * 2.0 - 1.0;
                Opponent {
                    prev_distance: car.distance,
                    car,
                    lane: spread * 7.0,
                    skill: 0.82 + 0.16 * (i as f32 / OPPONENT_COUNT.max(1) as f32),
                    progress: 0.0,
                    lap: 0,
                    tint: opponent_colour(i),
                }
            })
            .collect();

        Sim {
            track,
            player,
            opponents,
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

        for opponent in &mut self.opponents {
            // Lookahead scales with skill and speed: a quick driver reads
            // further ahead and so carries more speed through a bend.
            let lookahead = 18.0 + opponent.skill * 14.0 + opponent.car.speed() * 0.12;
            let mut ai =
                vehicle::autopilot_lane(&self.track, &opponent.car, lookahead, opponent.lane);
            ai.throttle *= opponent.skill;
            ai.boost = ai.boost && opponent.skill > 0.9;
            opponent.car.step(&self.track, &ai, dt);

            let delta = unwrap_delta(
                opponent.prev_distance,
                opponent.car.distance,
                self.track.length,
            );
            opponent.progress += delta;
            opponent.prev_distance = opponent.car.distance;
            opponent.lap = (opponent.progress / self.track.length).floor().max(0.0) as u32;
        }

        self.resolve_car_contacts();
        self.time += dt;

        let d = self.player.distance;
        let len = self.track.length;
        self.progress += unwrap_delta(self.prev_distance, d, len);
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

    /// Opponents have to actually race: complete laps, spread across the track
    /// rather than stacking on one line, and never end up inside each other.
    #[test]
    fn opponents_race_without_overlapping() {
        let mut sim = Sim::new(7);
        assert_eq!(sim.opponents.len(), OPPONENT_COUNT);

        let mut closest = f32::MAX;
        for _ in 0..(120.0 / TICK_DT) as usize {
            let controls = autopilot(&sim.track, &sim.player, 26.0);
            sim.tick(&controls, TICK_DT);

            for (i, a) in sim.opponents.iter().enumerate() {
                assert!(a.car.pos.is_finite(), "opponent {i} diverged");
                for b in sim.opponents.iter().skip(i + 1) {
                    closest = closest.min(a.car.pos.distance(b.car.pos));
                }
                closest = closest.min(a.car.pos.distance(sim.player.pos));
            }
        }

        // Contact resolution is a soft push, so allow a little overlap, but not
        // two cars sharing a space.
        assert!(closest > CONTACT_RADIUS * 0.5, "cars interpenetrated: {closest:.2} m apart");
        let laps: Vec<u32> = sim.opponents.iter().map(|o| o.lap).collect();
        assert!(laps.iter().all(|&l| l >= 1), "opponents did not complete a lap: {laps:?}");
        // Skill varies, so they should not finish in lockstep.
        let spread = sim.opponents.iter().map(|o| o.progress).fold(f32::MIN, f32::max)
            - sim.opponents.iter().map(|o| o.progress).fold(f32::MAX, f32::min);
        assert!(spread > 50.0, "field never spread out: {spread:.0} m between first and last");
    }

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

/// Change in arc length between two ticks, with the start-line seam unwrapped.
/// A step larger than half the track is the seam, not real travel.
fn unwrap_delta(previous: f32, current: f32, length: f32) -> f32 {
    let mut delta = current - previous;
    if delta > length * 0.5 {
        delta -= length;
    } else if delta < -length * 0.5 {
        delta += length;
    }
    delta
}

/// Distinct, saturated liveries, so cars are told apart at a glance and at
/// speed rather than by reading a number off them.
fn opponent_colour(index: usize) -> glam::Vec3 {
    const COLOURS: [glam::Vec3; 6] = [
        glam::Vec3::new(0.15, 0.55, 1.00),
        glam::Vec3::new(1.00, 0.75, 0.10),
        glam::Vec3::new(0.20, 0.85, 0.35),
        glam::Vec3::new(0.85, 0.20, 0.80),
        glam::Vec3::new(1.00, 0.45, 0.05),
        glam::Vec3::new(0.30, 0.90, 0.90),
    ];
    COLOURS[index % COLOURS.len()]
}

impl Sim {
    /// Keep cars from occupying the same space. A full rigid-body response
    /// between vehicles would be a lot of machinery for a racer; pushing them
    /// apart and trading a little momentum gives contact that reads correctly
    /// without letting anyone tunnel through a rival.
    fn resolve_car_contacts(&mut self) {
        let count = self.opponents.len();
        for i in 0..count {
            // Player against each opponent.
            let (player_pos, player_vel) = (self.player.pos, self.player.vel);
            let other = &mut self.opponents[i].car;
            if let Some((push, exchange)) = contact(player_pos, player_vel, other.pos, other.vel) {
                self.player.pos += push;
                self.player.vel += exchange;
                other.pos -= push;
                other.vel -= exchange;
            }

            // Opponents against each other.
            for j in (i + 1)..count {
                let (a_pos, a_vel) = (self.opponents[i].car.pos, self.opponents[i].car.vel);
                let (b_pos, b_vel) = (self.opponents[j].car.pos, self.opponents[j].car.vel);
                if let Some((push, exchange)) = contact(a_pos, a_vel, b_pos, b_vel) {
                    self.opponents[i].car.pos += push;
                    self.opponents[i].car.vel += exchange;
                    self.opponents[j].car.pos -= push;
                    self.opponents[j].car.vel -= exchange;
                }
            }
        }
    }

    /// Where the player sits in the race, 1 for the lead. Ranked on unwrapped
    /// progress, so being a lap up beats being ahead on this lap.
    pub fn player_position(&self) -> usize {
        1 + self
            .opponents
            .iter()
            .filter(|o| o.progress > self.progress)
            .count()
    }
}

/// Separation and velocity exchange for two overlapping cars, or None if they
/// are clear of each other.
fn contact(
    a_pos: glam::Vec3,
    a_vel: glam::Vec3,
    b_pos: glam::Vec3,
    b_vel: glam::Vec3,
) -> Option<(glam::Vec3, glam::Vec3)> {
    let offset = a_pos - b_pos;
    let distance = offset.length();
    if distance >= CONTACT_RADIUS || distance < 1e-4 {
        return None;
    }
    let normal = offset / distance;
    let overlap = CONTACT_RADIUS - distance;

    // Split the correction between them, and only trade the closing part of
    // their velocities so a side-by-side pair does not fling apart.
    let closing = (a_vel - b_vel).dot(normal);
    let exchange = if closing < 0.0 { normal * (-closing * 0.5) } else { glam::Vec3::ZERO };
    Some((normal * (overlap * 0.5), exchange))
}
