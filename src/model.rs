//! glTF/GLB model loading.
//!
//! Models are fitted to the car's physics dimensions on load rather than being
//! required to arrive at the right scale or orientation. Generated models
//! (Meshy, Tripo and friends) come out at arbitrary size and facing, and fixing
//! that in a DCC tool for every iteration is a waste of time.

use glam::{Mat4, Vec3};

use crate::mesh::Mesh;
use crate::track::Vertex;

/// Target length of the car in metres, taken from the physics chassis so art
/// and collision cannot drift apart.
const TARGET_LENGTH: f32 = crate::vehicle::HALF_EXTENTS.z * 2.0;
/// Widest the car may render: the physics chassis plus a little for wheels
/// standing proud of it. Fitting on length alone lets a stocky model spill well
/// outside the body that is actually colliding, so whichever axis binds first
/// wins.
const TARGET_WIDTH: f32 = crate::vehicle::HALF_EXTENTS.x * 2.0 + 0.2;
/// Below this ratio between the two horizontal axes, which one points forward
/// is not decidable from the bounding box.
const SQUARE_FOOTPRINT_RATIO: f32 = 1.15;
/// Mirror symmetry only decides the facing when one axis wins by this margin.
/// Generated meshes are only loosely symmetric, and a smaller gap than this
/// flips sign with the voxel resolution, which is to say it is noise.
const SYMMETRY_MARGIN: f32 = 0.10;

/// How to orient and size a model that was not authored for this game.
#[derive(Clone, Copy)]
pub struct Fit {
    /// Extra yaw, on top of the automatic sideways correction.
    pub yaw_degrees: f32,
    /// Extra pitch. Use -90 for a Z-up model, which Blender exports produce.
    pub pitch_degrees: f32,
    /// Extra roll about the length axis. Use 180 for a model that is upside
    /// down in its own file, which flips it without swapping front for back.
    pub roll_degrees: f32,
    /// Multiplier on the automatic fit, for taste.
    pub scale: f32,
}

impl Default for Fit {
    fn default() -> Fit {
        Fit { yaw_degrees: 0.0, pitch_degrees: 0.0, roll_degrees: 0.0, scale: 1.0 }
    }
}

pub struct Model {
    pub chassis: Mesh,
    /// Present only when the file has a node whose name mentions a wheel. A
    /// single fused mesh - what generators usually produce - has none, and its
    /// wheels are already part of the body.
    pub wheel: Option<Mesh>,
}

#[derive(Clone, Copy)]
pub struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    fn new() -> Bounds {
        Bounds { min: Vec3::splat(f32::MAX), max: Vec3::splat(f32::MIN) }
    }
    fn add(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }
    fn size(&self) -> Vec3 {
        self.max - self.min
    }
    fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }
}

