mod mesh;
mod terrain;
mod hydrology;
mod soil;
mod atmosphere;
mod geoscience;
mod hud;

use std::{collections::{HashMap,HashSet}, sync::Arc, time::Instant};
use bytemuck::{Pod, Zeroable};
use glam::{DVec3, Mat4, Vec3};
use terrain::{Block, Terrain, WORLD_SIZE, WORLD_HEIGHT};
use wgpu::util::DeviceExt;
use winit::{
    dpi::PhysicalSize,
    event::{DeviceEvent, ElementState, Event, MouseButton, WindowEvent},
    event_loop::EventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorGrabMode, WindowBuilder},
};

const VIEW_RADIUS: i32 = 3;
const PLAYER_HEIGHT: f64 = 1.3;
const PLAYER_RADIUS: f64 = 0.28;
const EYE_HEIGHT: f64 = 1.18;
const JUMP_SPEED: f64 = 7.5;
const GRAVITY: f64 = 20.0;
const MOUSE_SENSITIVITY: f32 = 0.002;

// Lab-only calendar acceleration. Surface-flow and player physics keep their
// existing stable fixed steps; climatic processes use scaled elapsed time.
const LAB_TIME_SCALES: [u32; 4] = [1, 10, 100, 1000];
fn next_lab_time_scale(current: u32) -> u32 {
    let i = LAB_TIME_SCALES.iter().position(|&speed| speed == current).unwrap_or(0);
    LAB_TIME_SCALES[(i + 1) % LAB_TIME_SCALES.len()]
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    viewport: [f32; 4],
    sunlight: [f32; 4],
}

struct Player {
    pos: DVec3, // precise world meters, feet; Z up; X/Y wrap
    velocity_z: f64,
    yaw: f32,
    pitch: f32,
    grounded: bool,
}
impl Player {
    fn forward(&self) -> Vec3 {
        Vec3::new(self.yaw.cos() * self.pitch.cos(), self.yaw.sin() * self.pitch.cos(), self.pitch.sin())
    }
    fn eye(&self) -> DVec3 { self.pos + DVec3::Z * EYE_HEIGHT }
}

// Player is a continuous AABB; voxel coordinates are integer unit cubes.
// The epsilon avoids treating a mere face-to-face touch as penetration.
const COLLISION_EPS: f64 = 0.0001;
const GROUND_PROBE: f64 = 0.025;
const PHYSICS_DT: f32 = 1.0 / 120.0;

fn collides(t: &Terrain, pos: DVec3) -> bool {
    let min_x = (pos.x - PLAYER_RADIUS + COLLISION_EPS).floor() as i32;
    let max_x = (pos.x + PLAYER_RADIUS - COLLISION_EPS).floor() as i32;
    let min_y = (pos.y - PLAYER_RADIUS + COLLISION_EPS).floor() as i32;
    let max_y = (pos.y + PLAYER_RADIUS - COLLISION_EPS).floor() as i32;
    let min_z = (pos.z + COLLISION_EPS).floor() as i32;
    let max_z = (pos.z + PLAYER_HEIGHT - COLLISION_EPS).floor() as i32;
    for z in min_z..=max_z {
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                if t.block(x, y, z).solid() { return true; }
            }
        }
    }
    false
}
fn wrap_pos(v: f64) -> f64 { v.rem_euclid(WORLD_SIZE as f64) }

fn support_below(t: &Terrain, p: &Player) -> bool {
    collides(t, p.pos - DVec3::Z * GROUND_PROBE)
}
fn horizontal_direction(p: &Player, keys: &HashSet<KeyCode>) -> DVec3 {
    let yaw = p.yaw as f64;
    let forward = DVec3::new(yaw.cos(), yaw.sin(), 0.0);
    let right = DVec3::new(yaw.sin(), -yaw.cos(), 0.0);
    let mut v = DVec3::ZERO;
    if keys.contains(&KeyCode::KeyW) { v += forward; }
    if keys.contains(&KeyCode::KeyS) { v -= forward; }
    if keys.contains(&KeyCode::KeyD) { v += right; }
    if keys.contains(&KeyCode::KeyA) { v -= right; }
    if v.length_squared() > 0.0 { v.normalize() } else { v }
}

// Called at a fixed 120 Hz, never with the display frame duration.
fn update_player(p: &mut Player, t: &Terrain, keys: &HashSet<KeyCode>, dt: f32) {
    let dt = dt as f64;
    let supported = support_below(t, p);
    if p.velocity_z <= 0.0 && supported {
        p.grounded = true;
        p.velocity_z = 0.0;
    } else if p.grounded && !supported {
        p.grounded = false;
    }

    if p.grounded && keys.contains(&KeyCode::Space) {
        p.velocity_z = JUMP_SPEED;
        p.grounded = false;
    }
    if !p.grounded {
        let old_velocity = p.velocity_z;
        p.velocity_z = (p.velocity_z - GRAVITY * dt).max(-35.0);
        let travel = (old_velocity + p.velocity_z) * 0.5 * dt;
        let steps = (travel.abs() / 0.06).ceil().max(1.0) as usize;
        for _ in 0..steps {
            let candidate = p.pos + DVec3::Z * (travel / steps as f64);
            if collides(t, candidate) {
                // Small substeps keep the player at their last nonpenetrating
                // position; no floor(z)+1 teleport / forced camera movement.
                if travel < 0.0 {
                    // The collided surface is the highest occupied block under
                    // the feet. Snap only if it is within this small travel step.
                    let top = candidate.z.floor() + 1.0;
                    if top >= p.pos.z - 0.07 && top <= p.pos.z + 0.07 {
                        p.pos.z = top;
                    }
                    p.grounded = true;
                }
                p.velocity_z = 0.0;
                break;
            }
            p.pos = candidate;
        }
    }

    let travel = horizontal_direction(p, keys) * 5.0 * dt;
    let steps = (travel.length() / 0.08).ceil().max(1.0) as usize;
    for _ in 0..steps {
        let step = travel / steps as f64;
        // Independent axes permit sliding along walls.
        let x = DVec3::new(wrap_pos(p.pos.x + step.x), p.pos.y, p.pos.z);
        if !collides(t, x) { p.pos = x; }
        let y = DVec3::new(p.pos.x, wrap_pos(p.pos.y + step.y), p.pos.z);
        if !collides(t, y) { p.pos = y; }
    }
    // A ledge can be crossed during horizontal movement; it is no longer
    // supported, but gravity starts on the next fixed simulation step.
    if p.grounded && !support_below(t, p) { p.grounded = false; }
    p.pos.z = p.pos.z.clamp(0.0, (WORLD_HEIGHT - 2) as f64);
}

