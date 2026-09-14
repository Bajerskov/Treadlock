mod camera;
mod gfx;
mod input;
mod mesh;
mod model;
mod sim;
mod track;
mod vehicle;

use std::time::Instant;

use glam::{Mat4, Quat, Vec3};
use winit::event::{ElementState, Event, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};

use camera::Camera;
use gfx::context::Context;
use gfx::renderer::{Draw, GpuMesh, Renderer};
use gfx::swapchain::Swapchain;
use sim::{Sim, TICK_DT};

struct Args {
    seed: u64,
    headless: Option<f32>,
    trace: bool,
    vsync: bool,
    /// Path to a .glb or .gltf car model. Falls back to the procedural box.
    car: Option<String>,
    /// Orientation and scale corrections for that model.
    car_fit: model::Fit,
}

/// Read `--flag value`. A flag that is present but missing or malformed is an
/// error rather than a silent fall back to the default, so a mistyped option
/// cannot look like it worked.
fn arg_value<T: std::str::FromStr>(argv: &[String], flag: &str) -> Option<T> {
    let index = argv.iter().position(|a| a == flag)?;
    // A negative number is a value; another `--option` is not.
    match argv.get(index + 1) {
        Some(raw) if !raw.starts_with("--") => match raw.parse() {
            Ok(value) => Some(value),
            Err(_) => fail(&format!("{flag}: could not read a value from {raw:?}")),
        },
        _ => fail(&format!("{flag}: expected a value after it")),
    }
}

