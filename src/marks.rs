//! Skid marks: the rubber a sliding tyre leaves behind.
//!
//! Marks are quads laid end to end along each wheel's contact path, in a ring
//! buffer so the oldest is overwritten once the buffer is full. They are drawn
//! with multiply blending rather than alpha: rubber darkens whatever is under
//! it, including the track's own markings, and fading toward white means a
//! mark that has worn away costs nothing and never pops out of existence.

use glam::Vec3;

use crate::particles::ParticleVertex;
use crate::sim::Sim;

/// Quads held at once. Beyond this the oldest is recycled.
pub const MAX_MARKS: usize = 3072;
/// How long a mark takes to wear away.
const LIFETIME: f32 = 9.0;
/// Lift off the surface, clear of z-fighting with the tube.
const LIFT: f32 = 0.06;
/// Half the width of a tyre's contact patch.
const HALF_WIDTH: f32 = 0.26;
/// Rubber comes off past a slip *angle*, not past a slip speed. Eight metres a
/// second sideways at 150 m/s is a three-degree drift that any fast lap
/// contains; the same eight at 20 m/s is a car travelling sideways. Measured
/// over a lap, clean autopilot driving sits at a ratio of 0.008 and a handbrake
/// slide at 0.255, so this sits well clear of ordinary cornering.
const SLIP_RATIO: f32 = 0.18;
/// Below this there is no meaningful contact patch speed and the ratio stops
/// meaning anything, because the denominator has gone to nothing.
const MIN_SLIP: f32 = 2.0;
/// A jump further than this in one step is a respawn, not a skid.
const TELEPORT: f32 = 25.0;

struct Mark {
    corners: [Vec3; 4],
    age: f32,
    /// How much of the underlying colour this mark removes when fresh.
    darkness: f32,
}

/// Where a wheel last laid rubber, and which way it was lying it.
#[derive(Clone, Copy)]
struct Trail {
    pos: Vec3,
    across: Vec3,
}

pub struct Marks {
    marks: Vec<Mark>,
    next: usize,
    trails: Vec<Option<Trail>>,
    vertices: Vec<ParticleVertex>,
}

impl Marks {
    pub fn new() -> Marks {
        Marks {
            marks: Vec::with_capacity(MAX_MARKS),
            next: 0,
            trails: Vec::new(),
            vertices: Vec::with_capacity(MAX_MARKS * 4),
        }
    }

