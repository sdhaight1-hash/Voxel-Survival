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
#[repr(C)]
#[derive(Clone,Copy,Pod,Zeroable)]
pub struct ChunkOffset {pub xy:[f32;2]}
impl ChunkOffset {
    pub fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        wgpu::VertexBufferLayout {
            array_stride:std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode:wgpu::VertexStepMode::Instance,
            attributes:&[wgpu::VertexAttribute {
                offset:0,shader_location:3,format:wgpu::VertexFormat::Float32x2,
            }],
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

fn block(t: &Terrain, x:i32, y:i32, z:i32, height:i32) -> Block {
    if !(0..WORLD_HEIGHT).contains(&z) {return Block::Air;}
    let cell=(wrap(x),wrap(y),z);
    if let Some(&b)=t.edits.get(&cell) {
        if b.solid() {return b;}
    }
    if let Some(&volume)=t.water.amounts.get(&cell) {
        return if volume>0 {Block::Water} else {Block::Air};
    }
    // Excavated air starts dry regardless of elevation.
    if t.edits.contains_key(&cell) {return Block::Air;}
    if z>height {
        if z<t.initial_water_surface(x,y) {Block::Water} else {Block::Air}
    } else if z==height {
        if height<SEA_LEVEL {Block::Dirt} else {Block::Grass}
    } else if z>=height-3 {Block::Dirt}
    else {Block::Stone}
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
            // Keep edited underground tunnels visible without scanning the solid core.
            let mut minimum_edit_z = WORLD_HEIGHT;
            // Include player-placed blocks above the original terrain surface.
            for (&(ex, ey, ez), _) in &t.edits {
                if (0..CHUNK_SIZE).contains(&delta(ex, base_x))
                    && (0..CHUNK_SIZE).contains(&delta(ey, base_y)) {
                    max_height = max_height.max(ez);
                    minimum_edit_z = minimum_edit_z.min(ez);
                }
            }
            for (&(ex,ey,ez), _) in &t.water.amounts {
                if (0..CHUNK_SIZE).contains(&delta(ex,base_x))
                    && (0..CHUNK_SIZE).contains(&delta(ey,base_y)) {
                    max_height=max_height.max(ez);
                    minimum_edit_z=minimum_edit_z.min(ez);
                }
            }
            for ly in 0..32 {
                for lx in 0..32 {
                    let x = base_x + lx;
                    let y = base_y + ly;
                    let h = heights[(ly + 1) as usize][(lx + 1) as usize];
                    // Stone far below every exposed surface cannot contribute a face.
                    // Keep a few subsurface layers and any edited underground cells.
                    let lowest_neighbor = h
                        .min(heights[ly as usize][(lx + 1) as usize])
                        .min(heights[(ly + 2) as usize][(lx + 1) as usize])
                        .min(heights[(ly + 1) as usize][lx as usize])
                        .min(heights[(ly + 1) as usize][(lx + 2) as usize]);
                    let start_z = (lowest_neighbor - 5).min(minimum_edit_z - 1).max(0);
                    for z in start_z..=(max_height + 1).min(WORLD_HEIGHT - 1) {
                        let b = block(t, x, y, z, h);
                        if b == Block::Air { continue; }
                        for (face, &(dx,dy,dz)) in DIRECTIONS.iter().enumerate() {
                            let nh = heights[(ly + dy + 1) as usize][(lx + dx + 1) as usize];
                            let neighbor = block(t, x + dx, y + dy, z + dz, nh);
                            let current_fill = if b == Block::Water {
                                t.water_liters(x,y,z) as f32 / 1000.0
                            } else {1.0};
                            let neighbor_fill = if neighbor == Block::Water && face < 4 {
                                t.water_liters(x+dx,y+dy,z) as f32 / 1000.0
                            } else {0.0};
                            let visible = if b == Block::Water {
                                // Never draw an internal panel between adjacent
                                // water cells: only the portion ABOVE the lower
                                // neighbor water level is exposed.
                                (face == 4 && neighbor == Block::Air)
                                    || (face < 4 && (neighbor == Block::Air
                                        || neighbor == Block::Water)
                                        && current_fill > neighbor_fill)
                            } else {
                                neighbor == Block::Air || neighbor == Block::Water
                            };
                            if !visible { continue; }
                            let start = result.vertices.len() as u32;
                            let px = delta(x, origin_x) as f32;
                            let py = delta(y, origin_y) as f32;
                            for i in 0..4 {
                                let c = CORNERS[face][i];
                                let bottom = if b == Block::Water && face < 4 {
                                    neighbor_fill
                                } else {0.0};
                                let surface_z = if c[2] > 0.5 {
                                    current_fill
                                } else {bottom};
                                result.vertices.push(Vertex {
                                    pos: [px + c[0], py + c[1], z as f32 + surface_z],
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

#[cfg(test)]
mod water_mesh_tests {
    use super::*;
    use crate::terrain::WORLD_SIZE;

    #[test]
    fn partial_water_neighbors_emit_only_height_difference_not_full_panels() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        t.water.atmosphere_liters=1200;
        assert_eq!(t.precipitate((x,y,240),800),800);
        assert_eq!(t.precipitate((x+1,y,240),400),400);
        let m=build(&t,x,y,0);
        let start=m.water_index_start as usize;
        let mut found=false;
        for tri in m.indices[start..].chunks_exact(3) {
            let a=m.vertices[tri[0] as usize].pos;
            let b=m.vertices[tri[1] as usize].pos;
            let c=m.vertices[tri[2] as usize].pos;
            // The east-facing side of the 800L cell lies at local X=1,
            // Y 0..1 and Z 240.4..240.8.
            if [a,b,c].iter().all(|v|(v[0]-1.0).abs()<1e-5)
                && [a,b,c].iter().all(|v|v[1]>=0.0 && v[1]<=1.0)
                && [a,b,c].iter().all(|v|v[2]>=240.3999 && v[2]<=240.8001)
                && [a,b,c].iter().any(|v|(v[2]-240.4).abs()<1e-4)
                && [a,b,c].iter().any(|v|(v[2]-240.8).abs()<1e-4) {
                found=true;
            }
        }
        assert!(found,"missing exposed height-difference side");
    }

    #[test]
    fn placing_over_water_does_not_remove_exposed_neighbor_water() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        t.water.atmosphere_liters=800;
        assert_eq!(t.precipitate((x,y,240),800),800);
        // Solid directly above should occlude only that top, not the
        // east side bordering empty air.
        t.set(x,y,241,Block::Stone);
        let m=build(&t,x,y,0);
        assert!(m.indices.len()>m.water_index_start as usize);
        assert_eq!(t.water_liters(x,y,240),800);
    }
}
