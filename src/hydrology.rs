//! Finite water bookkeeping. One voxel has capacity 1000 liters (1 m³).
//! The initial ocean is a finite implicit volume field, never an infinite
//! refill rule. Each withdrawn voxel is recorded as a sparse override.
use crate::terrain::{wrap, Block, Terrain, WORLD_HEIGHT};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::Bound::{Excluded, Unbounded};

pub type Cell = (i32, i32, i32);
pub const CAPACITY_L: u16 = 1000;

pub fn canonical((x,y,z): Cell) -> Cell {(wrap(x),wrap(y),z)}
fn neighbors((x,y,z): Cell) -> [Cell;6] {
    [(x,y,z-1),(x-1,y,z),(x+1,y,z),(x,y-1,z),(x,y+1,z),(x,y,z+1)]
}

#[derive(Default)]
pub struct WaterState {
    pub(crate) amounts: HashMap<Cell,u16>,
    // Spatial index for rebuilding only affected water meshes. Without
    // this, each changed chunk scans every modified water cell island-wide.
    pub(crate) changed_by_chunk: HashMap<(i32,i32),HashSet<Cell>>,
    // Surface water has one active item per horizontal column, no matter
    // how many meters deep the ocean is. Voxel activity is kept for local
    // excavation/cave gravity and pressure only.
    pub(crate) active: BTreeSet<Cell>,
    active_cursor:Option<Cell>,
    pub(crate) surface_active:BTreeSet<(i32,i32)>,
    surface_cursor:Option<(i32,i32)>,
    pub(crate) dirty: HashSet<Cell>,
    // Atmosphere is an explicitly finite reservoir, not a source of free rain.
    pub atmosphere_liters: u64,
    // Signed deviation from finite, implicit initial surface-water inventory.
    // Atmospheric/soil reservoirs must balance this delta in a closed system.
    pub surface_delta_liters: i64,
    // O(1) surface-column volume queries, relative to the implicit initial
    // water columns. Underground edits are excluded from these deltas.
    surface_column_delta:HashMap<(i32,i32),i64>,
    // Rare unsupported above-ground water and constructed roofs cannot be
    // interpreted as a single contiguous free-surface height column.
    highest_modified_wet_z:HashMap<(i32,i32),i32>,
    pub(crate) blocked_surface_columns:HashSet<(i32,i32)>,
}
impl WaterState {
    pub fn activate_surface(&mut self,(x,y):(i32,i32)) {
        for (dx,dy) in [(0,0),(-1,0),(1,0),(0,-1),(0,1)] {
            self.surface_active.insert((wrap(x+dx),wrap(y+dy)));
        }
    }
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
// A surface column is an independent fluid computation over voxel terrain;
// `Block::Water` is only a compatibility view for collision/meshing.
#[derive(Clone,Copy,Debug)]
struct SurfaceColumn {
    bed: i32,
    liters: u32,
    head_m: f64,
    blocked: bool,
}

impl Terrain {
    /// Water surface above a voxel terrain column. Ignore enclosed columns
    /// or disjoint floating liquid; those need a local 3D/conduit solver.
    fn surface_column(&self,x:i32,y:i32)->SurfaceColumn {
        let x=wrap(x);let y=wrap(y);
        let bed=(self.height(x,y)+1).min(WORLD_HEIGHT);
        let initial=self.initial_water_surface(x,y);
        let baseline=((initial-bed).max(0) as i64)*CAPACITY_L as i64;
        let delta=self.water.surface_column_delta.get(&(x,y)).copied().unwrap_or(0);
        let liters=(baseline+delta).max(0) as u32;
        let depth=liters as f64/CAPACITY_L as f64;
        let unsupported=self.water.highest_modified_wet_z.get(&(x,y))
            .is_some_and(|&top|top as f64>bed as f64+depth+0.000_001);
        let blocked=unsupported
            || self.water.blocked_surface_columns.contains(&(x,y));
        // A consistent invisible micro-relief field; its influence is
        // limited to thin runoff. Deep lake surfaces remain well-balanced.
        let mx=crate::terrain::delta(x,crate::terrain::WORLD_SIZE/2) as f64;
        let my=crate::terrain::delta(y,crate::terrain::WORLD_SIZE/2) as f64;
        let r=(mx+115.0).hypot(my-125.0);
        let lake_grade=0.08*((r-78.0)/25.0).clamp(0.0,1.0);
        let small_roughness=0.01*(mx/23.0).sin()*(my/19.0).cos();
        let micro=lake_grade+small_roughness;
        let head_m=bed as f64+depth+micro*(1.0-depth/0.25).clamp(0.0,1.0);
        SurfaceColumn {bed,liters,head_m,blocked}
    }

