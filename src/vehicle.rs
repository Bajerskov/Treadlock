//! Rollcage-style vehicle physics.
//!
//! The defining trick of Rollcage is that being flipped never ends a race. We
//! get most of that for free by taking gravity from the track: inside a tube,
//! down is always the radial direction into the nearest wall, so driving the
//! walls and ceiling needs no special case - the roof still points at the tube
//! axis all the way round. On top of that, a righting torque returns the car to
//! its wheels after a bad landing.

use glam::{Mat3, Quat, Vec3};

use crate::track::{look_rotation, Track};

/// Arcade gravity. Well above 9.81 so jumps land fast and the car feels planted
/// rather than floaty.
const GRAVITY: f32 = 24.0;
const MASS: f32 = 950.0;
/// Chassis half-extents (x = half width, y = half height, z = half length).
/// Public so model fitting can size art against the body that actually collides.
pub const HALF_EXTENTS: Vec3 = Vec3::new(1.15, 0.45, 2.1);
const WHEEL_RADIUS: f32 = 0.62;
const SUSPENSION_REST: f32 = 0.55;
const SUSPENSION_STIFFNESS: f32 = 62000.0;
const SUSPENSION_DAMPING: f32 = 6200.0;
/// Peak tyre friction as a multiple of normal load. Deliberately unrealistic.
const TYRE_FRICTION: f32 = 2.6;
/// Lateral force per unit of sideways slip velocity, before the friction clamp.
const LATERAL_GRIP: f32 = 9000.0;
const ENGINE_FORCE: f32 = 26000.0;
const BOOST_FORCE: f32 = 30000.0;
const BRAKE_FORCE: f32 = 30000.0;
const MAX_STEER: f32 = 0.55;
/// Quadratic drag, tuned against ENGINE_FORCE to set terminal speed.
const DRAG: f32 = 1.6;
const DOWNFORCE: f32 = 1.6;

#[derive(Clone, Copy, Default)]
pub struct Controls {
    pub throttle: f32,
    pub brake: f32,
    /// -1 left .. +1 right.
    pub steer: f32,
    pub boost: bool,
    pub handbrake: bool,
}

#[derive(Clone, Copy)]
pub struct Wheel {
    pub offset: Vec3,
    pub steers: bool,
    pub powered: bool,
    // Updated each step, for rendering and telemetry.
    pub world_pos: Vec3,
    pub contact: bool,
    pub compression: f32,
    /// Wheel angular velocity, rad/s.
    pub spin: f32,
    /// Accumulated rotation, for drawing the wheel turning.
    pub spin_angle: f32,
}

impl Wheel {
    fn new(offset: Vec3, steers: bool, powered: bool) -> Wheel {
        Wheel {
            offset,
            steers,
            powered,
            world_pos: Vec3::ZERO,
            contact: false,
            compression: 0.0,
            spin: 0.0,
            spin_angle: 0.0,
        }
    }
}

pub struct Vehicle {
    pub pos: Vec3,
    pub rot: Quat,
    pub vel: Vec3,
    pub ang_vel: Vec3,
    pub wheels: [Wheel; 4],
    /// Cached nearest-frame index, so track queries stay local.
    pub hint: usize,
    pub boost: f32,
    pub grounded: bool,
    pub contacts: u8,
    /// Arc length along the track, used for lap and placement logic.
    pub distance: f32,
}

impl Vehicle {
    pub fn new(track: &Track, lane_offset: f32) -> Vehicle {
        let (pos, rot) = track.spawn(lane_offset);
        let x = HALF_EXTENTS.x * 0.95;
        let z = HALF_EXTENTS.z * 0.72;
        Vehicle {
            pos,
            rot,
            vel: Vec3::ZERO,
            ang_vel: Vec3::ZERO,
            // Forward is -Z, so the front axle sits at negative z. Wheels sit on
            // the chassis mid-plane rather than slung underneath: they are taller
            // than the body is thick, so a flipped car still has traction and can
            // drive while the righting torque rolls it back onto its wheels.
            wheels: [
                Wheel::new(Vec3::new(-x, 0.0, -z), true, false),
                Wheel::new(Vec3::new(x, 0.0, -z), true, false),
                Wheel::new(Vec3::new(-x, 0.0, z), false, true),
                Wheel::new(Vec3::new(x, 0.0, z), false, true),
            ],
            hint: track.global_nearest_index(pos),
            boost: 1.0,
            grounded: false,
            contacts: 0,
            distance: 0.0,
        }
    }

