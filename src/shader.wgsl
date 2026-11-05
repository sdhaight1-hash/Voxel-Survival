struct Camera {
    view_proj: mat4x4<f32>,
    viewport: vec4<f32>,
    sunlight: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tiles: texture_2d_array<f32>;
@group(1) @binding(1) var nearest_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tile: u32,
    @location(3) chunk_offset: vec2<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) tile: u32,
    @location(2) shade: f32,
};
@vertex
fn vs_main(v: VertexInput) -> VertexOutput {
    var o: VertexOutput;
    o.clip_position = camera.view_proj * vec4<f32>(v.position.xy + v.chunk_offset, v.position.z, 1.0);
    o.uv = v.uv;
    o.tile = v.tile;
    // Vertex shading is intentionally subtle; distinct top/side textures do most of the work.
    o.shade = 1.0;
    return o;
}
@fragment
fn fs_main(v: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(tiles, nearest_sampler, v.uv, i32(v.tile));
    return vec4<f32>(color.rgb * v.shade * (0.22 + 0.78 * camera.sunlight.x), 1.0);
}

@fragment
fn fs_water(v: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(tiles, nearest_sampler, v.uv, i32(v.tile));
    return vec4<f32>(color.rgb * (0.25 + 0.75 * camera.sunlight.x), 0.58);
}

@vertex
fn vs_overlay(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var triangle = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0)
    );
    return vec4<f32>(triangle[idx], 0.0, 1.0);
}
@fragment
fn fs_crosshair(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    let d = abs(pixel.xy - camera.viewport.xy * 0.5);
    let arm = (d.x < 1.25 && d.y < 8.0) || (d.y < 1.25 && d.x < 8.0);
    if (!arm) { discard; }
    return vec4<f32>(0.98, 0.98, 0.92, 1.0);
}