    /// Remove a scheduled flux from the top of an open water column.
    fn remove_surface_liters(&mut self,(x,y):(i32,i32),mut amount:u32,bed:i32) {
        let volume=self.surface_column(x,y).liters;
        assert!(volume>=amount && amount>0,"surface donor overdraw");
        // The volume-derived waterline is only valid for a contiguous
        // column. Underground drainage can leave a void under an intact
        // upper layer. Scan from the highest *physically possible* water
        // cell instead; a truncated waterline used to panic here at runtime.
        let top=self.initial_water_surface(x,y).saturating_sub(1)
            .max(self.water.highest_modified_wet_z
                .get(&(wrap(x),wrap(y))).copied().unwrap_or(bed-1))
            .min(WORLD_HEIGHT-1);
        for z in (bed..=top).rev() {
            if amount==0 {break;}
            let current=self.water_liters(x,y,z);
            let moved=amount.min(current as u32) as u16;
            if moved>0 {
                self.water_assign((x,y,z),current-moved);
                amount-=moved as u32;
            }
        }
        assert_eq!(amount,0,"surface flux donor lacked water");
    }
    /// Store a scheduled flux in the lowest available cells above the bed.
    fn add_surface_liters(&mut self,(x,y):(i32,i32),mut amount:u32,bed:i32) {
        // Always fill the lowest open cell first. A cavity may have left
        // holes below the volume-derived surface; starting at the assumed
        // waterline would preserve those holes indefinitely.
        for z in bed..WORLD_HEIGHT {
            if amount==0 {break;}
            if self.block(x,y,z).solid() {continue;}
            let current=self.water_liters(x,y,z);
            let moved=amount.min((CAPACITY_L-current) as u32) as u16;
            if moved>0 {
                self.water_assign((x,y,z),current+moved);
                amount-=moved as u32;
            }
        }
        assert_eq!(amount,0,"surface flux receiver lacked capacity");
    }

    /// Schedule conservative edge transfers from the same snapshot. All
    /// fluxes are committed after the calculations, avoiding iteration-order
    /// dependent hydraulic head changes within one step.
    fn step_surface_flow(&mut self,active_columns:&[(i32,i32)]) -> usize {
        let mut touched=HashSet::new();
        for &(x,y) in active_columns {
            for (dx,dy) in [(0,0),(-1,0),(1,0),(0,-1),(0,1)] {
                touched.insert((wrap(x+dx),wrap(y+dy)));
            }
        }
        let mut columns=HashMap::with_capacity(touched.len());
        for &(x,y) in &touched {
            columns.insert((x,y),self.surface_column(x,y));
        }
        let mut coords:Vec<_>=touched.into_iter().collect();
        coords.sort_unstable();
        let mut taken:HashMap<(i32,i32),u32>=HashMap::new();
        let mut received:HashMap<(i32,i32),u32>=HashMap::new();
        let mut fluxes=Vec::new();
        for &(x,y) in &coords {
            let here=(x,y);
            for there in [(wrap(x+1),y),(x,wrap(y+1))] {
                let Some(&a)=columns.get(&here) else {continue;};
                let Some(&b)=columns.get(&there) else {continue;};
                if a.blocked || b.blocked {continue;}
                let (from,to,source,dest)=if a.head_m>b.head_m {
                    (here,there,a,b)
                } else {(there,here,b,a)};
                let difference=source.head_m-dest.head_m;
                if difference<=0.000_001 {continue;}
                // A complete voxel step is still a real spill barrier. A
                // microslope never allows crossing a solid one-meter wall.
                let crest=source.bed.max(dest.bed) as f64;
                let spillable=((source.head_m-crest).max(0.0)
                    *CAPACITY_L as f64).floor() as u32;
                let available=spillable.min(source.liters)
                    .saturating_sub(*taken.get(&from).unwrap_or(&0));
                let capacity=((WORLD_HEIGHT-dest.bed).max(0) as u32)
                    *CAPACITY_L as u32;
                let free=capacity.saturating_sub(dest.liters)
                    .saturating_sub(*received.get(&to).unwrap_or(&0));
                // Damped, bounded hydrostatic flux. Momentum will be added
                // separately for fast rivers, waterfalls and floods.
                // Integer liters need a real volume threshold; ceil()
                // previously made even a microscopic gradient transfer
                // 1L forever, reactivating calm ocean cells endlessly.
                let requested=(difference*500.0).floor() as u32;
                let liters=requested.min(250).min(available).min(free);
                if liters==0 {continue;}
                *taken.entry(from).or_default()+=liters;
                *received.entry(to).or_default()+=liters;
                fluxes.push((from,to,liters));
            }
        }
        for (&cell,&amount) in &taken {
            self.remove_surface_liters(cell,amount,columns[&cell].bed);
        }
        for (&cell,&amount) in &received {
            self.add_surface_liters(cell,amount,columns[&cell].bed);
        }
        fluxes.len()
    }

