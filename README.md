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

The first click captures the mouse; subsequent clicks edit blocks. Maximum reach is 6 meters.

## World rules

- **Horizontal torus:** X and Y wrap, including procedural noise, block edits, collision, ray picking, and mesh coordinates.
- **World extent:** 1,024,000 × 1,024,000 meters (**1024 km × 1024 km**).
- **Voxel:** exactly 1 meter.
- **Logical chunk:** 32 × 32 × 32 voxels; 32,000 × 32,000 horizontal chunk coordinates, eight vertical sections (Z=0..255). The first prototype meshes nearby 32×32 columns in one combined GPU buffer.
- **Vertical:** Z=0..255 (256 blocks); **sea level Z=110**, with simple opaque water tiles.
- **Player:** collision body 1.3 meters high, 0.56 meters wide; first-person camera 1.18 meters above feet.
- **Terrain:** seamless periodic 4D noise; stone core, dirt subsurface, green grass on land, water below sea level.
- **Streaming:** only nearby terrain within a seven-by-seven chunk-column window is generated and meshed. Edits are kept in memory for the session.

## Current limitations

This is a first playable technical slice, **not a finished Vintage Story clone**. There is no save/load, inventory, crosshair/HUD, sky lighting, transparent water, caves, crafting, entities, or network play yet. Texture designs are original procedural art. Mesh rebuilding is synchronous, including after block edits; long pauses are possible on slower CPUs. Edits persist only until exit. Larger-scale world streaming should use a persistent chunk cache, background generation, greedy meshing, and region save files.

Run `cargo test` to verify wrapping, periodic height generation and edit coordinates.
