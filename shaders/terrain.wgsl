// Opaque terrain and water. Quads are pulled from a storage buffer: the index
// buffer only supplies a repeating 4-vertex pattern and base_vertex selects the
// first quad, so vertex_index / 4 is the quad and vertex_index % 4 the corner.

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: camera position wrapped to the macro noise tile; w: time in seconds.
    cam_wrapped: vec4<f32>,
    // x: camera world y; y: near plane; z: fog distance; w: underwater flag.
    cam_info: vec4<f32>,
    fog_color: vec4<f32>,
    ambient: vec4<f32>,
    screen: vec4<f32>,
}

struct Draw {
    origin: vec3<f32>,
    slot: u32,
}

struct ChunkInfo {
    water_y: f32,
    scale: f32,
    tint: u32,
    pad: u32,
    light: array<u32, 32>,
}

struct Quad {
    a: u32,
    b: u32,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<storage, read> quads: array<Quad>;
@group(0) @binding(2) var<storage, read> draws: array<Draw>;
@group(0) @binding(3) var<storage, read> infos: array<ChunkInfo>;
@group(0) @binding(4) var tex: texture_2d_array<f32>;
@group(0) @binding(5) var tex_sampler: sampler;
@group(0) @binding(6) var macro_tex: texture_2d<f32>;
@group(0) @binding(7) var macro_sampler: sampler;

@group(1) @binding(0) var scene_depth: texture_depth_2d;

const MACRO_TILE: f32 = 2048.0;

var<private> NORMALS: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0),
    vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0),
);
var<private> TAN_U: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0),
);
var<private> TAN_V: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0),
);
var<private> SHADE: array<f32, 6> = array<f32, 6>(0.82, 0.72, 1.0, 0.42, 0.66, 0.76);
var<private> AO_CURVE: array<f32, 4> = array<f32, 4>(0.50, 0.68, 0.85, 1.0);

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) rel: vec3<f32>,
    @location(2) lit: f32,
    @location(3) @interpolate(flat) layer: u32,
    @location(4) @interpolate(flat) tint: vec3<f32>,
    @location(5) @interpolate(flat) water_y: f32,
    @location(6) @interpolate(flat) flags: u32,
}

fn light_byte(info_slot: u32, i: u32) -> f32 {
    let word = infos[info_slot].light[i >> 2u];
    return f32((word >> ((i & 3u) * 8u)) & 255u) / 255.0;
}

fn sample_light(slot: u32, p: vec3<f32>) -> f32 {
    let g = clamp(p / 8.0, vec3<f32>(0.0), vec3<f32>(3.999));
    let c = vec3<u32>(floor(g));
    let f = g - floor(g);
    let i = c.x + c.z * 5u + c.y * 25u;
    let c00 = mix(light_byte(slot, i), light_byte(slot, i + 1u), f.x);
    let c10 = mix(light_byte(slot, i + 5u), light_byte(slot, i + 6u), f.x);
    let c01 = mix(light_byte(slot, i + 25u), light_byte(slot, i + 26u), f.x);
    let c11 = mix(light_byte(slot, i + 30u), light_byte(slot, i + 31u), f.x);
    return mix(mix(c00, c10, f.z), mix(c01, c11, f.z), f.y);
}

fn unpack_tint(t: u32) -> vec3<f32> {
    let c = vec3<f32>(f32(t & 255u), f32((t >> 8u) & 255u), f32((t >> 16u) & 255u)) / 255.0;
    return c * c;
}

struct Decoded {
    local: vec3<f32>,
    uv: vec2<f32>,
    dir: u32,
    ao: f32,
    layer: u32,
    emissive: u32,
}

