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
- [x] glTF/GLB car models
- [ ] Model textures (geometry only for now)
- [ ] Ray-traced reflections
- [ ] Opponents, weapons, audio, HUD

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
separate wheel mesh was found. Textures are not loaded yet, so models currently
render with a flat tint.

Note that the car spends time inverted, and the physics treats a flip as
recoverable rather than fatal. A model with a strongly asymmetric silhouette
will read oddly during those moments; symmetric, big-wheeled designs suit the
game better.

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
| `src/gfx/` | Vulkan context, swapchain, buffers, renderer |
| `src/shaders/` | WGSL, translated at pipeline build |
