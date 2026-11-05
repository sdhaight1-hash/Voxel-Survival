//! Deterministic regional geophysics. All coordinates are toroidal and every
//! field can be evaluated without loading 1-meter voxels. This is a foundation,
//! not a full plate solver or numerical weather prediction model.
use crate::terrain::{wrap, WORLD_SIZE};
use std::f64::consts::{PI, TAU};

const PLATE_CELLS: i32 = 8;
const PLATE_SPACING: f64 = WORLD_SIZE as f64 / PLATE_CELLS as f64;

fn hash(x: i32, y: i32, salt: u64) -> f64 {
    let mut n = ((x.rem_euclid(PLATE_CELLS) as u64) << 32)
        | y.rem_euclid(PLATE_CELLS) as u64;
    n ^= salt;
    n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((n ^ (n >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}
fn periodic_delta(a: f64, b: f64) -> f64 {
    (a - b + WORLD_SIZE as f64 * 0.5).rem_euclid(WORLD_SIZE as f64)
        - WORLD_SIZE as f64 * 0.5
}

#[derive(Clone, Copy, Debug)]
pub struct TectonicSample {
    pub boundary_distance_m: f64,
    pub convergence: f64,
    pub uplift_m: f64,
}
#[derive(Clone, Copy)]
struct Plate {
    x: f64,
    y: f64,
    velocity_x: f64,
    velocity_y: f64,
}
fn plate(cx: i32, cy: i32) -> Plate {
    Plate {
        x: (cx as f64 + 0.2 + 0.6 * hash(cx,cy,1)) * PLATE_SPACING,
        y: (cy as f64 + 0.2 + 0.6 * hash(cx,cy,2)) * PLATE_SPACING,
        velocity_x: hash(cx,cy,3) * 2.0 - 1.0,
        velocity_y: hash(cx,cy,4) * 2.0 - 1.0,
    }
}

/// Kinematic plate-boundary uplift proxy, evaluated only on coarse region grids.
/// Geological-time plate motion and erosion solvers can replace this interface.
pub fn tectonics(x: i32, y: i32) -> TectonicSample {
    let x = wrap(x) as f64;
    let y = wrap(y) as f64;
    let cx = (x / PLATE_SPACING).floor() as i32;
    let cy = (y / PLATE_SPACING).floor() as i32;
    let mut nearest = [(f64::INFINITY, plate(0,0)); 2];
    for dy in -1..=1 {
        for dx in -1..=1 {
            let p = plate(cx + dx, cy + dy);
            let ddx = periodic_delta(x,p.x);
            let ddy = periodic_delta(y,p.y);
            let d2 = ddx*ddx + ddy*ddy;
            if d2 < nearest[0].0 {
                nearest[1] = nearest[0];
                nearest[0] = (d2,p);
            } else if d2 < nearest[1].0 {
                nearest[1] = (d2,p);
            }
        }
    }
    let a = nearest[0].1;
    let b = nearest[1].1;
    let vx = periodic_delta(b.x,a.x);
    let vy = periodic_delta(b.y,a.y);
    let inv = 1.0 / (vx*vx + vy*vy).sqrt().max(1.0);
    let convergence = ((a.velocity_x-b.velocity_x)*vx
        +(a.velocity_y-b.velocity_y)*vy)*inv;
    let distance = (nearest[1].0.sqrt() - nearest[0].0.sqrt()).abs()*0.5;
    let influence = (1.0 - distance / 20_000.0).max(0.0);
    TectonicSample {
        boundary_distance_m: distance,
        convergence,
        uplift_m: influence * convergence.max(0.0) * 900.0,
    }
}

/// A torus cannot have conventional poles. This periodic *virtual* latitude
/// produces repeated climate belts with no seam; it is not spherical geometry.
pub fn virtual_latitude_rad(y: i32) -> f64 {
    (-((wrap(y) as f64 / WORLD_SIZE as f64) * TAU).sin()).asin()
}

#[derive(Clone, Copy, Debug)]
pub struct SolarTime {
    pub day_fraction: f64,
    pub year_fraction: f64,
    pub axial_tilt_rad: f64,
}
impl Default for SolarTime {
    fn default() -> Self {
        Self {day_fraction:0.5,year_fraction:0.25,axial_tilt_rad:23.44_f64.to_radians()}
    }
}
/// Incoming direct+diffuse approximate shortwave flux, W/m^2.
pub fn solar_flux_w_m2(x: i32, y: i32, time: SolarTime) -> f64 {
    let lat = virtual_latitude_rad(y);
    let declination = time.axial_tilt_rad * (TAU*time.year_fraction).sin();
    let hour_angle = TAU * (time.day_fraction
        + wrap(x) as f64 / WORLD_SIZE as f64 - 0.5);
    let elevation = lat.sin()*declination.sin()
        + lat.cos()*declination.cos()*hour_angle.cos();
    1361.0 * 0.72 * elevation.max(0.0)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindBelt { TropicalEasterly, Westerly, PolarEasterly }
pub fn wind_belt(y: i32) -> WindBelt {
    let latitude = virtual_latitude_rad(y).abs() * 180.0/PI;
    if latitude < 30.0 { WindBelt::TropicalEasterly }
    else if latitude < 60.0 { WindBelt::Westerly }
    else { WindBelt::PolarEasterly }
}
pub fn zonal_wind_sign(y:i32) -> i32 {
    if wind_belt(y)==WindBelt::Westerly {1} else {-1}
}
/// Rain shadow proxy using upwind elevation samples. Rain falls preferentially
/// on rising air and is reduced behind the highest upwind ridge.
pub fn orographic_rain_factor(
    x:i32, y:i32, terrain_height:impl Fn(i32,i32)->i32,
) -> f64 {
    let wind = zonal_wind_sign(y);
    let local = terrain_height(x,y) as f64;
    let near = terrain_height(x - wind*4_000,y) as f64;
    let far = terrain_height(x - wind*12_000,y) as f64;
    let ridge = near.max(far);
    let ascent = (local - near).max(0.0);
    let shadow = (ridge - local).max(0.0);
    (1.0 + ascent/400.0 - shadow/300.0).clamp(0.1,3.0)
}

#[derive(Clone, Copy, Debug)]
pub enum Surface { Soil, Grass, Rock, Water, Snow }
impl Surface {
    pub fn albedo(self) -> f64 {
        match self { Self::Soil=>0.20, Self::Grass=>0.23,
            Self::Rock=>0.17,Self::Water=>0.07,Self::Snow=>0.75 }
    }
    pub fn effective_heat_capacity_j_m2_k(self) -> f64 {
        match self {Self::Water=>4_200_000.0, Self::Rock=>1_600_000.0,
            Self::Soil=>1_000_000.0, Self::Grass=>1_100_000.0,
            Self::Snow=>600_000.0}
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ThermalCell { pub temperature_c: f64 }
impl ThermalCell {
    /// Explicit heat budget: absorbed sun minus outgoing cooling/latent heat.
    /// Heat fluxes are W/m²; dt is seconds; no arbitrary temperature assignment.
    pub fn advance(&mut self, solar_w_m2:f64, surface:Surface,
                   atmospheric_w_m2:f64, evaporation_w_m2:f64, dt_s:f64) {
        let longwave = (self.temperature_c + 273.15).max(0.0).powi(4)
            * 5.670_374_419e-8 * 0.96;
        let net = solar_w_m2 * (1.0-surface.albedo())
            + atmospheric_w_m2 - longwave - evaporation_w_m2;
        self.temperature_c += net * dt_s / surface.effective_heat_capacity_j_m2_k();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_regional_fields_wrap() {
        let a = tectonics(123456,654321);
        let b = tectonics(123456 + WORLD_SIZE,654321 - WORLD_SIZE);
        assert!((a.uplift_m-b.uplift_m).abs()<1e-7);
        let t = SolarTime::default();
        assert!((solar_flux_w_m2(0,150000,t)-solar_flux_w_m2(WORLD_SIZE,150000,t)).abs()<1e-7);
        assert_eq!(wind_belt(-1),wind_belt(WORLD_SIZE-1));
    }
    #[test]
    fn noon_has_more_sun_than_night() {
        let time = SolarTime { day_fraction:0.5,year_fraction:0.0,
            axial_tilt_rad:23.44_f64.to_radians() };
        assert!(solar_flux_w_m2(0,0,time)>900.0);
        assert_eq!(solar_flux_w_m2(WORLD_SIZE/2,0,time),0.0);
    }
    #[test]
    fn latitude_north_of_map_center_is_positive() {
        assert!(virtual_latitude_rad(WORLD_SIZE/2+100)>0.0);
        assert!(virtual_latitude_rad(WORLD_SIZE/2-100)<0.0);
        assert!(virtual_latitude_rad(WORLD_SIZE/2).abs()<1e-10);
    }
    #[test]
    fn wind_belts_are_periodic() {
        assert_eq!(wind_belt(0),WindBelt::TropicalEasterly);
        assert_eq!(wind_belt(WORLD_SIZE/4),WindBelt::PolarEasterly);
        assert_eq!(wind_belt(WORLD_SIZE/8),WindBelt::Westerly);
    }
    #[test]
    fn mountain_creates_rain_shadow() {
        let ridge = |x:i32,_y:i32| if x < 0 {1000} else {100};
        let y = WORLD_SIZE / 8; // westerly; air travels from x-negative
        assert!(orographic_rain_factor(1_000,y,ridge) < 1.0);
    }
    #[test]
    fn ocean_heats_slower_than_soil() {
        let mut water = ThermalCell {temperature_c:20.0};
        let mut soil=water;
        water.advance(900.0,Surface::Water,300.0,0.0,3600.0);
        soil.advance(900.0,Surface::Soil,300.0,0.0,3600.0);
        assert!(water.temperature_c < soil.temperature_c);
    }
}
