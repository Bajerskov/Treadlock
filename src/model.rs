//! glTF/GLB model loading.
//!
//! Models are fitted to the car's physics dimensions on load rather than being
//! required to arrive at the right scale or orientation. Generated models
//! (Meshy, Tripo and friends) come out at arbitrary size and facing, and fixing
//! that in a DCC tool for every iteration is a waste of time.

use glam::{Mat4, Vec3};

use crate::mesh::Mesh;
use crate::track::Vertex;

/// Target length of the car in metres, matching the physics chassis.
const TARGET_LENGTH: f32 = 4.2;
/// Widest the car may render, roughly the physics chassis plus its wheels.
/// Fitting on length alone lets a stocky model spill well outside the body that
/// is actually colliding, so whichever axis binds first wins.
const TARGET_WIDTH: f32 = 2.5;

/// How to orient and size a model that was not authored for this game.
#[derive(Clone, Copy)]
pub struct Fit {
    /// Extra yaw, on top of the automatic sideways correction.
    pub yaw_degrees: f32,
    /// Extra pitch. Use -90 for a Z-up model, which Blender exports produce.
    pub pitch_degrees: f32,
    /// Multiplier on the automatic fit, for taste.
    pub scale: f32,
}

impl Default for Fit {
    fn default() -> Fit {
        Fit { yaw_degrees: 0.0, pitch_degrees: 0.0, scale: 1.0 }
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
            * Mat4::from_rotation_x(fit.pitch_degrees.to_radians());
        for part in parts.iter_mut() {
            apply(part, manual);
        }

        // glTF is Y-up with -Z forward, same as the engine, but a generated model
        // often still faces sideways. That shows up as the wider horizontal axis
        // being X rather than Z.
        let oriented = bounds_of(parts[0]).size();
        let auto_yaw = if oriented.x > oriented.z { 90.0f32 } else { 0.0 };
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
        if fitted.z < TARGET_LENGTH * 0.85 {
            println!(
                "  note: this model is stocky for a car, so matching the {TARGET_WIDTH:.1} m width \
                 budget leaves it {:.2} m long against a {TARGET_LENGTH:.1} m chassis. \
                 Pass --car-scale to override.",
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
    fn write_fixture(dir: &std::path::Path, name: &str, extents: Vec3) -> String {
        let (hx, hz) = (extents.x * 0.5, extents.z * 0.5);
        let positions: [f32; 9] = [-hx, 0.0, -hz, hx, 0.0, -hz, 0.0, extents.y, hz];
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
        let path = write_fixture(&dir, "Car", Vec3::new(2.0, 0.5, 4.0));
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
        let dir = temp_dir("stocky");
        // Facing along X, as generators often produce.
        let path = write_fixture(&dir, "Car", Vec3::new(1.90, 0.53, 1.37));
        let model = Model::load(&path, Fit::default()).expect("load");

        let size = bounds_of(&model.chassis).size();
        assert!(
            size.x <= TARGET_WIDTH + 1e-3,
            "fitted width {} exceeds the {TARGET_WIDTH} m budget",
            size.x
        );
        // The auto-yaw should have turned its long axis down the track.
        assert!(size.z > size.x, "long axis was not turned to face forward");
        assert!(size.z <= TARGET_LENGTH + 1e-3);
    }

    #[test]
    fn scale_override_multiplies_the_automatic_fit() {
        let dir = temp_dir("scaled");
        let path = write_fixture(&dir, "Car", Vec3::new(2.0, 0.5, 4.0));
        let base = Model::load(&path, Fit::default()).expect("load");
        let bigger = Model::load(&path, Fit { scale: 2.0, ..Fit::default() }).expect("load");

        let a = bounds_of(&base.chassis).size();
        let b = bounds_of(&bigger.chassis).size();
        assert!((b.z - a.z * 2.0).abs() < 1e-3, "scale override did not apply");
    }

    #[test]
    fn separates_a_named_wheel_node() {
        let dir = temp_dir("wheel");
        let path = write_fixture(&dir, "Wheel_FL", Vec3::new(2.0, 0.5, 4.0));
        let model = Model::load(&path, Fit::default());
        // The only geometry is a wheel, so there is no body to fit against.
        assert!(model.is_err(), "a wheel-only file should not load as a car");
    }
}