/// Read `--flag [value]`, where omitting the value is allowed and means
/// `default`.
fn optional_value<T: std::str::FromStr>(argv: &[String], flag: &str, default: T) -> Option<T> {
    let index = argv.iter().position(|a| a == flag)?;
    match argv.get(index + 1) {
        Some(raw) if !raw.starts_with("--") => match raw.parse() {
            Ok(value) => Some(value),
            Err(_) => fail(&format!("{flag}: could not read a value from {raw:?}")),
        },
        _ => Some(default),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    Args {
        seed: arg_value(&argv, "--seed").unwrap_or(7),
        // Bare `--headless` is valid and means a default run length.
        headless: optional_value(&argv, "--headless", 60.0),
        trace: argv.iter().any(|a| a == "--trace"),
        vsync: !argv.iter().any(|a| a == "--no-vsync"),
        car: arg_value(&argv, "--car"),
        car_fit: model::Fit {
            yaw_degrees: arg_value(&argv, "--car-yaw").unwrap_or(0.0),
            pitch_degrees: arg_value(&argv, "--car-pitch").unwrap_or(0.0),
            scale: arg_value(&argv, "--car-scale").unwrap_or(1.0),
        },
    }
}

fn main() {
    let args = parse_args();
    // Shader translation needs no GPU, so it can be checked on a build machine
    // that has no Vulkan driver at all.
    if std::env::args().any(|a| a == "--check-shaders") {
        let spirv = gfx::shader::compile(include_str!("shaders/forward.wgsl"));
        println!("forward.wgsl compiled: {} words of SPIR-V", spirv.len());
        return;
    }
    // Loading and refitting a model needs no GPU either, so a file can be
    // checked before committing to a run.
    if std::env::args().any(|a| a == "--check-model") {
        let Some(path) = args.car.as_ref() else {
            eprintln!("--check-model needs --car <path to .glb>");
            std::process::exit(2);
        };
        match model::Model::load(path, args.car_fit) {
            Ok(m) => {
                let tris = m.chassis.indices.len() / 3;
                if tris > 40_000 {
                    println!(
                        "warning: {tris} triangles is heavy for a 24 CU GPU; consider decimating to 20-30k"
                    );
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }

    match args.headless {
        Some(seconds) => headless(args.seed, seconds, args.trace),
        None => run(args),
    }
}

fn run(args: Args) {
    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let window = winit::window::WindowBuilder::new()
        .with_title("Treadlock")
        .with_inner_size(winit::dpi::LogicalSize::new(1600, 900))
        .build(&event_loop)
        .expect("failed to create window");

    let mut ctx = Context::new(&window);
    println!(
        "gpu: {} | ray query: {}",
        ctx.device_name,
        if ctx.ray_query { "yes" } else { "no (screen-space reflections only)" }
    );

    let size = window.inner_size();
    let mut swapchain = Swapchain::new(&mut ctx, size.width, size.height, args.vsync);
    let mut renderer = Renderer::new(&mut ctx, &swapchain);

    let mut sim = Sim::new(args.seed);
    println!(
        "track: seed {} | {:.0} m | {} frames",
        args.seed,
        sim.track.length,
        sim.track.frames.len()
    );

    let mut track_mesh = GpuMesh::upload(
        &mut ctx,
        "track",
        &sim.track.vertices,
        &sim.track.indices,
    );
    // A loaded model replaces the procedural body. If it has no separately
    // named wheel node its wheels are already modelled into the body, so drawing
    // the engine's own wheels on top would double them up.
    let loaded = args.car.as_ref().and_then(|path| match model::Model::load(path, args.car_fit) {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("{e}\nfalling back to the procedural car");
            None
        }
    });
    let show_wheels = loaded.as_ref().map_or(true, |m| m.wheel.is_some());

    let mut chassis_mesh = match &loaded {
        Some(m) => GpuMesh::from_mesh(&mut ctx, "chassis", &m.chassis),
        None => GpuMesh::from_mesh(
            &mut ctx,
            "chassis",
            &mesh::chassis(Vec3::new(1.15, 0.45, 2.1), 0.72),
        ),
    };
    let mut wheel_mesh = match loaded.as_ref().and_then(|m| m.wheel.as_ref()) {
        Some(w) => GpuMesh::from_mesh(&mut ctx, "wheel", w),
        None => GpuMesh::from_mesh(&mut ctx, "wheel", &mesh::wheel(0.62, 0.22, 16)),
    };

    let mut camera = Camera::new(&sim.player);
    let mut input = input::Input::new();
    println!("gamepads: {}", input.gamepad_count());

    let mut last = Instant::now();
    let mut fps_timer = Instant::now();
    let mut frames = 0u32;
    let mut destroyed = false;

    event_loop
        .run(move |event, target| match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    if size.width > 0 && size.height > 0 {
                        swapchain.recreate(&mut ctx, size.width, size.height, args.vsync);
                        renderer.rebuild_pipeline(&mut ctx, &swapchain);
                        renderer.needs_resize = false;
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    let pressed = event.state == ElementState::Pressed;
                    if let PhysicalKey::Code(code) = event.physical_key {
                        match code {
                            KeyCode::Escape => target.exit(),
                            KeyCode::KeyW | KeyCode::ArrowUp => input.keys.up = pressed,
                            KeyCode::KeyS | KeyCode::ArrowDown => input.keys.down = pressed,
                            KeyCode::KeyA | KeyCode::ArrowLeft => input.keys.left = pressed,
                            KeyCode::KeyD | KeyCode::ArrowRight => input.keys.right = pressed,
                            KeyCode::ShiftLeft => input.keys.boost = pressed,
                            KeyCode::Space => input.keys.handbrake = pressed,
                            KeyCode::KeyR if pressed => {
                                sim.player.respawn(&sim.track);
                                camera.snap(&sim.player, &sim.track);
                            }
                            _ => {}
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    let now = Instant::now();
                    let dt = (now - last).as_secs_f32().min(0.1);
                    last = now;

                    input.poll();
                    if input.take_respawn() {
                        sim.player.respawn(&sim.track);
                        camera.snap(&sim.player, &sim.track);
                    }
                    sim.update(&input.controls(), dt);
                    camera.follow(&sim.player, &sim.track, dt);

                    if renderer.needs_resize {
                        let size = window.inner_size();
                        if size.width > 0 && size.height > 0 {
                            swapchain.recreate(&mut ctx, size.width, size.height, args.vsync);
                            renderer.needs_resize = false;
                        }
                    }

                    let aspect =
                        swapchain.extent.width as f32 / swapchain.extent.height.max(1) as f32;
                    let draws =
                        build_draws(&sim, &track_mesh, &chassis_mesh, &wheel_mesh, show_wheels);
                    renderer.draw(
                        &mut ctx,
                        &swapchain,
                        camera.view_proj(aspect),
                        camera.pos,
                        sim.time,
                        &draws,
                    );

                    frames += 1;
                    if fps_timer.elapsed().as_secs_f32() >= 1.0 {
                        let fps = frames as f32 / fps_timer.elapsed().as_secs_f32();
                        window.set_title(&format!(
                            "Treadlock - {:.0} fps - {:.0} km/h - lap {}",
                            fps,
                            sim.player.speed_kph(),
                            sim.lap + 1
                        ));
                        frames = 0;
                        fps_timer = Instant::now();
                    }
                }
                _ => {}
            },
            Event::AboutToWait => window.request_redraw(),
            Event::LoopExiting => {
                // Tear down GPU resources while the device is still alive.
                if !destroyed {
                    destroyed = true;
                    renderer.destroy(&mut ctx);
                    track_mesh.destroy(&mut ctx);
                    chassis_mesh.destroy(&mut ctx);
                    wheel_mesh.destroy(&mut ctx);
                    swapchain.destroy(&mut ctx);
                }
            }
            _ => {}
        })
        .expect("event loop failed");
}

fn build_draws<'a>(
    sim: &Sim,
    track_mesh: &'a GpuMesh,
    chassis_mesh: &'a GpuMesh,
    wheel_mesh: &'a GpuMesh,
    show_wheels: bool,
) -> Vec<Draw<'a>> {
    let mut draws = vec![Draw {
        mesh: track_mesh,
        model: Mat4::IDENTITY,
        tint: Vec3::new(0.30, 0.33, 0.40),
        surface: 1.0,
        emissive: 0.0,
        metallic: 0.35,
    }];

    let car = &sim.player;
    let body = Mat4::from_rotation_translation(car.rot, car.pos);
    let boosting = car.boost < 0.999;
    draws.push(Draw {
        mesh: chassis_mesh,
        model: body,
        tint: Vec3::new(0.85, 0.16, 0.10),
        surface: 0.0,
        emissive: if boosting { 0.6 } else { 0.0 },
        metallic: 0.85,
    });

    if !show_wheels {
        return draws;
    }
    for wheel in &car.wheels {
        // Wheels are positioned by the physics contact solve, then spun about
        // their axle for the visual.
        let spin = Quat::from_rotation_x(wheel.spin_angle);
        draws.push(Draw {
            mesh: wheel_mesh,
            model: Mat4::from_rotation_translation(car.rot * spin, wheel.world_pos),
            tint: Vec3::new(0.09, 0.09, 0.11),
            surface: 0.0,
            emissive: 0.0,
            metallic: 0.2,
        });
    }
    draws
}

/// Drive the track on autopilot with no renderer, and report whether the car
/// actually behaves. This is the only way to validate handling on a machine
/// with no GPU.
fn headless(seed: u64, seconds: f32, trace: bool) {
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
    let mut inverted_ticks = 0usize;

    for tick in 0..ticks {
        let controls = vehicle::autopilot(&sim.track, &sim.player, 26.0);
        sim.tick(&controls, TICK_DT);

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
        let surf = sim.track.surface(sim.player.pos, sim.player.hint);
        // Outside the tube wall by more than a chassis is a containment failure.
        if surf.gap < -1.0 {
            escaped += 1;
        }
        // Roof pointing into the wall rather than at the tube axis.
        if sim.player.up().dot(-surf.down) < 0.0 {
            inverted_ticks += 1;
        }

        if trace && tick % sim::TICK_RATE as usize == 0 {
            println!(
                "t={:5.1}s spd={:6.1}km/h gap={:6.2}m contacts={} dist={:7.1}m up.n={:+.2}",
                sim.time,
                sim.player.speed_kph(),
                surf.gap,
                sim.player.contacts,
                sim.player.distance,
                sim.player.up().dot(-surf.down),
            );
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
    println!(
        "inverted {:.1}% of the time",
        inverted_ticks as f32 / ticks as f32 * 100.0
    );
    if sim.best_lap_time.is_finite() {
        println!("best lap {:.2}s", sim.best_lap_time);
    }
}
