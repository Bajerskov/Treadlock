//! Chase camera.
//!
//! The camera's up vector is taken from the car, not the world. That is what
//! sells wall and ceiling driving: the world rolls around the car instead of
//! the car appearing to climb a wall.

use glam::{Mat4, Vec3};

use crate::track::Track;
use crate::vehicle::Vehicle;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Follows the car from behind. The racing view.
    Chase,
    /// Orbits the car in its own frame, so the car holds still on screen while
    /// the world turns around it. That is what makes it an inspection view
    /// rather than just a detached camera: you are looking at the model, not
    /// at where it happens to be.
    Orbit,
}

pub struct Camera {
    pub pos: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub fov: f32,
    /// The player's field of view at a standstill, in radians. Speed widens the
    /// lens from here; the setting moves where "here" is.
    pub base_fov: f32,
    pub mode: Mode,
    orbit_yaw: f32,
    orbit_pitch: f32,
    orbit_distance: f32,
}

impl Camera {
    pub fn new(v: &Vehicle) -> Camera {
        Camera {
            pos: v.pos - v.forward() * 12.0 + v.up() * 4.0,
            target: v.pos,
            up: v.up(),
            fov: 70f32.to_radians(),
            base_fov: 68f32.to_radians(),
            mode: Mode::Chase,
            // Three-quarter view from behind and slightly above, which shows
            // the most of a car in one look.
            orbit_yaw: 0.7,
            orbit_pitch: 0.30,
            orbit_distance: 9.0,
        }
    }

    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            Mode::Chase => Mode::Orbit,
            Mode::Orbit => Mode::Chase,
        };
    }

    /// Drag deltas in pixels.
    pub fn rotate(&mut self, dx: f32, dy: f32) {
        self.orbit_yaw += dx * 0.008;
        // Stop just short of the poles, where the up vector degenerates and the
        // view snaps round.
        self.orbit_pitch = (self.orbit_pitch + dy * 0.008).clamp(-1.45, 1.45);
    }

    /// Positive zooms in. Scaled by the current distance so it feels even at
    /// both ends rather than crawling far out and lurching up close.
    pub fn zoom(&mut self, amount: f32) {
        self.orbit_distance = (self.orbit_distance * (1.0 - amount * 0.12)).clamp(2.5, 90.0);
    }

    pub fn update(&mut self, v: &Vehicle, track: &Track, dt: f32) {
        match self.mode {
            Mode::Chase => self.follow(v, track, dt),
            Mode::Orbit => self.orbit(v, dt),
        }
    }

    fn orbit(&mut self, v: &Vehicle, dt: f32) {
        // The offset is built in the car's frame, so turning the car turns the
        // world rather than sliding the car across the screen.
        let (sy, cy) = self.orbit_yaw.sin_cos();
        let (sp, cp) = self.orbit_pitch.sin_cos();
        let local = Vec3::new(sy * cp, sp, cy * cp);
        let desired = v.pos + v.rot * local * self.orbit_distance;

        // Light smoothing only: an inspection camera should answer the mouse,
        // not glide after it.
        let blend = 1.0 - (-dt * 22.0).exp();
        self.pos = self.pos.lerp(desired, blend);
        self.target = self.target.lerp(v.pos, blend);
        self.up = self.up.lerp(v.up(), blend).normalize_or(v.up());

        // Back to a neutral lens: the speed-widened field of view is a driving
        // effect and would distort what is being inspected.
        let target_fov = 60f32.to_radians();
        self.fov += (target_fov - self.fov) * blend;
    }

    pub fn follow(&mut self, v: &Vehicle, track: &Track, dt: f32) {
        let surf = track.surface(v.pos, v.hint);
        // Toward the tube axis. The camera is anchored to this rather than to the
        // car's own up: the car is symmetric and may be driving inverted, and
        // offsetting along its roof would put the camera through the wall.
        let interior = -surf.down;

        let speed_factor = (v.speed() / 120.0).clamp(0.0, 1.0);
        let distance = 11.0 + 3.5 * speed_factor;
        let height = 3.6;

        // Flatten the car's heading against the surface so the camera does not
        // dive or climb every time the chassis pitches.
        let fwd = (v.forward() - interior * v.forward().dot(interior))
            .normalize_or(v.forward());

        let desired = v.pos - fwd * distance + interior * height;
        let look_at = v.pos + fwd * 8.0;

        // Frame-rate independent exponential smoothing. Position lags more than
        // aim, so the car leads the frame under acceleration.
        let pos_blend = 1.0 - (-dt * 9.0).exp();
        let aim_blend = 1.0 - (-dt * 14.0).exp();
        let up_blend = 1.0 - (-dt * 7.0).exp();

        self.pos = self.pos.lerp(desired, pos_blend);
        self.target = self.target.lerp(look_at, aim_blend);
        self.up = self.up.lerp(interior, up_blend).normalize_or(interior);

        // Backstop: smoothing lags behind teleports, so a respawn can sling the
        // camera through the wall before it catches up. Keep it inside the tube.
        let camera_surf = track.surface(self.pos, surf.index);
        const MARGIN: f32 = 1.5;
        if camera_surf.gap < MARGIN {
            self.pos -= camera_surf.down * (MARGIN - camera_surf.gap);
        }

        // Widening the field of view with speed reads as acceleration even when
        // the number on the dial is not visible.
        let target_fov = self.base_fov + 22f32.to_radians() * speed_factor;
        self.fov += (target_fov - self.fov) * (1.0 - (-dt * 5.0).exp());
    }

    /// Jump straight to the follow pose, for respawns and race starts where
    /// smoothing across the gap would sweep the camera through geometry.
    pub fn snap(&mut self, v: &Vehicle, track: &Track) {
        self.follow(v, track, 1.0);
    }

    /// Screen-aligned right and up, for building billboards on the CPU.
    pub fn basis(&self) -> (Vec3, Vec3) {
        let forward = (self.target - self.pos).normalize_or(Vec3::NEG_Z);
        let right = forward.cross(self.up).normalize_or(Vec3::X);
        (right, right.cross(forward))
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at_rh(self.pos, self.target, self.up);
        // In this pipeline clip y = +1 is the top of the window, which the HUD
        // pins independently: ui::pixel_to_ndc sends pixel row 0 there and it
        // lands at the top. glam's perspective already sends view-space up to
        // +y, so it needs no correction. Negating it here, as the usual Vulkan
        // advice suggests, rendered the entire world upside down - which is
        // easy to miss, because a mirrored tube still looks like a tube.
        Mat4::perspective_rh(self.fov, aspect, 0.25, 4000.0) * view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Sim, TICK_DT};
    use crate::vehicle::autopilot;

    /// Something physically above the camera must appear above the middle of
    /// the screen. Getting this backwards flips the world without making it
    /// look broken - a mirrored tube is still a tube - and shows up instead as
    /// a car that seems to drive on the ceiling and never falls.
    ///
    /// The reference is the HUD: `ui::pixel_to_ndc` puts pixel row 0 at clip
    /// y = +1, and that is confirmed to render at the top of the window. So
    /// clip y = +1 is up here too.
    #[test]
    fn world_up_projects_to_the_top_of_the_screen() {
        let mut camera = Camera::new(&Sim::new(7).player);
        camera.pos = Vec3::ZERO;
        camera.target = Vec3::NEG_Z * 10.0;
        camera.up = Vec3::Y;
        let view_proj = camera.view_proj(16.0 / 9.0);

        let project = |p: Vec3| {
            let clip = view_proj * p.extend(1.0);
            clip.y / clip.w
        };
        let above = project(Vec3::new(0.0, 3.0, -20.0));
        let below = project(Vec3::new(0.0, -3.0, -20.0));

        assert!(above > 0.0, "a point above the camera projected below centre");
        assert!(below < 0.0, "a point below the camera projected above centre");
        assert!(above > below, "vertical axis is inverted");

        // And the same for the tube interior direction the camera rides on: the
        // surface under the car must land below the middle of the view.
        let mut sim = Sim::new(7);
        sim.tick(&crate::vehicle::Controls::default(), TICK_DT);
        let mut chase = Camera::new(&sim.player);
        chase.snap(&sim.player, &sim.track);
        let surf = sim.track.surface(sim.player.pos, sim.player.hint);

        let vp = chase.view_proj(16.0 / 9.0);
        let ground = sim.player.pos + surf.down * 4.0;
        let sky = sim.player.pos - surf.down * 4.0;
        let clip_y = |p: Vec3| {
            let c = vp * p.extend(1.0);
            c.y / c.w
        };
        assert!(
            clip_y(ground) < clip_y(sky),
            "the track surface projected above the tube interior, so the world is upside down"
        );
    }

    /// Orbiting has to keep the car framed while moving the viewpoint around
    /// it, and zoom has to actually change the distance. Otherwise it is not an
    /// inspection camera, just a differently broken chase camera.
    #[test]
    fn orbit_circles_the_car_and_zooms() {
        let mut sim = Sim::new(7);
        sim.tick(&crate::vehicle::Controls::default(), TICK_DT);

        let mut camera = Camera::new(&sim.player);
        camera.toggle_mode();
        assert!(camera.mode == Mode::Orbit);

        // Settle, then record where it sits.
        for _ in 0..200 {
            camera.update(&sim.player, &sim.track, TICK_DT);
        }
        let start = camera.pos;
        let start_distance = start.distance(sim.player.pos);

        // A quarter turn should move the camera without changing its range.
        camera.rotate(std::f32::consts::FRAC_PI_2 / 0.008 * 0.25, 0.0);
        for _ in 0..200 {
            camera.update(&sim.player, &sim.track, TICK_DT);
        }
        let turned = camera.pos;
        assert!(
            turned.distance(start) > 1.0,
            "dragging did not move the camera"
        );
        assert!(
            (turned.distance(sim.player.pos) - start_distance).abs() < 0.5,
            "orbiting changed the range instead of the angle"
        );
        // The car stays centred throughout, which is the point of the mode.
        assert!(camera.target.distance(sim.player.pos) < 0.5);

        camera.zoom(4.0);
        for _ in 0..200 {
            camera.update(&sim.player, &sim.track, TICK_DT);
        }
        assert!(
            camera.pos.distance(sim.player.pos) < start_distance - 1.0,
            "zooming in did not get closer"
        );
    }

    /// The camera is anchored to the tube interior, so it must never end up
    /// outside the wall, which renders the level inside out.
    #[test]
    fn camera_stays_inside_the_tube() {
        for seed in [1u64, 7, 42] {
            let mut sim = Sim::new(seed);
            let mut camera = Camera::new(&sim.player);
            let mut worst = f32::MAX;
            let mut worst_up = 1.0f32;

            for _ in 0..(45.0 / TICK_DT) as usize {
                let controls = autopilot(&sim.track, &sim.player, 26.0);
                sim.tick(&controls, TICK_DT);
                camera.follow(&sim.player, &sim.track, TICK_DT);

                let surf = sim.track.surface(camera.pos, sim.player.hint);
                worst = worst.min(surf.gap);
                // The camera's up should agree with the tube interior, or the
                // horizon rolls over and the world looks upside down.
                let car = sim.track.surface(sim.player.pos, sim.player.hint);
                worst_up = worst_up.min(camera.up.dot(-car.down));
            }
            assert!(worst > 0.0, "seed {seed}: camera left the tube, worst gap {worst:.2} m");
            assert!(
                worst_up > 0.0,
                "seed {seed}: camera up inverted against the tube, worst {worst_up:.2}"
            );
        }
    }
}
