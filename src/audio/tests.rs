//! The mixer is pure, so the whole sound of the game can be rendered into a
//! buffer here and measured. Everything below runs on a machine with no sound
//! card, which is where it was written.

use super::*;
use crate::sim::TICK_DT;

const RATE: f32 = 48_000.0;

fn energy(samples: &[f32], channel: usize, channels: usize) -> f32 {
    samples
        .chunks(channels)
        .map(|f| f[channel] * f[channel])
        .sum::<f32>()
        / (samples.len() / channels).max(1) as f32
}

/// Count sign changes, ignoring wobble around zero, so a little noise riding on
/// a tone does not read as extra cycles. Two crossings make a period.
fn pitch_hz(samples: &[f32], channels: usize, rate: f32) -> f32 {
    let threshold = 0.08;
    let mut crossings = 0;
    let mut above = false;
    let mut frames = 0;
    for frame in samples.chunks(channels) {
        frames += 1;
        if !above && frame[0] > threshold {
            above = true;
            crossings += 1;
        } else if above && frame[0] < -threshold {
            above = false;
            crossings += 1;
        }
    }
    crossings as f32 / 2.0 / (frames as f32 / rate)
}

fn loud_scene() -> Scene {
    let mut scene = Scene {
        rpm: 1.0,
        throttle: 1.0,
        slip: 1.0,
        speed: 190.0,
        boost: 1.0,
        enclosure: 1.0,
        sources: [Source::default(); MAX_SOURCES],
        music: 1.0,
    };
    // Every opponent right on top of the listener, which is the worst case the
    // mix will ever be asked for.
    for (i, source) in scene.sources.iter_mut().enumerate() {
        *source = Source {
            offset: Vec3::new(i as f32 - 3.5, 0.0, 2.0),
            closing: 60.0,
            speed: 150.0,
            rpm: 1.0,
            throttle: 1.0,
            active: true,
        };
    }
    scene
}

/// Everything at once must still come out as audio. A NaN here is silent on
/// some drivers and a full-scale square wave on others, which is how a mixing
/// bug turns into a damaged pair of speakers.
#[test]
fn a_full_mix_stays_finite_and_within_range() {
    let mut mixer = Mixer::new(RATE, 7);
    mixer.set_scene(loud_scene());
    for _ in 0..40 {
        mixer.fire(Cue::Impact, 1.0);
        mixer.fire(Cue::BoostPickup, 1.0);
    }

    let mut out = vec![0.0f32; 48_000 * 2];
    mixer.render(&mut out, 2);

    let mut peak = 0.0f32;
    for (i, s) in out.iter().enumerate() {
        assert!(s.is_finite(), "sample {i} was {s}");
        peak = peak.max(s.abs());
    }
    assert!(peak <= 1.0, "peak {peak} exceeded full scale");
    assert!(peak > 0.05, "a full mix produced near silence, peak {peak}");
}

/// Nothing happening must be genuinely nothing: no hiss, and no DC offset,
/// which is inaudible on its own and eats headroom from everything else.
#[test]
fn a_quiet_scene_is_quiet_and_free_of_offset() {
    let mut mixer = Mixer::new(RATE, 7);
    mixer.levels = Levels { master: 0.8, engine: 0.0, effects: 0.0, ambient: 0.0, music: 0.0 };
    mixer.set_scene(Scene::default());

    let mut out = vec![0.0f32; 24_000 * 2];
    mixer.render(&mut out, 2);
    // Skip the start, where the glides are still settling from their initial
    // values towards zero.
    let settled = &out[12_000..];
    let peak = settled.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    let mean = settled.iter().sum::<f32>() / settled.len() as f32;
    assert!(peak < 0.01, "idle mix peaked at {peak}");
    assert!(mean.abs() < 0.001, "idle mix has a DC offset of {mean}");
}

