//! Paints every block texture. Run from the repo root and commit the results:
//!
//!     cargo run --release --bin gen_textures [-- --sheet preview.png]
//!
//! All of them tile, since each block repeats its texture. `--sheet` also saves a
//! preview with each texture repeated, which makes seams easy to spot.

use image::{Rgba, RgbaImage};

const SIZE: u32 = 32;
const N: f32 = SIZE as f32;

type Rgb = [f32; 3];

const fn rgb(hex: u32) -> Rgb {
    [
        ((hex >> 16) & 255) as f32 / 255.0,
        ((hex >> 8) & 255) as f32 / 255.0,
        (hex & 255) as f32 / 255.0,
    ]
}

fn hash(x: i32, y: i32, salt: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ salt.wrapping_mul(0xCB1A_B31F);
    h = (h ^ (h >> 13)).wrapping_mul(0x5BD1_E995);
    h = (h ^ (h >> 15)).wrapping_mul(0x2545_F491);
    h ^= h >> 16;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Tiling value noise over `(px, py)` cells. `u` and `v` run over 0..1.
fn value(u: f32, v: f32, px: i32, py: i32, salt: u32) -> f32 {
    let (x, y) = (u * px as f32, v * py as f32);
    let (xf, yf) = (x.floor(), y.floor());
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(x - xf), s(y - yf));
    let (xi, yi) = (xf as i32, yf as i32);
    let at = |i: i32, j: i32| hash(i.rem_euclid(px), j.rem_euclid(py), salt);
    let top = at(xi, yi) + (at(xi + 1, yi) - at(xi, yi)) * sx;
    let bottom = at(xi, yi + 1) + (at(xi + 1, yi + 1) - at(xi, yi + 1)) * sx;
    top + (bottom - top) * sy
}

/// Fractal value noise, stretched to fill most of 0..1.
fn fbm(u: f32, v: f32, px: i32, py: i32, octaves: u32, salt: u32) -> f32 {
    let (mut sum, mut amp, mut norm) = (0.0, 1.0, 0.0);
    let (mut fx, mut fy) = (px, py);
    for i in 0..octaves {
        sum += value(u, v, fx, fy, salt + i) * amp;
        norm += amp;
        amp *= 0.5;
        fx *= 2;
        fy *= 2;
    }
    ((sum / norm - 0.5) * 1.9 + 0.5).clamp(0.0, 1.0)
}

struct Cell {
    /// Distance to the nearest and second-nearest points, in pixels.
    d1: f32,
    d2: f32,
    /// A value from 0 to 1 that's the same over the whole cell.
    id: f32,
    /// Offset from the cell's point to this pixel, in pixels.
    dx: f32,
    dy: f32,
}

/// Tiling Voronoi cells, `cx` across and `cy` down.
fn voronoi(x: f32, y: f32, cx: i32, cy: i32, salt: u32) -> Cell {
    let (w, h) = (N / cx as f32, N / cy as f32);
    let (ci, cj) = ((x / w).floor() as i32, (y / h).floor() as i32);
    let mut c = Cell {
        d1: f32::MAX,
        d2: f32::MAX,
        id: 0.0,
        dx: 0.0,
        dy: 0.0,
    };
    for j in cj - 2..=cj + 2 {
        for i in ci - 2..=ci + 2 {
            let (wi, wj) = (i.rem_euclid(cx), j.rem_euclid(cy));
            let sx = (i as f32 + 0.12 + 0.76 * hash(wi, wj, salt)) * w;
            let sy = (j as f32 + 0.12 + 0.76 * hash(wi, wj, salt + 1)) * h;
            let (dx, dy) = (x - sx, y - sy);
            let d = (dx * dx + dy * dy).sqrt();
            if d < c.d1 {
                c.d2 = c.d1;
                c.d1 = d;
                c.id = hash(wi, wj, salt + 2);
                c.dx = dx;
                c.dy = dy;
            } else if d < c.d2 {
                c.d2 = d;
            }
        }
    }
    c
}

