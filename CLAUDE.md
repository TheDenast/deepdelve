# Deepdelve

Underground voxel world tech demo: Rust and wgpu, no game engine. Read `docs/STATUS.md` first, for
where the work stands and what's known to be wrong.

| To learn about | Read |
| --- | --- |
| Current state, known issues, what's next | `docs/STATUS.md` |
| Game design and who decided what | `docs/DESIGN.md` |
| How the engine works, file layout, measurements | `docs/IMPLEMENTATION.md` |
| Running old versions, making releases, other platforms | `docs/RELEASES.md` |

The code says "tier" where the docs and the game say "layer". They're the same thing, and
`--tier N` counts from 0 at the top.

## Commands

Run everything through the flake's shell, from the repo root:

```sh
nix develop --command cargo test --release
nix develop --command cargo run --release -- --seed 777
nix develop --command cargo run --release --bin gen_textures -- --sheet preview.png
nix run .#play    # the version runner; `-- release VERSION` and `-- snapshot VERSION` too
(cd tools/play && nix develop --command go test ./...)
```

Off-screen runs, using a binary from `cargo build --release`:

```sh
./target/release/deepdelve --headless --seed 42                  # end-to-end test
./target/release/deepdelve --headless --seed 777 --size 1600x900 --tour OUT_DIR
./target/release/deepdelve --headless --seed 777 --size 2560x1440 --tier 0 --bench 20
```

## Definition of done

1. `cargo fmt --check`, `cargo clippy --release --all-targets` and `cargo test --release` are clean.
   Run `go test` too if `tools/play` changed.
2. The end-to-end test passes.
3. **Anything visual is checked by eye.** Run `--tour`, open the PNGs and look at them. Tests
   can't tell if a change looks right, and most problems so far were only visible in screenshots.
4. Anything that might cost performance is benchmarked per layer against the table in
   `docs/IMPLEMENTATION.md`, on the same settings.
5. The docs are updated, as below.

Say what wasn't tested. Off-screen runs say nothing about how the game feels to play, or how its
brightness looks on a real display.

## Keeping the docs current

This file and `docs/` are how a new chat picks up the work, so maintaining them is part of every
change, not a separate task:

- **`docs/STATUS.md`:** update it before finishing any piece of work. Add new known issues, remove
  fixed ones, and keep "Next" current. Keep it short, and leave out what git history already shows.
- **`docs/DESIGN.md`:** add any design decision, marked "Player" if the user made it.
- **`docs/IMPLEMENTATION.md`:** update it when how something works changes, and when you re-measure.
- **This file:** add anything that cost real time to discover, once it's proven. Delete what stops
  being true.

## Git

Work so far has gone straight onto `main`. Commit, tag or push only when asked. Commits never make
a release: only `play release` does, and it refuses to run with uncommitted changes.

## Traps

- **Flakes can't see untracked files.** A new file makes `nix build` and `nix run` fail or use old
  content until it's `git add`ed. `cargo` reads the working tree, so it won't show the problem.
- **The repo path contains a `$`.** Always quote it. In flake URLs it must be `%24`.
- **direnv can leak this repo's `LD_LIBRARY_PATH` into a shell.** When testing old builds or other
  nixpkgs versions, run under `env -i HOME=$HOME XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR PATH=...`, or the
  results are invalid. This produced misleading results once already.
- **The system has few tools.** Python, for one, isn't installed. Get tools from the flake's pinned
  nixpkgs with `nix shell github:NixOS/nixpkgs/<rev>#python3`, taking `<rev>` from `flake.nix`.
- **Changing Go dependencies invalidates `vendorHash` in `flake.nix`.** Set it to `lib.fakeHash`,
  run `nix build .#play`, and copy the correct hash from the error.
- **Changing the version in `Cargo.toml` needs a `cargo build`,** to update `Cargo.lock`.
- **Tour screenshots with big white lines** just mean the camera ended up inside rock. It's the
  block outline, and not a rendering bug.

## Code conventions

- **Generation is pure.** Every block is a function of position and seed, so that any chunk can
  build alone, in any order, at any level of detail. Don't add dependencies between chunks.
- **Layers are data.** Most changes to a layer's character are numbers in `THEMES` in
  `src/world/generator.rs`.
- **Textures are code.** Paint them in `src/bin/gen_textures.rs`, regenerate, and commit the PNGs.
  Never edit the PNGs by hand. New ones also need a line in `texture_layers!` in
  `src/world/block.rs`, and a test checks any layer numbers `shaders/terrain.wgsl` relies on.
- **Scrolling texture scales must repeat a whole number of times per 32 blocks** (k/32), because
  UVs restart at each chunk. Anything else leaves seams at chunk borders.
- **Surfaces shared across detail levels sit on multiples of 16,** the largest voxel, like the
  bedrock's outer faces.
- **Liquids go through the water path,** so that they keep its level snapping across detail levels.
- Match the comment style around you: comments say why, and tests describe behaviour.
