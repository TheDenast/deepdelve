// Outline of the block under the crosshair.

struct LineParams {
    view_proj: mat4x4<f32>,
    origin: vec3<f32>,
    pad: f32,
}

@group(0) @binding(0) var<uniform> params: LineParams;

@vertex
fn vs_line(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var edges = array<vec3<f32>, 24>(
        vec3<f32>(0.,0.,0.), vec3<f32>(1.,0.,0.), vec3<f32>(1.,0.,0.), vec3<f32>(1.,0.,1.),
        vec3<f32>(1.,0.,1.), vec3<f32>(0.,0.,1.), vec3<f32>(0.,0.,1.), vec3<f32>(0.,0.,0.),
        vec3<f32>(0.,1.,0.), vec3<f32>(1.,1.,0.), vec3<f32>(1.,1.,0.), vec3<f32>(1.,1.,1.),
        vec3<f32>(1.,1.,1.), vec3<f32>(0.,1.,1.), vec3<f32>(0.,1.,1.), vec3<f32>(0.,1.,0.),
        vec3<f32>(0.,0.,0.), vec3<f32>(0.,1.,0.), vec3<f32>(1.,0.,0.), vec3<f32>(1.,1.,0.),
        vec3<f32>(1.,0.,1.), vec3<f32>(1.,1.,1.), vec3<f32>(0.,0.,1.), vec3<f32>(0.,1.,1.),
    );
    let p = params.origin + (edges[vi] - 0.5) * 1.004 + 0.5;
    return params.view_proj * vec4<f32>(p, 1.0);
}

@fragment
fn fs_line() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
