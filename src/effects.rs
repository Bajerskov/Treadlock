//! Visual effects driven by simulation state.
//!
//! Emission lives here rather than in the sim so that physics stays
//! deterministic and headless runs do nothing extra. Everything reads car and
//! track state; nothing writes back.

use glam::Vec3;

use crate::particles::{slot, Emit, Particles};
use crate::sim::Sim;

/// Feed the particle system from this frame's simulation state.
pub fn update(
    particles: &mut Particles,
    sim: &Sim,
    dt: f32,
    throttle: f32,
    brake: f32,
    boosting: bool,
) {
    let car = &sim.player;
    let speed = car.speed();
    let back = car.forward() * -1.0;
    let exhaust = car.pos + back * 2.0;

    // Jet exhaust. Rate and heat both rise with throttle, so the plume reads as
    // engine load rather than just speed.
    let pad = car.pad_boost > 0.0;
    let heat = if pad { 1.0 } else if boosting { 0.8 } else { throttle * 0.45 };
    if heat > 0.02 {
        let drift = car.vel * 0.25;
        particles.emit_rate(slot::JET, 40.0 + 160.0 * heat, dt, |rng| {
            let spread = rng.direction() * rng.range(0.0, 0.5);
            Emit {
                pos: exhaust + spread,
                // Thrown backwards out of the car, then carried by its wake.
                vel: drift + back * rng.range(6.0, 14.0) * (0.5 + heat) + spread * 4.0,
                life: rng.range(0.18, 0.42) * (0.6 + heat),
                size: rng.range(0.5, 1.1) * (0.7 + heat),
                end_size: 0.05,
                // Hot core fading to smoke, hotter still on a pad boost.
                color: if pad {
                    Vec3::new(1.4, 1.0, 2.2)
                } else {
                    Vec3::new(1.5, 0.75, 0.25) * (0.6 + heat)
                },
                end_color: Vec3::new(0.15, 0.10, 0.14),
                drag: 3.0,
                gravity: Vec3::ZERO,
            }
        });
    }

    // Tyre smoke and sparks where a wheel is sliding. Slip is the honest
    // trigger: it fires on a drift and on a hard landing, and stays quiet when
    // the car is simply fast in a straight line.
    for wheel in &car.wheels {
        if !wheel.contact {
            continue;
        }
        let slide = (wheel.slip - 6.0).max(0.0);
        if slide <= 0.0 {
            continue;
        }
        let at = wheel.world_pos;
        let along = car.vel * 0.15;
        particles.emit_rate(slot::SPARKS, (slide * 8.0).min(120.0), dt, |rng| {
            let spark = rng.unit_chance(0.35);
            Emit {
                pos: at + rng.direction() * 0.25,
                vel: along + rng.direction() * rng.range(1.0, 5.0),
                life: if spark { rng.range(0.15, 0.35) } else { rng.range(0.4, 0.9) },
                size: if spark { 0.12 } else { rng.range(0.4, 0.9) },
                end_size: if spark { 0.02 } else { 1.6 },
                color: if spark {
                    Vec3::new(2.0, 1.2, 0.4)
                } else {
                    Vec3::new(0.5, 0.52, 0.58)
                },
                end_color: Vec3::new(0.08, 0.08, 0.10),
                drag: if spark { 1.2 } else { 3.5 },
                gravity: Vec3::NEG_Y * if spark { 14.0 } else { 0.0 },
            }
        });
    }

    // Braking. Two separate things, because they say different things: dust
    // thrown forward off the contact patch, which reads as the car shedding
    // speed, and the discs glowing, which reads as where that speed is going.
    // Both are gated on actually moving, so holding the brake at a standstill
    // does nothing.
    if brake > 0.2 && speed > 8.0 {
        let effort = brake * (speed / 80.0).clamp(0.25, 1.0);
        let heading = car.vel.normalize_or_zero();
        for wheel in &car.wheels {
            if !wheel.contact {
                continue;
            }
            let at = wheel.world_pos;
            let surface_up = -sim.track.surface(at, car.hint).down;
            particles.emit_rate(slot::BRAKE, 55.0 * effort, dt, |rng| Emit {
                // Thrown forward of the wheel, where the road is arriving from.
                pos: at + heading * rng.range(0.0, 1.2) + rng.direction() * 0.3,
                vel: heading * rng.range(2.0, 7.0) * effort
                    + surface_up * rng.range(0.5, 2.5)
                    + rng.direction() * 1.5,
                life: rng.range(0.25, 0.7),
                size: rng.range(0.25, 0.6),
                end_size: 1.8,
                color: Vec3::new(0.55, 0.53, 0.50) * (0.6 + 0.6 * effort),
                end_color: Vec3::new(0.10, 0.09, 0.09),
                drag: 4.0,
                gravity: Vec3::ZERO,
            });

            // The disc itself: a small hot point at the hub, not a plume.
            particles.emit_rate(slot::BRAKE_GLOW, 26.0 * effort, dt, |rng| Emit {
                pos: at + rng.direction() * 0.18,
                vel: car.vel * 0.96,
                life: rng.range(0.05, 0.12),
                size: rng.range(0.18, 0.30),
                end_size: 0.05,
                color: Vec3::new(2.2, 0.55, 0.12) * effort,
                end_color: Vec3::new(0.5, 0.08, 0.02),
                drag: 0.0,
                gravity: Vec3::ZERO,
            });
        }
    }

    // A pad firing gets one loud burst, so the moment of collection is legible
    // even at 400 km/h.
    if car.pad_triggered {
        let origin = car.pos;
        let up = -sim.track.surface(car.pos, car.hint).down;
        let forward = car.forward();
        particles.burst(70, |rng| Emit {
            pos: origin + rng.direction() * 1.5,
            vel: up * rng.range(4.0, 16.0) + forward * rng.range(0.0, 22.0) + rng.direction() * 5.0,
            life: rng.range(0.35, 0.85),
            size: rng.range(0.6, 1.4),
            end_size: 0.05,
            color: Vec3::new(0.5, 1.6, 2.4),
            end_color: Vec3::new(0.05, 0.2, 0.5),
            drag: 2.2,
            gravity: Vec3::ZERO,
        });
    }

    // Boost pads idle with a slow updraft, so they are visible before you reach
    // them rather than only once collected. Only nearby ones, to stay cheap.
    let near: Vec<_> = sim
        .track
        .boost_pads
        .iter()
        .filter(|p| p.pos.distance_squared(car.pos) < 260.0 * 260.0)
        .copied()
        .collect();
    if !near.is_empty() {
        let mut which = 0usize;
        particles.emit_rate(slot::PAD, 18.0 * near.len() as f32, dt, |rng| {
            let pad = near[which % near.len()];
            which += 1;
            Emit {
                pos: pad.pos + pad.up * 0.2 + rng.direction() * 2.4,
                vel: pad.up * rng.range(1.5, 4.5),
                life: rng.range(0.5, 1.1),
                size: rng.range(0.2, 0.5),
                end_size: 0.02,
                color: Vec3::new(0.3, 1.1, 1.8),
                end_color: Vec3::new(0.02, 0.15, 0.4),
                drag: 1.0,
                gravity: Vec3::ZERO,
            }
        });
    }

    // Speed streaks alongside the car once it is genuinely quick. They sit in
    // the periphery, which is where speed is actually perceived.
    if speed > 60.0 {
        let at = car.pos;
        let vel = car.vel;
        let right = car.right();
        let up = car.up();
        particles.emit_rate(slot::BOOST, (speed - 60.0) * 1.6, dt, |rng| {
            let side = if rng.unit_chance(0.5) { 1.0 } else { -1.0 };
            Emit {
                pos: at + right * side * rng.range(2.5, 6.0) + up * rng.range(-1.0, 2.5)
                    + vel.normalize_or_zero() * rng.range(4.0, 16.0),
                vel: -vel * 0.35,
                life: rng.range(0.1, 0.22),
                size: rng.range(0.10, 0.22),
                end_size: 0.02,
                color: Vec3::new(0.7, 0.85, 1.2),
                end_color: Vec3::new(0.1, 0.2, 0.4),
                drag: 0.5,
                gravity: Vec3::ZERO,
            }
        });
    }

    particles.update(dt);
}
