//! Versioned snapshots. Terrain generation identity and zero-water overrides are
//! part of the format; a mismatched or corrupt file must never become a new world.
use crate::{hydrology::{Cell,CAPACITY_L},survival::Survival,terrain::{Block,Terrain,WORLD_HEIGHT,WORLD_SIZE}};
use serde::{Deserialize,Serialize};
use std::{fs::{self,File,OpenOptions},io::{self,Read,Write},path::{Path,PathBuf}};
use fs2::FileExt;
const VERSION:u32=1;
const GENERATOR:&str="cube-torus-42-survival-1";
const MAX_BYTES:u64=256*1024*1024;
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    version:u32,generator:String,world_size:i32,world_height:i32,
    edits:Vec<(Cell,Block)>,water:Vec<(Cell,u16)>,active_water:Vec<Cell>,atmosphere_liters:u64,
    pub position:[f64;3],pub velocity_z:f64,pub yaw:f32,pub pitch:f32,pub grounded:bool,
    pub world_seconds:f64,pub temperature_c:f64,pub physics_accumulator:f32,pub water_accumulator:f32,
    pub survival:Survival,
}
fn invalid(message:&str)->io::Error {io::Error::new(io::ErrorKind::InvalidData,message)}
fn cell_valid(c:Cell)->bool {(0..WORLD_SIZE).contains(&c.0)&&(0..WORLD_SIZE).contains(&c.1)&&(0..WORLD_HEIGHT).contains(&c.2)}
impl Snapshot {
    pub fn capture(t:&Terrain,p:&crate::Player,s:&Survival,time:f64,temp:f64,physics:f32,water:f32)->Self {
        let mut edits:Vec<_>=t.edits.iter().map(|(&c,&b)|(c,b)).collect();edits.sort_unstable_by_key(|v|v.0);
        let mut amounts:Vec<_>=t.water.amounts.iter().map(|(&c,&n)|(c,n)).collect();amounts.sort_unstable_by_key(|v|v.0);
        let mut active:Vec<_>=t.water.active.iter().copied().collect();active.sort_unstable();
        Self{version:VERSION,generator:GENERATOR.into(),world_size:WORLD_SIZE,world_height:WORLD_HEIGHT,
            edits,water:amounts,active_water:active,atmosphere_liters:t.water.atmosphere_liters,
            position:p.pos.to_array(),velocity_z:p.velocity_z,yaw:p.yaw,pitch:p.pitch,grounded:p.grounded,
            world_seconds:time,temperature_c:temp,physics_accumulator:physics,water_accumulator:water,survival:s.clone()}
    }
    pub fn restore_terrain(&self)->io::Result<Terrain> {
        if self.version!=VERSION||self.generator!=GENERATOR||self.world_size!=WORLD_SIZE||self.world_height!=WORLD_HEIGHT {
            return Err(invalid("Save version/generator/world dimensions do not match this build"));
        }
        if !self.position.iter().all(|v|v.is_finite())
            || !(0.0..WORLD_SIZE as f64).contains(&self.position[0])
            || !(0.0..WORLD_SIZE as f64).contains(&self.position[1])
            || !(-1024.0..=WORLD_HEIGHT as f64+1024.0).contains(&self.position[2])
            || !self.velocity_z.is_finite() || self.velocity_z.abs()>10000.0
            || !self.yaw.is_finite()||!self.pitch.is_finite()||self.pitch.abs()>1.54
            || !self.world_seconds.is_finite()||self.world_seconds<0.0
            || !self.temperature_c.is_finite()|| !self.survival.valid()
            || !self.physics_accumulator.is_finite()||!(0.0..=0.2).contains(&self.physics_accumulator)
            || !self.water_accumulator.is_finite()||!(0.0..=0.2).contains(&self.water_accumulator) {
            return Err(invalid("Invalid player, clock, or survival state"));
        }
        let mut t=Terrain::new();
        for &(c,b) in &self.edits {
            if !cell_valid(c)||b==Block::Water||t.edits.insert(c,b).is_some(){return Err(invalid("Invalid or duplicate terrain edit"));}
        }
        for &(c,n) in &self.water {
            if !cell_valid(c)||n>CAPACITY_L||t.water.amounts.insert(c,n).is_some(){return Err(invalid("Invalid or duplicate water record"));}
            if n>0 && (t.edits.get(&c).is_some_and(|b|b.solid())
                || (!t.edits.contains_key(&c) && t.natural_block(c.0,c.1,c.2,t.height(c.0,c.1)).solid())) {
                return Err(invalid("Water stored inside solid terrain"));
            }
        }
        for &c in &self.active_water {
            if !cell_valid(c)||!t.water.active.insert(c){return Err(invalid("Invalid or duplicate active water cell"));}
        }
        t.water.atmosphere_liters=self.atmosphere_liters;
        Ok(t)
    }
}
pub struct WorldStore {directory:PathBuf,_lock:File}
impl WorldStore {
    pub fn open(path:impl AsRef<Path>)->io::Result<Self> {
        fs::create_dir_all(path.as_ref())?;
        let lock=OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path.as_ref().join("world.lock"))?;
        lock.try_lock_exclusive().map_err(|_|io::Error::new(io::ErrorKind::WouldBlock,"World is already open in another game process"))?;
        Ok(Self{directory:path.as_ref().to_owned(),_lock:lock})
    }
    pub fn load(&self)->io::Result<Option<Snapshot>> {
        let file=match File::open(self.directory.join("world.json")) {
            Ok(f)=>f,Err(e) if e.kind()==io::ErrorKind::NotFound=>return Ok(None),Err(e)=>return Err(e),
        };
        if file.metadata()?.len()>MAX_BYTES{return Err(invalid("Save exceeds 256 MiB snapshot limit"));}
        let mut bytes=Vec::new();file.take(MAX_BYTES+1).read_to_end(&mut bytes)?;
        if bytes.len() as u64>MAX_BYTES{return Err(invalid("Save exceeds snapshot limit"));}
        let snapshot:Snapshot=serde_json::from_slice(&bytes).map_err(|e|invalid(&format!("Cannot parse save: {e}")))?;
        snapshot.restore_terrain()?;
        Ok(Some(snapshot))
    }
    pub fn save(&self,snapshot:&Snapshot)->io::Result<()> {
        snapshot.restore_terrain()?;
        let bytes=serde_json::to_vec(snapshot).map_err(|e|invalid(&e.to_string()))?;
        if bytes.len() as u64>MAX_BYTES{return Err(invalid("Save exceeds snapshot limit"));}
        // The exclusive world lock makes this temporary name safe. Never truncate
        // the previous snapshot until a complete replacement is flushed to disk.
        let temp=self.directory.join("world.json.tmp");
        let mut f=File::create(&temp)?;f.write_all(&bytes)?;f.sync_all()?;drop(f);
        fs::rename(&temp,self.directory.join("world.json"))?;
        #[cfg(unix)] File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}
#[cfg(test)] mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64,Ordering};
    static ID:AtomicU64=AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp{fn new()->Self{Self(std::env::temp_dir().join(format!("voxel-save-{}-{}",std::process::id(),ID.fetch_add(1,Ordering::Relaxed))))}}
    impl Drop for Temp{fn drop(&mut self){let _=fs::remove_dir_all(&self.0);}}
    fn snapshot(t:&Terrain)->Snapshot {Snapshot::capture(t,&crate::find_safe_spawn(t),&Survival::default(),123.0,17.0,0.002,0.1)}
    #[test] fn reload_retains_empty_ocean_edits_inventory_clock_and_active_simulation() {
        let temp=Temp::new();let store=WorldStore::open(&temp.0).unwrap();let mut t=Terrain::new();
        // Find real implicit ocean water; withdrawing it must survive reload.
        let (x,y)=(0..200).map(|i|(i*4321%WORLD_SIZE,i*9871%WORLD_SIZE))
            .find(|&(x,y)|t.height(x,y)<crate::terrain::SEA_LEVEL).unwrap();
        let c=(x,y,crate::terrain::SEA_LEVEL);assert_eq!(t.evaporate(c,1000),1000);
        t.set(WORLD_SIZE-1,0,50,Block::Air);t.precipitate((0,0,240),350);
        let mut s=snapshot(&t);s.survival.add(crate::survival::Item::Wood,7);
        store.save(&s).unwrap();let restored=store.load().unwrap().unwrap();
        let mut loaded=restored.restore_terrain().unwrap();
        assert_eq!(loaded.water_liters(c.0,c.1,c.2),0);assert_eq!(loaded.block(-1,0,50),Block::Air);
        assert_eq!(loaded.water.atmosphere_liters,650);assert_eq!(restored.world_seconds,123.0);
        assert_eq!(restored.survival,s.survival);assert_eq!(loaded.water.active,t.water.active);
        for _ in 0..20{loaded.step_water(4096);t.step_water(4096);}
        assert_eq!(loaded.water.amounts,t.water.amounts);
        store.save(&snapshot(&loaded)).unwrap(); // replacement works on Windows too
    }
    #[test] fn rejects_corruption_versions_duplicates_and_invalid_volumes() {
        let t=Terrain::new();let mut s=snapshot(&t);s.version=999;assert!(s.restore_terrain().is_err());
        s.version=VERSION;s.water=vec![((0,0,240),1001)];assert!(s.restore_terrain().is_err());
        s.water=vec![((0,0,240),0),((0,0,240),0)];assert!(s.restore_terrain().is_err());
        let temp=Temp::new();let store=WorldStore::open(&temp.0).unwrap();
        fs::write(temp.0.join("world.json"),b"{broken").unwrap();assert!(store.load().is_err());
        assert_eq!(fs::read(temp.0.join("world.json")).unwrap(),b"{broken");
    }
    #[test] fn invalid_save_does_not_replace_previous_snapshot_and_world_is_locked() {
        let temp=Temp::new();let store=WorldStore::open(&temp.0).unwrap();
        assert!(WorldStore::open(&temp.0).is_err());let mut s=snapshot(&Terrain::new());store.save(&s).unwrap();
        s.survival.health=f32::NAN;assert!(store.save(&s).is_err());assert!(store.load().unwrap().is_some());
        drop(store);assert!(WorldStore::open(&temp.0).is_ok());
    }
}