/// Evenly spaced colour ramp.
fn ramp(colors: &[Rgb], t: f32) -> Rgb {
    let f = t.clamp(0.0, 0.9999) * (colors.len() - 1) as f32;
    let (i, k) = (f as usize, f.fract());
    let (a, b) = (colors[i], colors[i + 1]);
    [
        a[0] + (b[0] - a[0]) * k,
        a[1] + (b[1] - a[1]) * k,
        a[2] + (b[2] - a[2]) * k,
    ]
}

/// Hard-edged bands give the crisp, hand-picked look of pixel art.
fn bands(t: f32, levels: f32) -> f32 {
    ((t.clamp(0.0, 0.9999) * levels).floor() + 0.5) / levels
}

fn shade(c: Rgb, k: f32) -> Rgb {
    [c[0] * k, c[1] * k, c[2] * k]
}

/// Pixel-level grain, so that flat areas don't look airbrushed.
fn grain(c: Rgb, x: u32, y: u32, amount: f32, salt: u32) -> Rgb {
    shade(c, 1.0 + (hash(x as i32, y as i32, salt) - 0.5) * amount)
}

struct Canvas(Vec<Rgb>);

impl Canvas {
    fn paint(f: impl Fn(u32, u32, f32, f32) -> Rgb) -> Self {
        let mut px = Vec::with_capacity((SIZE * SIZE) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                px.push(f(x, y, (x as f32 + 0.5) / N, (y as f32 + 0.5) / N));
            }
        }
        Canvas(px)
    }

    fn get(&self, x: i32, y: i32) -> Rgb {
        let (x, y) = (x.rem_euclid(SIZE as i32), y.rem_euclid(SIZE as i32));
        self.0[(y as u32 * SIZE + x as u32) as usize]
    }

    fn set(&mut self, x: i32, y: i32, c: Rgb) {
        let (x, y) = (x.rem_euclid(SIZE as i32), y.rem_euclid(SIZE as i32));
        self.0[(y as u32 * SIZE + x as u32) as usize] = c;
    }

    /// Short strokes in random positions: grass blades and pine needles.
    fn strokes(&mut self, count: i32, len: i32, (dx, dy): (i32, i32), colors: &[Rgb], salt: u32) {
        for i in 0..count {
            let x = (hash(i, 0, salt) * N) as i32;
            let y = (hash(i, 1, salt) * N) as i32;
            let c = colors[(hash(i, 2, salt) * colors.len() as f32) as usize % colors.len()];
            let l = 1 + (hash(i, 3, salt) * len as f32) as i32;
            let bend = if hash(i, 4, salt) < 0.5 { 1 } else { -1 };
            for s in 0..l {
                let drift = if s * 2 > l { bend } else { 0 };
                self.set(
                    x + dx * s + drift * dy.abs(),
                    y + dy * s + drift * dx.abs(),
                    c,
                );
            }
        }
    }

    /// Takes out the tile's broad light and dark areas, and keeps its fine detail. Those areas
    /// are what the eye follows across many blocks: as rows when every block is the same, or
    /// as a grid along block edges when blocks are turned at random.
    fn even_out(mut self) -> Self {
        const RADIUS: i32 = 3;
        // One pass leaves a little of the shading behind, because a blur only partly finds it.
        const PASSES: usize = 3;
        let luma = |c: Rgb| c[0] * 0.3 + c[1] * 0.6 + c[2] * 0.1;
        let at = |v: &[f32], x: i32, y: i32| {
            let (x, y) = (x.rem_euclid(SIZE as i32), y.rem_euclid(SIZE as i32));
            v[(y as u32 * SIZE + x as u32) as usize]
        };
        for _ in 0..PASSES {
            let mut soft: Vec<f32> = self.0.iter().map(|c| luma(*c)).collect();
            // Blurring twice along each axis is close to a smooth round blur.
            for (dx, dy) in [(1, 0), (0, 1), (1, 0), (0, 1)] {
                let before = soft.clone();
                for y in 0..SIZE as i32 {
                    for x in 0..SIZE as i32 {
                        let sum: f32 = (-RADIUS..=RADIUS)
                            .map(|k| at(&before, x + k * dx, y + k * dy))
                            .sum();
                        soft[(y as u32 * SIZE + x as u32) as usize] = sum / (RADIUS * 2 + 1) as f32;
                    }
                }
            }
            let mean = soft.iter().sum::<f32>() / soft.len() as f32;
            for (c, s) in self.0.iter_mut().zip(&soft) {
                *c = c.map(|v| v * mean / s);
            }
        }
        self
    }

    fn image(&self) -> RgbaImage {
        let mut img = RgbaImage::new(SIZE, SIZE);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let c = self.get(x as i32, y as i32);
                let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                img.put_pixel(x, y, Rgba([b(c[0]), b(c[1]), b(c[2]), 255]));
            }
        }
        img
    }
}

