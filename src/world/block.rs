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
pub const BEDROCK: u8 = 12;
pub const LAVA: u8 = 13;
pub const DARK_GRASS: u8 = 14;
pub const DARK_DIRT: u8 = 15;
pub const SLATE: u8 = 16;
pub const MOSSY_COBBLE: u8 = 17;
pub const GRAVEL: u8 = 18;
pub const PINE_LOG: u8 = 19;
pub const PINE_NEEDLES: u8 = 20;
pub const BASALT: u8 = 21;
pub const EMBER: u8 = 22;
pub const ASH: u8 = 23;

/// Declares the texture array in one place: a constant holding each layer's index, plus
/// the embedded `textures/<name>_32.png` files in the same order.
macro_rules! texture_layers {
    ($($layer:ident $name:literal),* $(,)?) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[repr(u8)]
        enum Layer { $($layer),* }
        $(const $layer: u8 = Layer::$layer as u8;)*
        /// Names and PNG data, in layer order.
        pub static TEXTURES: &[(&str, &[u8])] = &[
            $(($name, include_bytes!(concat!("../../textures/", $name, "_32.png")))),*
        ];
    };
}

texture_layers! {
    L_STONE "stone",
    L_COBBLE "cobblestone",
    L_DIRT "dirt",
    L_GRASS "grass",
    L_GRASS_SIDE "grass_side",
    L_SAND "sand",
    L_WATER "water",
    L_LOG_SIDE "log_side",
    L_LOG_TOP "log_top",
    L_LEAVES "leaves",
    L_COAL "coal_ore",
    L_IRON "iron_ore",
    L_CRYSTAL "crystal",
    L_BEDROCK "bedrock",
    L_LAVA "lava",
    L_DARK_GRASS "dark_grass",
    L_DARK_GRASS_SIDE "dark_grass_side",
    L_DARK_DIRT "dark_dirt",
    L_SLATE "slate",
    L_MOSSY_COBBLE "mossy_cobblestone",
    L_GRAVEL "gravel",
    L_PINE_LOG_SIDE "pine_log_side",
    L_PINE_LOG_TOP "pine_log_top",
    L_PINE_NEEDLES "pine_needles",
    L_BASALT "basalt",
    L_EMBER "ember",
    L_ASH "ash",
}

#[inline(always)]
pub fn is_liquid(b: u8) -> bool {
    b == WATER || b == LAVA
}

#[inline(always)]
pub fn is_opaque(b: u8) -> bool {
    b != AIR && !is_liquid(b)
}

#[inline]
pub fn is_breakable(b: u8) -> bool {
    b != BEDROCK
}

/// Texture layer for a block face. `dir` follows the mesher's convention:
/// 0:+x 1:-x 2:+y 3:-y 4:+z 5:-z.
#[inline]
pub fn face_layer(b: u8, dir: u8) -> u8 {
    let vertical = dir == 2 || dir == 3;
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
        LOG if vertical => L_LOG_TOP,
        LOG => L_LOG_SIDE,
        LEAVES => L_LEAVES,
        COAL => L_COAL,
        IRON => L_IRON,
        CRYSTAL => L_CRYSTAL,
        BEDROCK => L_BEDROCK,
        LAVA => L_LAVA,
        DARK_GRASS => match dir {
            2 => L_DARK_GRASS,
            3 => L_DARK_DIRT,
            _ => L_DARK_GRASS_SIDE,
        },
        DARK_DIRT => L_DARK_DIRT,
        SLATE => L_SLATE,
        MOSSY_COBBLE => L_MOSSY_COBBLE,
        GRAVEL => L_GRAVEL,
        PINE_LOG if vertical => L_PINE_LOG_TOP,
        PINE_LOG => L_PINE_LOG_SIDE,
        PINE_NEEDLES => L_PINE_NEEDLES,
        BASALT => L_BASALT,
        EMBER => L_EMBER,
        ASH => L_ASH,
        _ => L_STONE,
    }
}

#[inline]
pub fn is_emissive(b: u8) -> bool {
    b == CRYSTAL || b == EMBER
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The terrain shader needs some layer numbers of its own, and WGSL can't share them.
    #[test]
    fn shader_layer_numbers_match() {
        let shader = include_str!("../../shaders/terrain.wgsl");
        let layers = [
            ("L_GRASS", L_GRASS),
            ("L_GRASS_SIDE", L_GRASS_SIDE),
            ("L_LEAVES", L_LEAVES),
            ("L_DARK_GRASS", L_DARK_GRASS),
            ("L_DARK_GRASS_SIDE", L_DARK_GRASS_SIDE),
            ("L_PINE_NEEDLES", L_PINE_NEEDLES),
            ("L_EMBER", L_EMBER),
            ("L_LAVA", L_LAVA),
        ];
        for (name, number) in layers {
            let line = format!("const {name}: u32 = {number}u;");
            assert!(shader.contains(&line), "terrain.wgsl needs `{line}`");
        }
    }
}
