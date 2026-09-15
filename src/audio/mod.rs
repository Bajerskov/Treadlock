//! Game audio.
//!
//! The mixer is pure: it turns a `Scene` - a snapshot of what is happening
//! around the listener - into interleaved samples, with no reference to a sound
//! card or a clock. That is deliberate. It means the entire soundtrack of a race
//! can be rendered and measured in a test on a build machine with no audio
//! hardware, which is where this was written.
//!
//! The device layer behind it is thin enough to be obviously correct, and is
//! behind the `audio` feature so the game still builds where the platform's
//! audio development headers are absent.

pub mod synth;
pub mod voices;
pub mod sound;

use std::sync::{Arc, Mutex};

use glam::Vec3;

use crate::camera::Camera;
use crate::sim::Sim;
pub use voices::{Cue, Source};
use voices::{BoostDrone, Engine, Music, OneShot, Passby, Screech, Tunnel, Wind};

/// One per opponent, with room to spare.
pub const MAX_SOURCES: usize = 8;
/// A pile-up fires several at once; beyond this the oldest is stolen rather
/// than the mix being allowed to grow without limit.
const MAX_ONESHOTS: usize = 12;

/// What the listener can hear, as of the last frame.
#[derive(Clone, Copy)]
pub struct Scene {
    pub rpm: f32,
    pub throttle: f32,
    pub slip: f32,
    pub speed: f32,
    pub boost: f32,
    /// 1 where the track is a closed tube, 0 on the open banked stretches.
    pub enclosure: f32,
    pub sources: [Source; MAX_SOURCES],
    /// Fades the music in and out without touching the master level.
    pub music: f32,
}

impl Default for Scene {
    fn default() -> Scene {
        Scene {
            rpm: 0.0,
            throttle: 0.0,
            slip: 0.0,
            speed: 0.0,
            boost: 0.0,
            enclosure: 1.0,
            sources: [Source::default(); MAX_SOURCES],
            music: 1.0,
        }
    }
}

/// Per-group gains, so music can be turned down without silencing the car.
#[derive(Clone, Copy)]
pub struct Levels {
    pub master: f32,
    pub engine: f32,
    pub effects: f32,
    pub ambient: f32,
    pub music: f32,
}

impl Default for Levels {
    fn default() -> Levels {
        Levels { master: 0.8, engine: 0.55, effects: 0.7, ambient: 0.4, music: 0.35 }
    }
}

impl From<crate::settings::Audio> for Levels {
    fn from(a: crate::settings::Audio) -> Levels {
        Levels {
            // Mute zeroes the master and leaves every other level alone, so
            // unmuting restores the balance rather than a default.
            master: if a.muted { 0.0 } else { a.master },
            engine: a.engine,
            effects: a.effects,
            ambient: a.ambient,
            music: a.music,
        }
    }
}

/// Every track found in the music folder, played in turn.
///
/// Scanning a folder rather than naming files in the manifest is deliberate:
/// adding music should be dropping a file in, not editing a text file as well.
pub struct Playlist {
    tracks: Vec<sound::Clip>,
    current: usize,
    frame: f64,
}

impl Playlist {
    pub fn new() -> Playlist {
        Playlist { tracks: Vec::new(), current: 0, frame: 0.0 }
    }

    // Built directly, for the tests. The game scans a folder.
    #[allow(dead_code)]
    pub fn from_tracks(tracks: Vec<sound::Clip>) -> Playlist {
        Playlist { tracks, current: 0, frame: 0.0 }
    }

