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
    spring_remainder:f64,
}

#[derive(Default)]
pub struct SoilState {
    columns:HashMap<Column,SoilColumn>,
    active:HashSet<Column>,
    // Signed changes from the implicit baseline in finite topsoil storage.
    pub delta_soil_liters:i64,
    pub groundwater_liters:u64,
    // Fractional signed flow debts along canonical east/north edges.
    groundwater_flux_remainder:HashMap<(Column,u8),f64>,
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
            spring_remainder:0.0,
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
            spring_remainder:0.0,
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
            // Saturated groundwater may seep back into the surface.
            // This is a conservative transfer, not new runoff or a sink.
            let current_surface=self.water_liters(key.0,key.1,bed);
            let spring_supply=column.groundwater_l.saturating_sub(900)
                .min(1000u16.saturating_sub(current_surface));
            let seeped=transfer_rate(4.0,dt_climate_seconds,
                &mut column.spring_remainder,spring_supply);
            if seeped>0 {
                column.groundwater_l-=seeped;
                self.water_assign((key.0,key.1,bed),current_surface+seeped);
                changed+=1;
            }
            self.update_soil_column(key,column);
            // Keep wet ground updating only if more runoff could infiltrate
            // or gravity drainage can still occur. Inactive soil sleeps.
            if (surface>0 && column.moisture_l<mat.capacity_l)
                || (column.moisture_l>mat.field_capacity_l
                    && column.groundwater_l<1000)
                || column.groundwater_l>0 {
                self.soil.active.insert(key);
            }
        }
        changed
    }
    /// Conservative lateral groundwater exchange with a specific-yield
    /// approximation: 200 liters/m2 change the water table by 1 meter.
    /// Edge fluxes share one initial snapshot and are committed together.
    /// Small fractional flows are retained between climate steps, so 1x
    /// time and F1 time-lapse do not suppress slow subsurface movement.
    pub fn step_groundwater(&mut self,dt_climate_seconds:f64,max_columns:usize)->usize {
        if dt_climate_seconds<=0.0 {return 0;}
        let mut active:Vec<Column>=self.soil.active.iter().copied().collect();
        active.sort_unstable();
        active.truncate(max_columns);
        let mut edges=HashSet::new();
        for (x,y) in active {
            edges.insert(((x,y),0u8)); // east
            edges.insert(((x,y),1u8)); // north
            edges.insert(((wrap(x-1),y),0u8)); // west neighbor's east edge
            edges.insert(((x,wrap(y-1)),1u8)); // south neighbor's north edge
        }
        let mut edges:Vec<_>=edges.into_iter().collect();
        edges.sort_unstable();
        let mut taken:HashMap<Column,u16>=HashMap::new();
        let mut received:HashMap<Column,u16>=HashMap::new();
        let mut changed=0;
        for edge @ (from_edge,axis) in edges {
            let target=if axis==0 {(wrap(from_edge.0+1),from_edge.1)}
                else {(from_edge.0,wrap(from_edge.1+1))};
            if SoilMaterial::from_terrain(self,from_edge.0,from_edge.1).capacity_l==0
                || SoilMaterial::from_terrain(self,target.0,target.1).capacity_l==0 {
                self.soil.groundwater_flux_remainder.remove(&edge);
                continue;
            }
            let left=self.groundwater_liters(from_edge.0,from_edge.1);
            let right=self.groundwater_liters(target.0,target.1);
            // Equal hydraulic heads do not flow, even with different fill
            // fractions. The 6m datum cancels between neighboring columns.
            let head_left=self.height(from_edge.0,from_edge.1) as f64
                +left as f64/200.0;
            let head_right=self.height(target.0,target.1) as f64
                +right as f64/200.0;
            let gradient=head_left-head_right;
            if gradient.abs()<0.01 {
                self.soil.groundwater_flux_remainder.remove(&edge);
                continue;
            }
            let old=self.soil.groundwater_flux_remainder.get(&edge).copied().unwrap_or(0.0);
            let desired=old+gradient*3.0*dt_climate_seconds/3600.0;
            let (source,dest,sign)=if desired>0.0 {
                (from_edge,target,1.0)
            } else {(target,from_edge,-1.0)};
            let supply=self.groundwater_liters(source.0,source.1)
                .saturating_sub(*taken.get(&source).unwrap_or(&0));
            let room=1000u16.saturating_sub(self.groundwater_liters(dest.0,dest.1))
                .saturating_sub(*received.get(&dest).unwrap_or(&0));
            let requested=desired.abs().floor().min(u16::MAX as f64) as u16;
            let moved=requested.min(supply).min(room);
            if moved>0 {
                *taken.entry(source).or_default()+=moved;
                *received.entry(dest).or_default()+=moved;
                changed+=1;
            }
            // If a boundary is blocked or a donor runs dry, discard rather
            // than accumulate an impossible debt for future ticks.
            let remainder=if requested>moved || supply==0 || room==0 {
                0.0
            } else {desired-sign*moved as f64};
            if remainder.abs()>1e-12 {
                self.soil.groundwater_flux_remainder.insert(edge,remainder);
            } else {self.soil.groundwater_flux_remainder.remove(&edge);}
        }
        let mut updates:HashMap<Column,i32>=HashMap::new();
        for (key,l) in taken {*updates.entry(key).or_default()-=l as i32;}
        for (key,l) in received {*updates.entry(key).or_default()+=l as i32;}
        let mut updates:Vec<_>=updates.into_iter().collect();
        updates.sort_unstable_by_key(|&(key,_)|key);
        for (key,diff) in updates {
            if diff==0 {continue;}
            let mat=SoilMaterial::from_terrain(self,key.0,key.1);
            let mut col=self.soil.columns.get(&key).copied().unwrap_or(SoilColumn {
                moisture_l:mat.initial_l,groundwater_l:0,
                infiltration_remainder:0.0,percolation_remainder:0.0,
                spring_remainder:0.0,
            });
            let updated=col.groundwater_l as i32+diff;
            assert!((0..=1000).contains(&updated),"groundwater flux overdraw");
            col.groundwater_l=updated as u16;
            self.update_soil_column(key,col);
            self.soil.activate(key);
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
            spring_remainder:0.0,
        });
        t.water_assign((x,y,bed),90);
        let before=t.water_mass_error_liters();
        t.step_soil(72_000.0,128);
        // Saturated soil absorbs nothing, but its finite groundwater store
        // may discharge an 80 L spring over this 20-hour interval.
        assert_eq!(t.water_liters(x,y,bed),170);
        assert_eq!(t.groundwater_liters(x,y),920);
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
    fn groundwater_moves_down_hydraulic_head_without_mass_loss() {
        let mut t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2,WORLD_SIZE/2);
        let mat=SoilMaterial::from_terrain(&t,x,y);
        t.update_soil_column((x,y),SoilColumn {
            moisture_l:mat.initial_l,groundwater_l:800,
            infiltration_remainder:0.0,percolation_remainder:0.0,
            spring_remainder:0.0,
        });
        t.soil.activate((x,y));
        let before=t.water_mass_error_liters();
        assert!(t.step_groundwater(3600.0,128)>0);
        assert!(t.groundwater_liters(x,y)<800);
        let neighbor_sum=t.groundwater_liters(x+1,y) as u32
            +t.groundwater_liters(x-1,y) as u32
            +t.groundwater_liters(x,y+1) as u32
            +t.groundwater_liters(x,y-1) as u32;
        assert!(neighbor_sum>0);
        assert_eq!(t.water_mass_error_liters(),before);
    }
    #[test]
    fn groundwater_does_not_pass_into_impermeable_stone() {
        let mut t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2,WORLD_SIZE/2);
        let neighbor=(x+1,y);
        let h=t.height(neighbor.0,neighbor.1);
        t.set(neighbor.0,neighbor.1,h,Block::Stone);
        let mat=SoilMaterial::from_terrain(&t,x,y);
        t.update_soil_column((x,y),SoilColumn {
            moisture_l:mat.initial_l,groundwater_l:800,
            infiltration_remainder:0.0,percolation_remainder:0.0,
            spring_remainder:0.0,
        });
        t.soil.activate((x,y));
        let before=t.water_mass_error_liters();
        t.step_groundwater(3600.0,128);
        assert_eq!(t.groundwater_liters(neighbor.0,neighbor.1),0);
        assert_eq!(t.water_mass_error_liters(),before);
    }
    #[test]
    fn high_water_table_recharges_surface_as_finite_spring() {
        let mut t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2,WORLD_SIZE/2);
        let bed=t.height(x,y)+1;
        let mat=SoilMaterial::from_terrain(&t,x,y);
        t.update_soil_column((x,y),SoilColumn {
            moisture_l:mat.capacity_l,groundwater_l:1000,
            infiltration_remainder:0.0,percolation_remainder:0.0,
            spring_remainder:0.0,
        });
        t.soil.activate((x,y));
        let before=t.water_mass_error_liters();
        t.step_soil(3600.0,128);
        assert_eq!(t.groundwater_liters(x,y),996);
        assert_eq!(t.water_liters(x,y,bed),4);
        assert_eq!(t.water_mass_error_liters(),before);
    }
    #[test]
    fn springs_cannot_discharge_through_stone() {
        let mut t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2,WORLD_SIZE/2);
        let h=t.height(x,y);
        let mat=SoilMaterial::from_terrain(&t,x,y);
        t.update_soil_column((x,y),SoilColumn {
            moisture_l:mat.capacity_l,groundwater_l:1000,
            infiltration_remainder:0.0,percolation_remainder:0.0,
            spring_remainder:0.0,
        });
        t.set(x,y,h,Block::Stone);
        let before=t.water_mass_error_liters();
        t.step_soil(3600.0,128);
        assert_eq!(t.groundwater_liters(x,y),1000);
        assert_eq!(t.water_mass_error_liters(),before);
    }
    #[test]
    fn finite_percolation_moves_water_to_groundwater() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        t.update_soil_column((x,y),SoilColumn {
            moisture_l:250,groundwater_l:0,
            infiltration_remainder:0.0,percolation_remainder:0.0,
            spring_remainder:0.0,
        });
        t.soil.activate((x,y));
        let before=t.water_mass_error_liters();
        t.step_soil(3600.0,128);
        assert_eq!(t.soil_moisture_liters(x,y),242);
        assert_eq!(t.groundwater_liters(x,y),8);
        assert_eq!(t.water_mass_error_liters(),before);
    }
}