    pub fn water_liters(&self, x:i32,y:i32,z:i32) -> u16 {
        if !(0..WORLD_HEIGHT).contains(&z) {return 0;}
        let cell=(wrap(x),wrap(y),z);
        if let Some(b)=self.edits.get(&cell) {
            if b.solid() {return 0;}
        }
        if let Some(&volume)=self.water.amounts.get(&cell) {return volume;}
        // New empty cells created by mining are empty until water moves in.
        if self.edits.contains_key(&cell) {return 0;}
        if z > self.height(cell.0,cell.1)
            && z<self.initial_water_surface(cell.0,cell.1) {
            CAPACITY_L
        } else {0}
    }
    pub(crate) fn water_assign(&mut self, cell:Cell, amount:u16) {
        let cell=canonical(cell);
        let old=self.water_liters(cell.0,cell.1,cell.2);
        let new_amount=amount.min(CAPACITY_L);
        let change=new_amount as i64-old as i64;
        if change==0 {return;}
        self.water.surface_delta_liters+=change;
        let ground=self.height(cell.0,cell.1);
        if cell.2>ground {
            *self.water.surface_column_delta.entry((cell.0,cell.1)).or_default()+=change;
            if new_amount>0 {
                self.water.highest_modified_wet_z.entry((cell.0,cell.1))
                    .and_modify(|top|*top=(*top).max(cell.2))
                    .or_insert(cell.2);
            } else if self.water.highest_modified_wet_z.get(&(cell.0,cell.1))==Some(&cell.2) {
                // The old path scanned every changed water cell on the
                // entire island for every drained top cell. A single 1m²
                // column has at most WORLD_HEIGHT cells; bound this lookup
                // to the local vertical column instead.
                let bed=self.height(cell.0,cell.1)+1;
                let top=(bed..cell.2).rev().find(|&z|
                    self.water.amounts.get(&(cell.0,cell.1,z))
                        .is_some_and(|&liters|liters>0));
                if let Some(z)=top {
                    self.water.highest_modified_wet_z.insert((cell.0,cell.1),z);
                } else {self.water.highest_modified_wet_z.remove(&(cell.0,cell.1));}
            }
        }
        self.water.amounts.insert(cell,new_amount);
        let chunk=(cell.0.div_euclid(crate::terrain::CHUNK_SIZE)*crate::terrain::CHUNK_SIZE,
            cell.1.div_euclid(crate::terrain::CHUNK_SIZE)*crate::terrain::CHUNK_SIZE);
        self.water.changed_by_chunk.entry(chunk).or_default().insert(cell);
        self.water.dirty.insert(cell);
        if cell.2>ground {
            // Open water flow is 2D. Do not activate every submerged 3D
            // neighbor on each tiny water transfer.
            self.water.activate_surface((cell.0,cell.1));
            // Exception: a water cell hanging over a mined/open cavity
            // still needs the local 3D falling-water adapter.
            if cell.2>0 && self.water_liters(cell.0,cell.1,cell.2-1)==0
                && !self.block(cell.0,cell.1,cell.2-1).solid() {
                self.water.active.insert(cell);
            }
        } else {
            // Underground edits still need local six-direction connectivity.
            for nearby in std::iter::once(cell).chain(neighbors(cell)) {
                let nearby=canonical(nearby);
                if (0..WORLD_HEIGHT).contains(&nearby.2)
                    && self.water_liters(nearby.0,nearby.1,nearby.2)>0 {
                    self.water.active.insert(nearby);
                }
            }
        }
        self.soil.activate((cell.0,cell.1));
    }
    pub fn take_water_dirty(&mut self) -> HashSet<Cell> {
        std::mem::take(&mut self.water.dirty)
    }
    /// Place a solid voxel, displacing its finite water volume into reachable
    /// cells. Find enough capacity *before* changing anything: failed
    /// placement must leave both water and terrain exactly unchanged.
    pub fn try_place_solid(&mut self, at:Cell, block:Block) -> bool {
        if !block.solid() || !(0..WORLD_HEIGHT).contains(&at.2) {return false;}
        let at=canonical(at);
        if self.block(at.0,at.1,at.2).solid() {return false;}
        let liters=self.water_liters(at.0,at.1,at.2);
        if liters==0 {
            self.set(at.0,at.1,at.2,block);
            return true;
        }

        // Breadth-first local displacement can move water up through a full
        // water column to its free surface. Never teleport through a solid.
        // A bounded search prevents pathologically expensive block placement.
        const MAX_SEARCH:usize=4096;
        let mut visited=HashSet::from([at]);
        let mut queue=VecDeque::from([at]);
        let mut destinations:Vec<(Cell,u16)>=Vec::new();
        let mut remaining=liters;
        while let Some(cell)=queue.pop_front() {
            if visited.len()>MAX_SEARCH || remaining==0 {break;}
            // Prefer the cell above, then lateral paths; gravity will settle
            // the transferred water during subsequent simulation steps.
            let around=[
                (cell.0,cell.1,cell.2+1),
                (cell.0-1,cell.1,cell.2),
                (cell.0+1,cell.1,cell.2),
                (cell.0,cell.1-1,cell.2),
                (cell.0,cell.1+1,cell.2),
                (cell.0,cell.1,cell.2-1),
            ];
            for next in around {
                if !(0..WORLD_HEIGHT).contains(&next.2) {continue;}
                let next=canonical(next);
                if !visited.insert(next) || self.block(next.0,next.1,next.2).solid() {
                    continue;
                }
                let available=CAPACITY_L-self.water_liters(next.0,next.1,next.2);
                if available>0 {
                    let moved=remaining.min(available);
                    destinations.push((next,moved));
                    remaining-=moved;
                    if remaining==0 {break;}
                }
                if visited.len()<=MAX_SEARCH {queue.push_back(next);}
            }
        }
        if remaining!=0 {return false;}
        self.water_assign(at,0);
        for (cell,amount) in destinations {
            let current=self.water_liters(cell.0,cell.1,cell.2);
            self.water_assign(cell,current+amount);
        }
        self.set(at.0,at.1,at.2,block);
        true
    }

