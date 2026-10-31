//! Derives the textures that weren't in the supplied set. Run once from the repo
//! root with `cargo run --release --bin gen_textures`; results are committed.

use image::{Rgba, RgbaImage};

const SIZE: u32 = 32;

fn hash(x: i32, y: i32, salt: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ salt.wrapping_mul(0xCB1A_B31F);
    h = (h ^ (h >> 13)).wrapping_mul(0x5BD1_E995);
    h ^= h >> 15;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Pale faceted crystal. Bright by design, since the shader treats it as emissive.
fn crystal() -> RgbaImage {
    const CELLS: i32 = 4;
    let cell = SIZE as f32 / CELLS as f32;
    let mut img = RgbaImage::new(SIZE, SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (mut d1, mut d2, mut id) = (f32::MAX, f32::MAX, (0, 0));
            let (cx, cy) = ((px / cell) as i32, (py / cell) as i32);
            for j in cy - 1..=cy + 1 {
                for i in cx - 1..=cx + 1 {
                    // Wrapping the cell ids keeps the texture tileable.
                    let (wi, wj) = (i.rem_euclid(CELLS), j.rem_euclid(CELLS));
                    let sx = (i as f32 + 0.15 + 0.7 * hash(wi, wj, 1)) * cell;
                    let sy = (j as f32 + 0.15 + 0.7 * hash(wi, wj, 2)) * cell;
                    let d = ((px - sx).powi(2) + (py - sy).powi(2)).sqrt();
                    if d < d1 {
                        d2 = d1;
                        d1 = d;
                        id = (wi, wj);
                    } else if d < d2 {
                        d2 = d;
                    }
                }
            }
            let facet = 0.62 + 0.38 * hash(id.0, id.1, 3);
            let edge = (1.0 - (d2 - d1) / 2.2).clamp(0.0, 1.0);
            let hue = hash(id.0, id.1, 4);
            let base = [0.45 + 0.30 * hue, 0.86 - 0.14 * hue, 1.0];
            let v = (facet + edge * 0.5).min(1.25);
            let c = |b: f32| ((b * v).clamp(0.0, 1.0) * 255.0) as u8;
            img.put_pixel(x, y, Rgba([c(base[0]), c(base[1]), c(base[2]), 255]));
        }
    }
    img
}

/// Dirt side with the grass top hanging over it in an uneven fringe.
fn grass_side() -> RgbaImage {
    let dirt = image::open("textures/dirt_32.png")
        .expect("dirt")
        .to_rgba8();
    let grass = image::open("textures/grass_32.png")
        .expect("grass")
        .to_rgba8();
    let mut img = RgbaImage::new(SIZE, SIZE);
    for x in 0..SIZE {
        // Sum of two periodic waves plus per-column jitter.
        let fx = x as f32 / SIZE as f32 * std::f32::consts::TAU;
        let fringe = 15.0
            + 3.0 * (fx * 2.0 + 0.7).sin()
            + 2.0 * (fx * 5.0).cos()
            + 4.0 * hash(x as i32, 0, 9);
        for y in 0..SIZE {
            let (src, shade) = if (y as f32) < fringe {
                (grass.get_pixel(x, y), 0.95)
            } else {
                // Soil just under the grass is a little darker.
                let k = ((y as f32 - fringe) / 3.0).clamp(0.0, 1.0);
                (dirt.get_pixel(x, y), 0.95 + 0.4 * k)
            };
            let c = |v: u8| (v as f32 * shade).min(255.0) as u8;
            img.put_pixel(x, y, Rgba([c(src[0]), c(src[1]), c(src[2]), 255]));
        }
    }
    img
}

fn main() {
    crystal()
        .save("textures/crystal_32.png")
        .expect("save crystal");
    grass_side()
        .save("textures/grass_side_32.png")
        .expect("save grass_side");
    println!("wrote textures/crystal_32.png and textures/grass_side_32.png");
}
