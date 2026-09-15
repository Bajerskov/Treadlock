mod assets;
mod audio;
mod camera;
mod effects;
mod gfx;
mod input;
mod marks;
mod menu;
mod mesh;
#[cfg(feature = "meshy")]
mod meshy;
mod model;
mod particles;
mod plates;
mod scenery;
mod settings;
mod sim;
mod track;
mod ui;
mod vehicle;
mod weapons;

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
            ("decal.wgsl", include_str!("shaders/decal.wgsl")),
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
    // Model generation is an author-time step, not something the game does.
    #[cfg(feature = "meshy")]
    {
        let argv: Vec<String> = std::env::args().collect();
        let flag = |name: &str| argv.iter().any(|a| a == name);
        if flag("--meshy-balance") || flag("--meshy-cars") {
            let result = if flag("--meshy-balance") {
                meshy::balance()
            } else {
                meshy::generate_cars(
                    &assets::Library::load("assets"),
                    flag("--refine"),
                    flag("--dry-run"),
                )
            };
            if let Err(e) = result {
                eprintln!("{e}");
                std::process::exit(1);
            }
            return;
        }
    }
    if std::env::args().any(|a| a.starts_with("--meshy")) && cfg!(not(feature = "meshy")) {
        eprintln!("--meshy needs the feature: cargo run --features meshy -- --meshy-cars");
        std::process::exit(2);
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
    let settings = settings::Settings::load();
    // The command line still wins where it was given, so an explicit flag is
    // never silently overridden by a saved setting.
    let mut vsync = args.vsync && settings.video.vsync;
    let mut swapchain = Swapchain::new(&mut ctx, size.width, size.height, vsync);
    let mut renderer = Renderer::new(&mut ctx, &swapchain);

    let seed = race_seed(&settings, args.seed);
    let mut sim = Sim::with_setup(seed, settings.race);
    println!(
        "track: seed {} | {:.0} m | {} frames",
        seed,
        sim.track.length,
        sim.track.frames.len()
    );

    let mut track_mesh = GpuMesh::upload(
        &mut ctx,
        "track",
        &sim.track.vertices,
        &sim.track.indices,
    );
    // The front end starts up over a live race, which is both the attract mode
    // and the thing the video settings are previewed against.
    let mut menu = menu::Menu::new(settings);
    let mut in_menu = true;
    // Work the menu asked for that has to happen between frames rather than
    // inside an event handler, where the swapchain is not reachable.
    let mut pending: Option<Pending> = None;

    let library = assets::Library::load("assets");
    let missing = library.missing().len();
    if missing > 0 {
        println!("assets: {missing} not present, using generated stand-ins (--assets to list)");
    }

    let mut garage = Garage::load(&mut ctx, &mut renderer, &args, &library);
    let mut props = Props::load(&mut ctx);
    println!(
        "garage: {} car{} for {} drivers",
        garage.cars.len(),
        if garage.cars.len() == 1 { "" } else { "s" },
        sim.opponents.len() + 1
    );

    let (pad_vertices, pad_indices) = sim.track.boost_pad_mesh();
    let mut pad_mesh = GpuMesh::upload(&mut ctx, "boost pads", &pad_vertices, &pad_indices);
    println!("boost pads: {}", sim.track.boost_pads.len());

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
    let mut skid = marks::Marks::new();
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
                        swapchain.recreate(&mut ctx, size.width, size.height, vsync);
                        renderer.rebuild_pipeline(&mut ctx, &swapchain);
                        renderer.needs_resize = false;
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    let pressed = event.state == ElementState::Pressed;
                    // While the menu is up it takes the keyboard. Driving keys
                    // are not fed through, or the car would be steered from
                    // behind the front end.
                    if in_menu {
                        if !pressed {
                            return;
                        }
                        let PhysicalKey::Code(code) = event.physical_key else {
                            return;
                        };
                        let action = match code {
                            KeyCode::ArrowUp | KeyCode::KeyW => Some(menu::Action::Up),
                            KeyCode::ArrowDown | KeyCode::KeyS => Some(menu::Action::Down),
                            KeyCode::ArrowLeft | KeyCode::KeyA => Some(menu::Action::Left),
                            KeyCode::ArrowRight | KeyCode::KeyD => Some(menu::Action::Right),
                            KeyCode::Enter | KeyCode::Space => Some(menu::Action::Accept),
                            KeyCode::Escape => Some(menu::Action::Back),
                            _ => None,
                        };
                        let Some(action) = action else { return };
                        match menu.input(action) {
                            menu::Outcome::Quit => target.exit(),
                            menu::Outcome::StartRace => {
                                pending = Some(Pending::Race);
                                in_menu = false;
                            }
                            menu::Outcome::Resume => in_menu = false,
                            menu::Outcome::VideoChanged => pending = Some(Pending::Video),
                            menu::Outcome::None => {}
                        }
                        return;
                    }
                    if let PhysicalKey::Code(code) = event.physical_key {
                        match code {
                            // Escape opens the menu rather than quitting: the
                            // race is still running behind it and quitting is a
                            // choice made on purpose, from the menu.
                            KeyCode::Escape if pressed => {
                                in_menu = true;
                                menu.racing = true;
                                menu.screen = menu::Screen::Main;
                                // Let go of everything, or a key held when the
                                // menu opened stays held when it closes.
                                input.keys = input::Keys::default();
                            }
                            KeyCode::KeyW | KeyCode::ArrowUp => input.keys.up = pressed,
                            KeyCode::KeyS | KeyCode::ArrowDown => input.keys.down = pressed,
                            KeyCode::KeyA | KeyCode::ArrowLeft => input.keys.left = pressed,
                            KeyCode::KeyD | KeyCode::ArrowRight => input.keys.right = pressed,
                            KeyCode::ShiftLeft => input.keys.boost = pressed,
                            KeyCode::Space => input.keys.handbrake = pressed,
                            KeyCode::ControlLeft | KeyCode::Enter => {
                                input.keys.fire = pressed
                            }
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

                    // The pad drives the menu too, polled here rather than
                    // handled as an event because evdev is read per frame. A
                    // console has to be usable without reaching for a keyboard.
                    if in_menu {
                        if let Some(action) = input.take_menu_action() {
                            match menu.input(action) {
                                menu::Outcome::Quit => target.exit(),
                                menu::Outcome::StartRace => {
                                    pending = Some(Pending::Race);
                                    in_menu = false;
                                }
                                menu::Outcome::Resume => in_menu = false,
                                menu::Outcome::VideoChanged => pending = Some(Pending::Video),
                                menu::Outcome::None => {}
                            }
                        }
                    }

                    // Act on what the menu asked for, here rather than in the
                    // key handler, where the swapchain is out of reach.
                    match pending.take() {
                        Some(Pending::Race) => {
                            let seed = race_seed(&menu.settings, args.seed);
                            sim = Sim::with_setup(seed, menu.settings.race);
                            println!(
                                "race: seed {seed} | {} opponents | {} | weapons {}",
                                menu.settings.race.opponents,
                                menu.settings.race.difficulty.name(),
                                if menu.settings.race.weapons { "on" } else { "off" }
                            );
                            // The track changed, so everything baked from it has
                            // to be rebuilt before it is drawn again.
                            unsafe { ctx.device.device_wait_idle().ok() };
                            track_mesh.destroy(&mut ctx);
                            pad_mesh.destroy(&mut ctx);
                            prop_mesh.destroy(&mut ctx);
                            track_mesh = GpuMesh::upload(
                                &mut ctx,
                                "track",
                                &sim.track.vertices,
                                &sim.track.indices,
                            );
                            let (pv, pi) = sim.track.boost_pad_mesh();
                            pad_mesh = GpuMesh::upload(&mut ctx, "boost pads", &pv, &pi);
                            let scenery = scenery::generate(&sim.track, seed, &library);
                            prop_mesh = GpuMesh::from_mesh(&mut ctx, "scenery", &scenery.props);

                            particles = particles::Particles::new();
                            skid = marks::Marks::new();
                            cues = audio::Cues::new();
                            ai_driving = false;
                            camera.snap(&sim.player, &sim.track);
                        }
                        Some(Pending::Video) => {
                            window.set_fullscreen(if menu.settings.video.fullscreen {
                                Some(winit::window::Fullscreen::Borderless(None))
                            } else {
                                None
                            });
                            vsync = args.vsync && menu.settings.video.vsync;
                            let size = window.inner_size();
                            if size.width > 0 && size.height > 0 {
                                swapchain.recreate(&mut ctx, size.width, size.height, vsync);
                                renderer.rebuild_pipeline(&mut ctx, &swapchain);
                                renderer.needs_resize = false;
                            }
                        }
                        None => {}
                    }

                    if input.take_respawn() {
                        sim.player.respawn(&sim.track);
                        camera.snap(&sim.player, &sim.track);
                        cues.respawned(&audio);
                    }
                    // Under AI the player car takes the same driver the
                    // opponents use, so what you are watching is the real
                    // racing line rather than a separate demo mode. Behind the
                    // menu that is also the attract mode: the field races on
                    // while the player reads the front end.
                    let controls = if ai_driving || in_menu {
                        let lookahead = 26.0 + sim.player.speed() * 0.12;
                        vehicle::autopilot_lane(&sim.track, &sim.player, lookahead, 0.0)
                    } else {
                        input.controls(&menu.settings.pad)
                    };
                    sim.update(&controls, dt);
                    camera.update(&sim.player, &sim.track, dt);
                    camera.base_fov = menu.settings.video.fov.to_radians();
                    cues.poll(&sim, &audio, dt);
                    audio.update(audio::observe(&sim, &camera, controls.throttle));

                    if renderer.needs_resize {
                        let size = window.inner_size();
                        if size.width > 0 && size.height > 0 {
                            swapchain.recreate(&mut ctx, size.width, size.height, vsync);
                            renderer.needs_resize = false;
                        }
                    }

                    let aspect =
                        swapchain.extent.width as f32 / swapchain.extent.height.max(1) as f32;
                    // The effects read the same controls the car did, not a
                    // fresh poll: under the AI or behind the menu those are the
                    // autopilot's, and a second poll would show an idle
                    // keyboard and stop the exhaust.
                    let video = menu.settings.video;
                    effects::update(
                        &mut particles,
                        &sim,
                        dt * video.particles,
                        controls.throttle,
                        controls.brake,
                        controls.boost,
                    );
                    if video.skid_marks {
                        skid.update(&sim, dt);
                    }
                    let (right, up) = camera.basis();
                    let particle_vertices = particles.build_vertices(right, up).to_vec();
                    let mark_vertices = if video.skid_marks {
                        skid.build_vertices().to_vec()
                    } else {
                        Vec::new()
                    };

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
                        &garage,
                        &props,
                        camera.pos,
                        video.scenery,
                    );
                    let (screen_w, screen_h) =
                        (swapchain.extent.width as f32, swapchain.extent.height as f32);
                    if in_menu {
                        // The menu replaces the HUD rather than sitting over
                        // it: two sets of numbers at once is unreadable.
                        menu::build(
                            &mut hud,
                            &menu,
                            screen_w,
                            screen_h,
                            input.gamepad_count(),
                        );
                    } else {
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
                    }

                    renderer.draw(
                        &mut ctx,
                        &swapchain,
                        camera.view_proj(aspect),
                        camera.pos,
                        sim.time,
                        &draws,
                        &particle_vertices,
                        &mark_vertices,
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
                    garage.destroy(&mut ctx);
                    props.destroy(&mut ctx);
                    swapchain.destroy(&mut ctx);
                }
            }
            _ => {}
        })
        .expect("event loop failed");
}

