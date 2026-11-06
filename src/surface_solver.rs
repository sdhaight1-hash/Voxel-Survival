//! Conservative 2D free-surface hydraulics (1m x 1m cells).
//! This module does not know about voxels, meshing, soil or weather.
//! Water volume is owned by columns; an edge stores persistent momentum.
//! All transfers are integer liters and balanced across each shared face.

const GRAVITY:f64=9.80665;
const LITERS_PER_M3:f64=1000.0;
const MIN_DEPTH_M:f64=0.000_01;

#[derive(Clone,Copy,Debug)]
pub struct SurfaceCell {
    pub bed_m:f64,
    pub liters:u64,
    pub blocked:bool,
}
impl SurfaceCell {
    pub fn stage_m(self)->f64 {self.bed_m+self.liters as f64/LITERS_PER_M3}
}

#[derive(Clone,Copy,Debug,Default)]
pub struct EdgeFlow {
    /// Signed positive from left to right, in cubic meters per second.
    pub rate_m3_s:f64,
    /// Sub-liter transport retained between physical substeps.
    pub remainder_l:f64,
}

/// Hydraulic edge acceleration with a real shared bottom/crest.
/// A lake at rest (identical absolute stages, different bottom heights)
/// yields exactly zero rate from zero initial momentum.
pub fn advance_edge(flow:&mut EdgeFlow,a:SurfaceCell,b:SurfaceCell,dt:f64) {
    assert!(dt>0.0 && dt<=0.025,"hydraulic substep violates conservative time budget");
    if a.blocked || b.blocked {
        *flow=EdgeFlow::default();
        return;
    }
    let crest=a.bed_m.max(b.bed_m);
    let eta_a=a.stage_m();
    let eta_b=b.stage_m();
    let depth=(eta_a.max(eta_b)-crest).max(0.0);
    if depth<=MIN_DEPTH_M {
        *flow=EdgeFlow::default();
        return;
    }
    // The virtual-pipe approximation stores signed edge momentum and
    // therefore does not drop all velocity at each tick. Linear friction
    // damps oscillation without synthesizing water.
    flow.rate_m3_s=(flow.rate_m3_s+GRAVITY*depth*(eta_a-eta_b)*dt)
        *(1.0-2.0*dt).max(0.0);
    if (flow.rate_m3_s>0.0 && eta_a<=crest)
        || (flow.rate_m3_s<0.0 && eta_b<=crest) {
        *flow=EdgeFlow::default();
    }
}

/// The local test domain is intentionally small. In-game scheduling will use
/// the same edge operation over active surface tiles, not allocate a full
/// vertical stack of ocean voxels.
pub struct SurfaceGrid {
    pub width:usize,
    pub height:usize,
    pub cells:Vec<SurfaceCell>,
    // One edge each to +x and +y, owned by its lower-coordinate cell.
    east:Vec<EdgeFlow>,
    north:Vec<EdgeFlow>,
}

