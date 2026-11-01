//! Greedy mesher with per-corner ambient occlusion.
//!
//! Output quads are 8 bytes and pulled by the vertex shader from one big
//! storage buffer. They are grouped by face direction so the renderer can skip
//! whole directions that point away from the camera.
//!
//! The voxels are first turned into bit columns running along x. Exposed faces
//! then come from a few bitwise operations per column, so the cost follows the
//! number of visible faces rather than the volume of the chunk.

use crate::world::block::*;
use crate::world::{PADDED, Voxels, pidx};
use bytemuck::{Pod, Zeroable};

pub const CS: usize = 32;
/// Range index holding liquid quads. 0..=5 are opaque quads per direction.
pub const WATER_RANGE: usize = 6;

/// Bits covering a chunk's own voxels in a padded column (index 0 is the border).
const INNER: u64 = ((1u64 << CS) - 1) << 1;

/// Bits of `a`: x:5 y:5 z:5 w-1:5 h-1:5 dir:3.
/// Bits of `b`: layer:8 ao:8 emissive:1.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct Quad {
    pub a: u32,
    pub b: u32,
}

impl Quad {
    #[inline]
    pub fn new(
        p: [usize; 3],
        w: usize,
        h: usize,
        dir: u8,
        layer: u8,
        ao: u8,
        emissive: bool,
    ) -> Self {
        Quad {
            a: (p[0] as u32)
                | (p[1] as u32) << 5
                | (p[2] as u32) << 10
                | ((w - 1) as u32) << 15
                | ((h - 1) as u32) << 20
                | (dir as u32) << 25,
            b: layer as u32 | (ao as u32) << 8 | (emissive as u32) << 16,
        }
    }
}

#[derive(Default, Clone)]
pub struct Mesh {
    pub quads: Vec<Quad>,
    /// (start, count) into `quads` per direction, then water.
    pub ranges: [(u32, u32); 7],
    /// Tight bounds of the geometry in voxel units, for culling.
    pub bounds_min: [u8; 3],
    pub bounds_max: [u8; 3],
}

impl Mesh {
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
}

/// Normal axis, tangent U and tangent V for each direction. Tangents are chosen
/// so that U x V equals the face normal, which keeps winding consistent. The
/// shader has the same table.
const AXES: [(usize, usize, usize, bool); 6] = [
    (0, 1, 2, true),  // +x: y, z
    (0, 2, 1, false), // -x: z, y
    (1, 2, 0, true),  // +y: z, x
    (1, 0, 2, false), // -y: x, z
    (2, 0, 1, true),  // +z: x, y
    (2, 1, 0, false), // -z: y, x
];

/// Reusable working memory. `planes` is all zeroes between uses, because the
/// greedy pass clears every cell it consumes.
pub struct MeshScratch {
    solid: Box<[u64; PADDED * PADDED]>,
    water: Box<[u64; PADDED * PADDED]>,
    planes: Vec<u16>,
    /// One bit per row with anything in it, for each plane.
    rows: [u32; CS],
}

impl Default for MeshScratch {
    fn default() -> Self {
        Self {
            solid: Box::new([0; PADDED * PADDED]),
            water: Box::new([0; PADDED * PADDED]),
            planes: vec![0; CS * CS * CS],
            rows: [0; CS],
        }
    }
}

#[inline(always)]
fn bit(cols: &[u64; PADDED * PADDED], p: [usize; 3]) -> bool {
    (cols[p[2] * PADDED + p[1]] >> p[0]) & 1 != 0
}

