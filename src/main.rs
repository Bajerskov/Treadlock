mod sim;
mod track;
mod vehicle;

use sim::{Sim, TICK_DT};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seed = args
        .iter()
        .position(|a| a == "--seed")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(7u64);

    if args.iter().any(|a| a == "--headless") {
        let seconds = args
            .iter()
            .position(|a| a == "--headless")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(60.0f32);
        headless(seed, seconds);
        return;
    }

    println!("treadlock: renderer not wired up yet; use --headless <seconds>");
}

/// Drive the track on autopilot with no renderer, and report whether the car
/// actually behaves. This is the only way to validate handling on a machine
/// with no GPU.
fn headless(seed: u64, seconds: f32) {
    let mut sim = Sim::new(seed);
    println!(
        "track seed {} | {:.0} m | {} frames",
        seed,
        sim.track.length,
        sim.track.frames.len()
    );

    let ticks = (seconds / TICK_DT) as usize;
    let mut top_speed = 0.0f32;
    let mut speed_sum = 0.0f64;
    let mut airborne_ticks = 0usize;
    let mut escaped = 0usize;

    let trace = std::env::args().any(|a| a == "--trace");
    let trace_every = sim::TICK_RATE as usize;

    for tick in 0..ticks {
        let controls = vehicle::autopilot(&sim.track, &sim.player, 26.0);
        sim.tick(&controls, TICK_DT);

        let fine = tick < sim::TICK_RATE as usize * 3 && tick % 6 == 0;
        if trace && (fine || tick % trace_every == 0) {
            let surf = sim.track.surface(sim.player.pos, sim.player.hint);
            let up_err = sim.player.up().dot(-surf.down);
            println!(
                "t={:5.1}s spd={:6.1}km/h gap={:6.2}m contacts={} idx={:4} dist={:7.1}m up.n={:+.2} angvel={:5.2}",
                sim.time,
                sim.player.speed_kph(),
                surf.gap,
                sim.player.contacts,
                surf.index,
                sim.player.distance,
                up_err,
                sim.player.ang_vel.length()
            );
            println!(
                "        v.down={:+7.2} comp=[{:.2} {:.2} {:.2} {:.2}]",
                sim.player.vel.dot(surf.down),
                sim.player.wheels[0].compression,
                sim.player.wheels[1].compression,
                sim.player.wheels[2].compression,
                sim.player.wheels[3].compression,
            );
        }

        let s = sim.player.speed();
        if !s.is_finite() || !sim.player.pos.is_finite() {
            println!("FAIL: simulation diverged (non-finite state)");
            std::process::exit(1);
        }
        top_speed = top_speed.max(s);
        speed_sum += s as f64;
        if !sim.player.grounded {
            airborne_ticks += 1;
        }
        // Outside the tube wall by more than a chassis is a containment failure.
        let surf = sim.track.surface(sim.player.pos, sim.player.hint);
        if surf.gap < -1.0 {
            escaped += 1;
        }
    }

    let avg = speed_sum / ticks as f64;
    println!(
        "after {:.0}s: laps {} | avg {:.1} km/h | top {:.1} km/h | airborne {:.1}% | outside wall {} ticks",
        seconds,
        sim.lap,
        avg * 3.6,
        top_speed * 3.6,
        airborne_ticks as f32 / ticks as f32 * 100.0,
        escaped
    );
    if sim.best_lap_time.is_finite() {
        println!("best lap {:.2}s", sim.best_lap_time);
    }
    println!("distance along track {:.0} m", sim.player.distance);
}
