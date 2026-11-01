//! Procedural world: three stacked cavern layers carved out of rock, capped by
//! bedrock above and below, with empty void beyond.
//!
//! Each *tier* defines, per column, a floor height and a ceiling height; caverns
//! exist where the ceiling lies above the floor. 2D fields give broad, walkable
//! floors and domed roofs, then 3D noise adds overhangs and wall roughness. A
//! tier's `Theme` decides what grows and what it's made of. See docs/DESIGN.md.
//!
//! Every function here is a pure function of world position, so any chunk at
//! any LOD can be produced independently, in any order, on any thread. LOD
//! chunks simply sample the same functions at a coarser spacing.

use super::block::*;
use super::noise::{Perlin, hash3, smoothstep, splitmix, unit};
use super::{ChunkKey, PADDED, Voxels, pidx};

/// Vertical distance between tiers. The top tier is centred on height 0.
const TIER_SPACING: f64 = 1200.0;
/// The outer faces of the bedrock. Beyond these there's only void. They're multiples of
/// the largest voxel, so that every level of detail puts the surface in the same place.
pub const WORLD_TOP: i64 = 768;
pub const WORLD_BOTTOM: i64 = -2816;
/// Bedrock is always this thick, measured from its outer face...
const BEDROCK_SOLID: f64 = 7.0;
/// ...and reaches up to this much further inward, unevenly.
const BEDROCK_ROUGH: f64 = 5.0;
/// Any tier's geometry stays within `mid + BAND_BELOW ..= mid + BAND_ABOVE`.
const BAND_BELOW: f64 = -330.0;
const BAND_ABOVE: f64 = 640.0;
/// Largest 3D displacement of the surfaces.
const MAX_NOISE_AMP: f32 = 34.0;
const WALL_NOISE_AMP: f32 = 34.0;
const CEIL_NOISE_AMP: f32 = 22.0;
const FLOOR_NOISE_AMP: f32 = 6.0;

const GIANT_TREE_CELL: i64 = 120;
const MAX_TREE_HEIGHT: f64 = 125.0;
/// Farthest a giant's canopy can extend from its trunk, horizontally.
const MAX_TREE_REACH: f64 = 58.0;

const CHASM_CELL: i64 = 1700;
const PILLAR_CELL: i64 = 430;

/// What a tier's ground is made of.
#[derive(Debug)]
struct Surface {
    top: u8,
    soil: u8,
    shore: u8,
    rock: u8,
    /// A second rock, in patches where a noise value passes `alt_above`.
    rock_alt: u8,
    alt_above: f32,
    /// Draws the second rock as thin wandering veins instead of patches.
    alt_veins: bool,
    /// Stands in for a forest too far away to resolve single trees.
    canopy: u8,
}

/// How uneven a tier's ordinary floor is, away from its mountains.
#[derive(Debug)]
struct Ground {
    /// Height of the rolling hills, in blocks.
    hills: f32,
    /// More octaves add smaller lumps on top of the hills.
    hill_octaves: u32,
    /// Size of the regions that are either plains or hills, in blocks.
    plains_scale: f64,
    /// Noise value where plains begin. Lower means more of them.
    plains_from: f32,
    /// How much of the hills and bumps survives on a plain. 1 means there are no plains.
    plains_keep: f32,
    /// Height between a plain's level shelves. Larger means wider shelves.
    shelf: f32,
    /// Strength of the small bumps everywhere except on mountains.
    bumps: f32,
}

/// Noise-shaped peaks rising out of a tier's floor.
#[derive(Debug)]
struct Mountains {
    /// Size of the noise, in blocks. Larger means broader and further apart.
    scale: f64,
    /// Noise value where the peaks begin. Higher means fewer.
    threshold: f32,
    height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeStyle {
    Broadleaf,
    Conifer,
}

#[derive(Debug)]
struct Trees {
    style: TreeStyle,
    log: u8,
    leaves: u8,
    small_cell: i64,
    small_chance: f32,
    giant_chance: f32,
}

/// Everything that makes one layer of the world different from another.
#[derive(Debug)]
pub struct Theme {
    pub name: &'static str,
    /// Tallest the cavern gets, before noise.
    hmax: f32,
    /// Raises the share of the layer that's open cavern, not solid rock.
    open_bias: f32,
    liquid: u8,
    /// Height of the liquid plane, relative to the tier's middle.
    liquid_rel: f64,
    /// Colour of the light falling on everything.
    tint: [u8; 3],
    /// Crystal colour, and its brightness in the last byte (255 is about 8 times white).
    crystal: [u8; 4],
    /// How much of the ceiling crystal covers. 1 is scattered colonies.
    crystal_cover: f32,
    /// The least crystal any part of the ceiling has.
    glow_min: f32,
    /// How far below the ceiling crystal can grow, which decides if it climbs pillars.
    crystal_reach: f32,
    /// Fog colour in the cavern's open air, in linear light.
    pub fog: [f32; 3],
    pub fog_distance: f32,
    /// Light inside the cavern, and inside solid rock.
    light: f32,
    rock_light: f32,
    /// Light comes up from the liquid here, rather than down from the ceiling.
    lit_from_below: bool,
    ceiling_spikes: f32,
    ground_spikes: f32,
    pillars: f32,
    /// The least forest cover anywhere that trees can grow.
    forest_min: f32,
    ground: Ground,
    mountains: Option<Mountains>,
    surface: Surface,
    trees: Option<Trees>,
}

/// The layers, from the top down. The player starts in the first.
static THEMES: [Theme; 3] = [
    Theme {
        name: "Overworld",
        hmax: 400.0,
        open_bias: 0.10,
        liquid: WATER,
        liquid_rel: -62.0,
        tint: [255, 250, 240],
        crystal: [96, 168, 255, 40],
        crystal_cover: 7.0,
        glow_min: 0.7,
        crystal_reach: 34.0,
        fog: [0.17, 0.31, 0.52],
        fog_distance: 2600.0,
        light: 0.86,
        rock_light: 0.24,
        lit_from_below: false,
        ceiling_spikes: 0.05,
        ground_spikes: 0.03,
        pillars: 0.14,
        forest_min: 0.0,
        ground: Ground {
            hills: 40.0,
            hill_octaves: 3,
            plains_scale: 900.0,
            plains_from: -0.16,
            plains_keep: 0.12,
            shelf: 32.0,
            bumps: 0.6,
        },
        mountains: None,
        surface: Surface {
            top: GRASS,
            soil: DIRT,
            shore: SAND,
            rock: STONE,
            rock_alt: COBBLE,
            alt_above: 0.32,
            alt_veins: false,
            canopy: LEAVES,
        },
        trees: Some(Trees {
            style: TreeStyle::Broadleaf,
            log: LOG,
            leaves: LEAVES,
            small_cell: 11,
            small_chance: 0.6,
            giant_chance: 0.28,
        }),
    },
    Theme {
        name: "Dark Forest",
        hmax: 340.0,
        open_bias: 0.10,
        liquid: WATER,
        liquid_rel: -120.0,
        tint: [205, 232, 216],
        crystal: [120, 255, 205, 54],
        crystal_cover: 0.8,
        glow_min: 0.0,
        crystal_reach: 400.0,
        fog: [0.026, 0.078, 0.066],
        fog_distance: 1050.0,
        light: 0.95,
        rock_light: 0.2,
        lit_from_below: false,
        ceiling_spikes: 0.6,
        ground_spikes: 0.05,
        pillars: 0.2,
        forest_min: 0.9,
        ground: Ground {
            hills: 36.0,
            hill_octaves: 3,
            plains_scale: 700.0,
            plains_from: -0.1,
            plains_keep: 0.15,
            shelf: 28.0,
            bumps: 0.6,
        },
        mountains: Some(Mountains {
            scale: 430.0,
            threshold: 0.1,
            height: 165.0,
        }),
        surface: Surface {
            top: DARK_GRASS,
            soil: DARK_DIRT,
            shore: GRAVEL,
            rock: SLATE,
            rock_alt: MOSSY_COBBLE,
            alt_above: 0.2,
            alt_veins: false,
            canopy: PINE_NEEDLES,
        },
        trees: Some(Trees {
            style: TreeStyle::Conifer,
            log: PINE_LOG,
            leaves: PINE_NEEDLES,
            small_cell: 9,
            small_chance: 0.95,
            giant_chance: 0.45,
        }),
    },
    Theme {
        name: "Underworld",
        hmax: 400.0,
        open_bias: 0.10,
        liquid: LAVA,
        liquid_rel: -36.0,
        tint: [255, 186, 138],
        crystal: [255, 92, 28, 60],
        crystal_cover: 0.3,
        glow_min: 0.0,
        crystal_reach: 400.0,
        fog: [0.2, 0.05, 0.018],
        fog_distance: 1500.0,
        light: 1.0,
        rock_light: 0.2,
        lit_from_below: true,
        ceiling_spikes: 0.85,
        ground_spikes: 0.2,
        pillars: 0.3,
        forest_min: 0.0,
        ground: Ground {
            hills: 58.0,
            hill_octaves: 4,
            plains_scale: 900.0,
            plains_from: 0.0,
            plains_keep: 1.0,
            shelf: 0.0,
            bumps: 1.0,
        },
        mountains: Some(Mountains {
            scale: 620.0,
            threshold: -0.02,
            height: 300.0,
        }),
        surface: Surface {
            top: ASH,
            soil: ASH,
            shore: BASALT,
            rock: BASALT,
            rock_alt: EMBER,
            alt_above: 0.0,
            alt_veins: true,
            canopy: ASH,
        },
        trees: None,
    },
];

#[derive(Clone, Debug)]
pub struct Tier {
    /// Position from the top, starting at 0.
    pub index: i32,
    pub mid: f64,
    /// Everything open below this height is liquid. Always a whole number.
    pub liquid_y: f64,
    pub theme: &'static Theme,
    off: [f64; 2],
    salt: u64,
}

/// Slow-varying description of one column inside one tier.
#[derive(Clone, Copy, Default, Debug)]
pub struct Field {
    pub floor: f32,
    pub ceil: f32,
    /// 0 inside solid rock, 1 in the middle of a cavern.
    pub open: f32,
    /// Strength of the ceiling crystal fields above this column.
    pub glow: f32,
    pub forest: f32,
    /// 0 on the ordinary floor, rising to 1 on a mountain.
    pub mountain: f32,
    /// How much of the floor's unevenness applies here: 1 in hills, low on plains.
    pub rough: f32,
}

impl Field {
    fn closed(mid: f32) -> Self {
        Field {
            floor: mid,
            ceil: mid,
            rough: 1.0,
            ..Default::default()
        }
    }
}

/// Reusable per-thread working memory.
pub struct Scratch {
    lattice: Vec<Field>,
    n3: Vec<f32>,
    trees: Vec<Tree>,
    pillars: Vec<Pillar>,
    column: [f32; PADDED + 1],
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            lattice: Vec::new(),
            n3: Vec::new(),
            trees: Vec::new(),
            pillars: Vec::new(),
            column: [0.0; PADDED + 1],
        }
    }
}

