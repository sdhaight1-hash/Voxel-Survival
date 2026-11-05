# Island Hydrology Lab (experimental branch)

This branch intentionally changes `WORLD_SIZE` from 1,024,000 meters to **1,024 meters**, retaining toroidal X/Y wrapping, 1m voxels, 32m chunks, vertical height 256m, sea at Z=110, and the existing WGPU/Vulkan gameplay controls.

**This is a contained terrain/hydrology testbed, not a replacement for the production world. Do not merge into main.**

## Test map

Coordinates below are shown by the in-game HUD **relative to the map center**, which is (0,0). X increases east; Y increases north. The map edge is at +/-512m.

| Region | Approximate center (X,Y), meters | Initial water elevation | Expected behavior |
| --- | --- | --- | --- |
| Dry central island / player spawn | (0,0) | None | Place and mine solid voxels |
| Enclosed freshwater lake | (-115,+125) | Z=126 | Deep finite reservoir surrounded by hills |
| River headwaters | (-40,+95) | About Z=123 | Narrow sloping channel connects lake toward sea |
| River mouth | (+350,-65) | Z=110 | Meets ocean along southeast coast |
| Swamp / wetlands | (+115,-125) | Z=115 | Shallow ponds separated by grassy hummocks |
| Ocean | Beyond ~400m radial distance | Z=110 | Saltwater-like *geometry* only; no salinity mechanics |

The island interior has been flattened into a broad plateau around Z=128 with only gentle 1–3m rolling variations. Lake shore, riverbanks and swamp margins are widened for easier traversal; the lake and wetlands themselves remain intentionally lower and wet. The surrounding ocean remains below Z=110.\n\nThe landscape is deterministic, not randomly generated every launch. The boundaries connect seamlessly. There are no biome assets, vegetation, salinity, currents or new hydrodynamic algorithms in this branch.

### Recommended test route

Start at dry central spawn on the gently rolling inland plateau. The terrain is intentionally easy to walk; water margins still have shallow descents. Use the compass/HUD to travel **northwest** to the freshwater lake (X roughly -115, Y +125). Dig into its bank, observe water entering a hole, and build/remove submerged blocks. Follow the meandering river east/southeast toward the ocean. Visit the swamp to test shallow puddles and partially filled cells. Travel beyond 400m from the map center to reach the surrounding ocean.

The world is intentionally small enough to walk between areas without teleport commands. The 7x7 chunk-column render window remains unchanged, so distant terrain only appears when approached.

## Hydrology research checklist

This branch supplies **repeatable initial conditions**, not a claim that the existing cellular water solver self-levels correctly.

1. **Lake:** perturb one bank; compare measured water heights across the same connected still body. Desired final state: same absolute water surface elevation.
2. **River:** test downhill movement and conservation without requiring a level surface.
3. **Swamp:** test water around dry hummocks and restricted channels.
4. **Ocean:** dig/replace blocks across chunk and torus boundaries; ensure zero lost or created liters.
5. **Separated reservoirs:** build and breach a dam to test connectivity changes and local water redistribution.
6. **U-shaped passage:** verify eventual equalization including upward pressure flow (currently *not supported*).
7. **Rendering:** monitor partial-volume side faces, transparency, and visually missing water under solid roofs.
8. **Performance:** record FPS/frame ms near the ocean and during mass movement; aim to update only active regions.

## Technical invariants

- Initial lake, river, swamp and ocean water is represented by **terrain column height plus a column-specific initial surface height**. Only above-bed cells are initially wet; finite sparse overrides record disturbances.
- The initial water height is never used as an automatic refill after a cell has been modified.
- Water still transfers in integer liters (1m³ = 1,000L).
- Water bodies do **not** yet have a shared reservoir ID, global equilibrium height, conservative pressure solver or catchment-wide volume–elevation curve.
- River elevation varies intentionally along its slope. Lake surface is initially flat.
- This 1km lab torus is not geographically realistic. The existing virtual latitude/wind/tectonic scaling should **not** be used to validate planetary climate on this tiny map.
- Edited terrain/water remains session-only; world saves are not yet implemented.

## Launch

From the repository root:

```powershell
git fetch origin
git switch experiment/island-hydrology-lab
cargo run --release
```

To return to the production world:

```powershell
git switch main
```

Avoid mixing test-world saves with production-world saves once persistence is introduced.