/// Builds a mesh from a padded voxel array.
pub fn build_mesh(v: &Voxels, lod: u8, sc: &mut MeshScratch) -> Mesh {
    let mut any_water = false;
    let mut any_solid = false;
    let mut i = 0;
    for y in 0..PADDED {
        for z in 0..PADDED {
            let mut s = 0u64;
            let mut w = 0u64;
            for x in 0..PADDED {
                let b = v[i];
                i += 1;
                s |= (is_opaque(b) as u64) << x;
                w |= (is_liquid(b) as u64) << x;
            }
            sc.solid[z * PADDED + y] = s;
            sc.water[z * PADDED + y] = w;
            any_solid |= s != 0;
            any_water |= w != 0;
        }
    }

    let mut mesh = Mesh::default();
    let skirts = lod > 0;
    let mut bmin = [u8::MAX; 3];
    let mut bmax = [0u8; 3];

    for dir in 0..6u8 {
        let start = mesh.quads.len() as u32;
        if any_solid {
            mesh_dir(
                v,
                dir,
                skirts,
                false,
                sc,
                &mut mesh.quads,
                &mut bmin,
                &mut bmax,
            );
        }
        mesh.ranges[dir as usize] = (start, mesh.quads.len() as u32 - start);
    }
    let start = mesh.quads.len() as u32;
    if any_water {
        for dir in 0..6u8 {
            mesh_dir(
                v,
                dir,
                false,
                true,
                sc,
                &mut mesh.quads,
                &mut bmin,
                &mut bmax,
            );
        }
    }
    mesh.ranges[WATER_RANGE] = (start, mesh.quads.len() as u32 - start);

    if !mesh.quads.is_empty() {
        mesh.bounds_min = bmin;
        mesh.bounds_max = bmax;
    }
    mesh
}

/// A corner's AO level from its two edge neighbours and diagonal: 3 is open.
#[inline(always)]
fn vertex_ao(a: bool, b: bool, c: bool) -> u16 {
    if a && b {
        0
    } else {
        3 - (a as u16 + b as u16 + c as u16)
    }
}

/// Columns neighbouring (y, z) in direction `dir`, for faces normal to y or z.
#[inline(always)]
fn neighbour_col(cols: &[u64; PADDED * PADDED], y: usize, z: usize, dir: u8) -> u64 {
    match dir {
        2 => cols[z * PADDED + y + 1],
        3 => cols[z * PADDED + y - 1],
        4 => cols[(z + 1) * PADDED + y],
        5 => cols[(z - 1) * PADDED + y],
        _ => 0,
    }
}