    /// Load every audio file in `folder`, sorted by name so the running order
    /// is the player's to choose by naming them.
    pub fn load(folder: &std::path::Path) -> Playlist {
        let mut paths: Vec<std::path::PathBuf> = match std::fs::read_dir(folder) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && sound::is_audio(p))
                .collect(),
            // No folder is the normal case before anyone adds music.
            Err(_) => Vec::new(),
        };
        paths.sort();

        let mut playlist = Playlist::new();
        for path in paths {
            match sound::load(&path) {
                Ok(clip) => {
                    println!(
                        "music: {} ({:.0}:{:02.0})",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        (clip.seconds() / 60.0).floor(),
                        clip.seconds() % 60.0
                    );
                    playlist.tracks.push(clip);
                }
                // One unreadable file should not cost the player the rest of
                // their music.
                Err(e) => eprintln!("music: {e}"),
            }
        }
        playlist
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// The next stereo frame, advancing to the following track at the end of
    /// each one and round to the first at the end of the list.
    fn next_frame(&mut self, device_rate: f32) -> (f32, f32) {
        if self.tracks.is_empty() {
            return (0.0, 0.0);
        }
        // A track that failed to decode to anything would otherwise spin here.
        let mut tried = 0;
        while self.tracks[self.current].frames() < 2 {
            self.current = (self.current + 1) % self.tracks.len();
            self.frame = 0.0;
            tried += 1;
            if tried > self.tracks.len() {
                return (0.0, 0.0);
            }
        }

        let clip = &self.tracks[self.current];
        let out = clip.sample(self.frame as f32);
        self.frame += (clip.rate / device_rate) as f64;
        if self.frame >= (clip.frames() - 1) as f64 {
            self.current = (self.current + 1) % self.tracks.len();
            self.frame = 0.0;
        }
        out
    }
}

pub struct Mixer {
    rate: f32,
    scene: Scene,
    pub levels: Levels,
    engine: Engine,
    screech: Screech,
    wind: Wind,
    tunnel: Tunnel,
    boost: BoostDrone,
    passbys: Vec<Passby>,
    oneshots: Vec<OneShot>,
    next_oneshot: usize,
    music: Music,
    /// Recorded music, if any was found. A non-empty playlist means the
    /// generated bed stands down.
    playlist: Playlist,
}

impl Mixer {
    pub fn new(rate: f32, seed: u64) -> Mixer {
        let seed32 = seed as u32;
        Mixer {
            rate,
            scene: Scene::default(),
            levels: Levels::default(),
            engine: Engine::new(seed32 ^ 0x01),
            screech: Screech::new(seed32 ^ 0x02),
            wind: Wind::new(seed32 ^ 0x03),
            tunnel: Tunnel::new(),
            boost: BoostDrone::new(),
            passbys: (0..MAX_SOURCES).map(|i| Passby::new(seed32 ^ (0x100 + i as u32))).collect(),
            oneshots: (0..MAX_ONESHOTS).map(|i| OneShot::new(seed32 ^ (0x200 + i as u32))).collect(),
            next_oneshot: 0,
            music: Music::new(seed),
            playlist: Playlist::new(),
        }
    }

    pub fn set_scene(&mut self, scene: Scene) {
        self.scene = scene;
    }

    /// Take a free slot if there is one, and otherwise steal round-robin. A
    /// dropped collision sound is better than an unbounded voice count.
    pub fn fire(&mut self, cue: Cue, strength: f32) {
        let slot = self
            .oneshots
            .iter()
            .position(|s| !s.active)
            .unwrap_or_else(|| {
                let s = self.next_oneshot;
                self.next_oneshot = (self.next_oneshot + 1) % MAX_ONESHOTS;
                s
            });
        self.oneshots[slot].trigger(cue, strength);
    }

    // A single clip, for the tests. The game loads a folder.
    #[allow(dead_code)]
    pub fn attach_music(&mut self, clip: sound::Clip) {
        self.playlist = Playlist { tracks: vec![clip], current: 0, frame: 0.0 };
    }

    pub fn attach_playlist(&mut self, playlist: Playlist) {
        self.playlist = playlist;
    }

    fn music_sample(&mut self) -> (f32, f32) {
        let gain = self.levels.music * self.scene.music;
        if self.playlist.len() > 0 {
            let (l, r) = self.playlist.next_frame(self.rate);
            return (l * gain, r * gain);
        }
        let m = self.music.render(gain, self.rate);
        (m, m)
    }

