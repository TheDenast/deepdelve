# Deepdelve

A tech demo of an underground voxel world in Rust and wgpu. There is no surface: only
rock, and inside it stacked caverns a kilometre or two across, holding forests, lakes and
seas under ceilings of glowing crystal. Chunks are true 3D and generate in a sphere around
you, with terrain visible about 4 km away.

## Running

Requires Nix with flakes enabled, on NixOS or anywhere else with Vulkan drivers.

```sh
nix develop                # enter the shell with Rust and all native libraries
cargo run --release        # play, with a random world
cargo run --release -- --seed 12345
```

`nix develop --command cargo run --release` does both at once. A binary built inside the
shell also runs on its own afterwards, as `./target/release/deepdelve`.

| Input | Action |
| --- | --- |
| Mouse | Look |
| W A S D | Fly |
| Space / C or Left Ctrl | Up / down |
| Shift | Fly six times faster |
| Mouse wheel | Change base speed |
| Left mouse | Break block (hold to repeat, reach 96 blocks) |
| R | Wipe the world and generate a new one |
| Esc | Release or recapture the mouse |
| F1 | Toggle the stats overlay |
| F2 | Toggle occlusion culling |
| F12 | Save a screenshot to `captures/` |

Useful options: `--render-height N` sets the internal resolution (default 1440, upscaled
to the window), `--no-vsync` uncaps the frame rate, `--fullscreen`. See `--help` for all.

## Testing without a window

```sh
cargo test --release                                  # unit tests
./target/release/deepdelve --headless                 # end-to-end: break, reload, reset
./target/release/deepdelve --headless --tour out/     # screenshots of several caverns
./target/release/deepdelve --headless --bench 20 --bench-speed 600 --size 2560x1440
./target/release/deepdelve --headless --shot view.png@172,300,875,0,-0.15   # x,y,z,yaw,pitch
```

## Documentation

- [docs/DESIGN.md](docs/DESIGN.md): game design decisions and why they were made.
- [docs/IMPLEMENTATION.md](docs/IMPLEMENTATION.md): how the engine works, and measurements.