/// Emits every quad facing one direction, either opaque or water.
#[allow(clippy::too_many_arguments)]
fn mesh_dir(
    v: &Voxels,
    dir: u8,
    skirts: bool,
    water_pass: bool,
    sc: &mut MeshScratch,
    out: &mut Vec<Quad>,
    bmin: &mut [u8; 3],
    bmax: &mut [u8; 3],
) {
    let (an, au, av, pos) = AXES[dir as usize];
    let mut used = 0u32;
    {
        let solid = &*sc.solid;
        let water = &*sc.water;
        let planes = &mut sc.planes;
        let rows = &mut sc.rows;
        for z in 1..=CS {
            for y in 1..=CS {
                let s = solid[z * PADDED + y];
                let w = water[z * PADDED + y];
                // A face shows when its neighbour can be seen through. Solid faces
                // look through anything not opaque; water only shows against air.
                let mut bits = if water_pass {
                    let block = match dir {
                        0 => (s | w) >> 1,
                        1 => (s | w) << 1,
                        _ => neighbour_col(solid, y, z, dir) | neighbour_col(water, y, z, dir),
                    };
                    w & !block & INNER
                } else {
                    let block = match dir {
                        0 => s >> 1,
                        1 => s << 1,
                        _ => neighbour_col(solid, y, z, dir),
                    };
                    s & !block & INNER
                };
                while bits != 0 {
                    let x = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    let c = [x, y, z];
                    let block = v[pidx(x, y, z)];
                    let key = if water_pass {
                        0x8000 | ((face_layer(block, dir) as u16) << 8) | 0xFF
                    } else {
                        let mut n = c;
                        n[an] = if pos { n[an] + 1 } else { n[an] - 1 };
                        let ao = face_ao(solid, n, au, av);
                        solid_key(block, dir, ao)
                    };
                    let p = c[an] - 1;
                    let (uu, vv) = (c[au] - 1, c[av] - 1);
                    planes[p * CS * CS + vv * CS + uu] = key;
                    rows[p] |= 1 << vv;
                    used |= 1 << p;
                }
            }
        }

        if skirts {
            // Skirt: seals cracks against neighbours at another LOD by showing the
            // wall where a surface crosses the chunk border.
            let p = if pos { CS - 1 } else { 0 };
            for vv in 0..CS {
                for uu in 0..CS {
                    let mut c = [0usize; 3];
                    c[an] = p + 1;
                    c[au] = uu + 1;
                    c[av] = vv + 1;
                    if !bit(solid, c) || planes[p * CS * CS + vv * CS + uu] != 0 {
                        continue;
                    }
                    let t = |axis: usize, d: isize| {
                        let mut q = c;
                        q[axis] = (c[axis] as isize + d) as usize;
                        q
                    };
                    let edge = !bit(solid, t(au, 1))
                        || !bit(solid, t(au, -1))
                        || !bit(solid, t(av, 1))
                        || !bit(solid, t(av, -1));
                    if edge {
                        planes[p * CS * CS + vv * CS + uu] =
                            solid_key(v[pidx(c[0], c[1], c[2])], dir, 0xFF);
                        rows[p] |= 1 << vv;
                        used |= 1 << p;
                    }
                }
            }
        }
    }

    while used != 0 {
        let p = used.trailing_zeros() as usize;
        used &= used - 1;
        let plane = &mut sc.planes[p * CS * CS..(p + 1) * CS * CS];
        greedy_plane(plane, sc.rows[p], p, an, au, av, dir, out, bmin, bmax);
        sc.rows[p] = 0;
    }
}

#[inline(always)]
fn solid_key(block: u8, dir: u8, ao: u16) -> u16 {
    0x8000 | ((is_emissive(block) as u16) << 14) | ((face_layer(block, dir) as u16) << 8) | ao
}

/// AO for a face whose open neighbour cell is `n`. Corner order matches the
/// quad's (u, v) corners: (0,0), (1,0), (0,1), (1,1). Two bits per corner.
#[inline]
fn face_ao(solid: &[u64; PADDED * PADDED], n: [usize; 3], au: usize, av: usize) -> u16 {
    let s = |du: isize, dv: isize| {
        let mut q = n;
        q[au] = (n[au] as isize + du) as usize;
        q[av] = (n[av] as isize + dv) as usize;
        bit(solid, q)
    };
    let (um, up, vm, vp) = (s(-1, 0), s(1, 0), s(0, -1), s(0, 1));
    let (mm, pm, mp, pp) = (s(-1, -1), s(1, -1), s(-1, 1), s(1, 1));
    let c0 = vertex_ao(um, vm, mm);
    let c1 = vertex_ao(up, vm, pm);
    let c2 = vertex_ao(um, vp, mp);
    let c3 = vertex_ao(up, vp, pp);
    c0 | c1 << 2 | c2 << 4 | c3 << 6
}

