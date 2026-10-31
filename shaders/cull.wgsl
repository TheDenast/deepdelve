// Hierarchical-Z occlusion culling. The depth pyramid is built from the previous
// frame's depth, and chunk boxes are tested in that frame's clip space. Depth is
// reversed (1 = near, 0 = far), so each pyramid texel keeps its farthest value.

struct Candidate {
    bounds_min: vec3<f32>,
    index_count: u32,
    bounds_max: vec3<f32>,
    base_vertex: i32,
}

struct Params {
    prev_view_proj: mat4x4<f32>,
    // Add to a box in current camera space to get previous camera space.
    shift: vec3<f32>,
    count: u32,
    pyramid_size: vec2<f32>,
    mip_count: f32,
    enabled: u32,
}

struct Indirect {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

struct Stats {
    drawn: atomic<u32>,
    quads: atomic<u32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> candidates: array<Candidate>;
@group(0) @binding(2) var<storage, read_write> indirect: array<Indirect>;
@group(0) @binding(3) var pyramid: texture_2d<f32>;
@group(0) @binding(4) var<storage, read_write> stats: Stats;

fn occluded(c: Candidate) -> bool {
    var uv_min = vec2<f32>(1e9);
    var uv_max = vec2<f32>(-1e9);
    var nearest = 0.0;
    for (var i = 0u; i < 8u; i++) {
        let p = vec3<f32>(
            select(c.bounds_min.x, c.bounds_max.x, (i & 1u) != 0u),
            select(c.bounds_min.y, c.bounds_max.y, (i & 2u) != 0u),
            select(c.bounds_min.z, c.bounds_max.z, (i & 4u) != 0u),
        ) + params.shift;
        let clip = params.prev_view_proj * vec4<f32>(p, 1.0);
        // A corner at or behind the camera can't be judged, so the box is drawn.
        if clip.w <= 0.2 {
            return false;
        }
        let ndc = clip.xyz / clip.w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        uv_min = min(uv_min, uv);
        uv_max = max(uv_max, uv);
        nearest = max(nearest, ndc.z);
    }
    // The previous frame didn't see outside its own screen, so it knows nothing.
    if uv_min.x <= 0.0 || uv_min.y <= 0.0 || uv_max.x >= 1.0 || uv_max.y >= 1.0 {
        return false;
    }
    let size = (uv_max - uv_min) * params.pyramid_size;
    let level = ceil(log2(max(max(size.x, size.y), 1.0)));
    let mip = i32(clamp(level, 0.0, params.mip_count - 1.0));
    let dim = vec2<i32>(textureDimensions(pyramid, mip));
    let a = clamp(vec2<i32>(uv_min * vec2<f32>(dim)), vec2<i32>(0), dim - 1);
    let b = clamp(vec2<i32>(uv_max * vec2<f32>(dim)), vec2<i32>(0), dim - 1);
    var farthest = 1.0;
    // The mip level is picked so the box spans at most two texels per axis.
    for (var y = a.y; y <= b.y; y++) {
        for (var x = a.x; x <= b.x; x++) {
            farthest = min(farthest, textureLoad(pyramid, vec2<i32>(x, y), mip).r);
        }
    }
    return nearest < farthest;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.count {
        return;
    }
    let c = candidates[i];
    var out: Indirect;
    out.index_count = c.index_count;
    out.first_index = 0u;
    out.base_vertex = c.base_vertex;
    out.first_instance = i;
    out.instance_count = 1u;
    if params.enabled != 0u && occluded(c) {
        out.instance_count = 0u;
    } else {
        atomicAdd(&stats.drawn, 1u);
        atomicAdd(&stats.quads, c.index_count / 6u);
    }
    indirect[i] = out;
}