    /// Render `out` as interleaved frames of `channels`. Mono sums the pair;
    /// more than two channels get the stereo pair and silence elsewhere, which
    /// is wrong for surround but never wrong in a way that is loud.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        let channels = channels.max(1);
        let rate = self.rate;
        let levels = self.levels;
        let scene = self.scene;

        for frame in out.chunks_mut(channels) {
            let engine = self.engine.render(scene.rpm, scene.throttle, levels.engine, 1.0, rate);
            let screech = self.screech.render(scene.slip, levels.effects, rate);
            let wind = self.wind.render(scene.speed, levels.ambient, rate);
            let tunnel = self.tunnel.render(scene.enclosure, levels.ambient * 0.6, rate);
            let boost = self.boost.render(scene.boost, levels.effects, rate);

            let mut left = engine + screech + wind + tunnel + boost;
            let mut right = left;

            for (passby, source) in self.passbys.iter_mut().zip(scene.sources.iter()) {
                if !source.active {
                    continue;
                }
                let (l, r) = passby.render(source, levels.engine * 0.85, rate);
                left += l;
                right += r;
            }

            for shot in self.oneshots.iter_mut() {
                let s = shot.render(levels.effects, rate);
                left += s;
                right += s;
            }

            let (ml, mr) = self.music_sample();
            left = synth::soft_clip((left + ml) * levels.master);
            right = synth::soft_clip((right + mr) * levels.master);

            match channels {
                1 => frame[0] = (left + right) * 0.5,
                _ => {
                    frame[0] = left;
                    frame[1] = right;
                    for extra in frame.iter_mut().skip(2) {
                        *extra = 0.0;
                    }
                }
            }
        }
    }
}

/// Reads the race and describes it to the mixer.
///
/// Positions are put in the camera's frame rather than the car's, because the
/// camera is where the player is listening from. Chasing the car from behind
/// and hearing an opponent overtake on the left is only right if left means
/// left on screen.
pub fn observe(sim: &Sim, camera: &Camera, throttle: f32) -> Scene {
    let listener = camera.pos;
    let forward = (camera.target - camera.pos).normalize_or(Vec3::NEG_Z);
    // The camera's own up is only roughly square to its forward: it is lerped
    // towards the tube interior every frame and lands wherever that leaves it.
    // Projecting onto a basis that is not orthonormal skews the distances, and
    // distance is what the falloff and the Doppler are computed from, so this
    // re-squares it exactly as the view matrix does.
    let (right, up) = camera.basis();
    let ear_velocity = sim.player.vel;

    let surf = sim.track.surface(sim.player.pos, sim.player.hint);
    let slip = sim
        .player
        .wheels
        .iter()
        .filter(|w| w.contact)
        .map(|w| w.slip)
        .fold(0.0f32, f32::max);

    let mut sources = [Source::default(); MAX_SOURCES];
    for (slot, opponent) in sources.iter_mut().zip(sim.opponents.iter()) {
        let to = opponent.car.pos - listener;
        let relative = opponent.car.vel - ear_velocity;
        let distance = to.length();
        *slot = Source {
            offset: Vec3::new(to.dot(right), to.dot(up), to.dot(forward)),
            // Rate of approach is the relative velocity along the line between
            // them. Using raw speed instead would Doppler a car that is
            // overtaking on a parallel line, which never happens in life.
            closing: if distance > 0.001 {
                -relative.dot(to / distance)
            } else {
                0.0
            },
            speed: opponent.car.speed(),
            rpm: voices::engine_rpm(opponent.car.speed()),
            throttle: 0.8,
            // Past a couple of hundred metres the gain is inaudible and the
            // voice is only costing cycles.
            active: distance < 220.0,
        };
    }

    Scene {
        rpm: voices::engine_rpm(sim.player.speed()),
        throttle,
        slip: (slip / 12.0).clamp(0.0, 1.0),
        speed: sim.player.speed(),
        boost: if sim.player.pad_boost > 0.0 { 1.0 } else { sim.player.boost },
        enclosure: sim.track.frames[surf.index].gravity_blend,
        sources,
        music: 1.0,
    }
}

