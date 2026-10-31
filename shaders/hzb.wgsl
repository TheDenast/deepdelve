// Builds the depth pyramid. Depth is reversed, so "farthest" is the minimum.

@group(0) @binding(0) var depth_src: texture_depth_2d;
@group(0) @binding(1) var color_src: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<r32float, write>;

// First level: the pyramid base is a power of two no larger than the screen, so
// each texel covers up to 3x3 screen pixels when the ratio isn't exact.
@compute @workgroup_size(8, 8)
fn from_depth(@builtin(global_invocation_id) id: vec3<u32>) {
    let dst_size = textureDimensions(dst);
    if id.x >= dst_size.x || id.y >= dst_size.y {
        return;
    }
    let src_size = vec2<f32>(textureDimensions(depth_src));
    let ratio = src_size / vec2<f32>(dst_size);
    let lo = vec2<u32>(floor(vec2<f32>(id.xy) * ratio));
    let hi = min(vec2<u32>(ceil((vec2<f32>(id.xy) + 1.0) * ratio)), vec2<u32>(src_size));
    var v = 1.0;
    for (var y = lo.y; y < hi.y; y++) {
        for (var x = lo.x; x < hi.x; x++) {
            v = min(v, textureLoad(depth_src, vec2<u32>(x, y), 0));
        }
    }
    textureStore(dst, id.xy, vec4<f32>(v, 0.0, 0.0, 0.0));
}

@compute @workgroup_size(8, 8)
fn downsample(@builtin(global_invocation_id) id: vec3<u32>) {
    let dst_size = textureDimensions(dst);
    if id.x >= dst_size.x || id.y >= dst_size.y {
        return;
    }
    let p = vec2<i32>(id.xy) * 2;
    let src = vec2<i32>(textureDimensions(color_src)) - 1;
    let a = textureLoad(color_src, min(p, src), 0).r;
    let b = textureLoad(color_src, min(p + vec2<i32>(1, 0), src), 0).r;
    let c = textureLoad(color_src, min(p + vec2<i32>(0, 1), src), 0).r;
    let d = textureLoad(color_src, min(p + vec2<i32>(1, 1), src), 0).r;
    textureStore(dst, id.xy, vec4<f32>(min(min(a, b), min(c, d)), 0.0, 0.0, 0.0));
}