fn decode(vi: u32) -> Decoded {
    let q = quads[vi >> 2u];
    var corner = vi & 3u;
    let voxel = vec3<f32>(
        f32(q.a & 31u),
        f32((q.a >> 5u) & 31u),
        f32((q.a >> 10u) & 31u),
    );
    let w = f32((q.a >> 15u) & 31u) + 1.0;
    let h = f32((q.a >> 20u) & 31u) + 1.0;
    let dir = (q.a >> 25u) & 7u;

    let ao_bits = (q.b >> 8u) & 255u;
    let a0 = ao_bits & 3u;
    let a1 = (ao_bits >> 2u) & 3u;
    let a2 = (ao_bits >> 4u) & 3u;
    let a3 = (ao_bits >> 6u) & 3u;
    // Pick the triangle diagonal that keeps AO gradients smooth by rotating
    // which real corner each index-buffer corner refers to.
    if a0 + a3 > a1 + a2 {
        var flip = array<u32, 4>(1u, 3u, 0u, 2u);
        corner = flip[corner];
    }
    let cu = f32(corner & 1u);
    let cv = f32(corner >> 1u);

    let n = NORMALS[dir];
    var local = voxel + TAN_U[dir] * cu * w + TAN_V[dir] * cv * h;
    if (dir & 1u) == 0u {
        local += n;
    }

    var d: Decoded;
    d.local = local;
    d.dir = dir;
    d.ao = AO_CURVE[(ao_bits >> (corner * 2u)) & 3u];
    d.layer = q.b & 255u;
    d.emissive = (q.b >> 16u) & 1u;
    if dir <= 1u {
        d.uv = vec2<f32>(local.z, -local.y);
    } else if dir >= 4u {
        d.uv = vec2<f32>(local.x, -local.y);
    } else {
        d.uv = vec2<f32>(local.x, local.z);
    }
    return d;
}

@vertex
fn vs_opaque(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let draw = draws[ii];
    let info = infos[draw.slot];
    let d = decode(vi);
    let rel = draw.origin + d.local * info.scale;

    var out: VOut;
    out.clip = globals.view_proj * vec4<f32>(rel, 1.0);
    out.uv = d.uv * info.scale;
    out.rel = rel;
    out.layer = d.layer;
    out.tint = unpack_tint(info.tint);
    out.water_y = info.water_y;
    out.flags = d.dir | (d.emissive << 3u);

    if d.emissive == 1u {
        out.lit = 1.0;
    } else {
        let l = sample_light(draw.slot, d.local);
        out.lit = l * d.ao * SHADE[d.dir];
    }
    return out;
}

fn apply_fog(color: vec3<f32>, dist: f32, weight: f32) -> vec3<f32> {
    let k = dist / globals.cam_info.z;
    let f = 1.0 - exp(-k * k * weight);
    return mix(color, globals.fog_color.rgb, f);
}

@fragment
fn fs_opaque(in: VOut) -> @location(0) vec4<f32> {
    let dist = length(in.rel);
    var base = textureSample(tex, tex_sampler, in.uv, in.layer).rgb;
    let wrapped = (in.rel + globals.cam_wrapped.xyz) / MACRO_TILE;
    let m = textureSample(macro_tex, macro_sampler, wrapped.xz + wrapped.y * 0.37).rg;
    // Large-scale colour drift hides the texture repeat from far away.
    base *= 0.74 + 0.52 * m.r;
    base = mix(base, base * vec3<f32>(1.06, 0.98, 0.86), m.g);

    // Grass and leaves vary between stands, so a forest isn't one flat green.
    if in.layer == 3u || in.layer == 4u || in.layer == 9u {
        let stand = textureSample(macro_tex, macro_sampler, wrapped.xz * 21.0 + wrapped.y * 3.0).rg;
        base = mix(base * vec3<f32>(1.14, 1.02, 0.62), base * vec3<f32>(0.64, 0.98, 0.84), smoothstep(0.32, 0.68, stand.r));
        base *= 0.88 + 0.28 * stand.g;
    }

    let emissive = (in.flags >> 3u) & 1u;
    var color: vec3<f32>;
    if emissive == 1u {
        let pulse = 0.92 + 0.08 * sin(globals.cam_wrapped.w * 0.9 + m.r * 25.0);
        // Crystal takes its colour from the tier's mood, kept saturated so it reads as
        // coloured light rather than clipping to white.
        let hue = in.tint * in.tint * in.tint;
        let bright = dot(base, vec3<f32>(0.3, 0.6, 0.1));
        color = hue * bright * 4.2 * pulse;
        return vec4<f32>(apply_fog(color, dist, 0.45), 1.0);
    }

    let lit = pow(in.lit, 1.1);
    color = base * in.tint * (lit * 1.55 + 0.02);

    // Light dies quickly below the water line.
    let world_y = in.rel.y + globals.cam_info.x;
    let depth = in.water_y - world_y;
    if depth > 0.0 {
        let fade = exp(-depth * 0.022);
        color = color * mix(vec3<f32>(0.20, 0.42, 0.50), vec3<f32>(0.70, 0.92, 1.0), fade);
    }
    return vec4<f32>(apply_fog(color, dist, 1.0), 1.0);
}