/// The track to race. A stored seed of zero means a fresh track each time,
/// which is what "RANDOM" in the menu selects; anything else is reproducible.
///
/// `fallback` is what `--seed` asked for, so the flag still decides when it was
/// given and the menu decides when it was not.
fn race_seed(settings: &settings::Settings, fallback: u64) -> u64 {
    if settings.race.seed != 0 {
        return settings.race.seed;
    }
    if fallback != 7 {
        // Not the default, so it was asked for explicitly.
        return fallback;
    }
    // Something that differs between runs without pulling in a clock crate.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(fallback)
        | 1
}

/// Something the menu asked for that has to be done between frames, where the
/// window and the swapchain are reachable.
enum Pending {
    Race,
    Video,
}

/// One body in the field: its meshes and its livery map.
struct Car {
    chassis: GpuMesh,
    wheel: GpuMesh,
    texture: Option<gfx::texture::Texture>,
    /// False when the model already has wheels modelled into the body, where
    /// drawing the engine's own on top would double them up.
    show_wheels: bool,
}

/// Every car available to the field.
///
/// Drivers are assigned one each and cycle if there are fewer bodies than
/// drivers. A repeated body is not a repeated car: the livery tint is per
/// driver, so the field still reads as six competitors rather than six clones.
struct Garage {
    cars: Vec<Car>,
}