/// Layered rock with cracks, shared by the three kinds of bare stone.
fn rock(colors: &[Rgb], crack: Rgb, salt: u32) -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let warp = value(u, v, 4, 4, salt + 40) * 3.0;
        let strata = fbm(u, (v + warp / N).fract(), 2, 7, 3, salt);
        let blotch = fbm(u, v, 5, 5, 3, salt + 10);
        let t = bands(strata * 0.55 + blotch * 0.45, 6.0);
        let cells = voronoi(px, py, 3, 3, salt + 20);
        let mut c = ramp(colors, t);
        let edge = cells.d2 - cells.d1;
        // Only some cell borders crack, or the rock would look like paving.
        if edge < 1.0 && value(u, v, 6, 6, salt + 30) > 0.58 {
            c = crack;
        } else if edge < 2.0 && value(u, v, 6, 6, salt + 30) > 0.58 {
            c = shade(c, 1.1);
        }
        grain(c, x, y, 0.07, salt + 50)
    })
    .even_out()
}

fn stone() -> Canvas {
    rock(
        &[rgb(0x5F6065), rgb(0x76777B), rgb(0x8C8C8D), rgb(0xA19F9C)],
        rgb(0x46474D),
        100,
    )
}

/// The dark forest's rock: blue-grey and strongly layered.
fn slate() -> Canvas {
    rock(
        &[rgb(0x3E474C), rgb(0x4E585C), rgb(0x5E6A6C), rgb(0x707C7C)],
        rgb(0x2A3237),
        200,
    )
}

/// The underworld's rock: nearly black, with faint purple-grey facets.
fn basalt() -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let c = voronoi(px, py, 4, 4, 300);
        let t = bands(c.id * 0.6 + fbm(u, v, 6, 6, 2, 310) * 0.4, 4.0);
        let mut col = ramp(
            &[rgb(0x201F26), rgb(0x2C2A33), rgb(0x393640), rgb(0x47434E)],
            t,
        );
        let edge = c.d2 - c.d1;
        if edge < 1.0 {
            col = rgb(0x121115);
        } else if edge < 2.0 && c.dy < 0.0 {
            col = shade(col, 1.35);
        }
        grain(col, x, y, 0.1, 320)
    })
}

/// Basalt split by glowing cracks. Only this block's bright parts emit light.
fn ember() -> Canvas {
    let base = basalt();
    Canvas::paint(|x, y, u, v| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let c = voronoi(px, py, 3, 3, 340);
        let edge = c.d2 - c.d1;
        let open = value(u, v, 5, 5, 350);
        if edge < 2.4 && open > 0.3 {
            let hot = (1.0 - edge / 2.4) * (0.65 + 0.35 * open);
            ramp(&[rgb(0xA12C08), rgb(0xF0730E), rgb(0xFFD35C)], hot)
        } else {
            shade(base.get(x as i32, y as i32), 0.5)
        }
    })
}

