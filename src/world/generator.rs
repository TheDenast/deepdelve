//! Procedural world: stacked cavern tiers carved out of infinite rock.
//!
//! The world is a vertical stack of *tiers*. Each tier defines, per column, a
//! floor height and a ceiling height; caverns exist where the ceiling lies
//! above the floor. 2D fields give broad, walkable floors and domed roofs,
//! then 3D noise adds overhangs and wall roughness. See docs/DESIGN.md.
//!
//! Every function here is a pure function of world position, so any chunk at
//! any LOD can be produced independently, in any order, on any thread. LOD
//! chunks simply sample the same functions at a coarser spacing.

use super::block::*;
use super::noise::{Perlin, hash3, smoothstep, splitmix, unit};
use super::{ChunkKey, PADDED, Voxels, pidx};

/// Vertical distance between tiers, before jitter.
pub const TIER_SPACING: f64 = 1400.0;
const TIER_JITTER: f64 = 220.0;
/// Tier indices run over `-MAX_TIER..=MAX_TIER`, about +/-32,000 blocks.
pub const MAX_TIER: i32 = 22;
/// Any tier's geometry stays within `mid + BAND_BELOW ..= mid + BAND_ABOVE`.
const BAND_BELOW: f64 = -330.0;
const BAND_ABOVE: f64 = 640.0;
/// Largest 3D displacement of the surfaces.
const MAX_NOISE_AMP: f32 = 34.0;
const WALL_NOISE_AMP: f32 = 34.0;
const CEIL_NOISE_AMP: f32 = 22.0;
const FLOOR_NOISE_AMP: f32 = 6.0;

const SMALL_TREE_CELL: i64 = 11;
const GIANT_TREE_CELL: i64 = 120;
const MAX_TREE_HEIGHT: f64 = 125.0;
/// Farthest a giant's canopy can extend from its trunk, horizontally.
const MAX_TREE_REACH: f64 = 58.0;

const CHASM_CELL: i64 = 1700;
const PILLAR_CELL: i64 = 430;

#[derive(Clone, Debug)]
pub struct Tier {
    pub index: i32,
    pub mid: f64,
    /// Everything open below this height is water. Always a whole number.
    pub water_y: f64,
    pub hmax: f32,
    pub open_bias: f32,
    pub tint: [u8; 3],
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
}

