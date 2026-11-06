# Hydrology solver decision — 2026-11-06 video regression

**Status:** Experimental design gate and implementation checklist. Not production-ready. Keep `main` untouched; work only on the 1,024 m island branch and draft PR #12. This supersedes the earlier assumption that more per-voxel flow or meshing optimizations alone can make the water behave realistically.

## Implementation checkpoint (2026-11-06)

The user authorized active implementation on the draft experimental branch. These
items are now in code, not just a research proposal:

- `src/surface_solver.rs`: standalone conservative 2D virtual-pipe momentum
  kernel with deterministic tests for lake-at-rest, wet/dry dam break, bank
  overtopping, four-neighbor donor limits, and long-run exact liter conservation.
  This is a bounded low-order hydraulic prototype, **not** a verified HLLC
  finite-volume Saint-Venant solver.
- `src/hydrology.rs`: open-water volume is owned by one packed surface-column
  ledger backed by finite implicit initial ocean/lake volumes plus sparse
  deltas. `water_liters` projects that inventory into voxel-facing APIs.
  Underground excavated cells retain sparse independent 3D water amounts.
  Source/receiver transfers change the *column inventory*, never independently
  edit underwater layers. Player-placed solids displace water into available
  height, and immediate roofs are not misreported as free surfaces.
- Persistent signed edge velocities are integrated in eight 25ms physical
  substeps per 0.2s water tick; paired integer-liter transfers are batched in
  scratch columns and committed once, avoiding intermediate 3D remesh churn.
  Tiny fractional flows retain remainder and tiles with momentum remain awake.
- Water simulation activation is now by 32m tile, not every wet 3D voxel or
  a tens-of-thousands-entry BTreeSet of square-meter columns. Soil and
  groundwater schedulers also use bounded, fair ordered cursors.
- `src/mesh.rs`: open water renders one top quad per wet 1m² surface column,
  plus exposed interface sides, rather than stacking transparent cubic
  water faces through a deep ocean. Sparse excavated 3D water still has local
  faces.
- Regressions cover mass exchanges, toroidal seam, submerged block
  displacement, lake height, lake-rest stability, dry banks, roofs, caves,
  atmospheric conservation and rain.

**Known unverified limitations:**

1. Graphics are still the existing basic WGPU alpha-blended water shader. The
   new water surface has flat 1m quads, with visible shallow-water and river
   steps until edge interpolation, depth/underwater attenuation and wet/dry
   clipping are implemented.
2. Surface momentum is a simple virtual-pipe approximation; river discharge
   and hydraulic equilibrium need stronger time-step and benchmark checks,
   especially at large jumps, culverts and blocked columns.
3. The sparse 3D cave/conduit transfer remains the older prototype. It is not
   yet a general bidirectional conservation-tested pressure graph with
   explicit exchanges for stacked water reservoirs.
4. F1 scales the Af climate subsystem but does **not** accelerate 5Hz hydraulic
   flow; long-term catchment balance needs a multirate reservoir method.
5. Regional rain and evaporation still run only near the player. The
   month-by-month whole-island Af target is not validated.
6. Windows and Ubuntu CI exercises source-level correctness; actual FPS,
   remaining underwater visual faults and water walls require the next
   hands-on gameplay video. No claims of 60fps or completed hydrology are
   justified by CI alone.

**Release guardrail:** No merge to `main` until a video and quantitative
performance review confirm the new solver and water rendering are materially
better, with no new regression.

## Observation, not conjecture

The November 6 video (09:30 recording) shows a free-standing vertical water sheet at startup, irregular thin/overlapping underwater water panels, and stepwise water faces against low banks. The release console reports startup 0.7s, water tick 25–36ms with 35k–52k active voxel cells, climate/soil batch 26–29ms, and dynamic remesh 26–37ms. There is no reported panic. The video shows *behavioral* correctness failure, not merely a GPU throughput problem.

Source-code mechanisms in the current implementation:

1. `Terrain::initial_water_surface` gives the initial river water elevation inside a 9m-wide channel and a much lower baseline outside it; `Terrain::height` had blended those adjacent banks below the filled level. The wetland perimeter had the same discontinuity. **Patch in progress:** correct the authored banks/low fringe, with an explicit test that neighboring dry terrain never lies below the authored waterline. This fixes invalid initial geometry only.
2. `WaterState` stores surface fluid as per-voxel liter overrides; `surface_column` treats their aggregate volume as a contiguous packed column. Gravity and excavation can create gaps, so the volume-derived head need not describe the *actual* free surface. Earlier fixes avoided a panic but did not restore this invariant.
3. `step_water` operates on individual wet voxels, allocating at most 4,096 processed cells per 0.2s tick; a water volume 10m deep can be queued at many vertical levels. Repeated small edge transfers keep waking neighbors and produce tens of thousands of active cells.
4. `step_surface_flow` transfers no more than 250L/edge/tick, uses a hydrostatic head comparison without momentum, and schedules only where a cell has been activated. The initial implicitly stored river is **not automatically activated**, so a slope is initially frozen until disturbed. A constant arbitrary per-tick limit also does not represent physical flow capacity.
5. `mesh::build_water` emits visible faces from individual water voxels. Blended faces are not sorted by depth. An actual 2D free surface can be continuous even if ground remains blocky; rendering block-face water panels makes discontinuities and transparency errors conspicuous.

The literal `ERR 0L` HUD is a **mass-ledger check**, not evidence of correct momentum, pressure, geometry, or visual behavior.

## Libraries evaluated