    pub fn forward(&self) -> Vec3 {
        self.rot * Vec3::NEG_Z
    }
    pub fn up(&self) -> Vec3 {
        self.rot * Vec3::Y
    }
    pub fn right(&self) -> Vec3 {
        self.rot * Vec3::X
    }
    pub fn speed(&self) -> f32 {
        self.vel.length()
    }
    pub fn speed_kph(&self) -> f32 {
        self.speed() * 3.6
    }

    fn inv_inertia_local() -> Vec3 {
        let h = HALF_EXTENTS * 2.0;
        let k = MASS / 12.0;
        Vec3::new(
            1.0 / (k * (h.y * h.y + h.z * h.z)),
            1.0 / (k * (h.x * h.x + h.z * h.z)),
            1.0 / (k * (h.x * h.x + h.y * h.y)),
        )
    }

    /// Inverse inertia tensor rotated into world space: R * I⁻¹ * Rᵀ.
    fn inv_inertia_world(&self) -> Mat3 {
        let r = Mat3::from_quat(self.rot);
        r * Mat3::from_diagonal(Self::inv_inertia_local()) * r.transpose()
    }

    pub fn step(&mut self, track: &Track, controls: &Controls, dt: f32) {
        let body = track.surface(self.pos, self.hint);
        self.hint = body.index;
        self.distance = body.distance;

        let mut force = Vec3::ZERO;
        let mut torque = Vec3::ZERO;

        // Gravity follows the tube, which is what allows wall and ceiling driving.
        force += body.down * (GRAVITY * MASS);

        let speed = self.vel.length();
        if speed > 0.01 {
            force -= self.vel / speed * (DRAG * speed * speed);
        }
        force += body.down * (DOWNFORCE * speed * speed);

        // Steering authority falls off with speed so the car is not twitchy flat out.
        let steer_scale = 1.0 - 0.55 * (speed / 120.0).clamp(0.0, 1.0);
        let steer_angle = controls.steer.clamp(-1.0, 1.0) * MAX_STEER * steer_scale;

        let mut contacts = 0;
        let inv_inertia = self.inv_inertia_world();

        for i in 0..self.wheels.len() {
            let wheel = self.wheels[i];
            let attach = self.pos + self.rot * wheel.offset;
            let surf = track.surface(attach, self.hint);
            let max_reach = SUSPENSION_REST + WHEEL_RADIUS;

            self.wheels[i].world_pos = attach + surf.down * (max_reach - WHEEL_RADIUS).min(surf.gap.max(0.0));

            if surf.gap >= max_reach {
                self.wheels[i].contact = false;
                self.wheels[i].compression = 0.0;
                // Freewheel, decaying slowly so the visual spin does not stop dead.
                self.wheels[i].spin += -self.wheels[i].spin * 0.6 * dt;
                continue;
            }
            contacts += 1;
            let compression = (max_reach - surf.gap).clamp(0.0, max_reach);
            self.wheels[i].contact = true;
            self.wheels[i].compression = compression;

            let arm = attach - self.pos;
            let point_vel = self.vel + self.ang_vel.cross(arm);

            // Suspension pushes away from the wall, i.e. along -down. `closing`
            // is positive while the wheel is travelling into the wall, so the
            // damper adds to the spring on compression and subtracts on rebound.
            let closing = point_vel.dot(surf.down);
            let normal_force =
                (SUSPENSION_STIFFNESS * compression + SUSPENSION_DAMPING * closing).max(0.0);
            let suspension = -surf.down * normal_force;
            force += suspension;
            torque += arm.cross(suspension);

            // Tyre forces act in the contact plane.
            let normal_up = -surf.down;
            let mut fwd = self.forward() - normal_up * self.forward().dot(normal_up);
            if fwd.length_squared() < 1e-6 {
                continue;
            }
            fwd = fwd.normalize();
            if wheel.steers {
                // Steering is about the contact normal, but when the car is
                // inverted that normal is upside down relative to the driver, so
                // the sign flips to keep "left" meaning left on screen.
                let inverted = self.up().dot(normal_up) < 0.0;
                let sign = if inverted { 1.0 } else { -1.0 };
                fwd = Quat::from_axis_angle(normal_up, sign * steer_angle) * fwd;
            }
            let side = normal_up.cross(fwd).normalize();

            let long_vel = point_vel.dot(fwd);
            let lat_vel = point_vel.dot(side);
            self.wheels[i].spin = long_vel / WHEEL_RADIUS;

            // Handbrake breaks the rear end loose - that is the drift button.
            let lat_scale = if controls.handbrake && !wheel.steers { 0.22 } else { 1.0 };
            let mut tyre = -side * (lat_vel * LATERAL_GRIP * lat_scale);

            if wheel.powered {
                let drive = if controls.boost && self.boost > 0.0 { BOOST_FORCE } else { ENGINE_FORCE };
                tyre += fwd * (drive * controls.throttle.clamp(0.0, 1.0) * 0.5);
            }
            let braking = controls.brake.clamp(0.0, 1.0)
                + if controls.handbrake && !wheel.steers { 1.0 } else { 0.0 };
            if braking > 0.0 && long_vel.abs() > 0.1 {
                tyre -= fwd * (long_vel.signum() * BRAKE_FORCE * braking.min(1.0) * 0.25);
            }

            // Friction circle: a tyre cannot exceed its load-limited grip.
            let limit = TYRE_FRICTION * normal_force;
            if tyre.length_squared() > limit * limit && tyre.length_squared() > 1e-6 {
                tyre = tyre.normalize() * limit;
            }
            force += tyre;
            torque += arm.cross(tyre);
        }

        self.grounded = contacts > 0;
        self.contacts = contacts as u8;

        // Settle the chassis wheels-down against the wall. Driving the ceiling of
        // the tube is not the same as being inverted: gravity already follows the
        // tube, so the roof still points at the tube axis all the way round. What
        // this torque provides is the Rollcage guarantee that flipping over never
        // ends a race - the car always rights itself.
        let up = self.up();
        let target_up = -body.down;
        // `cross` alone gives sin(angle), which collapses to zero at both 0 and
        // 180 degrees; scaling a normalised axis by the true angle keeps the
        // restoring torque meaningful right up to fully inverted.
        let cross = up.cross(target_up);
        let axis = cross.normalize_or(self.forward());
        let angle = up.dot(target_up).clamp(-1.0, 1.0).acos();
        // An inverted car rests on its wheels and the suspension resists being
        // rolled back, so a constant torque either fights normal driving or is
        // too weak to recover. Ramp it quadratically instead: barely present
        // while upright, overwhelming once past ninety degrees.
        let inversion = (angle / std::f32::consts::PI).clamp(0.0, 1.0);
        let align_strength = if self.grounded {
            4.0 + 30.0 * inversion * inversion
        } else {
            16.0
        };
        torque += axis * (angle * align_strength * MASS);

        if !self.grounded {
            // Air control: pitch and roll on the stick, so jumps are steerable.
            torque += self.forward() * (-controls.steer * 9.0 * MASS);
            torque += self.right() * ((controls.brake - controls.throttle) * 6.0 * MASS);
        } else {
            // A little direct yaw makes turn-in feel arcade-sharp rather than simulation-heavy.
            // Yaw about the car's own up so turn-in matches the driver's view even
            // during the moment it spends inverted after a bad landing.
            torque += self.up() * (steer_angle * speed.min(90.0) * 90.0);
        }

        // Angular damping, stronger in the air to avoid endless spin.
        torque -= self.ang_vel * (if self.grounded { 2200.0 } else { 4200.0 });

        if controls.boost && self.boost > 0.0 {
            self.boost = (self.boost - dt * 0.28).max(0.0);
        } else {
            self.boost = (self.boost + dt * 0.09).min(1.0);
        }

        // Semi-implicit Euler.
        self.vel += force / MASS * dt;
        self.ang_vel += inv_inertia * torque * dt;
        self.pos += self.vel * dt;
        if self.ang_vel.length_squared() > 1e-12 {
            let axis = self.ang_vel.normalize();
            let angle = self.ang_vel.length() * dt;
            self.rot = (Quat::from_axis_angle(axis, angle) * self.rot).normalize();
        }

        for wheel in &mut self.wheels {
            wheel.spin_angle = (wheel.spin_angle + wheel.spin * dt) % std::f32::consts::TAU;
        }

        self.resolve_wall(track);
    }

