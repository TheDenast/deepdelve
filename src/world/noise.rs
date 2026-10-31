//! Seeded gradient noise. Inputs are f64 so that sampling stays precise
//! millions of blocks from the origin; the math inside each cell is f32.

pub struct Perlin {
    perm: [u8; 512],
}

#[inline(always)]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline(always)]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline(always)]
fn grad2(hash: u8, x: f32, y: f32) -> f32 {
    match hash & 7 {
        0 => x + y,
        1 => x - y,
        2 => -x + y,
        3 => -x - y,
        4 => 1.414 * x,
        5 => -1.414 * x,
        6 => 1.414 * y,
        _ => -1.414 * y,
    }
}

#[inline(always)]
fn grad3(hash: u8, x: f32, y: f32, z: f32) -> f32 {
    let h = hash & 15;
    let u = if h < 8 { x } else { y };
    let v = if h < 4 {
        y
    } else if h == 12 || h == 14 {
        x
    } else {
        z
    };
    (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
}

impl Perlin {
    pub fn new(seed: u64) -> Self {
        let mut p = [0u8; 256];
        for (i, v) in p.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut s = seed ^ 0x9E37_79B9_7F4A_7C15;
        for i in (1..256).rev() {
            s = splitmix(s);
            let j = (s % (i as u64 + 1)) as usize;
            p.swap(i, j);
        }
        let mut perm = [0u8; 512];
        for i in 0..512 {
            perm[i] = p[i & 255];
        }
        Self { perm }
    }

    /// Roughly in [-1, 1].
    #[inline]
    pub fn n2(&self, x: f64, y: f64) -> f32 {
        let xf = x.floor();
        let yf = y.floor();
        let xi = (xf as i64 & 255) as usize;
        let yi = (yf as i64 & 255) as usize;
        let x = (x - xf) as f32;
        let y = (y - yf) as f32;
        let p = &self.perm;
        let a = p[xi] as usize + yi;
        let b = p[xi + 1] as usize + yi;
        let u = fade(x);
        let v = fade(y);
        lerp(
            lerp(grad2(p[a], x, y), grad2(p[b], x - 1.0, y), u),
            lerp(
                grad2(p[a + 1], x, y - 1.0),
                grad2(p[b + 1], x - 1.0, y - 1.0),
                u,
            ),
            v,
        ) * 0.72
    }

    /// Roughly in [-1, 1].
    #[inline]
    pub fn n3(&self, x: f64, y: f64, z: f64) -> f32 {
        let xf = x.floor();
        let yf = y.floor();
        let zf = z.floor();
        let xi = (xf as i64 & 255) as usize;
        let yi = (yf as i64 & 255) as usize;
        let zi = (zf as i64 & 255) as usize;
        let x = (x - xf) as f32;
        let y = (y - yf) as f32;
        let z = (z - zf) as f32;
        let p = &self.perm;
        let a = p[xi] as usize + yi;
        let aa = p[a] as usize + zi;
        let ab = p[a + 1] as usize + zi;
        let b = p[xi + 1] as usize + yi;
        let ba = p[b] as usize + zi;
        let bb = p[b + 1] as usize + zi;
        let u = fade(x);
        let v = fade(y);
        let w = fade(z);
        lerp(
            lerp(
                lerp(grad3(p[aa], x, y, z), grad3(p[ba], x - 1.0, y, z), u),
                lerp(
                    grad3(p[ab], x, y - 1.0, z),
                    grad3(p[bb], x - 1.0, y - 1.0, z),
                    u,
                ),
                v,
            ),
            lerp(
                lerp(
                    grad3(p[aa + 1], x, y, z - 1.0),
                    grad3(p[ba + 1], x - 1.0, y, z - 1.0),
                    u,
                ),
                lerp(
                    grad3(p[ab + 1], x, y - 1.0, z - 1.0),
                    grad3(p[bb + 1], x - 1.0, y - 1.0, z - 1.0),
                    u,
                ),
                v,
            ),
            w,
        )
    }

    /// Fractal 2D noise, normalized back to roughly [-1, 1].
    #[inline]
    pub fn fbm2(&self, x: f64, y: f64, octaves: u32) -> f32 {
        let mut sum = 0.0;
        let mut amp = 1.0;
        let mut norm = 0.0;
        let mut fx = x;
        let mut fy = y;
        for i in 0..octaves {
            // Offsetting each octave hides the lattice alignment at (0, 0).
            let off = i as f64 * 37.7;
            sum += self.n2(fx + off, fy - off) * amp;
            norm += amp;
            amp *= 0.5;
            fx *= 2.03;
            fy *= 2.03;
        }
        sum / norm
    }
}

#[inline(always)]
pub fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Fast deterministic hash of integer coordinates.
#[inline(always)]
pub fn hash3(seed: u64, x: i64, y: i64, z: i64) -> u64 {
    let mut h = seed;
    h ^= (x as u64).wrapping_mul(0xA24B_AED4_963E_E407);
    h = h.rotate_left(23).wrapping_mul(0x9FB2_1C65_1E98_DF25);
    h ^= (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h = h.rotate_left(29).wrapping_mul(0x9FB2_1C65_1E98_DF25);
    h ^= (z as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 28;
    h = h.wrapping_mul(0x9FB2_1C65_1E98_DF25);
    h ^ (h >> 32)
}

/// Maps the low bits of a hash to [0, 1).
#[inline(always)]
pub fn unit(h: u64) -> f32 {
    (h & 0xFF_FFFF) as f32 / 16_777_216.0
}

#[inline(always)]
pub fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
