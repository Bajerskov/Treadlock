mod assets;
mod audio;
mod camera;
mod effects;
mod gfx;
mod input;
mod mesh;
mod model;
mod particles;
mod plates;
mod scenery;
mod sim;
mod track;
mod ui;
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
    /// Label the screen corners, to confirm which way the overlay is mapped.
    debug_hud: bool,
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
        debug_hud: argv.iter().any(|a| a == "--debug-hud"),
        vsync: !argv.iter().any(|a| a == "--no-vsync"),
        car: arg_value(&argv, "--car"),
        car_fit: model::Fit {
            yaw_degrees: arg_value(&argv, "--car-yaw").unwrap_or(0.0),
            pitch_degrees: arg_value(&argv, "--car-pitch").unwrap_or(0.0),
            roll_degrees: arg_value(&argv, "--car-roll").unwrap_or(0.0),
            scale: arg_value(&argv, "--car-scale").unwrap_or(1.0),
        },
    }
}

fn main() {
    let args = parse_args();
    // Shader translation needs no GPU, so it can be checked on a build machine
    // that has no Vulkan driver at all.
    if std::env::args().any(|a| a == "--check-shaders") {
        for (name, source) in [
            ("forward.wgsl", include_str!("shaders/forward.wgsl")),
            ("particles.wgsl", include_str!("shaders/particles.wgsl")),
            ("ui.wgsl", include_str!("shaders/ui.wgsl")),
        ] {
            let spirv = gfx::shader::compile(source);
            println!("{name} compiled: {} words of SPIR-V", spirv.len());
        }
        return;
    }
    // The asset library is inspectable without launching anything, so "what is
    // still missing?" is a command rather than something to remember.
    if std::env::args().any(|a| a == "--assets") {
        assets::Library::load("assets").report(true);
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
            &mesh::chassis(vehicle::HALF_EXTENTS, 0.72),
        ),
    };
    let mut wheel_mesh = match loaded.as_ref().and_then(|m| m.wheel.as_ref()) {
        Some(w) => GpuMesh::from_mesh(&mut ctx, "wheel", w),
        None => GpuMesh::from_mesh(&mut ctx, "wheel", &mesh::wheel(0.62, 0.22, 16)),
    };

    let (pad_vertices, pad_indices) = sim.track.boost_pad_mesh();
    let mut pad_mesh = GpuMesh::upload(&mut ctx, "boost pads", &pad_vertices, &pad_indices);
    println!("boost pads: {}", sim.track.boost_pads.len());

    // Upload the model's base colour map, if it brought one.
    let mut car_texture = loaded.as_ref().and_then(|m| m.base_color.as_ref()).map(|image| {
        gfx::texture::Texture::new(
            &mut ctx,
            renderer.texture_layout,
            renderer.texture_pool,
            image.width,
            image.height,
            &image.rgba,
        )
    });

    let library = assets::Library::load("assets");
    let missing = library.missing().len();
    if missing > 0 {
        println!("assets: {missing} not present, using generated stand-ins (--assets to list)");
    }

    // The skyline and the sky behind it. Both are generated from the track seed
    // when the library has no file for them, so the world outside the tube is
    // never empty.
    let scenery = scenery::generate(&sim.track, args.seed, &library);
    println!("scenery: {} triangles", scenery.props.indices.len() / 3);
    let mut prop_mesh = GpuMesh::from_mesh(&mut ctx, "scenery", &scenery.props);
    let mut sky_mesh = GpuMesh::from_mesh(&mut ctx, "sky", &scenery.sky);
    let sky_plate = plates::backdrop(&library, args.seed, 2048, 1024);
    let mut sky_texture = gfx::texture::Texture::new(
        &mut ctx,
        renderer.texture_layout,
        renderer.texture_pool,
        sky_plate.width,
        sky_plate.height,
        &sky_plate.rgba,
    );

    // The HUD font is rasterised from a table in code, so there is no asset to
    // load. Nearest filtering keeps the pixels crisp at any scale.
    let mut font_texture = gfx::texture::Texture::new_pixel_art(
        &mut ctx,
        renderer.texture_layout,
        renderer.texture_pool,
        ui::ATLAS_WIDTH,
        ui::ATLAS_HEIGHT,
        &ui::build_atlas(),
    );
    let mut hud = ui::Ui::default();
    // Hand the player car to the same driver the opponents use, for watching
    // the car from outside while it races.
    let mut ai_driving = false;
    let mut dragging = false;
    let mut last_cursor: Option<glam::Vec2> = None;

    let mut particles = particles::Particles::new();
    let mut camera = Camera::new(&sim.player);
    camera.snap(&sim.player, &sim.track);
    let mut input = input::Input::new();
    println!("gamepads: {}", input.gamepad_count());

    let audio = audio::Audio::new(args.seed, &library);
    let mut cues = audio::Cues::new();

    // Orientation at the start line, so a report of "it looks upside down" can
    // be pinned on the engine or on the model rather than guessed at. All three
    // near +1.00 means the engine is upright and any remaining flip is baked
    // into the model's own geometry.
    {
        let surf = sim.track.surface(sim.player.pos, sim.player.hint);
        let interior = -surf.down;
        println!(
            "spawn check: car roof . tube interior {:+.2} | camera up . tube interior {:+.2} \
             | camera inside wall by {:.1} m | car facing along track {:+.2}",
            sim.player.up().dot(interior),
            camera.up.dot(interior),
            sim.track.surface(camera.pos, sim.player.hint).gap,
            sim.player.forward().dot(surf.tangent),
        );
    }

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
                                cues.respawned(&audio);
                            }
                            KeyCode::KeyP if pressed => {
                                ai_driving = !ai_driving;
                                println!(
                                    "player car: {}",
                                    if ai_driving { "AI" } else { "manual" }
                                );
                            }
                            KeyCode::KeyC if pressed => {
                                camera.toggle_mode();
                                // Snapping on the way back avoids a long sweep
                                // in from wherever the orbit left the camera.
                                if camera.mode == camera::Mode::Chase {
                                    camera.snap(&sim.player, &sim.track);
                                }
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
                        cues.respawned(&audio);
                    }
                    // Under AI the player car takes the same driver the
                    // opponents use, so what you are watching is the real
                    // racing line rather than a separate demo mode.
                    let controls = if ai_driving {
                        let lookahead = 26.0 + sim.player.speed() * 0.12;
                        vehicle::autopilot_lane(&sim.track, &sim.player, lookahead, 0.0)
                    } else {
                        input.controls()
                    };
                    sim.update(&controls, dt);
                    camera.update(&sim.player, &sim.track, dt);
                    cues.poll(&sim, &audio, dt);
                    audio.update(audio::observe(&sim, &camera, controls.throttle));

                    if renderer.needs_resize {
                        let size = window.inner_size();
                        if size.width > 0 && size.height > 0 {
                            swapchain.recreate(&mut ctx, size.width, size.height, args.vsync);
                            renderer.needs_resize = false;
                        }
                    }

                    let aspect =
                        swapchain.extent.width as f32 / swapchain.extent.height.max(1) as f32;
                    let controls = input.controls();
                    effects::update(&mut particles, &sim, dt, controls.throttle, controls.boost);
                    let (right, up) = camera.basis();
                    let particle_vertices = particles.build_vertices(right, up).to_vec();

                    let world = World {
                        track: &track_mesh,
                        pads: &pad_mesh,
                        props: &prop_mesh,
                        sky: &sky_mesh,
                        sky_texture: &sky_texture,
                    };
                    let draws = build_draws(
                        &sim,
                        &world,
                        &chassis_mesh,
                        &wheel_mesh,
                        car_texture.as_ref(),
                        show_wheels,
                        camera.pos,
                    );
                    let (screen_w, screen_h) =
                        (swapchain.extent.width as f32, swapchain.extent.height as f32);
                    ui::build_hud(&mut hud, &sim, screen_w, screen_h);
                    if sim.wrong_way() {
                        ui::build_wrong_way(&mut hud, screen_w, screen_h, sim.time);
                    }
                    ui::build_mode_banner(
                        &mut hud,
                        screen_w,
                        screen_h,
                        ai_driving,
                        camera.mode == camera::Mode::Orbit,
                    );
                    if args.debug_hud {
                        ui::build_orientation_markers(&mut hud, screen_w, screen_h);
                    }

                    renderer.draw(
                        &mut ctx,
                        &swapchain,
                        camera.view_proj(aspect),
                        camera.pos,
                        sim.time,
                        &draws,
                        &particle_vertices,
                        &hud.vertices,
                        Some(&font_texture),
                    );

                    frames += 1;
                    if fps_timer.elapsed().as_secs_f32() >= 1.0 {
                        let fps = frames as f32 / fps_timer.elapsed().as_secs_f32();
                        window.set_title(&format!(
                            "Treadlock - {:.0} fps - {:.0} km/h - lap {} - P{}/{}",
                            fps,
                            sim.player.speed_kph(),
                            sim.lap + 1,
                            sim.player_position(),
                            sim.opponents.len() + 1
                        ));
                        frames = 0;
                        fps_timer = Instant::now();
                    }
                }
                // Drag to orbit, wheel to zoom. Only while orbiting, so a stray
                // mouse movement cannot disturb the racing camera.
                WindowEvent::MouseInput { state, button, .. } => {
                    if button == winit::event::MouseButton::Left {
                        dragging = state == ElementState::Pressed;
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let current = glam::Vec2::new(position.x as f32, position.y as f32);
                    if let Some(previous) = last_cursor {
                        if dragging && camera.mode == camera::Mode::Orbit {
                            let delta = current - previous;
                            camera.rotate(delta.x, delta.y);
                        }
                    }
                    last_cursor = Some(current);
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    // A wheel notch and a trackpad scroll arrive in different
                    // units; normalise so both zoom at a usable rate.
                    let amount = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                        winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    camera.zoom(amount);
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
                    pad_mesh.destroy(&mut ctx);
                    prop_mesh.destroy(&mut ctx);
                    sky_mesh.destroy(&mut ctx);
                    sky_texture.destroy(&mut ctx);
                    font_texture.destroy(&mut ctx);
                    if let Some(texture) = car_texture.as_mut() {
                        texture.destroy(&mut ctx);
                    }
                    chassis_mesh.destroy(&mut ctx);
                    wheel_mesh.destroy(&mut ctx);
                    swapchain.destroy(&mut ctx);
                }
            }
            _ => {}
        })
        .expect("event loop failed");
}