/// Fine dark grit covering the underworld's flatter ground.
fn ash() -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let t = bands(fbm(u, v, 4, 4, 4, 400), 5.0);
        let mut c = ramp(
            &[rgb(0x3A3634), rgb(0x4A443E), rgb(0x5A5349), rgb(0x6A6155)],
            t,
        );
        let fleck = hash(x as i32, y as i32, 410);
        if fleck > 0.96 {
            c = rgb(0x6B6257);
        } else if fleck < 0.04 {
            c = rgb(0x17151A);
        }
        grain(c, x, y, 0.08, 420)
    })
    .even_out()
}

/// Jagged and high-contrast on purpose, so it reads as something unlike rock.
fn bedrock() -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let warp = (value(u, v, 8, 8, 500) - 0.5) * 6.0;
        let c = voronoi(px + warp, py - warp, 6, 6, 510);
        let t = bands(c.id * 0.75 + fbm(u, v, 8, 8, 2, 520) * 0.25, 4.0);
        let mut col = ramp(
            &[rgb(0x0C0C0E), rgb(0x2B2B2F), rgb(0x55555A), rgb(0x8A8A8E)],
            t,
        );
        if c.d2 - c.d1 < 0.9 {
            col = rgb(0x050506);
        }
        grain(col, x, y, 0.08, 530)
    })
}

/// Rounded stones set in dark mortar, lit from the top left.
fn cobbles(colors: &[Rgb], mortar: Rgb, salt: u32) -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let c = voronoi(px, py, 4, 4, salt);
        let edge = c.d2 - c.d1;
        if edge < 1.25 {
            return grain(mortar, x, y, 0.15, salt + 5);
        }
        let t = c.id * 0.65 + fbm(u, v, 8, 8, 2, salt + 10) * 0.35;
        let mut col = ramp(colors, bands(t, 5.0));
        if edge < 3.2 {
            // The stone's rim catches light on the upper left and falls off opposite.
            let lit = (-c.dx - c.dy) / c.d1.max(1.0);
            col = shade(col, 1.0 + lit * 0.17);
        }
        grain(col, x, y, 0.06, salt + 20)
    })
}

fn cobble() -> Canvas {
    cobbles(
        &[rgb(0x5B5C61), rgb(0x717277), rgb(0x87878A), rgb(0x9D9C9B)],
        rgb(0x34353A),
        600,
    )
}

/// Slate cobbles with moss creeping over them.
fn mossy_cobble() -> Canvas {
    let stones = cobbles(
        &[rgb(0x303739), rgb(0x3F484A), rgb(0x4E595A), rgb(0x5E6A69)],
        rgb(0x171C1E),
        620,
    );
    Canvas::paint(|x, y, u, v| {
        let moss = fbm(u, v, 3, 3, 4, 630);
        if moss > 0.56 {
            let c = ramp(
                &[rgb(0x1D3B26), rgb(0x2A5230), rgb(0x3B6B3A)],
                bands(fbm(u, v, 10, 10, 2, 640), 3.0),
            );
            grain(c, x, y, 0.12, 650)
        } else {
            stones.get(x as i32, y as i32)
        }
    })
}

/// Soil with a scatter of small stones.
fn soil(colors: &[Rgb], pebble: Rgb, salt: u32) -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let t = bands(fbm(u, v, 5, 5, 4, salt), 5.0);
        let mut c = ramp(colors, t);
        let p = voronoi(px, py, 6, 6, salt + 10);
        if p.id > 0.62 && p.d1 < 1.5 {
            let lit = if p.dy < 0.0 { 1.18 } else { 0.9 };
            c = shade(pebble, lit);
        }
        grain(c, x, y, 0.09, salt + 20)
    })
    .even_out()
}

fn dirt() -> Canvas {
    soil(
        &[rgb(0x4F3421), rgb(0x65442B), rgb(0x7A5535), rgb(0x8C6440)],
        rgb(0x8E8172),
        700,
    )
}

