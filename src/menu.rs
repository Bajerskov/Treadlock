//! The front end: the screen the game starts on, and the settings behind it.
//!
//! Built on the same bitmap UI the HUD uses, so the whole front end is still
//! one draw call and there is no second text path to keep in step.
//!
//! The menu owns a `Settings` and hands back an `Outcome` for anything it
//! cannot do itself - start a race, change the window, quit. It never touches
//! the sim, the window or the renderer directly, which is what lets the whole
//! thing be driven and checked without a GPU.

use glam::Vec4;

use crate::settings::{Difficulty, Settings, MAX_OPPONENTS};
use crate::ui::Ui;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Main,
    /// Reached by pressing play: how this particular race is set up.
    Race,
    Video,
    Audio,
    Controls,
}

/// What the player did, already mapped off the physical key or button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Accept,
    Back,
}

/// Something the menu cannot do for itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    None,
    StartRace,
    Quit,
    /// A video setting changed that the window or swapchain has to act on.
    VideoChanged,
    /// The menu closed without starting anything, which only happens when a
    /// race is already running behind it.
    Resume,
}

/// One line on screen.
pub struct Row {
    pub label: &'static str,
    /// None for a plain action like PLAY or BACK.
    pub value: Option<String>,
    /// Shown under the selection. Says what the setting does, not what it is.
    pub hint: &'static str,
}

pub struct Menu {
    pub screen: Screen,
    cursor: usize,
    pub settings: Settings,
    /// True once a race exists behind the menu, which turns the top entry from
    /// PLAY into RESUME and lets Escape close the menu instead of quitting.
    pub racing: bool,
    /// Set when anything changed, so settings are written once on the way out
    /// rather than on every keypress.
    dirty: bool,
}

