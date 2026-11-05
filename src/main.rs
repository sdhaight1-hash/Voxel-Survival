mod mesh;
mod terrain;

use std::{collections::HashSet, sync::Arc, time::Instant};
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

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform { view_proj: [[f32; 4]; 4] }

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

fn collides(t: &Terrain, pos: Vec3) -> bool {
    for dx in [-PLAYER_RADIUS, PLAYER_RADIUS] {
        for dy in [-PLAYER_RADIUS, PLAYER_RADIUS] {
            for dz in [0.04, 0.65, PLAYER_HEIGHT - 0.02] {
                let p = pos + Vec3::new(dx,dy,dz);
                if t.block(p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32).solid() {
                    return true;
                }
            }
        }
    }
    false
}
fn wrap_pos(v: f32) -> f32 { v.rem_euclid(WORLD_SIZE as f32) }

fn update_player(p: &mut Player, t: &Terrain, keys: &HashSet<KeyCode>, dt: f32) {
    let forward = Vec3::new(p.yaw.cos(), p.yaw.sin(), 0.0);
    let right = Vec3::new(-p.yaw.sin(), p.yaw.cos(), 0.0);
    let mut movement = Vec3::ZERO;
    if keys.contains(&KeyCode::KeyW) { movement += forward; }
    if keys.contains(&KeyCode::KeyS) { movement -= forward; }
    if keys.contains(&KeyCode::KeyD) { movement += right; }
    if keys.contains(&KeyCode::KeyA) { movement -= right; }
    if movement.length_squared() > 0.0 { movement = movement.normalize() * 5.0 * dt; }

    for axis in 0..2 {
        let mut candidate = p.pos;
        if axis == 0 { candidate.x = wrap_pos(candidate.x + movement.x); }
        else { candidate.y = wrap_pos(candidate.y + movement.y); }
        if !collides(t, candidate) {
            p.pos = candidate;
        } else if p.grounded {
            // One-meter stair step over ordinary block-height terrain.
            let raised = candidate + Vec3::Z;
            if !collides(t, raised) && !collides(t, p.pos + Vec3::Z) {
                p.pos = raised;
            }
        }
    }
    if p.grounded && keys.contains(&KeyCode::Space) {
        p.velocity_z = 6.0;
        p.grounded = false;
    }
    p.velocity_z = (p.velocity_z - 20.0 * dt).max(-35.0);
    let candidate = p.pos + Vec3::Z * p.velocity_z * dt;
    if collides(t, candidate) {
        if p.velocity_z < 0.0 {
            p.pos.z = candidate.z.floor() + 1.0;
            p.grounded = true;
        }
        p.velocity_z = 0.0;
    } else {
        p.pos = candidate;
        p.grounded = false;
    }
    // Prevent leaving the vertical world bounds.
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

struct Game {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    depth: wgpu::TextureView,
    camera_buffer: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    texture_bind: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    mesh_origin: (i32, i32),
    terrain: Terrain,
    player: Player,
    selected: Block,
    keys: HashSet<KeyCode>,
    mouse_captured: bool,
    last_frame: Instant,
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
            contents: bytemuck::bytes_of(&CameraUniform { view_proj: Mat4::IDENTITY.to_cols_array_2d() }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0, visibility: wgpu::ShaderStages::VERTEX,
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
                module:&shader, entry_point:"vs_main", buffers:&[mesh::Vertex::layout()],
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
        let terrain = Terrain::new();
        let spawn_x = WORLD_SIZE as f32 / 2.0;
        let spawn_y = WORLD_SIZE as f32 / 2.0;
        let spawn_z = terrain.height(spawn_x as i32, spawn_y as i32) as f32 + 2.0;
        let player = Player {
            pos:Vec3::new(spawn_x,spawn_y,spawn_z),
            velocity_z:0.0, yaw:0.3,pitch:-0.2,grounded:false,
        };
        let placeholder = device.create_buffer(&wgpu::BufferDescriptor {
            label:Some("placeholder"),size:4,usage:wgpu::BufferUsages::VERTEX|wgpu::BufferUsages::INDEX,
            mapped_at_creation:false,
        });
        let depth = depth_view(&device,size);
        let index_placeholder = device.create_buffer(&wgpu::BufferDescriptor {
            label:Some("index placeholder"),size:4,usage:wgpu::BufferUsages::INDEX,mapped_at_creation:false,
        });
        let mut game = Self {
            surface,device,queue,config,pipeline,
            depth,
            camera_buffer,camera_bind,texture_bind,
            vertex_buffer:placeholder,index_buffer:index_placeholder,index_count:0,
            mesh_origin:(0,0),terrain,player,selected:Block::Dirt,
            keys:HashSet::new(),mouse_captured:false,last_frame:Instant::now(),
        };
        game.rebuild();
        game
    }

    fn rebuild(&mut self) {
        let m = mesh::build(&self.terrain, self.player.pos.x as i32, self.player.pos.y as i32, VIEW_RADIUS);
        self.mesh_origin = (m.origin_x,m.origin_y);
        self.index_count = m.indices.len() as u32;
        self.vertex_buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:Some("visible voxel vertices"),contents:bytemuck::cast_slice(&m.vertices),
            usage:wgpu::BufferUsages::VERTEX,
        });
        self.index_buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:Some("visible voxel indices"),contents:bytemuck::cast_slice(&m.indices),
            usage:wgpu::BufferUsages::INDEX,
        });
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
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        update_player(&mut self.player,&self.terrain,&self.keys,dt);
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
        let uniform = CameraUniform {view_proj:(proj*view).to_cols_array_2d()};
        self.queue.write_buffer(&self.camera_buffer,0,bytemuck::bytes_of(&uniform));
    }
    fn edit(&mut self, button:MouseButton) {
        if let Some((hit,empty)) = target_block(&self.terrain,&self.player) {
            match button {
                MouseButton::Left => self.terrain.set(hit.0,hit.1,hit.2,Block::Air),
                MouseButton::Right => {
                    let old = self.terrain.block(empty.0,empty.1,empty.2);
                    if old == Block::Air || old == Block::Water {
                        self.terrain.set(empty.0,empty.1,empty.2,self.selected);
                        if collides(&self.terrain,self.player.pos) {
                            self.terrain.set(empty.0,empty.1,empty.2,old);
                        }
                    }
                }
                _ => return,
            }
            self.rebuild();
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
            pass.set_vertex_buffer(0,self.vertex_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..),wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.index_count,0,0..1);
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
                game.player.yaw += delta.0 as f32 * 0.002;
                game.player.pitch = (game.player.pitch - delta.1 as f32 * 0.002).clamp(-1.54,1.54);
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
