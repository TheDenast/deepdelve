# Game design

The fantasy: enormous caverns underground, with whole forests, lakes and seas in them. The
current stage is a proof of concept, in which you fly around a generated world in spectator
mode and can break blocks. Each decision below records who made it. "Player" means it was
chosen during design consultation; "engineering" means it was left to implementation and
is open to change.

## Decisions

| Topic | Decision | Made by |
| --- | --- | --- |
| Cavern size | Huge: about 1-2 km wide and 200-400 blocks tall, with rock pillars and a visible ceiling | Player |
| View range | About 4 km, with full-size blocks out to about 384 | Player |
| Light source | Glowing crystal ceilings act as a dim "cavern sun" | Player |
| World layout | Stacked tiers, about 1,400 blocks apart, over +/-32,000 blocks of height, with no surface | Player |
| First biome | Forest caverns with lakes and seas, all one biome family | Player |
| Trees | Mixed: dense 8-15 block trees plus rare giants 40-90 blocks tall | Player |
| Water | Static, with an animated surface. It never flows | Player |
| Graphics stack | wgpu, no game engine | Player |
| Controls, flight speeds, reach of 96 blocks | See the table in the README | Engineering |

## World structure

**Tiers.** The world is a stack of 45 tiers. Each has its own water level, colour mood and
cavern layout, so flying between them feels like entering another world. Tiers are placed
irregularly (up to 220 blocks off a regular spacing) so the pattern is harder to notice.

**Caverns.** Within a tier, each column has a floor height and a ceiling height. A cavern is
wherever the ceiling is above the floor. Those two surfaces come from broad noise, which is
what guarantees wide, walkable floors. Pure 3D noise makes Swiss cheese with few flat areas.
3D noise then roughens walls and adds overhangs, on top of that shape.

**Features inside a cavern**

- *Rock pillars*: hourglass columns joining floor and ceiling, usually 22-82 blocks in radius.
- *Stalactites and stalagmites*: rock spikes. Ceiling spikes are denser under crystal fields.
- *Chasms*: rare vertical shafts of 28-90 blocks in radius, connecting a tier to the next.
  They only open on dry ground, because static water couldn't pour down them.

**Water.** Everything open below a tier's water level is water, so one plane spans the whole
tier. About a third of tiers set it high ("sea tiers"), drowning the low ground and leaving
islands. The others set it low, leaving scattered lakes. Terrain height also drifts by
+/-150 blocks over a few kilometres, so one tier holds both dry forest and open water.

**Forests** grow in stands with meadows between them. Small trees come in three shapes:
broadleaf, two-lobed and conifer-like. Giants are the landmarks: they stay visible across a
cavern, where small trees blur into ground texture.

**Light.** Crystal grows in colonies on ceilings, in large fields. Those fields also drive
the ambient light, so the floor beneath a dense field is visibly brighter, with darkness
gathering towards the cavern walls. Each tier's colour mood tints its light, crystal and
haze: amber, cyan, teal, rose and so on. Distant terrain fades into coloured haze, which
gives a sense of depth.

## Known simplifications at this stage

- No gameplay: no collision, no block placing, no inventory, no creatures.
- One biome family. Variety comes from the mood of each tier and the amount of water.
- Crystal does not cast local light. It looks like it does because the ambient field follows
  it at large scale, but a single colony you carve free won't brighten what's nearby.
- Edits exist only at full detail: chunks beyond 384 blocks show the unedited world. They
  also aren't saved to disk, and R discards them.
- Breaking a block below the water line leaves a hole of air beside a standing wall of water.

## Ideas for later

Deeper tiers with their own biomes (giant mushrooms, magma); flowing water; tunnels joining
caverns horizontally; local light from crystal; collision and walking; ground plants.
