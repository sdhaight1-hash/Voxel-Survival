//! Finite water bookkeeping. One voxel has capacity 1000 liters (1 m³).
//! The initial ocean is a finite implicit volume field, never an infinite
//! refill rule. Each withdrawn voxel is recorded as a sparse override.
use crate::terrain::{wrap, Block, Terrain, SEA_LEVEL, WORLD_HEIGHT};
use std::collections::{HashMap, HashSet};

pub type Cell = (i32, i32, i32);
pub const CAPACITY_L: u16 = 1000;

pub fn canonical((x,y,z): Cell) -> Cell {(wrap(x),wrap(y),z)}
fn neighbors((x,y,z): Cell) -> [Cell;6] {
    [(x,y,z-1),(x-1,y,z),(x+1,y,z),(x,y-1,z),(x,y+1,z),(x,y,z+1)]
}

#[derive(Default)]
pub struct WaterState {
    pub(crate) amounts: HashMap<Cell,u16>,
    pub(crate) active: HashSet<Cell>,
    pub(crate) dirty: HashSet<Cell>,
    // Atmosphere is an explicitly finite reservoir, not a source of free rain.
    pub atmosphere_liters: u64,
}
impl WaterState {
    pub fn activate(&mut self, at:Cell) {
        let at=canonical(at);
        self.active.insert(at);
        for n in neighbors(at) {
            if (0..WORLD_HEIGHT).contains(&n.2) {
                self.active.insert(canonical(n));
            }
        }
    }
}
impl Terrain {
    pub fn water_liters(&self, x:i32,y:i32,z:i32) -> u16 {
        if !(0..WORLD_HEIGHT).contains(&z) {return 0;}
        let cell=(wrap(x),wrap(y),z);
        if let Some(b)=self.edits.get(&cell) {
            if b.solid() {return 0;}
            if *b==Block::Water {return CAPACITY_L;}
        }
        if let Some(&volume)=self.water.amounts.get(&cell) {return volume;}
        // New empty cells created by mining are empty until water moves in.
        if self.edits.contains_key(&cell) {return 0;}
        if z > self.height(cell.0,cell.1) && z<=SEA_LEVEL {
            CAPACITY_L
        } else {0}
    }
    fn water_assign(&mut self, cell:Cell, amount:u16) {
        let cell=canonical(cell);
        self.water.amounts.insert(cell,amount.min(CAPACITY_L));
        self.water.dirty.insert(cell);
        self.water.activate(cell);
    }
    pub fn take_water_dirty(&mut self) -> HashSet<Cell> {
        std::mem::take(&mut self.water.dirty)
    }
    /// Move existing mass, never synthesize water when a cavity is excavated.
    fn transfer(&mut self, from:Cell, to:Cell, limit:u16) -> u16 {
        let from=canonical(from);
        let to=canonical(to);
        if from==to || !(0..WORLD_HEIGHT).contains(&to.2)
            || self.block(to.0,to.1,to.2).solid() {return 0;}
        let a=self.water_liters(from.0,from.1,from.2);
        let b=self.water_liters(to.0,to.1,to.2);
        let move_l=a.min(CAPACITY_L-b).min(limit);
        if move_l>0 {
            self.water_assign(from,a-move_l);
            self.water_assign(to,b+move_l);
        }
        move_l
    }
    /// Process bounded active cells. A 0-volume override remembers depleted
    /// ocean water, including when the chunk is unloaded and later revisited.
    /// Returns the number of transfers for mesh invalidation.
    pub fn step_water(&mut self, max_cells:usize) -> usize {
        let mut pending:Vec<Cell>=self.water.active.drain().collect();
        pending.sort_unstable();
        let mut transfers=0;
        for (i,cell) in pending.into_iter().enumerate() {
            if i>=max_cells {
                self.water.active.insert(cell);
                continue;
            }
            if self.water_liters(cell.0,cell.1,cell.2)==0 {continue;}
            let below=(cell.0,cell.1,cell.2-1);
            if self.transfer(cell,below,500)>0 {transfers+=1;}
            for n in [ (cell.0-1,cell.1,cell.2),
                       (cell.0+1,cell.1,cell.2),
                       (cell.0,cell.1-1,cell.2),
                       (cell.0,cell.1+1,cell.2) ] {
                if self.block(n.0,n.1,n.2).solid() {continue;}
                let a=self.water_liters(cell.0,cell.1,cell.2);
                let b=self.water_liters(n.0,n.1,n.2);
                if a>b+1 && self.transfer(cell,n,(a-b)/4)>0 {
                    transfers+=1;
                }
            }
        }
        transfers
    }
    /// Evaporation transfers liquid to atmosphere; precipitation reverses it.
    /// Neither operation can increase the closed-system total.
    pub fn evaporate(&mut self, at:Cell, requested:u16) -> u16 {
        let at=canonical(at);
        let a=self.water_liters(at.0,at.1,at.2);
        let moved=a.min(requested);
        if moved>0 {
            self.water_assign(at,a-moved);
            self.water.atmosphere_liters+=moved as u64;
        }
        moved
    }
    pub fn precipitate(&mut self, at:Cell, requested:u16) -> u16 {
        let at=canonical(at);
        if self.block(at.0,at.1,at.2).solid() {return 0;}
        let a=self.water_liters(at.0,at.1,at.2);
        let moved=(CAPACITY_L-a).min(requested)
            .min(self.water.atmosphere_liters.min(u16::MAX as u64) as u16);
        if moved>0 {
            self.water.atmosphere_liters-=moved as u64;
            self.water_assign(at,a+moved);
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::WORLD_SIZE;
    #[test]
    fn a_mined_cavity_does_not_generate_water() {
        let mut t=Terrain::new();
        let x=0;
        let y=0;
        let z=50;
        t.set(x,y,z,Block::Air);
        assert_eq!(t.water_liters(x,y,z),0);
    }
    #[test]
    fn rain_is_limited_by_atmospheric_storage() {
        let mut t=Terrain::new();
        let at=(WORLD_SIZE/2,WORLD_SIZE/2,240);
        t.water.atmosphere_liters=300;
        assert_eq!(t.precipitate(at,700),300);
        assert_eq!(t.water.atmosphere_liters,0);
        assert_eq!(t.evaporate(at,125),125);
        assert_eq!(t.water.atmosphere_liters,125);
        assert_eq!(t.water_liters(at.0,at.1,at.2),175);
    }
    #[test]
    fn water_transfers_conserve_local_mass_and_wrap() {
        let mut t=Terrain::new();
        let from=(WORLD_SIZE-1,0,240);
        let to=(0,0,240);
        t.water_assign(from,1000);
        t.water_assign(to,0);
        let before=t.water_liters(from.0,from.1,from.2) as u32
            +t.water_liters(to.0,to.1,to.2) as u32;
        assert_eq!(t.transfer(from,to,200),200);
        assert_eq!(t.water_liters(from.0,from.1,from.2) as u32
            +t.water_liters(to.0,to.1,to.2) as u32,before);
    }
    #[test]
    fn water_flows_down_into_empty_cell() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        t.water_assign((x,y,240),1000);
        t.water_assign((x,y,239),0);
        let before=t.water_liters(x,y,239)+t.water_liters(x,y,240);
        t.step_water(128);
        assert!(t.water_liters(x,y,239)>0);
        // Water may spread laterally during the same tick. Count *every*
        // modified cell, not only the original source/destination pair.
        let total: u32=t.water.amounts.values().map(|&v| v as u32).sum();
        assert_eq!(before as u32,total);
    }
}
