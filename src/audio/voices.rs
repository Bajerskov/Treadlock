//! The voices themselves: what an engine, a skid, the wind and a passing car
//! actually sound like. Each one renders a single sample at a time from its own
//! state, which is what lets the mixer run them at any block size.

use super::synth::{
    distance_gain, doppler, glide, pan_gains, BandPass, Harmonics, LowPass, Noise, Osc, Pink,
};

/// Engine note from road speed, through an imaginary six-speed box.
///
/// A note that climbs forever with speed sounds like a slot car. Shifting is
/// what makes it sound like an engine, so the ratio steps down at each change
/// and the note drops with it.
pub fn engine_rpm(speed: f32) -> f32 {
    const SHIFT_AT: [f32; 6] = [16.0, 32.0, 54.0, 82.0, 118.0, 200.0];
    let mut low = 0.0;
    for &top in &SHIFT_AT {
        if speed < top || top == 200.0 {
            return (0.18 + 0.82 * ((speed - low) / (top - low)).clamp(0.0, 1.0)).min(1.0);
        }
        low = top;
    }
    1.0
}

pub struct Engine {
    harmonics: Harmonics<7>,
    grit: BandPass,
    noise: Noise,
    freq: f32,
    tilt: f32,
    level: f32,
}

impl Engine {
    pub fn new(seed: u32) -> Engine {
        Engine {
            harmonics: Harmonics::new(),
            grit: BandPass::default(),
            noise: Noise::new(seed),
            freq: 42.0,
            tilt: 0.0,
            level: 0.0,
        }
    }

    /// `pitch_scale` carries Doppler for opponents; it is 1.0 for the player.
    pub fn render(
        &mut self,
        rpm: f32,
        throttle: f32,
        gain: f32,
        pitch_scale: f32,
        rate: f32,
    ) -> f32 {
        let dt = 1.0 / rate;
        // Glide rather than jump: the scene updates once a frame, and stepping
        // pitch or gain at a block boundary is an audible click.
        let target = (42.0 + 268.0 * rpm.clamp(0.0, 1.0)) * pitch_scale;
        self.freq = glide(self.freq, target, 26.0, dt);
        self.tilt = glide(self.tilt, 0.25 + 0.6 * throttle.clamp(0.0, 1.0), 9.0, dt);
        self.level = glide(self.level, gain, 12.0, dt);

        let tone = self.harmonics.render(self.freq, self.tilt, rate);
        // Combustion is not periodic. A little band-passed noise riding on the
        // second harmonic is the difference between an engine and an organ.
        let grit = self
            .grit
            .process(self.noise.next(), self.freq * 2.0, 1.6, rate)
            * (0.12 + 0.28 * throttle.clamp(0.0, 1.0));
        (tone * 0.8 + grit) * self.level
    }
}

/// Tyre slip. Band-passed noise whose centre rises with how hard the tyres are
/// giving up, so a gentle drift whispers and a full lock-up shrieks.
pub struct Screech {
    filter: BandPass,
    noise: Pink,
    level: f32,
    centre: f32,
}

impl Screech {
    pub fn new(seed: u32) -> Screech {
        Screech {
            filter: BandPass::default(),
            noise: Pink::new(seed),
            level: 0.0,
            centre: 1400.0,
        }
    }

    pub fn render(&mut self, slip: f32, gain: f32, rate: f32) -> f32 {
        let dt = 1.0 / rate;
        let slip = slip.clamp(0.0, 1.0);
        self.level = glide(self.level, slip * gain, 18.0, dt);
        self.centre = glide(self.centre, 1200.0 + 2100.0 * slip, 14.0, dt);
        self.filter.process(self.noise.next(), self.centre, 7.0, rate) * self.level * 2.4
    }
}
/// Airflow over the car. Pink noise opening up with speed, which is the single
/// most effective cue that the car is going fast.
pub struct Wind {
    noise: Pink,
    filter: LowPass,
    cutoff: f32,
    level: f32,
}

impl Wind {
    pub fn new(seed: u32) -> Wind {
        Wind { noise: Pink::new(seed), filter: LowPass::default(), cutoff: 300.0, level: 0.0 }
    }

    pub fn render(&mut self, speed: f32, gain: f32, rate: f32) -> f32 {
        let dt = 1.0 / rate;
        let s = (speed / 180.0).clamp(0.0, 1.4);
        self.cutoff = glide(self.cutoff, 300.0 + 5200.0 * s, 6.0, dt);
        // Loudness rising with the square of speed, as the drag it comes from
        // does, so the top end of the range is where it really arrives.
        self.level = glide(self.level, s * s * gain, 5.0, dt);
        self.filter.process(self.noise.next(), self.cutoff, rate) * self.level * 1.6
    }
}