#[derive(Clone, Copy)]
struct Pillar {
    x: f64,
    z: f64,
    r: f32,
}

/// Offset from the tree's trunk position.
struct Blob {
    c: [f32; 3],
    rx: f32,
    ry: f32,
}

struct Tree {
    x: f64,
    z: f64,
    base: f32,
    height: f32,
    trunk_r: f32,
    giant: bool,
    /// Whether thick branches join the trunk to the canopy.
    branches: bool,
    log: u8,
    leaves: u8,
    blobs: Vec<Blob>,
}

pub struct Generator {
    pub seed: u64,
    noise: Perlin,
    tiers: Vec<Tier>,
}

/// Per-chunk data the renderer needs besides geometry.
#[derive(Clone, Copy, Debug)]
pub struct ChunkLook {
    /// Liquid plane of the tier touching this chunk.
    pub liquid_y: f32,
    pub tint: [u8; 3],
    pub crystal: [u8; 4],
}

impl Generator {
    pub fn new(seed: u64) -> Self {
        let tiers = THEMES
            .iter()
            .enumerate()
            .map(|(i, theme)| {
                let salt = splitmix(seed ^ (i as u64 + 1).wrapping_mul(0xD6E8_FEB8_6659_FD93));
                let r = |n: u64| unit(splitmix(salt.wrapping_add(n)));
                let mid = -(i as f64) * TIER_SPACING;
                Tier {
                    index: i as i32,
                    mid,
                    liquid_y: (mid + theme.liquid_rel).floor(),
                    theme,
                    off: [(r(7) as f64) * 50_000.0, (r(8) as f64) * 50_000.0],
                    salt,
                }
            })
            .collect();
        Self {
            seed,
            noise: Perlin::new(seed),
            tiers,
        }
    }

    pub fn tiers(&self) -> &[Tier] {
        &self.tiers
    }

    /// Tier whose band is closest to a height.
    pub fn tier_near(&self, y: f64) -> &Tier {
        let centre = |t: &Tier| (y - (t.mid + (BAND_ABOVE + BAND_BELOW) * 0.5)).abs();
        self.tiers
            .iter()
            .min_by(|a, b| centre(a).total_cmp(&centre(b)))
            .expect("the world has tiers")
    }

    fn tier(&self, index: i32) -> &Tier {
        &self.tiers[index.clamp(0, self.tiers.len() as i32 - 1) as usize]
    }

    fn cavern_mask(&self, t: &Tier, x: f64, z: f64) -> f32 {
        let n = &self.noise;
        let (xo, zo) = (x + t.off[0], z + t.off[1]);
        let wx = xo + n.n2(xo / 1400.0, zo / 1400.0) as f64 * 420.0;
        let wz = zo + n.n2(xo / 1400.0 + 91.3, zo / 1400.0 - 17.7) as f64 * 420.0;
        let m = n.fbm2(wx / 2700.0, wz / 2700.0, 3) + t.theme.open_bias;
        smoothstep(-0.09, 0.14, m)
    }

    /// Full-quality field for one column. Costs about 15 noise samples.
    pub fn field(&self, t: &Tier, x: f64, z: f64) -> Field {
        let n = &self.noise;
        let open = self.cavern_mask(t, x, z);
        let und = n.n2((x + t.off[1]) / 3300.0, (z - t.off[0]) / 3300.0) * 150.0;
        let mid = (t.mid + und as f64) as f32;
        if open <= 0.0 {
            return Field::closed(mid);
        }
        let (xo, zo) = (x + t.off[0], z + t.off[1]);
        let var = 0.72 + 0.28 * n.n2(xo / 1100.0 + 7.0, zo / 1100.0 + 3.0);
        let th = t.theme;
        let h = open.powf(0.62) * th.hmax * var;
        let roof = n.fbm2(xo / 240.0 - 40.0, zo / 240.0 + 11.0, 3) * 46.0 * open;
        let glow_n = n.fbm2(xo / 520.0 + 300.0, zo / 520.0 - 200.0, 2);
        let forest_n = n.fbm2(xo / 640.0 - 123.0, zo / 640.0 + 321.0, 2);
        let mountain = th.mountains.as_ref().map_or(0.0, |m| {
            let v = n.fbm2(xo / m.scale + 613.0, zo / m.scale - 271.0, 3);
            smoothstep(m.threshold, m.threshold + 0.26, v) * open
        });
        let g = &th.ground;
        // Plains give way to rough ground before a mountain starts to rise, so that its
        // foot doesn't look pasted onto a table.
        let off_mountain = 1.0 - smoothstep(0.0, 0.3, mountain);
        let plains = if g.plains_keep < 1.0 {
            let p = n.fbm2(xo / g.plains_scale - 77.0, zo / g.plains_scale + 145.0, 2);
            smoothstep(g.plains_from, g.plains_from + 0.2, p) * off_mountain
        } else {
            0.0
        };
        let calm = 1.0 + (g.plains_keep - 1.0) * plains;
        let hills = n.fbm2(xo / 300.0, zo / 300.0, g.hill_octaves) * g.hills * open * calm;
        let mut floor = mid - 0.2 * h + hills;
        if plains > 0.0 {
            // The land's slow drift would leave a plain tilted, and striped with block
            // edges. One shelf sits just above the liquid, so that shores stay dry.
            let level = shelve(floor, t.liquid_y as f32 + 5.0, g.shelf);
            floor += (level - floor) * plains;
        }
        let ceil = mid + 0.8 * h + roof;
        if let Some(m) = &th.mountains {
            let k = mountain;
            if k > 0.0 {
                // Folding noise around zero draws sharp ridgelines along the peaks.
                let fold = n.n2(xo / (m.scale * 0.21), zo / (m.scale * 0.21)).abs();
                let ridge = 1.0 - (fold * 2.2).min(1.0);
                let crag = n.n2(xo / (m.scale * 0.07) + 31.0, zo / (m.scale * 0.07)) * 0.13;
                floor += k * k * m.height * (0.58 + 0.36 * ridge + crag);
                // Peaks can reach the ceiling, but shouldn't punch through it.
                floor = floor.min(ceil - 24.0);
            }
        }
        let forest = smoothstep(-0.10, 0.30, forest_n).max(th.forest_min);
        Field {
            floor,
            ceil,
            open,
            glow: smoothstep(-0.12, 0.32, glow_n).max(th.glow_min),
            forest: forest * (1.0 - smoothstep(0.0, 0.1, mountain)),
            mountain,
            rough: calm * (1.0 + (g.bumps - 1.0) * off_mountain),
        }
    }

    /// Fills `out` with the chunk's blocks, including its one-voxel border.
    pub fn generate(&self, key: ChunkKey, sc: &mut Scratch, out: &mut Voxels) {
        let s = key.voxel_size() as f64;
        let o = key.origin();
        let y_min = o[1] as f64 - s;
        let y_max = o[1] as f64 + 33.0 * s;
        // Past the bedrock there's nothing at all.
        if y_min >= WORLD_TOP as f64 || y_max <= WORLD_BOTTOM as f64 {
            out.fill(AIR);
            return;
        }

        self.fill_rock(key, out);
        for t in &self.tiers {
            if y_max > t.mid + BAND_BELOW && y_min < t.mid + BAND_ABOVE {
                self.carve_tier(t, key, sc, out);
            }
        }
        self.carve_chasms(key, out);
        self.stamp_bedrock(key, out);
    }

