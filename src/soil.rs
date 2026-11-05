//! Sparse, finite, column-based soil water and groundwater accounting.
//! Surface/soil/groundwater movement uses real stored liters.  Dry baseline
//! soil is implicit; only disturbed/wet columns are allocated.
use crate::terrain::{wrap, Block, Terrain, WORLD_SIZE};
use std::collections::{HashMap, HashSet};

type Column=(i32,i32);

#[derive(Clone,Copy,Debug)]
pub struct SoilColumn {
    pub moisture_l:u16,
    pub groundwater_l:u16,
    infiltration_remainder:f64,
    percolation_remainder:f64,
}

#[derive(Default)]
pub struct SoilState {
    columns:HashMap<Column,SoilColumn>,
    active:HashSet<Column>,
    // Signed changes from the implicit baseline in finite topsoil storage.
    pub delta_soil_liters:i64,
    pub groundwater_liters:u64,
}
impl SoilState {
    pub fn activate(&mut self,(x,y):Column) {
        self.active.insert((wrap(x),wrap(y)));
    }
    pub fn active_count(&self)->usize {self.active.len()}
}

#[derive(Clone,Copy,Debug)]
struct SoilMaterial {
    capacity_l:u16,
    field_capacity_l:u16,
    infiltration_l_hour:f64,
    percolation_l_hour:f64,
    initial_l:u16,
}
impl SoilMaterial {
    fn from_terrain(t:&Terrain,x:i32,y:i32)->Self {
        let h=t.height(x,y);
        match t.block(x,y,h) {
            Block::Grass | Block::Dirt => Self {
                capacity_l:400,field_capacity_l:180,
                infiltration_l_hour:30.0,percolation_l_hour:8.0,
                initial_l:if h<crate::terrain::SEA_LEVEL {180} else {80},
            },
            _ => Self {
                capacity_l:0,field_capacity_l:0,
                infiltration_l_hour:0.0,percolation_l_hour:0.0,initial_l:0,
            },
        }
    }
}
fn transfer_rate(rate_l_hour:f64,dt_seconds:f64,remainder:&mut f64,available:u16)->u16 {
    if available==0 || rate_l_hour<=0.0 {
        *remainder=0.0;
        return 0;
    }
    let desired=rate_l_hour*dt_seconds.max(0.0)/3600.0+*remainder;
    let amount=(desired.floor().max(0.0) as u32).min(available as u32) as u16;
    *remainder=if amount==available {0.0} else {(desired-amount as f64).clamp(0.0,0.999999999)};
    amount
}

impl Terrain {
    /// Returns the implicit moisture baseline unless the column is edited.
    pub fn soil_moisture_liters(&self,x:i32,y:i32)->u16 {
        let key=(wrap(x),wrap(y));
        self.soil.columns.get(&key).map(|c|c.moisture_l)
            .unwrap_or_else(||SoilMaterial::from_terrain(self,key.0,key.1).initial_l)
    }
    pub fn groundwater_liters(&self,x:i32,y:i32)->u16 {
        self.soil.columns.get(&(wrap(x),wrap(y)))
            .map_or(0,|c|c.groundwater_l)
    }
    pub fn water_mass_error_liters(&self)->i64 {
        self.water.surface_delta_liters+self.soil.delta_soil_liters
            +self.soil.groundwater_liters as i64
            +self.water.atmosphere_liters as i64
    }

    fn update_soil_column(&mut self,key:Column,new:SoilColumn) {
        let base=SoilMaterial::from_terrain(self,key.0,key.1).initial_l;
        let previous=self.soil.columns.get(&key).map_or(base,|c|c.moisture_l);
        let old_ground=self.soil.columns.get(&key).map_or(0,|c|c.groundwater_l);
        self.soil.delta_soil_liters+=new.moisture_l as i64-previous as i64;
        self.soil.groundwater_liters=self.soil.groundwater_liters
            .checked_add(new.groundwater_l as u64)
            .and_then(|v|v.checked_sub(old_ground as u64))
            .expect("groundwater ledger underflow");
        self.soil.columns.insert(key,new);
    }

    /// Evaporate accessible topsoil moisture to an external regional vapor
    /// inventory. The caller must credit exactly the returned liters.
    pub fn withdraw_soil_evaporation(&mut self,x:i32,y:i32,requested:u16)->u16 {
        let key=(wrap(x),wrap(y));
        let material=SoilMaterial::from_terrain(self,key.0,key.1);
        if material.capacity_l==0 {return 0;}
        let mut column=self.soil.columns.get(&key).copied().unwrap_or(SoilColumn {
            moisture_l:material.initial_l,groundwater_l:0,
            infiltration_remainder:0.0,percolation_remainder:0.0,
        });
        // Only near-surface accessible water evaporates. Drier soil retains
        // a finite residual; detailed capillary transport comes later.
        let moved=requested.min(column.moisture_l.saturating_sub(20));
        if moved>0 {
            column.moisture_l-=moved;
            self.update_soil_column(key,column);
        }
        moved
    }

