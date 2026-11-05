use noise::{NoiseFn, Perlin};
use std::collections::HashMap;
use crate::hydrology::WaterState;

pub const WORLD_SIZE: i32 = 1_024_000;
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
}
impl Terrain {
    pub fn new() -> Self { Self { noise: Perlin::new(42), edits: HashMap::new(), water: WaterState::default() } }

    // Periodic embedding of X/Y into four dimensions. The map has no seam.
    pub fn height(&self, x: i32, y: i32) -> i32 {
        let a = wrap(x) as f64 * std::f64::consts::TAU / WORLD_SIZE as f64;
        let b = wrap(y) as f64 * std::f64::consts::TAU / WORLD_SIZE as f64;
        let sample = |radius: f64| self.noise.get([
            a.cos() * radius, a.sin() * radius,
            b.cos() * radius, b.sin() * radius,
        ]);
        // Frequency is measured around the whole 1024 km torus. Small radii
        // barely change across a player's local view; use local scales too.
        (119.0 + sample(75.0) * 26.0 + sample(1_000.0) * 15.0
             + sample(9_000.0) * 10.0 + sample(45_000.0) * 2.5)
            .round().clamp(2.0, 250.0) as i32
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
        if (0..WORLD_HEIGHT).contains(&z) {
            let cell=(wrap(x),wrap(y),z);
            self.edits.insert(cell,block);
            self.water.activate(cell);
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
    fn edits_wrap_and_override() {
        let mut t = Terrain::new();
        t.set(-1, WORLD_SIZE, 120, Block::Dirt);
        assert_eq!(t.block(WORLD_SIZE - 1, 0, 120), Block::Dirt);
        t.set(-1, 0, 120, Block::Air);
        assert_eq!(t.block(WORLD_SIZE - 1, 0, 120), Block::Air);
    }
}