    /// Solid rock, of whichever kind belongs to each height.
    fn fill_rock(&self, key: ChunkKey, out: &mut Voxels) {
        let s = key.voxel_size() as f64;
        let y0 = key.origin()[1] as f64 - s * 0.5;
        let first = self.tier_near(y0).theme.surface.rock;
        let last = self
            .tier_near(y0 + (PADDED - 1) as f64 * s)
            .theme
            .surface
            .rock;
        if first == last {
            out.fill(first);
            return;
        }
        for (iy, slab) in out.chunks_exact_mut(PADDED * PADDED).enumerate() {
            let y = y0 + iy as f64 * s;
            slab.fill(self.tier_near(y).theme.surface.rock);
        }
    }

    /// Caps the world: flat on the void side, and jagged on the inside, like the Nether.
    fn stamp_bedrock(&self, key: ChunkKey, out: &mut Voxels) {
        let s = key.voxel_size() as f64;
        let o = key.origin();
        let (top, bottom) = (WORLD_TOP as f64, WORLD_BOTTOM as f64);
        // A coarse voxel's centre can sit further in than the bedrock is thick.
        let solid_depth = BEDROCK_SOLID.max(s * 0.6);
        let reach = solid_depth + BEDROCK_ROUGH;
        let y_lo = o[1] as f64 - s;
        let y_hi = o[1] as f64 + 33.0 * s;
        if y_hi <= top - reach && y_lo >= bottom + reach {
            return;
        }
        for iy in 0..PADDED {
            let y = o[1] as f64 + (iy as f64 - 0.5) * s;
            if y >= top || y < bottom {
                out[iy * PADDED * PADDED..(iy + 1) * PADDED * PADDED].fill(AIR);
                continue;
            }
            let depth = (top - y).min(y - bottom);
            if depth >= reach {
                continue;
            }
            for iz in 0..PADDED {
                for ix in 0..PADDED {
                    let solid = depth < solid_depth || {
                        // Only the finest detail can show single jagged blocks.
                        let x = o[0] + (ix as i64 - 1) * s as i64;
                        let z = o[2] + (iz as i64 - 1) * s as i64;
                        let h = hash3(self.seed ^ 0xBED0, x, (y * 2.0) as i64, z);
                        key.lod <= 1 && unit(h) as f64 * BEDROCK_ROUGH > depth - solid_depth
                    };
                    if solid {
                        out[pidx(ix, iy, iz)] = BEDROCK;
                    }
                }
            }
        }
    }

