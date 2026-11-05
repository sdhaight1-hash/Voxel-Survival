struct Camera {
    view_proj: mat4x4<f32>,
    viewport: vec4<f32>,
    sunlight: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var hud_atlas: texture_2d<f32>;

@vertex
fn vs_hud(@builtin(vertex_index) i:u32) -> @builtin(position) vec4<f32> {
    var triangle=array<vec2<f32>,3>(
        vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0)
    );
    return vec4<f32>(triangle[i],0.0,1.0);
}

@fragment
fn fs_hud(@builtin(position) frag:vec4<f32>) -> @location(0) vec4<f32> {
    let x=frag.x;
    let y=frag.y;
    let center_left=(camera.viewport.x - 416.0)*0.5;
    let right_left=camera.viewport.x - 332.0;
    var atlas_x=-1.0;
    if x>=center_left && x<center_left+416.0 && y>=12.0 && y<136.0 {
        atlas_x=x-center_left;
    }
    // Right-hand diagnostics take precedence on narrow windows.
    if x>=right_left && x<right_left+320.0 && y>=12.0 && y<136.0 {
        atlas_x=416.0+x-right_left;
    }
    if atlas_x<0.0 {discard;}
    let color=textureLoad(hud_atlas,vec2<i32>(i32(atlas_x),i32(y-12.0)),0);
    if color.a<0.01 {discard;}
    return color;
}
