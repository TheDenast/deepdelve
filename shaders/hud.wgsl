// Parallelograms in pixel coordinates: glyph runs, the crosshair and the hotbar's
// block icons, which are cubes drawn as three textured faces.

struct Screen {
    size: vec2<f32>,
    pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> screen: Screen;
@group(0) @binding(1) var blocks: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) layer: i32,
}

@vertex
fn vs(
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) edges: vec4<f32>,
    @location(2) tex: vec2<f32>,
    @location(3) color: vec4<f32>,
) -> VOut {
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    let p = pos + corner.x * edges.xy + corner.y * edges.zw;
    var out: VOut;
    out.clip = vec4<f32>(p.x / screen.size.x * 2.0 - 1.0, 1.0 - p.y / screen.size.y * 2.0, 0.0, 1.0);
    out.color = color;
    out.uv = corner;
    out.layer = i32(floor(tex.x));
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    // Sampled before branching, since derivatives need uniform control flow.
    let t = textureSample(blocks, block_sampler, in.uv, max(in.layer, 0));
    if in.layer < 0 {
        return in.color;
    }
    return vec4<f32>(t.rgb * in.color.rgb, t.a * in.color.a);
}