impl Field {
    fn closed(mid: f32) -> Self {
        Field {
            floor: mid,
            ceil: mid,
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
    /// Water plane of the tier touching this chunk.
    pub water_y: f32,
    pub tint: [u8; 3],
}

impl Generator {
    pub fn new(seed: u64) -> Self {
        let mut tiers = Vec::new();
        for index in -MAX_TIER..=MAX_TIER {
            let salt = splitmix(seed ^ (index as i64 as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93));
            let r = |n: u64| unit(splitmix(salt.wrapping_add(n)));
            let mid = index as f64 * TIER_SPACING + (r(1) as f64 - 0.5) * 2.0 * TIER_JITTER;
            let hmax = 260.0 + r(2) * 140.0;
            let open_bias = (r(3) - 0.5) * 0.12;
            // About one tier in three is a "sea tier": the water plane sits high
            // enough to drown the low regions. Others only keep scattered lakes.
            let sea = r(4) < 0.33;
            let water_rel = if sea {
                -18.0 + r(5) * 30.0
            } else {
                -125.0 + r(5) * 40.0
            };
            let tint = palette(r(6), index);
            let off = [(r(7) as f64) * 50_000.0, (r(8) as f64) * 50_000.0];
            tiers.push(Tier {
                index,
                mid,
                water_y: (mid + water_rel as f64).floor(),
                hmax,
                open_bias,
                tint,
                off,
                salt,
            });
        }
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
        let i = (y / TIER_SPACING).round() as i32;
        let i = i.clamp(-MAX_TIER, MAX_TIER);
        // The jitter means the neighbouring index can be the better match.
        let mut best = &self.tiers[(i + MAX_TIER) as usize];
        let mut best_d = f64::MAX;
        for j in (i - 1).max(-MAX_TIER)..=(i + 1).min(MAX_TIER) {
            let t = &self.tiers[(j + MAX_TIER) as usize];
            let d = (y - (t.mid + (BAND_ABOVE + BAND_BELOW) * 0.5)).abs();
            if d < best_d {
                best_d = d;
                best = t;
            }
        }
        best
    }

    fn cavern_mask(&self, t: &Tier, x: f64, z: f64) -> f32 {
        let n = &self.noise;
        let (xo, zo) = (x + t.off[0], z + t.off[1]);
        let wx = xo + n.n2(xo / 1400.0, zo / 1400.0) as f64 * 420.0;
        let wz = zo + n.n2(xo / 1400.0 + 91.3, zo / 1400.0 - 17.7) as f64 * 420.0;
        let m = n.fbm2(wx / 2700.0, wz / 2700.0, 3) + t.open_bias;
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
        let h = open.powf(0.62) * t.hmax * var;
        let hills = n.fbm2(xo / 300.0, zo / 300.0, 4) * 58.0 * open;
        let roof = n.fbm2(xo / 240.0 - 40.0, zo / 240.0 + 11.0, 3) * 46.0 * open;
        let glow_n = n.fbm2(xo / 520.0 + 300.0, zo / 520.0 - 200.0, 2);
        let forest_n = n.fbm2(xo / 640.0 - 123.0, zo / 640.0 + 321.0, 2);
        Field {
            floor: mid - 0.2 * h + hills,
            ceil: mid + 0.8 * h + roof,
            open,
            glow: smoothstep(-0.12, 0.32, glow_n),
            forest: smoothstep(-0.10, 0.30, forest_n),
        }
    }

    /// Fills `out` with the chunk's blocks, including its one-voxel border.
    pub fn generate(&self, key: ChunkKey, sc: &mut Scratch, out: &mut Voxels) {
        out.fill(STONE);
        let s = key.voxel_size() as f64;
        let o = key.origin();
        let y_min = o[1] as f64 - s;
        let y_max = o[1] as f64 + 33.0 * s;

        let first = ((y_min - BAND_ABOVE - TIER_JITTER) / TIER_SPACING).floor() as i32;
        let last = ((y_max - BAND_BELOW + TIER_JITTER) / TIER_SPACING).ceil() as i32;
        for i in first.max(-MAX_TIER)..=last.min(MAX_TIER) {
            let t = &self.tiers[(i + MAX_TIER) as usize];
            if y_max > t.mid + BAND_BELOW && y_min < t.mid + BAND_ABOVE {
                self.carve_tier(t, key, sc, out);
            }
        }
        self.carve_chasms(key, out);
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

        let water_y = t.water_y as f32;
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
                // Slope of the floor, for deciding what can grow here.
                let dfx = ((b.floor - a.floor) * (1.0 - fz) + (d.floor - c.floor) * fz) / gw as f32;
                let dfz = ((c.floor - a.floor) * (1.0 - fx) + (d.floor - b.floor) * fx) / gw as f32;
                let slope = (dfx * dfx + dfz * dfz).sqrt();

                if detail {
                    floor += self.noise.n2(x / 19.0, z / 19.0) * 3.4 * open;
                    if lod == 0 {
                        floor += self.noise.n2(x / 7.0 + 50.0, z / 7.0) * 1.3 * open;
                    }
                    ceil -= self.spikes(t, x, z, glow) * open;
                    floor += self.stalagmite(t, x, z, forest) * open;
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
                let floor_amp = WALL_NOISE_AMP + (FLOOR_NOISE_AMP - WALL_NOISE_AMP) * open;
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
                let cobble_n = if lod <= 1 {
                    self.noise.n2(x / 34.0 + 11.0, z / 34.0 - 77.0)
                } else {
                    0.0
                };
                for iy in yl..yh.min(PADDED) {
                    let d = column[iy];
                    let y = sample(o[1], iy) as f32;
                    let idx = pidx(ix, iy, iz);
                    if d > 0.0 {
                        out[idx] = if y < water_y { WATER } else { AIR };
                        continue;
                    }
                    let exposed = iy < yh && column[iy + 1] > 0.0;
                    let floor_side = (y - floor) < (ceil - y);
                    let depth = -d;
                    out[idx] = if floor_side {
                        pick_floor_block(
                            exposed,
                            depth,
                            y,
                            y - floor,
                            water_y,
                            slope,
                            forest,
                            cobble_n,
                            lod,
                            s as f32,
                        )
                    } else {
                        pick_ceiling_block(self.seed, depth, x, y as f64, z, glow, open, s as f32)
                    };
                    if out[idx] == STONE && lod == 0 && depth < 14.0 {
                        out[idx] = ore_at(self.seed, x, y as f64, z);
                    }
                }
            }
        }
        sc.column = column;

        // Trees sit on the floor; skip the work when the chunk is nowhere near it.
        if y_hi_chunk > fmin as f64 - 8.0 && y_lo_chunk < fmax as f64 + MAX_TREE_HEIGHT {
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
        let density = 0.25 + 0.6 * glow;
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
        if unit(h) > 0.22 * (1.0 - forest * 0.8) {
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
                if unit(h) > 0.34 {
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
        let o = key.origin();
        let size = key.size() as f64;
        let voxel = key.voxel_size() as f64;
        let scan = |cell: i64, reach: f64, giant: bool, out: &mut Vec<Tree>| {
            let cx0 = ((o[0] as f64 - reach) / cell as f64).floor() as i64;
            let cx1 = ((o[0] as f64 + size + reach) / cell as f64).floor() as i64;
            let cz0 = ((o[2] as f64 - reach) / cell as f64).floor() as i64;
            let cz1 = ((o[2] as f64 + size + reach) / cell as f64).floor() as i64;
            for cz in cz0..=cz1 {
                for cx in cx0..=cx1 {
                    if let Some(tree) = self.tree_in_cell(t, cx, cz, giant) {
                        out.push(tree);
                    }
                }
            }
        };
        scan(GIANT_TREE_CELL, MAX_TREE_REACH + voxel, true, out);
        // Small trees would vanish between samples beyond LOD 3.
        if key.lod <= 3 {
            scan(SMALL_TREE_CELL, 11.0 + voxel, false, out);
        }
    }

    fn tree_in_cell(&self, t: &Tier, cx: i64, cz: i64, giant: bool) -> Option<Tree> {
        let (cell, chance) = if giant {
            (GIANT_TREE_CELL as f64, 0.55)
        } else {
            (SMALL_TREE_CELL as f64, 0.72)
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
        let height_room = f.ceil - f.floor;
        if f.floor < t.water_y as f32 + 3.0 {
            return None;
        }
        let f2 = self.field(t, x + 6.0, z);
        let f3 = self.field(t, x, z + 6.0);
        let slope = ((f2.floor - f.floor).powi(2) + (f3.floor - f.floor).powi(2)).sqrt() / 6.0;
        if slope > 0.9 {
            return None;
        }

        let base = f.floor - 3.0;
        let r = |n: u32| unit(h2.rotate_left(n * 7) ^ (n as u64).wrapping_mul(0x1F3B));
        if giant {
            let height = 42.0 + 50.0 * r(1);
            if height_room < height + 25.0 {
                return None;
            }
            let trunk_r = 2.6 + 3.2 * (height - 42.0) / 50.0;
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
            Some(Tree {
                x,
                z,
                base,
                height: height + 3.0,
                trunk_r,
                giant,
                blobs,
            })
        } else {
            let height = 7.0 + 8.0 * r(1);
            if height_room < height + 12.0 {
                return None;
            }
            let top = base + height + 3.0;
            let (cxf, czf) = (0.0f32, 0.0f32);
            let kind = r(4);
            let mut blobs = Vec::with_capacity(3);
            if kind < 0.45 {
                // Round broadleaf.
                let rx = 3.2 + 2.8 * r(2);
                let ry = 2.8 + 2.4 * r(3);
                blobs.push(Blob {
                    c: [cxf, top - ry * 0.3, czf],
                    rx,
                    ry,
                });
            } else if kind < 0.78 {
                // Two lobes, offset to one side.
                let a = r(5) * std::f32::consts::TAU;
                let rx = 3.0 + 2.2 * r(2);
                let ry = 2.4 + 1.4 * r(3);
                blobs.push(Blob {
                    c: [cxf, top - 1.0, czf],
                    rx,
                    ry,
                });
                blobs.push(Blob {
                    c: [
                        cxf + a.cos() * rx * 0.9,
                        top - ry - 1.0,
                        czf + a.sin() * rx * 0.9,
                    ],
                    rx: rx * 0.8,
                    ry: ry * 0.8,
                });
            } else {
                // Narrow conifer: tiers shrinking towards the top.
                let rx = 2.6 + 1.6 * r(2);
                for i in 0..3 {
                    let f = i as f32 / 3.0;
                    blobs.push(Blob {
                        c: [cxf, top - height * 0.42 * (1.0 - f) + 0.5, czf],
                        rx: rx * (1.0 - f * 0.55),
                        ry: 1.8 + 0.6 * (1.0 - f),
                    });
                }
            }
            Some(Tree {
                x,
                z,
                base,
                height: height + 3.0,
                trunk_r: 0.5,
                giant,
                blobs,
            })
        }
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
                    let lower = &self.tiers[up];
                    let upper = &self.tiers[up + 1];
                    if y_hi < lower.mid || y_lo > upper.mid + BAND_ABOVE {
                        continue;
                    }
                    if unit(hash3(h, up as i64, 5, 0)) > 0.5 {
                        continue;
                    }
                    let fu = self.field(upper, x, z);
                    let fl = self.field(lower, x, z);
                    if fu.open < 0.7 || fl.open < 0.7 || fu.floor < upper.water_y as f32 + 10.0 {
                        continue;
                    }
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
                                    if out[i] != WATER {
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

    /// Light at a world position, 0..1. Evaluated on a coarse per-chunk grid
    /// and interpolated by the vertex shader, so it must stay low-frequency.
    fn light_at(&self, x: f64, y: f64, z: f64) -> f32 {
        let t = self.tier_near(y);
        let f = self.field(t, x, z);
        if f.open <= 0.0 {
            return 0.24;
        }
        let height = (f.ceil - f.floor).max(1.0);
        let t01 = ((y as f32 - f.floor) / height).clamp(0.0, 1.0);
        let vert = 0.72 + 0.4 * t01;
        let lit = 0.46 + 0.54 * f.glow;
        (0.24 + 0.76 * lit * vert * smoothstep(0.0, 0.34, f.open)).clamp(0.0, 1.0)
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
        let cy = key.origin()[1] as f64 + key.size() as f64 * 0.5;
        let t = self.tier_near(cy);
        ChunkLook {
            water_y: t.water_y as f32,
            tint: t.tint,
        }
    }

    /// Looks for a nice open spot in the given tier to start the player.
    pub fn find_spawn(&self, tier_index: i32) -> [f64; 3] {
        let t = &self.tiers[(tier_index.clamp(-MAX_TIER, MAX_TIER) + MAX_TIER) as usize];
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
            let above_water = f.floor - t.water_y as f32;
            if above_water < 6.0 {
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
        let up = (tier_index.clamp(-MAX_TIER + 1, MAX_TIER) + MAX_TIER - 1) as usize;
        let (lower, upper) = (&self.tiers[up], &self.tiers[up + 1]);
        for cz in -6i64..=6 {
            for cx in -6i64..=6 {
                let h = hash3(self.seed ^ 0xC4A5, cx, 0, cz);
                if unit(hash3(h, up as i64, 5, 0)) > 0.5 {
                    continue;
                }
                let h2 = splitmix(h);
                let x = (cx as f64 + 0.2 + 0.6 * unit(h2) as f64) * CHASM_CELL as f64;
                let z = (cz as f64 + 0.2 + 0.6 * unit(h2 >> 24) as f64) * CHASM_CELL as f64;
                let (fu, fl) = (self.field(upper, x, z), self.field(lower, x, z));
                if fu.open >= 0.7 && fl.open >= 0.7 && fu.floor >= upper.water_y as f32 + 10.0 {
                    return Some([x, fu.floor as f64, z]);
                }
            }
        }
        None
    }

    /// A spot just above the water line of a tier, for looking across its lakes.
    pub fn find_shore(&self, tier_index: i32) -> [f64; 3] {
        let t = &self.tiers[(tier_index.clamp(-MAX_TIER, MAX_TIER) + MAX_TIER) as usize];
        for i in 0..8000 {
            let a = i as f64 * 2.399_963;
            let r = 30.0 * (i as f64).sqrt();
            let x = (a.cos() * r).floor() + 0.5;
            let z = (a.sin() * r).floor() + 0.5;
            let f = self.field(t, x, z);
            let above = f.floor - t.water_y as f32;
            if f.open > 0.9 && (0.0..10.0).contains(&above) && f.ceil - f.floor > 120.0 {
                return [x, t.water_y + 32.0, z];
            }
        }
        self.find_spawn(tier_index)
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

#[inline(always)]
fn bilerp(a: f32, b: f32, c: f32, d: f32, fx: f32, fz: f32) -> f32 {
    let t = a + (b - a) * fx;
    let u = c + (d - c) * fx;
    t + (u - t) * fz
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn pick_floor_block(
    exposed: bool,
    depth: f32,
    y: f32,
    above_floor: f32,
    water_y: f32,
    slope: f32,
    forest: f32,
    cobble_n: f32,
    lod: u8,
    voxel: f32,
) -> u8 {
    let shore = y < water_y + 2.5;
    // Pillar flanks and walls rising from the floor are bare rock.
    if slope > 1.15 || above_floor > 6.0 + voxel {
        if exposed && cobble_n > 0.32 {
            return COBBLE;
        }
        return STONE;
    }
    if exposed {
        if shore {
            return SAND;
        }
        // Past LOD 3 single trees no longer resolve: the canopy joins the ground.
        if lod >= 4 && forest > 0.6 {
            return LEAVES;
        }
        return GRASS;
    }
    if depth < 4.0 + voxel {
        return if shore { SAND } else { DIRT };
    }
    STONE
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn pick_ceiling_block(
    seed: u64,
    depth: f32,
    x: f64,
    y: f64,
    z: f64,
    glow: f32,
    open: f32,
    voxel: f32,
) -> u8 {
    let _ = y;
    if glow > 0.18 && depth < 2.4 + voxel * 0.9 && open > 0.25 {
        // Each cell may hold one colony. Where the field is strong, more cells do.
        let cell = (voxel as f64 * 2.5).max(17.0);
        let (cx, cz) = ((x / cell).floor(), (z / cell).floor());
        let h = hash3(seed ^ 0xC257, cx as i64, 0, cz as i64);
        if unit(h) < (glow - 0.18) * 1.1 {
            let h2 = splitmix(h);
            let jx = (cx + 0.25 + 0.5 * unit(h2) as f64) * cell;
            let jz = (cz + 0.25 + 0.5 * unit(h2 >> 24) as f64) * cell;
            let r = cell * (0.14 + 0.24 * unit(h2 >> 40) as f64);
            let (dx, dz) = (x - jx, z - jz);
            if dx * dx + dz * dz < r * r {
                return CRYSTAL;
            }
        }
    }
    STONE
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
                    // Fray the outer shell so canopies don't read as ellipsoids.
                    if d > 0.72 {
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
                        out[i] = LEAVES;
                    }
                }
            }
        }
    }

    // Trunk.
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
                    if v == AIR || v == LEAVES {
                        out[i] = LOG;
                    }
                }
            }
        }
    }

    // Branches connect a giant's trunk to its canopy blobs.
    if t.giant && key.lod <= 2 {
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
                            if v == AIR || v == LEAVES {
                                out[i] = LOG;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Tier colours: warm and cool ambient moods, shifting with depth.
fn palette(r: f32, index: i32) -> [u8; 3] {
    const MOODS: [[f32; 3]; 6] = [
        [1.00, 0.90, 0.72],
        [0.72, 0.90, 1.00],
        [0.70, 1.00, 0.84],
        [0.98, 0.78, 0.92],
        [0.86, 0.92, 1.00],
        [1.00, 0.84, 0.60],
    ];
    let m = MOODS[((r * 6.0) as usize).min(5)];
    // Deeper tiers lean warmer.
    let heat = (-(index as f32) / MAX_TIER as f32).clamp(-1.0, 1.0) * 0.12;
    [
        ((m[0] + heat).clamp(0.0, 1.0) * 255.0) as u8,
        (m[1].clamp(0.0, 1.0) * 255.0) as u8,
        ((m[2] - heat).clamp(0.0, 1.0) * 255.0) as u8,
    ]
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
