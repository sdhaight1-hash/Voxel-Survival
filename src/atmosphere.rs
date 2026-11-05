//! A separate, finite, coarse atmospheric water inventory. Atmospheric
//! moisture is NOT an air voxel, a biome attribute, or a humidity percentage.
//! The laboratory atmosphere is 16x16 periodic 64m regions, one 2000m layer.
//! Wind is a prescribed passive-tracer field, not solved air dynamics.
use crate::terrain::{wrap,WORLD_SIZE};
use std::f64::consts::TAU;

pub const REGION_SIZE_M:i32=64;
pub const N:usize=(WORLD_SIZE/REGION_SIZE_M) as usize;
const CELLS:usize=N*N;
const AREA_M2:f64=(REGION_SIZE_M*REGION_SIZE_M) as f64;
const LAYER_HEIGHT_M:f64=2000.0;
const AIR_MASS_KG:f64=AREA_M2*LAYER_HEIGHT_M*1.18;
const PRESSURE_PA:f64=101_325.0;

#[derive(Clone,Copy,Debug)]
pub struct AirCell {
    pub vapor_kg:f64,
    pub cloud_kg:f64,
    pub temperature_c:f64,
    pub wind_x_m_s:f64,
    pub wind_y_m_s:f64,
}

pub struct Atmosphere {
    pub cells:Vec<AirCell>,
    pub time_seconds:f64,
    initial_water_kg:f64,
}

impl Atmosphere {
    pub fn new_af()->Self {
        let temp=27.0;
        let q_sat=saturation_specific_humidity(temp);
        // RH is an INITIAL CONDITION only: vapor mass remains authoritative.
        let water=q_sat*AIR_MASS_KG*0.83;
        let cells=vec![AirCell {
            vapor_kg:water,cloud_kg:0.0,temperature_c:temp,
            wind_x_m_s:2.0,wind_y_m_s:0.5,
        };CELLS];
        Self {
            cells,
            time_seconds:0.0,
            initial_water_kg:water*CELLS as f64,
        }
    }
    pub fn cell_index(x:i32,y:i32)->usize {
        let rx=wrap(x).div_euclid(REGION_SIZE_M) as usize;
        let ry=wrap(y).div_euclid(REGION_SIZE_M) as usize;
        ry*N+rx
    }
    pub fn relative_humidity(&self,x:i32,y:i32)->f64 {
        let cell=&self.cells[Self::cell_index(x,y)];
        let q=cell.vapor_kg/(AIR_MASS_KG+cell.vapor_kg);
        q/saturation_specific_humidity(cell.temperature_c)
    }
    pub fn vapor_density_kg_m3(&self,x:i32,y:i32)->f64 {
        self.cells[Self::cell_index(x,y)].vapor_kg/(AREA_M2*LAYER_HEIGHT_M)
    }
    pub fn total_water_kg(&self)->f64 {
        self.cells.iter().map(|c|c.vapor_kg+c.cloud_kg).sum()
    }
    pub fn water_delta_kg(&self)->f64 {
        self.total_water_kg()-self.initial_water_kg
    }
    /// Transfer finite evaporation into this region from another reservoir.
    pub fn add_vapor(&mut self,x:i32,y:i32,liters:f64) {
        assert!(liters>=0.0 && liters.is_finite());
        self.cells[Self::cell_index(x,y)].vapor_kg+=liters;
    }
    /// Remove actual cloud mass for explicitly accounted precipitation.
    pub fn withdraw_cloud(&mut self,x:i32,y:i32,requested_liters:f64)->f64 {
        let cell=&mut self.cells[Self::cell_index(x,y)];
        let moved=cell.cloud_kg.min(requested_liters.max(0.0));
        cell.cloud_kg-=moved;
        moved
    }
    /// Conservatively forward-remap moisture over a periodic weather grid.
    /// This remains nonnegative even when F1 jumps thousands of seconds.
    /// A full weather engine would also advect air mass/heat and wind itself.
    pub fn transport(&mut self,dt_seconds:f64) {
        if dt_seconds<=0.0{return;}
        let mut vapor=vec![0.0;CELLS];
        let mut cloud=vec![0.0;CELLS];
        for (index,source) in self.cells.iter().enumerate() {
            let x=(index%N) as f64+
                source.wind_x_m_s*dt_seconds/REGION_SIZE_M as f64;
            let y=(index/N) as f64+
                source.wind_y_m_s*dt_seconds/REGION_SIZE_M as f64;
            let x=x.rem_euclid(N as f64);
            let y=y.rem_euclid(N as f64);
            let x0=x.floor() as usize;
            let y0=y.floor() as usize;
            let fx=x-x0 as f64;let fy=y-y0 as f64;
            for (dy,wy) in [(0,1.0-fy),(1,fy)] {
                for (dx,wx) in [(0,1.0-fx),(1,fx)] {
                    let to=((y0+dy)%N)*N+(x0+dx)%N;
                    let weight=wx*wy;
                    vapor[to]+=source.vapor_kg*weight;
                    cloud[to]+=source.cloud_kg*weight;
                }
            }
        }
        for (i,cell) in self.cells.iter_mut().enumerate() {
            cell.vapor_kg=vapor[i].max(0.0);
            cell.cloud_kg=cloud[i].max(0.0);
        }
    }
    pub fn advance_af(&mut self,dt_seconds:f64) {
        if dt_seconds<=0.0{return;}
        self.transport(dt_seconds);
        self.time_seconds+=dt_seconds;
        let day=self.time_seconds/86_400.0;
        let daily=(TAU*(day-0.25)).sin();
        let seasonal=(TAU*day/120.0).sin();
        for (i,cell) in self.cells.iter_mut().enumerate() {
            // The Af laboratory baseline is warm in all climate months,
            // with a day/night cycle and mild seasonal variation.
            let small_region_variation=0.4*((i%N) as f64*TAU/N as f64).sin();
            cell.temperature_c=27.0+2.5*daily+1.0*seasonal
                +small_region_variation;
            // Supersaturated air condenses into an explicit cloud store.
            // This does not itself produce precipitation.
            let capacity=saturation_specific_humidity(cell.temperature_c)*AIR_MASS_KG;
            let excess=(cell.vapor_kg-capacity).max(0.0);
            if excess>0.0 {
                cell.vapor_kg-=excess;
                cell.cloud_kg+=excess;
            }
        }
    }
}

