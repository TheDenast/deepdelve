# Status

Where the work stands. Update this before finishing any piece of work. For what's in the working
tree right now, check `git status` and `git log a0.3.0..HEAD`, since this file won't keep up with
those.

## Current focus

`a0.3.0` is out, with Minecraft-style creative mode: building, a creative inventory, collision,
walking, jumping and swimming, and N for free flight. What comes next is up to the user.

## Since the last release (`a0.3.0`)

Nothing yet.

## Known issues

- **Small conifers look odd up close,** a bit like cacti, though they're fine at a distance and
  the giants look right. Look at `conifer()` in `src/world/generator.rs`.
- **Brightness is only tuned from screenshots.** The Dark Forest and Underworld may look too dark
  or too bright on a real display.
- **The three layers haven't been checked in a real window,** only off-screen.
- **Movement uses Minecraft's numbers,** not ones tuned in play: speeds, jump, acceleration and
  the 0.3 s double tap. They're constants at the top of `src/player.rs`.
- **The world's underside is very dim** when seen from the void below.
- **Ground may now look too plain.** Evening out made grass and sand much smoother from a distance.
  If so, lower `PASSES` in `Canvas::even_out`, though the repeat will start to come back.
- **The plains are only tuned from screenshots.** Their share and flatness may need adjusting after
  playing: see `Ground` in the themes. Ramps between shelves can look like straight stripes from
  high up, and the Dark Forest's trees hide most of its ground.

## Next

- Whatever the user picks. `docs/DESIGN.md` has a list of ideas, and the issues above are open.