impl Model {
    /// Load a .glb or .gltf file, refit it to the physics chassis, and split out
    /// a wheel mesh if the file names one.
    pub fn load(path: &str, fit: Fit) -> Result<Model, String> {
        let (document, buffers, _images) =
            gltf::import(path).map_err(|e| format!("failed to read {path}: {e}"))?;

        let mut chassis_parts: Vec<Mesh> = Vec::new();
        let mut wheel_parts: Vec<Mesh> = Vec::new();

        for scene in document.scenes() {
            for node in scene.nodes() {
                collect_node(&node, Mat4::IDENTITY, &buffers, &mut chassis_parts, &mut wheel_parts);
            }
        }

        let mut chassis = merge(chassis_parts);
        let mut wheel = if wheel_parts.is_empty() { None } else { Some(merge(wheel_parts)) };

        if chassis.vertices.is_empty() {
            return Err(format!("{path} contains no geometry"));
        }

        // Measure the body alone throughout. Including wheels would shrink the
        // body to compensate for tyres standing proud of it.
        let source = bounds_of(&chassis).size();

        // Caller-supplied correction first, since it decides which axis is which
        // before anything is measured for the automatic pass.
        let mut parts: Vec<&mut Mesh> =
            std::iter::once(&mut chassis).chain(wheel.as_mut()).collect();
        let manual = Mat4::from_rotation_y(fit.yaw_degrees.to_radians())
            * Mat4::from_rotation_x(fit.pitch_degrees.to_radians())
            * Mat4::from_rotation_z(fit.roll_degrees.to_radians());
        for part in parts.iter_mut() {
            apply(part, manual);
        }

        // glTF is Y-up with -Z forward, same as the engine, but a generated model
        // often still faces sideways. That shows up as the wider horizontal axis
        // being X rather than Z.
        let oriented = bounds_of(parts[0]).size();
        let longer = oriented.x.max(oriented.z);
        let shorter = oriented.x.min(oriented.z).max(1e-3);
        let square_footprint = longer / shorter < SQUARE_FOOTPRINT_RATIO;

        // Prefer mirror symmetry over the bounding box. A car is symmetric left
        // to right but not front to back, so the axis it mirrors across is its
        // width, and the other one points down the track. This still works for a
        // model that is nearly square in plan, where comparing extents is barely
        // better than a coin toss.
        let (symmetry_x, symmetry_z) = symmetry_scores(parts[0]);
        let decisive = (symmetry_x - symmetry_z).abs() > SYMMETRY_MARGIN;
        let auto_yaw = if decisive {
            // Symmetric across Z means Z is the width, so the length is along X
            // and needs turning to face down the track.
            if symmetry_z > symmetry_x { 90.0f32 } else { 0.0 }
        } else if oriented.x > oriented.z {
            90.0
        } else {
            0.0
        };
        if auto_yaw != 0.0 {
            let turn = Mat4::from_rotation_y(auto_yaw.to_radians());
            for part in parts.iter_mut() {
                apply(part, turn);
            }
        }

        let bounds = bounds_of(parts[0]);
        let size = bounds.size();
        let by_length = TARGET_LENGTH / size.z.max(1e-3);
        let by_width = TARGET_WIDTH / size.x.max(1e-3);
        let limit = if by_width < by_length { "width" } else { "length" };
        let scale = by_length.min(by_width) * fit.scale;

        let transform = Mat4::from_scale(Vec3::splat(scale)) * Mat4::from_translation(-bounds.center());
        for part in parts.iter_mut() {
            apply(part, transform);
        }

        let fitted = bounds_of(&chassis).size();
        println!(
            "model {path}: {} tris, source {:.2}x{:.2}x{:.2} m -> {:.2}x{:.2}x{:.2} m \
             (scale {:.3}x, limited by {limit}{}){}",
            chassis.indices.len() / 3,
            source.x, source.y, source.z,
            fitted.x, fitted.y, fitted.z,
            scale,
            if auto_yaw != 0.0 { ", auto-yawed 90 deg" } else { "" },
            if wheel.is_some() { ", separate wheel mesh" } else { ", wheels fused into body" },
        );
        if decisive {
            println!(
                "  facing: mirror symmetry is {:.0}% across X and {:.0}% across Z, so {} is the \
                 width and the car faces along {}",
                symmetry_x * 100.0,
                symmetry_z * 100.0,
                if symmetry_x > symmetry_z { "X" } else { "Z" },
                if symmetry_x > symmetry_z { "Z" } else { "X" },
            );
        } else if square_footprint {
            println!(
                "  note: the footprint is nearly square ({:.2} x {:.2} m) and mirror symmetry is \
                 inconclusive ({:.0}% vs {:.0}%), so which axis points forward is a guess. If the \
                 car drives sideways, add --car-yaw 90.",
                oriented.x, oriented.z,
                symmetry_x * 100.0,
                symmetry_z * 100.0,
            );
        }
        // Width is the budget that keeps art aligned with collision, so say so
        // loudly when a manual scale has pushed the model past it.
        let chassis_width = crate::vehicle::HALF_EXTENTS.x * 2.0;
        if fitted.x > TARGET_WIDTH * 1.02 {
            println!(
                "  warning: {:.2} m wide, past the {TARGET_WIDTH:.1} m budget and well past the \
                 {chassis_width:.1} m physics chassis. Wheels will visibly overhang the car during \
                 contact. Drop --car-scale to about {:.2} to stay within it.",
                fitted.x,
                fit.scale * TARGET_WIDTH / fitted.x.max(1e-3)
            );
        } else if fitted.z < TARGET_LENGTH * 0.85 {
            println!(
                "  note: this model is stocky for a car, so matching the {TARGET_WIDTH:.1} m width \
                 budget leaves it {:.2} m long against a {TARGET_LENGTH:.1} m chassis. Either \
                 accept it, pass --car-scale, or widen HALF_EXTENTS so the physics matches the art.",
                fitted.z
            );
        }

        Ok(Model { chassis, wheel })
    }
}