    // Read by the tests, which are where the ring buffer bound is checked.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.marks.len()
    }

    /// Segments get longer with speed. A fixed length would lay a thousand
    /// quads a second at racing pace and exhaust the buffer in a corner.
    fn segment_length(speed: f32) -> f32 {
        0.5 + speed * 0.03
    }

    pub fn update(&mut self, sim: &Sim, dt: f32) {
        for mark in &mut self.marks {
            mark.age += dt;
        }

        let cars = std::iter::once(&sim.player).chain(sim.opponents.iter().map(|o| &o.car));
        for (car_index, car) in cars.enumerate() {
            for (wheel_index, wheel) in car.wheels.iter().enumerate() {
                let slot = car_index * 4 + wheel_index;
                if slot >= self.trails.len() {
                    self.trails.resize(slot + 1, None);
                }

                let ratio = wheel.slip / car.speed().max(1.0);
                let sliding = wheel.contact && wheel.slip > MIN_SLIP && ratio > SLIP_RATIO;
                if !sliding {
                    // Lift the pen. Without this, the next slide would be
                    // joined to the last one by a stripe across everything in
                    // between.
                    self.trails[slot] = None;
                    continue;
                }

                // `world_pos` is the hub, a wheel radius clear of the road.
                // Rubber is left by the contact patch, so drop to the surface
                // and lift back off it by the z-fighting margin only.
                let surface = sim.track.surface(wheel.world_pos, car.hint);
                let up = -surface.down;
                let to = wheel.world_pos + surface.down * surface.gap;

                let Some(trail) = self.trails[slot] else {
                    self.trails[slot] = Some(Trail { pos: to, across: Vec3::ZERO });
                    continue;
                };
                let travel = to - trail.pos;
                let distance = travel.length();
                // A respawn moves a wheel further in one step than any skid
                // could, and joining those two points draws a stripe across
                // the whole track.
                if distance > TELEPORT {
                    self.trails[slot] = Some(Trail { pos: to, across: Vec3::ZERO });
                    continue;
                }
                if distance < Self::segment_length(car.speed()) {
                    continue;
                }

                let across = (travel / distance).cross(up).normalize_or(Vec3::ZERO) * HALF_WIDTH;
                if across == Vec3::ZERO {
                    continue;
                }
                // The first segment of a skid has no previous edge to meet, so
                // it starts square.
                let behind = if trail.across == Vec3::ZERO { across } else { trail.across };
                let lift = up * LIFT;
                self.push(Mark {
                    corners: [
                        trail.pos - behind + lift,
                        trail.pos + behind + lift,
                        to + across + lift,
                        to - across + lift,
                    ],
                    age: 0.0,
                    darkness: ((ratio - SLIP_RATIO) / 0.4).clamp(0.20, 0.80),
                });
                self.trails[slot] = Some(Trail { pos: to, across });
            }
        }
    }

    fn push(&mut self, mark: Mark) {
        if self.marks.len() < MAX_MARKS {
            self.marks.push(mark);
        } else {
            self.marks[self.next] = mark;
            self.next = (self.next + 1) % MAX_MARKS;
        }
    }

    /// Quads for the decal pass. The colour is a multiplier: 1 leaves the track
    /// untouched, 0 paints it black, so worn-out marks cost nothing to draw.
    pub fn build_vertices(&mut self) -> &[ParticleVertex] {
        self.vertices.clear();
        for mark in &self.marks {
            let wear = (mark.age / LIFETIME).clamp(0.0, 1.0);
            let factor = 1.0 - mark.darkness * (1.0 - wear);
            if factor > 0.995 {
                continue;
            }
            let colour = [factor, factor, factor, 1.0];
            for (corner, uv) in mark.corners.iter().zip([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]])
            {
                self.vertices.push(ParticleVertex { pos: corner.to_array(), color: colour, uv });
            }
        }
        &self.vertices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Sim, TICK_DT};
    use crate::vehicle::{autopilot, Controls};

    /// Drive a race and return the marks laid during it.
    ///
    /// Driven alone, weapons off, with the first seconds thrown away. All
    /// three are noise here: a car that has been shot slides because it was
    /// shot, a start-line scramble slides because six cars want the same
    /// corner, and traffic slides you all race long. None of that says
    /// anything about whether a sliding tyre leaves rubber.
    fn race(seed: u64, seconds: f32, mut drive: impl FnMut(&Sim) -> Controls) -> (Sim, Marks) {
        let setup = crate::settings::Race { opponents: 0, weapons: false, ..Default::default() };
        let mut sim = Sim::with_setup(seed, setup);
        let mut marks = Marks::new();
        for _ in 0..(8.0 / TICK_DT) as usize {
            let controls = autopilot(&sim.track, &sim.player, 26.0);
            sim.tick(&controls, TICK_DT);
        }
        for _ in 0..(seconds / TICK_DT) as usize {
            let controls = drive(&sim);
            sim.tick(&controls, TICK_DT);
            marks.update(&sim, TICK_DT);
        }
        (sim, marks)
    }

    /// Rubber comes off when a tyre is sliding and not otherwise. A mark that
    /// appears under normal cornering is just a dirty track.
    #[test]
    fn marks_are_laid_by_sliding_and_not_by_driving() {
        let (_, clean) = race(7, 20.0, |sim| autopilot(&sim.track, &sim.player, 26.0));
        let (_, sliding) = race(7, 20.0, |sim| {
            let mut c = autopilot(&sim.track, &sim.player, 26.0);
            // Full lock and the handbrake: the car spends the run sideways.
            c.steer = 1.0;
            c.handbrake = true;
            c
        });
        assert!(
            sliding.len() > clean.len() * 3,
            "sliding laid {} marks against {} for clean driving, which is not a difference",
            sliding.len(),
            clean.len()
        );
        assert!(sliding.len() > 20, "a twenty second slide laid only {} marks", sliding.len());
    }

    /// Marks lie on the road. Sunk into it they vanish, floating above it they
    /// read as a ribbon hanging in the air.
    #[test]
    fn marks_lie_on_the_track_surface() {
        let (sim, marks) = race(7, 25.0, |sim| {
            let mut c = autopilot(&sim.track, &sim.player, 26.0);
            c.handbrake = true;
            c
        });
        assert!(!marks.marks.is_empty(), "no marks to check");

        let mut hint = 0;
        for mark in &marks.marks {
            for corner in &mark.corners {
                let surface = sim.track.surface(*corner, hint);
                hint = surface.index;
                assert!(
                    surface.gap > 0.0 && surface.gap < 0.5,
                    "a mark sits {:.2} m from the wall, which is not on it",
                    surface.gap
                );
            }
        }
    }

    /// A respawn teleports the car. Joining the wheel's last contact point to
    /// its new one would paint a stripe clean across the track.
    #[test]
    fn respawning_does_not_draw_a_stripe_across_the_track() {
        let mut sim = Sim::new(7);
        let mut marks = Marks::new();
        let mut slide = Controls::default();
        slide.throttle = 1.0;
        slide.steer = 1.0;
        slide.handbrake = true;

        for step in 0..600 {
            sim.tick(&slide, TICK_DT);
            marks.update(&sim, TICK_DT);
            if step == 300 {
                sim.player.respawn(&sim.track);
            }
        }

        let longest = marks
            .marks
            .iter()
            .map(|m| m.corners[0].distance(m.corners[3]))
            .fold(0.0f32, f32::max);
        assert!(
            longest < TELEPORT,
            "a single mark spans {longest:.1} m, so a respawn was joined to the skid before it"
        );
    }

    /// Worn-out marks have to stop drawing, and the buffer has to stay bounded
    /// however long the race runs.
    #[test]
    fn marks_wear_away_and_the_buffer_stays_bounded() {
        let (_, mut marks) = race(7, 90.0, |sim| {
            let mut c = autopilot(&sim.track, &sim.player, 26.0);
            c.handbrake = true;
            c
        });
        assert!(marks.len() <= MAX_MARKS, "{} marks exceeds the cap", marks.len());

        // Nothing is emitted for a mark that has worn out.
        for mark in &mut marks.marks {
            mark.age = LIFETIME * 2.0;
        }
        assert!(
            marks.build_vertices().is_empty(),
            "fully worn marks are still being drawn"
        );
    }

    /// Multiply blending means the colour is a multiplier: a fresh mark must
    /// darken and a worn one must leave the track exactly as it was.
    #[test]
    fn a_mark_darkens_when_fresh_and_does_nothing_when_worn() {
        let mut marks = Marks::new();
        marks.push(Mark { corners: [Vec3::ZERO; 4], age: 0.0, darkness: 0.8 });
        let fresh = marks.build_vertices()[0].color[0];
        assert!(fresh < 0.3, "a fresh mark barely darkens: multiplier {fresh}");

        marks.marks[0].age = LIFETIME * 0.999;
        let worn = marks.build_vertices();
        assert!(
            worn.is_empty() || worn[0].color[0] > 0.99,
            "a worn mark still tints the track"
        );
    }

    /// The whole field leaves rubber, not just the player. A race where only
    /// one car marks the road reads as a bug.
    #[test]
    fn opponents_leave_marks_too() {
        let mut sim = Sim::new(7);
        let mut marks = Marks::new();
        // Park the player and let the field race away from it. Attribution is
        // by position rather than by the trail state, which is cleared the
        // moment a wheel stops sliding and so is almost always empty when
        // looked at.
        for _ in 0..(30.0 / TICK_DT) as usize {
            sim.tick(&Controls::default(), TICK_DT);
            marks.update(&sim, TICK_DT);
        }
        let far_from_the_player = marks
            .marks
            .iter()
            .filter(|m| m.corners[0].distance(sim.player.pos) > 60.0)
            .count();
        assert!(
            far_from_the_player > 0,
            "the field laid no rubber in thirty seconds of racing ({} marks in total, all beside the parked player)",
            marks.len()
        );
    }
}
