# Status

Where the work stands. Update this before finishing any piece of work. For what's in the working
tree right now, check `git status` and `git log a0.2.0..HEAD`, since this file won't keep up with
those.

## Current focus

For the next version, the user asked for Minecraft's creative mode: block placement, then
collision, walking and jumping, with a key to turn collision off for free flight, then a creative
inventory that scales to many more blocks. All are done.

## Since the last release (`a0.2.0`)

- Block placement, a hotbar of nine blocks drawn as cube icons, and picking blocks with the middle
  mouse button.
- Collision, walking, jumping, sneaking and swimming, creative flight on a double tap of Space,
  and N for noclip, the old free flight. The player now spawns on the ground.
- A creative inventory on E, after early Minecraft: every placeable block in a scrolling window, to
  drag or click onto the hotbar.

## Known issues

- **Small conifers look odd up close,** a bit like cacti, though they're fine at a distance and
  the giants look right. Look at `conifer()` in `src/world/generator.rs`.
- **Brightness is only tuned from screenshots.** The Dark Forest and Underworld may look too dark
  or too bright on a real display.
- **The three layers haven't been checked in a real window,** only off-screen.
- **Building and walking haven't been played.** Every new key and mouse button is untested in a
  real window, and the movement numbers (speeds, jump, acceleration, the 0.3 s double tap) only
  match Minecraft on paper. They're constants at the top of `src/player.rs`.
- **The inventory has only been driven by tests,** never with a real pointer. Watch for the
  pointer not appearing or not lining up with the slots, which would be a window or scaling issue
  in `src/app.rs`. With 20 blocks the list fits without scrolling, so only tests have scrolled it.
- **The world's underside is very dim** when seen from the void below.
- **Ground may now look too plain.** Evening out made grass and sand much smoother from a distance.
  If so, lower `PASSES` in `Canvas::even_out`, though the repeat will start to come back.
- **The plains are only tuned from screenshots.** Their share and flatness may need adjusting after
  playing: see `Ground` in the themes. Ramps between shelves can look like straight stripes from
  high up, and the Dark Forest's trees hide most of its ground.

## Next

- The user plays the new version, and movement gets tuned from their feedback.
- Then whatever the user picks. `docs/DESIGN.md` has a list of ideas, and the issues above are
  open.