fn collect_node(
    node: &gltf::Node,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    chassis: &mut Vec<Mesh>,
    wheels: &mut Vec<Mesh>,
) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;

    let is_wheel = node
        .name()
        .map(|n| {
            let n = n.to_ascii_lowercase();
            n.contains("wheel") || n.contains("tyre") || n.contains("tire")
        })
        .unwrap_or(false);

    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if let Some(part) = read_primitive(&primitive, world, buffers) {
                if is_wheel {
                    wheels.push(part);
                } else {
                    chassis.push(part);
                }
            }
        }
    }
    for child in node.children() {
        collect_node(&child, world, buffers, chassis, wheels);
    }
}

fn read_primitive(
    primitive: &gltf::Primitive,
    transform: Mat4,
    buffers: &[gltf::buffer::Data],
) -> Option<Mesh> {
    if primitive.mode() != gltf::mesh::Mode::Triangles {
        return None;
    }
    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
    let positions: Vec<[f32; 3]> = reader.read_positions()?.collect();
    if positions.is_empty() {
        return None;
    }

    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|n| n.collect());
    let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|t| t.into_f32().collect());

    // Normals transform by the inverse transpose; for the rigid, uniformly
    // scaled transforms we build here the matrix itself is sufficient.
    let mut vertices: Vec<Vertex> = positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let pos = transform.transform_point3(Vec3::from(*p));
            let normal = normals
                .as_ref()
                .map(|n| transform.transform_vector3(Vec3::from(n[i])).normalize_or(Vec3::Y))
                .unwrap_or(Vec3::Y);
            Vertex {
                pos: pos.to_array(),
                normal: normal.to_array(),
                uv: uvs.as_ref().map(|u| u[i]).unwrap_or([0.0, 0.0]),
                // Only track surfaces carry a gravity zone.
                gravity_blend: 0.0,
            }
        })
        .collect();

    let indices: Vec<u32> = match reader.read_indices() {
        Some(i) => i.into_u32().collect(),
        None => (0..vertices.len() as u32).collect(),
    };

    if normals.is_none() {
        compute_flat_normals(&mut vertices, &indices);
    }
    Some(Mesh { vertices, indices })
}

/// Generated models sometimes ship without normals, which would otherwise light
/// as a flat silhouette.
fn compute_flat_normals(vertices: &mut [Vertex], indices: &[u32]) {
    let mut accum = vec![Vec3::ZERO; vertices.len()];
    for tri in indices.chunks_exact(3) {
        let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (pa, pb, pc) = (
            Vec3::from(vertices[a].pos),
            Vec3::from(vertices[b].pos),
            Vec3::from(vertices[c].pos),
        );
        let n = (pb - pa).cross(pc - pa);
        accum[a] += n;
        accum[b] += n;
        accum[c] += n;
    }
    for (v, n) in vertices.iter_mut().zip(accum) {
        v.normal = n.normalize_or(Vec3::Y).to_array();
    }
}

fn merge(parts: Vec<Mesh>) -> Mesh {
    let mut out = Mesh { vertices: Vec::new(), indices: Vec::new() };
    for part in parts {
        let base = out.vertices.len() as u32;
        out.vertices.extend(part.vertices);
        out.indices.extend(part.indices.into_iter().map(|i| i + base));
    }
    out
}

