//! Procedural meshes for the car. Everything is generated at runtime, which
//! suits a board whose M.2 slot is only PCIe 2.0 x2 - there is no asset
//! streaming to be bottlenecked by.

use glam::Vec3;

use crate::track::Vertex;

pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl Mesh {
    fn push_quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3, uv_scale: f32) {
        let normal = (b - a).cross(d - a).normalize_or(Vec3::Y);
        let base = self.vertices.len() as u32;
        for (p, uv) in [(a, [0.0, 0.0]), (b, [uv_scale, 0.0]), (c, [uv_scale, uv_scale]), (d, [0.0, uv_scale])] {
            self.vertices.push(Vertex { pos: p.to_array(), normal: normal.to_array(), uv, gravity_blend: 0.0 });
        }
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// A box, optionally tapered toward the top so the chassis reads as a vehicle
/// rather than a crate. The car is symmetric top-to-bottom because it has to
/// look right while driving inverted.
pub fn chassis(half: Vec3, taper: f32) -> Mesh {
    let mut m = Mesh { vertices: Vec::new(), indices: Vec::new() };
    let (x, y, z) = (half.x, half.y, half.z);
    let tx = x * taper;
    let tz = z * taper;

    // Top and bottom faces are inset by `taper`, sides are trapezoids.
    let top = [
        Vec3::new(-tx, y, -tz),
        Vec3::new(tx, y, -tz),
        Vec3::new(tx, y, tz),
        Vec3::new(-tx, y, tz),
    ];
    let bottom = [
        Vec3::new(-tx, -y, -tz),
        Vec3::new(-tx, -y, tz),
        Vec3::new(tx, -y, tz),
        Vec3::new(tx, -y, -tz),
    ];
    let belt = [
        Vec3::new(-x, 0.0, -z),
        Vec3::new(x, 0.0, -z),
        Vec3::new(x, 0.0, z),
        Vec3::new(-x, 0.0, z),
    ];

    m.push_quad(top[0], top[1], top[2], top[3], 1.0);
    m.push_quad(bottom[0], bottom[1], bottom[2], bottom[3], 1.0);
    for i in 0..4 {
        let j = (i + 1) % 4;
        m.push_quad(belt[i], belt[j], top[j], top[i], 1.0);
        m.push_quad(belt[j], belt[i], bottom[3 - i], bottom[3 - j], 1.0);
    }
    m
}

/// A wheel: a cylinder whose axle runs along local X, with flat caps.
pub fn wheel(radius: f32, half_width: f32, segments: usize) -> Mesh {
    let mut m = Mesh { vertices: Vec::new(), indices: Vec::new() };

    for s in 0..segments {
        let a0 = s as f32 / segments as f32 * std::f32::consts::TAU;
        let a1 = (s + 1) as f32 / segments as f32 * std::f32::consts::TAU;
        let (y0, z0) = (a0.cos() * radius, a0.sin() * radius);
        let (y1, z1) = (a1.cos() * radius, a1.sin() * radius);

        // Tread band.
        m.push_quad(
            Vec3::new(-half_width, y0, z0),
            Vec3::new(half_width, y0, z0),
            Vec3::new(half_width, y1, z1),
            Vec3::new(-half_width, y1, z1),
            1.0,
        );

        // Caps as triangle fans, one per side.
        for (x, normal) in [(half_width, Vec3::X), (-half_width, Vec3::NEG_X)] {
            let base = m.vertices.len() as u32;
            let centre = Vec3::new(x, 0.0, 0.0);
            let p0 = Vec3::new(x, y0, z0);
            let p1 = Vec3::new(x, y1, z1);
            for p in [centre, p0, p1] {
                m.vertices.push(Vertex {
                    pos: p.to_array(),
                    normal: normal.to_array(),
                    uv: [0.5, 0.5],
                    gravity_blend: 0.0,
                });
            }
            if x > 0.0 {
                m.indices.extend_from_slice(&[base, base + 1, base + 2]);
            } else {
                m.indices.extend_from_slice(&[base, base + 2, base + 1]);
            }
        }
    }
    m
}