    fn carve_tier(&self, t: &Tier, key: ChunkKey, sc: &mut Scratch, out: &mut Voxels) {
        let lod = key.lod;
        let s = key.voxel_size() as f64;
        let o = key.origin();
        let sample = |org: i64, i: usize| org as f64 + (i as f64 - 1.0 + 0.5) * s;

        // Slow fields are computed on a world-aligned lattice and interpolated.
        let g: i64 = match lod {
            0 | 1 => 4,
            2 => 2,
            _ => 1,
        };
        let gw = g as f64 * s;
        let lx0 = ((o[0] as f64 - s) / gw).floor();
        let lz0 = ((o[2] as f64 - s) / gw).floor();
        let extent = 34.0 * s + gw;
        let ln = (extent / gw).ceil() as usize + 1;
        sc.lattice.clear();
        sc.lattice.reserve(ln * ln);
        let (mut fmin, mut fmax, mut cmin, mut cmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        let mut any_open = false;
        for lz in 0..ln {
            for lx in 0..ln {
                let f = self.field(t, (lx0 + lx as f64) * gw, (lz0 + lz as f64) * gw);
                fmin = fmin.min(f.floor);
                fmax = fmax.max(f.floor);
                cmin = cmin.min(f.ceil);
                cmax = cmax.max(f.ceil);
                any_open |= f.open > 0.0;
                sc.lattice.push(f);
            }
        }
        if !any_open {
            return;
        }

        let y_lo_chunk = o[1] as f64 - s;
        let y_hi_chunk = o[1] as f64 + 34.0 * s;
        let margin = MAX_NOISE_AMP as f64 + 70.0;
        if y_hi_chunk < fmin as f64 - margin || y_lo_chunk > cmax as f64 + margin {
            return;
        }

        self.collect_pillars(t, key, &mut sc.pillars);
        prepare_n3(lod, &mut sc.n3);

        let th = t.theme;
        let liquid_y = t.liquid_y as f32;
        let detail = lod <= 2;
        let mut column = sc.column;
        for iz in 0..PADDED {
            let z = sample(o[2], iz);
            let tz = (z / gw - lz0) as f32;
            let lz = tz.floor() as usize;
            let fz = tz - lz as f32;
            for ix in 0..PADDED {
                let x = sample(o[0], ix);
                let tx = (x / gw - lx0) as f32;
                let lx = tx.floor() as usize;
                let fx = tx - lx as f32;
                let l = &sc.lattice;
                let a = &l[lz * ln + lx];
                let b = &l[lz * ln + lx + 1];
                let c = &l[(lz + 1) * ln + lx];
                let d = &l[(lz + 1) * ln + lx + 1];
                let open = bilerp(a.open, b.open, c.open, d.open, fx, fz);
                if open <= 0.0 {
                    continue;
                }
                let mut floor = bilerp(a.floor, b.floor, c.floor, d.floor, fx, fz);
                let mut ceil = bilerp(a.ceil, b.ceil, c.ceil, d.ceil, fx, fz);
                let glow = bilerp(a.glow, b.glow, c.glow, d.glow, fx, fz);
                let forest = bilerp(a.forest, b.forest, c.forest, d.forest, fx, fz);
                let mountain = bilerp(a.mountain, b.mountain, c.mountain, d.mountain, fx, fz);
                let rough = bilerp(a.rough, b.rough, c.rough, d.rough, fx, fz);
                // Slope of the floor, for deciding what can grow here.
                let dfx = ((b.floor - a.floor) * (1.0 - fz) + (d.floor - c.floor) * fz) / gw as f32;
                let dfz = ((c.floor - a.floor) * (1.0 - fx) + (d.floor - b.floor) * fx) / gw as f32;
                let slope = (dfx * dfx + dfz * dfz).sqrt();

                if detail {
                    floor += self.noise.n2(x / 19.0, z / 19.0) * 3.4 * open * rough;
                    if lod == 0 {
                        floor += self.noise.n2(x / 7.0 + 50.0, z / 7.0) * 1.3 * open * rough;
                    }
                    ceil -= self.spikes(t, x, z, glow) * open;
                    floor += self.stalagmite(t, x, z, forest) * open;
                    if mountain > 0.0 {
                        floor += self.noise.n2(x / 37.0 - 9.0, z / 37.0 + 4.0) * 13.0 * mountain;
                        floor += self.noise.n2(x / 13.0 + 21.0, z / 13.0) * 4.5 * mountain;
                    }
                }

                // Pillars pinch the column towards the cavern's middle height.
                let mut pillar_d = f32::MAX;
                for p in &sc.pillars {
                    let dx = (x - p.x) as f32;
                    let dz = (z - p.z) as f32;
                    let dist = (dx * dx + dz * dz).sqrt() - p.r;
                    if dist < pillar_d {
                        pillar_d = dist;
                    }
                }

                let amp = WALL_NOISE_AMP + (CEIL_NOISE_AMP - WALL_NOISE_AMP) * open;
                let floor_amp = WALL_NOISE_AMP + (FLOOR_NOISE_AMP * rough - WALL_NOISE_AMP) * open;
                let cy_mid = (floor + ceil) * 0.5;
                let half = ((ceil - floor) * 0.5).max(1.0);

                let reach = MAX_NOISE_AMP + s as f32;
                let y0 = ((floor - reach) as f64 - sample(o[1], 0)) / s;
                let y1 = ((ceil + reach) as f64 - sample(o[1], 0)) / s;
                let yl = (y0.floor().max(0.0)) as usize;
                let yh = (y1.ceil().min(PADDED as f64)) as usize;
                if yl >= PADDED || yl > yh {
                    continue;
                }

                // Density above zero means open space.
                #[allow(clippy::needless_range_loop)]
                for iy in yl..=yh {
                    let y = sample(o[1], iy) as f32;
                    let bf = y - floor;
                    let bc = ceil - y;
                    let mut d = if bf < bc {
                        if bf.abs() > floor_amp {
                            bf
                        } else {
                            bf + self.n3_at(&mut sc.n3, lod, o, ix, iy, iz) * floor_amp
                        }
                    } else if bc.abs() > amp {
                        bc
                    } else {
                        bc + self.n3_at(&mut sc.n3, lod, o, ix, iy, iz) * amp
                    };
                    if pillar_d < 90.0 {
                        let t01 = (y - cy_mid) / half;
                        let pinch = 0.5 + 0.9 * t01 * t01;
                        let wob = self.n3_at(&mut sc.n3, lod, o, ix, iy, iz) * 14.0;
                        let pd = (pillar_d + wob) * 1.0 - (pinch - 1.0) * 26.0;
                        d = d.min(pd);
                    }
                    column[iy] = d;
                }

                // Blocks are picked from a column's densities.
                let mut rough_n = if lod <= 3 {
                    self.noise.n2(x / 34.0 + 11.0, z / 34.0 - 77.0)
                } else {
                    0.0
                };
                // Veins follow noise's zero lines, which close into rings like the contours
                // of a map unless something breaks them up.
                if th.surface.alt_veins && self.noise.n2(x / 83.0 - 31.0, z / 83.0 + 17.0) < 0.06 {
                    rough_n = 1.0;
                }
                for iy in yl..yh.min(PADDED) {
                    let d = column[iy];
                    let y = sample(o[1], iy) as f32;
                    let idx = pidx(ix, iy, iz);
                    if d > 0.0 {
                        out[idx] = if y < liquid_y { th.liquid } else { AIR };
                        continue;
                    }
                    let exposed = iy < yh && column[iy + 1] > 0.0;
                    let floor_side = (y - floor) < (ceil - y);
                    let depth = -d;
                    out[idx] = if floor_side {
                        pick_floor_block(
                            &th.surface,
                            exposed,
                            depth,
                            y,
                            y - floor,
                            liquid_y,
                            slope,
                            forest,
                            mountain,
                            rough_n,
                            lod,
                            s as f32,
                        )
                    } else {
                        pick_ceiling_block(
                            th,
                            self.seed,
                            depth,
                            x,
                            z,
                            ceil - y,
                            glow,
                            open,
                            s as f32,
                        )
                    };
                    if out[idx] == STONE && lod == 0 && depth < 14.0 {
                        out[idx] = ore_at(self.seed, x, y as f64, z);
                    }
                }
            }
        }
        sc.column = column;

        // Trees sit on the floor; skip the work when the chunk is nowhere near it.
        if th.trees.is_some()
            && y_hi_chunk > fmin as f64 - 8.0
            && y_lo_chunk < fmax as f64 + MAX_TREE_HEIGHT
        {
            self.collect_trees(t, key, &mut sc.trees);
            for tree in &sc.trees {
                stamp_tree(tree, key, self.seed, out);
            }
        }
    }

    /// Downward crystal/stone spikes under the ceiling.
    #[inline]
    fn spikes(&self, t: &Tier, x: f64, z: f64, glow: f32) -> f32 {
        const CELL: f64 = 26.0;
        let cx = (x / CELL).floor();
        let cz = (z / CELL).floor();
        let h = hash3(t.salt ^ 0x51, cx as i64, 3, cz as i64);
        let keep = unit(h);
        let density = t.theme.ceiling_spikes * (0.3 + 0.7 * glow);
        if keep > density {
            return 0.0;
        }
        let h2 = splitmix(h);
        let jx = (cx + 0.3 + 0.4 * unit(h2) as f64) * CELL;
        let jz = (cz + 0.3 + 0.4 * unit(h2 >> 24) as f64) * CELL;
        let r = 3.0 + 7.0 * unit(h2 >> 12);
        let dx = (x - jx) as f32;
        let dz = (z - jz) as f32;
        let dist = (dx * dx + dz * dz).sqrt();
        if dist >= r {
            return 0.0;
        }
        let k = 1.0 - dist / r;
        k * k * (10.0 + 42.0 * unit(h2 >> 36))
    }

    /// Occasional upward rock spikes on open ground.
    #[inline]
    fn stalagmite(&self, t: &Tier, x: f64, z: f64, forest: f32) -> f32 {
        const CELL: f64 = 44.0;
        let cx = (x / CELL).floor();
        let cz = (z / CELL).floor();
        let h = hash3(t.salt ^ 0x77, cx as i64, 9, cz as i64);
        if unit(h) > t.theme.ground_spikes * (1.0 - forest * 0.8) {
            return 0.0;
        }
        let h2 = splitmix(h);
        let jx = (cx + 0.3 + 0.4 * unit(h2) as f64) * CELL;
        let jz = (cz + 0.3 + 0.4 * unit(h2 >> 24) as f64) * CELL;
        let r = 3.0 + 8.0 * unit(h2 >> 12);
        let dx = (x - jx) as f32;
        let dz = (z - jz) as f32;
        let dist = (dx * dx + dz * dz).sqrt();
        if dist >= r {
            return 0.0;
        }
        let k = 1.0 - dist / r;
        k * k * (6.0 + 22.0 * unit(h2 >> 36))
    }

    #[inline]
    fn n3_at(&self, buf: &mut [f32], lod: u8, o: [i64; 3], ix: usize, iy: usize, iz: usize) -> f32 {
        // Lattice step, in samples.
        let (g, dim) = n3_lattice(lod);
        let s = (1i64 << lod) as f64;
        let cell = |i: usize| i / g;
        let frac = |i: usize| (i % g) as f32 / g as f32;
        let (cx, cy, cz) = (cell(ix), cell(iy), cell(iz));
        let (fx, fy, fz) = (frac(ix), frac(iy), frac(iz));
        let mut sum = 0.0;
        for k in 0..8usize {
            let (bx, by, bz) = (k & 1, (k >> 1) & 1, (k >> 2) & 1);
            let wx = if bx == 1 { fx } else { 1.0 - fx };
            let wy = if by == 1 { fy } else { 1.0 - fy };
            let wz = if bz == 1 { fz } else { 1.0 - fz };
            let w = wx * wy * wz;
            if w == 0.0 {
                continue;
            }
            let (lx, ly, lz) = (cx + bx, cy + by, cz + bz);
            let i = lz * dim * dim + ly * dim + lx;
            let mut v = buf[i];
            if v.is_nan() {
                let wxp = o[0] as f64 + ((lx * g) as f64 - 1.0 + 0.5) * s;
                let wyp = o[1] as f64 + ((ly * g) as f64 - 1.0 + 0.5) * s;
                let wzp = o[2] as f64 + ((lz * g) as f64 - 1.0 + 0.5) * s;
                v = self.noise.n3(wxp / 64.0, wyp / 44.0, wzp / 64.0);
                if lod <= 2 {
                    v += 0.42
                        * self
                            .noise
                            .n3(wxp / 21.0 + 100.0, wyp / 15.0, wzp / 21.0 - 40.0);
                }
                if lod == 0 {
                    v += 0.14 * self.noise.n3(wxp / 8.0 - 60.0, wyp / 6.0 + 9.0, wzp / 8.0);
                }
                buf[i] = v;
            }
            sum += v * w;
        }
        sum
    }

    fn collect_pillars(&self, t: &Tier, key: ChunkKey, out: &mut Vec<Pillar>) {
        out.clear();
        let o = key.origin();
        let size = key.size();
        let reach = 210i64;
        let cx0 = (o[0] - reach).div_euclid(PILLAR_CELL);
        let cx1 = (o[0] + size + reach).div_euclid(PILLAR_CELL);
        let cz0 = (o[2] - reach).div_euclid(PILLAR_CELL);
        let cz1 = (o[2] + size + reach).div_euclid(PILLAR_CELL);
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let h = hash3(t.salt ^ 0x1234, cx, 0, cz);
                if unit(h) > t.theme.pillars {
                    continue;
                }
                let h2 = splitmix(h);
                let x = (cx as f64 + 0.2 + 0.6 * unit(h2) as f64) * PILLAR_CELL as f64;
                let z = (cz as f64 + 0.2 + 0.6 * unit(h2 >> 24) as f64) * PILLAR_CELL as f64;
                let r = 22.0 + 60.0 * unit(h2 >> 12).powi(2);
                out.push(Pillar { x, z, r });
            }
        }
    }

    fn collect_trees(&self, t: &Tier, key: ChunkKey, out: &mut Vec<Tree>) {
        out.clear();
        let Some(trees) = &t.theme.trees else {
            return;
        };
        let o = key.origin();
        let size = key.size() as f64;
        let voxel = key.voxel_size() as f64;
        // A dense forest at this distance would be costly and look no different at half
        // the trees, which also stay the same ones as you approach.
        let thin = key.lod == 3 && trees.small_cell < 10;
        let scan = |cell: i64, reach: f64, giant: bool, out: &mut Vec<Tree>| {
            let cx0 = ((o[0] as f64 - reach) / cell as f64).floor() as i64;
            let cx1 = ((o[0] as f64 + size + reach) / cell as f64).floor() as i64;
            let cz0 = ((o[2] as f64 - reach) / cell as f64).floor() as i64;
            let cz1 = ((o[2] as f64 + size + reach) / cell as f64).floor() as i64;
            for cz in cz0..=cz1 {
                for cx in cx0..=cx1 {
                    if !giant && thin && (cx + cz) & 1 != 0 {
                        continue;
                    }
                    if let Some(tree) = self.tree_in_cell(t, trees, cx, cz, giant) {
                        out.push(tree);
                    }
                }
            }
        };
        scan(GIANT_TREE_CELL, MAX_TREE_REACH + voxel, true, out);
        // Small trees would vanish between samples beyond LOD 3.
        if key.lod <= 3 {
            scan(trees.small_cell, 11.0 + voxel, false, out);
        }
    }

    fn tree_in_cell(&self, t: &Tier, trees: &Trees, cx: i64, cz: i64, giant: bool) -> Option<Tree> {
        let (cell, chance) = if giant {
            (GIANT_TREE_CELL as f64, trees.giant_chance)
        } else {
            (trees.small_cell as f64, trees.small_chance)
        };
        let h = hash3(t.salt ^ if giant { 0xA5A5 } else { 0x5A5A }, cx, 1, cz);
        let keep = unit(h);
        if keep > chance {
            return None;
        }
        let h2 = splitmix(h);
        let x = ((cx as f64 + unit(h2) as f64) * cell).floor() + 0.5;
        let z = ((cz as f64 + unit(h2 >> 24) as f64) * cell).floor() + 0.5;
        let f = self.field(t, x, z);
        if f.open < 0.55 || keep > chance * f.forest {
            return None;
        }
        if f.floor < t.liquid_y as f32 + 3.0 {
            return None;
        }
        let f2 = self.field(t, x + 6.0, z);
        let f3 = self.field(t, x, z + 6.0);
        let slope = ((f2.floor - f.floor).powi(2) + (f3.floor - f.floor).powi(2)).sqrt() / 6.0;
        if slope > 0.9 || self.over_chasm(t.index as usize, x, z) {
            return None;
        }

        let base = f.floor - 3.0;
        let r = |n: u32| unit(h2.rotate_left(n * 7) ^ (n as u64).wrapping_mul(0x1F3B));
        let (height, trunk_r, branches, blobs) = match (trees.style, giant) {
            (TreeStyle::Broadleaf, true) => giant_broadleaf(base, &r),
            (TreeStyle::Broadleaf, false) => small_broadleaf(base, &r),
            (TreeStyle::Conifer, true) => {
                let height = 56.0 + 50.0 * r(1);
                conifer(base, height, height * 0.2, &r)
            }
            (TreeStyle::Conifer, false) => {
                let height = 13.0 + 20.0 * r(1);
                conifer(base, height, 1.6 + height * 0.2, &r)
            }
        };
        if f.ceil - f.floor < height + if giant { 25.0 } else { 12.0 } {
            return None;
        }
        // A conifer's trunk ends inside its needles, or the tip would show from afar.
        let trunk_height = match trees.style {
            TreeStyle::Broadleaf => height + 3.0,
            TreeStyle::Conifer => height * 0.74,
        };
        Some(Tree {
            x,
            z,
            base,
            height: trunk_height,
            trunk_r,
            giant,
            branches,
            log: trees.log,
            leaves: trees.leaves,
            blobs,
        })
    }

    /// Rare vertical shafts joining a tier to the one below it.
    fn carve_chasms(&self, key: ChunkKey, out: &mut Voxels) {
        let o = key.origin();
        let size = key.size();
        let s = key.voxel_size() as f64;
        let reach = 110i64;
        let cx0 = (o[0] - reach).div_euclid(CHASM_CELL);
        let cx1 = (o[0] + size + reach).div_euclid(CHASM_CELL);
        let cz0 = (o[2] - reach).div_euclid(CHASM_CELL);
        let cz1 = (o[2] + size + reach).div_euclid(CHASM_CELL);
        let y_lo = o[1] as f64 - s;
        let y_hi = o[1] as f64 + 33.0 * s;
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let h = hash3(self.seed ^ 0xC4A5, cx, 0, cz);
                let h2 = splitmix(h);
                let x = (cx as f64 + 0.2 + 0.6 * unit(h2) as f64) * CHASM_CELL as f64;
                let z = (cz as f64 + 0.2 + 0.6 * unit(h2 >> 24) as f64) * CHASM_CELL as f64;
                let r = 28.0 + 62.0 * unit(h2 >> 12) as f64;
                for up in 0..self.tiers.len() - 1 {
                    let upper = &self.tiers[up];
                    let lower = &self.tiers[up + 1];
                    if y_hi < lower.mid || y_lo > upper.mid + BAND_ABOVE {
                        continue;
                    }
                    if unit(hash3(h, up as i64, 5, 0)) > 0.5 {
                        continue;
                    }
                    let Some((fu, fl)) = self.chasm_fields(up, x, z) else {
                        continue;
                    };
                    let top = fu.floor as f64 + 6.0;
                    let bottom = fl.ceil as f64 - 10.0;
                    for iz in 0..PADDED {
                        let vz = o[2] as f64 + (iz as f64 - 0.5) * s;
                        for ix in 0..PADDED {
                            let vx = o[0] as f64 + (ix as f64 - 0.5) * s;
                            let dx = vx - x;
                            let dz = vz - z;
                            if dx * dx + dz * dz > (r + 30.0) * (r + 30.0) {
                                continue;
                            }
                            for iy in 0..PADDED {
                                let vy = o[1] as f64 + (iy as f64 - 0.5) * s;
                                if vy > top || vy < bottom {
                                    continue;
                                }
                                let wob = self.noise.n3(vx / 30.0, vy / 45.0, vz / 30.0) as f64
                                    * 22.0
                                    + self.noise.n2(vy / 160.0, h as u32 as f64 / 1.0e7) as f64
                                        * 16.0;
                                let rr = r + wob;
                                if dx * dx + dz * dz < rr * rr {
                                    let i = pidx(ix, iy, iz);
                                    if !is_liquid(out[i]) {
                                        out[i] = AIR;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The floor above and ceiling below a chasm, if one really opens at this spot.
    fn chasm_fields(&self, up: usize, x: f64, z: f64) -> Option<(Field, Field)> {
        let (upper, lower) = (&self.tiers[up], &self.tiers[up + 1]);
        let (fu, fl) = (self.field(upper, x, z), self.field(lower, x, z));
        (fu.open >= 0.7 && fl.open >= 0.7 && fu.floor >= upper.liquid_y as f32 + 10.0)
            .then_some((fu, fl))
    }

    /// Is this column over the mouth of a chasm leading down from the tier? Nothing above
    /// the floor gets carved out, so a tree growing here would float.
    fn over_chasm(&self, tier: usize, x: f64, z: f64) -> bool {
        if tier + 1 >= self.tiers.len() {
            return false;
        }
        let (cx, cz) = (
            (x / CHASM_CELL as f64).floor() as i64,
            (z / CHASM_CELL as f64).floor() as i64,
        );
        for j in cz - 1..=cz + 1 {
            for i in cx - 1..=cx + 1 {
                let h = hash3(self.seed ^ 0xC4A5, i, 0, j);
                if unit(hash3(h, tier as i64, 5, 0)) > 0.5 {
                    continue;
                }
                let h2 = splitmix(h);
                let sx = (i as f64 + 0.2 + 0.6 * unit(h2) as f64) * CHASM_CELL as f64;
                let sz = (j as f64 + 0.2 + 0.6 * unit(h2 >> 24) as f64) * CHASM_CELL as f64;
                let r = 28.0 + 62.0 * unit(h2 >> 12) as f64;
                // Generous, since the shaft's edge wobbles and a canopy has width.
                let reach = r + 56.0;
                if (x - sx).powi(2) + (z - sz).powi(2) < reach * reach
                    && self.chasm_fields(tier, sx, sz).is_some()
                {
                    return true;
                }
            }
        }
        false
    }

    /// Light at a world position, 0..1. Evaluated on a coarse per-chunk grid
    /// and interpolated by the vertex shader, so it must stay low-frequency.
    fn light_at(&self, x: f64, y: f64, z: f64) -> f32 {
        // The outside of the bedrock stays visible from the void.
        if y >= WORLD_TOP as f64 - 1.0 || y <= WORLD_BOTTOM as f64 + 1.0 {
            return 1.0;
        }
        let t = self.tier_near(y);
        let th = t.theme;
        let f = self.field(t, x, z);
        if f.open <= 0.0 {
            return th.rock_light;
        }
        let height = (f.ceil - f.floor).max(1.0);
        let t01 = ((y as f32 - f.floor) / height).clamp(0.0, 1.0);
        let vert = if th.lit_from_below {
            1.15 - 0.6 * t01
        } else {
            0.72 + 0.4 * t01
        };
        let lit = 0.46 + 0.54 * f.glow;
        let inside = th.light * lit * vert * smoothstep(0.0, 0.34, f.open);
        (th.rock_light + (1.0 - th.rock_light) * inside).clamp(0.0, 1.0)
    }

    /// Light on a 5x5x5 grid over the chunk's corners.
    pub fn light_grid(&self, key: ChunkKey) -> [u8; 125] {
        let mut out = [0u8; 125];
        let o = key.origin();
        let step = key.size() as f64 / 4.0;
        for iy in 0..5 {
            for iz in 0..5 {
                for ix in 0..5 {
                    let l = self.light_at(
                        o[0] as f64 + ix as f64 * step,
                        o[1] as f64 + iy as f64 * step,
                        o[2] as f64 + iz as f64 * step,
                    );
                    out[iy * 25 + iz * 5 + ix] = (l * 255.0) as u8;
                }
            }
        }
        out
    }

    pub fn chunk_look(&self, key: ChunkKey) -> ChunkLook {
        let lo = key.origin()[1];
        let hi = lo + key.size();
        let t = self.tier_near((lo + hi) as f64 * 0.5);
        // Daylight-coloured on the void side of the bedrock, where no layer's air reaches.
        let at_edge = (lo..=hi).contains(&WORLD_TOP) || (lo..=hi).contains(&WORLD_BOTTOM);
        ChunkLook {
            liquid_y: t.liquid_y as f32,
            tint: if at_edge {
                [255, 255, 255]
            } else {
                t.theme.tint
            },
            crystal: t.theme.crystal,
        }
    }

    /// Looks for a nice open spot in the given tier to start the player.
    pub fn find_spawn(&self, tier_index: i32) -> [f64; 3] {
        let t = self.tier(tier_index);
        let mut best = None;
        let mut best_score = f32::MIN;
        for i in 0..4000 {
            let a = i as f64 * 2.399_963;
            let r = 40.0 * (i as f64).sqrt();
            let x = (a.cos() * r).floor() + 0.5;
            let z = (a.sin() * r).floor() + 0.5;
            let f = self.field(t, x, z);
            if f.open < 0.95 {
                continue;
            }
            let height = f.ceil - f.floor;
            let above_liquid = f.floor - t.liquid_y as f32;
            if above_liquid < 6.0 {
                continue;
            }
            let score = height * 0.01 + f.forest * 1.5 + f.glow * 1.5 - (i as f32) * 0.0004;
            if score > best_score {
                best_score = score;
                best = Some([x, (f.floor + height * 0.35) as f64, z]);
            }
        }
        best.unwrap_or([0.5, t.mid + 80.0, 0.5])
    }
    /// Centre of a chasm below the given tier, if one exists nearby.
    pub fn find_chasm(&self, tier_index: i32) -> Option<[f64; 3]> {
        let up = tier_index.clamp(0, self.tiers.len() as i32 - 2) as usize;
        for cz in -6i64..=6 {
            for cx in -6i64..=6 {
                let h = hash3(self.seed ^ 0xC4A5, cx, 0, cz);
                if unit(hash3(h, up as i64, 5, 0)) > 0.5 {
                    continue;
                }
                let h2 = splitmix(h);
                let x = (cx as f64 + 0.2 + 0.6 * unit(h2) as f64) * CHASM_CELL as f64;
                let z = (cz as f64 + 0.2 + 0.6 * unit(h2 >> 24) as f64) * CHASM_CELL as f64;
                if let Some((fu, _)) = self.chasm_fields(up, x, z) {
                    return Some([x, fu.floor as f64, z]);
                }
            }
        }
        None
    }

    /// A spot just above the liquid line of a tier, for looking across its lakes.
    pub fn find_shore(&self, tier_index: i32) -> [f64; 3] {
        let t = self.tier(tier_index);
        for i in 0..8000 {
            let a = i as f64 * 2.399_963;
            let r = 30.0 * (i as f64).sqrt();
            let x = (a.cos() * r).floor() + 0.5;
            let z = (a.sin() * r).floor() + 0.5;
            let f = self.field(t, x, z);
            let above = f.floor - t.liquid_y as f32;
            if f.open > 0.9 && (0.0..10.0).contains(&above) && f.ceil - f.floor > 120.0 {
                return [x, t.liquid_y + 32.0, z];
            }
        }
        self.find_spawn(tier_index)
    }

    /// The highest spot on a mountain near the start of a tier, if it has any.
    pub fn find_mountain(&self, tier_index: i32) -> Option<[f64; 3]> {
        let t = self.tier(tier_index);
        let mut best: Option<([f64; 3], f32)> = None;
        for i in 0..6000 {
            let a = i as f64 * 2.399_963;
            let r = 30.0 * (i as f64).sqrt();
            let (x, z) = ((a.cos() * r).floor() + 0.5, (a.sin() * r).floor() + 0.5);
            let f = self.field(t, x, z);
            if f.open > 0.9 && f.mountain > 0.6 && best.is_none_or(|(_, top)| f.floor > top) {
                best = Some(([x, f.floor as f64, z], f.floor));
            }
        }
        best.map(|(p, _)| p)
    }
}

const fn n3_lattice(lod: u8) -> (usize, usize) {
    // (step in samples, lattice size per axis). Covers sample indices 0..=PADDED.
    match lod {
        0 | 1 => (4, 10),
        2 => (2, 19),
        _ => (1, 36),
    }
}

fn prepare_n3(lod: u8, buf: &mut Vec<f32>) {
    let (_, dim) = n3_lattice(lod);
    buf.clear();
    buf.resize(dim * dim * dim, f32::NAN);
}

/// Moves a height onto level shelves `step` apart, one of them at `base`, joined by ramps.
fn shelve(y: f32, base: f32, step: f32) -> f32 {
    let q = (y - base) / step;
    let shelf = q.floor();
    base + (shelf + smoothstep(0.25, 0.75, q - shelf)) * step
}

#[inline(always)]
fn bilerp(a: f32, b: f32, c: f32, d: f32, fx: f32, fz: f32) -> f32 {
    let t = a + (b - a) * fx;
    let u = c + (d - c) * fx;
    t + (u - t) * fz
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn pick_floor_block(
    surface: &Surface,
    exposed: bool,
    depth: f32,
    y: f32,
    above_floor: f32,
    liquid_y: f32,
    slope: f32,
    forest: f32,
    mountain: f32,
    rough_n: f32,
    lod: u8,
    voxel: f32,
) -> u8 {
    let shore = y < liquid_y + 2.5;
    // Mountains, pillar flanks and walls rising from the floor are bare rock.
    if slope > 1.15 || above_floor > 6.0 + voxel || mountain > 0.1 {
        let alt = if surface.alt_veins {
            rough_n.abs() < 0.022
        } else {
            rough_n > surface.alt_above
        };
        if exposed && alt {
            return surface.rock_alt;
        }
        return surface.rock;
    }
    if exposed {
        if shore {
            return surface.shore;
        }
        // Past LOD 3 single trees no longer resolve: the canopy joins the ground.
        if lod >= 4 && forest > 0.6 {
            return surface.canopy;
        }
        return surface.top;
    }
    if depth < 4.0 + voxel {
        return if shore { surface.shore } else { surface.soil };
    }
    surface.rock
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn pick_ceiling_block(
    theme: &Theme,
    seed: u64,
    depth: f32,
    x: f64,
    z: f64,
    below_ceil: f32,
    glow: f32,
    open: f32,
    voxel: f32,
) -> u8 {
    if glow > 0.18
        && depth < 2.4 + voxel * 0.9
        && open > 0.25
        && below_ceil < theme.crystal_reach + voxel
    {
        // Each cell may hold one colony. Where the field is strong, more cells do.
        let cell = (voxel as f64 * 2.5).max(17.0);
        let (cx, cz) = ((x / cell).floor(), (z / cell).floor());
        let h = hash3(seed ^ 0xC257, cx as i64, 0, cz as i64);
        if unit(h) < (glow - 0.18) * 1.1 * theme.crystal_cover {
            let h2 = splitmix(h);
            let jx = (cx + 0.25 + 0.5 * unit(h2) as f64) * cell;
            let jz = (cz + 0.25 + 0.5 * unit(h2 >> 24) as f64) * cell;
            let spread = theme.crystal_cover.max(1.0).sqrt() as f64;
            let r = cell * (0.14 + 0.24 * unit(h2 >> 40) as f64) * spread;
            let (dx, dz) = (x - jx, z - jz);
            if dx * dx + dz * dz < r * r {
                return CRYSTAL;
            }
        }
    }
    theme.surface.rock
}

#[inline]
fn ore_at(seed: u64, x: f64, y: f64, z: f64) -> u8 {
    const VEIN: f64 = 5.0;
    let h = hash3(
        seed ^ 0x0E0E,
        (x / VEIN).floor() as i64,
        (y / VEIN).floor() as i64,
        (z / VEIN).floor() as i64,
    );
    let r = unit(h);
    if r > 0.022 {
        return STONE;
    }
    let vh = hash3(
        seed ^ 0x0F0F,
        x.floor() as i64,
        y.floor() as i64,
        z.floor() as i64,
    );
    if unit(vh) > 0.45 {
        return STONE;
    }
    if r < 0.006 { IRON } else { COAL }
}

fn stamp_tree(t: &Tree, key: ChunkKey, seed: u64, out: &mut Voxels) {
    let o = key.origin();
    let s = key.voxel_size() as f32;
    // All math below is relative to the trunk, so it holds any distance from the
    // world origin. (ox, oz) is the chunk corner in that space.
    let inflate = if key.lod >= 1 { s * 0.35 } else { 0.0 };
    let (ox, oy, oz) = (
        (o[0] as f64 - t.x) as f32,
        o[1] as f32,
        (o[2] as f64 - t.z) as f32,
    );
    let (tx, tz) = (t.x, t.z);

    let to_i = |org: f32, v: f32| ((v - org) / s + 0.5).floor() as i32 + 1;
    let center = |org: f32, i: i32| org + (i as f32 - 1.0 + 0.5) * s;
    let max_i = PADDED as i32 - 1;

    // Leaves first; logs then overwrite them.
    for b in &t.blobs {
        let (rx, ry) = (b.rx + inflate, b.ry + inflate);
        let x0 = to_i(ox, b.c[0] - rx).max(0);
        let x1 = to_i(ox, b.c[0] + rx).min(max_i);
        let y0 = to_i(oy, b.c[1] - ry).max(0);
        let y1 = to_i(oy, b.c[1] + ry).min(max_i);
        let z0 = to_i(oz, b.c[2] - rx).max(0);
        let z1 = to_i(oz, b.c[2] + rx).min(max_i);
        for iy in y0..=y1 {
            let dy = (center(oy, iy) - b.c[1]) / ry;
            for iz in z0..=z1 {
                let dz = (center(oz, iz) - b.c[2]) / rx;
                for ix in x0..=x1 {
                    let dx = (center(ox, ix) - b.c[0]) / rx;
                    let d = dx * dx + dy * dy + dz * dz;
                    if d > 1.0 {
                        continue;
                    }
                    // Fray the outer shell so canopies don't read as ellipsoids. Thin discs
                    // are all shell, and would be frayed away to nothing.
                    if d > 0.72 && rx > 2.6 {
                        let h = hash3(
                            seed ^ 0x1EAF,
                            (center(ox, ix) as f64 + tx).floor() as i64,
                            center(oy, iy).floor() as i64,
                            (center(oz, iz) as f64 + tz).floor() as i64,
                        );
                        if unit(h) < (d - 0.72) * 2.6 {
                            continue;
                        }
                    }
                    let i = pidx(ix as usize, iy as usize, iz as usize);
                    if out[i] == AIR {
                        out[i] = t.leaves;
                    }
                }
            }
        }
    }

    // Trunk. A far-off conifer's narrow trunk would only poke through its own needles.
    if t.log == PINE_LOG && !t.giant && key.lod >= 2 {
        return;
    }
    let flare = if t.giant { t.trunk_r * 1.9 } else { 0.0 };
    let rmax = t.trunk_r + flare + inflate + 0.5;
    let x0 = to_i(ox, -rmax).max(0);
    let x1 = to_i(ox, rmax).min(max_i);
    let z0 = to_i(oz, -rmax).max(0);
    let z1 = to_i(oz, rmax).min(max_i);
    let y0 = to_i(oy, t.base).max(0);
    let y1 = to_i(oy, t.base + t.height).min(max_i);
    for iy in y0..=y1 {
        let cy = center(oy, iy);
        let f = ((cy - t.base) / t.height).clamp(0.0, 1.0);
        let r = if t.giant {
            t.trunk_r * (1.0 - 0.65 * f) + flare * (-(cy - t.base) / 7.0).exp() + inflate
        } else {
            t.trunk_r + inflate
        };
        let r2 = r * r;
        for iz in z0..=z1 {
            let dz = center(oz, iz);
            for ix in x0..=x1 {
                let dx = center(ox, ix);
                if dx * dx + dz * dz <= r2 {
                    let i = pidx(ix as usize, iy as usize, iz as usize);
                    let v = out[i];
                    if v == AIR || v == t.leaves {
                        out[i] = t.log;
                    }
                }
            }
        }
    }

    // Branches connect a giant's trunk to its canopy blobs.
    if t.branches && key.lod <= 2 {
        let br = (1.3f32).max(s * 0.6);
        for b in t.blobs.iter().skip(1) {
            let start = [0.0, b.c[1] - 8.0, 0.0];
            let end = b.c;
            let min = |a: f32, c: f32| a.min(c) - br;
            let max = |a: f32, c: f32| a.max(c) + br;
            let x0 = to_i(ox, min(start[0], end[0])).max(0);
            let x1 = to_i(ox, max(start[0], end[0])).min(max_i);
            let y0 = to_i(oy, min(start[1], end[1])).max(0);
            let y1 = to_i(oy, max(start[1], end[1])).min(max_i);
            let z0 = to_i(oz, min(start[2], end[2])).max(0);
            let z1 = to_i(oz, max(start[2], end[2])).min(max_i);
            let d = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
            let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            for iy in y0..=y1 {
                for iz in z0..=z1 {
                    for ix in x0..=x1 {
                        let p = [center(ox, ix), center(oy, iy), center(oz, iz)];
                        let w = [p[0] - start[0], p[1] - start[1], p[2] - start[2]];
                        let t01 =
                            ((w[0] * d[0] + w[1] * d[1] + w[2] * d[2]) / len2).clamp(0.0, 1.0);
                        let c = [w[0] - d[0] * t01, w[1] - d[1] * t01, w[2] - d[2] * t01];
                        if c[0] * c[0] + c[1] * c[1] + c[2] * c[2] <= br * br {
                            let i = pidx(ix as usize, iy as usize, iz as usize);
                            let v = out[i];
                            if v == AIR || v == t.leaves {
                                out[i] = t.log;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A tree's height, trunk radius, whether it has branches, and its canopy.
type TreeShape = (f32, f32, bool, Vec<Blob>);

/// A huge old tree: a broad crown over lobes of leaves on branches.
fn giant_broadleaf(base: f32, r: &impl Fn(u32) -> f32) -> TreeShape {
    let height = 42.0 + 50.0 * r(1);
    let n_blobs = 5 + (r(2) * 5.0) as u32;
    let mut blobs = Vec::with_capacity(n_blobs as usize + 1);
    let top = base + height + 3.0;
    blobs.push(Blob {
        c: [0.0, top - 3.0, 0.0],
        rx: 13.0 + 7.0 * r(3),
        ry: 9.0 + 4.0 * r(4),
    });
    for i in 0..n_blobs {
        let a = r(5 + i) * std::f32::consts::TAU;
        let frac = 0.5 + 0.42 * r(15 + i);
        let reach = (0.2 + 0.8 * r(25 + i)) * (10.0 + height * 0.26) * (1.1 - frac * 0.5);
        blobs.push(Blob {
            c: [a.cos() * reach, base + 3.0 + height * frac, a.sin() * reach],
            rx: 7.0 + 8.0 * r(35 + i),
            ry: 5.5 + 3.5 * r(45 + i),
        });
    }
    (height, 2.6 + 3.2 * (height - 42.0) / 50.0, true, blobs)
}

/// A rounded tree, with either one lobe of leaves or two.
fn small_broadleaf(base: f32, r: &impl Fn(u32) -> f32) -> TreeShape {
    let height = 7.0 + 8.0 * r(1);
    let top = base + height + 3.0;
    let mut blobs = Vec::with_capacity(2);
    if r(4) < 0.6 {
        let (rx, ry) = (3.4 + 2.8 * r(2), 3.0 + 2.4 * r(3));
        blobs.push(Blob {
            c: [0.0, top - ry * 0.3, 0.0],
            rx,
            ry,
        });
    } else {
        let a = r(5) * std::f32::consts::TAU;
        let (rx, ry) = (3.0 + 2.2 * r(2), 2.4 + 1.4 * r(3));
        blobs.push(Blob {
            c: [0.0, top - 1.0, 0.0],
            rx,
            ry,
        });
        blobs.push(Blob {
            c: [a.cos() * rx * 0.9, top - ry - 1.0, a.sin() * rx * 0.9],
            rx: rx * 0.8,
            ry: ry * 0.8,
        });
    }
    (height, 0.5, false, blobs)
}

/// Stacked discs of needles narrowing to a point, on a trunk bare at its foot.
fn conifer(base: f32, height: f32, radius: f32, r: &impl Fn(u32) -> f32) -> TreeShape {
    let top = base + height + 3.0;
    let first = base + 3.0 + height * (0.16 + 0.12 * r(3));
    let spacing = 1.8 + radius * 0.11;
    let discs = (((top - first) / spacing) as u32).clamp(3, 18);
    let mut blobs = Vec::with_capacity(discs as usize);
    for i in 0..discs {
        let f = i as f32 / (discs - 1) as f32;
        // Thin discs that keep narrowing and flaring again read as tiers of boughs, each
        // overhanging the one below. Solid stacks look like columns from up close.
        let bough = 1.0 - 0.42 * (i % 3) as f32 / 2.0;
        let wobble = 0.9 + 0.2 * r(10 + i);
        // Stays full high up the tree, or the top third is just a pole. Only the last discs
        // narrow to a single block.
        let min_r = if i + 2 >= discs { 1.0 } else { 1.5 };
        blobs.push(Blob {
            c: [0.0, first + (top - first) * f, 0.0],
            rx: (radius * (1.0 - f.powf(1.4)) * bough * wobble).max(min_r),
            ry: spacing * 0.62,
        });
    }
    let trunk_r = if radius > 8.0 { radius * 0.16 } else { 0.5 };
    (height, trunk_r, false, blobs)
}

/// True when a whole padded chunk holds a single block.
pub fn is_uniform(v: &Voxels) -> Option<u8> {
    let first = v[0];
    if v.iter().all(|&b| b == first) {
        Some(first)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::CHUNK_SIZE;

    fn chunk(g: &Generator, x: i64, y: i64, z: i64, lod: u8) -> (Box<Voxels>, ChunkKey) {
        let size = (CHUNK_SIZE as i64) << lod;
        let key = ChunkKey::new(
            x.div_euclid(size) as i32,
            y.div_euclid(size) as i32,
            z.div_euclid(size) as i32,
            lod,
        );
        let mut out: Box<Voxels> = vec![0u8; crate::world::PADDED_VOLUME]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        g.generate(key, &mut Scratch::default(), &mut out);
        (out, key)
    }

    #[test]
    fn three_layers_with_the_player_starting_in_the_top_one() {
        for seed in [1, 777, 123_456] {
            let g = Generator::new(seed);
            let names: Vec<_> = g.tiers().iter().map(|t| t.theme.name).collect();
            assert_eq!(names, ["Overworld", "Dark Forest", "Underworld"]);
            let spawn = g.find_spawn(0);
            assert_eq!(g.tier_near(spawn[1]).index, 0, "seed {seed}");
            assert!(spawn[1] > g.tiers()[0].liquid_y, "seed {seed}");
        }
    }

    #[test]
    fn every_layer_stays_inside_the_bedrock() {
        let bands = |t: &Tier| (t.mid + BAND_BELOW, t.mid + BAND_ABOVE);
        let g = Generator::new(1);
        let reach = BEDROCK_SOLID + BEDROCK_ROUGH;
        assert!(bands(&g.tiers()[0]).1 < WORLD_TOP as f64 - reach);
        assert!(bands(&g.tiers()[2]).0 > WORLD_BOTTOM as f64 + reach);
        for pair in g.tiers().windows(2) {
            assert!(
                bands(&pair[1]).1 < bands(&pair[0]).0,
                "bands must not overlap"
            );
        }
    }

    #[test]
    fn the_world_ends_in_bedrock_then_void_at_every_detail() {
        let g = Generator::new(5);
        for lod in 0..=crate::world::MAX_LOD {
            let (void_up, _) = chunk(&g, 100, WORLD_TOP + 2000, 100, lod);
            assert!(void_up.iter().all(|&b| b == AIR), "void above, lod {lod}");
            let (void_down, _) = chunk(&g, 100, WORLD_BOTTOM - 2000, 100, lod);
            assert!(void_down.iter().all(|&b| b == AIR), "void below, lod {lod}");

            for (edge, inward) in [(WORLD_TOP, -1i64), (WORLD_BOTTOM, 1)] {
                let s = 1i64 << lod;
                // The outermost voxel inside the world, and the first one beyond.
                let inner = if inward < 0 { edge - s } else { edge };
                let outer = if inward < 0 { edge } else { edge - s };
                let at = |y: i64| {
                    let (v, key) = chunk(&g, 100, y, 100, lod);
                    let o = key.origin();
                    let i = |w: i64, org: i64| ((w - org).div_euclid(s) + 1) as usize;
                    v[pidx(i(100, o[0]), i(y, o[1]), i(100, o[2]))]
                };
                assert_eq!(at(inner), BEDROCK, "inside {edge}, lod {lod}");
                assert_eq!(at(outer), AIR, "beyond {edge}, lod {lod}");
            }
        }
    }

    /// Counts blocks in a wide patch of a tier, sampled at a coarse level of detail.
    fn census(g: &Generator, tier: usize) -> std::collections::HashMap<u8, u64> {
        let mut counts = std::collections::HashMap::new();
        let mid = g.tiers()[tier].mid as i64;
        let mut sc = Scratch::default();
        let mut out: Box<Voxels> = vec![0u8; crate::world::PADDED_VOLUME]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        for cz in -8..8 {
            for cx in -8..8 {
                for y in -3..4 {
                    let key = ChunkKey::new(cx, (mid / 128) as i32 + y, cz, 2);
                    g.generate(key, &mut sc, &mut out);
                    for &b in out.iter() {
                        *counts.entry(b).or_insert(0) += 1;
                    }
                }
            }
        }
        counts
    }

    #[test]
    fn each_layer_has_its_own_materials() {
        let g = Generator::new(777);
        let has =
            |c: &std::collections::HashMap<u8, u64>, b: u8| c.get(&b).copied().unwrap_or(0) > 0;

        let over = census(&g, 0);
        for b in [GRASS, LOG, LEAVES, WATER, CRYSTAL] {
            assert!(has(&over, b), "overworld lacks {b}");
        }
        assert!(!has(&over, LAVA) && !has(&over, PINE_LOG));

        let dark = census(&g, 1);
        for b in [DARK_GRASS, PINE_LOG, PINE_NEEDLES] {
            assert!(has(&dark, b), "dark forest lacks {b}");
        }
        assert!(!has(&dark, GRASS) && !has(&dark, LOG) && !has(&dark, LAVA));

        let under = census(&g, 2);
        for b in [BASALT, LAVA] {
            assert!(has(&under, b), "underworld lacks {b}");
        }
        for b in [
            LOG,
            PINE_LOG,
            LEAVES,
            PINE_NEEDLES,
            GRASS,
            DARK_GRASS,
            WATER,
        ] {
            assert!(!has(&under, b), "underworld shouldn't have {b}");
        }
    }

    #[test]
    fn no_trees_float_over_a_chasm() {
        // Several seeds, since not every start has a chasm nearby.
        let mut checked = 0;
        for seed in 1..40 {
            let g = Generator::new(seed);
            let Some(c) = g.find_chasm(0) else {
                continue;
            };
            let t = &g.tiers()[0];
            let trees = t.theme.trees.as_ref().unwrap();
            let cell = trees.small_cell as f64;
            let (cx, cz) = ((c[0] / cell).floor() as i64, (c[2] / cell).floor() as i64);
            for j in cz - 8..=cz + 8 {
                for i in cx - 8..=cx + 8 {
                    if let Some(tree) = g.tree_in_cell(t, trees, i, j, false) {
                        // Even the narrowest chasm keeps trees this far from its centre.
                        let d = ((tree.x - c[0]).powi(2) + (tree.z - c[2]).powi(2)).sqrt();
                        assert!(d >= 84.0, "seed {seed}: tree {d} from the chasm at {c:?}");
                    }
                }
            }
            checked += 1;
        }
        assert!(checked > 5, "only {checked} seeds had a chasm to check");
    }

    #[test]
    fn the_upper_layers_have_level_ground() {
        const PATCHES: u32 = 150;
        for seed in [777, 42] {
            let g = Generator::new(seed);
            for t in &g.tiers()[..2] {
                let (mut patches, mut level) = (0, 0);
                'search: for pz in -40i64..40 {
                    for px in -40i64..40 {
                        let (x, z) = (px * 128, pz * 128);
                        let f = g.field(t, x as f64 + 16.0, z as f64 + 16.0);
                        let dry = f.floor as f64 > t.liquid_y + 6.0;
                        if f.open < 0.95 || f.mountain > 0.02 || !dry {
                            continue;
                        }
                        // The ground's height in each column of a chunk-sized patch, ignoring trees.
                        let mut heights = std::collections::HashMap::new();
                        let cy = (f.floor as i64).div_euclid(32) * 32;
                        let mut top = [[i64::MIN; 32]; 32];
                        for k in -2..=1 {
                            let (c, key) = chunk(&g, x, cy + k * 32, z, 0);
                            for (iz, row) in top.iter_mut().enumerate() {
                                for (ix, h) in row.iter_mut().enumerate() {
                                    for iy in 0..32 {
                                        let b = c[pidx(ix + 1, iy + 1, iz + 1)];
                                        let plant = [LOG, LEAVES, PINE_LOG, PINE_NEEDLES];
                                        if b != AIR && b != WATER && !plant.contains(&b) {
                                            *h = (*h).max(key.origin()[1] + iy as i64);
                                        }
                                    }
                                }
                            }
                        }
                        for h in top.iter().flatten() {
                            *heights.entry(*h).or_insert(0u32) += 1;
                        }
                        patches += 1;
                        if *heights.values().max().unwrap() as f32 >= 0.8 * 1024.0 {
                            level += 1;
                        }
                        if patches == PATCHES {
                            break 'search;
                        }
                    }
                }
                assert_eq!(patches, PATCHES, "{} is short of dry land", t.theme.name);
                assert!(
                    level * 10 >= patches,
                    "{}: only {level} of {patches} patches are level",
                    t.theme.name
                );
            }
        }
    }

    #[test]
    fn plains_stop_short_of_the_mountains() {
        let g = Generator::new(777);
        let t = &g.tiers()[1];
        let mut seen = 0;
        for i in 0..4000 {
            let (x, z) = ((i % 64 * 90) as f64, (i / 64 * 90) as f64);
            let f = g.field(t, x, z);
            if f.mountain >= 0.3 {
                seen += 1;
                assert_eq!(f.rough, 1.0, "smoothed mountain at {x}, {z}");
            }
        }
        assert!(seen > 50, "only {seen} samples landed on mountains");
    }

    #[test]
    fn trees_stay_off_the_mountains() {
        let g = Generator::new(777);
        let t = &g.tiers()[1];
        let trees = t.theme.trees.as_ref().unwrap();
        let (mut grown, mut on_peaks) = (0, 0);
        for cz in -150..150 {
            for cx in -150..150 {
                if let Some(tree) = g.tree_in_cell(t, trees, cx, cz, false) {
                    grown += 1;
                    if g.field(t, tree.x, tree.z).mountain > 0.1 {
                        on_peaks += 1;
                    }
                }
            }
        }
        assert!(grown > 1000, "should be a dense forest, got {grown}");
        assert_eq!(on_peaks, 0);
    }
}