// Spawn on a *tested* surface, never at the edge of a cliff or inside a
// higher neighboring column. The player's feet are at the exact block top.
fn spawn_is_safe(t: &Terrain, position: DVec3) -> bool {
    if collides(t, position)
        || !collides(t, position - DVec3::Z * GROUND_PROBE)
        || t.block(position.x.floor() as i32, position.y.floor() as i32,
                   (position.z + EYE_HEIGHT).floor() as i32).solid() {
        return false;
    }
    // Give the camera a clear 2m radius. The old algorithm only measured
    // the center column; nearby higher terrain caused spawn clipping and
    // exposed invisible terrain interiors when looking around.
    let x=position.x.floor() as i32;
    let y=position.y.floor() as i32;
    for dy in -2..=2 {
        for dx in -2..=2 {
            // Query actual blocks, not just procedural elevation. This also
            // catches a later block edit beside the intended spawning area.
            for z in position.z.floor() as i32..=(position.z + PLAYER_HEIGHT + 0.5).floor() as i32 {
                if t.block(x+dx,y+dy,z).solid() {return false;}
            }
        }
    }
    true
}

fn find_safe_spawn(t: &Terrain) -> Player {
    let center = WORLD_SIZE / 2;
    // Search by expanding rings to avoid bias toward one direction.
    // 4m spacing covers a 512m square while retaining local flat clearings.
    for radius in 0i32..=64 {
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx.abs()!=radius && dy.abs()!=radius {continue;}
                let x=center+dx*4;
                let y=center+dy*4;
                let h=t.height(x,y);
                if h < terrain::SEA_LEVEL+3 || h+PLAYER_HEIGHT.ceil() as i32 >= WORLD_HEIGHT {
                    continue;
                }
                let pos=DVec3::new(x as f64+0.5,y as f64+0.5,h as f64+1.0);
                if spawn_is_safe(t,pos) {
                    return Player {
                        pos,velocity_z:0.0,yaw:0.3,pitch:-0.2,grounded:true,
                    };
                }
            }
        }
    }
    panic!("No collision-free dry spawn found within 256m of map center");
}

// A block can be placed in water, but never into the player's own AABB.
// Use toroidal deltas so this remains correct across both map seams.
fn player_overlaps_cell(p:&Player, cell:(i32,i32,i32)) -> bool {
    let fx=p.pos.x.fract();
    let fy=p.pos.y.fract();
    let dx=terrain::delta(cell.0,p.pos.x.floor() as i32) as f64;
    let dy=terrain::delta(cell.1,p.pos.y.floor() as i32) as f64;
    dx < fx + PLAYER_RADIUS-COLLISION_EPS
        && dx+1.0 > fx-PLAYER_RADIUS+COLLISION_EPS
        && dy < fy+PLAYER_RADIUS-COLLISION_EPS
        && dy+1.0 > fy-PLAYER_RADIUS+COLLISION_EPS
        && (cell.2 as f64) < p.pos.z+PLAYER_HEIGHT-COLLISION_EPS
        && ((cell.2+1) as f64) > p.pos.z+COLLISION_EPS
}

// Return targeted solid block and the preceding empty cell.
fn target_block(t: &Terrain, p: &Player) -> Option<((i32,i32,i32), (i32,i32,i32))> {
    let start = p.eye();
    let direction = p.forward().as_dvec3();
    let mut previous = None;
    for i in 1..=120 {
        let q = start + direction * (i as f64 * 0.05);
        let cell = (q.x.floor() as i32, q.y.floor() as i32, q.z.floor() as i32);
        if previous == Some(cell) { continue; }
        if t.block(cell.0, cell.1, cell.2).solid() {
            return previous.map(|empty| (cell, empty));
        }
        previous = Some(cell);
    }
    None
}

// When no solid block is in reach, right click can still place a solid
// directly into a visible water voxel. Check the actual partial water level,
// not just the cell's occupancy classification.
fn target_water(t:&Terrain,p:&Player) -> Option<(i32,i32,i32)> {
    let start=p.eye();
    let forward=p.forward().as_dvec3();
    for i in 1..=120 {
        let q=start+forward*(i as f64*0.05);
        let cell=(q.x.floor() as i32,q.y.floor() as i32,q.z.floor() as i32);
        if t.block(cell.0,cell.1,cell.2)==Block::Water
            && q.z.fract()<t.water_liters(cell.0,cell.1,cell.2) as f64/1000.0 {
            return Some(cell);
        }
        if t.block(cell.0,cell.1,cell.2).solid() {break;}
    }
    None
}

fn pixel_noise(x: u32, y: u32, tile: u32) -> u32 {
    let mut n = x.wrapping_mul(374761393)
        ^ y.wrapping_mul(668265263) ^ tile.wrapping_mul(2246822519) ^ 0xA341316C;
    n ^= n >> 13;
    n = n.wrapping_mul(1274126177);
    n ^ (n >> 16)
}
fn texture_pixels() -> Vec<u8> {
    let mut pixels = Vec::with_capacity(16 * 16 * 5 * 4);
    for tile in 0..5 {
        for y in 0..16 {
            for x in 0..16 {
                let noise = (pixel_noise(x,y,tile) % 43) as i32 - 21;
                let n = |c: i32| (c + noise).clamp(0,255) as u8;
                let color = match tile {
                    0 => [n(116), n(76), n(43)], // dirt: warm earthy brown
                    1 => [n(79), n(115), n(45)], // grass top: moss and olive
                    2 if y < 4 + (pixel_noise(x,0,tile) % 3) => [n(72),n(110),n(42)],
                    2 => [n(115),n(78),n(45)], // grass side: turf over soil
                    3 => [n(111),n(111),n(106)], // stone: cool gray flecks
                    _ => [n(41),n(78),n(124)], // water
                };
                pixels.extend_from_slice(&[color[0],color[1],color[2],255]);
            }
        }
    }
    pixels
}

struct ChunkGpu {
    origin: (i32,i32),
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    opaque_count: u32,
    water_vertices: wgpu::Buffer,
    water_indices: wgpu::Buffer,
    water_count: u32,
}

struct Game {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    water_pipeline: wgpu::RenderPipeline,
    crosshair_pipeline: wgpu::RenderPipeline,
    hud_pipeline: wgpu::RenderPipeline,
    hud_texture: wgpu::Texture,
    hud_bind: wgpu::BindGroup,
    depth: wgpu::TextureView,
    camera_buffer: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    texture_bind: wgpu::BindGroup,
    chunks: Vec<ChunkGpu>,
    // Changed water faces can touch many chunks in one simulation tick.
    // Coalesce those changes and remesh a fixed number per render frame.
    pending_mesh_chunks: HashSet<(i32,i32)>,
    mesh_refresh_cursor: usize,
    chunk_offsets: wgpu::Buffer,
    mesh_origin: (i32, i32),
    terrain: Terrain,
    atmosphere: atmosphere::Atmosphere,
    player: Player,
    selected: Block,
    keys: HashSet<KeyCode>,
    mouse_captured: bool,
    last_frame: Instant,
    hud_frame_seconds: f32,
    hud_elapsed: f32,
    hud_frames: u32,
    hud_fps: f32,
    hud_frame_ms: f32,
    physics_accumulator: f32,
    water_accumulator: f32,
    soil_accumulator: f32,
    soil_climate_seconds: f64,
    world_time_seconds: f64,
    time_scale: u32,
    surface_temperature: geoscience::ThermalCell,
    sunlight: f32,
}

