# Treadlock

A fast arcade racer on procedurally generated tube tracks, where cars drive on
the walls and the ceiling. Built in Rust on a custom Vulkan renderer, targeting
an AMD BC-250 board as a Steam-machine-style console but portable to any
Vulkan 1.3 system.

The handling model is aimed squarely at **Rollcage** (PS1, 1999): huge wheels,
flipping over never ends a race, absurd speed.

## Current state

Playable vertical slice in progress.

- [x] Procedural closed-loop tube tracks from a seed
- [x] Rollcage-style vehicle physics with self-righting
- [x] Fixed-timestep sim with lap timing
- [x] Forward Vulkan renderer with chase camera
- [x] Keyboard and gamepad input
- [x] glTF/GLB car models, with textures
- [x] Opponents, HUD, ranking and minimap
- [x] Synthesised audio: engine, tyres, passing cars, ambience, music
- [x] Background scenery and a seeded sky
- [ ] Ray-traced reflections
- [ ] Weapons

## Building and running

Needs a Rust toolchain and a Vulkan 1.3 driver. No Vulkan SDK is required:
shaders are authored in WGSL and translated to SPIR-V with `naga`, which is
pure Rust.

```sh
cargo run --release                    # play
cargo run --release -- --seed 42       # a different track
cargo run --release -- --no-vsync      # uncapped frame rate
cargo run --release -- --car car.glb   # use your own car model
```

### Controls

| Action | Keyboard | Gamepad |
| --- | --- | --- |
| Throttle | W / Up | Right trigger |
| Brake | S / Down | Left trigger |
| Steer | A D / Left Right | Left stick |
| Boost | Left Shift | A |
| Handbrake | Space | B |
| Respawn | R | Start |
| Hand the car to the AI | P | - |
| Orbit camera on/off | C | - |
| Orbit / zoom, while orbiting | Drag / wheel | - |
| Quit | Esc | - |

`P` gives the player car to the same driver the opponents use, so what you are
watching is the real racing line rather than a separate demo mode. Together
with `C` that is how to inspect a car model: let the AI drive and orbit around
it. The orbit camera is anchored to the car's own frame, so the car holds still
on screen while the world turns around it.

### Headless mode

The sim runs without a GPU, which is how handling is validated in CI or on a
build machine with no display:

```sh
cargo run --release -- --headless 90          # autopilot 90s, print telemetry
cargo run --release -- --headless 90 --trace  # per-second detail
cargo run --release -- --check-shaders        # translate WGSL, no GPU needed
```

Headless mode reports average and top speed, wheel contact percentage, lap
times, and whether the car ever escaped the tube. A diverging or escaping run
exits non-zero.

## Car models

Pass a `.glb` or `.gltf` file with `--car`. Models are refitted on load rather
than having to arrive correct, because generated models (Meshy, Tripo and
similar) come out at arbitrary scale and facing:

- yawed 90 degrees automatically when the model is wider than it is long, since
  generated cars often face along X
- scaled uniformly to the first limit it hits, either 4.2 m long or 2.5 m wide.
  Fitting on length alone lets a stocky model overhang the body that is actually
  colliding, which reads as wheels floating outside the car
- recentred on the origin, where the physics body sits
- normals generated when the file has none

Corrections, when a guess goes wrong:

| Flag | Use |
| --- | --- |
| `--car-yaw <deg>` | Model faces the wrong way round the vertical axis |
| `--car-pitch <deg>` | Model is Z-up; pass `-90`. Blender exports do this |
| `--car-scale <x>` | Multiplier on the automatic fit, for taste |

If any node is named with `wheel`, `tyre` or `tire` it becomes the wheel mesh,
drawn four times with spin and steering applied. Otherwise the model is assumed
to include its own wheels and the engine does not draw its own on top.

Check a file without launching the game, and without a GPU:

```sh
cargo run --release -- --check-model --car car.glb
```

That reports triangle count, source and fitted dimensions, and whether a
separate wheel mesh was found. A model's base colour map is loaded and used; a
model without one renders with a flat tint.

Note that the car spends time inverted, and the physics treats a flip as
recoverable rather than fatal. A model with a strongly asymmetric silhouette
will read oddly during those moments; symmetric, big-wheeled designs suit the
game better.

### Generating cars with Meshy

Car bodies can be generated rather than modelled, through the Meshy API. This
is an author-time step behind an optional feature: the game binary carries no
HTTP client and never talks to a paid service while racing. Generate once,
commit the `.glb` files, and the manifest picks them up.

```sh
export MESHY_API_KEY=...                                    # never written to disk
cargo run --features meshy -- --meshy-cars --dry-run        # prompts and targets, spends nothing
cargo run --features meshy -- --meshy-cars                  # generate the missing cars
cargo run --features meshy -- --meshy-cars --refine         # slower, costlier, textured
cargo run --features meshy -- --meshy-balance               # credits remaining
```

Slots that already have a file are skipped, so an interrupted run resumes
instead of paying twice, and one car failing does not discard the ones that
worked.

The prompts ask for symmetric, big-wheeled bodies on purpose: the game spends
real time upside down, and a car with a strongly asymmetric silhouette reads as
broken during those moments rather than as inverted.

Generated models arrive at arbitrary scale and facing, which the loader already
corrects. Check one before racing it:

```sh
cargo run --release -- --check-model --car assets/models/cars/player.glb
```

