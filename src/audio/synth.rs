//! Signal generators, filters and voices.
//!
//! Everything here is a pure function of its own state and the sample rate. No
//! device, no clock, no allocation while running, so the whole sound of the
//! game can be rendered and measured in a test on a machine with no sound card.
//!
//! Tones are built by adding sine partials rather than by clipping a sawtooth.
//! A naive saw at engine pitch folds its upper harmonics back down as aliasing,
//! which is the metallic buzz that makes cheap synthesised engines sound cheap.
//! Partials above Nyquist are simply not summed, so that cannot happen.

use std::f32::consts::{PI, TAU};

/// Speed of sound, for Doppler. Sea level, near enough.
const SOUND_SPEED: f32 = 343.0;

/// Deterministic white noise, in -1..1.
pub struct Noise {
    state: u32,
}

impl Noise {
    pub fn new(seed: u32) -> Noise {
        // xorshift is dead at zero, and seeds that differ only in low bits
        // decorrelate slowly, so spread the seed first.
        Noise { state: seed.wrapping_mul(2_654_435_761).max(1) }
    }

    pub fn next(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Pink noise: white tilted by -3 dB per octave. Wind and tyre noise are pink
/// in the real world, and white noise in their place reads as hiss.
///
/// Paul Kellett's three-pole approximation, accurate to about a decibel across
/// the audible range, which is far closer than the ear needs.
pub struct Pink {
    noise: Noise,
    b: [f32; 3],
}

impl Pink {
    pub fn new(seed: u32) -> Pink {
        Pink { noise: Noise::new(seed), b: [0.0; 3] }
    }

    pub fn next(&mut self) -> f32 {
        let white = self.noise.next();
        self.b[0] = 0.99765 * self.b[0] + white * 0.0990460;
        self.b[1] = 0.96300 * self.b[1] + white * 0.2965164;
        self.b[2] = 0.57000 * self.b[2] + white * 1.0526913;
        (self.b[0] + self.b[1] + self.b[2] + white * 0.1848) * 0.2
    }
}

/// One-pole low-pass, cutoff in Hz and settable per sample.
#[derive(Default)]
pub struct LowPass {
    y: f32,
}

impl LowPass {
    pub fn process(&mut self, x: f32, cutoff: f32, rate: f32) -> f32 {
        let c = cutoff.clamp(1.0, rate * 0.45);
        let a = 1.0 - (-TAU * c / rate).exp();
        self.y += a * (x - self.y);
        self.y
    }
}

/// Chamberlin state-variable filter. The band-pass output is what turns noise
/// into a screech or a whoosh: the centre frequency is the "note" of the sound
/// even though there is no oscillator in it.
#[derive(Default)]
pub struct BandPass {
    low: f32,
    band: f32,
}

impl BandPass {
    pub fn process(&mut self, x: f32, cutoff: f32, q: f32, rate: f32) -> f32 {
        // This topology goes unstable above about a quarter of the sample rate,
        // so the cutoff is clamped there rather than at Nyquist.
        let f = 2.0 * (PI * cutoff.clamp(20.0, rate * 0.24) / rate).sin();
        let damp = (1.0 / q.max(0.5)).min(2.0);
        self.low += f * self.band;
        let high = x - self.low - damp * self.band;
        self.band += f * high;
        self.band
    }
}

/// Phase accumulator held in turns rather than radians, so wrapping is exact
/// and a swept pitch cannot drift.
#[derive(Default, Clone, Copy)]
pub struct Osc {
    phase: f32,
}

impl Osc {
    pub fn sine(&mut self, freq: f32, rate: f32) -> f32 {
        self.phase = (self.phase + freq / rate).fract();
        (self.phase * TAU).sin()
    }
}

/// A stack of sine partials: the building block for every pitched sound here.
pub struct Harmonics<const N: usize> {
    oscs: [Osc; N],
}

impl<const N: usize> Harmonics<N> {
    pub fn new() -> Harmonics<N> {
        Harmonics { oscs: [Osc::default(); N] }
    }

    /// `tilt` shifts energy towards the upper partials, which is what an engine
    /// under load does. 0 is soft and hollow, 1 is bright and hard.
    pub fn render(&mut self, fundamental: f32, tilt: f32, rate: f32) -> f32 {
        let nyquist = rate * 0.5;
        let mut sum = 0.0;
        let mut norm = 0.0;
        for (i, osc) in self.oscs.iter_mut().enumerate() {
            let n = i as f32 + 1.0;
            let freq = fundamental * n;
            if freq >= nyquist {
                break;
            }
            let amp = (1.0 / n).powf(1.6 - tilt.clamp(0.0, 1.0));
            sum += osc.sine(freq, rate) * amp;
            norm += amp;
        }
        if norm > 0.0 {
            sum / norm
        } else {
            0.0
        }
    }
}

/// How much a pitch is shifted by a source closing on the listener at
/// `closing` metres per second. Positive closing means approaching, and an
/// approaching source is pitched up.
///
/// Clamped well inside the point where the denominator would vanish: cars here
/// reach speeds where the untruncated formula would scream or invert.
pub fn doppler(closing: f32) -> f32 {
    (SOUND_SPEED / (SOUND_SPEED - closing.clamp(-260.0, 260.0))).clamp(0.55, 2.2)
}

/// Equal-power stereo pan. `pan` is -1 hard left to +1 hard right. Equal power
/// rather than linear, or a source crossing the middle dips in loudness.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * (PI / 4.0);
    (angle.cos(), angle.sin())
}

/// Inverse-square-ish falloff, softened near zero so a source passing through
/// the listener does not blow up.
pub fn distance_gain(distance: f32, reference: f32) -> f32 {
    let d = (distance / reference.max(0.01)).max(0.0);
    1.0 / (1.0 + d * d)
}

/// Frame-rate independent approach, for parameters that must not step.
/// A raw assignment of gain or pitch per audio block is an audible click.
pub fn glide(current: f32, target: f32, rate_per_second: f32, dt: f32) -> f32 {
    current + (target - current) * (1.0 - (-rate_per_second * dt).exp())
}

/// Soft clip. The mixer sums a dozen voices, and on a loud frame that can pass
/// 1.0; hard clipping there is a crack, this is a squash.
pub fn soft_clip(x: f32) -> f32 {
    x.tanh()
}
