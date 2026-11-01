# Releases

A release is a git tag plus a folder. Each one can be run again at any time through `play`,
from an existing build or from its source.

## Running versions

```sh
nix run .#play
```

| Key | Action |
| --- | --- |
| Up, Down | Choose a version |
| Enter | Run it, building first if needed |
| B | Build without running |
| D | Remove a release's build, which then rebuilds from source on its next run |
| A | Set launch options for this version, such as `--seed 777 --no-vsync` |
| L | Switch between the details and the last output |
| S | Stop what's running |

"Build and run current" is `cargo run --release` on the working tree, including uncommitted
changes. The game's overlay and `--version` show which build you're in: `a0.1.0` for a
packaged release, and `a0.1.0-dev` for a build from cargo.

Commits and releases are separate. Ordinary commits never appear in the runner's list: a version
only exists once `play release` tags it. `Cargo.toml` keeps the last version until the next bump,
so work done after `a0.1.0` shows as `a0.1.0-dev` even though it's newer.

## Making a release

Versions look like `a0.2.0`: the middle number is for content updates and the last for fixes.

1. Set the version in `Cargo.toml`, and run `cargo build` to update `Cargo.lock`.
2. Write `releases/a0.2.0/CHANGELOG.md`.
3. Record a benchmark and a fixed comparison screenshot, adding any others you like:
   ```sh
   nix run .#play -- snapshot a0.2.0 captures/some_shot.png
   ```
4. Commit everything.
5. Run the release:
   ```sh
   nix run .#play -- release a0.2.0
   ```
   This refuses to run if anything from the steps above is missing. It runs the tests, builds
   the package, runs the end-to-end test on that package, then tags the commit and builds
   from the tag.
6. Publish it, which the tool never does by itself:
   ```sh
   git push origin HEAD a0.2.0
   ```

Screenshots are scaled down and saved as JPEG, since anything committed stays in the
repository's history forever. The originals in `captures/` aren't committed.

Snapshots use a fixed seed and viewpoint so that versions can be compared. Performance numbers
only compare fairly on the same machine, and the screenshots only match while world generation
stays the same.

## How builds work

`nix build` of a tag gives a package, linked at `.builds/<version>/x86_64-linux`. Nix reuses a
build that already exists, and the link protects it from garbage collection.

The package carries its own Mesa, set through `VK_DRIVER_FILES`, and disables the system's
implicit Vulkan layers. Without this, old releases eventually stop starting: their Vulkan
loader can't load a driver built against a newer glibc.

Measured on a Radeon 780M, with the system on glibc 2.42 and Mesa 26.1.8. Only the first row ran
the game itself; the others are Vulkan startup checks with `vulkaninfo`:

| Simulated old release | glibc | With system driver | With its own Mesa |
| --- | --- | --- | --- |
| nixos-25.11 | 2.40 | Works | Works |
| nixos-24.11 | 2.40 | Fails | Works |
| nixos-24.05 | 2.39 | Fails | Works |
| nixos-23.11 | 2.38 | Fails | Works |

This has three costs:

- **Disk space:** about 1 GiB for each nixpkgs revision used, shared between the releases on
  that revision, and with the system if it's on the same one.
- **New hardware:** an old Mesa may not support a GPU that came out after it. Such a release
  would need building against newer nixpkgs, which could occasionally fail on a newer compiler:
  ```sh
  nix build "git+file://$PWD?ref=refs/tags/a0.1.0#default" --no-write-lock-file \
    --override-input nixpkgs github:NixOS/nixpkgs/nixos-unstable
  ```
  A `$` in the path must be written as `%24` here.
- **Other drivers:** only Mesa's drivers are visible to a release, so this doesn't work with
  NVIDIA's proprietary driver. Supporting that would mean changing the wrapper in `flake.nix`.

## Other platforms

Only local Linux builds exist so far. Nothing about a release depends on Nix: other platforms
would add a build recipe beside `buildSteps` in `tools/play/repo.go`, or a CI job triggered by
the tag, and attach their files to a GitHub Release. Nix is welcome where it works well, but isn't
required. None of the below has been tried yet, so this is what reading the code suggests:

- **Other Linux distributions:** needs its own recipe, since a Nix package only runs where its
  `/nix/store` paths exist. It should target an older glibc and use the player's own Vulkan driver,
  without the Mesa wrapper. The binary links only glibc and libgcc directly and loads the rest at
  run time, so this should be manageable.
- **Windows:** probably mostly a build task, using Nix's MinGW cross-compiler or a Windows runner.
  Vulkan is hard-coded in `src/app.rs` and `src/headless.rs`. A cross-compiled build should still be
  smoke-tested on real Windows.
- **Android:** a real port and not just another build. The renderer allocates a 768 MiB quad buffer
  and expects desktop GPU features, and the controls assume a mouse and keyboard.

**CI** can run these same `nix build` commands on a pushed tag and upload the results. Standard
GitHub runners have no GPU, so `--headless` runs there would need a software driver such as Mesa's
lavapipe, which is slow and won't catch problems specific to real GPUs. Builds would also want a
Nix binary cache such as Cachix.

## The tool

`tools/play` is a small Go program using Bubble Tea, built by the flake. It always runs from
the current checkout, so old tags don't need to contain it, but each tag does need the flake's
`default` package. It's Go and not bash because live build output and per-version launch options
outgrow a shell script quickly, and the extra cost was small. New features for it can be tried
without Nix:

```sh
cd tools/play && go test ./... && go run .
```
