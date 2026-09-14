//! Procedural track generation.
//!
//! A track is a closed loop spline wrapped in a tube. Cars drive on the *inside*
//! of the tube, so the local "down" direction at any point is simply the radial
//! direction pointing away from the centerline. That single fact is what lets a
//! car drive up the walls and across the ceiling without any special cases.

use glam::{Mat3, Quat, Vec3};

/// Vertices around the circumference of the tube.
pub const RING_SEGMENTS: usize = 28;
/// Target spacing between centerline samples, in metres.
const SAMPLE_SPACING: f32 = 6.0;
/// Control points in the generating loop.
const CONTROL_POINTS: usize = 16;
/// How many open, world-gravity stretches there are per lap. The rest is tube.
const OPEN_ZONES: usize = 3;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// 1 where gravity follows the tube, 0 where it points at the world floor.
    /// Carried on the vertex so the surface can be shaded to say which it is.
    pub gravity_blend: f32,
}

/// A rotation-minimizing frame on the centerline.
#[derive(Clone, Copy)]
pub struct Frame {
    pub pos: Vec3,
    pub tangent: Vec3,
    pub normal: Vec3,
    pub binormal: Vec3,
    pub radius: f32,
    /// Distance along the centerline from the start of the loop.
    pub distance: f32,
    /// 1 = gravity follows the tube, so every surface is drivable. 0 = gravity
    /// points at the world floor, so the car is pinned to the bottom and a
    /// corner has to be taken on the banking.
    pub gravity_blend: f32,
}

/// The result of locating a world position relative to the tube surface.
#[allow(dead_code)] // tangent and radius are the basis for the upcoming AI racing line
pub struct Surface {
    /// Index of the nearest centerline frame; feed back as a hint next query.
    pub index: usize,
    /// Closest point on the centerline.
    pub center: Vec3,
    /// Unit radial direction from the centerline to the query point: the
    /// surface normal, pointing into the tube wall. Suspension and tyres work
    /// against this, whatever gravity happens to be doing.
    pub down: Vec3,
    /// Direction gravity pulls here, blended between the world floor and the
    /// tube wall. Deliberately not normalised: where the two oppose they
    /// cancel, and a moment of low gravity is a better answer than a
    /// discontinuity.
    pub gravity: Vec3,
    /// Direction of travel along the centerline at `center`.
    pub tangent: Vec3,
    /// Tube radius at `center`.
    pub radius: f32,
    /// Distance from the query point to the wall, measured radially. Negative
    /// means the point has passed through the wall.
    pub gap: f32,
    pub distance: f32,
}

pub struct Track {
    pub frames: Vec<Frame>,
    pub length: f32,
    /// Frame holding the start line. Chosen for being flat and straight rather
    /// than being first: starting on a slope or a crest makes the track dive out
    /// of view and reads as standing on top of a hill.
    pub start: usize,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

/// xorshift64*, so track generation is deterministic from a seed without
/// pulling in an RNG crate.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 40) as f32) / (1u32 << 24) as f32
    }
    /// Uniform in [-1, 1].
    fn signed(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }
}