fn dark_dirt() -> Canvas {
    soil(
        &[rgb(0x1E1713), rgb(0x2A201A), rgb(0x362921), rgb(0x423329)],
        rgb(0x4E4A45),
        720,
    )
}

/// Turf: broad patches of colour, with blades picked out on top.
fn turf(colors: &[Rgb], blades: &[Rgb], salt: u32) -> Canvas {
    let mut c = Canvas::paint(|x, y, u, v| {
        let t = bands(fbm(u, v, 4, 4, 3, salt), 5.0);
        grain(ramp(colors, t), x, y, 0.06, salt + 10)
    });
    c.strokes(90, 3, (0, -1), blades, salt + 20);
    c.even_out()
}

const GRASS_GREENS: [Rgb; 4] = [rgb(0x47803A), rgb(0x548F40), rgb(0x629E47), rgb(0x72AB50)];
const GRASS_BLADES: [Rgb; 4] = [rgb(0x3A6C30), rgb(0x8AB95E), rgb(0x437A36), rgb(0x97C468)];
const DARK_GREENS: [Rgb; 4] = [rgb(0x1E4730), rgb(0x255338), rgb(0x2C5F40), rgb(0x346B47)];
const DARK_BLADES: [Rgb; 4] = [rgb(0x163A26), rgb(0x3F7E50), rgb(0x1B4530), rgb(0x478A58)];

fn grass() -> Canvas {
    turf(&GRASS_GREENS, &GRASS_BLADES, 800)
}

fn dark_grass() -> Canvas {
    turf(&DARK_GREENS, &DARK_BLADES, 820)
}

/// A soil side with turf hanging over its top edge in uneven tufts.
fn turf_side(top: &Canvas, soil: &Canvas, salt: u32) -> Canvas {
    Canvas::paint(|x, y, _, _| {
        // Two waves that repeat over the width keep the fringe tiling.
        let a = x as f32 / N * std::f32::consts::TAU;
        let fringe = 13.0
            + 1.4 * (a * 2.0 + 0.7).sin()
            + 0.9 * (a * 5.0).cos()
            + 3.6 * hash(x as i32, 0, salt);
        let py = y as f32;
        if py < fringe {
            top.get(x as i32, y as i32)
        } else if py < fringe + 1.0 {
            shade(top.get(x as i32, y as i32), 0.62)
        } else {
            // Soil just under the turf sits in its shade.
            let k = ((py - fringe) / 4.0).clamp(0.0, 1.0);
            shade(soil.get(x as i32, y as i32), 0.72 + 0.28 * k)
        }
    })
}

fn sand() -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let wave = (std::f32::consts::TAU * (v * 3.0 + value(u, v, 3, 3, 900) * 1.4)).sin();
        let t = bands(
            0.5 + wave * 0.17 + (fbm(u, v, 8, 8, 2, 910) - 0.5) * 0.45,
            5.0,
        );
        let mut c = ramp(
            &[rgb(0xB89E6C), rgb(0xCDB480), rgb(0xDCC592), rgb(0xE8D5A4)],
            t,
        );
        let fleck = hash(x as i32, y as i32, 920);
        if fleck > 0.97 {
            c = rgb(0xF4E6BE);
        } else if fleck < 0.03 {
            c = rgb(0x9E8558);
        }
        grain(c, x, y, 0.04, 930)
    })
    .even_out()
}

/// Wet shore stones for the dark forest's ponds.
fn gravel() -> Canvas {
    Canvas::paint(|x, y, _, _| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let c = voronoi(px, py, 7, 7, 940);
        if c.d2 - c.d1 < 0.8 {
            return rgb(0x14171A);
        }
        let mut col = ramp(
            &[rgb(0x2C3236), rgb(0x3D4448), rgb(0x50575A), rgb(0x666C6C)],
            bands(c.id, 4.0),
        );
        if c.dy < -0.8 && c.dx < 0.5 {
            col = shade(col, 1.22);
        }
        grain(col, x, y, 0.08, 950)
    })
}

