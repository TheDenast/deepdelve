//! Block ids and their rendering properties.

pub const AIR: u8 = 0;
pub const STONE: u8 = 1;
pub const COBBLE: u8 = 2;
pub const DIRT: u8 = 3;
pub const GRASS: u8 = 4;
pub const SAND: u8 = 5;
pub const WATER: u8 = 6;
pub const LOG: u8 = 7;
pub const LEAVES: u8 = 8;
pub const COAL: u8 = 9;
pub const IRON: u8 = 10;
pub const CRYSTAL: u8 = 11;

/// Texture array layers, in order. Each name maps to `textures/<name>_32.png`.
pub const TEXTURE_NAMES: &[&str] = &[
    "stone",
    "cobblestone",
    "dirt",
    "grass",
    "grass_side",
    "sand",
    "water",
    "log_side",
    "log_top",
    "leaves",
    "coal_ore",
    "iron_ore",
    "crystal",
];

const L_STONE: u8 = 0;
const L_COBBLE: u8 = 1;
const L_DIRT: u8 = 2;
const L_GRASS: u8 = 3;
const L_GRASS_SIDE: u8 = 4;
const L_SAND: u8 = 5;
const L_WATER: u8 = 6;
const L_LOG_SIDE: u8 = 7;
const L_LOG_TOP: u8 = 8;
const L_LEAVES: u8 = 9;
const L_COAL: u8 = 10;
const L_IRON: u8 = 11;
const L_CRYSTAL: u8 = 12;

#[inline(always)]
pub fn is_opaque(b: u8) -> bool {
    b != AIR && b != WATER
}

/// Texture layer for a block face. `dir` follows the mesher's convention:
/// 0:+x 1:-x 2:+y 3:-y 4:+z 5:-z.
#[inline]
pub fn face_layer(b: u8, dir: u8) -> u8 {
    match b {
        STONE => L_STONE,
        COBBLE => L_COBBLE,
        DIRT => L_DIRT,
        GRASS => match dir {
            2 => L_GRASS,
            3 => L_DIRT,
            _ => L_GRASS_SIDE,
        },
        SAND => L_SAND,
        WATER => L_WATER,
        LOG => {
            if dir == 2 || dir == 3 {
                L_LOG_TOP
            } else {
                L_LOG_SIDE
            }
        }
        LEAVES => L_LEAVES,
        COAL => L_COAL,
        IRON => L_IRON,
        CRYSTAL => L_CRYSTAL,
        _ => L_STONE,
    }
}

#[inline]
pub fn is_emissive(b: u8) -> bool {
    b == CRYSTAL
}