fn depth_view(device: &wgpu::Device, size: PhysicalSize<u32>) -> wgpu::TextureView {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d { width: size.width.max(1), height: size.height.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth24Plus,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    }).create_view(&Default::default())
}

impl Game {
    async fn new(window: Arc<winit::window::Window>) -> Self {
        let startup_clock=Instant::now();
        // Explicit Vulkan backend as requested; no OpenGL/DX12 fallback.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN, ..Default::default()
        });
        let surface = instance.create_surface(window.clone()).expect("Vulkan surface");
        let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }).await.expect("A Vulkan-capable GPU and driver are required");
        let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("voxel GPU"), required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
        }, None).await.expect("Create GPU device");
        eprintln!("hydrology startup: Vulkan device ready in {:.1}s",
            startup_clock.elapsed().as_secs_f64());
        let size = window.inner_size();
        let mut config = surface.get_default_config(&adapter,size.width.max(1),size.height.max(1))
            .expect("Surface configuration");
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("block shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera uniform"),
            contents: bytemuck::bytes_of(&CameraUniform {
                view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                viewport: [size.width as f32, size.height as f32, 0.0, 0.0],
                sunlight: [1.0,0.0,0.0,0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None,
                }, count: None,
            }],
        });
        let camera_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind"), layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: camera_buffer.as_entire_binding() }],
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("original 16x16 pixel textures"),
            size: wgpu::Extent3d { width:16, height:16, depth_or_array_layers:5 },
            mip_level_count:1, sample_count:1, dimension:wgpu::TextureDimension::D2,
            format:wgpu::TextureFormat::Rgba8UnormSrgb,
            usage:wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats:&[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture { texture:&texture, mip_level:0, origin:wgpu::Origin3d::ZERO, aspect:wgpu::TextureAspect::All },
            &texture_pixels(),
            wgpu::ImageDataLayout { offset:0, bytes_per_row:Some(64), rows_per_image:Some(16) },
            wgpu::Extent3d {width:16,height:16,depth_or_array_layers:5},
        );
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter:wgpu::FilterMode::Nearest, min_filter:wgpu::FilterMode::Nearest,
            mipmap_filter:wgpu::FilterMode::Nearest, ..Default::default()
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label:Some("textures layout"),
            entries:&[
                wgpu::BindGroupLayoutEntry {
                    binding:0, visibility:wgpu::ShaderStages::FRAGMENT,
                    ty:wgpu::BindingType::Texture {
                        sample_type:wgpu::TextureSampleType::Float {filterable:true},
                        view_dimension:wgpu::TextureViewDimension::D2Array, multisampled:false,
                    }, count:None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:1, visibility:wgpu::ShaderStages::FRAGMENT,
                    ty:wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count:None,
                },
            ],
        });
        let texture_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:Some("textures bind"), layout:&texture_layout,
            entries:&[
                wgpu::BindGroupEntry {binding:0,resource:wgpu::BindingResource::TextureView(&texture_view)},
                wgpu::BindGroupEntry {binding:1,resource:wgpu::BindingResource::Sampler(&sampler)},
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:Some("render layout"), bind_group_layouts:&[&camera_layout,&texture_layout],
            push_constant_ranges:&[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:Some("voxel pipeline"), layout:Some(&layout),
            vertex:wgpu::VertexState {
                module:&shader, entry_point:"vs_main", buffers:&[mesh::Vertex::layout(),mesh::ChunkOffset::layout()],
                compilation_options:Default::default(),
            },
            fragment:Some(wgpu::FragmentState {
                module:&shader,entry_point:"fs_main",
                targets:&[Some(wgpu::ColorTargetState {
                    format:config.format, blend:None, write_mask:wgpu::ColorWrites::ALL,
                })],compilation_options:Default::default(),
            }),
            primitive:wgpu::PrimitiveState {cull_mode:None,..Default::default()},
            depth_stencil:Some(wgpu::DepthStencilState {
                format:wgpu::TextureFormat::Depth24Plus, depth_write_enabled:true,
                depth_compare:wgpu::CompareFunction::Less,
                stencil:Default::default(), bias:Default::default(),
            }),
            multisample:Default::default(), multiview:None,
        });
        let water_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:Some("blended water"), layout:Some(&layout),
            vertex:wgpu::VertexState {
                module:&shader, entry_point:"vs_main", buffers:&[mesh::Vertex::layout(),mesh::ChunkOffset::layout()],
                compilation_options:Default::default(),
            },
            fragment:Some(wgpu::FragmentState {
                module:&shader,entry_point:"fs_water",
                targets:&[Some(wgpu::ColorTargetState {
                    format:config.format, blend:Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask:wgpu::ColorWrites::ALL,
                })],compilation_options:Default::default(),
            }),
            primitive:wgpu::PrimitiveState {cull_mode:None,..Default::default()},
            depth_stencil:Some(wgpu::DepthStencilState {
                format:wgpu::TextureFormat::Depth24Plus, depth_write_enabled:false,
                depth_compare:wgpu::CompareFunction::Less,
                stencil:Default::default(), bias:Default::default(),
            }),
            multisample:Default::default(),multiview:None,
        });
        let crosshair_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:Some("crosshair layout"),bind_group_layouts:&[&camera_layout],
            push_constant_ranges:&[],
        });
        let crosshair_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:Some("crosshair"),layout:Some(&crosshair_layout),
            vertex:wgpu::VertexState {
                module:&shader,entry_point:"vs_overlay",buffers:&[],
                compilation_options:Default::default(),
            },
            fragment:Some(wgpu::FragmentState {
                module:&shader,entry_point:"fs_crosshair",
                targets:&[Some(wgpu::ColorTargetState {
                    format:config.format, blend:Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask:wgpu::ColorWrites::ALL,
                })],compilation_options:Default::default(),
            }),
            primitive:Default::default(),
            // This pipeline shares the terrain render pass, which has a depth attachment.
            // Match its format without reading or modifying terrain depth.
            depth_stencil:Some(wgpu::DepthStencilState {
                format:wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled:false,
                depth_compare:wgpu::CompareFunction::Always,
                stencil:Default::default(),
                bias:Default::default(),
            }),
            multisample:Default::default(),multiview:None,
        });
        let hud_texture = device.create_texture(&wgpu::TextureDescriptor {
            label:Some("HUD RGBA atlas"),
            size:wgpu::Extent3d {width:hud::WIDTH,height:hud::HEIGHT,depth_or_array_layers:1},
            mip_level_count:1,sample_count:1,
            dimension:wgpu::TextureDimension::D2,
            format:wgpu::TextureFormat::Rgba8Unorm,
            usage:wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats:&[],
        });
        let hud_view=hud_texture.create_view(&Default::default());
        let hud_layout=device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label:Some("hud texture layout"),
            entries:&[wgpu::BindGroupLayoutEntry {
                binding:0,
                visibility:wgpu::ShaderStages::FRAGMENT,
                ty:wgpu::BindingType::Texture {
                    sample_type:wgpu::TextureSampleType::Float {filterable:false},
                    view_dimension:wgpu::TextureViewDimension::D2,
                    multisampled:false,
                },
                count:None,
            }],
        });
        let hud_bind=device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:Some("hud texture bind"),layout:&hud_layout,
            entries:&[wgpu::BindGroupEntry {
                binding:0,resource:wgpu::BindingResource::TextureView(&hud_view)
            }],
        });
        let hud_shader=device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:Some("hud shader"),
            source:wgpu::ShaderSource::Wgsl(include_str!("hud.wgsl").into()),
        });
        let hud_pipeline_layout=device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:Some("hud pipeline layout"),
            bind_group_layouts:&[&camera_layout,&hud_layout],
            push_constant_ranges:&[],
        });
        let hud_pipeline=device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:Some("diagnostic and compass hud"),
            layout:Some(&hud_pipeline_layout),
            vertex:wgpu::VertexState {
                module:&hud_shader,entry_point:"vs_hud",buffers:&[],
                compilation_options:Default::default(),
            },
            fragment:Some(wgpu::FragmentState {
                module:&hud_shader,entry_point:"fs_hud",
                targets:&[Some(wgpu::ColorTargetState {
                    format:config.format,blend:Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask:wgpu::ColorWrites::ALL,
                })],compilation_options:Default::default(),
            }),
            primitive:Default::default(),
            depth_stencil:Some(wgpu::DepthStencilState {
                format:wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled:false,
                depth_compare:wgpu::CompareFunction::Always,
                stencil:Default::default(),bias:Default::default(),
            }),
            multisample:Default::default(),multiview:None,
        });
        let terrain = Terrain::new();
        let player = find_safe_spawn(&terrain);
        let depth = depth_view(&device,size);
        let chunk_offsets = device.create_buffer(&wgpu::BufferDescriptor {
            label:Some("chunk offset placeholder"),size:8,
            usage:wgpu::BufferUsages::VERTEX,mapped_at_creation:false,
        });
        let mut game = Self {
            surface,device,queue,config,pipeline,water_pipeline,crosshair_pipeline,
            hud_pipeline,hud_texture,hud_bind,
            depth,
            camera_buffer,camera_bind,texture_bind,
            chunks:Vec::new(),chunk_offsets,
            pending_mesh_chunks:HashSet::new(),mesh_refresh_cursor:0,
            mesh_origin:(0,0),terrain,
            atmosphere:atmosphere::Atmosphere::new_af(),
            player,selected:Block::Dirt,
            keys:HashSet::new(),mouse_captured:false,last_frame:Instant::now(),
            hud_frame_seconds:0.0,hud_elapsed:0.0,hud_frames:0,hud_fps:0.0,hud_frame_ms:0.0,
            physics_accumulator:0.0,water_accumulator:0.0,
            soil_accumulator:0.0,soil_climate_seconds:0.0,
            world_time_seconds:0.0,time_scale:1,
            surface_temperature:geoscience::ThermalCell {temperature_c:15.0},
            sunlight:1.0,
        };
        let mesh_clock=Instant::now();
        game.rebuild();
        eprintln!("hydrology startup: initial chunk meshes {:.0}ms; total {:.1}s",
            mesh_clock.elapsed().as_secs_f64()*1000.0,
            startup_clock.elapsed().as_secs_f64());
        game.upload_hud();
        game
    }

    /// Dynamic liquid occupies separate GPU buffers. A changing water level
    /// must never force rebuilding static stone/soil faces in the same chunk.
    fn water_buffers(&self, mesh:&mesh::Mesh)->(wgpu::Buffer,wgpu::Buffer,u32) {
        let vertices=if mesh.vertices.is_empty() {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label:Some("empty chunk water vertices"),
                size:std::mem::size_of::<mesh::Vertex>() as u64,
                usage:wgpu::BufferUsages::VERTEX,
                mapped_at_creation:false,
            })
        } else {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label:Some("chunk water vertices"),
                contents:bytemuck::cast_slice(&mesh.vertices),
                usage:wgpu::BufferUsages::VERTEX,
            })
        };
        let indices=if mesh.indices.is_empty() {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label:Some("empty chunk water indices"),size:4,
                usage:wgpu::BufferUsages::INDEX,mapped_at_creation:false,
            })
        } else {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label:Some("chunk water indices"),
                contents:bytemuck::cast_slice(&mesh.indices),
                usage:wgpu::BufferUsages::INDEX,
            })
        };
        (vertices,indices,mesh.indices.len() as u32)
    }

    fn gpu_chunk(&self,x:i32,y:i32) -> ChunkGpu {
        let solid=mesh::build(&self.terrain,x,y,0);
        let water=mesh::build_water(&self.terrain,x,y,0);
        let (water_vertices,water_indices,water_count)=self.water_buffers(&water);
        ChunkGpu {
            origin:(terrain::wrap(x),terrain::wrap(y)),
            opaque_count:solid.water_index_start,
            vertices:self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label:Some("chunk opaque vertices"),
                contents:bytemuck::cast_slice(&solid.vertices),
                usage:wgpu::BufferUsages::VERTEX,
            }),
            indices:self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label:Some("chunk opaque indices"),
                contents:bytemuck::cast_slice(&solid.indices),
                usage:wgpu::BufferUsages::INDEX,
            }),
            water_vertices,water_indices,water_count,
        }
    }

    /// Solid terrain edits are infrequent and may change opaque topology.
    /// Unlike climate-driven liquid changes, those require a full rebuild.
    fn refresh_solid_edit_chunks(&mut self,changed:&HashSet<(i32,i32,i32)>) {
        let affected=Self::changed_chunk_origins(changed);
        let visible:Vec<_>=self.chunks.iter().enumerate()
            .filter(|(_,chunk)|affected.contains(&chunk.origin))
            .map(|(i,chunk)|(i,chunk.origin)).collect();
        for (i,(x,y)) in visible {
            self.chunks[i]=self.gpu_chunk(x,y);
            self.pending_mesh_chunks.remove(&(x,y));
        }
    }

    // Keeps already generated chunks and builds only the new edge when crossing
    // a 32m chunk boundary (7 instead of 49 chunk columns in the common case).
    fn rebuild(&mut self) {
        let cx=(self.player.pos.x.floor() as i32).div_euclid(32)*32;
        let cy=(self.player.pos.y.floor() as i32).div_euclid(32)*32;
        self.mesh_origin=(cx,cy);
        let mut previous:HashMap<(i32,i32),ChunkGpu>=
            std::mem::take(&mut self.chunks).into_iter()
                .map(|chunk|(chunk.origin,chunk)).collect();
        for dy in -VIEW_RADIUS..=VIEW_RADIUS {
            for dx in -VIEW_RADIUS..=VIEW_RADIUS {
                let origin=(terrain::wrap(cx+dx*32),terrain::wrap(cy+dy*32));
                let chunk=match previous.remove(&origin) {
                    Some(chunk)=>chunk,
                    None=>self.gpu_chunk(origin.0,origin.1),
                };
                self.chunks.push(chunk);
            }
        }
        let offsets:Vec<mesh::ChunkOffset>=self.chunks.iter().map(|chunk| {
            mesh::ChunkOffset {xy:[
                terrain::delta(chunk.origin.0,cx) as f32,
                terrain::delta(chunk.origin.1,cy) as f32,
            ]}
        }).collect();
        self.chunk_offsets=self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:Some("visible chunk offsets"),
            contents:bytemuck::cast_slice(&offsets),
            usage:wgpu::BufferUsages::VERTEX,
        });
    }

    // Water modifications can change the face of a neighboring chunk.
    // Return at most four distinct touched chunk origins per changed cell.
    fn changed_chunk_origins(changed:&HashSet<(i32,i32,i32)>)
        ->HashSet<(i32,i32)> {
        fn origin(n:i32)->i32 {
            terrain::wrap(n).div_euclid(terrain::CHUNK_SIZE)*terrain::CHUNK_SIZE
        }
        let mut result=HashSet::new();
        for &(x,y,_) in changed {
            let current_x=origin(x);
            let current_y=origin(y);
            let nearby_x=[origin(x-1),current_x,origin(x+1)];
            let nearby_y=[origin(y-1),current_y,origin(y+1)];
            for xx in nearby_x {for yy in nearby_y {
                result.insert((xx,yy));
            }}
        }
        result
    }

    fn queue_edited_chunks(&mut self,changed:&HashSet<(i32,i32,i32)>) {
        self.pending_mesh_chunks.extend(Self::changed_chunk_origins(changed));
    }

    /// Avoid blocking one frame for 49 chunk rebuilds. In the recorded
    /// regression this took 100-243ms, even after the water solver passed.
    /// A round-robin budget ensures distant visible chunks are eventually
    /// refreshed instead of starving under continuous near-player motion.
    fn flush_queued_chunks(&mut self) {
        const MAX_CHUNKS_PER_FRAME:usize=2;
        let count=self.chunks.len();
        if count==0 || self.pending_mesh_chunks.is_empty() {return;}
        // Only currently visible chunk meshes matter. Walking into a new
        // chunk already builds unseen chunks from the current terrain state.
        let visible:HashSet<_>=self.chunks.iter().map(|c|c.origin).collect();
        self.pending_mesh_chunks.retain(|key|visible.contains(key));
        let start=self.mesh_refresh_cursor%count;
        let mut next=start;
        let mut rebuilt=0;
        for step in 0..count {
            if rebuilt>=MAX_CHUNKS_PER_FRAME {break;}
            let i=(start+step)%count;
            let (x,y)=self.chunks[i].origin;
            if !self.pending_mesh_chunks.remove(&(x,y)) {continue;}
            let water=mesh::build_water(&self.terrain,x,y,0);
            let (vertices,indices,water_count)=self.water_buffers(&water);
            self.chunks[i].water_vertices=vertices;
            self.chunks[i].water_indices=indices;
            self.chunks[i].water_count=water_count;
            rebuilt+=1;
            next=(i+1)%count;
        }
        self.mesh_refresh_cursor=next;
    }
    fn upload_hud(&self) {
        let eye_cell=(self.player.eye().x.floor() as i32,
            self.player.eye().y.floor() as i32,self.player.eye().z.floor() as i32);
        let sample=target_water(&self.terrain,&self.player).unwrap_or(eye_cell);
        let water_liters=self.terrain.water_liters(sample.0,sample.1,sample.2);
        let data=hud::rasterize(self.player.yaw,self.player.pos.x,
            self.player.pos.y,self.player.pos.z,
            self.hud_fps,self.hud_frame_ms,self.time_scale,self.world_time_seconds,
            water_liters,
            self.terrain.water.active.len()+self.terrain.water.surface_active.len(),
            self.terrain.water.surface_delta_liters,self.terrain.water.atmosphere_liters,
            self.terrain.water_mass_error_liters()
                +self.atmosphere.water_delta_kg().round() as i64,
            self.atmosphere.relative_humidity(sample.0,sample.1),
            self.atmosphere.cells[atmosphere::Atmosphere::cell_index(sample.0,sample.1)].temperature_c,
            self.atmosphere.rain_delivered_liters,self.atmosphere.rain_unmet_liters,
            (self.atmosphere.time_seconds/(10.0*86_400.0)).floor() as usize%12,
            self.atmosphere.rain_gauge_mm(
                self.player.pos.x.floor() as i32,
                self.player.pos.y.floor() as i32,
                (self.atmosphere.time_seconds/(10.0*86_400.0)).floor() as usize%12));
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture:&self.hud_texture,mip_level:0,
                origin:wgpu::Origin3d::ZERO,aspect:wgpu::TextureAspect::All,
            },
            &data,
            wgpu::ImageDataLayout {
                offset:0,bytes_per_row:Some(hud::WIDTH*4),rows_per_image:Some(hud::HEIGHT),
            },
            wgpu::Extent3d {width:hud::WIDTH,height:hud::HEIGHT,depth_or_array_layers:1},
        );
    }

    fn resize(&mut self, size:PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 { return; }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device,&self.config);
        self.depth = depth_view(&self.device,size);
    }
    fn update(&mut self) {
        let now = Instant::now();
        let measured_dt = now.duration_since(self.last_frame).as_secs_f32();
        let frame_dt = measured_dt.min(0.1);
        self.last_frame = now;
        self.hud_elapsed += measured_dt;
        self.hud_frame_seconds += measured_dt;
        self.hud_frames += 1;
        if self.hud_elapsed >= 0.10 {
            let average=self.hud_frame_seconds/self.hud_frames as f32;
            self.hud_fps=1.0/average.max(0.00001);
            self.hud_frame_ms=average*1000.0;
            self.hud_elapsed=0.0;
            self.hud_frame_seconds=0.0;
            self.hud_frames=0;
            self.upload_hud();
        }
        // Accelerate astronomical/climate time only. Multiplying the 120 Hz
        // player step or the existing 0.2 s fluid tick would be unstable
        // and prohibitively expensive at 1000x.
        let prior_time=self.world_time_seconds;
        let advanced=frame_dt as f64*self.time_scale as f64;
        self.world_time_seconds+=advanced;
        self.soil_accumulator+=frame_dt;
        self.soil_climate_seconds+=advanced*72.0;
        // One 20-minute calendar day corresponds to 86,400 climatic seconds.
        // Substep heat integration so 1000x never applies an enormous dt.
        let thermal_seconds=advanced*72.0;
        let thermal_steps=(thermal_seconds/120.0).ceil().max(1.0) as usize;
        for i in 0..thermal_steps {
            let sample_time=prior_time+advanced*(i as f64+0.5)/thermal_steps as f64;
            let sample_solar=geoscience::SolarTime {
                day_fraction:(sample_time/1200.0).fract(),
                year_fraction:(0.25+sample_time/(1200.0*120.0)).fract(),
                ..Default::default()
            };
            let solar=geoscience::solar_flux_w_m2(
                self.player.pos.x as i32,self.player.pos.y as i32,sample_solar,
            );
            self.surface_temperature.advance(solar,geoscience::Surface::Grass,
                300.0,0.0,thermal_seconds/thermal_steps as f64);
        }
        let solar_time=geoscience::SolarTime {
            day_fraction:(self.world_time_seconds/1200.0).fract(),
            year_fraction:(0.25+self.world_time_seconds/(1200.0*120.0)).fract(),
            ..Default::default()
        };
        let solar=geoscience::solar_flux_w_m2(
            self.player.pos.x as i32,self.player.pos.y as i32,solar_time,
        );
        self.sunlight=(solar/950.0).clamp(0.0,1.0) as f32;
        self.physics_accumulator += frame_dt;
        let mut ticks = 0;
        while self.physics_accumulator >= PHYSICS_DT && ticks < 12 {
            update_player(&mut self.player, &self.terrain, &self.keys, PHYSICS_DT);
            self.physics_accumulator -= PHYSICS_DT;
            ticks += 1;
        }
        // Fluid runs at a lower fixed rate than player physics. Only active
        // cells are visited; calm ocean cells do not update every tick.
        self.water_accumulator += frame_dt;
        if self.water_accumulator >= 0.2 {
            self.water_accumulator = 0.0;
            let water_clock=Instant::now();
            self.terrain.step_water(4096);
            let ms=water_clock.elapsed().as_secs_f64()*1000.0;
            if ms>25.0 {
                eprintln!("hydrology slow water tick: {:.0}ms cave_active={} surface_columns={}",
                    ms,self.terrain.water.active.len(),
                    self.terrain.water.surface_active.len());
            }
        }
        if self.soil_accumulator >= 0.2 {
            let climate_clock=Instant::now();
            let elapsed=self.soil_climate_seconds;
            self.soil_accumulator=0.0;
            self.soil_climate_seconds=0.0;
            self.terrain.step_soil(elapsed,4096);
            self.terrain.step_groundwater(elapsed,4096);
            self.atmosphere.advance_af(elapsed);
            self.atmosphere.evaporate_near(
                &mut self.terrain,
                self.player.pos.x.floor() as i32,
                self.player.pos.y.floor() as i32,
                elapsed,
            );
            self.atmosphere.rain_near(
                &mut self.terrain,
                self.player.pos.x.floor() as i32,
                self.player.pos.y.floor() as i32,
                elapsed,
            );
            let ms=climate_clock.elapsed().as_secs_f64()*1000.0;
            if ms>25.0 {
                eprintln!("hydrology slow climate batch: {:.0}ms soil_active={}",
                    ms,self.terrain.soil.active_count());
            }
        }
        let dirty=self.terrain.take_water_dirty();
        if !dirty.is_empty() {self.queue_edited_chunks(&dirty);}
        let cx = (self.player.pos.x as i32).div_euclid(32) * 32;
        let cy = (self.player.pos.y as i32).div_euclid(32) * 32;
        if (cx,cy) != self.mesh_origin { self.rebuild(); }
        let remesh_clock=Instant::now();
        self.flush_queued_chunks();
        let remesh_ms=remesh_clock.elapsed().as_secs_f64()*1000.0;
        if remesh_ms>25.0 {
            eprintln!("hydrology slow remesh batch: {:.0}ms queued={}",
                remesh_ms,self.pending_mesh_chunks.len());
        }
        let eye = self.player.eye();
        let x = (terrain::delta(eye.x.floor() as i32,self.mesh_origin.0) as f64 + eye.x.fract()) as f32;
        let y = (terrain::delta(eye.y.floor() as i32,self.mesh_origin.1) as f64 + eye.y.fract()) as f32;
        let local_eye = Vec3::new(x,y,eye.z as f32);
        let view = Mat4::look_to_rh(local_eye,self.player.forward(),Vec3::Z);
        let aspect = self.config.width as f32 / self.config.height as f32;
        let proj = Mat4::perspective_rh(75.0_f32.to_radians(),aspect,0.05,400.0);
        let uniform = CameraUniform {
            view_proj:(proj*view).to_cols_array_2d(),
            viewport:[self.config.width as f32,self.config.height as f32,0.0,0.0],
            sunlight:[self.sunlight,0.0,0.0,0.0],
        };
        self.queue.write_buffer(&self.camera_buffer,0,bytemuck::bytes_of(&uniform));
    }
    fn edit(&mut self, button:MouseButton) {
        let solid_target=target_block(&self.terrain,&self.player);
        let mut changed=HashSet::new();
        match button {
            // Water is transparent to mining rays: mine the solid underneath.
            MouseButton::Left=>{
                if let Some((hit,_))=solid_target {
                    self.terrain.set(hit.0,hit.1,hit.2,Block::Air);
                    changed.insert(hit);
                }
            }
            MouseButton::Right=>{
                // Prefer the face against a submerged solid, but allow free
                // placement in visible water when no solid can be targeted.
                let placement=solid_target.map(|(_,empty)|empty)
                    .or_else(||target_water(&self.terrain,&self.player));
                if let Some(cell)=placement {
                    if !player_overlaps_cell(&self.player,cell)
                        && self.terrain.try_place_solid(cell,self.selected) {
                        changed.insert(cell);
                    }
                }
            }
            _=>return,
        }
        if !changed.is_empty() {self.refresh_solid_edit_chunks(&changed);}
        let dirty=self.terrain.take_water_dirty();
        if !dirty.is_empty() {self.queue_edited_chunks(&dirty);}
    }
    fn render(&mut self) -> Result<(),wgpu::SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());
        let sun=self.sunlight as f64;
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {label:Some("frame")});
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label:Some("voxels"),
                color_attachments:&[Some(wgpu::RenderPassColorAttachment {
                    view:&view,resolve_target:None,
                    ops:wgpu::Operations {
                        load:wgpu::LoadOp::Clear(wgpu::Color {r:0.025+0.465*sun,g:0.035+0.635*sun,b:0.065+0.725*sun,a:1.0}),
                        store:wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment:Some(wgpu::RenderPassDepthStencilAttachment {
                    view:&self.depth,
                    depth_ops:Some(wgpu::Operations {
                        load:wgpu::LoadOp::Clear(1.0),store:wgpu::StoreOp::Store,
                    }),stencil_ops:None,
                }),
                timestamp_writes:None,occlusion_query_set:None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0,&self.camera_bind,&[]);
            pass.set_bind_group(1,&self.texture_bind,&[]);
            for (i,chunk) in self.chunks.iter().enumerate() {
                if chunk.opaque_count==0 {continue;}
                pass.set_vertex_buffer(0,chunk.vertices.slice(..));
                let start=(i*8) as u64;
                pass.set_vertex_buffer(1,self.chunk_offsets.slice(start..start+8));
                pass.set_index_buffer(chunk.indices.slice(..),wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..chunk.opaque_count,0,0..1);
            }
            pass.set_pipeline(&self.water_pipeline);
            for (i,chunk) in self.chunks.iter().enumerate() {
                if chunk.water_count==0 {continue;}
                pass.set_vertex_buffer(0,chunk.water_vertices.slice(..));
                let start=(i*8) as u64;
                pass.set_vertex_buffer(1,self.chunk_offsets.slice(start..start+8));
                pass.set_index_buffer(chunk.water_indices.slice(..),wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..chunk.water_count,0,0..1);
            }
            pass.set_pipeline(&self.crosshair_pipeline);
            pass.set_bind_group(0,&self.camera_bind,&[]);
            pass.set_scissor_rect(
                self.config.width.saturating_sub(20)/2,
                self.config.height.saturating_sub(20)/2,
                20.min(self.config.width),20.min(self.config.height),
            );
            pass.draw(0..3,0..1);
            pass.set_pipeline(&self.hud_pipeline);
            pass.set_bind_group(0,&self.camera_bind,&[]);
            pass.set_bind_group(1,&self.hud_bind,&[]);
            pass.set_scissor_rect(0,0,self.config.width,self.config.height);
            pass.draw(0..3,0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("Event loop");
    let window = Arc::new(WindowBuilder::new()
        .with_title("Voxel Survival | WASD Move | Space Jump | Click capture | Left mine | Right place | 1 Dirt 2 Grass 3 Stone | Esc release")
        .with_inner_size(PhysicalSize::new(1280,720))
        .build(&event_loop).expect("Window"));
    let mut game = pollster::block_on(Game::new(window.clone()));
    event_loop.run(move |event, target| {
        match event {
            Event::DeviceEvent {event:DeviceEvent::MouseMotion {delta},..} if game.mouse_captured => {
                game.player.yaw -= delta.0 as f32 * MOUSE_SENSITIVITY;
                game.player.pitch = (game.player.pitch - delta.1 as f32 * MOUSE_SENSITIVITY).clamp(-1.54,1.54);
            }
            Event::WindowEvent {event,window_id} if window_id == window.id() => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => game.resize(size),
                WindowEvent::KeyboardInput {event,..} => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        if code == KeyCode::Escape && event.state == ElementState::Pressed {
                            game.mouse_captured = false;
                            let _ = window.set_cursor_grab(CursorGrabMode::None);
                            window.set_cursor_visible(true);
                        }
                        if event.state == ElementState::Pressed {
                            game.keys.insert(code);
                            if code == KeyCode::F1 && !event.repeat {
                                game.time_scale=next_lab_time_scale(game.time_scale);
                                game.upload_hud();
                            }
                            game.selected = match code {
                                KeyCode::Digit1 => Block::Dirt,
                                KeyCode::Digit2 => Block::Grass,
                                KeyCode::Digit3 => Block::Stone,
                                _ => game.selected,
                            };
                        } else { game.keys.remove(&code); }
                    }
                }
                WindowEvent::Focused(false) => {
                    game.keys.clear();
                    game.mouse_captured = false;
                    let _ = window.set_cursor_grab(CursorGrabMode::None);
                    window.set_cursor_visible(true);
                }
                WindowEvent::MouseInput {button,state:ElementState::Pressed,..} => {
                    if !game.mouse_captured {
                        if window.set_cursor_grab(CursorGrabMode::Locked)
                            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined)).is_ok() {
                            window.set_cursor_visible(false);
                            game.mouse_captured = true;
                        }
                    } else { game.edit(button); }
                }
                WindowEvent::RedrawRequested => {
                    game.update();
                    match game.render() {
                        Ok(()) => {}
                        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                            game.resize(window.inner_size());
                        }
                        Err(wgpu::SurfaceError::OutOfMemory) => target.exit(),
                        Err(wgpu::SurfaceError::Timeout) => {}
                    }
                }
                _ => {}
            }
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        }
    }).expect("Run event loop");
}

