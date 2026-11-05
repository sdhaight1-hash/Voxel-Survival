mod mesh;
mod terrain;
mod hydrology;
mod geoscience;

use std::{collections::{HashMap,HashSet}, sync::Arc, time::Instant};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
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
const PLAYER_HEIGHT: f32 = 1.3;
const PLAYER_RADIUS: f32 = 0.28;
const EYE_HEIGHT: f32 = 1.18;
const JUMP_SPEED: f32 = 7.5;
const GRAVITY: f32 = 20.0;
const MOUSE_SENSITIVITY: f32 = 0.002;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    viewport: [f32; 4],
}

struct Player {
    pos: Vec3, // feet; Z up; X/Y wrap
    velocity_z: f32,
    yaw: f32,
    pitch: f32,
    grounded: bool,
}
impl Player {
    fn forward(&self) -> Vec3 {
        Vec3::new(self.yaw.cos() * self.pitch.cos(), self.yaw.sin() * self.pitch.cos(), self.pitch.sin())
    }
    fn eye(&self) -> Vec3 { self.pos + Vec3::Z * EYE_HEIGHT }
}

// Player is a continuous AABB; voxel coordinates are integer unit cubes.
// The epsilon avoids treating a mere face-to-face touch as penetration.
const COLLISION_EPS: f32 = 0.0001;
const GROUND_PROBE: f32 = 0.025;
const PHYSICS_DT: f32 = 1.0 / 120.0;

fn collides(t: &Terrain, pos: Vec3) -> bool {
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
fn wrap_pos(v: f32) -> f32 { v.rem_euclid(WORLD_SIZE as f32) }

fn support_below(t: &Terrain, p: &Player) -> bool {
    collides(t, p.pos - Vec3::Z * GROUND_PROBE)
}
fn horizontal_direction(p: &Player, keys: &HashSet<KeyCode>) -> Vec3 {
    let forward = Vec3::new(p.yaw.cos(), p.yaw.sin(), 0.0);
    let right = Vec3::new(p.yaw.sin(), -p.yaw.cos(), 0.0);
    let mut v = Vec3::ZERO;
    if keys.contains(&KeyCode::KeyW) { v += forward; }
    if keys.contains(&KeyCode::KeyS) { v -= forward; }
    if keys.contains(&KeyCode::KeyD) { v += right; }
    if keys.contains(&KeyCode::KeyA) { v -= right; }
    if v.length_squared() > 0.0 { v.normalize() } else { v }
}

// Called at a fixed 120 Hz, never with the display frame duration.
fn update_player(p: &mut Player, t: &Terrain, keys: &HashSet<KeyCode>, dt: f32) {
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
            let candidate = p.pos + Vec3::Z * (travel / steps as f32);
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
        let step = travel / steps as f32;
        // Independent axes permit sliding along walls.
        let x = Vec3::new(wrap_pos(p.pos.x + step.x), p.pos.y, p.pos.z);
        if !collides(t, x) { p.pos = x; }
        let y = Vec3::new(p.pos.x, wrap_pos(p.pos.y + step.y), p.pos.z);
        if !collides(t, y) { p.pos = y; }
    }
    // A ledge can be crossed during horizontal movement; it is no longer
    // supported, but gravity starts on the next fixed simulation step.
    if p.grounded && !support_below(t, p) { p.grounded = false; }
    p.pos.z = p.pos.z.clamp(0.0, (WORLD_HEIGHT - 2) as f32);
}

