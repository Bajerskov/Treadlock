//! Background plates: the flat imagery behind the track.
//!
//! A plate is an RGBA layer in equirectangular space - x is the compass
//! bearing, y runs from the zenith down past the horizon. Several are stacked
//! into one sky texture, so the whole backdrop costs a single draw with no
//! blending state.
//!
//! Each layer is generated from the track seed if its file is absent, so the
//! game has a sky from the first run and a supplied PNG replaces exactly one
//! layer without disturbing the others.

use crate::assets::Library;

/// Horizon height as a fraction of the plate. Below the middle, because most of
/// what is worth seeing is above it.
const HORIZON: f32 = 0.58;

#[derive(Clone)]
pub struct Plate {
    pub width: u32,
    pub height: u32,
    /// Straight (non-premultiplied) RGBA8.
    pub rgba: Vec<u8>,
}

impl Plate {
    pub fn new(width: u32, height: u32) -> Plate {
        Plate { width, height, rgba: vec![0; (width * height * 4) as usize] }
    }

    fn set(&mut self, x: u32, y: u32, colour: [f32; 4]) {
        let i = ((y * self.width + x) * 4) as usize;
        for c in 0..4 {
            self.rgba[i + c] = (colour[c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }

    /// Source-over composite of `top` onto self. Layers are authored at the
    /// same size, so a mismatch is a mistake rather than something to paper
    /// over by resampling.
    pub fn over(&mut self, top: &Plate) {
        if top.width != self.width || top.height != self.height {
            eprintln!(
                "plates: layer is {}x{} but the sky is {}x{}; skipping it",
                top.width, top.height, self.width, self.height
            );
            return;
        }
        for i in (0..self.rgba.len()).step_by(4) {
            let a = top.rgba[i + 3] as u32;
            if a == 0 {
                continue;
            }
            for c in 0..3 {
                let src = top.rgba[i + c] as u32;
                let dst = self.rgba[i + c] as u32;
                self.rgba[i + c] = ((src * a + dst * (255 - a)) / 255) as u8;
            }
            self.rgba[i + 3] = 255;
        }
    }
}

/// Deterministic value noise on a ring, so the left and right edges of the
/// plate meet: the sky wraps all the way round and a seam would be a visible
/// vertical line at one compass bearing.
fn ring_noise(x: f32, seed: u64, harmonics: u32) -> f32 {
    let mut rng = crate::track::seed_rng(seed);
    let mut sum = 0.0;
    let mut weight = 0.0;
    for k in 1..=harmonics {
        let amplitude = 1.0 / k as f32;
        let phase = rng() * std::f32::consts::TAU;
        sum += (x * std::f32::consts::TAU * k as f32 + phase).sin() * amplitude;
        weight += amplitude;
    }
    sum / weight
}

/// The base layer: a vertical gradient from zenith to ground, hue shifted by
/// the seed so two tracks do not share a sky.
pub fn sky_gradient(seed: u64, width: u32, height: u32) -> Plate {
    let mut rng = crate::track::seed_rng(seed ^ 0xA17);
    let hue = rng();
    let zenith = [0.05 + 0.10 * hue, 0.07, 0.20 + 0.14 * (1.0 - hue), 1.0];
    let horizon = [0.55 + 0.35 * hue, 0.30 + 0.15 * hue, 0.28, 1.0];
    let ground = [0.04, 0.035, 0.05, 1.0];

    let mut plate = Plate::new(width, height);
    for y in 0..height {
        let t = y as f32 / (height - 1).max(1) as f32;
        let colour = if t < HORIZON {
            // Squared so the warm band hugs the horizon instead of washing the
            // whole upper sky out.
            let k = (t / HORIZON).powf(2.2);
            blend(zenith, horizon, k)
        } else {
            blend(horizon, ground, ((t - HORIZON) / (1.0 - HORIZON) * 3.0).min(1.0))
        };
        for x in 0..width {
            plate.set(x, y, colour);
        }
    }
    plate
}

/// A silhouette ridge along the horizon. Two of these at different heights and
/// darknesses are what give the backdrop depth.
pub fn ridge(seed: u64, width: u32, height: u32, scale: f32, colour: [f32; 3]) -> Plate {
    let mut plate = Plate::new(width, height);
    let horizon = HORIZON * height as f32;
    for x in 0..width {
        let u = x as f32 / width as f32;
        // Two octaves: the low one makes massifs, the high one makes peaks.
        let profile = ring_noise(u, seed, 4) * 0.7 + ring_noise(u, seed ^ 0x99, 11) * 0.3;
        let top = horizon - (0.20 + 0.80 * (profile * 0.5 + 0.5)) * scale * height as f32;
        for y in 0..height {
            if (y as f32) < top {
                continue;
            }
            // Fade the base into the horizon haze rather than ending on a hard
            // line, which reads as distance.
            let depth = ((y as f32 - top) / (horizon - top).max(1.0)).clamp(0.0, 1.0);
            let alpha = 0.35 + 0.65 * depth;
            plate.set(x, y, [colour[0], colour[1], colour[2], alpha]);
        }
    }
    plate
}

/// A soft band of cloud across the upper sky.
pub fn clouds(seed: u64, width: u32, height: u32) -> Plate {
    let mut plate = Plate::new(width, height);
    for x in 0..width {
        let u = x as f32 / width as f32;
        let drift = ring_noise(u, seed ^ 0x5C1, 7);
        for y in 0..height {
            let v = y as f32 / (height - 1).max(1) as f32;
            if v > HORIZON {
                continue;
            }
            let band = 1.0 - ((v / HORIZON - 0.42 - drift * 0.12) / 0.20).abs();
            let density = (band * (0.55 + 0.45 * ring_noise(u * 3.0 + v, seed ^ 0x77, 9)))
                .clamp(0.0, 1.0);
            if density > 0.02 {
                plate.set(x, y, [0.75, 0.72, 0.78, density * 0.40]);
            }
        }
    }
    plate
}

fn blend(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

/// Load a plate from the library, falling back to `generated` when the file is
/// absent or unreadable. A file that is the wrong size is resampled rather than
/// rejected, because a plate is scenery and nothing depends on its resolution.
fn layer(library: &Library, id: &str, width: u32, height: u32, generated: impl FnOnce() -> Plate) -> Plate {
    let Some(path) = library.path(id) else {
        return generated();
    };
    match image::open(path) {
        Ok(loaded) => {
            let resized = image::imageops::resize(
                &loaded.to_rgba8(),
                width,
                height,
                image::imageops::FilterType::CatmullRom,
            );
            Plate { width, height, rgba: resized.into_raw() }
        }
        Err(e) => {
            eprintln!("plates: {} could not be read ({e}); generating {id}", path.display());
            generated()
        }
    }
}

/// The finished sky: every layer stacked, ready to upload as one texture.
pub fn backdrop(library: &Library, seed: u64, width: u32, height: u32) -> Plate {
    let mut sky = layer(library, "sky_gradient", width, height, || {
        sky_gradient(seed, width, height)
    });
    sky.over(&layer(library, "cloud_band", width, height, || clouds(seed, width, height)));
    // Far ridge first, then the nearer and darker one in front of it.
    sky.over(&layer(library, "backdrop_far", width, height, || {
        ridge(seed, width, height, 0.10, [0.20, 0.20, 0.30])
    }));
    sky.over(&layer(library, "backdrop_near", width, height, || {
        ridge(seed ^ 0xBEEF, width, height, 0.17, [0.07, 0.07, 0.12])
    }));
    sky
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sky wraps all the way round, so the two edges of the plate are the
    /// same compass bearing. A seam there is a vertical line hanging in the
    /// sky, which is obvious in motion and invisible in a still.
    #[test]
    fn the_backdrop_has_no_seam_where_it_wraps() {
        let library = Library::load("assets");
        let plate = backdrop(&library, 7, 512, 256);
        let mut worst = 0i32;
        for y in 0..plate.height {
            let left = ((y * plate.width) * 4) as usize;
            let right = ((y * plate.width + plate.width - 1) * 4) as usize;
            for c in 0..3 {
                let d = plate.rgba[left + c] as i32 - plate.rgba[right + c] as i32;
                worst = worst.max(d.abs());
            }
        }
        assert!(worst < 24, "the sky seams at the wrap: worst channel gap {worst}");
    }

    /// Fully opaque, or the dome shows the void behind it.
    #[test]
    fn the_backdrop_is_opaque_everywhere() {
        let plate = backdrop(&Library::load("assets"), 42, 256, 128);
        assert!(plate.rgba.chunks(4).all(|p| p[3] == 255), "the sky has transparent pixels");
    }

    /// Ground below, sky above. Getting this upside down is the kind of thing
    /// that has cost this project a week already.
    #[test]
    fn the_sky_is_brighter_at_the_top_than_at_the_bottom() {
        let plate = backdrop(&Library::load("assets"), 7, 128, 128);
        let row = |y: u32| -> f32 {
            (0..plate.width)
                .map(|x| {
                    let i = ((y * plate.width + x) * 4) as usize;
                    (plate.rgba[i] as f32 + plate.rgba[i + 1] as f32 + plate.rgba[i + 2] as f32)
                        / 765.0
                })
                .sum::<f32>()
                / plate.width as f32
        };
        assert!(
            row(4) > row(plate.height - 4),
            "the ground is brighter than the sky, so the backdrop is upside down"
        );
    }

    /// Two seeds must not produce the same sky, or the seed is decorative.
    #[test]
    fn different_seeds_give_different_skies() {
        let library = Library::load("assets");
        let a = backdrop(&library, 1, 128, 64);
        let b = backdrop(&library, 2, 128, 64);
        assert!(a.rgba != b.rgba, "two seeds produced an identical sky");
    }

    /// A supplied layer has to actually replace the generated one, or dropping
    /// a PNG in does nothing and the manifest is a lie.
    #[test]
    fn compositing_puts_later_layers_in_front() {
        let mut base = Plate::new(2, 1);
        base.set(0, 0, [0.0, 0.0, 0.0, 1.0]);
        base.set(1, 0, [0.0, 0.0, 0.0, 1.0]);

        let mut top = Plate::new(2, 1);
        top.set(0, 0, [1.0, 1.0, 1.0, 1.0]);
        // Second pixel left fully transparent.
        base.over(&top);

        assert_eq!(&base.rgba[0..4], &[255, 255, 255, 255], "an opaque layer did not cover");
        assert_eq!(&base.rgba[4..8], &[0, 0, 0, 255], "a transparent pixel painted anyway");
    }
}