    /// Hard backstop: if the chassis has pushed through the tube wall, put it
    /// back on the surface and kill the outward velocity. Without this a big
    /// enough impact can escape the tube entirely.
    fn resolve_wall(&mut self, track: &Track) {
        let surf = track.surface(self.pos, self.hint);
        let limit = HALF_EXTENTS.y + WHEEL_RADIUS * 0.25;
        if surf.gap < limit {
            self.pos += surf.down * (surf.gap - limit);
            let outward = self.vel.dot(surf.down);
            if outward > 0.0 {
                // Scrub along the wall rather than bouncing off it.
                self.vel -= surf.down * outward * 1.15;
                self.vel *= 0.985;
            }
        }
    }

    /// Put the car back on the floor facing down-track, keeping its progress.
    pub fn respawn(&mut self, track: &Track) {
        let i = track.global_nearest_index(self.pos);
        let f = &track.frames[i];
        let down = {
            let d = Vec3::NEG_Y - f.tangent * Vec3::NEG_Y.dot(f.tangent);
            d.normalize_or(f.normal)
        };
        self.pos = f.pos + down * (f.radius - 1.6);
        self.rot = look_rotation(f.tangent, -down);
        self.vel = f.tangent * self.vel.length().min(40.0);
        self.ang_vel = Vec3::ZERO;
        self.hint = i;
    }
}