impl Menu {
    pub fn new(settings: Settings) -> Menu {
        Menu { screen: Screen::Main, cursor: 0, settings, racing: false, dirty: false }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn rows(&self) -> Vec<Row> {
        let s = &self.settings;
        let flag = |b: bool| Some(String::from(if b { "ON" } else { "OFF" }));
        match self.screen {
            Screen::Main => vec![
                Row {
                    label: if self.racing { "RESUME" } else { "PLAY" },
                    value: None,
                    hint: if self.racing {
                        "BACK TO THE RACE"
                    } else {
                        "SET UP A RACE AND GO"
                    },
                },
                Row { label: "VIDEO", value: None, hint: "DISPLAY AND DETAIL" },
                Row { label: "AUDIO", value: None, hint: "VOLUME FOR EACH PART OF THE MIX" },
                Row { label: "CONTROLS", value: None, hint: "GAMEPAD FEEL AND BINDINGS" },
                Row { label: "QUIT", value: None, hint: "CLOSE THE GAME" },
            ],
            Screen::Race => vec![
                Row {
                    label: "OPPONENTS",
                    value: Some(s.race.opponents.to_string()),
                    hint: "HOW MANY CARS RACE AGAINST YOU",
                },
                Row {
                    label: "DIFFICULTY",
                    value: Some(s.race.difficulty.name().to_string()),
                    hint: s.race.difficulty.blurb(),
                },
                Row {
                    label: "WEAPONS",
                    value: flag(s.race.weapons),
                    hint: "OFF MAKES IT A PURE RACE   NO CRATES",
                },
                Row {
                    label: "TRACK",
                    value: Some(if s.race.seed == 0 {
                        "RANDOM".to_string()
                    } else {
                        s.race.seed.to_string()
                    }),
                    hint: "A NUMBER PICKS THE SAME TRACK EVERY TIME",
                },
                Row { label: "START", value: None, hint: "GO RACING" },
                Row { label: "BACK", value: None, hint: "" },
            ],
            Screen::Video => vec![
                Row {
                    label: "FULLSCREEN",
                    value: flag(s.video.fullscreen),
                    hint: "BORDERLESS ON THE CURRENT MONITOR",
                },
                Row {
                    label: "VSYNC",
                    value: flag(s.video.vsync),
                    hint: "OFF UNCAPS THE FRAME RATE AND MAY TEAR",
                },
                Row {
                    label: "FIELD OF VIEW",
                    value: Some(format!("{:.0}", s.video.fov)),
                    hint: "AT A STANDSTILL   SPEED WIDENS IT",
                },
                Row {
                    label: "PARTICLES",
                    value: Some(percent(s.video.particles)),
                    hint: "THE CHEAPEST THING TO TURN DOWN",
                },
                Row {
                    label: "SKID MARKS",
                    value: flag(s.video.skid_marks),
                    hint: "RUBBER LEFT ON THE ROAD",
                },
                Row {
                    label: "SCENERY",
                    value: flag(s.video.scenery),
                    hint: "THE SKYLINE AND SKY OUTSIDE THE TUBE",
                },
                Row { label: "BACK", value: None, hint: "" },
            ],
            Screen::Audio => vec![
                Row {
                    label: "MASTER",
                    value: Some(percent(s.audio.master)),
                    hint: "EVERYTHING AT ONCE",
                },
                Row {
                    label: "ENGINES",
                    value: Some(percent(s.audio.engine)),
                    hint: "YOUR CAR AND EVERY CAR AROUND YOU",
                },
                Row {
                    label: "EFFECTS",
                    value: Some(percent(s.audio.effects)),
                    hint: "TYRES   IMPACTS   WEAPONS   PICKUPS",
                },
                Row {
                    label: "AMBIENCE",
                    value: Some(percent(s.audio.ambient)),
                    hint: "WIND AND THE TUNNEL TONE",
                },
                Row {
                    label: "MUSIC",
                    value: Some(percent(s.audio.music)),
                    hint: "GENERATED UNTIL A TRACK IS SUPPLIED",
                },
                Row {
                    label: "MUTE",
                    value: flag(s.audio.muted),
                    hint: "SILENCE WITHOUT LOSING THE BALANCE",
                },
                Row { label: "BACK", value: None, hint: "" },
            ],
            Screen::Controls => vec![
                Row {
                    label: "DEADZONE",
                    value: Some(percent(s.pad.deadzone / 0.45)),
                    hint: "RAISE IT IF THE CAR STEERS ON ITS OWN",
                },
                Row {
                    label: "STEERING",
                    value: Some(format!("{:.2}", s.pad.steer_sensitivity)),
                    hint: "HOW FAR THE STICK TURNS THE WHEELS",
                },
                Row {
                    label: "STEER CURVE",
                    value: Some(format!("{:.2}", s.pad.steer_curve)),
                    hint: "HIGHER IS GENTLER NEAR THE CENTRE",
                },
                Row {
                    label: "INVERT STEER",
                    value: flag(s.pad.invert_steer),
                    hint: "",
                },
                Row { label: "RUMBLE", value: flag(s.pad.rumble), hint: "" },
                Row { label: "BACK", value: None, hint: "" },
            ],
        }
    }

    pub fn input(&mut self, action: Action) -> Outcome {
        let count = self.rows().len();
        match action {
            // Wrapping, because a menu this short is quicker to reach the
            // bottom of by going up.
            Action::Up => {
                self.cursor = (self.cursor + count - 1) % count;
                Outcome::None
            }
            Action::Down => {
                self.cursor = (self.cursor + 1) % count;
                Outcome::None
            }
            Action::Left => self.adjust(-1),
            Action::Right => self.adjust(1),
            Action::Accept => self.activate(),
            Action::Back => self.back(),
        }
    }

    /// Dispatch is by label rather than by row number on purpose. Adding one
    /// entry to a screen shifts every index below it, and a match on indices
    /// would then quietly point at the wrong setting - adding AUDIO to the
    /// main screen would have made QUIT out of it.
    fn selected(&self) -> &'static str {
        self.rows().get(self.cursor).map(|r| r.label).unwrap_or("")
    }