/// Fraction of the model that mirrors onto itself across the X and Z midplanes.
/// Vertices are rasterised into a coarse voxel grid first, so this costs one
/// pass over the mesh rather than a nearest-neighbour search, and it tolerates
/// the asymmetric triangulation a generated mesh usually has.
fn symmetry_scores(mesh: &Mesh) -> (f32, f32) {
    const N: usize = 16;
    let bounds = bounds_of(mesh);
    let size = bounds.size().max(Vec3::splat(1e-3));

    let mut occupied = vec![false; N * N * N];
    for v in &mesh.vertices {
        let p = (Vec3::from(v.pos) - bounds.min) / size * (N as f32 - 1.0);
        let (x, y, z) = (
            (p.x as usize).min(N - 1),
            (p.y as usize).min(N - 1),
            (p.z as usize).min(N - 1),
        );
        occupied[(z * N + y) * N + x] = true;
    }

    let at = |x: usize, y: usize, z: usize| occupied[(z * N + y) * N + x];
    let (mut total, mut mirrored_x, mut mirrored_z) = (0usize, 0usize, 0usize);
    for z in 0..N {
        for y in 0..N {
            for x in 0..N {
                if !at(x, y, z) {
                    continue;
                }
                total += 1;
                if at(N - 1 - x, y, z) {
                    mirrored_x += 1;
                }
                if at(x, y, N - 1 - z) {
                    mirrored_z += 1;
                }
            }
        }
    }
    if total == 0 {
        return (0.0, 0.0);
    }
    (mirrored_x as f32 / total as f32, mirrored_z as f32 / total as f32)
}

fn bounds_of(mesh: &Mesh) -> Bounds {
    let mut b = Bounds::new();
    for v in &mesh.vertices {
        b.add(Vec3::from(v.pos));
    }
    b
}