/// Soft and low in contrast, because the shader scrolls two copies over each other
/// and anything sharper shows up as a grid across a whole sea.
fn water() -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let warp = fbm(u, v, 3, 3, 3, 1000);
        let swell = (std::f32::consts::TAU * (v * 2.0 + warp * 1.6)).sin() * 0.5 + 0.5;
        let t = swell * 0.55 + fbm(u, v, 5, 5, 3, 1010) * 0.45;
        grain(
            ramp(&[rgb(0x6C93A8), rgb(0x7EA6BA), rgb(0x90B8CA)], t),
            x,
            y,
            0.015,
            1020,
        )
    })
}

/// White-hot channels between dark plates of crust, stored as brightness alone. The
/// shader turns this into colour, and scrolls two copies to make it churn.
fn lava() -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let warp = (value(u, v, 4, 4, 1100) - 0.5) * 7.0;
        let (px, py) = (x as f32 + 0.5 + warp, y as f32 + 0.5 - warp);
        let c = voronoi(px, py, 3, 3, 1110);
        let edge = c.d2 - c.d1;
        let channel = (1.0 - edge / 5.0).clamp(0.0, 1.0);
        let t = channel * channel * 0.82 + c.id * 0.08 + fbm(u, v, 6, 6, 2, 1120) * 0.14;
        [t, t, t]
    })
}

/// Bark in vertical ridges. Wide bands wander so the trunk doesn't look ruled.
fn bark(colors: &[Rgb], furrow: Rgb, salt: u32) -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let wander = (value(u, v, 2, 2, salt) - 0.5) * 0.05;
        let ridge = fbm((u + wander).rem_euclid(1.0), v, 10, 1, 2, salt + 10);
        let mut c = ramp(colors, bands(ridge, 5.0));
        if ridge < 0.22 {
            c = furrow;
        }
        grain(c, x, y, 0.07, salt + 20)
    })
}

/// A sawn log: growth rings inside a rim of bark.
fn log_end(rings: &[Rgb], rim: Rgb, salt: u32) -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let (dx, dy) = (u - 0.5, v - 0.5);
        let wobble = (value(u, v, 4, 4, salt) - 0.5) * 0.05;
        let r = (dx * dx + dy * dy).sqrt() + wobble;
        // The square's edge is the rim, so that neighbouring logs meet cleanly.
        if dx.abs().max(dy.abs()) > 0.46 {
            return grain(rim, x, y, 0.12, salt + 10);
        }
        let ring = (r * 26.0).sin() * 0.5 + 0.5;
        grain(ramp(rings, bands(ring, 4.0)), x, y, 0.05, salt + 20)
    })
}

/// Leaves in overlapping clumps, darker in the gaps.
fn foliage(colors: &[Rgb], gap: Rgb, salt: u32) -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let c = voronoi(px, py, 7, 7, salt);
        if c.d2 - c.d1 < 0.75 {
            return grain(gap, x, y, 0.1, salt + 10);
        }
        let t = c.id * 0.6 + fbm(u, v, 3, 3, 2, salt + 20) * 0.4;
        let mut col = ramp(colors, bands(t, 4.0));
        // Each clump catches light on its upper side.
        if c.dy < -0.6 {
            col = shade(col, 1.2);
        } else if c.dy > 1.6 {
            col = shade(col, 0.82);
        }
        grain(col, x, y, 0.05, salt + 30)
    })
}

fn leaves() -> Canvas {
    foliage(
        &[rgb(0x2F7A2C), rgb(0x3C8F31), rgb(0x4DA338), rgb(0x60B441)],
        rgb(0x1C4F1F),
        1200,
    )
}

