use crate::terrain::{wrap, delta, Block, Terrain, CHUNK_SIZE, SEA_LEVEL, WORLD_HEIGHT};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub tile: u32,
}
impl Vertex {
    pub fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { offset: 0, shader_location: 0, format: wgpu::VertexFormat::Float32x3 },
                wgpu::VertexAttribute { offset: 12, shader_location: 1, format: wgpu::VertexFormat::Float32x2 },
                wgpu::VertexAttribute { offset: 20, shader_location: 2, format: wgpu::VertexFormat::Uint32 },
            ],
        }
    }
}
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub water_index_start: u32,
    pub origin_x: i32,
    pub origin_y: i32,
}

const DIRECTIONS: [(i32, i32, i32); 6] =
    [(1,0,0),(-1,0,0),(0,1,0),(0,-1,0),(0,0,1),(0,0,-1)];
const CORNERS: [[[f32;3];4];6] = [
    [[1.,0.,0.],[1.,1.,0.],[1.,1.,1.],[1.,0.,1.]],
    [[0.,1.,0.],[0.,0.,0.],[0.,0.,1.],[0.,1.,1.]],
    [[1.,1.,0.],[0.,1.,0.],[0.,1.,1.],[1.,1.,1.]],
    [[0.,0.,0.],[1.,0.,0.],[1.,0.,1.],[0.,0.,1.]],
    [[0.,0.,1.],[1.,0.,1.],[1.,1.,1.],[0.,1.,1.]],
    [[0.,1.,0.],[1.,1.,0.],[1.,0.,0.],[0.,0.,0.]],
];
const UV: [[f32;2];4] = [[0.,1.],[1.,1.],[1.,0.],[0.,0.]];

fn block(t: &Terrain, x: i32, y: i32, z: i32, height: i32) -> Block {
    if !(0..WORLD_HEIGHT).contains(&z) { return Block::Air; }
    if let Some(&b) = t.edits.get(&(wrap(x), wrap(y), z)) { return b; }
    if z > height {
        if z <= SEA_LEVEL { Block::Water } else { Block::Air }
    } else if z == height {
        if height < SEA_LEVEL { Block::Dirt } else { Block::Grass }
    } else if z >= height - 3 {
        Block::Dirt
    } else {
        Block::Stone
    }
}

fn tile(b: Block, face: usize) -> u32 {
    match b {
        Block::Dirt => 0,
        Block::Grass if face == 4 => 1,
        Block::Grass => 2,
        Block::Stone => 3,
        Block::Water => 4,
        Block::Air => 0,
    }
}

pub fn build(t: &Terrain, center_x: i32, center_y: i32, radius: i32) -> Mesh {
    let origin_x = center_x.div_euclid(CHUNK_SIZE) * CHUNK_SIZE;
    let origin_y = center_y.div_euclid(CHUNK_SIZE) * CHUNK_SIZE;
    let mut result = Mesh { vertices: Vec::new(), indices: Vec::new(), water_index_start: 0, origin_x, origin_y };
    let mut water_indices = Vec::new();
    for cy in -radius..=radius {
        for cx in -radius..=radius {
            let base_x = origin_x + cx * CHUNK_SIZE;
            let base_y = origin_y + cy * CHUNK_SIZE;
            // 34x34 heights include the neighbor border; no noise calls in the voxel loop.
            let mut heights = [[0i32; 34]; 34];
            let mut max_height = SEA_LEVEL;
            for ly in -1..=32 {
                for lx in -1..=32 {
                    let h = t.height(base_x + lx, base_y + ly);
                    heights[(ly + 1) as usize][(lx + 1) as usize] = h;
                    max_height = max_height.max(h);
                }
            }
            // Include player-placed blocks above the original terrain surface.
            for (&(ex, ey, ez), _) in &t.edits {
                if (0..CHUNK_SIZE).contains(&delta(ex, base_x))
                    && (0..CHUNK_SIZE).contains(&delta(ey, base_y)) {
                    max_height = max_height.max(ez);
                }
            }
            for ly in 0..32 {
                for lx in 0..32 {
                    let x = base_x + lx;
                    let y = base_y + ly;
                    let h = heights[(ly + 1) as usize][(lx + 1) as usize];
                    // This is equivalent to traversing 32x32x32 sections in Z.
                    for z in 0..=(max_height + 1).min(WORLD_HEIGHT - 1) {
                        let b = block(t, x, y, z, h);
                        if b == Block::Air { continue; }
                        for (face, &(dx,dy,dz)) in DIRECTIONS.iter().enumerate() {
                            let nh = heights[(ly + dy + 1) as usize][(lx + dx + 1) as usize];
                            let neighbor = block(t, x + dx, y + dy, z + dz, nh);
                            let visible = if b == Block::Water {
                                // Only water's exposed upper surface: sides and bottoms
                                // are not useful until transparent water volumes exist.
                                face == 4 && neighbor == Block::Air
                            } else {
                                neighbor == Block::Air || neighbor == Block::Water
                            };
                            if !visible { continue; }
                            let start = result.vertices.len() as u32;
                            let px = delta(x, origin_x) as f32;
                            let py = delta(y, origin_y) as f32;
                            for i in 0..4 {
                                let c = CORNERS[face][i];
                                result.vertices.push(Vertex {
                                    pos: [px + c[0], py + c[1], z as f32 + c[2]],
                                    uv: UV[i],
                                    tile: tile(b, face),
                                });
                            }
                            let indices = if b == Block::Water { &mut water_indices } else { &mut result.indices };
                            indices.extend_from_slice(&[start,start+1,start+2,start,start+2,start+3]);
                        }
                    }
                }
            }
        }
    }
    result.water_index_start = result.indices.len() as u32;
    result.indices.extend(water_indices);
    result
}