// Return targeted solid block and the preceding empty cell.
fn target_block(t: &Terrain, p: &Player) -> Option<((i32,i32,i32), (i32,i32,i32))> {
    let start = p.eye();
    let direction = p.forward();
    let mut previous = None;
    for i in 1..=120 {
        let q = start + direction * (i as f32 * 0.05);
        let cell = (q.x.floor() as i32, q.y.floor() as i32, q.z.floor() as i32);
        if previous == Some(cell) { continue; }
        if t.block(cell.0, cell.1, cell.2).solid() {
            return previous.map(|empty| (cell, empty));
        }
        previous = Some(cell);
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
    count: u32,
}

struct Game {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    water_pipeline: wgpu::RenderPipeline,
    crosshair_pipeline: wgpu::RenderPipeline,
    depth: wgpu::TextureView,
    camera_buffer: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    texture_bind: wgpu::BindGroup,
    chunks: Vec<ChunkGpu>,
    chunk_offsets: wgpu::Buffer,
    mesh_origin: (i32, i32),
    terrain: Terrain,
    player: Player,
    selected: Block,
    keys: HashSet<KeyCode>,
    mouse_captured: bool,
    last_frame: Instant,
    physics_accumulator: f32,
    water_accumulator: f32,
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
        let terrain = Terrain::new();
        let spawn_x = WORLD_SIZE as f32 / 2.0;
        let spawn_y = WORLD_SIZE as f32 / 2.0;
        // Pick the closest dry ground within a 256 m square; no submerged start.
        let mut spawn = (spawn_x as i32, spawn_y as i32);
        let mut found = false;
        for radius in 0i32..=8 {
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    if dx.abs() != radius && dy.abs() != radius { continue; }
                    let x = spawn_x as i32 + dx * 16;
                    let y = spawn_y as i32 + dy * 16;
                    if terrain.height(x, y) >= terrain::SEA_LEVEL + 3 {
                        spawn = (x, y);
                        found = true;
                        break;
                    }
                }
                if found { break; }
            }
            if found { break; }
        }
        let spawn_z = terrain.height(spawn.0, spawn.1).max(terrain::SEA_LEVEL) as f32 + 2.0;
        let player = Player {
            pos:Vec3::new(spawn.0 as f32,spawn.1 as f32,spawn_z),
            velocity_z:0.0, yaw:0.3,pitch:-0.2,grounded:false,
        };
        let depth = depth_view(&device,size);
        let chunk_offsets = device.create_buffer(&wgpu::BufferDescriptor {
            label:Some("chunk offset placeholder"),size:8,
            usage:wgpu::BufferUsages::VERTEX,mapped_at_creation:false,
        });
        let mut game = Self {
            surface,device,queue,config,pipeline,water_pipeline,crosshair_pipeline,
            depth,
            camera_buffer,camera_bind,texture_bind,
            chunks:Vec::new(),chunk_offsets,
            mesh_origin:(0,0),terrain,player,selected:Block::Dirt,
            keys:HashSet::new(),mouse_captured:false,last_frame:Instant::now(),physics_accumulator:0.0,water_accumulator:0.0,
        };
        game.rebuild();
        game
    }

    fn gpu_chunk(&self,x:i32,y:i32) -> ChunkGpu {
        let mesh=mesh::build(&self.terrain,x,y,0);
        ChunkGpu {
            origin:(terrain::wrap(x),terrain::wrap(y)),
            opaque_count:mesh.water_index_start,
            count:mesh.indices.len() as u32,
            vertices:self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label:Some("chunk vertices"),
                contents:bytemuck::cast_slice(&mesh.vertices),
                usage:wgpu::BufferUsages::VERTEX,
            }),
            indices:self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label:Some("chunk indices"),
                contents:bytemuck::cast_slice(&mesh.indices),
                usage:wgpu::BufferUsages::INDEX,
            }),
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

    // A changed voxel also affects visible faces across the adjacent border.
    fn refresh_edited_chunks(&mut self, changed:&HashSet<(i32,i32,i32)>) {
        let affected:Vec<(usize,i32,i32)>=self.chunks.iter().enumerate()
            .filter(|(_,chunk)| changed.iter().any(|&(x,y,_)| {
                let dx=terrain::delta(x,chunk.origin.0);
                let dy=terrain::delta(y,chunk.origin.1);
                (-1..=32).contains(&dx) && (-1..=32).contains(&dy)
            }))
            .map(|(index,chunk)|(index,chunk.origin.0,chunk.origin.1))
            .collect();
        for (index,x,y) in affected {
            self.chunks[index]=self.gpu_chunk(x,y);
        }
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
        let frame_dt = now.duration_since(self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
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
            self.terrain.step_water(4096);
            let dirty=self.terrain.take_water_dirty();
            if !dirty.is_empty() {self.refresh_edited_chunks(&dirty);}
        }
        let cx = (self.player.pos.x as i32).div_euclid(32) * 32;
        let cy = (self.player.pos.y as i32).div_euclid(32) * 32;
        if (cx,cy) != self.mesh_origin { self.rebuild(); }
        let eye = self.player.eye();
        let x = terrain::delta(eye.x.floor() as i32,self.mesh_origin.0) as f32 + eye.x.fract();
        let y = terrain::delta(eye.y.floor() as i32,self.mesh_origin.1) as f32 + eye.y.fract();
        let local_eye = Vec3::new(x,y,eye.z);
        let view = Mat4::look_to_rh(local_eye,self.player.forward(),Vec3::Z);
        let aspect = self.config.width as f32 / self.config.height as f32;
        let proj = Mat4::perspective_rh(75.0_f32.to_radians(),aspect,0.05,400.0);
        let uniform = CameraUniform {
            view_proj:(proj*view).to_cols_array_2d(),
            viewport:[self.config.width as f32,self.config.height as f32,0.0,0.0],
        };
        self.queue.write_buffer(&self.camera_buffer,0,bytemuck::bytes_of(&uniform));
    }
    fn edit(&mut self, button:MouseButton) {
        if let Some((hit,empty)) = target_block(&self.terrain,&self.player) {
            match button {
                MouseButton::Left => self.terrain.set(hit.0,hit.1,hit.2,Block::Air),
                MouseButton::Right => {
                    let old = self.terrain.block(empty.0,empty.1,empty.2);
                    // Until displacement is implemented, never delete finite
                    // water volume when placing a block.
                    if old == Block::Air {
                        self.terrain.set(empty.0,empty.1,empty.2,self.selected);
                        if collides(&self.terrain,self.player.pos) {
                            self.terrain.set(empty.0,empty.1,empty.2,old);
                        }
                    }
                }
                _ => return,
            }
            let changed=HashSet::from([hit,empty]);
            self.refresh_edited_chunks(&changed);
        }
    }
    fn render(&mut self) -> Result<(),wgpu::SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {label:Some("frame")});
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label:Some("voxels"),
                color_attachments:&[Some(wgpu::RenderPassColorAttachment {
                    view:&view,resolve_target:None,
                    ops:wgpu::Operations {
                        load:wgpu::LoadOp::Clear(wgpu::Color {r:0.49,g:0.67,b:0.79,a:1.0}),
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
                if chunk.opaque_count==chunk.count {continue;}
                pass.set_vertex_buffer(0,chunk.vertices.slice(..));
                let start=(i*8) as u64;
                pass.set_vertex_buffer(1,self.chunk_offsets.slice(start..start+8));
                pass.set_index_buffer(chunk.indices.slice(..),wgpu::IndexFormat::Uint32);
                pass.draw_indexed(chunk.opaque_count..chunk.count,0,0..1);
            }
            pass.set_pipeline(&self.crosshair_pipeline);
            pass.set_bind_group(0,&self.camera_bind,&[]);
            pass.set_scissor_rect(
                self.config.width.saturating_sub(20)/2,
                self.config.height.saturating_sub(20)/2,
                20.min(self.config.width),20.min(self.config.height),
            );
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
}

#[cfg(test)]
mod movement_tests {
    use super::*;

    fn player(x: f32, y: f32, z: f32) -> Player {
        Player { pos: Vec3::new(x,y,z), velocity_z:0.0, yaw:0.0,
                 pitch:0.0, grounded:true }
    }

    #[test]
    fn strafe_right_moves_screen_right_and_left_moves_screen_left() {
        let t = Terrain::new();
        let x = (WORLD_SIZE / 2) as f32 + 0.5;
        let y = x;
        let z = 230.0; // Air above terrain, so direction is isolated.
        let mut p = player(x,y,z);
        update_player(&mut p,&t,&HashSet::from([KeyCode::KeyD]),1.0/60.0);
        assert!(p.pos.y < y, "D should move right, toward negative Y at yaw 0");
        let mut p = player(x,y,z);
        update_player(&mut p,&t,&HashSet::from([KeyCode::KeyA]),1.0/60.0);
        assert!(p.pos.y > y, "A should move left, toward positive Y at yaw 0");
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
        assert!(p.pos.x > x as f32 + 3.3, "player should traverse the block");
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
        assert!(p.pos.x < x as f32 + 2.0);
        assert!(p.pos.z < 201.1, "walking must not teleport up one meter");
    }
}