    /// Approximate pressure exchange through a small connected, flooded
    /// subterranean passage. Water connectivity transports the pressure
    /// signal; mass is moved between exposed free surfaces at a bounded rate.
    /// This is a sparse conduit adapter, not a full 3D Navier-Stokes solver.
    fn step_conduit_pressure(&mut self,active_cells:&[Cell])->usize {
        const MAX_COMPONENT:usize=4096;
        const MAX_COMPONENTS:usize=8;
        let mut global_seen=HashSet::new();
        let mut transfers=0;
        let mut components=0;
        for &start in active_cells {
            if components>=MAX_COMPONENTS {break;}
            let start=canonical(start);
            if start.2>self.height(start.0,start.1)
                || self.water_liters(start.0,start.1,start.2)==0
                || global_seen.contains(&start) {continue;}
            components+=1;
            let mut queue=VecDeque::from([start]);
            let mut seen=HashSet::from([start]);
            let mut free_surfaces:Vec<(Cell,f64,u16)>=Vec::new();
            while let Some(cell)=queue.pop_front() {
                if seen.len()>MAX_COMPONENT {break;}
                let liters=self.water_liters(cell.0,cell.1,cell.2);
                if liters==0 {continue;}
                let above=(cell.0,cell.1,cell.2+1);
                if cell.2+1<WORLD_HEIGHT
                    && self.block(above.0,above.1,above.2)==Block::Air {
                    free_surfaces.push((cell,cell.2 as f64+liters as f64/1000.0,liters));
                }
                for next in neighbors(cell) {
                    if !(0..WORLD_HEIGHT).contains(&next.2) {continue;}
                    let next=canonical(next);
                    if seen.contains(&next) || self.water_liters(next.0,next.1,next.2)==0 {
                        continue;
                    }
                    seen.insert(next);
                    if seen.len()<=MAX_COMPONENT {queue.push_back(next);}
                }
            }
            global_seen.extend(seen);
            if global_seen.len()>MAX_COMPONENT*MAX_COMPONENTS {break;}
            // A capped/unfinished component must never teleport mass:
            // connectivity to the other free surface was not established.
            if free_surfaces.len()<2 || free_surfaces.len()>MAX_COMPONENT {continue;}
            free_surfaces.sort_unstable_by(|a,b|a.1.total_cmp(&b.1));
            let &(low,low_head,low_amount)=free_surfaces.first().unwrap();
            let &(high,high_head,high_amount)=free_surfaces.last().unwrap();
            if high_head-low_head<=0.001 || high==low {continue;}
            let receiver=if low_amount==CAPACITY_L {
                (low.0,low.1,low.2+1)
            } else {low};
            let available=CAPACITY_L-self.water_liters(receiver.0,receiver.1,receiver.2);
            let moved=((high_head-low_head)*125.0).ceil() as u16;
            let moved=moved.min(100).min(high_amount).min(available);
            if moved==0 {continue;}
            let recipient=self.water_liters(receiver.0,receiver.1,receiver.2);
            self.water_assign(high,high_amount-moved);
            self.water_assign(receiver,recipient+moved);
            // Continue updating this pressure conduit while unequal; only
            // the root below terrain needs to remain a pressure seed.
            self.water.active.insert(start);
            transfers+=1;
        }
        transfers
    }

    /// Top of a contiguous open-water column for rendering/evaporation.
    /// An underground cave or floating liquid cell is not surface water.
    pub(crate) fn exposed_surface_cell(&self,x:i32,y:i32)->Option<Cell> {
        let (x,y)=(wrap(x),wrap(y));
        let col=self.surface_column(x,y);
        if col.blocked || col.liters==0 {return None;}
        let z=col.bed+((col.liters-1)/CAPACITY_L as u32) as i32;
        if z>=WORLD_HEIGHT || self.water_liters(x,y,z)==0
            || self.block(x,y,z+1)!=Block::Air {return None;}
        Some((x,y,z))
    }

    /// Receive precipitation from a regional cloud reservoir into the
    /// lowest available open cell. The caller debits exactly returned liters
    /// from cloud mass; blocked or full columns accept nothing.
    pub(crate) fn receive_precipitation(&mut self,x:i32,y:i32,requested:u16)->u16 {
        let (x,y)=(wrap(x),wrap(y));
        let col=self.surface_column(x,y);
        if col.blocked{return 0;}
        let z=col.bed+(col.liters/CAPACITY_L as u32) as i32;
        if z>=WORLD_HEIGHT || self.block(x,y,z).solid(){return 0;}
        let current=self.water_liters(x,y,z);
        let added=requested.min(CAPACITY_L-current);
        if added>0 {self.water_assign((x,y,z),current+added);}
        added
    }