impl Garage {
    fn load(ctx: &mut Context, renderer: &mut Renderer, args: &Args, library: &assets::Library) -> Garage {
        let mut cars = Vec::new();

        // `--car` is the player's, and comes first so it is always driver 0.
        let paths: Vec<(String, model::Fit)> = args
            .car
            .iter()
            .map(|p| (p.clone(), args.car_fit))
            .chain(
                ["car_player", "car_rival_a", "car_rival_b", "car_rival_c", "car_rival_d", "car_rival_e"]
                    .iter()
                    .filter(|id| !(args.car.is_some() && **id == "car_player"))
                    .filter_map(|id| Some((library.path(id)?.to_str()?.to_string(), model::Fit::default()))),
            )
            .collect();

        for (path, fit) in paths {
            match model::Model::load(&path, fit) {
                Ok(m) => cars.push(Car::upload(ctx, renderer, Some(m))),
                Err(e) => eprintln!("{e}\nskipping that car"),
            }
        }

        // The procedural body is the floor, not a special case: with no models
        // at all the field is still a field.
        if cars.is_empty() {
            cars.push(Car::upload(ctx, renderer, None));
        }
        Garage { cars }
    }

    fn for_driver(&self, index: usize) -> &Car {
        &self.cars[index % self.cars.len()]
    }

