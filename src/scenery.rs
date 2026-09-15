//! Background scenery: the structures outside the tube, and the dome the sky is
//! painted on.
//!
//! Props are generated rather than authored, which suits a board whose storage
//! link is PCIe 2.0 x2, and are baked into a single mesh at load. They never
//! move and they are never near enough to need their own transform, so one
//! draw call is the whole skyline.
//!
//! Any prop in the asset library replaces its generated shape. That is the
//! point of the manifest: the game looks finished now and looks better later,
//! without either version being a special case.

use glam::{Quat, Vec3};

use crate::assets::Library;
use crate::mesh::Mesh;
use crate::model;
use crate::track::{Track, Vertex};

/// Total props placed around a lap. Enough to fill the open stretches, few
/// enough that the baked mesh stays small.
const MAX_PROPS: usize = 96;
/// Clearance between the tube wall and the nearest part of any prop.
const CLEARANCE: f32 = 10.0;

pub struct Scenery {
    /// Every prop on the track, baked into one static mesh in world space.
    pub props: Mesh,
    /// Unit-radius dome, drawn centred on the camera.
    pub sky: Mesh,
}

/// Push a box, tapering toward the top by `taper` (1.0 is a plain box).
///
/// `variation` rides along in the vertex's spare `gravity_blend` slot, which
/// means nothing for a prop. The shader hashes it to give each structure its
/// own colour and window pattern without a per-prop draw or a second buffer.
fn push_box(mesh: &mut Mesh, centre: Vec3, half: Vec3, taper: f32, variation: f32) {
    let (x, y, z) = (half.x, half.y, half.z);
    let (tx, tz) = (x * taper, z * taper);
    let corners = [
        // bottom, then top
        Vec3::new(-x, -y, -z),
        Vec3::new(x, -y, -z),
        Vec3::new(x, -y, z),
        Vec3::new(-x, -y, z),
        Vec3::new(-tx, y, -tz),
        Vec3::new(tx, y, -tz),
        Vec3::new(tx, y, tz),
        Vec3::new(-tx, y, tz),
    ];
    const FACES: [[usize; 4]; 6] = [
        [0, 1, 2, 3], // bottom
        [7, 6, 5, 4], // top
        [0, 4, 5, 1],
        [1, 5, 6, 2],
        [2, 6, 7, 3],
        [3, 7, 4, 0],
    ];
    for face in FACES {
        let p: Vec<Vec3> = face.iter().map(|&i| centre + corners[i]).collect();
        let normal = (p[1] - p[0]).cross(p[3] - p[0]).normalize_or(Vec3::Y);
        let base = mesh.vertices.len() as u32;
        for (k, &pos) in p.iter().enumerate() {
            // v runs 0 at the base to 1 at the top, so the shader can shade and
            // light a structure by its own height rather than by world space.
            let v = if face == FACES[0] {
                0.0
            } else if face == FACES[1] {
                1.0
            } else {
                [0.0, 1.0, 1.0, 0.0][k]
            };
            mesh.vertices.push(Vertex {
                pos: pos.to_array(),
                normal: normal.to_array(),
                uv: [[0.0, 1.0, 1.0, 0.0][k], v],
                gravity_blend: variation,
            });
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// Append `other`, moved and turned into place.
fn append(mesh: &mut Mesh, other: &Mesh, at: Vec3, rotation: Quat, scale: f32, variation: f32) {
    let base = mesh.vertices.len() as u32;
    for v in &other.vertices {
        let pos = at + rotation * (Vec3::from_array(v.pos) * scale);
        mesh.vertices.push(Vertex {
            pos: pos.to_array(),
            normal: (rotation * Vec3::from_array(v.normal)).to_array(),
            uv: v.uv,
            gravity_blend: variation,
        });
    }
    mesh.indices.extend(other.indices.iter().map(|i| i + base));
}

fn pylon(height: f32, variation: f32) -> Mesh {
    let mut m = Mesh { vertices: Vec::new(), indices: Vec::new() };
    push_box(&mut m, Vec3::new(0.0, height * 0.5, 0.0), Vec3::new(3.5, height * 0.5, 3.5), 0.45, variation);
    m
}

fn tower(height: f32, variation: f32) -> Mesh {
    let mut m = Mesh { vertices: Vec::new(), indices: Vec::new() };
    // Stepped, each tier narrower than the one below, so a skyline of them has
    // a silhouette rather than being a row of slabs.
    let tiers = 3;
    let mut base = 0.0;
    let mut half = 9.0;
    for t in 0..tiers {
        let tier_height = height / tiers as f32 * (1.0 - 0.12 * t as f32);
        push_box(
            &mut m,
            Vec3::new(0.0, base + tier_height * 0.5, 0.0),
            Vec3::new(half, tier_height * 0.5, half * 0.8),
            0.94,
            variation,
        );
        base += tier_height;
        half *= 0.72;
    }
    m
}

fn antenna(height: f32, variation: f32) -> Mesh {
    let mut m = Mesh { vertices: Vec::new(), indices: Vec::new() };
    push_box(&mut m, Vec3::new(0.0, height * 0.5, 0.0), Vec3::new(1.0, height * 0.5, 1.0), 0.3, variation);
    push_box(&mut m, Vec3::new(0.0, height * 0.82, 0.0), Vec3::new(7.0, 0.4, 0.5), 1.0, variation);
    push_box(&mut m, Vec3::new(0.0, height * 0.66, 0.0), Vec3::new(4.5, 0.4, 0.5), 1.0, variation);
    m
}

fn billboard(variation: f32) -> Mesh {
    let mut m = Mesh { vertices: Vec::new(), indices: Vec::new() };
    push_box(&mut m, Vec3::new(0.0, 8.0, 0.0), Vec3::new(14.0, 8.0, 0.6), 1.0, variation);
    for side in [-9.0f32, 9.0] {
        push_box(&mut m, Vec3::new(side, 0.0, 0.0), Vec3::new(0.8, 8.0, 0.8), 1.0, variation);
    }
    m
}

/// A gateway over the road. Built in track space: x across, y up, z along.
fn arch(span: f32, height: f32, variation: f32) -> Mesh {
    let mut m = Mesh { vertices: Vec::new(), indices: Vec::new() };
    for side in [-1.0f32, 1.0] {
        push_box(
            &mut m,
            Vec3::new(side * span * 0.5, height * 0.5, 0.0),
            Vec3::new(2.4, height * 0.5, 2.4),
            0.8,
            variation,
        );
    }
    push_box(&mut m, Vec3::new(0.0, height, 0.0), Vec3::new(span * 0.5 + 2.4, 2.6, 3.2), 1.0, variation);
    m
}

/// Inward-facing UV sphere. Equirectangular, so the sky plate maps onto it with
/// x as the bearing and y from zenith to nadir - the space the plates are drawn
/// in.
fn dome(rings: usize, segments: usize) -> Mesh {
    let mut m = Mesh { vertices: Vec::new(), indices: Vec::new() };
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let polar = v * std::f32::consts::PI;
        let (sp, cp) = polar.sin_cos();
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            let azimuth = u * std::f32::consts::TAU;
            let (sa, ca) = azimuth.sin_cos();
            // +y at v = 0, so the top of the plate is the top of the sky.
            let pos = Vec3::new(sp * ca, cp, sp * sa);
            m.vertices.push(Vertex {
                pos: pos.to_array(),
                // Facing inward: the camera is inside it.
                normal: (-pos).to_array(),
                uv: [u, v],
                gravity_blend: 0.0,
            });
        }
    }
    let stride = (segments + 1) as u32;
    for r in 0..rings as u32 {
        for s in 0..segments as u32 {
            let a = r * stride + s;
            m.indices.extend_from_slice(&[a, a + 1, a + stride + 1, a, a + stride + 1, a + stride]);
        }
    }
    m
}

/// Where a prop stands: beside the open stretches, where there is sky to see it
/// against. Inside the tube it would only be a wall with a building behind it.
pub fn generate(track: &Track, seed: u64, library: &Library) -> Scenery {
    let mut rng = crate::track::seed_rng(seed ^ 0x5CE7);
    let mut props = Mesh { vertices: Vec::new(), indices: Vec::new() };

    // A supplied model replaces the generated shape for that slot. The loader
    // refits whatever it is given to the size of a car, which is right for a
    // car and useless for a hundred-metre tower, so its height comes back with
    // it and placement scales it to the size the slot wanted.
    let supplied = |id: &str| -> Option<(Mesh, f32)> {
        let path = library.path(id)?;
        match model::Model::load(path.to_str()?, model::Fit::default()) {
            Ok(m) => {
                let height = m
                    .chassis
                    .vertices
                    .iter()
                    .map(|v| v.pos[1])
                    .fold(f32::MIN, f32::max)
                    - m.chassis.vertices.iter().map(|v| v.pos[1]).fold(f32::MAX, f32::min);
                Some((m.chassis, height.max(0.01)))
            }
            Err(e) => {
                eprintln!("scenery: {e}; using the generated {id}");
                None
            }
        }
    };
    let custom = [
        supplied("prop_pylon"),
        supplied("prop_tower"),
        supplied("prop_antenna"),
        supplied("prop_billboard"),
        supplied("prop_arch"),
    ];

    // Only the open stretches, and spaced out along them.
    let candidates: Vec<usize> = (0..track.frames.len())
        .filter(|&i| track.frames[i].gravity_blend < 0.45)
        .collect();
    if candidates.is_empty() {
        return Scenery { props, sky: dome(24, 48) };
    }
    let step = (candidates.len() / MAX_PROPS.min(candidates.len())).max(3);

    for &i in candidates.iter().step_by(step) {
        let frame = &track.frames[i];
        // Across the track, level with the world rather than with the tube, so
        // a structure stands up straight next to a banked corner.
        let across = frame.tangent.cross(Vec3::Y).normalize_or(Vec3::X);
        let variation = rng();

        let kind = (rng() * 4.0) as usize;
        if rng() < 0.16 {
            // A gateway, straddling the road. The legs sit outside the tube
            // wall and the crossbar clears the top of it.
            let span = (frame.radius + CLEARANCE) * 2.0;
            let height = frame.radius * 2.2;
            let rotation = crate::track::look_rotation(frame.tangent, Vec3::Y);
            let base = frame.pos - Vec3::Y * frame.radius;
            let generated = arch(span, height, variation);
            let (m, scale) = match custom[4].as_ref() {
                Some((m, h)) => (m, height / h),
                None => (&generated, 1.0),
            };
            append(&mut props, m, base, rotation, scale, variation);
            continue;
        }

        for side in [-1.0f32, 1.0] {
            if rng() < 0.35 {
                continue;
            }
            let out = frame.radius + CLEARANCE + rng() * 70.0;
            // Founded below the road, so a structure rises past it rather than
            // appearing to rest on thin air at eye level.
            let base = frame.pos + across * side * out - Vec3::Y * (frame.radius + 6.0 + rng() * 40.0);
            let height = 30.0 + rng() * 110.0;
            let rotation = Quat::from_rotation_y(rng() * std::f32::consts::TAU);

            let slot = kind.min(3);
            let generated = match slot {
                0 => pylon(height, variation),
                1 => tower(height, variation),
                2 => antenna(height, variation),
                _ => billboard(variation),
            };
            let (m, scale) = match custom[slot].as_ref() {
                Some((m, h)) => (m, height / h),
                None => (&generated, 1.0),
            };
            append(&mut props, m, base, rotation, scale, variation);
        }
    }

    Scenery { props, sky: dome(24, 48) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenery(seed: u64) -> Scenery {
        generate(&Track::generate(seed), seed, &Library::load("assets"))
    }

    /// Scenery must never poke through the tube wall into the road. There is no
    /// collision against it, so a prop that intersects the track is a building
    /// the car drives straight through.
    #[test]
    fn no_prop_reaches_the_driving_surface() {
        for seed in [1u64, 7, 42] {
            let track = Track::generate(seed);
            let s = generate(&track, seed, &Library::load("assets"));
            assert!(!s.props.vertices.is_empty(), "seed {seed}: no scenery was placed");

            let mut hint = 0;
            let mut worst = f32::MAX;
            for v in &s.props.vertices {
                let p = Vec3::from_array(v.pos);
                let surface = track.surface(p, hint);
                hint = surface.index;
                // `gap` is the distance in to the wall: positive means the point
                // is inside the tube, which is exactly what must not happen.
                worst = worst.min(-surface.gap);
            }
            assert!(
                worst > 0.0,
                "seed {seed}: scenery intrudes {:.1} m into the tube",
                -worst
            );
        }
    }

    /// Props belong beside the open stretches. Inside the tube they are hidden
    /// behind a wall and cost fill rate for nothing.
    #[test]
    fn scenery_stands_beside_the_open_stretches() {
        let track = Track::generate(7);
        let s = generate(&track, 7, &Library::load("assets"));
        let mut hint = 0;
        let mut enclosed = 0;
        let mut total = 0;
        for v in s.props.vertices.iter().step_by(24) {
            let surface = track.surface(Vec3::from_array(v.pos), hint);
            hint = surface.index;
            total += 1;
            if track.frames[surface.index].gravity_blend > 0.6 {
                enclosed += 1;
            }
        }
        assert!(total > 0);
        assert!(
            (enclosed as f32 / total as f32) < 0.25,
            "{enclosed} of {total} sampled prop vertices sit beside enclosed tube"
        );
    }

    /// The skyline has to be worth the draw, and has to stay affordable on a
    /// 24 compute unit part.
    #[test]
    fn the_prop_mesh_is_substantial_but_bounded() {
        let s = scenery(7);
        let tris = s.props.indices.len() / 3;
        assert!(tris > 200, "only {tris} triangles of scenery; the skyline is bare");
        assert!(tris < 60_000, "{tris} triangles of static scenery is too many");
        assert_eq!(s.props.indices.len() % 3, 0);
        assert!(s.props.indices.iter().all(|&i| (i as usize) < s.props.vertices.len()));
    }

    /// The dome carries the sky plate, so its mapping has to match the space
    /// the plates are drawn in: v = 0 at the top.
    #[test]
    fn the_dome_faces_inward_and_maps_the_sky_the_right_way_up() {
        let s = scenery(7);
        let top = s
            .sky
            .vertices
            .iter()
            .min_by(|a, b| a.uv[1].total_cmp(&b.uv[1]))
            .expect("empty dome");
        assert!(top.pos[1] > 0.9, "v = 0 is not the top of the dome");

        // Every normal points back at the centre, because the camera is inside.
        for v in &s.sky.vertices {
            let pos = Vec3::from_array(v.pos);
            let normal = Vec3::from_array(v.normal);
            assert!((pos.length() - 1.0).abs() < 1e-3, "the dome is not a unit sphere");
            assert!(normal.dot(pos) < 0.0, "a dome face points outward");
        }
    }

    /// Same seed, same skyline: the world has to be reproducible from a seed
    /// alone, which is how a track can be shared as a number.
    #[test]
    fn the_same_seed_gives_the_same_scenery() {
        let a = scenery(11);
        let b = scenery(11);
        assert_eq!(a.props.indices.len(), b.props.indices.len());
        assert!(a
            .props
            .vertices
            .iter()
            .zip(b.props.vertices.iter())
            .all(|(x, y)| x.pos == y.pos));
    }
}
