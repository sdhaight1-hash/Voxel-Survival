# Game implementation and acceptance ledger

This is a staged game implementation, not a claim that the whole design is done.
The user's current authorization includes choosing previously undecided gameplay
mechanics. Confirmed world requirements take precedence over older proposals.

## Current design

- Rust/WGPU/Vulkan, first-person, 1 m cube voxels, 32 m logical chunks.
- Production world: 1,024 km periodic X/Y, 256 m vertical, sea level Z=110.
- Solid terrain and finite water retain separate state; no infinite refill sources.
- The 1 km island hydrology lab and draft PR #12 remain separate. Its unresolved
  numerical and visual gates must pass before any production port.
- World foundation: deterministic generation, persistent edits and environmental
  state, bounded streaming, regional climate, geological terrain, living ecosystems.
- Survival progression: gathering, tools, shelter, cooking, cultivation, fired clay,
  metallurgy, manufacturing. Exact recipes and balances are provisional gameplay choices.
- Rendering target: restrained earth colors, colored shadows, nonwhite highlights,
  selective saturation, foliage motion, atmosphere, readable inspection and map UI.
- Performance target hardware: user's RTX 2060-class GPU, i5-9000F, 16 GB RAM, 1080p.
  Do not infer performance from CI or software rendering.

## Implemented on work/survival-gameplay

- Finite counted inventory; mining adds material, placement consumes it only after
  collision and finite-water displacement succeed. Full inventory rejects harvesting.
- Hold-to-mine with material/tool time, stamina cost and tool wear. Bedrock protected.
- Wood/leaves and copper-bearing rock as deterministic provisional resource patches.
- Eight recipes: planks, stone pick/axe, campfire, cooked berries, copper ingot/pick,
  wood fiber. Cooking/smelting consume fuel and require a nearby campfire.
- Hunger/thirst drain, food, finite-water drinking, regeneration, starvation,
  fall damage, death and explicit respawn. Respawn loses carried items but keeps world.
- Inspect-target readout, inventory, recipe ingredients, durability, survival HUD,
  contextual error messages, keyboard controls.
- Versioned world snapshots, automatic load, manual/periodic/exit saves, exclusive
  world lock, validated snapshots and atomic file replacement. Exact water overrides,
  including zero-volume depletion, active cells and atmosphere are restored.
- Player position/view/vertical velocity, inventory, needs, clock, local temperature,
  and physics/fluid timer remainders are persisted.

### Deliberate first-loop simplifications

Trees are simple solid voxel canopies; leaf blocks yield berries and fiber.
Copper patches are not host-rock geology. Campfire smelting is a temporary game
abstraction, not a physically accurate furnace model. Drinking immediately accounts
for bodily water return in the atmospheric reservoir, without a body-fluid model.
No claim of hydrology improvement is made on this branch. The production solver is
still the earlier cell-based implementation. The independent island experiment has
not been merged or copied into this branch.

## Remaining implementation, in dependency order

| Checkpoint | Concrete acceptance criteria | Status |
| --- | --- | --- |
| Playable loop and saves | Gather, craft, place, eat/drink, die/respawn; reload restores inventory, edits, time and depleted water; Windows visual/input test | Automated checks implemented; visual playtest pending |
| Streaming and storage | Bounded worker queues; no main-thread chunk generation; region/section saves; canceled stale jobs; edit persistence across seams; measured frame stalls | Not implemented |
| Geology/world creation | Coarse tectonic evolution and crust; erosion/sediment; saved height/substrate atlas; deterministic periodic fields; host-rock ores/caves | Only older regional proxy exists |
| Hydrology | Issue #11 and PR #12 acceptance gates, shoreline return, connected head equilibrium, exact mass, cave connections, soil/groundwater, finite atmosphere; separate production port | Island WIP only |
| Weather and ecology | Regional temperature/moisture/wind; seasons and snow; finite exchanges; vegetation species, growth and environmental suitability | Not implemented as coupled simulation |
| Farming and food | Soil conditions, planting, time-based growth, harvest, storage/decay, cooking stations; save/load at every growth stage | Not implemented |
| Production progression | Knapping, clay/firing, furnace/charcoal, alloying, smithing, manufacturing; station/tool/fuel/material/time requirements | Basic recipes only |
| Fauna, NPCs and combat | Spawn budget, perception, navigation, needs, attacks and damage, drops, persistence; no NPC simulation for the whole planet | Not implemented; detailed design pending |
| Presentation | Map/minimap, inventory interaction, settings, sound, foliage motion, art pipeline and underwater presentation | Basic HUD only |
| Performance and distribution | Measured 1080p GPU/frame/streaming costs; Windows executable; save migrations; crash-recovery tests; complete gameplay playtest | CI and test executable build only |

## First Windows visual acceptance gate

1. Start a new world: visible terrain, trees, top compass, bottom inventory and crosshair.
2. Hold left mouse on wood, leaves and stone. Inventory increases once per mined block.
3. Craft a stone pick and axe; verify faster gathering and decreasing durability.
4. Craft/place a campfire; cook berries. Missing materials or distant fire reject crafting.
5. Build a small shelter. Right click consumes one block; self-intersecting placement
   and placement without inventory do not consume anything.
6. F5, close and relaunch: exact position, inventory, terrain edits and time return.
7. Record a short 1080p video with HUD FPS while walking between chunks and mining.
   Report visible stalls, unreadable UI, collision issues or save failures.

Passing this gate validates the first loop only. It does not validate the separate
island hydrology work or turn the remaining ledger into completed features.
