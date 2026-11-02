# Deepdelve

A tech demo of an underground voxel world in Rust and wgpu. There is no surface: only three
stacked layers of caverns a kilometre or two across, sealed between bedrock with void beyond.
You start in the Overworld, with green forests and lakes under a sky of blue crystal. Below it
lie the Dark Forest and the lava-lit Underworld. You walk, fly and build as in Minecraft's
creative mode. Chunks are true 3D and generate in a sphere around you, with terrain visible
about 4 km away.

## Running

Requires Nix with flakes enabled, on NixOS or anywhere else with Vulkan drivers.

```sh
nix develop                # enter the shell with Rust and all native libraries
cargo run --release        # play, with a random world
cargo run --release -- --seed 12345
```

`nix develop --command cargo run --release` does both at once. A binary built inside the
shell also runs on its own afterwards, as `./target/release/deepdelve`.

To choose between the working tree and any past release, use the runner. Each release has its
changelog, screenshots and benchmark in [releases/](releases/).

```sh
nix run .#play
```

| Input | Action |
| --- | --- |
| Mouse | Look |
| W A S D | Walk, or fly |
| Space | Jump, swim up, or fly up. Press twice quickly to start or stop flying |
| C or Left Ctrl | Sneak, which stops you walking off edges, or fly down |
| Shift | Sprint. In noclip, fly six times faster |
| N | Noclip: fly through everything. Press again to collide, flying |
| Mouse wheel | Choose a hotbar slot. In noclip, change flight speed |
| Left mouse | Break block (hold to repeat, reach 96 blocks) |
| Right mouse | Place the selected block (hold to repeat) |
| Middle mouse | Pick the block you look at into the hotbar |
| 1-9 | Choose a hotbar slot |
| R | Wipe the world and generate a new one |
| Esc | Release or recapture the mouse |
| F1 | Toggle the stats overlay |
| F2 | Toggle occlusion culling |
| F12 | Save a screenshot to `captures/` |

Useful options: `--tier N` starts in another layer (0 top, 1 middle, 2 bottom),
`--render-height N` sets the internal resolution (default 1440, upscaled to the window),
`--no-vsync` uncaps the frame rate, `--fullscreen`. See `--help` for all.

## Testing without a window

```sh
cargo test --release                                  # unit tests
./target/release/deepdelve --headless                 # end-to-end: break, reload, reset
./target/release/deepdelve --headless --tour out/     # screenshots of every layer
./target/release/deepdelve --headless --bench 20 --bench-speed 600 --size 2560x1440
./target/release/deepdelve --headless --tier 2 --bench 20   # the same, in a lower layer
./target/release/deepdelve --headless --shot view.png@326,110,-1106,0.6,-0.12   # x,y,z,yaw,pitch
./target/release/deepdelve --headless --hud --shot view.png        # the spawn view, with the hotbar drawn
```

## Documentation

- [docs/DESIGN.md](docs/DESIGN.md): game design decisions and why they were made.
- [docs/IMPLEMENTATION.md](docs/IMPLEMENTATION.md): how the engine works, and measurements.
- [docs/RELEASES.md](docs/RELEASES.md): running old versions and making new releases.
- [docs/STATUS.md](docs/STATUS.md): where the work stands, and known issues.
- [CLAUDE.md](CLAUDE.md): how to work on this repo, for AI agents and humans alike.
