//! Particle system.
//!
//! Particles are simulated on the CPU and drawn as camera-facing billboards
//! built fresh each frame. At these counts that costs far less than it would to
//! manage GPU-side simulation buffers, and it keeps the effect entirely in the
//! render layer: nothing here feeds back into physics, so a headless run is
//! unaffected.

use glam::{Vec3, Vec4};

/// Hard cap on live particles. Oldest are recycled once it is reached, so a
/// long boost cannot grow the buffer or drop the frame rate.
pub const MAX_PARTICLES: usize = 3072;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
    pub uv: [f32; 2],
}

/// What to spawn. Grouped rather than passed as a dozen arguments, since every
/// emitter sets most of them.
#[derive(Clone, Copy)]
pub struct Emit {
    pub pos: Vec3,
    pub vel: Vec3,
    pub life: f32,
    pub size: f32,
    pub end_size: f32,
    pub color: Vec3,
    pub end_color: Vec3,
    /// Fraction of velocity shed per second.
    pub drag: f32,
    pub gravity: Vec3,
}

impl Default for Emit {
    fn default() -> Emit {
        Emit {
            pos: Vec3::ZERO,
            vel: Vec3::ZERO,
            life: 0.6,
            size: 0.5,
            end_size: 0.1,
            color: Vec3::ONE,
            end_color: Vec3::ZERO,
            drag: 2.0,
            gravity: Vec3::ZERO,
        }
    }
}

#[derive(Clone, Copy)]
struct Particle {
    pos: Vec3,
    vel: Vec3,
    life: f32,
    max_life: f32,
    size: f32,
    end_size: f32,
    color: Vec3,
    end_color: Vec3,
    drag: f32,
    gravity: Vec3,
}

/// xorshift, so jitter is cheap and needs no dependency.
pub struct Rng(u32);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
    fn in_sphere(&mut self) -> Vec3 {
        Vec3::new(self.signed(), self.signed(), self.signed()).normalize_or_zero()
    }
}

pub struct Particles {
    live: Vec<Particle>,
    vertices: Vec<ParticleVertex>,
    rng: Rng,
    /// Carries fractional emission between frames, so a rate of 90/s does not
    /// round to nothing at high frame rates.
    accumulators: [f32; EMITTER_SLOTS],
}

/// Independent rate-based emitters need their own remainder, or they steal
/// fractions from each other and stutter.
pub const EMITTER_SLOTS: usize = 8;

impl Particles {
    pub fn new() -> Particles {
        Particles {
            live: Vec::with_capacity(MAX_PARTICLES),
            vertices: Vec::with_capacity(MAX_PARTICLES * 4),
            rng: Rng(0x9E3779B9),
            accumulators: [0.0; EMITTER_SLOTS],
        }
    }


    pub fn spawn(&mut self, emit: Emit) {
        let particle = Particle {
            pos: emit.pos,
            vel: emit.vel,
            life: emit.life,
            max_life: emit.life.max(1e-3),
            size: emit.size,
            end_size: emit.end_size,
            color: emit.color,
            end_color: emit.end_color,
            drag: emit.drag,
            gravity: emit.gravity,
        };
        if self.live.len() < MAX_PARTICLES {
            self.live.push(particle);
        } else {
            // Recycle the particle closest to death rather than refusing to
            // spawn, so a full pool degrades smoothly instead of freezing.
            let mut oldest = 0;
            for (i, p) in self.live.iter().enumerate() {
                if p.life < self.live[oldest].life {
                    oldest = i;
                }
            }
            self.live[oldest] = particle;
        }
    }

    /// Spawn at `rate` per second, carrying the remainder between frames.
    /// `slot` keeps each emitter's remainder separate.
    pub fn emit_rate(&mut self, slot: usize, rate: f32, dt: f32, mut make: impl FnMut(&mut Rng) -> Emit) {
        let slot = slot % EMITTER_SLOTS;
        self.accumulators[slot] += rate * dt;
        // Cap the burst a single long frame can produce.
        let count = (self.accumulators[slot] as usize).min(64);
        self.accumulators[slot] -= count as f32;
        for _ in 0..count {
            let emit = make(&mut self.rng);
            self.spawn(emit);
        }
    }

    pub fn burst(&mut self, count: usize, mut make: impl FnMut(&mut Rng) -> Emit) {
        for _ in 0..count {
            let emit = make(&mut self.rng);
            self.spawn(emit);
        }
    }

    pub fn update(&mut self, dt: f32) {
        let mut i = 0;
        while i < self.live.len() {
            let p = &mut self.live[i];
            p.life -= dt;
            if p.life <= 0.0 {
                self.live.swap_remove(i);
                continue;
            }
            p.vel += p.gravity * dt;
            // Exponential decay, so drag is stable at any timestep.
            p.vel *= (-p.drag * dt).exp();
            let vel = p.vel;
            p.pos += vel * dt;
            i += 1;
        }
    }

    /// Build camera-facing quads. `right` and `up` come from the camera basis,
    /// so the billboards always face the viewer without a geometry shader.
    pub fn build_vertices(&mut self, right: Vec3, up: Vec3) -> &[ParticleVertex] {
        self.vertices.clear();
        for p in &self.live {
            let t = (p.life / p.max_life).clamp(0.0, 1.0);
            let size = p.end_size + (p.size - p.end_size) * t;
            let rgb = p.end_color + (p.color - p.end_color) * t;
            // Additive blending, so alpha is the fade and needs no sorting.
            let color = Vec4::new(rgb.x, rgb.y, rgb.z, t * t);

            let r = right * size;
            let u = up * size;
            let corners = [
                (p.pos - r - u, [0.0f32, 0.0f32]),
                (p.pos + r - u, [1.0, 0.0]),
                (p.pos + r + u, [1.0, 1.0]),
                (p.pos - r + u, [0.0, 1.0]),
            ];
            for (pos, uv) in corners {
                self.vertices.push(ParticleVertex {
                    pos: pos.to_array(),
                    color: color.to_array(),
                    uv,
                });
            }
        }
        &self.vertices
    }

    /// Quad indices for the whole pool, built once and reused.
    pub fn indices() -> Vec<u32> {
        let mut indices = Vec::with_capacity(MAX_PARTICLES * 6);
        for quad in 0..MAX_PARTICLES as u32 {
            let base = quad * 4;
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        indices
    }
}

/// Emitter slots, named so two effects cannot silently share a remainder.
pub mod slot {
    pub const JET: usize = 0;
    pub const BOOST: usize = 1;
    pub const SPARKS: usize = 2;
    pub const PAD: usize = 3;
}

impl Rng {
    pub fn direction(&mut self) -> Vec3 {
        self.in_sphere()
    }
    /// True with the given probability.
    pub fn unit_chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}