/// The tube closing overhead. A low resonant hum, faded in by how enclosed the
/// track is, which gives the open banked stretches somewhere to open out to.
pub struct Tunnel {
    low: Osc,
    fifth: Osc,
    level: f32,
}

impl Tunnel {
    pub fn new() -> Tunnel {
        Tunnel { low: Osc::default(), fifth: Osc::default(), level: 0.0 }
    }

    pub fn render(&mut self, enclosure: f32, gain: f32, rate: f32) -> f32 {
        self.level = glide(self.level, enclosure.clamp(0.0, 1.0) * gain, 2.5, 1.0 / rate);
        (self.low.sine(88.0, rate) * 0.7 + self.fifth.sine(132.0, rate) * 0.3) * self.level
    }
}

/// Held for the duration of a boost. Deliberately a drone rather than a
/// one-shot: the point is that the player hears when it ends.
pub struct BoostDrone {
    harmonics: Harmonics<4>,
    level: f32,
}

impl BoostDrone {
    pub fn new() -> BoostDrone {
        BoostDrone { harmonics: Harmonics::new(), level: 0.0 }
    }

    pub fn render(&mut self, amount: f32, gain: f32, rate: f32) -> f32 {
        self.level = glide(self.level, amount.clamp(0.0, 1.0) * gain, 14.0, 1.0 / rate);
        self.harmonics.render(58.0, 0.85, rate) * self.level
    }
}

/// One source somewhere around the listener: an opponent's car, with its engine
/// Doppler-shifted, panned and attenuated by where it is. This is the
/// "swooshing by" - it is not a sample triggered on approach, it is what an
/// engine that is moving past you does on its own.
pub struct Passby {
    engine: Engine,
    air: BandPass,
    noise: Noise,
    gain_l: f32,
    gain_r: f32,
    air_level: f32,
}

impl Passby {
    pub fn new(seed: u32) -> Passby {
        Passby {
            engine: Engine::new(seed),
            air: BandPass::default(),
            noise: Noise::new(seed ^ 0x9e37),
            gain_l: 0.0,
            gain_r: 0.0,
            air_level: 0.0,
        }
    }

    /// `offset` is the source relative to the listener in the listener's own
    /// frame: x right, y up, z forward. `closing` is metres per second of
    /// approach, positive towards.
    pub fn render(&mut self, source: &Source, gain: f32, rate: f32) -> (f32, f32) {
        let dt = 1.0 / rate;
        let distance = source.offset.length();
        let falloff = distance_gain(distance, 14.0);
        let pan = if distance > 0.001 {
            (source.offset.x / distance).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let (l, r) = pan_gains(pan);
        self.gain_l = glide(self.gain_l, l * falloff * gain, 20.0, dt);
        self.gain_r = glide(self.gain_r, r * falloff * gain, 20.0, dt);

        let shift = doppler(source.closing);
        let engine = self.engine.render(source.rpm, source.throttle, 1.0, shift, rate);

        // Displaced air, loudest when the car is both close and quick. Without
        // it a pass reads as an engine that got louder rather than something
        // physically going past.
        let air_target = falloff * (source.speed / 120.0).clamp(0.0, 1.2);
        self.air_level = glide(self.air_level, air_target, 16.0, dt);
        let air = self
            .air
            .process(self.noise.next(), 520.0 * shift, 1.1, rate)
            * self.air_level
            * gain
            * 0.9;

        (engine * self.gain_l + air * l, engine * self.gain_r + air * r)
    }
}

/// A sound source around the listener, in the listener's frame.
#[derive(Clone, Copy, Default)]
pub struct Source {
    pub offset: glam::Vec3,
    /// Metres per second of approach; positive is closing.
    pub closing: f32,
    pub speed: f32,
    pub rpm: f32,
    pub throttle: f32,
    pub active: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cue {
    BoostPickup,
    Impact,
    Respawn,
    Suspension,
    PadCharge,
    WeaponPickup,
    RocketLaunch,
    Explosion,
    ShieldUp,
    ShieldBreak,
}

/// A fire-and-forget sound. Held in a fixed pool, so a pile-up cannot allocate
/// on the audio thread or run the mix away.
pub struct OneShot {
    cue: Cue,
    t: f32,
    strength: f32,
    osc: Osc,
    sweep: Osc,
    noise: Noise,
    filter: LowPass,
    pub active: bool,
}

impl OneShot {
    pub fn new(seed: u32) -> OneShot {
        OneShot {
            cue: Cue::Impact,
            t: 0.0,
            strength: 0.0,
            osc: Osc::default(),
            sweep: Osc::default(),
            noise: Noise::new(seed),
            filter: LowPass::default(),
            active: false,
        }
    }