fn catmull_rom(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> Vec3 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

impl Track {
    pub fn generate(seed: u64) -> Track {
        let mut rng = Rng::new(seed);

        // Control points on a jittered ring. The loop radius is large relative
        // to the tube radius, so radial and vertical jitter cannot make the
        // tube intersect itself.
        let loop_radius = 520.0;
        let mut control = Vec::with_capacity(CONTROL_POINTS);
        for i in 0..CONTROL_POINTS {
            let a = i as f32 / CONTROL_POINTS as f32 * std::f32::consts::TAU;
            let r = loop_radius * (1.0 + 0.28 * rng.signed());
            let height = 70.0 * rng.signed();
            control.push(Vec3::new(a.cos() * r, height, a.sin() * r));
        }

        // Dense sample of the closed Catmull-Rom spline, then resample to
        // uniform arc length so physics and mesh spacing are predictable.
        let dense_per_segment = 24;
        let mut dense = Vec::with_capacity(CONTROL_POINTS * dense_per_segment);
        for i in 0..CONTROL_POINTS {
            let p0 = control[(i + CONTROL_POINTS - 1) % CONTROL_POINTS];
            let p1 = control[i];
            let p2 = control[(i + 1) % CONTROL_POINTS];
            let p3 = control[(i + 2) % CONTROL_POINTS];
            for s in 0..dense_per_segment {
                let t = s as f32 / dense_per_segment as f32;
                dense.push(catmull_rom(p0, p1, p2, p3, t));
            }
        }

        let centers = resample_closed(&dense, SAMPLE_SPACING);
        let n = centers.len();
        assert!(n > 8, "track too short to be drivable");

        // Tangents by central difference around the closed loop.
        let mut tangents = Vec::with_capacity(n);
        for i in 0..n {
            let prev = centers[(i + n - 1) % n];
            let next = centers[(i + 1) % n];
            tangents.push((next - prev).normalize());
        }

        // Rotation-minimizing frames: carry the normal forward by the minimal
        // rotation between consecutive tangents, rather than using the Frenet
        // normal which flips on straights.
        let mut normals = Vec::with_capacity(n);
        let seed_axis = if tangents[0].y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
        normals.push(tangents[0].cross(seed_axis).normalize());
        for i in 1..n {
            let rot = Quat::from_rotation_arc(tangents[i - 1], tangents[i]);
            let carried = rot * normals[i - 1];
            // Re-orthogonalise against accumulated float drift.
            normals.push((carried - tangents[i] * carried.dot(tangents[i])).normalize());
        }

        // The loop is closed, so the frame carried all the way round generally
        // does not line up with where it started. Spread that mismatch evenly
        // so the UVs do not seam. Geometry is unaffected - the tube is round.
        let closing = Quat::from_rotation_arc(tangents[n - 1], tangents[0]) * normals[n - 1];
        let residual = signed_angle(closing, normals[0], tangents[0]);
        for i in 0..n {
            let correction = -residual * (i as f32 / n as f32);
            normals[i] = Quat::from_axis_angle(tangents[i], correction) * normals[i];
        }

        let mut frames = Vec::with_capacity(n);
        let mut distance = 0.0;
        for i in 0..n {
            if i > 0 {
                distance += (centers[i] - centers[i - 1]).length();
            }
            // Radius breathes along the track: tight sections feel fast, wide
            // sections give room to fight for a line.
            let phase = i as f32 / n as f32 * std::f32::consts::TAU;
            let radius = 30.0 + 6.0 * (phase * 3.0).sin() + 3.5 * (phase * 7.0).cos();
            // Alternate between full tube and open road, so a lap has a rhythm
            // rather than one continuous pipe: stretches where the walls and
            // ceiling are yours, and stretches where gravity pins you to the
            // floor and a corner has to be carried on the banking.
            let zone = (phase * OPEN_ZONES as f32).sin();
            let gravity_blend = smoothstep(-0.35, 0.35, zone);

            let tangent = tangents[i];
            let normal = normals[i];
            frames.push(Frame {
                pos: centers[i],
                tangent,
                normal,
                binormal: tangent.cross(normal).normalize(),
                radius,
                distance,
                gravity_blend,
            });
        }
        let length = distance + (centers[0] - centers[n - 1]).length();

        let start = pick_start(&frames);
        let (vertices, indices) = build_mesh(&frames);
        Track { frames, length, start, vertices, indices }
    }

    /// Nearest centerline frame to `pos`. `hint` is the previous result; the
    /// search stays local to it so this stays O(1) per physics query instead of
    /// scanning the whole track.
    pub fn nearest_index(&self, pos: Vec3, hint: usize) -> usize {
        let n = self.frames.len();
        const WINDOW: usize = 24;
        let mut best = hint.min(n - 1);
        let mut best_d = f32::MAX;
        for k in 0..=WINDOW * 2 {
            let i = (hint + n + k - WINDOW) % n;
            let d = self.frames[i].pos.distance_squared(pos);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        // If the local search hit the edge of its window the hint was stale
        // (teleport, respawn, first frame) - fall back to a full scan.
        let offset = (best + n - hint) % n;
        if offset == WINDOW || offset == (n - WINDOW) % n {
            return self.global_nearest_index(pos);
        }
        // The edge check alone is not enough: the best frame inside a stale
        // window can sit just short of the edge and still be the wrong part of
        // the track entirely. Anything in or near the tube is within a radius
        // and a couple of samples of its true nearest frame, so a result
        // farther than that means the hint was stale too.
        let reach = self.frames[best].radius + SAMPLE_SPACING * 3.0;
        if best_d > reach * reach {
            return self.global_nearest_index(pos);
        }
        best
    }

    pub fn global_nearest_index(&self, pos: Vec3) -> usize {
        let mut best = 0;
        let mut best_d = f32::MAX;
        for (i, f) in self.frames.iter().enumerate() {
            let d = f.pos.distance_squared(pos);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }

    /// Locate `pos` against the tube wall. Because the tube is a swept circle,
    /// this is an exact projection onto the centerline rather than a raycast -
    /// far cheaper than tracing against the mesh, and it never tunnels.
    pub fn surface(&self, pos: Vec3, hint: usize) -> Surface {
        let n = self.frames.len();
        let i = self.nearest_index(pos, hint);

        // Project onto the two segments touching the nearest frame and keep the
        // better one, so the result is smooth across frame boundaries.
        let mut best = (
            f32::MAX,
            self.frames[i].pos,
            self.frames[i].tangent,
            self.frames[i].radius,
            self.frames[i].distance,
            self.frames[i].gravity_blend,
        );
        for (a, b) in [((i + n - 1) % n, i), (i, (i + 1) % n)] {
            let fa = &self.frames[a];
            let fb = &self.frames[b];
            let seg = fb.pos - fa.pos;
            let len_sq = seg.length_squared();
            let t = if len_sq > 1e-6 {
                ((pos - fa.pos).dot(seg) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let center = fa.pos + seg * t;
            let d = center.distance_squared(pos);
            if d < best.0 {
                let tangent = fa.tangent.lerp(fb.tangent, t).normalize_or(fa.tangent);
                let radius = fa.radius + (fb.radius - fa.radius) * t;
                // `fb.distance` wraps to 0 at the seam; use arc length instead.
                let span = if b == 0 { self.length - fa.distance } else { fb.distance - fa.distance };
                let blend = fa.gravity_blend + (fb.gravity_blend - fa.gravity_blend) * t;
                best = (d, center, tangent, radius, fa.distance + span * t, blend);
            }
        }

        let (dist_sq, center, tangent, radius, distance, gravity_blend) = best;
        let radial = pos - center;
        // Exactly on the axis has no defined radial direction; any perpendicular
        // will do and the car is 30m from a wall anyway.
        let down = if dist_sq > 1e-8 { radial / dist_sq.sqrt() } else { self.frames[i].normal };
        Surface {
            index: i,
            center,
            down,
            // In an open stretch this is world down wherever you are, so the
            // walls stop being drivable and the car is held on the floor.
            gravity: Vec3::NEG_Y.lerp(down, gravity_blend),
            tangent,
            radius,
            gap: radius - dist_sq.sqrt(),
            distance,
        }
    }

    /// A start position and orientation on the tube floor.
    pub fn spawn(&self, lane_offset: f32) -> (Vec3, Quat) {
        let f = &self.frames[self.start];
        // "Floor" is whichever side of the tube is furthest from world up.
        let down = pick_floor_direction(f);
        let right = f.tangent.cross(-down).normalize();
        let pos = f.pos + down * (f.radius - 1.6) + right * lane_offset;
        (pos, look_rotation(f.tangent, -down))
    }
}

/// Pick the start line: the flattest, straightest stretch on the loop. Frame 0
/// is wherever the generator happened to begin, which can be mid-climb or on a
/// crest, and then the track falls out of sight the moment you look at it.
fn pick_start(frames: &[Frame]) -> usize {
    let n = frames.len();
    // Look far enough ahead that a short flat spot inside a bend does not win.
    let lookahead = (40.0 / SAMPLE_SPACING) as usize;
    let mut best = 0;
    let mut best_score = f32::MAX;

    for i in 0..n {
        let here = &frames[i];
        let mut gradient = 0.0f32;
        let mut turn = 0.0f32;
        for step in 0..lookahead {
            let a = &frames[(i + step) % n];
            let b = &frames[(i + step + 1) % n];
            gradient += a.tangent.y.abs();
            turn += 1.0 - a.tangent.dot(b.tangent).clamp(-1.0, 1.0);
        }
        // Climbing or diving is worse than turning: a bend still shows the road
        // ahead, whereas a slope hides it entirely.
        let score = gradient * 3.0 + turn * 40.0 + here.tangent.y.abs() * 6.0;
        if score < best_score {
            best_score = score;
            best = i;
        }
    }
    best
}

/// Direction from the centerline toward the part of the tube that reads as the
/// floor, i.e. the most world-downhill point of the ring.
fn pick_floor_direction(f: &Frame) -> Vec3 {
    let d = Vec3::NEG_Y - f.tangent * Vec3::NEG_Y.dot(f.tangent);
    d.normalize_or(f.normal)
}

/// Rotation whose -Z axis is `forward` and whose +Y axis is as close to `up` as
/// possible. glam's `look_to_rh` builds a view matrix, which is the inverse of
/// the object transform we want here.
pub fn look_rotation(forward: Vec3, up: Vec3) -> Quat {
    let f = forward.normalize();
    // Right-handed: right = forward x up. Taking the cross the other way round
    // yields a basis with determinant -1, and `Quat::from_mat3` silently returns
    // a garbage orientation for a mirror rather than a rotation.
    let r = f.cross(up).normalize_or(Vec3::X);
    let u = r.cross(f);
    Quat::from_mat3(&Mat3::from_cols(r, u, -f))
}

/// Smooth 0..1 ramp, for blending between gravity zones without a seam.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn signed_angle(from: Vec3, to: Vec3, axis: Vec3) -> f32 {
    let dot = from.dot(to).clamp(-1.0, 1.0);
    let angle = dot.acos();
    if from.cross(to).dot(axis) < 0.0 {
        -angle
    } else {
        angle
    }
}

/// Walk a densely sampled closed curve and emit points at uniform arc length.
fn resample_closed(dense: &[Vec3], spacing: f32) -> Vec<Vec3> {
    let n = dense.len();
    let mut total = 0.0;
    for i in 0..n {
        total += dense[i].distance(dense[(i + 1) % n]);
    }
    // Round the count so the final step back to the start matches the rest.
    let count = (total / spacing).round().max(8.0) as usize;
    let step = total / count as f32;

    let mut out = Vec::with_capacity(count);
    let mut seg = 0usize;
    let mut seg_start = 0.0f32;
    let mut seg_len = dense[0].distance(dense[1 % n]);
    for k in 0..count {
        let target = k as f32 * step;
        while seg + 1 < n && seg_start + seg_len < target {
            seg_start += seg_len;
            seg += 1;
            seg_len = dense[seg].distance(dense[(seg + 1) % n]);
        }
        let t = if seg_len > 1e-6 { (target - seg_start) / seg_len } else { 0.0 };
        out.push(dense[seg].lerp(dense[(seg + 1) % n], t.clamp(0.0, 1.0)));
    }
    out
}

fn build_mesh(frames: &[Frame]) -> (Vec<Vertex>, Vec<u32>) {
    let n = frames.len();
    let mut vertices = Vec::with_capacity(n * RING_SEGMENTS);
    for f in frames {
        for s in 0..RING_SEGMENTS {
            let a = s as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
            let radial = f.normal * a.cos() + f.binormal * a.sin();
            let pos = f.pos + radial * f.radius;
            vertices.push(Vertex {
                pos: pos.to_array(),
                // Surfaces face the inside of the tube, where the cars are.
                normal: (-radial).to_array(),
                uv: [s as f32 / RING_SEGMENTS as f32, f.distance],
                gravity_blend: f.gravity_blend,
            });
        }
    }

    let mut indices = Vec::with_capacity(n * RING_SEGMENTS * 6);
    for i in 0..n {
        let next = (i + 1) % n;
        for s in 0..RING_SEGMENTS {
            let s_next = (s + 1) % RING_SEGMENTS;
            let a = (i * RING_SEGMENTS + s) as u32;
            let b = (i * RING_SEGMENTS + s_next) as u32;
            let c = (next * RING_SEGMENTS + s_next) as u32;
            let d = (next * RING_SEGMENTS + s) as u32;
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_rotation_is_a_proper_rotation() {
        let forward = Vec3::new(0.3, -0.2, 0.9).normalize();
        let up = Vec3::new(0.1, 0.95, 0.2).normalize();
        let rot = look_rotation(forward, up);

        assert!((rot * Vec3::NEG_Z - forward).length() < 1e-3, "-Z must map to forward");
        assert!((rot * Vec3::Y).dot(up) > 0.9, "+Y must point roughly at up");

        // A rotation preserves handedness: X cross Y must equal Z, not -Z.
        let (x, y, z) = (rot * Vec3::X, rot * Vec3::Y, rot * Vec3::Z);
        assert!((x.cross(y) - z).length() < 1e-3, "basis is mirrored, not rotated");
    }

    /// The start line must be flat and straight. Starting on a slope makes the
    /// track fall out of view, which reads as being perched on top of a hill
    /// rather than standing on a road.
    #[test]
    fn start_line_is_flat_and_straight() {
        for seed in [1u64, 7, 42, 1337, 99999] {
            let track = Track::generate(seed);
            let start = &track.frames[track.start];

            let gradient = start.tangent.y.abs();
            assert!(
                gradient < 0.05,
                "seed {seed}: start line is on a {:.0}% gradient",
                gradient * 100.0
            );

            // And it should stay straight for a few car lengths ahead.
            let n = track.frames.len();
            let ahead = &track.frames[(track.start + 6) % n];
            assert!(
                start.tangent.dot(ahead.tangent) > 0.97,
                "seed {seed}: start line is inside a bend"
            );
        }
    }

    #[test]
    fn spawn_sits_inside_the_tube_facing_along_it() {
        let track = Track::generate(7);
        let (pos, rot) = track.spawn(0.0);
        let surf = track.surface(pos, 0);

        assert!(surf.gap > 0.0, "spawn is outside the tube wall");
        assert!(
            (rot * Vec3::Y).dot(-surf.down) > 0.9,
            "car's roof must point into the tube, not through the wall"
        );
        assert!(
            (rot * Vec3::NEG_Z).dot(surf.tangent).abs() > 0.9,
            "car must face along the track"
        );
    }
}
