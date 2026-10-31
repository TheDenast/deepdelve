//! World data model: 3D chunks, block storage and procedural generation.

pub mod block;
pub mod generator;
pub mod noise;

/// Edge length of a chunk, in voxels, at every LOD level.
pub const CHUNK_SIZE: i32 = 32;
/// Chunks are generated with a one-voxel border so that each can be meshed
/// without waiting for its neighbours.
pub const PADDED: usize = 34;
pub const PADDED_VOLUME: usize = PADDED * PADDED * PADDED;
/// Coarsest LOD level. Voxels at level L measure 2^L blocks.
pub const MAX_LOD: u8 = 4;

pub type Voxels = [u8; PADDED_VOLUME];

/// Index into a padded voxel array. Coordinates are in `0..PADDED`, so the
/// chunk's own voxels live in `1..=32`.
#[inline(always)]
pub const fn pidx(x: usize, y: usize, z: usize) -> usize {
    y * PADDED * PADDED + z * PADDED + x
}

/// Identifies a chunk at a LOD level. Coordinates count chunks of that level,
/// i.e. units of `32 << lod` blocks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ChunkKey {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub lod: u8,
}

impl ChunkKey {
    pub const fn new(x: i32, y: i32, z: i32, lod: u8) -> Self {
        Self { x, y, z, lod }
    }

    /// Edge length in blocks.
    #[inline]
    pub const fn size(self) -> i64 {
        (CHUNK_SIZE as i64) << self.lod
    }

    #[inline]
    pub const fn voxel_size(self) -> i64 {
        1i64 << self.lod
    }

    /// World position of the chunk's minimum corner.
    #[inline]
    pub const fn origin(self) -> [i64; 3] {
        let s = self.size();
        [self.x as i64 * s, self.y as i64 * s, self.z as i64 * s]
    }

    /// The eight children one level finer. Only valid when `lod > 0`.
    #[inline]
    pub fn children(self) -> [ChunkKey; 8] {
        let l = self.lod - 1;
        let (x, y, z) = (self.x * 2, self.y * 2, self.z * 2);
        [
            ChunkKey::new(x, y, z, l),
            ChunkKey::new(x + 1, y, z, l),
            ChunkKey::new(x, y + 1, z, l),
            ChunkKey::new(x + 1, y + 1, z, l),
            ChunkKey::new(x, y, z + 1, l),
            ChunkKey::new(x + 1, y, z + 1, l),
            ChunkKey::new(x, y + 1, z + 1, l),
            ChunkKey::new(x + 1, y + 1, z + 1, l),
        ]
    }

    /// Squared distance from a point to this chunk's bounding box.
    #[inline]
    pub fn dist2(self, p: [f64; 3]) -> f64 {
        let o = self.origin();
        let s = self.size() as f64;
        let mut d2 = 0.0;
        for i in 0..3 {
            let lo = o[i] as f64;
            let d = (lo - p[i]).max(p[i] - (lo + s)).max(0.0);
            d2 += d * d;
        }
        d2
    }
}

/// Chunk containing a block position, at LOD 0.
#[inline]
pub fn block_to_chunk(p: i64) -> i32 {
    p.div_euclid(CHUNK_SIZE as i64) as i32
}
