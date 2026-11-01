# Alpha 0.1.0

The first release: a proof of concept you can fly around in.

## World

- Stacked cavern tiers, each with its own water level and colour mood
- Huge caverns with rock pillars, stalactites and chasms between tiers
- Forests of small trees and rare giants, plus lakes and seas
- Glowing crystal ceilings that light the caverns

## Engine

- True 3D chunks generating in a sphere, with terrain visible about 4 km away
- Level of detail, frustum culling and GPU occlusion culling
- Greedy meshing and vertex pulling
- HDR rendering with bloom and tonemapping

## Play

- Spectator flight
- Block breaking, with edits that survive flying away and back
- World reset and a stats overlay

## Known limitations

- No collision, block placing, inventory or creatures
- Edits only show within about 384 blocks, and aren't saved to disk