/// Merges one plane's faces into rectangles, zeroing each cell it consumes.
#[allow(clippy::too_many_arguments)]
#[inline]
fn greedy_plane(
    plane: &mut [u16],
    mut rows: u32,
    p: usize,
    an: usize,
    au: usize,
    av: usize,
    dir: u8,
    out: &mut Vec<Quad>,
    bmin: &mut [u8; 3],
    bmax: &mut [u8; 3],
) {
    while rows != 0 {
        let vv = rows.trailing_zeros() as usize;
        rows &= rows - 1;
        let mut uu = 0;
        while uu < CS {
            let key = plane[vv * CS + uu];
            if key == 0 {
                uu += 1;
                continue;
            }
            let mut w = 1;
            while uu + w < CS && plane[vv * CS + uu + w] == key {
                w += 1;
            }
            let mut h = 1;
            'grow: while vv + h < CS {
                for k in 0..w {
                    if plane[(vv + h) * CS + uu + k] != key {
                        break 'grow;
                    }
                }
                h += 1;
            }
            for j in 0..h {
                plane[(vv + j) * CS + uu..(vv + j) * CS + uu + w].fill(0);
            }
            let mut pos = [0usize; 3];
            pos[an] = p;
            pos[au] = uu;
            pos[av] = vv;
            let layer = ((key >> 8) & 0x3F) as u8;
            let emissive = key & 0x4000 != 0;
            out.push(Quad::new(pos, w, h, dir, layer, key as u8, emissive));

            let positive = dir & 1 == 0;
            let plane_coord = if positive { p + 1 } else { p } as u8;
            bmin[an] = bmin[an].min(plane_coord);
            bmax[an] = bmax[an].max(plane_coord);
            bmin[au] = bmin[au].min(uu as u8);
            bmax[au] = bmax[au].max((uu + w) as u8);
            bmin[av] = bmin[av].min(vv as u8);
            bmax[av] = bmax[av].max((vv + h) as u8);
            uu += w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::PADDED_VOLUME;

    fn empty() -> Box<Voxels> {
        vec![AIR; PADDED_VOLUME]
            .into_boxed_slice()
            .try_into()
            .unwrap()
    }

    fn count_faces(m: &Mesh) -> u32 {
        m.quads
            .iter()
            .map(|q| ((q.a >> 15 & 31) + 1) * ((q.a >> 20 & 31) + 1))
            .sum()
    }

    #[test]
    fn single_block_has_six_faces() {
        let mut v = empty();
        v[pidx(10, 10, 10)] = STONE;
        let m = build_mesh(&v, 0, &mut MeshScratch::default());
        assert_eq!(m.quads.len(), 6);
        for r in &m.ranges[..6] {
            assert_eq!(r.1, 1);
        }
    }

    #[test]
    fn greedy_merges_a_flat_floor() {
        let mut v = empty();
        for z in 1..=32 {
            for x in 1..=32 {
                for y in 0..=5 {
                    v[pidx(x, y, z)] = STONE;
                }
            }
        }
        // Include the border so that the sides of the slab are hidden.
        for z in 0..PADDED {
            for x in 0..PADDED {
                for y in 0..=5 {
                    v[pidx(x, y, z)] = STONE;
                }
            }
        }
        let m = build_mesh(&v, 0, &mut MeshScratch::default());
        assert_eq!(m.quads.len(), 1, "a flat floor must merge into one quad");
        assert_eq!(count_faces(&m), 32 * 32);
    }

    #[test]
    fn breaking_a_block_exposes_inner_faces() {
        // A solid chunk has no faces; removing an interior block exposes exactly
        // its six neighbours' inner faces, facing into the hole.
        let mut v: Box<Voxels> = vec![STONE; PADDED_VOLUME]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        let mut sc = MeshScratch::default();
        assert!(build_mesh(&v, 0, &mut sc).is_empty());
        v[pidx(15, 15, 15)] = AIR;
        let m = build_mesh(&v, 0, &mut sc);
        assert_eq!(count_faces(&m), 6);
    }

    #[test]
    fn border_voxels_cull_but_do_not_emit() {
        // A block in the padding belongs to the neighbour chunk: no faces here,
        // but it must still hide this chunk's adjacent face.
        let mut v = empty();
        v[pidx(0, 10, 10)] = STONE;
        let mut sc = MeshScratch::default();
        assert!(build_mesh(&v, 0, &mut sc).is_empty());
        v[pidx(1, 10, 10)] = STONE;
        let m = build_mesh(&v, 0, &mut sc);
        assert_eq!(count_faces(&m), 5);
    }

    #[test]
    fn water_only_shows_against_air() {
        let mut v = empty();
        for z in 0..PADDED {
            for x in 0..PADDED {
                for y in 0..=9 {
                    v[pidx(x, y, z)] = WATER;
                }
            }
        }
        let m = build_mesh(&v, 0, &mut MeshScratch::default());
        assert_eq!(m.ranges[WATER_RANGE].1, 1, "one surface quad expected");
        for r in &m.ranges[..6] {
            assert_eq!(r.1, 0);
        }
        // The surface is a top face (dir 2).
        assert_eq!(m.quads[0].a >> 25 & 7, 2);
    }

    #[test]
    fn ao_darkens_a_corner_next_to_a_wall() {
        let mut v = empty();
        v[pidx(10, 10, 10)] = STONE;
        // A block diagonally above one edge shades only that side of the top face.
        v[pidx(11, 11, 10)] = STONE;
        let m = build_mesh(&v, 0, &mut MeshScratch::default());
        let start = m.ranges[2].0 as usize;
        let top = m.quads[start..]
            .iter()
            .find(|q| q.a & 31 == 9 && (q.a >> 5) & 31 == 9)
            .expect("top face of the lower block");
        assert_ne!(top.b >> 8 & 0xFF, 0xFF);
    }

    #[test]
    fn skirts_only_appear_at_lod() {
        let mut v = empty();
        // A wall that crosses the +x chunk boundary, with air beside it.
        for x in 0..PADDED {
            for y in 5..=20 {
                v[pidx(x, y, 10)] = STONE;
            }
        }
        let mut sc = MeshScratch::default();
        let lod0 = build_mesh(&v, 0, &mut sc);
        let lod1 = build_mesh(&v, 1, &mut sc);
        assert_eq!(lod0.ranges[0].1, 0, "no +x faces in a full-detail chunk");
        assert!(
            lod1.ranges[0].1 > 0,
            "LOD chunks need a skirt on the border"
        );
    }

    /// Compares against the plain definition of "exposed", face by face, on a
    /// noisy volume: every visible unit face must be covered exactly once.
    #[test]
    fn matches_a_brute_force_face_count() {
        let mut v = empty();
        let mut h = 0x1234_5678_u64;
        for i in v.iter_mut() {
            h = crate::world::noise::splitmix(h);
            *i = match h % 5 {
                0 | 1 => AIR,
                2 => WATER,
                3 => STONE,
                _ => LEAVES,
            };
        }
        let mut expected_solid = 0u32;
        let mut expected_water = 0u32;
        for z in 1..=32 {
            for y in 1..=32 {
                for x in 1..=32 {
                    let b = v[pidx(x, y, z)];
                    for (dx, dy, dz) in [
                        (1, 0, 0),
                        (-1, 0, 0),
                        (0, 1, 0),
                        (0, -1, 0),
                        (0, 0, 1),
                        (0, 0, -1),
                    ] {
                        let n = v[pidx(
                            (x as isize + dx) as usize,
                            (y as isize + dy) as usize,
                            (z as isize + dz) as usize,
                        )];
                        if is_opaque(b) && !is_opaque(n) {
                            expected_solid += 1;
                        }
                        if b == WATER && n == AIR {
                            expected_water += 1;
                        }
                    }
                }
            }
        }
        let m = build_mesh(&v, 0, &mut MeshScratch::default());
        let area = |range: (u32, u32)| {
            m.quads[range.0 as usize..(range.0 + range.1) as usize]
                .iter()
                .map(|q| ((q.a >> 15 & 31) + 1) * ((q.a >> 20 & 31) + 1))
                .sum::<u32>()
        };
        let solid: u32 = (0..6).map(|d| area(m.ranges[d])).sum();
        assert_eq!(solid, expected_solid);
        assert_eq!(area(m.ranges[WATER_RANGE]), expected_water);
    }
}
