# Voxel Survival — Vulkan prototype

A minimal first-person voxel survival world using **Rust**, **WGPU 0.20**, **Vulkan only**, and **winit**. Art is procedurally generated at startup: original 16×16 dirt, grass-top, grass-side, stone, and water pixel tiles, inspired by earthy survival-game palettes (not copied from Vintage Story).

## Run

Install the stable Rust toolchain and up-to-date Vulkan-capable GPU drivers, then:

```sh
cargo run --release
```

Debug builds also work with `cargo run`, but initial terrain meshing is significantly slower.

## Controls

| Input | Action |
| --- | --- |
| Click inside window | Capture mouse |
| Mouse | Look |
| W / A / S / D | Move |
| Space | Jump |
| Left click | Break targeted block |
| Right click | Place selected block |
| 1 / 2 / 3 | Select dirt / grass / stone |
| Escape | Release mouse |

## On-screen HUD

- **Top-right:** averaged frames per second (FPS), mean milliseconds per frame,
  and XYZ positions in **meters relative to the map center**, not the spawn
  point. The center of the 1,024 km torus reads X=0 and Y=0; Z remains height
  above the voxel world's vertical origin.
- **Top-center:** scrolling direction compass (N/NE/E/SE/S/SW/W/NW) with
  heading in degrees, and a *virtual* latitude/longitude readout directly
  underneath. Longitude uses the centered X axis mapped to [-180°,180°).
  Latitude uses the existing toroidal climate bands; it is periodic and
  **not geographic latitude on a spherical planet**.
- Readouts refresh approximately ten times a second to reduce texture uploads; frame time
  is the average game-frame duration, not GPU-only render duration.

The first click captures the mouse; subsequent clicks edit blocks. Maximum reach is 6 meters. Placement into a water-filled voxel is currently disabled rather than silently deleting finite water.

## World rules

- **Horizontal torus:** X and Y wrap, including procedural noise, block edits, collision, ray picking, and mesh coordinates.
- **World extent:** 1,024,000 × 1,024,000 meters (**1024 km × 1024 km**).
- **Voxel:** exactly 1 meter.
- **Logical chunk:** 32 × 32 × 32 voxels; 32,000 × 32,000 horizontal chunk coordinates, eight vertical sections (Z=0..255). The prototype caches nearby 32×32 chunk-column meshes individually on the GPU (logical 32³ storage is a later milestone).
- **Vertical:** Z=0..255 (256 blocks); **sea level Z=110**, with alpha-blended finite-volume surface and side tiles.
- **Player:** collision body 1.3 meters high, 0.56 meters wide; first-person camera 1.18 meters above feet.
- **Terrain:** seamless multi-scale periodic 4D noise; dry-land spawn search; stone, dirt, grass; finite initialized ocean water. Regional plate-boundary, wind-belt, rain-shadow and solar-energy models are available but geological and climate evolution are not yet coupled into terrain generation.
- **Streaming:** seven-by-seven nearby chunk-column window; existing GPU meshes are retained during movement and only newly entered or locally modified columns are remeshed. Edits and water overrides remain in memory for the session.

## Simulation progress

- Player collision: fixed 120 Hz simulation, 0.56×0.56×1.3 m AABB,
  ground-contact probe, no idle bobbing, 1m jump traversal.
- Finite water: 0–1,000 L per voxel, localized gravity-first and horizontal
  equalizing transfers, torus wrapping, sparse overrides to a finite initial
  ocean. Mining below sea level creates **dry space** until water flows in.
- Atmosphere-water mass bookkeeping: precipitation withdraws from a finite
  atmosphere reservoir, evaporation returns water to it. These are primitive
  transfers, **not yet an automatically running weather simulation**.
- Regional geophysics: deterministic toroidal plate-boundary uplift proxy,
  virtual latitude, prevailing east/west wind belts, simplified mountain
  rain-shadow function, solar-angle/seasonal insolation, albedo/heat-capacity.
- The sun currently drives a 20-minute day/night sky and block-light cycle
  and a representative local thermal cell, not a 3D atmospheric model.

See [world-systems implementation plan](docs/WORLD_SYSTEMS_PLAN.md)
for dependencies, physical invariants, the milestone roadmap and acceptance tests.

## Current limitations

This is a playable **technical prototype**, not a finished survival game.
There are no persistent saves, caves, inventory, groundwater, actual moving
tectonic plates, atmospheric moisture advection, realistic pressure-driven
water flow, or full climate/weather yet. Initial terrain is still procedural
noise; the tectonic uplift model is a tested regional primitive not yet used
to build the height map. Water visuals are basic and no underwater shading
exists. First-time chunk meshing and edits are still synchronous, although
already cached chunks are reused across boundary crossings. Stable Rust
checks and unit tests run in CI; a real Windows Vulkan playtest is required.

Run `cargo test` for world-wrap, terrain, collision/grounding, finite
water conservation, regional climate, solar energy and WGSL shader tests.