    fn adjust(&mut self, delta: i32) -> Outcome {
        let step = delta as f32;
        let mut outcome = Outcome::None;
        let label = self.selected();
        let s = &mut self.settings;
        match label {
            "OPPONENTS" => {
                let next = s.race.opponents as i32 + delta;
                s.race.opponents = next.clamp(0, MAX_OPPONENTS as i32) as usize;
            }
            "DIFFICULTY" => {
                let index = Difficulty::ALL
                    .iter()
                    .position(|d| *d == s.race.difficulty)
                    .unwrap_or(0) as i32;
                let next = (index + delta).clamp(0, Difficulty::ALL.len() as i32 - 1);
                s.race.difficulty = Difficulty::ALL[next as usize];
            }
            "WEAPONS" => s.race.weapons = !s.race.weapons,
            "TRACK" => {
                // Seed 0 means "a fresh track each time", and is the floor
                // rather than something you can go below into nonsense.
                s.race.seed = (s.race.seed as i64 + delta as i64).max(0) as u64;
            }
            "FULLSCREEN" => {
                s.video.fullscreen = !s.video.fullscreen;
                outcome = Outcome::VideoChanged;
            }
            "VSYNC" => {
                s.video.vsync = !s.video.vsync;
                outcome = Outcome::VideoChanged;
            }
            "FIELD OF VIEW" => s.video.fov += step * 5.0,
            "PARTICLES" => s.video.particles += step * 0.1,
            "SKID MARKS" => s.video.skid_marks = !s.video.skid_marks,
            "SCENERY" => s.video.scenery = !s.video.scenery,
            "MASTER" => s.audio.master += step * 0.05,
            "ENGINES" => s.audio.engine += step * 0.05,
            "EFFECTS" => s.audio.effects += step * 0.05,
            "AMBIENCE" => s.audio.ambient += step * 0.05,
            "MUSIC" => s.audio.music += step * 0.05,
            "MUTE" => s.audio.muted = !s.audio.muted,
            "DEADZONE" => s.pad.deadzone += step * 0.02,
            "STEERING" => s.pad.steer_sensitivity += step * 0.05,
            "STEER CURVE" => s.pad.steer_curve += step * 0.1,
            "INVERT STEER" => s.pad.invert_steer = !s.pad.invert_steer,
            "RUMBLE" => s.pad.rumble = !s.pad.rumble,
            // Plain actions have nothing to adjust.
            _ => return Outcome::None,
        }
        // Clamped every time rather than per-branch, so a new setting cannot be
        // added with its limits forgotten.
        self.settings = self.settings.clamped();
        self.dirty = true;
        outcome
    }

    fn activate(&mut self) -> Outcome {
        match self.selected() {
            "RESUME" => Outcome::Resume,
            "PLAY" => self.go(Screen::Race),
            "VIDEO" => self.go(Screen::Video),
            "AUDIO" => self.go(Screen::Audio),
            "CONTROLS" => self.go(Screen::Controls),
            "QUIT" => Outcome::Quit,
            "START" => {
                self.save();
                Outcome::StartRace
            }
            "BACK" => self.back(),
            // Everything else on a settings row is a value, and Accept nudges
            // it the same way Right does rather than doing nothing.
            _ => self.adjust(1),
        }
    }

    fn back(&mut self) -> Outcome {
        match self.screen {
            Screen::Main if self.racing => Outcome::Resume,
            Screen::Main => Outcome::None,
            _ => {
                self.save();
                self.screen = Screen::Main;
                self.cursor = 0;
                Outcome::None
            }
        }
    }

    fn go(&mut self, screen: Screen) -> Outcome {
        self.screen = screen;
        self.cursor = 0;
        Outcome::None
    }

    /// Written once on leaving a screen rather than on every keypress, so
    /// holding left on a slider does not hammer the disk.
    fn save(&mut self) {
        if self.dirty {
            self.settings.save();
            self.dirty = false;
        }
    }
}

fn percent(fraction: f32) -> String {
    format!("{:.0}%", fraction.clamp(0.0, 1.0) * 100.0)
}