    fn destroy(&mut self, ctx: &mut Context) {
        for car in &mut self.cars {
            car.chassis.destroy(ctx);
            car.wheel.destroy(ctx);
            if let Some(texture) = car.texture.as_mut() {
                texture.destroy(ctx);
            }
        }
    }
}

impl Car {
    fn upload(ctx: &mut Context, renderer: &mut Renderer, loaded: Option<model::Model>) -> Car {
        let show_wheels = loaded.as_ref().map_or(true, |m| m.wheel.is_some());
        let chassis = match &loaded {
            Some(m) => GpuMesh::from_mesh(ctx, "chassis", &m.chassis),
            None => GpuMesh::from_mesh(ctx, "chassis", &mesh::chassis(vehicle::HALF_EXTENTS, 0.72)),
        };
        let wheel = match loaded.as_ref().and_then(|m| m.wheel.as_ref()) {
            Some(w) => GpuMesh::from_mesh(ctx, "wheel", w),
            None => GpuMesh::from_mesh(ctx, "wheel", &mesh::wheel(0.62, 0.22, 16)),
        };
        let texture = loaded.as_ref().and_then(|m| m.base_color.as_ref()).map(|image| {
            gfx::texture::Texture::new(
                ctx,
                renderer.texture_layout,
                renderer.texture_pool,
                image.width,
                image.height,
                &image.rgba,
            )
        });
        Car { chassis, wheel, texture, show_wheels }
    }
}

/// The small meshes weapons are drawn with, uploaded once and reused for every
/// crate, rocket and mine on the track.
struct Props {
    crate_box: GpuMesh,
    rocket: GpuMesh,
    mine: GpuMesh,
}

impl Props {
    fn load(ctx: &mut Context) -> Props {
        Props {
            // All three are existing shapes at new sizes rather than new
            // geometry: a tapered cube, a dart, and a short cylinder.
            crate_box: GpuMesh::from_mesh(ctx, "crate", &mesh::chassis(Vec3::splat(1.3), 0.45)),
            rocket: GpuMesh::from_mesh(
                ctx,
                "rocket",
                &mesh::chassis(Vec3::new(0.3, 0.3, 1.1), 0.35),
            ),
            mine: GpuMesh::from_mesh(ctx, "mine", &mesh::wheel(1.7, 0.28, 12)),
        }
    }