/// The engine note has to track the car. If it does not, the most important
/// single cue in a racing game is decoration.
#[test]
fn the_engine_note_rises_with_revs() {
    let measure = |rpm: f32| {
        let mut mixer = Mixer::new(RATE, 7);
        mixer.levels = Levels { engine: 1.0, effects: 0.0, ambient: 0.0, music: 0.0, master: 1.0 };
        mixer.set_scene(Scene { rpm, throttle: 0.0, ..Scene::default() });
        let mut out = vec![0.0f32; 24_000 * 2];
        mixer.render(&mut out, 2);
        // The first half is the pitch gliding up to target.
        pitch_hz(&out[24_000..], 2, RATE)
    };

    let low = measure(0.05);
    let high = measure(0.95);
    assert!(
        high > low * 2.0,
        "engine pitch barely moved across the rev range: {low:.0} Hz to {high:.0} Hz"
    );
    // Against the formula the voice is built from, so a silent rescaling of the
    // note cannot pass unnoticed.
    assert!((low - (42.0 + 268.0 * 0.05)).abs() < 12.0, "idle note was {low:.0} Hz");
    assert!((high - (42.0 + 268.0 * 0.95)).abs() < 25.0, "top note was {high:.0} Hz");
}

/// Shifting up has to drop the note. Without it the engine climbs forever and
/// the car sounds like a slot car.
#[test]
fn revs_drop_at_a_gearshift() {
    let mut dropped = false;
    let mut last = voices::engine_rpm(0.0);
    let mut speed = 0.0;
    while speed < 150.0 {
        speed += 0.25;
        let rpm = voices::engine_rpm(speed);
        if rpm < last - 0.2 {
            dropped = true;
        }
        assert!((0.0..=1.0).contains(&rpm), "rpm {rpm} out of range at {speed} m/s");
        last = rpm;
    }
    assert!(dropped, "the engine never shifted; revs climb without limit");
}

/// Doppler, stated as the thing it has to do rather than as the formula, so a
/// sign slip cannot survive.
#[test]
fn approaching_pitches_up_and_receding_pitches_down() {
    assert!(synth::doppler(50.0) > 1.0, "an approaching car was not pitched up");
    assert!(synth::doppler(-50.0) < 1.0, "a receding car was not pitched down");
    assert!((synth::doppler(0.0) - 1.0).abs() < 1e-6);
    // Faster than sound is possible in this game, and must not divide by zero.
    for closing in [-900.0, -343.0, 342.9, 343.0, 900.0, f32::MAX] {
        let d = synth::doppler(closing);
        assert!(d.is_finite() && d > 0.0, "doppler({closing}) = {d}");
    }
}

/// A car on the right has to be louder on the right, and must get quieter as it
/// goes away. Getting the pan backwards is invisible in a waveform and obvious
/// in a chair.
#[test]
fn a_passing_car_is_placed_and_attenuated() {
    // Measured on the voice rather than through the mixer. The player's own
    // engine is deliberately centred and sits in both channels, and at these
    // levels it swamps the difference this is trying to see; the mixer's own
    // stereo path is covered separately below.
    let render = |offset: Vec3| {
        let mut passby = voices::Passby::new(3);
        let source = Source {
            offset,
            closing: 0.0,
            speed: 80.0,
            rpm: 0.6,
            throttle: 0.8,
            active: true,
        };
        let mut left = 0.0f32;
        let mut right = 0.0f32;
        let frames = 24_000;
        for i in 0..frames {
            let (l, r) = passby.render(&source, 1.0, RATE);
            // Skip the first half, where the gain glides are still settling.
            if i >= frames / 2 {
                left += l * l;
                right += r * r;
            }
        }
        let n = (frames / 2) as f32;
        (left / n, right / n)
    };

    let (l, r) = render(Vec3::new(10.0, 0.0, 0.0));
    assert!(r > l * 8.0, "a car on the right was not louder on the right: {l:.6} vs {r:.6}");
    let (l, r) = render(Vec3::new(-10.0, 0.0, 0.0));
    assert!(l > r * 8.0, "a car on the left was not louder on the left: {l:.6} vs {r:.6}");

    let (near, _) = render(Vec3::new(-6.0, 0.0, 0.0));
    let (far, _) = render(Vec3::new(-120.0, 0.0, 0.0));
    assert!(far < near * 0.2, "distance barely changed the level: {near:.6} vs {far:.6}");
}