| Option | Evidence / fit | Decision |
| --- | --- | --- |
| [Salva](https://github.com/dimforge/salva) (`salva2d`, `salva3d`) | Rust particle-based DFSPH/IISPH, viscosity, surface tension; optional Rapier coupling. v0.11.0 changelog dated 2026-11-01. Good for local splashes, contained basins, fluid-body coupling, not a direct sparse 1km ocean/groundwater/climate representation. | **Do not adopt as primary island solver.** Revisit for small active 3D effects after surface conservation is proved. |
| [Dimforge Nexus](https://github.com/dimforge/nexus) | GPU MPM fluids and rigid bodies, early/high-change project, Rust-GPU/SPIR-V shader toolchain; would impose substantial dependency/integration complexity atop existing WGPU Vulkan. | **Not now.** A future optional high-detail near-player simulator, not baseline catchment water ledger. |
| Full-world 3D voxel flux / SPH / FLIP | Handles complex geometry but scales with wet *volume* and/or particle count. Large deep oceans become expensive without adding visible information. | **Reject as primary grid.** Use sparse 3D locally for underground connectivity. |
| 2D conservative finite-volume / virtual-pipe shallow water | Each open-water column has one volume/head, shared edge fluxes, terrain-height boundary crests, and optional momentum. Scales with wet *surface area*, permits a cheap still ocean and a continuous rendering surface. Virtual pipes: Mei, Decaudin & Hu 2007; extended multilayer connections: Dagenais et al. 2018. | **Choose** for river, ocean, lakes, marshes and rain runoff. |
| Sparse graph-connected conduits / multilayer heightfields | Necessary for U-tubes, water below ceilings and multiple reservoirs stacked at the same x,y. Can exchange conserved liters through explicitly connected openings to 2D surface columns. | **Choose** as separate local 3D subsystem; cannot be represented by one heightfield. |

References: [Mei et al. 2007](https://evasion.imag.fr/Publications/2007/MDH07/); [Extended Virtual Pipes 2018](https://www.sciencedirect.com/science/article/pii/S0097849318301341); [Integrating Fluid Simulation with a Voxel Engine 2016](https://link.springer.com/article/10.1007/s40869-016-0020-5); [Kellomäki 2014 volume displacement](https://www.modeemi.fi/~daemou/ijcgt-final.pdf). **No performance claim is made for a library without benchmarking on this project.**

## Implementation gates — do not hide the physics failure under an aesthetic patch

### Gate A: valid authored initial conditions (isolated)
- [x] Record waterfall/bank/wetland failure from video and localize the initial surface geometry mismatch.
- [ ] Make the river's initial water boundary supported by bank elevation or connected wet lower ground; lake and ocean remain level.
- [ ] Make wetland fringe inundation consistent with terrain elevation; no water-wall cliff at a lower dry cell.
- [ ] Add tests for boundary support and global liter accounting; pass both Linux and Windows CI.
- [ ] Explicitly distinguish correction of test-map authoring from a working fluid solver.

### Gate B: authoritative finite free-surface storage
- [ ] Keep solid voxel edits and cave water separate from aboveground fluid. Open cell `(x,y)` owns `volume_liters`, `bed_m` (effective edge/passability), and optional momentum/edge flux. Avoid a stack of active water voxels per deep column.
- [ ] Preserve implicit finite initial ocean inventory without eagerly allocating 1,048,576 cells; cache regions/tiles and use sparse delta for disturbed columns. **Do not add infinite ocean refill.**
- [ ] Supply exactly one projection `water_volume_at_voxel(x,y,z)` for collision, edits and rendering, derived from the authoritative surface volume. Empty gaps below a free surface are impossible unless an explicitly separate cave/conduit exists.
- [ ] Convert `try_place_solid`, mine, rain, soil and evaporation paths to ledger debit/credit once, with mass conservation checked at the API boundary.

### Gate C: conservative surface flux / hydrostatic and river tests
- [ ] Use paired edge fluxes computed from one snapshot; scale by `dt` and hydraulic head/crest, and constrain total outgoing flux to donor liters. Carry fractional remainders so small flows don't round into permanent 1L oscillations.
- [ ] Add a CFL/stability bound (substep or stable flux formulation) and make rest-at-equal-head *exactly* zero flow irrespective of terrain bed.
- [ ] Activate region/tile surface flows at initial rivers and newly disturbed wet/dry interfaces, not every vertically submerged voxel. No permanently frozen initial river.
- [ ] Test basin at rest on variable bed; breached dam; flooding higher/lower bank; step river discharge without a prescribed infinite source; wrap seam; sealed barriers; exact closed water mass before/after thousands of iterations.
- [ ] Keep momentum/wave features only as warranted by observable test failures; first conservative flux milestone may use a damped virtual-pipe rate, clearly labelled as such.

### Gate D: 3D connection and rainfall/soil
- [ ] Model underwater excavations as sparse connected volumes with explicit surface ↔ conduit exchange; U-tube pressure equalizes without teleporting through rock; solid block displaces liters.
- [ ] Keep 16×16 weather-cell vapor/cloud as separate finite masses; surface evap and rain credit only the new authoritative open-water storage.
- [ ] Preserve 120-day simulated year and lab Af criteria (every 10-day month >=60mm and coldest monthly mean >=18°C) as **measured targets**, not created rainfall. F1 fast climate time must not multiply real-time water solver steps.

### Gate E: coherent water rendering and performance
- [ ] Generate one mesh from actual open-water elevation per surface region, with continuous top polygons, wet/dry clipping and only physically justified waterfall boundaries; terrain stays voxel-based.
- [ ] Handle underwater attenuation/refraction or fog separately; do not build interleaved transparent cube walls. Validate both above and below the surface.
- [ ] Instrument active surface *columns* vs cave cells, water solver time, mesh rebuild time and frame percentiles; compare identical camera/time routes. On the user's machine, target a near-shoreline 60fps experience without recurrent >25ms simulation work. Establish baselines rather than assume new code is faster.
- [ ] Only then evaluate near-field Salva/Nexus effects as optional visual detail with exact two-way water-volume reconciliation.

### Guardrail

No library adoption or broad rewrite is justified by appearance alone. For each gate, require a failing regression or measured defect, a bounded change, passing CI, and a gameplay recording only when static tests cannot validate the outcome. Keep PR #12 as a draft and `main` untouched until water *behavior* is credible.