**A caveat worth knowing.** Both `docs.meshy.ai` and `api.meshy.ai` were
unreachable from the machine this was written on, so the endpoint paths and
JSON field names are unverified against the live service. They are all in one
block at the top of `src/meshy.rs` for that reason. If a call returns 404 or
complains about a missing field, that block is the only place to look.

## Assets

Everything the game will ever load is declared in `assets/manifest.txt`,
whether or not the file exists. A missing asset is not an error: each line
records what the game does instead, so the manifest doubles as a brief for
whoever is making it.

```sh
cargo run --release -- --assets    # what is present, what is missing, and the fallback for each
```

Nothing is blocked waiting on art. Every sound is synthesised, the music is a
generated bed, the skyline is generated geometry and the sky is a generated
plate. Dropping a real file into `assets/` replaces exactly that one thing.

| Kind | Format | Replaces |
| --- | --- | --- |
| `sound`, `music` | WAV (16/24-bit PCM or 32-bit float, mono or stereo) | The synthesised voice or the generated music bed |
| `model` | `.glb` / `.gltf` | One generated prop shape, rescaled to the height the slot wanted |
| `plate` | PNG or JPEG | One layer of the sky, resampled to the sky texture's size |

## Audio

Every sound is synthesised rather than sampled, which suits a game where the
things making noise are continuous: the engine note follows road speed through
an imaginary six-speed box, and a car going past is Doppler-shifted by its own
closing speed rather than triggered as a "whoosh" when it gets near enough.

Tones are summed from sine partials rather than clipped from a sawtooth. A
naive saw at engine pitch folds its upper harmonics back down as aliasing,
which is the metallic buzz that makes synthesised engines sound cheap.

The mixer is pure. It turns a `Scene` - a snapshot of what is around the
listener - into interleaved samples with no reference to a device or a clock,
so the whole soundtrack can be rendered into a buffer and measured by tests on
a machine with no sound card. The listener is the camera rather than the car,
because that is where the player is: an opponent visibly overtaking on the left
has to be on the left in the mix.

Audio output is `cpal` behind the default `audio` feature. Build with
`--no-default-features` on a machine without audio development headers; the
game runs silent and everything else is unaffected.

## The world outside the tube

The track alternates between closed tube and open road. The open stretches are
cut open in the mesh, not merely given different gravity, which is what gives
those sections a sky and makes the scenery worth drawing.

Scenery is generated from the track seed and baked into a single static mesh,
so the whole skyline is one draw call. It stands only beside the open stretches
and never intrudes into the tube. Note that the physics still treats the tube
as closed, so there is a ceiling over the open sections that can be hit but not
seen; in practice nothing reaches it, because those are exactly the stretches
where gravity pulls the car to the floor.

The sky is one texture composited from four layers - gradient, clouds, far
ridge, near ridge - drawn in equirectangular space and mapped onto a dome
carried on the camera. Each layer is generated from the seed unless the library
has a file for it.

## How it works

### Tracks

A track is a seeded Catmull-Rom loop resampled to uniform arc length, then
swept into a tube using rotation-minimizing frames. Because the loop closes,
the frame carried all the way round does not line up with where it started;
that residual twist is spread evenly across the track so the surface does not
seam.

### Driving on walls

The whole wall-and-ceiling mechanic comes from one decision: **gravity is taken
from the track, not the world.** Inside a tube, "down" is the radial direction
pointing into the nearest wall. There is no special case for wall driving,
because from the car's point of view it is never on a wall.

Surface queries project onto the centerline rather than raycasting the mesh.
That is far cheaper than tracing triangles and it cannot tunnel at speed.

### Flipping never ends a race

Driving the ceiling of a tube is not the same as being inverted: gravity already
follows the tube, so the car's roof points at the tube axis the whole way round.
What a flip means is the chassis rolling relative to that surface, and it is
always recoverable.

The wheels sit on the chassis mid-plane and are taller than the body is thick,
so a flipped car still has traction and can keep driving while it rights itself.
The righting torque targets wheels-down and ramps quadratically with how
inverted the car is: negligible in normal driving, decisive past ninety degrees.
A constant torque cannot do this, because an inverted car rests on its wheels
and the suspension resists being rolled back. Steering flips sign with the car
so "left" still means left on screen during the recovery.

## Target hardware

See [docs/bc250.md](docs/bc250.md) for the BC-250's specifications and what
they mean for this engine.

## Layout

| Path | Contents |
| --- | --- |
| `src/track.rs` | Procedural generation and surface queries |
| `src/vehicle.rs` | Suspension, tyre model, autopilot |
| `src/sim.rs` | Fixed-timestep update, lap timing |
| `src/camera.rs` | Chase camera |
| `src/input.rs` | Keyboard, and gamepads via Linux evdev |
| `src/mesh.rs` | Runtime car meshes |
| `src/model.rs` | glTF/GLB loading and refitting |
| `src/assets.rs` | The manifest: what the game loads, and what it does without |
| `src/audio/` | Synthesis, mixing, WAV loading, cpal device |
| `src/scenery.rs` | Background props and the sky dome |
| `src/plates.rs` | Generated and loaded background layers |
| `src/marks.rs` | Skid marks |
| `src/meshy.rs` | Author-time model generation (optional feature) |
| `src/gfx/` | Vulkan context, swapchain, buffers, renderer |
| `src/shaders/` | WGSL, translated at pipeline build |
