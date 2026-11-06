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
    // Rendering reads the authoritative finite liquid inventory, not a
    // second copy of the implicit ocean/lake initialization rule.
    if t.water_liters(x,y,z)>0 {return Block::Water;}
    // Excavated air starts dry regardless of elevation.
    if t.edits.contains_key(&cell) {return Block::Air;}
    if z>height {Block::Air}
    else if z==height {
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
    build_internal(t,center_x,center_y,radius,false)
}

/// Fast path for dynamic water refreshes: don't rebuild opaque faces.
pub fn build_water(t:&Terrain, center_x:i32, center_y:i32, radius:i32)->Mesh {
    build_internal(t,center_x,center_y,radius,true)
}

fn build_internal(t:&Terrain,center_x:i32,center_y:i32,radius:i32,water_only:bool)->Mesh {
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
            let mut surfaces = [[0i32; 34]; 34];
            let mut max_height = SEA_LEVEL;
            for ly in -1..=32 {
                for lx in -1..=32 {
                    let h = t.height(base_x + lx, base_y + ly);
                    heights[(ly + 1) as usize][(lx + 1) as usize] = h;
                    let surface=t.initial_water_surface(base_x+lx,base_y+ly);
                    surfaces[(ly + 1) as usize][(lx + 1) as usize]=surface;
                    max_height=max_height.max(h).max(surface);
                }
            }
            // Per-column water-only work bounds. Calm ocean columns need
            // just their top layer, not every voxel down to the seabed.
            // When a cell is modified, include its height and neighboring
            // levels so gapped water columns and cave faces are not lost.
            let mut changed_low=[[WORLD_HEIGHT;34];34];
            let mut changed_high=[[-1i32;34];34];
            let mut explicitly_wet=[[false;34];34];
            // Keep edited underground tunnels visible in full opaque meshes.
            let mut minimum_edit_z = WORLD_HEIGHT;
            // Include player-placed blocks above the original terrain surface.
            for (&(ex,ey,ez),_) in &t.edits {
                let lx=delta(ex,base_x);
                let ly=delta(ey,base_y);
                if (-1..=CHUNK_SIZE).contains(&lx)
                    && (-1..=CHUNK_SIZE).contains(&ly) {
                    changed_low[(ly+1) as usize][(lx+1) as usize]=
                        changed_low[(ly+1) as usize][(lx+1) as usize].min(ez-1);
                    if (0..CHUNK_SIZE).contains(&lx)
                        && (0..CHUNK_SIZE).contains(&ly) {
                        max_height=max_height.max(ez);
                        minimum_edit_z=minimum_edit_z.min(ez);
                    }
                }
            }
            // Include the 1-cell border for exposed water side faces.
            // The other eight neighbor-chunk indexes are only needed when
            // modifications fall directly on a chunk boundary.
            for cy in -1..=1 {
                for cx in -1..=1 {
                    let key=(wrap(base_x+cx*CHUNK_SIZE),
                        wrap(base_y+cy*CHUNK_SIZE));
                    if let Some(changed)=t.water.changed_by_chunk.get(&key) {
                        for &(ex,ey,ez) in changed {
                            let lx=delta(ex,base_x);
                            let ly=delta(ey,base_y);
                            if !(-1..=CHUNK_SIZE).contains(&lx)
                                || !(-1..=CHUNK_SIZE).contains(&ly) {continue;}
                            let iy=(ly+1) as usize;
                            let ix=(lx+1) as usize;
                            changed_low[iy][ix]=changed_low[iy][ix].min(ez-1);
                            changed_high[iy][ix]=changed_high[iy][ix].max(ez);
                            if t.water_liters(ex,ey,ez)>0 {
                                explicitly_wet[iy][ix]=true;
                            }
                            if (0..CHUNK_SIZE).contains(&lx)
                                && (0..CHUNK_SIZE).contains(&ly) {
                                max_height=max_height.max(ez);
                                minimum_edit_z=minimum_edit_z.min(ez);
                            }
                        }
                    }
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
                    let (start_z,end_z)=if water_only {
                        let iy=(ly+1) as usize;
                        let ix=(lx+1) as usize;
                        let upper=(surfaces[iy][ix]-1)
                            .max(changed_high[iy][ix]);
                        // No initial water and no modified wet cells.
                        if surfaces[iy][ix]<=h+1 && !explicitly_wet[iy][ix] {
                            continue;
                        }
                        let mut low=surfaces[iy][ix]-1;
                        for (oy,ox) in [(0isize,0isize),(-1,0),(1,0),(0,-1),(0,1)] {
                            let ny=(iy as isize+oy) as usize;
                            let nx=(ix as isize+ox) as usize;
                            low=low.min(surfaces[ny][nx]-1)
                                .min(changed_low[ny][nx]);
                        }
                        (low.max(0),upper.min(WORLD_HEIGHT-1))
                    } else {
                        ((lowest_neighbor-5).min(minimum_edit_z-1).max(0),
                            (max_height+1).min(WORLD_HEIGHT-1))
                    };
                    if start_z>end_z {continue;}
                    for z in start_z..=end_z {
                        let b=if water_only {
                            if t.water_liters(x,y,z)==0 {continue;}
                            Block::Water
                        } else {
                            let b=block(t,x,y,z,h);
                            if b==Block::Air {continue;}
                            b
                        };
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
    fn dynamic_water_only_mesh_matches_full_chunk_water_faces() {
        let mut t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2-115,WORLD_SIZE/2+125);
        let bed=t.height(x,y)+1;
        t.water_assign((x,y,bed),750);
        let whole=build(&t,x,y,0);
        let liquid=build_water(&t,x,y,0);
        assert_eq!(liquid.water_index_start,0);
        assert_eq!(whole.indices.len()-whole.water_index_start as usize,
            liquid.indices.len());
        let full_faces:Vec<_>=whole.indices[whole.water_index_start as usize..]
            .iter().map(|&i|whole.vertices[i as usize].pos).collect();
        let liquid_faces:Vec<_>=liquid.indices.iter()
            .map(|&i|liquid.vertices[i as usize].pos).collect();
        assert_eq!(full_faces,liquid_faces);
    }
    #[test]
    fn optimized_water_scan_preserves_full_water_faces_at_shores_and_caves() {
        fn parity(t:&Terrain,x:i32,y:i32) {
            let whole=build(t,x,y,0);
            let liquid=build_water(t,x,y,0);
            let first:Vec<_>=whole.indices[whole.water_index_start as usize..]
                .iter().map(|&i|whole.vertices[i as usize].pos).collect();
            let second:Vec<_>=liquid.indices.iter()
                .map(|&i|liquid.vertices[i as usize].pos).collect();
            assert_eq!(first,second,"water-only mesh changed actual geometry");
        }
        let mut t=Terrain::new();
        let m=WORLD_SIZE/2;
        // Untouched water, river bank and the island's periodic ocean seam.
        parity(&t,0,0);
        parity(&t,m-115+78,m+125);
        // Artificial partial waterfall and a lower gap in an elevated lake.
        let lake=(m-115,m+125);
        let bed=t.height(lake.0,lake.1)+1;
        t.water_assign((lake.0,lake.1,bed),0);
        t.water_assign((lake.0+1,lake.1,125),420);
        parity(&t,lake.0,lake.1);
        // Roofed cave water and a separate, high unsupported cell.
        let rock=(m,m,t.height(m,m)-3);
        t.set(rock.0,rock.1,rock.2,Block::Air);
        t.water_assign(rock,675);
        t.water_assign((m,m,238),620);
        parity(&t,m,m);
        // The neighbor chunk must show the changed edge correctly.
        t.water_assign((31,0,109),700);
        parity(&t,0,0);
        parity(&t,32,0);
    }
    #[test]
    fn water_mesh_uses_authoritative_liquid_inventory_after_depletion() {
        let mut t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2-115,WORLD_SIZE/2+125);
        assert_eq!(block(&t,x,y,125,t.height(x,y)),Block::Water);
        t.water_assign((x,y,125),0);
        assert_eq!(block(&t,x,y,125,t.height(x,y)),Block::Air);
        assert_eq!(t.water_liters(x,y,125),0);
    }
    #[test]
    fn sparse_water_edits_remain_in_chunk_mesh_after_indexing() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        t.water_assign((x,y,240),700);
        t.water_assign((x+32,y,246),200);
        let m=build(&t,x,y,0);
        assert!(m.indices.len()>m.water_index_start as usize);
        assert!(m.indices[m.water_index_start as usize..].iter()
            .any(|&i| m.vertices[i as usize].pos[2]>=240.6));
        assert!(m.vertices.iter().all(|v|v.pos[2]<246.0),
            "off-chunk water edits must not bleed into local mesh");
    }
    #[test]
    fn elevated_lake_mesh_reaches_absolute_initial_water_surface() {
        let t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2-115,WORLD_SIZE/2+125);
        assert_eq!(t.initial_water_surface(x,y),126);
        assert!(t.height(x,y)<125);
        let m=build(&t,x,y,0);
        let water_vertices=m.indices[m.water_index_start as usize..].iter()
            .map(|&index|m.vertices[index as usize].pos);
        assert!(water_vertices.into_iter().any(|p|(p[2]-126.0).abs()<1e-5),
            "the elevated lake must render its free surface at Z=126");
    }
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
