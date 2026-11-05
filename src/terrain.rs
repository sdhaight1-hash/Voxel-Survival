use noise::{NoiseFn, Perlin};
use std::collections::HashMap;
use crate::hydrology::WaterState;
use crate::soil::SoilState;

pub const WORLD_SIZE: i32 = 1_024; // Hydrology lab only; main remains 1,024,000.
pub const WORLD_HEIGHT: i32 = 256;
pub const CHUNK_SIZE: i32 = 32;
pub const SEA_LEVEL: i32 = 110;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Block {
    Air = 0,
    Dirt = 1,
    Grass = 2,
    Stone = 3,
    Water = 4,
}
impl Block {
    pub fn solid(self) -> bool {
        matches!(self, Self::Dirt | Self::Grass | Self::Stone)
    }
}

pub fn wrap(n: i32) -> i32 { n.rem_euclid(WORLD_SIZE) }
pub fn delta(n: i32, origin: i32) -> i32 {
    let d = (n - origin).rem_euclid(WORLD_SIZE);
    if d > WORLD_SIZE / 2 { d - WORLD_SIZE } else { d }
}

pub struct Terrain {
    noise: Perlin,
    pub(crate) edits: HashMap<(i32, i32, i32), Block>,
    pub(crate) water: WaterState,
    pub(crate) soil: SoilState,
}
impl Terrain {
    pub fn new() -> Self { Self { noise: Perlin::new(42), edits: HashMap::new(), water: WaterState::default(), soil:SoilState::default() } }

    // Coordinates below use toroidal *meters* relative to the map center.
    // The lab is deliberately authored for testable waterways; production
    // geology and climate generation are not being replaced.
    fn lab_xy(x:i32,y:i32) -> (f64,f64) {
        (delta(wrap(x),WORLD_SIZE/2) as f64,
         delta(wrap(y),WORLD_SIZE/2) as f64)
    }
    fn river(x:f64,y:f64) -> Option<(f64,f64)> {
        if !(-125.0..=365.0).contains(&x) {return None;}
        let t=((x+115.0)/475.0).clamp(0.0,1.0);
        let channel_y=125.0-190.0*t+(t*std::f64::consts::TAU*2.0).sin()*9.0;
        let distance=(y-channel_y).abs();
        // The river falls from lake surface +126m to ocean +110m.
        let surface=126.0-16.0*t;
        Some((distance,surface))
    }
    fn lake_distance(x:f64,y:f64) -> f64 {
        ((x+115.0).powi(2)+(y-125.0).powi(2)).sqrt()
    }
    fn swamp_distance(x:f64,y:f64) -> f64 {
        (((x-115.0)/85.0).powi(2)+((y+125.0)/62.0).powi(2)).sqrt()
    }
    /// Initial finite free-surface elevation. This is an INITIAL CONDITION,
    /// not a refilling source: edited/depleted cells use sparse overrides.
    pub fn initial_water_surface(&self,x:i32,y:i32) -> i32 {
        let (x,y)=Self::lab_xy(x,y);
        if Self::lake_distance(x,y)<78.0 {return 126;}
        if let Some((distance,surface))=Self::river(x,y) {
            if distance<=9.0 {return surface.floor() as i32;}
        }
        if Self::swamp_distance(x,y)<1.0 {return 115;}
        SEA_LEVEL
    }

    pub fn height(&self,x:i32,y:i32) -> i32 {
        let (x,y)=Self::lab_xy(x,y);
        let radius=x.hypot(y);
        // Broad coastal ramp: ~52m of rise spread across 240m, not the
        // former steep 115m band. The inland plateau is easy to traverse.
        // Outside radius 465m the seabed is flat at Z=76 on every seam.
        let island=((465.0-radius)/240.0).clamp(0.0,1.0);
        let island=island*island*(3.0-2.0*island);
        let phase_x=std::f64::consts::TAU*wrap(x as i32+WORLD_SIZE/2) as f64/WORLD_SIZE as f64;
        let phase_y=std::f64::consts::TAU*wrap(y as i32+WORLD_SIZE/2) as f64/WORLD_SIZE as f64;
        let noise=self.noise.get([
            phase_x.cos()*32.0,phase_x.sin()*32.0,
            phase_y.cos()*32.0,phase_y.sin()*32.0,
        ]);
        let mut h=76.0+island*(52.0+noise*1.8
            +1.4*(x/58.0).sin()*(y/66.0).cos());

        // Basin above sea level: the lake has a level surface at +126m.
        let lake=Self::lake_distance(x,y);
        if lake<112.0 {
            let bed=115.0+10.0*(lake/78.0).powi(2);
            let blend=((112.0-lake)/34.0).clamp(0.0,1.0);
            h=h*(1.0-blend)+h.min(bed)*blend;
        }
        // Winding channel with sloping bed; broad banks are dry and only
        // its central 9m are initially inundated.
        if let Some((distance,surface))=Self::river(x,y) {
            if distance<30.0 {
                let bed=surface-2.0+0.1*(x/20.0).sin();
                let blend=((30.0-distance)/21.0).clamp(0.0,1.0);
                h=h*(1.0-blend)+h.min(bed)*blend;
            }
        }
        // Small dry hummocks and shallow wet pockets in a low marsh basin.
        let swamp=Self::swamp_distance(x,y);
        if swamp<1.5 {
            let bed=114.5+1.6*(x/26.0).sin()*(y/24.0).cos();
            let blend=((1.5-swamp)/0.5).clamp(0.0,1.0);
            h=h*(1.0-blend)+h.min(bed)*blend;
        }
        h.round().clamp(2.0,250.0) as i32
    }

