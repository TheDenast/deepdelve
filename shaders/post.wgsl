// Bloom chain and the final composite (bloom + filmic tonemap).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var bloom: texture_2d<f32>;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_fullscreen(@builtin(vertex_index) vi: u32) -> VOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VOut;
    out.clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn downsample13(uv: vec2<f32>) -> vec3<f32> {
    let t = 1.0 / vec2<f32>(textureDimensions(src));
    let s = t;
    let a = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(-2.0, -2.0), 0.0).rgb;
    let b = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(0.0, -2.0), 0.0).rgb;
    let c = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(2.0, -2.0), 0.0).rgb;
    let d = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(-2.0, 0.0), 0.0).rgb;
    let e = textureSampleLevel(src, src_sampler, uv, 0.0).rgb;
    let f = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(2.0, 0.0), 0.0).rgb;
    let g = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(-2.0, 2.0), 0.0).rgb;
    let h = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(0.0, 2.0), 0.0).rgb;
    let i = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(2.0, 2.0), 0.0).rgb;
    let j = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(-1.0, -1.0), 0.0).rgb;
    let k = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(1.0, -1.0), 0.0).rgb;
    let l = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(-1.0, 1.0), 0.0).rgb;
    let m = textureSampleLevel(src, src_sampler, uv + s * vec2<f32>(1.0, 1.0), 0.0).rgb;
    return e * 0.125 + (a + c + g + i) * 0.03125 + (b + d + f + h) * 0.0625 + (j + k + l + m) * 0.125;
}

@fragment
fn fs_bright(in: VOut) -> @location(0) vec4<f32> {
    let c = downsample13(in.uv);
    let l = max(c.r, max(c.g, c.b));
    let k = smoothstep(0.7, 1.8, l);
    return vec4<f32>(min(c * k, vec3<f32>(24.0)), 1.0);
}

@fragment
fn fs_down(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(downsample13(in.uv), 1.0);
}

@fragment
fn fs_up(in: VOut) -> @location(0) vec4<f32> {
    let t = 1.0 / vec2<f32>(textureDimensions(src));
    var c = textureSampleLevel(src, src_sampler, in.uv, 0.0).rgb * 4.0;
    c += textureSampleLevel(src, src_sampler, in.uv + t * vec2<f32>(-1.0, 0.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(src, src_sampler, in.uv + t * vec2<f32>(1.0, 0.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(src, src_sampler, in.uv + t * vec2<f32>(0.0, -1.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(src, src_sampler, in.uv + t * vec2<f32>(0.0, 1.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(src, src_sampler, in.uv + t * vec2<f32>(-1.0, -1.0), 0.0).rgb;
    c += textureSampleLevel(src, src_sampler, in.uv + t * vec2<f32>(1.0, -1.0), 0.0).rgb;
    c += textureSampleLevel(src, src_sampler, in.uv + t * vec2<f32>(-1.0, 1.0), 0.0).rgb;
    c += textureSampleLevel(src, src_sampler, in.uv + t * vec2<f32>(1.0, 1.0), 0.0).rgb;
    return vec4<f32>(c / 16.0, 1.0);
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_composite(in: VOut) -> @location(0) vec4<f32> {
    let hdr = textureSampleLevel(src, src_sampler, in.uv, 0.0).rgb;
    let glow = textureSampleLevel(bloom, src_sampler, in.uv, 0.0).rgb;
    var c = hdr + glow * 0.42;
    c = aces(c * 0.9);
    return vec4<f32>(c, 1.0);
}