/// Watches the race for the moments worth a sound.
///
/// Edge detection lives here rather than being sprinkled through the game loop,
/// so adding a cue does not mean finding the one place in `main` that knows a
/// boost pad was just crossed.
pub struct Cues {
    velocity: Vec3,
    pad_boost: f32,
    compression: f32,
    /// Which pad the approach tone last played for, so it plays once per pad
    /// rather than every frame the pad is in range.
    charged_pad: Option<usize>,
    started: bool,
}

/// How far out a boost pad announces itself.
const PAD_EARSHOT: f32 = 70.0;

impl Cues {
    pub fn new() -> Cues {
        Cues {
            velocity: Vec3::ZERO,
            pad_boost: 0.0,
            compression: 0.0,
            charged_pad: None,
            started: false,
        }
    }

    pub fn poll(&mut self, sim: &Sim, audio: &Audio, dt: f32) {
        let car = &sim.player;
        let compression = car.wheels.iter().map(|w| w.compression).fold(0.0f32, f32::max);

        // The nearest pad within earshot gets an approach tone, so a boost is
        // something to aim for rather than something that happens to you.
        let nearest = sim
            .track
            .boost_pads
            .iter()
            .enumerate()
            .map(|(i, pad)| (i, pad.pos.distance_squared(car.pos)))
            .filter(|(_, d2)| *d2 < PAD_EARSHOT * PAD_EARSHOT)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        if nearest != self.charged_pad {
            if let Some(index) = nearest {
                if self.started {
                    audio.fire(Cue::PadCharge, 1.0);
                }
                self.charged_pad = Some(index);
            } else {
                self.charged_pad = None;
            }
        }

        // The first frame has no previous state to compare against, and the
        // gap from zero would read as a head-on collision.
        if self.started && dt > 1e-4 {
            if car.pad_boost > 0.0 && self.pad_boost <= 0.0 {
                audio.fire(Cue::BoostPickup, 1.0);
            }
            // A collision is the only thing that can change velocity this hard
            // in a single frame: the engine, the brakes and a boost pad all
            // manage well under a tenth of this.
            let jolt = (car.vel - self.velocity).length() / dt;
            if jolt > 400.0 {
                audio.fire(Cue::Impact, ((jolt - 400.0) / 900.0).clamp(0.2, 1.0));
            }
            if compression - self.compression > 0.35 {
                audio.fire(Cue::Suspension, 1.0);
            }
        }

        // Weapons report what happened rather than being re-derived here, so
        // the sound and the sim cannot disagree about whether a shield held.
        for event in &sim.weapons.events {
            match *event {
                crate::weapons::Event::Collected { driver: 0, .. } => {
                    audio.fire(Cue::WeaponPickup, 1.0)
                }
                crate::weapons::Event::Fired { weapon, .. } => match weapon {
                    crate::weapons::Weapon::Rocket => audio.fire(Cue::RocketLaunch, 1.0),
                    crate::weapons::Weapon::Shield => audio.fire(Cue::ShieldUp, 1.0),
                    // A mine going down and a shock going off are both quiet at
                    // the moment of use; what they do is heard when it lands.
                    _ => {}
                },
                // Everyone's hits are audible, not only the player's: half of
                // knowing where you are in a race is hearing it happen to
                // someone else.
                crate::weapons::Event::Struck { .. } => audio.fire(Cue::Explosion, 1.0),
                crate::weapons::Event::Blocked { .. } => audio.fire(Cue::ShieldBreak, 1.0),
                _ => {}
            }
        }

        self.velocity = car.vel;
        self.pad_boost = car.pad_boost;
        self.compression = compression;
        self.started = true;
    }