/// And that the mixer actually carries that placement through to the output,
/// rather than summing the voices to mono on the way past.
#[test]
fn the_mixer_keeps_the_channels_apart() {
    let mut mixer = Mixer::new(RATE, 7);
    mixer.levels = Levels { engine: 1.0, effects: 0.0, ambient: 0.0, music: 0.0, master: 1.0 };
    let mut scene = Scene { rpm: 0.0, ..Scene::default() };
    scene.sources[0] = Source {
        offset: Vec3::new(9.0, 0.0, 0.0),
        closing: 0.0,
        speed: 90.0,
        rpm: 0.7,
        throttle: 0.9,
        active: true,
    };
    mixer.set_scene(scene);
    let mut out = vec![0.0f32; 24_000 * 2];
    mixer.render(&mut out, 2);

    let settled = &out[24_000..];
    let difference = settled
        .chunks(2)
        .map(|f| (f[0] - f[1]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        difference > 0.01,
        "the two channels were identical, so the mix is mono: max difference {difference}"
    );
}

/// A one-shot has to end, and its slot has to come back. A stuck voice is a
/// tone that never stops.
#[test]
fn one_shots_finish_and_release_their_slots() {
    let mut mixer = Mixer::new(RATE, 7);
    mixer.set_scene(Scene { rpm: 0.0, speed: 0.0, enclosure: 0.0, music: 0.0, ..Scene::default() });
    mixer.levels = Levels { engine: 0.0, ambient: 0.0, music: 0.0, ..Levels::default() };

    for cue in [
        Cue::BoostPickup,
        Cue::Impact,
        Cue::Respawn,
        Cue::Suspension,
        Cue::PadCharge,
        Cue::WeaponPickup,
        Cue::RocketLaunch,
        Cue::Explosion,
        Cue::ShieldUp,
        Cue::ShieldBreak,
    ] {
        mixer.fire(cue, 1.0);
        let mut during = vec![0.0f32; 2_400 * 2];
        mixer.render(&mut during, 2);
        assert!(
            during.iter().any(|s| s.abs() > 0.01),
            "{cue:?} made no sound"
        );

        // Two seconds, and only the last of them is examined: the longest cue
        // runs 0.9 s, and a window that opens before it ends measures the cue
        // rather than the silence after it.
        let mut after = vec![0.0f32; 96_000 * 2];
        mixer.render(&mut after, 2);
        let tail = &after[96_000..];
        let peak = tail.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak < 0.005, "{cue:?} was still sounding after a second, peak {peak}");
    }
    assert!(
        mixer.oneshots.iter().all(|s| !s.active),
        "a one-shot slot was never released"
    );
}

/// Firing far more cues than there are slots must not grow the voice count or
/// the level. A twelve-car pile-up should not be louder than a crash.
#[test]
fn the_one_shot_pool_is_bounded() {
    let mut mixer = Mixer::new(RATE, 7);
    mixer.levels = Levels { engine: 0.0, ambient: 0.0, music: 0.0, ..Levels::default() };
    mixer.set_scene(Scene { rpm: 0.0, speed: 0.0, enclosure: 0.0, music: 0.0, ..Scene::default() });
    for _ in 0..500 {
        mixer.fire(Cue::Impact, 1.0);
    }
    assert_eq!(mixer.oneshots.len(), MAX_ONESHOTS);
    let mut out = vec![0.0f32; 4_800 * 2];
    mixer.render(&mut out, 2);
    assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
}

/// The procedural bed is the fallback for a missing music file, so it has to
/// actually play, and keep playing.
#[test]
fn the_generated_music_bed_plays() {
    let mut mixer = Mixer::new(RATE, 7);
    mixer.levels = Levels { engine: 0.0, effects: 0.0, ambient: 0.0, music: 1.0, master: 1.0 };
    mixer.set_scene(Scene { rpm: 0.0, speed: 0.0, enclosure: 0.0, ..Scene::default() });

    let mut out = vec![0.0f32; 48_000 * 2 * 4];
    mixer.render(&mut out, 2);
    assert!(out.iter().all(|s| s.is_finite()));
    // Four seconds is several bars at 148 BPM: both halves must have notes in
    // them, or the bed has run down rather than looped.
    let first = energy(&out[..out.len() / 2], 0, 2);
    let second = energy(&out[out.len() / 2..], 0, 2);
    assert!(first > 1e-5, "the music bed was silent");
    assert!(second > first * 0.25, "the music bed faded out instead of looping");
}

