//! Small CPU-rasterized HUD atlas: no extra render framework needed.
//! The shader positions the compass at top-center and diagnostics at top-right.
use font8x8::{BASIC_FONTS, UnicodeFonts};
use std::f64::consts::{PI, TAU};
use crate::terrain::WORLD_SIZE;

pub const WIDTH: u32 = 768;
pub const HEIGHT: u32 = 144;
const COMPASS_W: i32 = 416;
const INFO_X: i32 = 416;
const PANEL_BG: [u8;4] = [8,18,24,178];
const WHITE: [u8;4] = [239,244,240,255];
const MUTED: [u8;4] = [166,195,198,255];
const ACCENT: [u8;4] = [255,218,125,255];

/// 0° = north (+Y), 90° = east (+X), 180° = south.
/// Camera yaw is measured counterclockwise from +X.
pub fn heading_degrees(yaw: f32) -> f64 {
    (90.0 - yaw as f64 * 180.0 / PI).rem_euclid(360.0)
}

/// Centered map-space positions in meters, independent of spawn location.
pub fn centered_coordinate(world:f64) -> f64 {
    world.rem_euclid(WORLD_SIZE as f64)
        - WORLD_SIZE as f64 / 2.0
}
pub fn longitude_degrees(x:f64) -> f64 {
    centered_coordinate(x) * 360.0 / WORLD_SIZE as f64
}
pub fn latitude_degrees(y:f64) -> f64 {
    // Periodic virtual latitude, used by the climate simulator.
    // This is NOT spherical geographic latitude.
    (-((y / WORLD_SIZE as f64)*TAU).sin()).asin().to_degrees()
}