    /// Called from the respawn paths, which are an input event rather than
    /// something observable in the sim afterwards.
    pub fn respawned(&mut self, audio: &Audio) {
        audio.fire(Cue::Respawn, 1.0);
        self.started = false;
    }
}

/// The game's handle on the audio system. Cloneable state lives behind a lock
/// the audio callback takes once per block, not once per sample.
pub struct Audio {
    mixer: Arc<Mutex<Mixer>>,
    #[cfg(feature = "audio")]
    stream: Option<cpal::Stream>,
}

impl Audio {
    /// Never fails. A machine with no sound card, or a device that refuses the
    /// stream, gets a silent `Audio` and a line on stderr - losing sound is not
    /// a reason to lose the race.
    pub fn new(seed: u64, music_folder: &std::path::Path) -> Audio {
        let rate = Self::device_rate();
        let mut mixer = Mixer::new(rate, seed);

        let playlist = Playlist::load(music_folder);
        if playlist.len() == 0 {
            println!(
                "music: nothing in {}, using the generated bed",
                music_folder.display()
            );
        }
        mixer.attach_playlist(playlist);

        let mixer = Arc::new(Mutex::new(mixer));
        let mut audio = Audio {
            mixer: mixer.clone(),
            #[cfg(feature = "audio")]
            stream: None,
        };
        audio.start();
        audio
    }

    // Used by the tests, which is where the mixer is measured: the game itself
    // always has a device, even when opening it fails.
    #[allow(dead_code)]
    /// A mixer with no device at all, for tests and for `--headless`.
    pub fn silent(seed: u64, rate: f32) -> Audio {
        Audio {
            mixer: Arc::new(Mutex::new(Mixer::new(rate, seed))),
            #[cfg(feature = "audio")]
            stream: None,
        }
    }

    pub fn update(&self, scene: Scene) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.set_scene(scene);
        }
    }

    pub fn set_levels(&self, levels: Levels) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.levels = levels;
        }
    }

    pub fn fire(&self, cue: Cue, strength: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.fire(cue, strength);
        }
    }

    #[allow(dead_code)]
    /// For tests: render a block without a device.
    pub fn render_block(&self, out: &mut [f32], channels: usize) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.render(out, channels);
        }
    }

    #[cfg(not(feature = "audio"))]
    fn device_rate() -> f32 {
        48_000.0
    }

    #[cfg(not(feature = "audio"))]
    fn start(&mut self) {}

    #[cfg(feature = "audio")]
    fn device_rate() -> f32 {
        use cpal::traits::{DeviceTrait, HostTrait};
        cpal::default_host()
            .default_output_device()
            .and_then(|d| d.default_output_config().ok())
            .map(|c| c.sample_rate() as f32)
            .unwrap_or(48_000.0)
    }

    #[cfg(feature = "audio")]
    fn start(&mut self) {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let Some(device) = cpal::default_host().default_output_device() else {
            eprintln!("audio: no output device; running silent");
            return;
        };
        let supported = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("audio: no usable output config ({e}); running silent");
                return;
            }
        };
        let channels = supported.channels() as usize;
        let config = supported.config();
        let mixer = self.mixer.clone();

        let stream = device.build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| match mixer.lock() {
                Ok(mut mixer) => mixer.render(data, channels),
                // A poisoned lock means a game-thread panic, which is already
                // being reported. Silence beats a second panic on this thread.
                Err(_) => data.fill(0.0),
            },
            |e| eprintln!("audio: stream error: {e}"),
            None,
        );

        match stream {
            Ok(stream) => {
                if let Err(e) = stream.play() {
                    eprintln!("audio: could not start the stream ({e}); running silent");
                    return;
                }
                self.stream = Some(stream);
            }
            Err(e) => eprintln!("audio: could not open an output stream ({e}); running silent"),
        }
    }
}

#[cfg(test)]
mod tests;