/// Steer toward a point further along the centerline. Used by the headless
/// drivability check and as the basis for opponent AI.
pub fn autopilot(track: &Track, v: &Vehicle, lookahead: f32) -> Controls {
    let surf = track.surface(v.pos, v.hint);
    let n = track.frames.len();
    let steps = (lookahead / 6.0).max(1.0) as usize;
    let target_frame = &track.frames[(surf.index + steps) % n];
    // Aim at the floor of the tube ahead, not its centerline.
    let ahead_down = {
        let d = Vec3::NEG_Y - target_frame.tangent * Vec3::NEG_Y.dot(target_frame.tangent);
        d.normalize_or(target_frame.normal)
    };
    let target = target_frame.pos + ahead_down * (target_frame.radius - 1.6);

    let to_target = target - v.pos;
    let up = -surf.down;
    let flat = (to_target - up * to_target.dot(up)).normalize_or(v.forward());
    // Steering is expressed in the car's own frame, so it stays correct whether
    // the car is on its wheels or its roof.
    let mut steer = flat.dot(v.right()).clamp(-1.0, 1.0) * 2.0;
    let facing = flat.dot(v.forward());
    if facing < 0.0 {
        // Pointing back down the track: commit to a full-lock turn rather than
        // creeping, since a target directly behind gives almost no steer signal.
        steer = if steer >= 0.0 { 1.0 } else { -1.0 };
    }

    Controls {
        throttle: 1.0,
        brake: 0.0,
        steer,
        boost: facing > 0.5 && v.speed() < 90.0,
        handbrake: false,
    }
}
