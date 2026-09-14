# Treadlock

A fast arcade racer on procedurally generated tube tracks, where cars drive on
the walls and the ceiling. Built in Rust on a custom Vulkan renderer, targeting
an AMD BC-250 board as a Steam-machine-style console but portable to any
Vulkan 1.3 system.

The handling model is aimed squarely at **Rollcage** (PS1, 1999): huge wheels,
no upside down, absurd speed.

## Current state

Playable vertical slice in progress.

- [x] Procedural closed-loop tube tracks from a seed
- [x] Rollcage-style vehicle physics, including inverted driving
- [x] Fixed-timestep sim with lap timing
- [x] Forward Vulkan renderer with chase camera
- [x] Keyboard and gamepad input
- [ ] Ray-traced reflections
- [ ] Opponents, weapons, audio, HUD

## Building and running

Needs a Rust toolchain and a Vulkan 1.3 driver. No Vulkan SDK is required:
shaders are authored in WGSL and translated to SPIR-V with `naga`, which is
pure Rust.

```sh
cargo run --release                 # play
cargo run --release -- --seed 42    # a different track
cargo run --release -- --no-vsync   # uncapped frame rate
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
| Quit | Esc | - |

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

### Never upside down

The wheels sit on the chassis mid-plane and are taller than the body is thick,
so the car meets the track from either side. Landing on the roof is a valid way
to drive, not a state to recover from. The chassis settles toward whichever of
the two flat orientations is closer, and the steering axis flips with it so
"left" still means left on screen.

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
| `src/gfx/` | Vulkan context, swapchain, buffers, renderer |
| `src/shaders/` | WGSL, translated at pipeline build |