struct WaterOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rel: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) lit: f32,
    @location(3) @interpolate(flat) tint: vec3<f32>,
    @location(4) @interpolate(flat) layer: u32,
}

@vertex
fn vs_water(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> WaterOut {
    let draw = draws[ii];
    let info = infos[draw.slot];
    let d = decode(vi);
    var rel = draw.origin + d.local * info.scale;
    // Coarse voxels can't land on the real water height, so top faces are
    // snapped to it. All LOD levels of a sea then share one flat plane.
    if d.dir == 2u {
        rel.y = info.water_y - globals.cam_info.x;
    }
    var out: WaterOut;
    out.clip = globals.view_proj * vec4<f32>(rel, 1.0);
    out.rel = rel;
    out.uv = d.uv * info.scale;
    out.lit = sample_light(draw.slot, d.local);
    out.tint = unpack_tint(info.tint);
    out.layer = d.layer;
    return out;
}

@fragment
fn fs_water(in: WaterOut) -> @location(0) vec4<f32> {
    let t = globals.cam_wrapped.w;
    let dist = length(in.rel);
    let world = in.rel + globals.cam_wrapped.xyz;

    // Two scrolling layers of the water texture give the moving surface.
    let uv_a = in.uv * 0.25 + vec2<f32>(t * 0.031, t * 0.017);
    let uv_b = in.uv.yx * 0.11 + vec2<f32>(-t * 0.013, t * 0.023);
    let tex_a = textureSample(tex, tex_sampler, uv_a, in.layer).rgb;
    let tex_b = textureSample(tex, tex_sampler, uv_b, in.layer).rgb;
    var water = (tex_a + tex_b) * 0.5;

    // Thickness of the water along the view ray, from the opaque scene's depth.
    let scene_d = textureLoad(scene_depth, vec2<i32>(in.clip.xy), 0);
    let near = globals.cam_info.y;
    let view_scene = near / max(scene_d, 1e-6);
    let view_water = near / max(in.clip.z, 1e-6);
    let thickness = max(view_scene - view_water, 0.0);
    let absorb = 1.0 - exp(-thickness * 0.085);

    let lit = in.lit;
    let deep = vec3<f32>(0.010, 0.060, 0.085) * in.tint;
    let shallow = water * vec3<f32>(0.26, 0.55, 0.62) * in.tint;
    var color = mix(shallow, deep, absorb) * (lit * 1.9 + 0.05);

    // Glancing angles mirror the glowing ceiling far above.
    let view_dir = normalize(in.rel);
    let ripple = textureSample(macro_tex, macro_sampler, world.xz / 19.0 + vec2<f32>(t * 0.011, -t * 0.008)).r - 0.5;
    let ndv = clamp(abs(view_dir.y) + ripple * 0.09, 0.0, 1.0);
    let fresnel = 0.03 + 0.97 * pow(1.0 - ndv, 5.0);
    let sky = globals.ambient.rgb * (0.30 + lit * 0.8) * in.tint;
    color = mix(color, sky, fresnel);

    // A faint sparkle where ripples catch the light.
    let sparkle = pow(max(ripple * 2.0, 0.0), 8.0) * lit * 0.45;
    color += in.tint * sparkle;

    let alpha = clamp(0.45 + 0.5 * absorb + fresnel * 0.6, 0.0, 1.0);
    return vec4<f32>(apply_fog(color, dist, 1.0), alpha);
}