impl SurfaceGrid {
    pub fn new(width:usize,height:usize,bed_m:f64)->Self {
        assert!(width>0 && height>0);
        let size=width*height;
        Self {
            width,height,
            cells:vec![SurfaceCell {bed_m,liters:0,blocked:false};size],
            east:vec![EdgeFlow::default();size],
            north:vec![EdgeFlow::default();size],
        }
    }
    fn index(&self,x:usize,y:usize)->usize {y*self.width+x}
    pub fn total_liters(&self)->u64 {
        self.cells.iter().map(|c|c.liters).sum()
    }
    /// Integrate a bounded physical timestep. This wrapper substeps even
    /// when caller requests an accumulated 0.2s gameplay water tick.
    pub fn step(&mut self,seconds:f64)->usize {
        assert!(seconds>=0.0 && seconds<=10.0);
        let n=(seconds/0.02).ceil() as usize;
        if n==0 {return 0;}
        let dt=seconds/n as f64;
        let mut transfers=0;
        for _ in 0..n {transfers+=self.substep(dt);}
        transfers
    }
    fn substep(&mut self,dt:f64)->usize {
        // Stage one: compute all shared edge rates from a single snapshot.
        // An edge points to its receiver, and a donor's proposed outgoing
        // flow is normalized against its available volume before commit.
        #[derive(Clone,Copy)]
        struct Proposal {a:usize,b:usize,requested:u64,forward:bool,crest:f64}
        let mut proposals=Vec::new();
        let mut outflow=vec![0u64;self.cells.len()];
        for y in 0..self.height {
            for x in 0..self.width {
                let i=self.index(x,y);
                for axis in 0..2 {
                    let next=if axis==0 {
                        if x+1==self.width {continue;}
                        self.index(x+1,y)
                    } else {
                        if y+1==self.height {continue;}
                        self.index(x,y+1)
                    };
                    let flow=if axis==0 {&mut self.east[i]}
                        else {&mut self.north[i]};
                    let a=self.cells[i];
                    let b=self.cells[next];
                    advance_edge(flow,a,b,dt);
                    if flow.rate_m3_s==0.0 {continue;}
                    let forward=flow.rate_m3_s>0.0;
                    let donor=if forward {i} else {next};
                    let crest=a.bed_m.max(b.bed_m);
                    let spillable=((self.cells[donor].stage_m()-crest).max(0.0)
                        *LITERS_PER_M3).floor() as u64;
                    let fractional=flow.rate_m3_s.abs()*dt*LITERS_PER_M3
                        +flow.remainder_l;
                    let wanted=fractional.floor() as u64;
                    flow.remainder_l=fractional-wanted as f64;
                    if wanted==0 {continue;}
                    // Cap at the amount above the crest, not the whole
                    // submerged column (a bank cannot drain the lake dry).
                    let requested=wanted.min(spillable);
                    if requested==0 {continue;}
                    outflow[donor]=outflow[donor].saturating_add(requested);
                    proposals.push(Proposal {a:i,b:next,requested,forward,crest});
                }
            }
        }
        let mut remaining=vec![0u64;self.cells.len()];
        for (i,&requested) in outflow.iter().enumerate() {
            remaining[i]=requested.min(self.cells[i].liters);
        }
        // Stage two: never spend more water than the donor had at the
        // start of the step, even if four neighbors all want it.
        // Sequential integer rounding is deterministic and conservative.
        let mut debit=vec![0u64;self.cells.len()];
        let mut credit=vec![0u64;self.cells.len()];
        let mut moved_edges=0;
        for proposal in proposals {
            let from=if proposal.forward {proposal.a} else {proposal.b};
            let to=if proposal.forward {proposal.b} else {proposal.a};
            let available_above_crest=((self.cells[from].stage_m()
                -proposal.crest).max(0.0)*LITERS_PER_M3).floor() as u64;
            let available=available_above_crest.saturating_sub(debit[from])
                .min(remaining[from]);
            let moved=proposal.requested.min(available);
            if moved>0 {
                debit[from]+=moved;
                credit[to]+=moved;
                remaining[from]-=moved;
                moved_edges+=1;
            }
        }
        for i in 0..self.cells.len() {
            self.cells[i].liters=self.cells[i].liters-debit[i]+credit[i];
        }
        moved_edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn still_lake_over_uneven_bed_is_exactly_stationary() {
        let mut g=SurfaceGrid::new(9,3,0.0);
        for y in 0..3 {for x in 0..9 {
            let i=g.index(x,y);
            g.cells[i].bed_m=((x*7+y*3)%5) as f64;
            g.cells[i].liters=((7.0-g.cells[i].bed_m)*1000.0) as u64;
        }}
        let before=g.cells.iter().map(|c|c.liters).collect::<Vec<_>>();
        for _ in 0..100 {assert_eq!(g.step(0.2),0);}
        assert_eq!(g.cells.iter().map(|c|c.liters).collect::<Vec<_>>(),before);
    }
    #[test]
    fn dam_break_wets_dry_ground_without_creating_water() {
        let mut g=SurfaceGrid::new(9,1,0.0);
        g.cells[1].liters=12_000;
        let initial=g.total_liters();
        let mut moved=0;
        for _ in 0..80 {moved+=g.step(0.2);}
        assert!(moved>0);
        assert!(g.cells[2].liters>0);
        assert_eq!(g.total_liters(),initial);
    }
    #[test]
    fn dry_bank_blocks_until_water_overtops_crest() {
        let mut g=SurfaceGrid::new(3,1,0.0);
        g.cells[1].bed_m=3.0;
        g.cells[0].liters=2_000;
        for _ in 0..30 {g.step(0.2);}
        assert_eq!(g.cells[1].liters,0);
        assert_eq!(g.cells[2].liters,0);
        g.cells[0].liters+=3_000;
        let initial=g.total_liters();
        for _ in 0..50 {g.step(0.2);}
        assert!(g.cells[1].liters>0 || g.cells[2].liters>0);
        assert_eq!(g.total_liters(),initial);
    }
    #[test]
    fn lake_momentum_moves_and_damps_without_mass_error() {
        let mut g=SurfaceGrid::new(8,2,0.0);
        for i in 0..g.cells.len() {g.cells[i].liters=2_000;}
        g.cells[0].liters+=800;
        let initial=g.total_liters();
        for _ in 0..1000 {g.step(0.2);}
        assert_eq!(g.total_liters(),initial);
        assert!(g.cells.iter().all(|c|c.liters>0));
    }
    #[test]
    fn wet_cell_cannot_be_overdrawn_by_four_dry_neighbors() {
        let mut g=SurfaceGrid::new(3,3,0.0);
        g.cells[4].liters=100;
        let initial=g.total_liters();
        for _ in 0..200 {g.step(0.2);}
        assert_eq!(g.total_liters(),initial);
    }
}
