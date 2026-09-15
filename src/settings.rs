//! Player settings, and the file they live in.
//!
//! Stored as plain `key value` lines, parsed the same forgiving way the asset
//! manifest is: an unknown key is skipped with a note, a malformed value keeps
//! the default, and a missing or damaged file is not an error. Losing your
//! settings should never stop the game starting.
//!
//! Every field here changes something. A menu full of knobs that do nothing is
//! worse than a short menu, so nothing is listed that is not wired up.

use std::path::{Path, PathBuf};

pub const FILE: &str = "treadlock.cfg";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Difficulty {
    Cruise,
    Racer,
    Veteran,
    Rollcage,
}

impl Difficulty {
    pub const ALL: [Difficulty; 4] =
        [Difficulty::Cruise, Difficulty::Racer, Difficulty::Veteran, Difficulty::Rollcage];

    pub fn name(self) -> &'static str {
        match self {
            Difficulty::Cruise => "CRUISE",
            Difficulty::Racer => "RACER",
            Difficulty::Veteran => "VETERAN",
            Difficulty::Rollcage => "ROLLCAGE",
        }
    }

    /// One line explaining what changes, shown under the selection. A
    /// difficulty label on its own tells a player nothing.
    pub fn blurb(self) -> &'static str {
        match self {
            Difficulty::Cruise => "SLOWER FIELD   THEY RARELY SHOOT",
            Difficulty::Racer => "EVEN FIELD   NORMAL WEAPONS",
            Difficulty::Veteran => "FAST FIELD   THEY SHOOT OFTEN",
            Difficulty::Rollcage => "FLAT OUT   THEY SHOOT ON SIGHT",
        }
    }

    /// Lowest and highest driver skill in the field. Skill scales throttle,
    /// how far ahead a driver reads the track, and whether they use boost.
    pub fn skill_range(self) -> (f32, f32) {
        match self {
            Difficulty::Cruise => (0.58, 0.70),
            Difficulty::Racer => (0.82, 0.98),
            Difficulty::Veteran => (0.92, 1.0),
            Difficulty::Rollcage => (0.99, 1.0),
        }
    }

    /// How readily the AI uses what it is holding. 0 is never, 1 is the
    /// baseline, above that is faster to pull the trigger.
    pub fn aggression(self) -> f32 {
        match self {
            Difficulty::Cruise => 0.35,
            Difficulty::Racer => 1.0,
            Difficulty::Veteran => 1.6,
            Difficulty::Rollcage => 2.4,
        }
    }

    fn parse(word: &str) -> Option<Difficulty> {
        Difficulty::ALL.into_iter().find(|d| d.name() == word)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Video {
    pub fullscreen: bool,
    pub vsync: bool,
    /// Degrees at a standstill; the camera widens this with speed.
    pub fov: f32,
    /// Scales every particle emitter. The cheapest thing to turn down on a
    /// 24 compute unit part, and the first thing worth trying if frames drop.
    pub particles: f32,
    /// Draws skid marks. A whole pass, so it is worth being able to switch off.
    pub skid_marks: bool,
    /// Background scenery and sky. Also a whole pass.
    pub scenery: bool,
}

impl Default for Video {
    fn default() -> Video {
        Video { fullscreen: false, vsync: true, fov: 68.0, particles: 1.0, skid_marks: true, scenery: true }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Pad {
    /// Stick movement below this is ignored, so a worn stick does not steer on
    /// its own. The single most useful controller setting there is.
    pub deadzone: f32,
    /// Multiplies stick deflection into steering.
    pub steer_sensitivity: f32,
    /// Above 1 the stick is progressive: small movements steer less than
    /// proportionally, which makes a twitchy car manageable at speed.
    pub steer_curve: f32,
    pub invert_steer: bool,
    /// Vibration is declared here so the menu can offer it, and is honoured
    /// once there is a rumble path to honour it with.
    pub rumble: bool,
}

impl Default for Pad {
    fn default() -> Pad {
        Pad { deadzone: 0.12, steer_sensitivity: 1.0, steer_curve: 1.6, invert_steer: false, rumble: true }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Race {
    pub opponents: usize,
    pub difficulty: Difficulty,
    /// 0 means a fresh track every race.
    pub seed: u64,
    pub weapons: bool,
}

impl Default for Race {
    fn default() -> Race {
        Race { opponents: 5, difficulty: Difficulty::Racer, seed: 0, weapons: true }
    }
}

/// Most cars on the grid at once. The grid has to hold them and the BC-250 has
/// to draw them.
pub const MAX_OPPONENTS: usize = 9;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Settings {
    pub video: Video,
    pub pad: Pad,
    pub race: Race,
}

impl Settings {
    pub fn path() -> PathBuf {
        PathBuf::from(FILE)
    }

    pub fn load() -> Settings {
        Settings::load_from(&Settings::path())
    }

    pub fn load_from(path: &Path) -> Settings {
        let Ok(text) = std::fs::read_to_string(path) else {
            // No file yet is the normal first run, not a problem.
            return Settings::default();
        };
        Settings::parse(&text)
    }

    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
                eprintln!("settings: line {} is malformed, skipping", number + 1);
                continue;
            };
            // A value that will not parse keeps the default rather than taking
            // the whole file down with it.
            let flag = |v: &str| matches!(v, "1" | "true" | "yes" | "on");
            match key {
                "fullscreen" => s.video.fullscreen = flag(value),
                "vsync" => s.video.vsync = flag(value),
                "fov" => s.video.fov = value.parse().unwrap_or(s.video.fov),
                "particles" => s.video.particles = value.parse().unwrap_or(s.video.particles),
                "skid_marks" => s.video.skid_marks = flag(value),
                "scenery" => s.video.scenery = flag(value),
                "deadzone" => s.pad.deadzone = value.parse().unwrap_or(s.pad.deadzone),
                "steer_sensitivity" => {
                    s.pad.steer_sensitivity = value.parse().unwrap_or(s.pad.steer_sensitivity)
                }
                "steer_curve" => s.pad.steer_curve = value.parse().unwrap_or(s.pad.steer_curve),
                "invert_steer" => s.pad.invert_steer = flag(value),
                "rumble" => s.pad.rumble = flag(value),
                "opponents" => s.race.opponents = value.parse().unwrap_or(s.race.opponents),
                "difficulty" => {
                    s.race.difficulty = Difficulty::parse(value).unwrap_or(s.race.difficulty)
                }
                "seed" => s.race.seed = value.parse().unwrap_or(s.race.seed),
                "weapons" => s.race.weapons = flag(value),
                _ => eprintln!("settings: line {} has unknown setting {key:?}", number + 1),
            }
        }
        s.clamped()
    }

    /// Bring every value back inside its range.
    ///
    /// Applied on load as well as on edit, because the file is text and someone
    /// will eventually put a field of view of 400 in it.
    pub fn clamped(mut self) -> Settings {
        self.video.fov = self.video.fov.clamp(50.0, 110.0);
        self.video.particles = self.video.particles.clamp(0.0, 1.0);
        self.pad.deadzone = self.pad.deadzone.clamp(0.0, 0.45);
        self.pad.steer_sensitivity = self.pad.steer_sensitivity.clamp(0.4, 2.0);
        self.pad.steer_curve = self.pad.steer_curve.clamp(1.0, 3.0);
        self.race.opponents = self.race.opponents.min(MAX_OPPONENTS);
        self
    }

    pub fn to_text(self) -> String {
        let flag = |b: bool| if b { "true" } else { "false" };
        format!(
            "# Treadlock settings. Edited by the in-game menus; safe to edit here too.\n\
             \n# video\n\
             fullscreen         {}\n\
             vsync              {}\n\
             fov                {:.0}\n\
             particles          {:.2}\n\
             skid_marks         {}\n\
             scenery            {}\n\
             \n# controller\n\
             deadzone           {:.2}\n\
             steer_sensitivity  {:.2}\n\
             steer_curve        {:.2}\n\
             invert_steer       {}\n\
             rumble             {}\n\
             \n# race\n\
             opponents          {}\n\
             difficulty         {}\n\
             seed               {}\n\
             weapons            {}\n",
            flag(self.video.fullscreen),
            flag(self.video.vsync),
            self.video.fov,
            self.video.particles,
            flag(self.video.skid_marks),
            flag(self.video.scenery),
            self.pad.deadzone,
            self.pad.steer_sensitivity,
            self.pad.steer_curve,
            flag(self.pad.invert_steer),
            flag(self.pad.rumble),
            self.race.opponents,
            self.race.difficulty.name(),
            self.race.seed,
            flag(self.race.weapons),
        )
    }

    pub fn save(self) {
        self.save_to(&Settings::path());
    }

    pub fn save_to(self, path: &Path) {
        if let Err(e) = std::fs::write(path, self.to_text()) {
            // Not being able to save settings is worth saying once and is not
            // worth stopping for.
            eprintln!("settings: could not write {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("treadlock-test-{name}.cfg"))
    }

    /// Everything written must come back. A setting that silently resets on
    /// restart is worse than one that cannot be changed at all.
    #[test]
    fn settings_survive_a_round_trip() {
        let original = Settings {
            video: Video {
                fullscreen: true,
                vsync: false,
                fov: 95.0,
                particles: 0.25,
                skid_marks: false,
                scenery: false,
            },
            pad: Pad {
                deadzone: 0.3,
                steer_sensitivity: 1.75,
                steer_curve: 2.5,
                invert_steer: true,
                rumble: false,
            },
            race: Race {
                opponents: 8,
                difficulty: Difficulty::Rollcage,
                seed: 4242,
                weapons: false,
            },
        };

        let path = scratch("roundtrip");
        original.save_to(&path);
        let loaded = Settings::load_from(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded, original, "a setting did not survive being written and read");
    }

    /// A missing file is the normal first run.
    #[test]
    fn a_missing_file_gives_defaults() {
        let loaded = Settings::load_from(&scratch("definitely-not-here"));
        assert_eq!(loaded, Settings::default());
    }

    /// A damaged file must not stop the game. Every line here is wrong in a
    /// different way, and the two good ones still have to take effect.
    #[test]
    fn a_damaged_file_keeps_what_it_can() {
        let loaded = Settings::parse(
            "fov\n\
             particles not-a-number\n\
             wobble 3\n\
             difficulty SPICY\n\
             opponents 7\n\
             # a comment\n\
             \n\
             vsync off\n",
        );
        assert_eq!(loaded.race.opponents, 7, "a good line after bad ones was dropped");
        assert!(!loaded.video.vsync, "a good line was dropped");
        assert_eq!(loaded.video.fov, Video::default().fov, "a malformed value was applied");
        assert_eq!(loaded.video.particles, Video::default().particles);
        assert_eq!(loaded.race.difficulty, Race::default().difficulty);
    }

    /// The file is text and people edit text. Nothing out of range may reach
    /// the game.
    #[test]
    fn absurd_values_are_brought_back_into_range() {
        let loaded = Settings::parse(
            "fov 400\n\
             particles 90\n\
             deadzone -3\n\
             steer_sensitivity 99\n\
             opponents 500\n",
        );
        assert!((50.0..=110.0).contains(&loaded.video.fov), "fov {} escaped", loaded.video.fov);
        assert!((0.0..=1.0).contains(&loaded.video.particles));
        assert!((0.0..=0.45).contains(&loaded.pad.deadzone));
        assert!((0.4..=2.0).contains(&loaded.pad.steer_sensitivity));
        assert!(loaded.race.opponents <= MAX_OPPONENTS);
    }

    /// Difficulty has to actually mean something, in the direction the label
    /// implies, or it is set dressing.
    #[test]
    fn difficulty_rises_across_the_settings() {
        let mut previous = (0.0f32, 0.0f32);
        for difficulty in Difficulty::ALL {
            let (low, high) = difficulty.skill_range();
            assert!(low <= high, "{} has an inverted skill range", difficulty.name());
            assert!(
                low >= previous.0 && difficulty.aggression() >= previous.1,
                "{} is not harder than the setting below it",
                difficulty.name()
            );
            previous = (low, difficulty.aggression());
        }
        assert!(Difficulty::Cruise.aggression() < Difficulty::Rollcage.aggression() * 0.5);
    }
}