    pub fn trigger(&mut self, cue: Cue, strength: f32) {
        self.cue = cue;
        self.t = 0.0;
        self.strength = strength.clamp(0.0, 1.0);
        self.active = true;
    }

    fn length(cue: Cue) -> f32 {
        match cue {
            Cue::BoostPickup => 0.55,
            Cue::Impact => 0.40,
            Cue::Respawn => 0.90,
            Cue::Suspension => 0.12,
            Cue::PadCharge => 0.30,
            Cue::WeaponPickup => 0.35,
            Cue::RocketLaunch => 0.60,
            Cue::Explosion => 0.70,
            Cue::ShieldUp => 0.45,
            Cue::ShieldBreak => 0.50,
        }
    }

    pub fn render(&mut self, gain: f32, rate: f32) -> f32 {
        if !self.active {
            return 0.0;
        }
        let t = self.t;
        self.t += 1.0 / rate;
        if self.t >= Self::length(self.cue) {
            self.active = false;
        }

        let out = match self.cue {
            // Three rising notes, so it reads as a reward rather than an alarm.
            Cue::BoostPickup => {
                let step = (t / 0.09).floor().min(3.0);
                let freq = 520.0 * 2f32.powf(step * 3.0 / 12.0);
                let env = (-(t % 0.09) * 26.0).exp() * (1.0 - t / 0.55).max(0.0);
                let swell = self.filter.process(self.noise.next(), 1800.0, rate) * 0.5 * env;
                self.osc.sine(freq, rate) * env * 0.7 + swell
            }
            // A transient with a body under it. Noise alone is a hiss, the
            // thump alone is a drum; together it is a hit.
            Cue::Impact => {
                let env = (-t * 16.0).exp();
                let crack = self.filter.process(self.noise.next(), 2600.0 - t * 5000.0, rate);
                let thump = self.sweep.sine(90.0 - t * 90.0, rate);
                (crack * 0.7 + thump * 0.8) * env * (0.35 + 0.65 * self.strength)
            }
            Cue::Respawn => {
                let half = 0.45;
                let freq = if t < half {
                    700.0 - 520.0 * (t / half)
                } else {
                    180.0 + 620.0 * ((t - half) / half)
                };
                let env = (1.0 - (t / 0.9 * 2.0 - 1.0).abs()).max(0.0);
                self.osc.sine(freq, rate) * env * 0.55
            }
            Cue::Suspension => {
                let env = (-t * 45.0).exp();
                self.filter.process(self.noise.next(), 900.0, rate) * env * 1.4
            }
            Cue::PadCharge => {
                let env = (-t * 10.0).exp();
                self.osc.sine(340.0, rate) * env * 0.25
            }
            // Two quick rising notes: collected, not fired.
            Cue::WeaponPickup => {
                let step = (t / 0.12).floor().min(1.0);
                let env = (-(t % 0.12) * 22.0).exp() * (1.0 - t / 0.35).max(0.0);
                self.osc.sine(680.0 * 2f32.powf(step * 5.0 / 12.0), rate) * env * 0.6
            }
            // Noise swept upward by a resonant filter, so it leaves rather than
            // arrives. The pitch rising is what separates a launch from a hit.
            Cue::RocketLaunch => {
                let env = (1.0 - t / 0.6).max(0.0);
                let sweep = 260.0 + 1800.0 * (t / 0.6);
                let body = self.filter.process(self.noise.next(), sweep, rate);
                let whistle = self.sweep.sine(sweep * 2.0, rate) * 0.25;
                (body * 1.6 + whistle) * env * env
            }
            // A crack, a body and a tail, in that order. Any one alone reads as
            // a click, a drum or a hiss.
            Cue::Explosion => {
                let env = (-t * 5.0).exp();
                let crack = self.noise.next() * (-t * 40.0).exp();
                let body = self.filter.process(self.noise.next(), 1400.0 - t * 1600.0, rate);
                let thump = self.sweep.sine(120.0 - t * 110.0, rate);
                (crack * 0.5 + body * 0.8 + thump * 0.9) * env * (0.4 + 0.6 * self.strength)
            }
            // Rising and settling: something closing around you.
            Cue::ShieldUp => {
                let env = (1.0 - t / 0.45).max(0.0);
                let freq = 220.0 + 520.0 * (t / 0.45).min(1.0);
                (self.osc.sine(freq, rate) * 0.5 + self.sweep.sine(freq * 1.5, rate) * 0.25) * env
            }
            // The same shape inverted, which is what makes it read as loss.
            Cue::ShieldBreak => {
                let env = (-t * 7.0).exp();
                let freq = 740.0 - 560.0 * (t / 0.5).min(1.0);
                let shatter = self.filter.process(self.noise.next(), 3000.0, rate) * 0.5;
                (self.osc.sine(freq, rate) * 0.55 + shatter) * env
            }
        };
        out * gain * 0.8
    }
}

/// A procedural music bed, seeded from the track.
///
/// This is the fallback for `music_race`, and it exists because silence while
/// the real track is being written makes the game feel unfinished for reasons
/// that have nothing to do with the racing. Drop a file in and this stops.
pub struct Music {
    step: u32,
    samples_into_step: f32,
    root: f32,
    pattern: [u8; 16],
    bass: Harmonics<3>,
    lead: Harmonics<4>,
    kick: Osc,
    noise: Noise,
    hat: LowPass,
    bass_env: f32,
    lead_env: f32,
    kick_t: f32,
    hat_t: f32,
    level: f32,
}

/// Minor pentatonic. Every pair of degrees in it is consonant, so a pattern
/// picked at random from a seed cannot come out wrong.
const SCALE: [f32; 5] = [0.0, 3.0, 5.0, 7.0, 10.0];

impl Music {
    pub fn new(seed: u64) -> Music {
        let mut rng = Noise::new(seed as u32 ^ 0x5bd1);
        let mut pattern = [0u8; 16];
        for slot in pattern.iter_mut() {
            *slot = ((rng.next() * 0.5 + 0.5) * 5.0) as u8 % 5;
        }
        // A root between A and E, so different tracks sound different without
        // any of them landing somewhere uncomfortable.
        let root = 55.0 * 2f32.powf(((rng.next() * 0.5 + 0.5) * 7.0).floor() / 12.0);
        Music {
            step: 0,
            samples_into_step: 0.0,
            root,
            pattern,
            bass: Harmonics::new(),
            lead: Harmonics::new(),
            kick: Osc::default(),
            noise: Noise::new(seed as u32 ^ 0x1234),
            hat: LowPass::default(),
            bass_env: 0.0,
            lead_env: 0.0,
            kick_t: 1.0,
            hat_t: 1.0,
            level: 0.0,
        }
    }

