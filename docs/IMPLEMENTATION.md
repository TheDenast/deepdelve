# Implementation

## Stack

Rust with [wgpu](https://wgpu.rs) 30 on Vulkan, winit for windows and input, glam for math.
There is no game engine. Dependencies come from `flake.nix`, whose nixpkgs is **pinned to
the running NixOS revision on purpose**: the Vulkan loader in the shell loads the system's
Mesa driver, and both need the same glibc. If Vulkan fails to initialize after a system
upgrade, re-pin it with the command in the comment at the top of `flake.nix`.

## Layout

| Path | Purpose |
| --- | --- |
| `src/world/generator.rs` | Terrain: tiers, caverns, trees, chasms, the light field |
| `src/world/noise.rs` | Seeded Perlin noise and hashes |
| `src/world/block.rs` | Block ids, texture layers |
| `src/world/mod.rs` | `ChunkKey`, padded voxel layout |
| `src/mesher.rs` | Greedy mesher with ambient occlusion |
| `src/stream.rs` | Octree of wanted chunks, worker threads, draw selection, edits |
| `src/game.rs` | Camera, raycast, breaking, atmosphere, overlay text |
| `src/app.rs` | Window, input, presentation |
| `src/headless.rs` | Off-screen runs: screenshots, benchmark, end-to-end test |
| `src/render/mod.rs` | All GPU work: buffers, culling, passes |
| `src/render/textures.rs`, `font.rs`, `pool.rs` | Texture array, HUD font, GPU memory allocator |
| `src/bin/gen_textures.rs` | Derives `crystal_32.png` and `grass_side_32.png` |
| `shaders/` | WGSL shaders, compiled at run time by wgpu |

## Chunks and level of detail

Chunks hold 32x32x32 voxels at every level, in true 3D with no vertical columns. A chunk at
level L has voxels of 2^L blocks, so it spans `32 << L` blocks.

The wanted set is an **octree cut** of a sphere around the camera. A node splits into eight
children if it's nearer than `384 << (L - 1)` blocks; level 4 is the root. So full-size
blocks reach about 384 blocks, then each band doubles both its size and its voxels, out to
4,200. That's about 39,000 chunks, of which about 8,000 have any geometry, since rock and
open air are uniform. `leaves_do_not_overlap_and_fill_the_sphere` tests the invariant.

**Every chunk builds independently.** It's generated with a one-voxel border from pure
functions of world position, so meshing never waits on a neighbour, and a coarse chunk is
just the same functions sampled further apart. Uniform chunks are stored as a single byte.
Voxel data is only kept at level 0, which is the only level that can be edited.

**No holes while loading.** Draw selection walks the octree. If a wanted chunk is missing, it
draws whatever is loaded for that volume, coarser or finer, and old chunks stay until their
replacement arrives. Workers take the most urgent job first: distance divided by voxel size,
with a bonus for chunks in front of the camera. So coarse far terrain shows up first.

**Cracks between levels** are hidden by skirts: coarse chunks also emit the faces where a
surface crosses their border. Water top faces snap to the true water height in the vertex
shader, so a sea stays one flat plane across every level.

## Generation

Two costs are avoided. Slow 2D fields are computed on a coarse lattice and interpolated.
3D noise is computed lazily on another lattice, and only near surfaces. Chunks outside a
tier's height band skip all of it. Trees are found by hashing grid cells, so a chunk knows
about trees rooted in its neighbours without those being generated. A tree's position and
shape depend only on the seed, so both chunks agree on it.

Everything uses f64 for absolute horizontal positions and f32 only for offsets. The world
works unchanged 20 million blocks from the origin, which was checked by test screenshot.

## Meshing

Voxels become bit columns along x. Exposed faces are then one shift-and-mask per column, for
all six directions, so the cost follows the number of visible faces, not the chunk's volume.
Faces go into per-plane buffers and are merged into rectangles by a greedy pass, joining only
those with the same texture and the same AO pattern. Per-corner AO gets its triangle diagonal
flipped in the vertex shader to avoid the usual lighting anisotropy.

`matches_a_brute_force_face_count` compares this to the plain definition on a noisy volume.

Quads are 8 bytes: position, size, direction, texture layer, AO and an emissive flag. They're
grouped by direction so that a whole direction can be skipped when it faces away.

**Breaking a block** updates that chunk and the border of every neighbour touching it (up to
eight chunks at a corner), since the border drives culling and AO. Those chunks remesh
synchronously on the main thread, about 1 ms, so the result shows in the same frame. Edits
also go in a separate map that workers apply when rebuilding, so they survive unloading.

## Rendering

**Vertex pulling.** All quads live in one storage buffer, handled by a range allocator. A
small repeating index buffer gives each quad 4 vertices; `base_vertex` picks the first quad,
so the shader reads `quads[vertex_index / 4]`. `first_instance` indexes per-draw data, so
each chunk direction costs one multi-draw entry, with no state changes or per-chunk binds.

**Frame:**

1. CPU: frustum test per chunk, drop directions facing away. Chunks are already sorted near
   to far, which helps early Z.
2. Compute: build a hierarchical depth pyramid from the previous frame, then test each draw
   against it, zeroing the instance count of those hidden. Boxes are tested in the previous
   frame's clip space, and anything outside that frame's view is kept.
3. Opaque terrain into an HDR target, using reversed-Z with an infinite far plane.
4. Water, back to front. It reads a copy of the opaque depth, so absorption follows how deep
   the water really is at each pixel.
5. Bloom chain, then composite with a filmic tonemap, upscaled to the window, HUD on top.

**Light** is a 5x5x5 byte grid per chunk, sampled from the same fields as the terrain and
interpolated per vertex, so it costs nothing per block and keeps quads mergeable. Being a
smooth large-scale field, it needs no flood fill. Minecraft-style block light couldn't reach
across a cavern anyway.

## Measurements

Radeon 780M integrated GPU, Ryzen 7 7840U, 14 worker threads, seed 777 unless noted.

| Measure | Result |
| --- | --- |
| Full sphere, cold start | about 2.1 s for about 39,000 chunks and 8M quads (65 MB of GPU data) |
| Average job (generate and mesh) | about 720-770 us |
| Break a block | about 1-1.5 ms, including remeshing |
| Occlusion culling correctness | 0 differing pixels against culling off, in 3 viewpoints |
| Occlusion culling benefit | Removes about 5-25% of quads at a cost of about 0.15-0.3 ms |

Flying through caverns:

| Resolution, flight speed | Frame mean (fps) | p50 | p95 | p99 | Worst | GPU terrain | Main thread mean, p99 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1920x1080, 120 blocks/s | 5.2 ms (190) | 4.7 | 9.0 | 11.6 | 22.4 | 1.9 ms | 0.4, 5.7 ms |
| 2560x1440, 120 blocks/s | 7.6 ms (130) | 6.9 | 12.4 | 15.9 | 23.4 | 2.3 ms | 0.7, 7.6 ms |
| 2560x1440, 600 blocks/s | 9.5 ms (105) | 8.6 | 14.3 | 17.2 | 37.7 | 3.4 ms | 1.2, 7.8 ms |


Frame times come from the off-screen benchmark, which waits for the GPU after each frame.
They don't include swapchain presentation. The benchmark flies along open caverns, and the
quads drawn stay near 1M in most views, so views packed with geometry can cost more.

## Verifying a change

`cargo test --release` covers the mesher, allocator and octree. `deepdelve --headless` with no
other options runs the full game path against generated terrain: it breaks a block through a
raycast, checks the edit survives flying away and back, then checks the R reset. A headless
`--tour` gives screenshots of many caverns for judging terrain and rendering changes.

A real window can be tested with no display of your own, inside weston:

```sh
weston --backend=headless --renderer=gl --socket=test &
WAYLAND_DISPLAY=test ./target/release/deepdelve --exit-after 12
```