    pub fn block(&self, x: i32, y: i32, z: i32) -> Block {
        if !(0..WORLD_HEIGHT).contains(&z) { return Block::Air; }
        let x=wrap(x);
        let y=wrap(y);
        let cell=(x,y,z);
        if let Some(&b)=self.edits.get(&cell) {
            if b.solid() {return b;}
        }
        if self.water_liters(x,y,z)>0 {return Block::Water;}
        if self.edits.contains_key(&cell) {return Block::Air;}
        let h=self.height(x,y);
        if z>h {Block::Air}
        else if z==h {if h<SEA_LEVEL {Block::Dirt} else {Block::Grass}}
        else if z>=h-3 {Block::Dirt}
        else {Block::Stone}
    }
    pub fn set(&mut self, x:i32, y:i32, z:i32, block:Block) {
        // Liquid is moved only through the finite-volume hydrology API.
        if block == Block::Water {return;}
        if (0..WORLD_HEIGHT).contains(&z) {
            let cell=(wrap(x),wrap(y),z);
            self.edits.insert(cell,block);
            self.water.activate(cell);
            self.soil.activate((cell.0,cell.1));
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrap_edges() {
        assert_eq!(wrap(-1), WORLD_SIZE - 1);
        assert_eq!(wrap(WORLD_SIZE), 0);
        assert_eq!(delta(0, WORLD_SIZE - 1), 1);
        assert_eq!(delta(WORLD_SIZE - 1, 0), -1);
    }
    #[test]
    fn terrain_is_periodic() {
        let t = Terrain::new();
        for (x, y) in [(0, 0), (1234, 5678), (WORLD_SIZE - 1, 9)] {
            assert_eq!(t.height(x, y), t.height(x + WORLD_SIZE, y));
            assert_eq!(t.height(x, y), t.height(x, y - WORLD_SIZE));
        }
    }
    #[test]
    fn terrain_has_local_relief() {
        let t = Terrain::new();
        let heights: Vec<_> = (0..64).step_by(4)
            .flat_map(|y| (0..64).step_by(4)
                .map(move |x| (x, y)))
            .map(|(x,y)| t.height(WORLD_SIZE / 2 + x, WORLD_SIZE / 2 + y))
            .collect();
        assert!(heights.iter().max().unwrap() - heights.iter().min().unwrap() >= 2);
    }
    #[test]
    fn starting_area_is_dry() {
        let t = Terrain::new();
        assert!(t.height(WORLD_SIZE / 2, WORLD_SIZE / 2) > SEA_LEVEL + 2);
    }
    #[test]
    fn inland_ground_is_gently_rolling_and_walkable() {
        let t=Terrain::new();
        let m=WORLD_SIZE/2;
        // A representative path from spawn toward the wetland, remaining
        // outside its intentionally excavated shallow water pockets.
        let path:(Vec<i32>,i32)=((-100..=110).collect(),m-35);
        let heights:Vec<_>=path.0.iter().map(|&dx|t.height(m+dx,path.1)).collect();
        let spread=heights.iter().max().unwrap()-heights.iter().min().unwrap();
        assert!(spread<=8,"inland plateau too uneven: {spread}m");
        for pair in heights.windows(2) {
            assert!((pair[1]-pair[0]).abs()<=1,
                "rolling terrain created a tall walking wall: {:?}",pair);
        }
    }
    #[test]
    fn island_has_ocean_on_all_seams_and_dry_center() {
        let t=Terrain::new();
        let m=WORLD_SIZE/2;
        assert!(t.height(m,m)>SEA_LEVEL+3);
        for &(x,y) in &[(0,m),(WORLD_SIZE-1,m),(m,0),(m,WORLD_SIZE-1),(0,0)] {
            assert!(t.height(x,y)<SEA_LEVEL,"seam must be ocean at {x},{y}");
            assert_eq!(t.initial_water_surface(x,y),SEA_LEVEL);
        }
    }
    #[test]
    fn lake_is_elevated_and_river_descends_to_ocean() {
        let t=Terrain::new();
        let m=WORLD_SIZE/2;
        let lake=(m-115,m+125);
        assert!(t.height(lake.0,lake.1)<120);
        assert_eq!(t.initial_water_surface(lake.0,lake.1),126);
        assert_eq!(t.block(lake.0,lake.1,125),Block::Water);
        assert_eq!(t.block(lake.0,lake.1,127),Block::Air);
        let river_a=(m-40,m+95);
        let river_b=(m+250,m-19);
        assert!(t.initial_water_surface(river_a.0,river_a.1)
            > t.initial_water_surface(river_b.0,river_b.1));
    }
    #[test]
    fn swamp_has_shallow_pools_and_dry_hummocks() {
        let t=Terrain::new();
        let m=WORLD_SIZE/2;
        let mut wet=0;
        let mut dry=0;
        for dy in -25..=25 {
            for dx in -40..=40 {
                let x=m+115+dx;let y=m-125+dy;
                if t.height(x,y)<115 {wet+=1;} else {dry+=1;}
            }
        }
        assert!(wet>100 && dry>100,"marsh should mix water and dry hummocks");
    }
    #[test]
    fn edits_wrap_and_override() {
        let mut t = Terrain::new();
        t.set(-1, WORLD_SIZE, 120, Block::Dirt);
        assert_eq!(t.block(WORLD_SIZE - 1, 0, 120), Block::Dirt);
        t.set(-1, 0, 120, Block::Air);
        assert_eq!(t.block(WORLD_SIZE - 1, 0, 120), Block::Air);
    }
}