    pub fn render(&mut self, gain: f32, rate: f32) -> f32 {
        const BPM: f32 = 148.0;
        let step_seconds = 60.0 / BPM / 4.0;
        self.level = glide(self.level, gain, 1.5, 1.0 / rate);

        self.samples_into_step += 1.0 / rate;
        if self.samples_into_step >= step_seconds {
            self.samples_into_step -= step_seconds;
            self.step = (self.step + 1) % 16;
            let s = self.step;
            if s % 4 == 0 {
                self.kick_t = 0.0;
            }
            if s % 4 == 2 {
                self.hat_t = 0.0;
            }
            if s % 2 == 0 {
                self.bass_env = 1.0;
            }
            self.lead_env = 1.0;
        }

        let decay = |v: &mut f32, k: f32| {
            *v *= (-k / rate).exp();
            *v
        };
        let degree = SCALE[self.pattern[self.step as usize] as usize];
        let bass_freq = self.root * 2f32.powf(SCALE[(self.step / 8) as usize % 5] / 12.0);
        let lead_freq = self.root * 4.0 * 2f32.powf(degree / 12.0);

        let bass = self.bass.render(bass_freq, 0.5, rate) * decay(&mut self.bass_env, 7.0) * 0.30;
        let lead = self.lead.render(lead_freq, 0.7, rate) * decay(&mut self.lead_env, 16.0) * 0.14;

        self.kick_t = (self.kick_t + 1.0 / rate).min(1.0);
        let kick = if self.kick_t < 0.18 {
            self.kick.sine(150.0 - 110.0 * (self.kick_t / 0.18), rate)
                * (-self.kick_t * 22.0).exp()
                * 0.55
        } else {
            0.0
        };

        self.hat_t = (self.hat_t + 1.0 / rate).min(1.0);
        let hat = if self.hat_t < 0.06 {
            let raw = self.noise.next() - self.hat.process(self.noise.next(), 4000.0, rate);
            raw * (-self.hat_t * 70.0).exp() * 0.12
        } else {
            0.0
        };

        (bass + lead + kick + hat) * self.level
    }
}