fn apply(mesh: &mut Mesh, transform: Mat4) {
    for v in &mut mesh.vertices {
        v.pos = transform.transform_point3(Vec3::from(v.pos)).to_array();
        v.normal = transform
            .transform_vector3(Vec3::from(v.normal))
            .normalize_or(Vec3::Y)
            .to_array();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a minimal glTF 2.0 file with an external buffer: one triangle
    /// spanning 2 x 0.5 x 4 metres, so refitting has something to measure.
    /// `faces_along_x` mirrors how a real car is shaped: symmetric across its
    /// width, asymmetric front to back. A wedge symmetric across its *long* axis
    /// is not a car, and testing against one hides orientation bugs.
    fn write_fixture(
        dir: &std::path::Path,
        name: &str,
        extents: Vec3,
        faces_along_x: bool,
    ) -> String {
        let (hx, hz) = (extents.x * 0.5, extents.z * 0.5);
        let positions: [f32; 9] = if faces_along_x {
            // Length along X, so the mirror plane is across Z.
            [-hx, 0.0, -hz, -hx, 0.0, hz, hx, extents.y, 0.0]
        } else {
            // Length along Z, so the mirror plane is across X.
            [-hx, 0.0, -hz, hx, 0.0, -hz, 0.0, extents.y, hz]
        };
        let indices: [u16; 3] = [0, 1, 2];

        let mut bin = Vec::new();
        for f in positions {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        for i in indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        std::fs::write(dir.join("fixture.bin"), &bin).unwrap();

        let json = format!(
            r#"{{
"asset": {{"version": "2.0"}},
"scene": 0,
"scenes": [{{"nodes": [0]}}],
"nodes": [{{"mesh": 0, "name": "{name}"}}],
"meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0}}, "indices": 1}}]}}],
"accessors": [
  {{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
    "min": [{:?}, 0.0, {:?}], "max": [{:?}, {:?}, {:?}]}},
  {{"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}}
],
"bufferViews": [
  {{"buffer": 0, "byteOffset": 0, "byteLength": 36}},
  {{"buffer": 0, "byteOffset": 36, "byteLength": 6}}
],
"buffers": [{{"uri": "fixture.bin", "byteLength": {}}}]
}}"#,
            -hx, -hz, hx, extents.y, hz, bin.len()
        );
        let path = dir.join("fixture.gltf");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(json.as_bytes()).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("treadlock-model-test-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn refits_model_to_the_physics_chassis() {
        let dir = temp_dir("refit");
        let path = write_fixture(&dir, "Car", Vec3::new(2.0, 0.5, 4.0), false);
        let model = Model::load(&path, Fit::default()).expect("load");

        let size = bounds_of(&model.chassis).size();
        // Longest horizontal axis is scaled to the target car length.
        assert!(
            (size.z - TARGET_LENGTH).abs() < 1e-3,
            "expected length {TARGET_LENGTH}, got {}",
            size.z
        );
        // Proportions are preserved: source was 2 x 0.5 x 4.
        assert!((size.x - TARGET_LENGTH / 2.0).abs() < 1e-3);
        // Recentred on the origin, which is where the physics body sits.
        let centre = bounds_of(&model.chassis).center();
        assert!(centre.length() < 1e-3, "model not centred: {centre}");
        // No node mentioned a wheel, so the wheels are part of the body.
        assert!(model.wheel.is_none());
        // Normals were absent in the fixture and must be generated.
        assert!(Vec3::from(model.chassis.vertices[0].normal).length() > 0.9);
    }

    /// A stocky model - wide relative to its length, which is what a generated
    /// car with big exposed wheels looks like - must not be scaled until it
    /// overhangs the body that is actually colliding.
    #[test]
    fn stocky_model_is_limited_by_width_not_length() {
        // Both cases are measured from real generated cars, which come out far
        // wider relative to their length than a road car and face along X.
        for extents in [Vec3::new(1.90, 0.53, 1.37), Vec3::new(1.00, 0.31, 0.91)] {
            let dir = temp_dir(&format!("stocky-{}", extents.x));
            let path = write_fixture(&dir, "Car", extents, true);
            let model = Model::load(&path, Fit::default()).expect("load");

            let size = bounds_of(&model.chassis).size();
            assert!(
                size.x <= TARGET_WIDTH + 1e-3,
                "{extents}: fitted width {} exceeds the {TARGET_WIDTH} m budget",
                size.x
            );
            // The auto-yaw should have turned its long axis down the track.
            assert!(size.z > size.x, "{extents}: long axis was not turned to face forward");
            assert!(size.z <= TARGET_LENGTH + 1e-3);
        }
    }

    /// A manual scale is an override, so it is allowed to exceed the width
    /// budget - but the fit must not quietly clamp it, because the printed
    /// warning is what tells the user their art will overhang the collision.
    #[test]
    fn manual_scale_may_exceed_the_width_budget() {
        let dir = temp_dir("oversized");
        let path = write_fixture(&dir, "Car", Vec3::new(1.00, 0.31, 0.91), true);
        let model = Model::load(&path, Fit { scale: 1.5, ..Fit::default() }).expect("load");
        assert!(bounds_of(&model.chassis).size().x > TARGET_WIDTH);
    }

    #[test]
    fn scale_override_multiplies_the_automatic_fit() {
        let dir = temp_dir("scaled");
        let path = write_fixture(&dir, "Car", Vec3::new(2.0, 0.5, 4.0), false);
        let base = Model::load(&path, Fit::default()).expect("load");
        let bigger = Model::load(&path, Fit { scale: 2.0, ..Fit::default() }).expect("load");

        let a = bounds_of(&base.chassis).size();
        let b = bounds_of(&bigger.chassis).size();
        assert!((b.z - a.z * 2.0).abs() < 1e-3, "scale override did not apply");
    }

    #[test]
    fn separates_a_named_wheel_node() {
        let dir = temp_dir("wheel");
        let path = write_fixture(&dir, "Wheel_FL", Vec3::new(2.0, 0.5, 4.0), false);
        let model = Model::load(&path, Fit::default());
        // The only geometry is a wheel, so there is no body to fit against.
        assert!(model.is_err(), "a wheel-only file should not load as a car");
    }
}