    /// Remove surface water for transfer into a spatial atmosphere.
    /// Unlike the legacy evaporate() API, it does not use the single global
    /// atmosphere counter. The caller must credit the regional vapor store.
    pub(crate) fn withdraw_surface_evaporation(&mut self,at:Cell,requested:u16)->u16 {
        let current=self.water_liters(at.0,at.1,at.2);
        let moved=current.min(requested);
        if moved>0 {self.water_assign(at,current-moved);}
        moved
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
        // An ordered active set lets us take only the budgeted cells.
        // The old HashSet path drained, sorted and reinserted *every*
        // pending cell each tick (over 40k in the reported recording).
        let budget=max_cells.min(self.water.active.len());
        let mut pending=Vec::with_capacity(budget);
        // Round-robin over the ordered set: repeatedly disturbed low-X
        // cells must not starve the other active regions indefinitely.
        if let Some(last)=self.water.active_cursor {
            pending.extend(self.water.active.range((Excluded(last),Unbounded))
                .take(budget).copied());
        }
        if pending.len()<budget {
            pending.extend(self.water.active.iter().take(budget-pending.len()).copied());
        }
        if let Some(&last)=pending.last() {self.water.active_cursor=Some(last);}
        for &cell in &pending {self.water.active.remove(&cell);}
        let mut transfers=0;
        let mut processed=Vec::new();
        for cell in pending {
            if self.water_liters(cell.0,cell.1,cell.2)==0 {continue;}
            // Only wet cells can drive hydraulic gradients or conduits.
            // Previously thousands of dry/solid neighbors entered the
            // expensive surface-column sort on every 0.2s tick.
            processed.push(cell);
            // Gravity in cavities and unsupported cells stays on the local
            // voxel adapter until the conduit/pressure phase is integrated.
            if self.transfer(cell,(cell.0,cell.1,cell.2-1),500)>0 {
                transfers+=1;
            }
            // Below original terrain, excavated tunnels use the existing
            // local sideways transfer; the free surface uses head fluxes.
            if cell.2<=self.height(cell.0,cell.1) {
                for n in [(cell.0-1,cell.1,cell.2),
                          (cell.0+1,cell.1,cell.2),
                          (cell.0,cell.1-1,cell.2),
                          (cell.0,cell.1+1,cell.2)] {
                    if self.block(n.0,n.1,n.2).solid() {continue;}
                    let a=self.water_liters(cell.0,cell.1,cell.2);
                    let b=self.water_liters(n.0,n.1,n.2);
                    if a>b+1 && self.transfer(cell,n,(a-b)/4)>0 {transfers+=1;}
                }
            }
        }
        // Work on distinct horizontal wet columns, not every voxel in
        // their depth. The column scheduler uses the same round-robin
        // fairness as the sparse underground voxel queue.
        let column_budget=self.water.surface_active.len().min(max_cells);
        let mut columns=Vec::with_capacity(column_budget);
        if let Some(last)=self.water.surface_cursor {
            columns.extend(self.water.surface_active
                .range((Excluded(last),Unbounded)).take(column_budget).copied());
        }
        if columns.len()<column_budget {
            columns.extend(self.water.surface_active.iter()
                .take(column_budget-columns.len()).copied());
        }
        if let Some(&last)=columns.last() {self.water.surface_cursor=Some(last);}
        for &cell in &columns {self.water.surface_active.remove(&cell);}
        transfers+=self.step_surface_flow(&columns);
        transfers+self.step_conduit_pressure(&processed)
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
    fn placing_inside_water_displaces_every_liter_and_preserves_surroundings() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        let center=(x,y,240);
        t.water_assign(center,750);
        t.water_assign((x,y,241),0);
        let before:u32=t.water.amounts.values().map(|&v|v as u32).sum();
        assert!(t.try_place_solid(center,Block::Stone));
        assert_eq!(t.block(x,y,240),Block::Stone);
        assert_eq!(t.water_liters(x,y,240),0);
        assert_eq!(t.water_liters(x,y,241),750);
        assert_eq!(before,t.water.amounts.values().map(|&v|v as u32).sum());
    }

    #[test]
    fn placing_into_full_water_column_moves_volume_to_surface() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        for z in 232..=235 {t.water_assign((x,y,z),CAPACITY_L);}
        let before:u32=t.water.amounts.values().map(|&v|v as u32).sum();
        assert!(t.try_place_solid((x,y,232),Block::Dirt));
        assert_eq!(t.block(x,y,232),Block::Dirt);
        assert_eq!(before,t.water.amounts.values().map(|&v|v as u32).sum());
    }