/// Draw the current screen.
pub fn build(ui: &mut Ui, menu: &Menu, width: f32, height: f32, pads: usize) {
    ui.clear();
    let scale = (height / 360.0).max(1.0).floor();
    let white = Vec4::ONE;
    let dim = Vec4::new(0.62, 0.68, 0.80, 1.0);
    let accent = Vec4::new(0.35, 0.95, 1.0, 1.0);

    // Dim the race behind, rather than hiding it. Watching the field go round
    // is half the reason the front end is over a live track at all.
    ui.rect(0.0, 0.0, width, height, Vec4::new(0.02, 0.03, 0.05, 0.62));

    let title_scale = scale * 5.0;
    let title = "TREADLOCK";
    ui.text_shadowed(
        (width - Ui::text_width(title, title_scale)) * 0.5,
        height * 0.10,
        title_scale,
        white,
        title,
    );

    let subtitle = match menu.screen {
        Screen::Main => "",
        Screen::Race => "RACE SETUP",
        Screen::Video => "VIDEO",
        Screen::Audio => "AUDIO",
        Screen::Controls => "CONTROLS",
    };
    if !subtitle.is_empty() {
        let s = scale * 2.0;
        ui.text_shadowed(
            (width - Ui::text_width(subtitle, s)) * 0.5,
            height * 0.10 + 8.0 * title_scale,
            s,
            accent,
            subtitle,
        );
    }

    let rows = menu.rows();
    let row_scale = scale * 2.5;
    let line = 11.0 * row_scale;
    let top = height * 0.34;
    // Wide enough for the longest label and value together, and centred, so
    // the block does not shift as values change width.
    let block = width * 0.46;
    let left = (width - block) * 0.5;

    for (index, row) in rows.iter().enumerate() {
        let y = top + index as f32 * line;
        let selected = index == menu.cursor();
        if selected {
            ui.rect(
                left - 6.0 * scale,
                y - 3.0 * scale,
                block + 12.0 * scale,
                7.0 * row_scale + 6.0 * scale,
                Vec4::new(0.12, 0.30, 0.42, 0.85),
            );
            // A marker as well as a highlight: on a washed-out TV the panel
            // alone can be hard to pick out.
            ui.text_shadowed(left - 5.0 * row_scale, y, row_scale, accent, ">");
        }
        ui.text_shadowed(left, y, row_scale, if selected { white } else { dim }, row.label);
        if let Some(value) = &row.value {
            ui.text_shadowed(
                left + block - Ui::text_width(value, row_scale),
                y,
                row_scale,
                if selected { accent } else { dim },
                value,
            );
        }
    }

    // The hint for the selection, under the list, where it does not move the
    // rows around as it changes length.
    if let Some(row) = rows.get(menu.cursor()) {
        if !row.hint.is_empty() {
            let s = scale * 1.5;
            ui.text_shadowed(
                (width - Ui::text_width(row.hint, s)) * 0.5,
                top + rows.len() as f32 * line + 6.0 * s,
                s,
                dim,
                row.hint,
            );
        }
    }

    // Footer: how to drive the menu, and whether a pad is plugged in. The
    // second is the fastest answer to "why is my controller not working".
    let s = scale * 1.5;
    let footer = "ARROWS MOVE   LEFT RIGHT CHANGE   ENTER SELECT   ESC BACK";
    ui.text_shadowed(
        (width - Ui::text_width(footer, s)) * 0.5,
        height - 12.0 * scale - 9.0 * s,
        s,
        dim,
        footer,
    );
    let pad_line = match pads {
        0 => "NO GAMEPAD DETECTED   KEYBOARD ONLY".to_string(),
        1 => "GAMEPAD CONNECTED".to_string(),
        n => format!("{n} GAMEPADS CONNECTED"),
    };
    ui.text_shadowed(
        (width - Ui::text_width(&pad_line, s)) * 0.5,
        height - 12.0 * scale,
        s,
        if pads > 0 { accent } else { dim },
        &pad_line,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        Menu::new(Settings::default())
    }

    /// Every word the front end can put on screen has to exist in the font. A
    /// missing glyph is a hole in a word, not an error, so nothing else would
    /// catch it.
    #[test]
    fn every_menu_string_is_in_the_font() {
        let mut m = menu();
        // Walk every screen, and every value each adjustable row can take.
        for screen in [Screen::Main, Screen::Race, Screen::Video, Screen::Audio, Screen::Controls] {
            m.screen = screen;
            for racing in [false, true] {
                m.racing = racing;
                for cursor in 0..m.rows().len() {
                    m.cursor = cursor;
                    // Run each row to both ends of its range.
                    for _ in 0..40 {
                        m.input(Action::Left);
                    }
                    for _ in 0..40 {
                        m.input(Action::Right);
                        for row in m.rows() {
                            for text in
                                [row.label.to_string(), row.hint.to_string()]
                                    .into_iter()
                                    .chain(row.value)
                            {
                                assert!(
                                    crate::ui::missing_glyph(&text).is_none(),
                                    "no glyph for {:?}, needed by {text:?}",
                                    crate::ui::missing_glyph(&text).unwrap()
                                );
                            }
                        }
                    }
                }
            }
        }
        for text in [
            "TREADLOCK",
            "RACE SETUP",
            "ARROWS MOVE   LEFT RIGHT CHANGE   ENTER SELECT   ESC BACK",
            "NO GAMEPAD DETECTED   KEYBOARD ONLY",
            "GAMEPAD CONNECTED",
            "2 GAMEPADS CONNECTED",
            ">",
        ] {
            assert!(crate::ui::missing_glyph(text).is_none(), "no glyph in {text:?}");
        }
    }

    /// The cursor wraps and never leaves the list, on any screen.
    #[test]
    fn the_cursor_stays_on_the_list() {
        let mut m = menu();
        for screen in [Screen::Main, Screen::Race, Screen::Video, Screen::Audio, Screen::Controls] {
            m.screen = screen;
            m.cursor = 0;
            let count = m.rows().len();
            m.input(Action::Up);
            assert_eq!(m.cursor(), count - 1, "up from the top did not wrap to the bottom");
            m.input(Action::Down);
            assert_eq!(m.cursor(), 0, "down from the bottom did not wrap to the top");
            for _ in 0..count * 3 {
                m.input(Action::Down);
                assert!(m.cursor() < count, "the cursor left the list");
            }
        }
    }

    /// Holding a direction must not push a setting out of range, and must
    /// actually move it.
    #[test]
    fn settings_move_and_stay_in_range() {
        let mut m = menu();
        m.screen = Screen::Video;
        m.cursor = 2; // field of view
        let start = m.settings.video.fov;
        m.input(Action::Right);
        assert!(m.settings.video.fov > start, "right did not raise the field of view");
        for _ in 0..200 {
            m.input(Action::Right);
        }
        assert!(m.settings.video.fov <= 110.0, "field of view ran past its limit");
        for _ in 0..400 {
            m.input(Action::Left);
        }
        assert!(m.settings.video.fov >= 50.0, "field of view ran below its limit");

        m.screen = Screen::Race;
        m.cursor = 0; // opponents
        for _ in 0..50 {
            m.input(Action::Right);
        }
        assert_eq!(m.settings.race.opponents, MAX_OPPONENTS);
        for _ in 0..50 {
            m.input(Action::Left);
        }
        assert_eq!(m.settings.race.opponents, 0, "a solo race must be reachable");

        m.cursor = 1; // difficulty
        for _ in 0..20 {
            m.input(Action::Left);
        }
        assert_eq!(m.settings.race.difficulty, Difficulty::ALL[0]);
        for _ in 0..20 {
            m.input(Action::Right);
        }
        assert_eq!(m.settings.race.difficulty, *Difficulty::ALL.last().unwrap());
    }

    /// Volumes move, and never leave the range the mixer expects.
    #[test]
    fn volumes_move_and_stay_in_range() {
        let mut m = menu();
        m.screen = Screen::Audio;
        for row in 0..m.rows().len() {
            m.cursor = row;
            for _ in 0..80 {
                m.input(Action::Right);
            }
            for _ in 0..160 {
                m.input(Action::Left);
            }
            for _ in 0..80 {
                m.input(Action::Right);
            }
        }
        let a = m.settings.audio;
        for (name, level) in [
            ("master", a.master),
            ("engine", a.engine),
            ("effects", a.effects),
            ("ambient", a.ambient),
            ("music", a.music),
        ] {
            assert!((0.0..=1.0).contains(&level), "{name} escaped its range at {level}");
        }

        // And the sliders actually reach both ends.
        m.cursor = 0;
        for _ in 0..80 {
            m.input(Action::Left);
        }
        assert_eq!(m.settings.audio.master, 0.0, "master would not go to silence");
        for _ in 0..80 {
            m.input(Action::Right);
        }
        assert_eq!(m.settings.audio.master, 1.0, "master would not go to full");
    }

    /// Mute silences the output and gives the balance back untouched. A mute
    /// that reset the mix would cost the player their settings every time they
    /// answered the door.
    #[test]
    fn mute_silences_without_losing_the_balance() {
        let mut m = menu();
        m.settings.audio.engine = 0.9;
        m.settings.audio.music = 0.1;
        m.screen = Screen::Audio;
        while m.selected() != "MUTE" {
            m.input(Action::Down);
        }

        m.input(Action::Accept);
        assert!(m.settings.audio.muted);
        let muted: crate::audio::Levels = m.settings.audio.into();
        assert_eq!(muted.master, 0.0, "mute did not silence the master");
        assert_eq!(muted.engine, 0.9, "mute disturbed the engine level");
        assert_eq!(muted.music, 0.1, "mute disturbed the music level");

        m.input(Action::Accept);
        assert!(!m.settings.audio.muted);
        let live: crate::audio::Levels = m.settings.audio.into();
        assert!(live.master > 0.0, "unmuting did not restore the master");
        assert_eq!(live.engine, 0.9);
        assert_eq!(live.music, 0.1);
    }

    /// What the menu shows has to be what the mixer is given. A settings screen
    /// that quietly rescales is a settings screen nobody can reason about.
    #[test]
    fn the_levels_the_menu_shows_are_the_levels_the_mixer_gets() {
        let mut m = menu();
        m.settings.audio = crate::settings::Audio {
            master: 0.6,
            engine: 0.4,
            effects: 0.8,
            ambient: 0.2,
            music: 1.0,
            muted: false,
        };
        let levels: crate::audio::Levels = m.settings.audio.into();
        assert_eq!(levels.master, 0.6);
        assert_eq!(levels.engine, 0.4);
        assert_eq!(levels.effects, 0.8);
        assert_eq!(levels.ambient, 0.2);
        assert_eq!(levels.music, 1.0);
    }

    /// Turning a bus down has to make that bus quieter in the actual output,
    /// not merely store a smaller number.
    #[test]
    fn turning_the_engine_down_makes_the_engine_quieter() {
        let render = |audio: crate::settings::Audio| {
            let mut mixer = crate::audio::Mixer::new(48_000.0, 7);
            mixer.levels = audio.into();
            mixer.set_scene(crate::audio::Scene {
                rpm: 0.8,
                throttle: 1.0,
                speed: 0.0,
                enclosure: 0.0,
                music: 0.0,
                ..Default::default()
            });
            let mut out = vec![0.0f32; 24_000 * 2];
            mixer.render(&mut out, 2);
            // Skip the settling glides.
            out[24_000..].iter().map(|s| s * s).sum::<f32>()
        };

        let quiet = crate::settings::Audio { music: 0.0, engine: 0.1, ..Default::default() };
        let loud = crate::settings::Audio { music: 0.0, engine: 1.0, ..Default::default() };
        let muted = crate::settings::Audio { muted: true, ..loud };

        assert!(render(loud) > render(quiet) * 4.0, "the engine level barely changed the mix");
        assert!(render(muted) < 1e-9, "mute left sound coming out");
    }

    /// Play leads to the race setup, and start from there starts a race. This
    /// is the one path through the front end that everybody takes.
    #[test]
    fn play_then_start_begins_a_race() {
        let mut m = menu();
        assert_eq!(m.input(Action::Accept), Outcome::None);
        assert_eq!(m.screen, Screen::Race, "play did not open the race setup");
        assert_eq!(m.cursor(), 0, "the cursor was not reset on the new screen");

        // Down to START.
        while m.rows()[m.cursor()].label != "START" {
            m.input(Action::Down);
        }
        assert_eq!(m.input(Action::Accept), Outcome::StartRace);
    }

    /// No entry may be dead.
    ///
    /// Dispatch is by label, so a typo in one would fall through to the
    /// value-adjusting arm and do nothing at all - a menu entry that looks
    /// fine and simply never responds. Every plain action is pressed here and
    /// has to do something.
    #[test]
    fn every_plain_entry_does_something_when_pressed() {
        for screen in [Screen::Main, Screen::Race, Screen::Video, Screen::Audio, Screen::Controls] {
            for racing in [false, true] {
                let mut m = menu();
                m.screen = screen;
                m.racing = racing;
                for row in 0..m.rows().len() {
                    // A row with no value is an action rather than a setting.
                    if m.rows()[row].value.is_some() {
                        continue;
                    }
                    let mut m = menu();
                    m.screen = screen;
                    m.racing = racing;
                    m.cursor = row;
                    let label = m.selected();
                    let outcome = m.input(Action::Accept);
                    assert!(
                        outcome != Outcome::None || m.screen != screen,
                        "{label:?} on {screen:?} did nothing when pressed"
                    );
                }
            }
        }
    }

    /// Escape backs out of a sub-screen rather than quitting, and quits only
    /// from the entry the player chose.
    #[test]
    fn back_leaves_a_screen_and_quit_is_deliberate() {
        let mut m = menu();
        m.screen = Screen::Video;
        m.cursor = 3;
        assert_eq!(m.input(Action::Back), Outcome::None);
        assert_eq!(m.screen, Screen::Main);
        assert_eq!(m.cursor(), 0);

        // Nothing on the main screen quits except QUIT itself.
        assert_eq!(m.input(Action::Back), Outcome::None, "escape on the main screen quit the game");
        while m.rows()[m.cursor()].label != "QUIT" {
            m.input(Action::Down);
        }
        assert_eq!(m.input(Action::Accept), Outcome::Quit);
    }

    /// Once a race is running the menu is a pause screen, so the top entry
    /// resumes and escape closes it.
    #[test]
    fn the_menu_becomes_a_pause_screen_once_racing() {
        let mut m = menu();
        m.racing = true;
        assert_eq!(m.rows()[0].label, "RESUME");
        assert_eq!(m.input(Action::Accept), Outcome::Resume);
        assert_eq!(m.input(Action::Back), Outcome::Resume);
    }

    /// Changing the window is the one thing the menu cannot do itself, so it
    /// has to say so.
    #[test]
    fn video_changes_that_need_the_window_report_it() {
        let mut m = menu();
        m.screen = Screen::Video;
        m.cursor = 0; // fullscreen
        assert_eq!(m.input(Action::Right), Outcome::VideoChanged);
        m.cursor = 1; // vsync
        assert_eq!(m.input(Action::Right), Outcome::VideoChanged);
        // The field of view is applied per frame and needs nothing rebuilt.
        m.cursor = 2;
        assert_eq!(m.input(Action::Right), Outcome::None);
    }

    /// The setup the menu produces has to be the race that actually runs.
    #[test]
    fn the_chosen_setup_is_the_race_that_runs() {
        let mut m = menu();
        m.settings.race.opponents = 2;
        m.settings.race.difficulty = Difficulty::Rollcage;
        m.settings.race.weapons = false;

        let sim = crate::sim::Sim::with_setup(7, m.settings.race);
        assert_eq!(sim.opponents.len(), 2, "the field is not the size that was asked for");
        assert!(!sim.weapons.enabled, "weapons were left on after being turned off");
        let (low, _) = Difficulty::Rollcage.skill_range();
        assert!(
            sim.opponents.iter().all(|o| o.skill >= low),
            "the field is not racing at the difficulty that was chosen"
        );
    }
}
