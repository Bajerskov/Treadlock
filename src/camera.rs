//! Chase camera.
//!
//! The camera's up vector is taken from the car, not the world. That is what
//! sells wall and ceiling driving: the world rolls around the car instead of
//! the car appearing to climb a wall.

use glam::{Mat4, Vec3};

use crate::vehicle::Vehicle;

pub struct Camera {
    pub pos: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub fov: f32,
}

impl Camera {
    pub fn new(v: &Vehicle) -> Camera {
        Camera {
            pos: v.pos - v.forward() * 12.0 + v.up() * 4.0,
            target: v.pos,
            up: v.up(),
            fov: 70f32.to_radians(),
        }
    }

    pub fn follow(&mut self, v: &Vehicle, dt: f32) {
        let speed_factor = (v.speed() / 120.0).clamp(0.0, 1.0);
        let distance = 11.0 + 3.5 * speed_factor;
        let height = 3.6;

        let desired = v.pos - v.forward() * distance + v.up() * height;
        let look_at = v.pos + v.forward() * 8.0;

        // Frame-rate independent exponential smoothing. Position lags more than
        // aim, so the car leads the frame under acceleration.
        let pos_blend = 1.0 - (-dt * 9.0).exp();
        let aim_blend = 1.0 - (-dt * 14.0).exp();
        let up_blend = 1.0 - (-dt * 7.0).exp();

        self.pos = self.pos.lerp(desired, pos_blend);
        self.target = self.target.lerp(look_at, aim_blend);
        self.up = self.up.lerp(v.up(), up_blend).normalize_or(v.up());

        // Widening the field of view with speed reads as acceleration even when
        // the number on the dial is not visible.
        let target_fov = (68.0 + 22.0 * speed_factor).to_radians();
        self.fov += (target_fov - self.fov) * (1.0 - (-dt * 5.0).exp());
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at_rh(self.pos, self.target, self.up);
        let mut proj = Mat4::perspective_rh(self.fov, aspect, 0.25, 4000.0);
        // Vulkan's clip space has +Y pointing down, unlike OpenGL's.
        proj.y_axis.y *= -1.0;
        proj * view
    }
}