    #[test]
    fn underwater_mining_does_not_erase_adjacent_water() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        t.set(x,y,239,Block::Stone);
        t.water_assign((x,y,240),550);
        t.set(x,y,239,Block::Air);
        assert_eq!(t.water_liters(x,y,239),0);
        assert_eq!(t.water_liters(x,y,240),550);
        t.step_water(64);
        let total:u32=t.water.amounts.values().map(|&v|v as u32).sum();
        assert_eq!(total,550);
    }

    #[test]
    fn initial_surface_elevation_is_voxel_top_not_top_plus_one() {
        let t=Terrain::new();
        let x=WORLD_SIZE/2-115;
        let y=WORLD_SIZE/2+125;
        assert_eq!(t.water_liters(x,y,125),1000);
        assert_eq!(t.water_liters(x,y,126),0);
        assert_eq!(t.initial_water_surface(x,y),126);
    }
    #[test]
    fn closed_water_ledger_accounts_for_implicit_lakes_and_atmosphere() {
        let mut t=Terrain::new();
        let cell=(WORLD_SIZE/2-115,WORLD_SIZE/2+125,125);
        assert_eq!(t.water.surface_delta_liters,0);
        assert_eq!(t.evaporate(cell,100),100);
        assert_eq!(t.water.surface_delta_liters,-100);
        assert_eq!(t.water.surface_delta_liters+t.water.atmosphere_liters as i64,0);
        assert_eq!(t.precipitate(cell,50),50);
        assert_eq!(t.water.surface_delta_liters+t.water.atmosphere_liters as i64,0);
    }
    #[test]
    fn lake_water_is_finite_and_does_not_refill_after_evaporation() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2-115;
        let y=WORLD_SIZE/2+125;
        assert_eq!(t.water_liters(x,y,125),CAPACITY_L);
        assert_eq!(t.evaporate((x,y,125),CAPACITY_L),CAPACITY_L);
        assert_eq!(t.water_liters(x,y,125),0);
        assert_eq!(t.block(x,y,125),Block::Air);
        assert_eq!(t.water.atmosphere_liters,CAPACITY_L as u64);
    }
    #[test]
    fn digging_under_the_lake_moves_existing_water_not_new_water() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2-115;
        let y=WORLD_SIZE/2+125;
        let floor=t.height(x,y);
        assert!(floor<126);
        assert!(t.block(x,y,floor).solid());
        t.set(x,y,floor,Block::Air);
        assert_eq!(t.water_liters(x,y,floor),0);
        t.step_water(512);
        assert!(t.water_liters(x,y,floor)>0,"lake must reach mined floor");
        let moved=t.water_liters(x,y,floor);
        assert!(moved<=CAPACITY_L);
        // Pressure redistribution may replenish the top cell from its
        // neighbors; the whole implicit water ledger must still balance.
        assert_eq!(t.water.surface_delta_liters,0);
    }
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
    fn surface_column_has_absolute_head_on_uneven_ocean_floor() {
        let t=Terrain::new();
        let a=t.surface_column(0,0);
        let b=t.surface_column(WORLD_SIZE/2,160);
        assert_eq!(t.initial_water_surface(0,0),110);
        assert_eq!(t.initial_water_surface(WORLD_SIZE/2,160),110);
        assert!((a.head_m-110.0).abs()<0.001);
        assert!((b.head_m-110.0).abs()<0.001);
        assert_ne!(a.liters,b.liters,"different bed depths hold different volume");
    }
    #[test]
    fn level_lake_and_ocean_never_spontaneously_flow() {
        let mut t=Terrain::new();
        let lake=(WORLD_SIZE/2-115,WORLD_SIZE/2+125,125);
        t.water.activate_surface((lake.0,lake.1));
        let ocean=(0,0,109);
        t.water.activate_surface((ocean.0,ocean.1));
        for _ in 0..10_000 {
            assert_eq!(t.step_water(4096),0);
        }
        assert_eq!(t.water.surface_delta_liters,0);
        assert!(t.water.amounts.is_empty());
    }
    #[test]
    fn displaced_water_spreads_downhill_and_conserves_exact_volume() {
        let mut t=Terrain::new();
        // A non-porous temporary spill beside a connected lake bank.
        let m=WORLD_SIZE/2;
        let y=m+125;
        let (x,z)=(80..=125).map(|r| {
            let x=m-115+r;
            (x,t.height(x,y)+1)
        }).find(|&(_,z)|z>=127).expect("high ground near lake bank");
        t.water_assign((x,y,z),1000);
        let initial_delta=t.water.surface_delta_liters;
        let mut has_flow=false;
        for _ in 0..100 {
            if t.step_water(4096)>0 {has_flow=true;}
            if t.water.active.is_empty() && t.water.surface_active.is_empty(){break;}
        }
        assert!(has_flow,"spill should move according to head");
        assert_eq!(t.water.surface_delta_liters,initial_delta,
            "hydraulic flow must conserve volume against implicit baseline");
    }
    #[test]
    fn impermeable_shore_spill_flows_back_to_connected_lake() {
        let mut t=Terrain::new();
        let m=WORLD_SIZE/2;
        // Radial bank of the authored elevated lake: r=78 is outside its
        // initial shoreline, and r<=76 contains connected open lake water.
        let lake_x=m-115;
        let y=m+125;
        let shore_x=lake_x+78;
        let z=t.height(shore_x,y)+1;
        assert_eq!(t.water_liters(shore_x,y,z),0);
        t.water_assign((shore_x,y,z),1000);
        let initial_delta=t.water.surface_delta_liters;
        // No soil/atmosphere functions run: all spill must remain surface.
        for _ in 0..150 {
            t.step_water(4096);
        }
        let lake_return:u32=t.water.amounts.iter()
            .filter(|(&(x,yy,z),&liters)| {
                let dx=x-lake_x;let dy=yy-y;
                dx*dx+dy*dy<=76*76 && z>=126 && liters>0
            })
            .map(|(_, &liters)|liters as u32).sum();
        assert!(lake_return>0,
            "water must return across a valid downhill bank into the lake");
        assert_eq!(t.water.surface_delta_liters,initial_delta,
            "shore runoff must not evaporate or infiltrate implicitly");
    }
    #[test]
    fn surface_water_does_not_cross_a_solid_crest_from_below() {
        let mut t=Terrain::new();
        let m=WORLD_SIZE/2;
        let source=(m,m,240);
        let neighbor=(m+1,m,240);
        t.water_assign(source,100);
        // A cap at z240 obstructs the receiving column.
        t.set(neighbor.0,neighbor.1,240,Block::Stone);
        let before=t.water.surface_delta_liters;
        for _ in 0..10 {t.step_water(4096);}
        assert_eq!(t.water.surface_delta_liters,before);
        assert_eq!(t.water_liters(neighbor.0,neighbor.1,240),0);
    }
    #[test]
    fn pressurized_u_tube_equalizes_through_a_roofed_bottom_passage() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;
        let y=WORLD_SIZE/2;
        // Excavated U-shaped flooded tunnel below the natural ground.
        // Bottom cells lie at Z118; overlying stone forms the roof. Both
        // shafts have 1m air cavities immediately above their waterlines.
        for z in 118..=124 {t.set(x,y,z,Block::Air);}
        for z in 118..=122 {t.set(x+3,y,z,Block::Air);}
        for dx in 1..=2 {t.set(x+dx,y,118,Block::Air);}
        for z in 118..=123 {t.water_assign((x,y,z),CAPACITY_L);}
        for z in 118..=121 {t.water_assign((x+3,y,z),CAPACITY_L);}
        for dx in 1..=2 {t.water_assign((x+dx,y,118),CAPACITY_L);}
        let before=t.water.surface_delta_liters;
        t.water.activate((x+1,y,118));
        let mut moves=0;
        for _ in 0..40 {moves+=t.step_water(4096);}
        assert!(moves>0,"connected flooded conduit must transmit pressure");
        assert!(t.water_liters(x,y,123)<CAPACITY_L);
        assert!(t.water_liters(x+3,y,122)>0);
        assert_eq!(t.water.surface_delta_liters,before,
            "pressure exchange must conserve every liter");
    }
    #[test]
    #[ignore = "manual diagnostic; do not assert runner-dependent timing"]
    fn hydrology_lab_perf_measurement() {
        use std::time::Instant;
        let mut calm=Terrain::new();
        calm.water.activate((0,0,109));
        let start=Instant::now();
        for _ in 0..1000 {calm.step_water(4096);}
        let calm_ms=start.elapsed().as_secs_f64()*1000.0/1000.0;
        let mut disturbed=Terrain::new();
        let lake=(WORLD_SIZE/2-115,WORLD_SIZE/2+125,125);
        disturbed.water_assign(lake,900);
        let mass=disturbed.water.surface_delta_liters;
        let start=Instant::now();
        let mut transfers=0usize;
        let mut dirty=0usize;
        for _ in 0..1000 {
            transfers+=disturbed.step_water(4096);
            dirty+=disturbed.take_water_dirty().len();
        }
        let active_ms=start.elapsed().as_secs_f64()*1000.0/1000.0;
        assert_eq!(disturbed.water.surface_delta_liters,mass);
        eprintln!("hydrology bench: calm={:.3} ms/tick active={:.3} ms/tick transfers={} dirty_cells={}",
            calm_ms,active_ms,transfers,dirty);
    }
    #[test]
    fn rain_and_draining_lake_survive_repeated_solver_ticks_with_zero_mass_error() {
        let mut t=Terrain::new();
        let mut atmosphere=crate::atmosphere::Atmosphere::new_af();
        let x=WORLD_SIZE/2-115;let y=WORLD_SIZE/2+125;
        let bed=t.height(x,y)+1;
        assert!(t.water_liters(x,y,bed)>0);
        // A drainage cavity below multiple layers of implicit lake water
        // creates an unsettled column while the upper lake stays flooded.
        t.set(x,y,bed-1,Block::Air);
        for tick in 0..80 {
            if tick%8==0 {
                atmosphere.advance_af(3_600.0);
                atmosphere.rain_near(&mut t,x+72,y,3_600.0);
                atmosphere.evaporate_near(&mut t,x+72,y,3_600.0);
                t.step_soil(3_600.0,4096);
                t.step_groundwater(3_600.0,4096);
            }
            t.step_water(4096);
            let error=t.water_mass_error_liters() as f64
                +atmosphere.water_delta_kg();
            assert!(error.abs()<0.01,"closed water budget drift at tick {tick}: {error}");
        }
    }
    #[test]
    fn gapped_column_donor_moves_real_water_not_derived_waterline() {
        let mut t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2-115,WORLD_SIZE/2+125);
        let bed=t.height(x,y)+1;
        assert!(t.initial_water_surface(x,y)>bed+1);
        // A mined drainage path can empty a lower layer while upper
        // implicit lake water is still present. Aggregate volume alone
        // does not tell us where those real liters reside.
        t.water_assign((x,y,bed),0);
        let upper=t.initial_water_surface(x,y)-1;
        assert_eq!(t.water_liters(x,y,upper),CAPACITY_L);
        let before=t.water.surface_delta_liters;
        t.remove_surface_liters((x,y),45,bed);
        assert_eq!(t.water_liters(x,y,upper),CAPACITY_L-45);
        assert_eq!(t.water.surface_delta_liters,before-45);
    }
    #[test]
    fn gapped_column_receiver_fills_lowest_real_open_cell() {
        let mut t=Terrain::new();
        let (x,y)=(WORLD_SIZE/2-115,WORLD_SIZE/2+125);
        let bed=t.height(x,y)+1;
        t.water_assign((x,y,bed),0);
        let before=t.water.surface_delta_liters;
        t.add_surface_liters((x,y),45,bed);
        assert_eq!(t.water_liters(x,y,bed),45);
        assert_eq!(t.water.surface_delta_liters,before+45);
    }
    #[test]
    fn water_changes_wake_columns_not_stacks_of_voxels() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        let z=t.height(x,y)+1;
        t.water_assign((x,y,z),100);
        assert!(t.water.active.is_empty());
        assert_eq!(t.water.surface_active.len(),5);
        assert!(t.water.surface_active.contains(&(x,y)));
    }
    #[test]
    fn hydrology_tick_obeys_active_budget_without_draining_backlog() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        t.water.active.clear();
        for dx in 0..5000 {
            t.water.active.insert((wrap(x+dx),y,240));
        }
        let count=t.water.active.len();
        assert!(count>=500,"budget fixture must contain many dry cells");
        assert_eq!(t.step_water(17),0);
        assert_eq!(t.water.active.len(),count-17);
    }
    #[test]
    fn water_work_budget_rotates_instead_of_starving_later_cells() {
        let mut t=Terrain::new();
        t.water.active.clear();
        let y=WORLD_SIZE/2;
        for x in 0..10 {t.water.active.insert((x,y,240));}
        t.step_water(3);
        assert!(!t.water.active.contains(&(0,y,240)));
        for x in 0..3 {t.water.active.insert((x,y,240));}
        t.step_water(3);
        assert!(t.water.active.contains(&(0,y,240)));
        for x in 3..6 {assert!(!t.water.active.contains(&(x,y,240)));}
    }
    #[test]
    fn modified_water_cells_are_indexed_by_canonical_chunk() {
        let mut t=Terrain::new();
        let y=WORLD_SIZE/2;
        t.water_assign((-1,y,240),100);
        t.water_assign((0,y,240),200);
        let left=(WORLD_SIZE-32,y);
        let right=(0,y);
        assert!(t.water.changed_by_chunk[&left].contains(&(WORLD_SIZE-1,y,240)));
        assert!(t.water.changed_by_chunk[&right].contains(&(0,y,240)));
        t.water_assign((0,y,240),400);
        assert_eq!(t.water.changed_by_chunk[&right].len(),1,
            "editing the same cell must not duplicate mesh-index entries");
    }
    #[test]
    fn deep_surface_water_activation_does_not_multiply_with_depth() {
        let mut t=Terrain::new();
        let x=0;let y=0;
        let bed=t.height(x,y)+1;
        assert!(t.initial_water_surface(x,y)-bed>20);
        for z in bed..t.initial_water_surface(x,y) {
            t.water_assign((x,y,z),999);
        }
        assert!(t.water.active.len()<=1,
            "ocean depth must not multiply surface update work");
        assert_eq!(t.water.surface_active.len(),5);
    }
    #[test]
    fn low_head_difference_does_not_churn_a_liter_forever() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        let z=t.height(x,y)+1;
        t.water_assign((x,y,z),100);
        let prior=t.water.surface_delta_liters;
        for _ in 0..20 {t.step_water(4096);}
        assert_eq!(t.water.surface_delta_liters,prior);
        assert!(t.water.surface_active.len()<20);
    }
    #[test]
    fn dry_active_neighbors_are_not_hydraulic_sources() {
        let mut t=Terrain::new();
        let x=WORLD_SIZE/2;let y=WORLD_SIZE/2;
        t.water.activate((x,y,230));
        assert_eq!(t.step_water(4096),0);
        assert_eq!(t.water.surface_delta_liters,0);
        assert!(t.water.amounts.is_empty());
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