/// The static half of the scene: uploaded once, drawn every frame, never moved.
/// Bundled because five same-typed mesh references in an argument list is a
/// swap waiting to happen.
struct World<'a> {
    track: &'a GpuMesh,
    pads: &'a GpuMesh,
    props: &'a GpuMesh,
    sky: &'a GpuMesh,
    sky_texture: &'a gfx::texture::Texture,
}

fn build_draws<'a>(
    sim: &Sim,
    world: &World<'a>,
    chassis_mesh: &'a GpuMesh,
    wheel_mesh: &'a GpuMesh,
    car_texture: Option<&'a gfx::texture::Texture>,
    show_wheels: bool,
    camera_pos: Vec3,
) -> Vec<Draw<'a>> {
    let mut draws = vec![
        // The sky first, and carried on the camera so it can never be reached.
        // Its radius sits inside the far plane; drawn as real geometry rather
        // than as a full-screen pass, because it is cheap either way and this
        // needs no separate pipeline.
        Draw {
            mesh: world.sky,
            model: Mat4::from_translation(camera_pos) * Mat4::from_scale(Vec3::splat(3000.0)),
            tint: Vec3::ONE,
            surface: 4.0,
            emissive: 0.0,
            metallic: 0.0,
            texture: Some(world.sky_texture),
        },
        Draw {
            mesh: world.props,
            model: Mat4::IDENTITY,
            tint: Vec3::ONE,
            surface: 3.0,
            emissive: 0.0,
            metallic: 0.0,
            texture: None,
        },
        Draw {
            mesh: world.track,
            model: Mat4::IDENTITY,
            tint: Vec3::new(0.30, 0.33, 0.40),
            surface: 1.0,
            emissive: 0.0,
            metallic: 0.35,
            texture: None,
        },
        // Pads are their own mesh so they can glow without needing a per-vertex
        // material on the tube. surface = 2 selects the pad shading.
        Draw {
            mesh: world.pads,
            model: Mat4::IDENTITY,
            tint: Vec3::new(0.10, 0.45, 0.75),
            surface: 2.0,
            emissive: 1.0,
            metallic: 0.5,
            texture: None,
        },
    ];

    // Player first, then the field. A textured model carries its own colours,
    // so its tint stays near white and the livery comes from the map; the
    // procedural body has no map and is coloured by the tint alone.
    let livery = |base: Vec3| if car_texture.is_some() { Vec3::splat(0.9) * base * 1.6 } else { base };

    let cars = std::iter::once((&sim.player, Vec3::new(0.85, 0.16, 0.10)))
        .chain(sim.opponents.iter().map(|o| (&o.car, o.tint)));

    for (car, colour) in cars {
        let boosting = car.pad_boost > 0.0 || car.boost < 0.999;
        draws.push(Draw {
            mesh: chassis_mesh,
            model: Mat4::from_rotation_translation(car.rot, car.pos),
            tint: livery(colour),
            surface: 0.0,
            emissive: if boosting { 0.6 } else { 0.0 },
            metallic: 0.85,
            texture: car_texture,
        });

        if !show_wheels {
            continue;
        }
        for wheel in &car.wheels {
            // Wheels are positioned by the physics contact solve, then spun
            // about their axle for the visual.
            let spin = Quat::from_rotation_x(wheel.spin_angle);
            draws.push(Draw {
                mesh: wheel_mesh,
                model: Mat4::from_rotation_translation(car.rot * spin, wheel.world_pos),
                tint: Vec3::new(0.09, 0.09, 0.11),
                surface: 0.0,
                emissive: 0.0,
                metallic: 0.2,
                texture: car_texture,
            });
        }
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

    {
        // Roof-toward-the-axis holds on the ceiling just as much as on the
        // floor, so it cannot answer "which side of the tube am I on". The
        // radial direction against world down can.
        let surf = sim.track.surface(sim.player.pos, sim.player.hint);
        let (mut low, mut high) = (f32::MAX, f32::MIN);
        for f in &sim.track.frames {
            low = low.min(f.pos.y);
            high = high.max(f.pos.y);
        }
        println!(
            "spawn: on tube floor {:+.2} (+1 floor, -1 ceiling) | height {:.0} m in a {:.0}..{:.0} m \
             track | gradient {:+.0}%",
            surf.down.dot(glam::Vec3::NEG_Y),
            sim.player.pos.y,
            low,
            high,
            surf.tangent.y * 100.0,
        );
    }

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