#[cfg(test)]
mod shader_tests {
    #[test]
    fn shader_parses_and_validates() {
        let module = naga::front::wgsl::parse_str(include_str!("shader.wgsl"))
            .expect("WGSL shader syntax");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        ).validate(&module).expect("WGSL shader validation");
    }
    #[test]
    fn hud_shader_parses_and_validates() {
        let module=naga::front::wgsl::parse_str(include_str!("hud.wgsl"))
            .expect("HUD WGSL syntax");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),naga::valid::Capabilities::all(),
        ).validate(&module).expect("HUD shader validation");
    }

}

#[cfg(test)]
mod movement_tests {
    #[test]
    fn changed_water_faces_invalidate_only_neighboring_chunks() {
        let mut changed=HashSet::new();
        changed.insert((15,15,120));
        let chunks=Game::changed_chunk_origins(&changed);
        assert_eq!(chunks,HashSet::from([(0,0)]));
        changed.insert((31,31,120));
        let chunks=Game::changed_chunk_origins(&changed);
        assert!(chunks.contains(&(32,32)));
        assert_eq!(chunks.len(),4);
        let seam=Game::changed_chunk_origins(&HashSet::from([(0,0,120)]));
        assert_eq!(seam.len(),4);
        assert!(seam.contains(&(terrain::WORLD_SIZE-32,terrain::WORLD_SIZE-32)));
        assert!(seam.contains(&(0,0)));
    }