/// Dark needles in slanting sprays.
fn pine_needles() -> Canvas {
    let mut c = Canvas::paint(|x, y, u, v| {
        let t = bands(fbm(u, v, 4, 4, 3, 1220), 4.0);
        grain(
            ramp(
                &[rgb(0x14352B), rgb(0x1A4134), rgb(0x214E3D), rgb(0x285B47)],
                t,
            ),
            x,
            y,
            0.06,
            1230,
        )
    });
    let tones = [rgb(0x37765E), rgb(0x0F2A22), rgb(0x418568), rgb(0x1C4737)];
    c.strokes(70, 4, (1, 1), &tones, 1240);
    c.strokes(70, 4, (-1, 1), &tones, 1250);
    c
}

/// Facets in greys only. The shader colours crystal for each layer, so this holds
/// nothing but the brightness of each facet.
fn crystal() -> Canvas {
    Canvas::paint(|x, y, _, _| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let c = voronoi(px, py, 4, 4, 1300);
        let facet = 0.66 + 0.14 * c.id;
        let edge = (1.0 - (c.d2 - c.d1) / 2.2).clamp(0.0, 1.0);
        let lit = if c.dy < 0.0 { 0.04 } else { 0.0 };
        let t = (facet + edge * 0.16 + lit).min(1.0);
        [t, t, t]
    })
}

/// Ore: clusters of nuggets set into the stone.
fn ore(base: &Canvas, colors: &[Rgb], salt: u32) -> Canvas {
    Canvas::paint(|x, y, u, v| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let c = voronoi(px, py, 5, 5, salt);
        let rich = value(u, v, 3, 3, salt + 10) > 0.42;
        if rich && c.id > 0.25 && c.d1 < 2.6 {
            let t = (1.0 - c.d1 / 2.6) * 0.6 + if c.dy < 0.0 { 0.4 } else { 0.0 };
            ramp(colors, bands(t, 4.0))
        } else if rich && c.id > 0.25 && c.d1 < 3.4 {
            shade(base.get(x as i32, y as i32), 0.68)
        } else {
            base.get(x as i32, y as i32)
        }
    })
}

