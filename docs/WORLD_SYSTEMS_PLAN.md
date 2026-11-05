# Voxel Survival — World Systems Implementation Plan

This plan separates what is **implemented in this pass** from the systems that
still require engineering work. A 1024 km × 1024 km × 256 m, 1 m³ voxel world
must never be simulated or stored as a dense global voxel array.

## Invariants

- Horizontal X and Y both wrap modulo 1,024,000 meters, including environmental fields.
- The world is a **mathematical torus**, not a spherical planet. Earth-like
  latitude bands are virtual, periodic climate coordinates. There are no literal
  geographic poles. This approximation must be documented in gameplay.
- Logical chunks are 32×32×32 voxels, with 8 vertical sections. Current GPU
  meshes are cached 32×32 columns that may contain several such sections.
- Z is vertical, world height is 256 meters, sea level is Z=110, player AABB
  is 0.56×0.56×1.3 m.
- Water is finite: each 1m³ voxel holds 0–1000 liters. Flow is transfer only.
  Precipitation takes mass out of the atmosphere reservoir; evaporation
  returns it. No water can spawn spontaneously in excavated tunnels.
- Energy uses W/m² and J/m²/K; time must be tracked independently of frame rate.
- All generated fields must be deterministic and periodic for a given seed.
- Persistence is necessary for true long-term conservation: world edits,
  water overrides, groundwater and atmosphere must be saved together.

## This pass — implemented

| Area | Changes | Verification |
| --- | --- | --- |
| Physics | 120 Hz fixed timestep, true AABB voxel overlap, stable ground probe, no repeated fall/snapping at rest, separate X/Y/Z collision, bounded substeps | Idle-height, jump, head collision, ledge fall, direction tests |
| Rendering | Per-column GPU mesh cache, update only newly entered columns and changed neighbors, instance offsets for torus-safe positioning | Cargo check and WGSL validation; Vulkan playtest outstanding |
| Water | Sparse finite-liter state layered over the initial finite ocean field, gravity and horizontal redistribution, liquid surface height, exposed sides | Total-volume, wrap, mined cavity, weather-reservoir tests |
| Regional geology | Deterministic toroidal plate seeds, relative plate velocity and convergence-derived boundary uplift proxy | Periodicity |
| Regional circulation | Virtual climate latitudes, tropical/polar easterlies, midlatitude westerlies, upwind mountain rain-shadow response | Belt and orography tests |
| Solar energy | Seasonal axial tilt, local solar time, insolation, albedo and material thermal inertia; initial sun-driven sky/voxel illumination | Noon/night, periodicity, thermal inertia |
| Source control | Feature branch and CI tests | cargo check + cargo test |

## Next work, dependency order

### 1. Collision performance and runtime telemetry
- Instrument fixed-step time, mesh generation time, water update time, GPU frame time.
- Perform a Windows Vulkan smoke test and check camera bob while idle and on slopes.
- Keep player coordinates in chunk-relative high precision to avoid f32 quantization
  at coordinates near X=512,000 / Y=512,000.
- Stop accidental auto-repeat jumps when Space is held if desired.
- Benchmark worst-case chunk-boundary crossings, swimming and cave mining.

### 2. Durable world state and chunk streaming
- Implement actual 32³ section storage with sparse edits and a versioned save format.
- Persist water quantities, atmospheric and future groundwater stores together.
- Move generation and meshing onto bounded background job queues. Rendering never
  blocks on an expensive chunk generation event.
- Define unloaded-chunk water flux contracts that preserve mass; never refill an
  unloaded region implicitly after its water has been withdrawn.
- Add regression tests for save/load, world seams, and cross-boundary volume flow.

### 3. Geological-time terrain generation
- Replace regional boundary proxy with explicit plate trajectories and crust types.
- Run collision/uplift/rift/subduction on a coarse grid using geological timesteps.
- Add erosion, sediment deposition, rock layers and soil formation as staged passes.
- Bake a deterministic height+substrate atlas to regional tiles for chunk queries.
- Maintain topographic and sediment mass budgets at appropriate approximations.
- Couple regional tectonic uplift into generation **only after** the model passes
  coherence tests for mountain chains and coastlines.

### 4. Planetary-scale climate (on a virtual torus)
- Define fictional rotational period, axial tilt, solar constant and atmospheric mix.
- Simulate coarse atmosphere cells: surface heat, humidity, pressure and velocity.
- Implement advection, orographic uplift, precipitation, evaporation and snow.
- Test windward precipitation, leeward dryness, seasonal bands, conservation
  of atmospheric + surface water, and periodic boundary continuity.
- The present rain-shadow calculation is only a deterministic regional proxy,
  not moisture advection or a complete weather prediction model.

### 5. Hydrology, water table and ecosystems
- Evolve active liquid toward a conservative flux solver with hydrostatic
  equalization, multiple levels and stable flow under large volumes.
- Include soil pore water, infiltration, groundwater conductance, streams,
  aquifers, springs, snowpack and freeze/thaw phase changes.
- Atmospheric reservoir must carry water from evaporation through advection
  before precipitation; implement regional mass ledgers.
- Wetlands, deserts, grasslands and forests arise from water+heat+soil histories,
  not noise-painted biome labels.
- Add lighting, fog, translucent water ordering and underwater shading.

## Acceptance tests for the next milestone

1. Standing for 10,000 fixed physics steps causes zero Z movement.
2. Walking horizontally on a flat surface causes zero Z movement.
3. Player can jump onto 1 m obstacles but cannot walk through/climb them without jumping.
4. Chunks on both torus seams agree on block geometry and water level.
5. Transferring water between cells conserves integer liters exactly.
6. Underground cavities start dry and fill only from connected liquid.
7. Evaporation+precipitation conserves atmosphere + accessible surface water.
8. Re-entering a region after save/reload does not reset depleted finite water.
9. Regional climate fields are seamless on both map edges.
10. Region-to-region wind/rain patterns produce testable rain shadows.

## Known limitations of this pass

- No real numerical tectonic evolution, atmosphere advection, runoff catchment
  routing, soil infiltration, groundwater, weather system or persistent saves yet.
- The solar clock illuminates the rendered world and updates a representative
  local thermal cell, **not** a fully coupled planetary heat budget.
- Water transfers are a simple bounded active-cell automaton, not Navier–Stokes
  fluid dynamics or a full hydrostatic pressure solver.
- The initial ocean is defined implicitly on unedited cells; while finite across
  the finite torus, the ledger requires persistence to remain consistent on disk.
- Cached chunks still mesh synchronously on first entry or after local edits;
  streaming queues are the next performance milestone.
- Partial-water side geometry and alpha blending are intentionally simple.
- GPU behavior has not been exercised in CI because runners lack a configured
  interactive Vulkan display.
