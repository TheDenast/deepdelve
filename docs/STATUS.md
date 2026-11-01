# Status

Where the work stands. Update this before finishing any piece of work. For what's in the working
tree right now, check `git status` and `git log a0.1.0..HEAD`, since this file won't keep up with
those.

## Current focus

Adding variety to the world, now that the release system is in place. The user wants to experiment
with features, one request at a time.

## Since the last release (`a0.1.0`)

- Three themed layers instead of 45 random tiers: Overworld, Dark Forest, Underworld.
- Bedrock and void at both ends of the world.
- Lava, as a second liquid.
- Every texture regenerated from code.
- Level plains between the hills in the Overworld and Dark Forest, with calmer ground overall.
- The version number hasn't been bumped, so these builds still call themselves `a0.1.0`. Packaged
  builds show that exactly, and cargo builds show `a0.1.0-dev`.

## Known issues

- **Small conifers look odd up close,** a bit like cacti, though they're fine at a distance and
  the giants look right. Look at `conifer()` in `src/world/generator.rs`.
- **Brightness is only tuned from screenshots.** The Dark Forest and Underworld may look too dark
  or too bright on a real display.
- **The three layers haven't been checked in a real window,** only off-screen.
- **The world's underside is very dim** when seen from the void below.
- **The plains are only tuned from screenshots.** Their share and flatness may need adjusting after
  playing: see `Ground` in the themes. Ramps between shelves can look like straight stripes from
  high up, and the Dark Forest's trees hide most of its ground.

## Next

- Have the user play it, then fix what they find. Start with the issues above.
- Cut `a0.2.0` when they're happy, following `docs/RELEASES.md`.
- After that, whatever the user picks. `docs/DESIGN.md` has a list of ideas.