fn main() {
    let (stone_tex, dirt_tex, dark_dirt_tex, grass_tex, dark_grass_tex) =
        (stone(), dirt(), dark_dirt(), grass(), dark_grass());
    let oak = [rgb(0x3E2A1B), rgb(0x523825), rgb(0x65472E), rgb(0x785738)];
    let pine = [rgb(0x1E1C1A), rgb(0x292623), rgb(0x34302B), rgb(0x403A33)];
    let textures: Vec<(&str, Canvas)> = vec![
        ("grass_side", turf_side(&grass_tex, &dirt_tex, 810)),
        (
            "dark_grass_side",
            turf_side(&dark_grass_tex, &dark_dirt_tex, 830),
        ),
        ("cobblestone", cobble()),
        ("mossy_cobblestone", mossy_cobble()),
        ("slate", slate()),
        ("basalt", basalt()),
        ("ember", ember()),
        ("ash", ash()),
        ("bedrock", bedrock()),
        ("sand", sand()),
        ("gravel", gravel()),
        ("water", water()),
        ("lava", lava()),
        ("log_side", bark(&oak, rgb(0x2A1B11), 1400)),
        (
            "log_top",
            log_end(
                &[rgb(0x5E4229), rgb(0x6B4D30), rgb(0x785737), rgb(0x85623F)],
                rgb(0x3A2819),
                1420,
            ),
        ),
        ("pine_log_side", bark(&pine, rgb(0x130D0A), 1440)),
        (
            "pine_log_top",
            log_end(
                &[rgb(0x4A3724), rgb(0x57412B), rgb(0x644B32), rgb(0x705439)],
                rgb(0x271B14),
                1460,
            ),
        ),
        ("leaves", leaves()),
        ("pine_needles", pine_needles()),
        ("crystal", crystal()),
        (
            "coal_ore",
            ore(
                &stone_tex,
                &[rgb(0x0E0E11), rgb(0x1C1C20), rgb(0x303036), rgb(0x46464D)],
                1500,
            ),
        ),
        (
            "iron_ore",
            ore(
                &stone_tex,
                &[rgb(0x8A6242), rgb(0xB08458), rgb(0xD2A777), rgb(0xE8C79C)],
                1520,
            ),
        ),
        ("stone", stone_tex),
        ("dirt", dirt_tex),
        ("dark_dirt", dark_dirt_tex),
        ("grass", grass_tex),
        ("dark_grass", dark_grass_tex),
    ];
    for (name, canvas) in &textures {
        canvas
            .image()
            .save(format!("textures/{name}_32.png"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    println!("wrote {} textures", textures.len());

    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--sheet") {
        save_sheet(&textures, &args[i + 1]);
    }
}

/// Each texture repeated three times each way, at double size.
fn save_sheet(textures: &[(&str, Canvas)], path: &str) {
    const COLS: u32 = 7;
    const SCALE: u32 = 2;
    let cell = SIZE * 3 * SCALE + 6;
    let rows = (textures.len() as u32).div_ceil(COLS);
    let mut sheet = RgbaImage::from_pixel(cell * COLS, cell * rows, Rgba([255, 0, 255, 255]));
    for (i, (_, canvas)) in textures.iter().enumerate() {
        let img = canvas.image();
        let (ox, oy) = ((i as u32 % COLS) * cell, (i as u32 / COLS) * cell);
        for y in 0..SIZE * 3 * SCALE {
            for x in 0..SIZE * 3 * SCALE {
                let p = *img.get_pixel((x / SCALE) % SIZE, (y / SCALE) % SIZE);
                sheet.put_pixel(ox + x, oy + y, p);
            }
        }
    }
    sheet.save(path).expect("save sheet");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How far the brightest and darkest 8x8 areas of a tile are from each other, as a share
    /// of its average brightness.
    fn patchiness(c: &Canvas) -> f32 {
        let mut patches = Vec::new();
        for py in 0..4 {
            for px in 0..4 {
                let mut sum = 0.0;
                for y in 0..8 {
                    for x in 0..8 {
                        let p = c.get(px * 8 + x, py * 8 + y);
                        sum += p[0] * 0.3 + p[1] * 0.6 + p[2] * 0.1;
                    }
                }
                patches.push(sum / 64.0);
            }
        }
        let max = patches.iter().copied().fold(f32::MIN, f32::max);
        let min = patches.iter().copied().fold(f32::MAX, f32::min);
        (max - min) / (patches.iter().sum::<f32>() / patches.len() as f32)
    }

    #[test]
    fn evening_out_removes_broad_shading_and_keeps_fine_detail() {
        let shaded = || {
            Canvas::paint(|x, _, u, _| {
                let broad = 0.5 + 0.25 * (std::f32::consts::TAU * u).sin();
                let fine = if x % 2 == 0 { 1.1 } else { 0.9 };
                [broad * fine; 3]
            })
        };
        let evened = shaded().even_out();
        assert!(patchiness(&shaded()) > 0.5);
        assert!(patchiness(&evened) < 0.05);
        let contrast = evened.get(8, 0)[0] / evened.get(9, 0)[0];
        assert!(contrast > 1.15, "fine detail lost: {contrast}");
    }

    #[test]
    fn ground_textures_have_no_broad_light_and_dark_areas() {
        let ground = [
            ("grass", grass()),
            ("dark_grass", dark_grass()),
            ("dirt", dirt()),
            ("dark_dirt", dark_dirt()),
            ("sand", sand()),
            ("stone", stone()),
            ("slate", slate()),
            ("ash", ash()),
        ];
        let report: Vec<String> = ground
            .iter()
            .map(|(name, tile)| format!("{name} {:.3}", patchiness(tile)))
            .collect();
        let worst = ground
            .iter()
            .map(|(_, tile)| patchiness(tile))
            .fold(0.0, f32::max);
        assert!(worst < 0.12, "too patchy: {report:?}");
    }
}