    /// Advance only active columns; max 1m² of near-surface soil per column.
    /// The 1m deep topsoil has a finite pore volume, and finite groundwater
    /// storage below it. No off-world sink, infinite refill, or free water.
    pub fn step_soil(&mut self,dt_climate_seconds:f64,max_columns:usize)->usize {
        let mut pending:Vec<Column>=self.soil.active.drain().collect();
        pending.sort_unstable();
        let mut changed=0;
        for (i,key) in pending.into_iter().enumerate() {
            if i>=max_columns {self.soil.active.insert(key);continue;}
            let mat=SoilMaterial::from_terrain(self,key.0,key.1);
            if mat.capacity_l==0 {continue;}
            let mut column=self.soil.columns.get(&key).copied().unwrap_or(SoilColumn {
                moisture_l:mat.initial_l,groundwater_l:0,
                infiltration_remainder:0.0,percolation_remainder:0.0,
            });
            let bed=self.height(key.0,key.1)+1;
            let surface=if bed<crate::terrain::WORLD_HEIGHT {
                self.water_liters(key.0,key.1,bed)
            } else {0};
            let room=mat.capacity_l.saturating_sub(column.moisture_l);
            let can_infiltrate=room.min(surface);
            let infiltrated=transfer_rate(mat.infiltration_l_hour,dt_climate_seconds,
                &mut column.infiltration_remainder,can_infiltrate);
            if infiltrated>0 {
                self.water_assign((key.0,key.1,bed),surface-infiltrated);
                column.moisture_l+=infiltrated;
                changed+=1;
            }
            // Percolate only water exceeding field capacity into a finite
            // lower groundwater store. Saturated soil is not a magic sink.
            let drainable=column.moisture_l.saturating_sub(mat.field_capacity_l);
            let groundwater_room=1000u16.saturating_sub(column.groundwater_l);
            let percolated=transfer_rate(mat.percolation_l_hour,dt_climate_seconds,
                &mut column.percolation_remainder,drainable.min(groundwater_room));
            if percolated>0 {
                column.moisture_l-=percolated;
                column.groundwater_l+=percolated;
                changed+=1;
            }
            self.update_soil_column(key,column);
            // Keep wet ground updating only if more runoff could infiltrate
            // or gravity drainage can still occur. Inactive soil sleeps.
            if (surface>0 && column.moisture_l<mat.capacity_l)
                || (column.moisture_l>mat.field_capacity_l
                    && column.groundwater_l<1000) {
                self.soil.active.insert(key);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::WORLD_SIZE;
    #[test]
    fn dry_grass_absorbs_liters_but_closed_system_mass_does_not_change() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        let bed=t.height(x,y)+1;
        t.water_assign((x,y,bed),100);
        let before=t.water_mass_error_liters();
        let previous=t.soil_moisture_liters(x,y);
        t.step_soil(3600.0,128);
        assert_eq!(t.soil_moisture_liters(x,y),previous+30);
        assert_eq!(t.water_liters(x,y,bed),70);
        assert_eq!(t.water_mass_error_liters(),before);
    }
    #[test]
    fn soil_saturation_prevents_infinite_infiltration() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        let bed=t.height(x,y)+1;
        let material=SoilMaterial::from_terrain(&t,x,y);
        t.update_soil_column((x,y),SoilColumn{
            moisture_l:material.capacity_l,groundwater_l:1000,
            infiltration_remainder:0.0,percolation_remainder:0.0,
        });
        t.water_assign((x,y,bed),90);
        let before=t.water_mass_error_liters();
        t.step_soil(72_000.0,128);
        assert_eq!(t.water_liters(x,y,bed),90);
        assert_eq!(t.soil_moisture_liters(x,y),400);
        assert_eq!(t.water_mass_error_liters(),before);
    }
    #[test]
    fn rock_blocks_soil_absorption_and_groundwater_transfer() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        let h=t.height(x,y);
        t.set(x,y,h,Block::Stone);
        let bed=h+1;
        t.water_assign((x,y,bed),100);
        let before=t.water_mass_error_liters();
        t.step_soil(3600.0,128);
        assert_eq!(t.water_liters(x,y,bed),100);
        assert_eq!(t.water_mass_error_liters(),before);
    }
    #[test]
    fn finite_percolation_moves_water_to_groundwater() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        t.update_soil_column((x,y),SoilColumn {
            moisture_l:250,groundwater_l:0,
            infiltration_remainder:0.0,percolation_remainder:0.0,
        });
        t.soil.activate((x,y));
        let before=t.water_mass_error_liters();
        t.step_soil(3600.0,128);
        assert_eq!(t.soil_moisture_liters(x,y),242);
        assert_eq!(t.groundwater_liters(x,y),8);
        assert_eq!(t.water_mass_error_liters(),before);
    }
}
