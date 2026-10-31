//! Block texture array and the tiling macro-variation noise.

use crate::world::block::TEXTURE_NAMES;
use wgpu::util::DeviceExt;

const SIZE: u32 = 32;
const MIPS: u32 = 6;

macro_rules! texture_bytes {
    ($($name:literal),* $(,)?) => {
        &[$(include_bytes!(concat!("../../textures/", $name, "_32.png"))),*]
    };
}

/// Same order as `TEXTURE_NAMES`, which `tests::names_match` checks.
static TEXTURE_FILES: &[&[u8]] = texture_bytes![
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

fn to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f32) -> u8 {
    let v = if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Box-filters in linear light, so that distant terrain keeps its brightness.
fn downsample(src: &[u8], size: u32) -> Vec<u8> {
    let half = size / 2;
    let mut out = vec![0u8; (half * half * 4) as usize];
    for y in 0..half {
        for x in 0..half {
            let mut sum = [0.0f32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = (((y * 2 + dy) * size + (x * 2 + dx)) * 4) as usize;
                for c in 0..3 {
                    sum[c] += to_linear(src[i + c]);
                }
                sum[3] += src[i + 3] as f32;
            }
            let o = ((y * half + x) * 4) as usize;
            for c in 0..3 {
                out[o + c] = to_srgb(sum[c] * 0.25);
            }
            out[o + 3] = (sum[3] * 0.25) as u8;
        }
    }
    out
}

/// Tiling value noise. R: broad brightness drift. G: a second, offset pattern.
fn macro_noise(size: u32) -> Vec<u8> {
    let hash = |x: i32, y: i32, salt: u32, period: i32| {
        let (x, y) = (x.rem_euclid(period) as u32, y.rem_euclid(period) as u32);
        let mut h = x.wrapping_mul(0x8DA6_B343)
            ^ y.wrapping_mul(0xD816_3841)
            ^ salt.wrapping_mul(0xCB1A_B31F);
        h = (h ^ (h >> 13)).wrapping_mul(0x5BD1_E995);
        h ^= h >> 15;
        (h & 0xFFFF) as f32 / 65535.0
    };
    let value = |u: f32, v: f32, period: i32, salt: u32| {
        let (x, y) = (u * period as f32, v * period as f32);
        let (xf, yf) = (x.floor(), y.floor());
        let (fx, fy) = (x - xf, y - yf);
        let s = |t: f32| t * t * (3.0 - 2.0 * t);
        let (sx, sy) = (s(fx), s(fy));
        let (xi, yi) = (xf as i32, yf as i32);
        let a = hash(xi, yi, salt, period);
        let b = hash(xi + 1, yi, salt, period);
        let c = hash(xi, yi + 1, salt, period);
        let d = hash(xi + 1, yi + 1, salt, period);
        (a + (b - a) * sx) * (1.0 - sy) + (c + (d - c) * sx) * sy
    };
    let fbm = |u: f32, v: f32, salt: u32| {
        let mut sum = 0.0;
        let mut amp = 0.5;
        let mut period = 8;
        for i in 0..6 {
            sum += value(u, v, period, salt + i) * amp;
            amp *= 0.55;
            period *= 2;
        }
        // The amplitudes sum to about 1.08; stretch contrast around the middle.
        ((sum / 1.08 - 0.5) * 2.2 + 0.5).clamp(0.0, 1.0)
    };
    let mut out = vec![0u8; (size * size * 2) as usize];
    for y in 0..size {
        for x in 0..size {
            let (u, v) = (x as f32 / size as f32, y as f32 / size as f32);
            let i = ((y * size + x) * 2) as usize;
            out[i] = (fbm(u, v, 11) * 255.0) as u8;
            out[i + 1] = (fbm(u, v, 97) * 255.0) as u8;
        }
    }
    out
}

fn downsample_rg(src: &[u8], size: u32) -> Vec<u8> {
    let half = size / 2;
    let mut out = vec![0u8; (half * half * 2) as usize];
    for y in 0..half {
        for x in 0..half {
            let o = ((y * half + x) * 2) as usize;
            for c in 0..2 {
                let mut sum = 0u32;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    sum += src[((y * 2 + dy) * size + (x * 2 + dx)) as usize * 2 + c] as u32;
                }
                out[o + c] = (sum / 4) as u8;
            }
        }
    }
    out
}

/// Returns views of the block texture array and the macro noise texture.
pub fn create(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::TextureView, wgpu::TextureView) {
    assert_eq!(TEXTURE_FILES.len(), TEXTURE_NAMES.len());
    let layers = TEXTURE_FILES.len() as u32;
    // wgpu expects layer 0 mips first with LayerMajor order.
    let mut mips_per_layer = Vec::new();
    for (bytes, name) in TEXTURE_FILES.iter().zip(TEXTURE_NAMES) {
        let img = image::load_from_memory(bytes)
            .unwrap_or_else(|e| panic!("texture {name}: {e}"))
            .to_rgba8();
        assert_eq!(
            img.dimensions(),
            (SIZE, SIZE),
            "texture {name} must be {SIZE}x{SIZE}"
        );
        let mut level = img.into_raw();
        let mut size = SIZE;
        let mut mips = vec![level.clone()];
        for _ in 1..MIPS {
            level = downsample(&level, size);
            size /= 2;
            mips.push(level.clone());
        }
        mips_per_layer.push(mips);
    }
    let mut data = Vec::new();
    for mips in &mips_per_layer {
        for level in mips {
            data.extend_from_slice(level);
        }
    }
    let tex = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("blocks"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: layers,
            },
            mip_level_count: MIPS,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &data,
    );
    let view = tex.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });

    const MACRO_SIZE: u32 = 512;
    let mut level = macro_noise(MACRO_SIZE);
    let mut size = MACRO_SIZE;
    let mut macro_data = level.clone();
    let mut mip_count = 1;
    while size > 1 {
        level = downsample_rg(&level, size);
        size /= 2;
        macro_data.extend_from_slice(&level);
        mip_count += 1;
    }
    let macro_tex = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("macro noise"),
            size: wgpu::Extent3d {
                width: MACRO_SIZE,
                height: MACRO_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &macro_data,
    );
    (view, macro_tex.create_view(&Default::default()))
}
