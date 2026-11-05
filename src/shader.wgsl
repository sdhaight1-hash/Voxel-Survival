struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tiles: texture_2d_array<f32>;
@group(1) @binding(1) var nearest_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tile: u32,
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
    o.clip_position = camera.view_proj * vec4<f32>(v.position, 1.0);
    o.uv = v.uv;
    o.tile = v.tile;
    // Vertex shading is intentionally subtle; distinct top/side textures do most of the work.
    o.shade = 1.0;
    return o;
}
@fragment
fn fs_main(v: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(tiles, nearest_sampler, v.uv, i32(v.tile));
    return vec4<f32>(color.rgb * v.shade, 1.0);
}