/// A recorded track, once present, replaces the generated one.
#[test]
fn a_supplied_clip_takes_over_from_the_bed() {
    let mut mixer = Mixer::new(RATE, 7);
    mixer.levels = Levels { engine: 0.0, effects: 0.0, ambient: 0.0, music: 1.0, master: 1.0 };
    mixer.set_scene(Scene { rpm: 0.0, speed: 0.0, enclosure: 0.0, ..Scene::default() });
    // A short DC clip: nothing the synth would ever produce, so its presence in
    // the output is proof the file is what is playing.
    mixer.attach_music(wav::Clip { samples: vec![0.5; 2_000], rate: RATE });

    let mut out = vec![0.0f32; 4_800 * 2];
    mixer.render(&mut out, 2);
    let mean = out.iter().sum::<f32>() / out.len() as f32;
    assert!(mean > 0.05, "the supplied clip did not replace the generated bed (mean {mean})");
}

/// Mono and surround devices must both get something sane rather than a
/// buffer-length mismatch or a silent front pair.
#[test]
fn other_channel_counts_are_handled() {
    for channels in [1usize, 2, 6] {
        let mut mixer = Mixer::new(RATE, 7);
        mixer.set_scene(loud_scene());
        let mut out = vec![9.0f32; 1_024 * channels];
        mixer.render(&mut out, channels);
        assert!(
            out.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
            "{channels} channels produced out-of-range samples"
        );
        assert!(
            out.chunks(channels).any(|f| f[0].abs() > 0.01),
            "{channels} channels produced silence on the first channel"
        );
    }
}

/// The listener is the camera, not the car. An opponent that is visibly on the
/// left of the screen has to be on the left in the mix.
#[test]
fn opponents_are_heard_where_the_camera_sees_them() {
    let mut sim = crate::sim::Sim::new(7);
    let mut camera = Camera::new(&sim.player);
    camera.snap(&sim.player, &sim.track);
    for _ in 0..240 {
        let controls = crate::vehicle::autopilot(&sim.track, &sim.player, 26.0);
        sim.tick(&controls, TICK_DT);
        camera.update(&sim.player, &sim.track, TICK_DT);
    }

    let scene = observe(&sim, &camera, 1.0);
    assert!(scene.rpm > 0.0 && scene.rpm <= 1.0);
    assert!(scene.speed > 1.0, "the car was not moving after four seconds");
    assert!((0.0..=1.0).contains(&scene.enclosure));

    let forward = (camera.target - camera.pos).normalize();
    let right = forward.cross(camera.up).normalize();
    let active = scene.sources.iter().filter(|s| s.active).count();
    assert!(active > 0, "no opponents were audible");

    for (source, opponent) in scene.sources.iter().zip(sim.opponents.iter()) {
        if !source.active {
            continue;
        }
        let to = opponent.car.pos - camera.pos;
        assert!(
            (source.offset.x - to.dot(right)).abs() < 0.01,
            "an opponent was placed on the wrong side of the listener"
        );
        assert!(
            (source.offset.length() - to.length()).abs() < 0.05,
            "the listener frame is not a rotation: it changed the distance"
        );
        assert!(source.closing.abs() < 400.0, "implausible closing speed {}", source.closing);
    }
}

/// The facade must survive having no device, because that is the normal case on
/// a build machine and must not be the difference between a test passing here
/// and the game working there.
#[test]
fn a_silent_audio_handle_still_mixes() {
    let audio = Audio::silent(7, RATE);
    audio.update(loud_scene());
    audio.fire(Cue::BoostPickup, 1.0);
    let mut out = vec![0.0f32; 2_048];
    audio.render_block(&mut out, 2);
    assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    assert!(out.iter().any(|s| s.abs() > 0.01), "the silent handle rendered nothing at all");
}