    fn destroy(&mut self, ctx: &mut Context) {
        self.crate_box.destroy(ctx);
        self.rocket.destroy(ctx);
        self.mine.destroy(ctx);
    }
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
    garage: &'a Garage,
    props: &'a Props,
    camera_pos: Vec3,
    scenery: bool,
) -> Vec<Draw<'a>> {
    let mut draws: Vec<Draw> = Vec::new();
    if scenery {
        draws.extend([
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
        ]);
    }

    draws.push(Draw {
        mesh: world.track,
        model: Mat4::IDENTITY,
        tint: Vec3::new(0.30, 0.33, 0.40),
        surface: 1.0,
        emissive: 0.0,
        metallic: 0.35,
        texture: None,
    });
    // Pads are their own mesh so they can glow without needing a per-vertex
    // material on the tube. surface = 2 selects the pad shading.
    draws.push(Draw {
        mesh: world.pads,
        model: Mat4::IDENTITY,
        tint: Vec3::new(0.10, 0.45, 0.75),
        surface: 2.0,
        emissive: 1.0,
        metallic: 0.5,
        texture: None,
    });

    // Weapon props. Each is its own draw with a small shared mesh: a crate has
    // to be able to wink out on its own when collected, which a single baked
    // mesh could not do.
    for pickup in &sim.weapons.pickups {
        if pickup.cooldown > 0.0 {
            continue;
        }
        // Turning slowly about the surface normal, which is how a pickup has
        // read as collectable since before any of this was 3D.
        let spin = Quat::from_axis_angle(pickup.up, sim.time * 1.6);
        draws.push(Draw {
            mesh: &props.crate_box,
            model: Mat4::from_rotation_translation(spin, pickup.pos + pickup.up * 1.6),
            tint: Vec3::new(0.35, 1.0, 0.75),
            surface: 5.0,
            emissive: pickup.frame as f32 * 0.1,
            metallic: 0.5,
            texture: None,
        });
    }

    for rocket in &sim.weapons.rockets {
        let heading = rocket.vel.normalize_or(Vec3::NEG_Z);
        draws.push(Draw {
            mesh: &props.rocket,
            model: Mat4::from_rotation_translation(
                track::look_rotation(heading, Vec3::Y),
                rocket.pos,
            ),
            tint: Vec3::new(1.0, 0.55, 0.20),
            surface: 5.0,
            emissive: 0.0,
            metallic: 0.8,
            texture: None,
        });
    }

    for mine in &sim.weapons.mines {
        // Dark until it arms, then lit: a mine you cannot yet set off should
        // not look like one you can.
        let live = mine.arm <= 0.0;
        draws.push(Draw {
            mesh: &props.mine,
            model: Mat4::from_rotation_translation(
                track::look_rotation(mine.up, Vec3::Y),
                mine.pos + mine.up * 0.3,
            ),
            tint: if live {
                Vec3::new(1.0, 0.22, 0.22)
            } else {
                Vec3::new(0.30, 0.16, 0.16)
            },
            surface: 5.0,
            emissive: if live { 0.0 } else { 40.0 },
            metallic: 0.4,
            texture: None,
        });
    }

    // Player first, then the field, each driver taking its own body from the
    // garage. A textured model carries its own colours, so its tint stays near
    // white and the livery comes from the map; the procedural body has no map
    // and is coloured by the tint alone.
    let cars = std::iter::once((&sim.player, Vec3::new(0.85, 0.16, 0.10)))
        .chain(sim.opponents.iter().map(|o| (&o.car, o.tint)));

    for (driver, (car, colour)) in cars.enumerate() {
        let body = garage.for_driver(driver);
        let tint = if body.texture.is_some() {
            Vec3::splat(0.9) * colour * 1.6
        } else {
            colour
        };
        let boosting = car.pad_boost > 0.0 || car.boost < 0.999;
        draws.push(Draw {
            mesh: &body.chassis,
            model: Mat4::from_rotation_translation(car.rot, car.pos),
            tint,
            surface: 0.0,
            emissive: if boosting { 0.6 } else { 0.0 },
            metallic: 0.85,
            texture: body.texture.as_ref(),
        });

        if !body.show_wheels {
            continue;
        }
        for wheel in &car.wheels {
            // Wheels are positioned by the physics contact solve, then spun
            // about their axle for the visual.
            let spin = Quat::from_rotation_x(wheel.spin_angle);
            draws.push(Draw {
                mesh: &body.wheel,
                model: Mat4::from_rotation_translation(car.rot * spin, wheel.world_pos),
                tint: Vec3::new(0.09, 0.09, 0.11),
                surface: 0.0,
                emissive: 0.0,
                metallic: 0.2,
                texture: body.texture.as_ref(),
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