    use super::*;

    fn player(x: f32, y: f32, z: f32) -> Player {
        Player { pos: DVec3::new(x as f64,y as f64,z as f64), velocity_z:0.0, yaw:0.0,
                 pitch:0.0, grounded:true }
    }

    #[test]
    fn f1_cycles_through_lab_time_rates_and_back_to_normal() {
        assert_eq!(LAB_TIME_SCALES,[1,10,100,1000]);
        let mut scale=1;
        for expected in [10,100,1000,1] {
            scale=next_lab_time_scale(scale);
            assert_eq!(scale,expected);
        }
    }

    #[test]
    fn accelerated_calendar_is_independent_of_fixed_physics_ticks() {
        // F1 scales calendar advancement, not the 120 Hz player timestep.
        let real_dt=0.05_f64;
        assert_eq!(real_dt*LAB_TIME_SCALES[2] as f64,5.0);
        assert!((PHYSICS_DT as f64-1.0/120.0).abs()<1e-8);
    }
    #[test]
    fn strafe_right_moves_screen_right_and_left_moves_screen_left() {
        let t = Terrain::new();
        let x = (WORLD_SIZE / 2) as f32 + 0.5;
        let y = x;
        let z = 230.0; // Air above terrain, so direction is isolated.
        let mut p = player(x,y,z);
        update_player(&mut p,&t,&HashSet::from([KeyCode::KeyD]),1.0/60.0);
        assert!(p.pos.y < y as f64, "D should move right, toward negative Y at yaw 0");
        let mut p = player(x,y,z);
        update_player(&mut p,&t,&HashSet::from([KeyCode::KeyA]),1.0/60.0);
        assert!(p.pos.y > y as f64, "A should move left, toward positive Y at yaw 0");
    }