fn pixel(out:&mut [u8],x:i32,y:i32,color:[u8;4]) {
    if x<0 || y<0 || x>=WIDTH as i32 || y>=HEIGHT as i32 {return;}
    let i=((y as u32*WIDTH+x as u32)*4) as usize;
    out[i..i+4].copy_from_slice(&color);
}
fn fill(out:&mut [u8],x:i32,y:i32,w:i32,h:i32,color:[u8;4]) {
    for yy in y..y+h {for xx in x..x+w {pixel(out,xx,yy,color);}}
}
fn text(out:&mut [u8],x:i32,y:i32,message:&str,color:[u8;4],scale:i32) {
    let mut cursor=x;
    for ch in message.chars() {
        if let Some(glyph)=BASIC_FONTS.get(ch) {
            for (row,bits) in glyph.iter().enumerate() {
                for col in 0..8 {
                    if *bits & (1<<col)!=0 {
                        fill(out,cursor+col*scale,y+row as i32*scale,
                            scale,scale,color);
                    }
                }
            }
        }
        cursor+=8*scale;
    }
}
fn centered_text(out:&mut [u8],center:i32,y:i32,msg:&str,color:[u8;4],scale:i32) {
    text(out,center-msg.len() as i32*4*scale,y,msg,color,scale);
}
fn compass(out:&mut [u8],heading:f64) {
    fill(out,0,0,COMPASS_W,HEIGHT as i32,PANEL_BG);
    // Scroll compass ticks under the fixed center indicator.
    let heading = (heading.round() as i32).rem_euclid(360) as f64;
    for offset in -13..=13 {
        let degrees=(heading / 15.0).floor() as i32 * 15 + offset*15;
        let dx=(degrees as f64-heading)*4.0;
        let px=(COMPASS_W as f64/2.0+dx).round() as i32;
        if !(10..COMPASS_W-10).contains(&px) {continue;}
        let dir=degrees.rem_euclid(360);
        let major=dir % 45==0;
        fill(out,px,25,if major {2} else {1},if major {15}else{8},MUTED);
        if major {
            let name=match dir {
                0=>"N",45=>"NE",90=>"E",135=>"SE",180=>"S",225=>"SW",
                270=>"W",315=>"NW",_=>"",
            };
            centered_text(out,px,8,name,WHITE,1);
        }
    }
    fill(out,COMPASS_W/2-1,19,3,22,ACCENT);
    centered_text(out,COMPASS_W/2,48,&format!("{:03} deg",heading as i32),WHITE,1);
}
pub fn rasterize(yaw:f32,x:f64,y:f64,z:f64,fps:f32,frame_ms:f32,
    time_scale:u32, world_time_seconds:f64, water_liters:u16,
    active_cells:usize, surface_delta_liters:i64, _atmosphere_liters:u64,
    mass_error_liters:i64, relative_humidity:f64, air_temperature_c:f64,
    rain_liters:u64, unmet_rain_liters:u64,
    rain_month:usize, rain_gauge_mm:Option<u32>) -> Vec<u8> {
    let mut out=vec![0u8;(WIDTH*HEIGHT*4) as usize];
    compass(&mut out,heading_degrees(yaw));
    fill(&mut out,INFO_X,0,320,HEIGHT as i32,PANEL_BG);
    text(&mut out,INFO_X+12,8,&format!("FPS {:5.1}  {:5.1} ms",fps,frame_ms),ACCENT,1);
    text(&mut out,INFO_X+12,28,&format!("X {:+8.0}  Y {:+8.0}",centered_coordinate(x),centered_coordinate(y)),WHITE,1);
    text(&mut out,INFO_X+12,47,&format!("Z {:+7.0} m",z),WHITE,1);
    let day=(world_time_seconds/1200.0).floor() as u64+1;
    // Longitude-adjusted local solar time; spawn at map center starts at noon.
    let local=(world_time_seconds/1200.0+x.rem_euclid(WORLD_SIZE as f64)/WORLD_SIZE as f64).fract();
    let minutes=(local*1440.0).floor() as u32;
    text(&mut out,INFO_X+12,66,
        &format!("F1 {:>4}x DAY {} {:02}:{:02}",time_scale,day,minutes/60,minutes%60),
        ACCENT,1);
    text(&mut out,INFO_X+12,85,
        &format!("WATER {:04}L TILES {} M{:02}:{}mm",water_liters,active_cells,
            rain_month+1,rain_gauge_mm.map_or("--".to_string(),|n|n.to_string())),WHITE,1);
    text(&mut out,INFO_X+12,103,
        &format!("D{:+}L R{}L MISS{}L",surface_delta_liters,rain_liters,unmet_rain_liters),
        MUTED,1);
    text(&mut out,INFO_X+12,122,
        &format!("RH {:3.0}%  AIR {:4.1}C  ERR {}L",
            relative_humidity*100.0,air_temperature_c,mass_error_liters),
        ACCENT,1);
    centered_text(&mut out,COMPASS_W/2,68,
        &format!("V-LAT {:+6.2}  LON {:+7.2}",latitude_degrees(y),longitude_degrees(x)),
        MUTED,1);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compass_directions_and_wrap() {
        assert!((heading_degrees(0.0)-90.0).abs()<0.001);
        assert!(heading_degrees(std::f32::consts::FRAC_PI_2).min(360.0-heading_degrees(std::f32::consts::FRAC_PI_2))<0.001);
        assert!((heading_degrees(-std::f32::consts::FRAC_PI_2)-180.0).abs()<0.001);
        assert!((heading_degrees(2.0*std::f32::consts::PI)-90.0).abs()<0.01);
    }
    #[test]
    fn coordinates_use_map_center_and_are_periodic() {
        let half=WORLD_SIZE as f64/2.0;
        assert_eq!(centered_coordinate(half),0.0);
        assert_eq!(centered_coordinate(0.0),-half as f64);
        assert_eq!(centered_coordinate(half+125.0),125.0);
        assert_eq!(longitude_degrees(half),0.0);
        assert!((longitude_degrees(0.0)+180.0).abs()<0.00001);
        assert!((longitude_degrees(half+100.0)-longitude_degrees(half)
                -360.0*100.0/WORLD_SIZE as f64).abs()<0.00001);
        assert!((latitude_degrees(0.0)-latitude_degrees(WORLD_SIZE as f64)).abs()<1e-10);
        assert!(latitude_degrees(WORLD_SIZE as f64/4.0) < -89.99);
        assert!(latitude_degrees(WORLD_SIZE as f64*0.5+100.0)>0.0);
        assert!(latitude_degrees(WORLD_SIZE as f64*0.5-100.0)<0.0);
        assert!(latitude_degrees(WORLD_SIZE as f64*0.5).abs()<0.000001);
    }
    #[test]
    fn lab_time_is_visible_and_updates_in_hud() {
        let normal=rasterize(0.0,512.0,512.0,128.0,60.0,16.7,1,0.0,0,0,0,0,0,0.83,27.0,0,0,0,None);
        let fast=rasterize(0.0,512.0,512.0,128.0,60.0,16.7,100,1200.0,0,0,0,0,0,0.83,27.0,0,0,0,None);
        assert_ne!(normal,fast,"HUD must visibly reflect F1 rate and elapsed days");
        assert_eq!(normal.len(),(WIDTH*HEIGHT*4) as usize);
    }
    #[test]
    fn atlas_is_rgba_and_nonempty() {
        let pixels=rasterize(0.0,512.0,512.0,128.0,60.0,16.7,1,0.0,0,0,0,0,0,0.83,27.0,0,0,0,None);
        assert_eq!(pixels.len(),(WIDTH*HEIGHT*4) as usize);
        assert!(pixels.iter().any(|&v|v>0));
    }
}