/// Tetens saturation pressure, converted to specific humidity at fixed
/// sea-level pressure. Atmospheric dynamics/variable pressure are deferred.
pub fn saturation_specific_humidity(temperature_c:f64)->f64 {
    let e=610.8*(17.27*temperature_c/(temperature_c+237.3)).exp();
    (0.622*e/(PRESSURE_PA-0.378*e)).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn periodic_weather_regions_wrap_correctly() {
        assert_eq!(Atmosphere::cell_index(-1,0),Atmosphere::cell_index(WORLD_SIZE-1,0));
        assert_eq!(Atmosphere::cell_index(0,WORLD_SIZE),Atmosphere::cell_index(0,0));
    }
    #[test]
    fn vapor_transport_conserves_mass_and_never_goes_negative() {
        let mut a=Atmosphere::new_af();
        let total=a.total_water_kg();
        a.cells[0].vapor_kg+=1234.0;
        for _ in 0..30 {a.transport(3600.0);}
        assert!((a.total_water_kg()-total-1234.0).abs()<0.001);
        assert!(a.cells.iter().all(|c|c.vapor_kg>=0.0 && c.cloud_kg>=0.0));
    }
    #[test]
    fn same_vapor_is_more_humid_when_air_cools() {
        let mut a=Atmosphere::new_af();
        a.cells[0].temperature_c=30.0;
        let hot=a.relative_humidity(0,0);
        a.cells[0].temperature_c=22.0;
        let cold=a.relative_humidity(0,0);
        assert!(cold>hot);
    }
    #[test]
    fn condensation_moves_mass_to_clouds_without_raining() {
        let mut a=Atmosphere::new_af();
        a.cells[0].vapor_kg=saturation_specific_humidity(29.0)*AIR_MASS_KG*1.5;
        a.cells[0].wind_x_m_s=0.0;
        a.cells[0].wind_y_m_s=0.0;
        let total=a.total_water_kg();
        a.advance_af(1.0);
        assert!(a.cells[0].cloud_kg>0.0);
        assert!((a.total_water_kg()-total).abs()<0.001);
    }
    #[test]
    fn af_air_remains_tropically_warm_across_game_year() {
        let mut a=Atmosphere::new_af();
        let mut monthly_temp=[0.0;12];
        for day in 0..120 {
            a.advance_af(86_400.0);
            monthly_temp[day/10]+=a.cells[0].temperature_c/10.0;
        }
        assert!(monthly_temp.iter().all(|&v|v>=18.0));
    }
}