    #[test]
    fn jump_clears_one_meter_block_without_automatic_stepping() {
        let mut t = Terrain::new();
        let x = WORLD_SIZE / 2;
        let y = WORLD_SIZE / 2;
        // Small, flat arena well above the default terrain height.
        for dx in -2..=7 {
            for dy in -2..=2 {
                t.set(x+dx,y+dy,200,Block::Stone);
            }
        }
        for dy in -1..=1 { t.set(x+2,y+dy,201,Block::Stone); }
        let mut p = player(x as f32+0.5,y as f32+0.5,201.0);
        let mut peak = p.pos.z;
        let keys = HashSet::from([KeyCode::KeyW,KeyCode::Space]);
        for _ in 0..120 {
            update_player(&mut p,&t,&keys,1.0/120.0);
            peak = peak.max(p.pos.z);
        }
        assert!(peak > 202.15, "jump must exceed one-meter obstacle with clearance");
        assert!(p.pos.x > x as f64 + 3.3, "player should traverse the block");
    }

    #[test]
    fn ray_targets_solid_through_water_for_underwater_mining_and_placement() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        t.set(x+2,y,241,Block::Stone);
        t.water.atmosphere_liters=600;
        assert_eq!(t.precipitate((x+1,y,241),600),600);
        let mut p=player(x as f32+0.5,y as f32+0.5,240.0);
        p.yaw=0.0;
        let (hit,previous)=target_block(&t,&p).expect("submerged wall target");
        assert_eq!(hit,(x+2,y,241));
        assert_eq!(previous,(x+1,y,241));
        assert!(!player_overlaps_cell(&p,previous));
        assert!(t.try_place_solid(previous,Block::Dirt));
        assert_eq!(t.block(x+1,y,241),Block::Dirt);
        assert_eq!(t.water.amounts.values().map(|&v|v as u32).sum::<u32>(),600);
        t.set(x+2,y,241,Block::Air);
        assert_eq!(t.block(x+2,y,241),Block::Air);
    }

    #[test]
    fn placement_ray_can_select_water_without_a_solid_backing() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        t.water.atmosphere_liters=800;
        t.precipitate((x+2,y,241),800);
        let mut p=player(x as f32+0.5,y as f32+0.5,240.0);
        p.yaw=0.0;
        assert_eq!(target_water(&t,&p),Some((x+2,y,241)));
        assert!(!player_overlaps_cell(&p,(x+2,y,241)));
        assert!(t.try_place_solid((x+2,y,241),Block::Dirt));
        assert_eq!(t.water.amounts.values().map(|&v|v as u32).sum::<u32>(),800);
    }

    #[test]
    fn spawn_has_clear_body_camera_and_support() {
        let t=Terrain::new();
        let p=find_safe_spawn(&t);
        assert!(!collides(&t,p.pos),"player intersects terrain at spawn");
        assert!(spawn_is_safe(&t,p.pos),"spawn lacks safe surrounding clearance");
        assert!(support_below(&t,&p),"spawn has no ground below feet");
        assert!(p.pos.z>terrain::SEA_LEVEL as f64+2.0);
    }

    #[test]
    fn reject_spawn_with_higher_neighbor() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        for dx in -3..=3 {
            for dy in -3..=3 {t.set(x+dx,y+dy,200,Block::Stone);}
        }
        let pos=DVec3::new(x as f64+0.5,y as f64+0.5,201.0);
        assert!(spawn_is_safe(&t,pos));
        t.set(x+2,y,201,Block::Stone);
        assert!(!spawn_is_safe(&t,pos));
    }

    #[test]
    fn subcentimeter_motion_is_not_lost_at_large_world_coordinates() {
        let t = Terrain::new();
        let center = WORLD_SIZE as f64 / 2.0;
        let mut p = Player {
            pos:DVec3::new(center + 0.5,center + 0.5,230.0),
            velocity_z:0.0,yaw:0.0,pitch:0.0,grounded:false,
        };
        let start = p.pos.x;
        update_player(&mut p,&t,&HashSet::from([KeyCode::KeyW]),0.0002);
        let moved = p.pos.x - start;
        assert!((moved - 0.001).abs() < 0.000_001,
            "1mm movement must survive at world center; got {moved}");
    }

    #[test]
    fn world_wrapping_preserves_fractional_player_position() {
        let t = Terrain::new();
        let mut p=Player {
            pos:DVec3::new(WORLD_SIZE as f64 - 0.001,500_000.5,230.0),
            velocity_z:0.0,yaw:0.0,pitch:0.0,grounded:false,
        };
        update_player(&mut p,&t,&HashSet::from([KeyCode::KeyW]),0.001);
        assert!((p.pos.x-0.004).abs() < 0.000_001,
            "wrapped X should retain millimeter precision: {}",p.pos.x);
    }

    #[test]
    fn grounded_player_stays_at_exact_same_height() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        for dx in -2..=2 {
            for dy in -2..=2 { t.set(x+dx,y+dy,200,Block::Stone); }
        }
        let mut p=player(x as f32+0.5,y as f32+0.5,201.0);
        for _ in 0..720 {
            update_player(&mut p,&t,&HashSet::new(),PHYSICS_DT);
            assert_eq!(p.pos.z,201.0,"stationary camera bobbed on level floor");
            assert!(p.grounded);
        }
    }

    #[test]
    fn player_falls_only_after_walking_off_edge() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        for dy in -1..=1 { t.set(x,y+dy,200,Block::Stone); }
        let mut p=player(x as f32+0.5,y as f32+0.5,201.0);
        for _ in 0..150 {
            update_player(&mut p,&t,&HashSet::from([KeyCode::KeyW]),PHYSICS_DT);
        }
        assert!(p.pos.z<200.0,"the player did not fall off the platform");
    }

    #[test]
    fn head_collisions_do_not_teleport_player_upwards() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        for dx in -1..=1 {
            for dy in -1..=1 {
                t.set(x+dx,y+dy,200,Block::Stone);
                t.set(x+dx,y+dy,203,Block::Stone);
            }
        }
        let mut p=player(x as f32+0.5,y as f32+0.5,201.0);
        for _ in 0..120 {
            update_player(&mut p,&t,&HashSet::from([KeyCode::Space]),PHYSICS_DT);
            assert!(p.pos.z<=201.71,"head entered the ceiling");
        }
    }

    #[test]
    fn walking_into_a_one_meter_wall_does_not_auto_climb() {
        let mut t = Terrain::new();
        let x = WORLD_SIZE / 2;
        let y = WORLD_SIZE / 2;
        for dx in -2..=5 {
            for dy in -2..=2 { t.set(x+dx,y+dy,200,Block::Stone); }
        }
        for dy in -1..=1 { t.set(x+2,y+dy,201,Block::Stone); }
        let mut p = player(x as f32+0.5,y as f32+0.5,201.0);
        let keys = HashSet::from([KeyCode::KeyW]);
        for _ in 0..100 { update_player(&mut p,&t,&keys,1.0/120.0); }
        assert!(p.pos.x < x as f64 + 2.0);
        assert!(p.pos.z < 201.1, "walking must not teleport up one meter");
    }
}
