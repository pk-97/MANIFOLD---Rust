# MANIFOLD docs index (hand-curated)

## Core architecture maps
- `MANIFOLD_GPU_ARCHITECTURE.md` — native Metal backend, uniforms, textures
- `CORE_ENGINE_MAP.md` — playback, control, timeline, MIDI/OSC
- `FREEZE_COMPILER_MAP.md` — freeze/fusion compiler + graph runtime
- `CORE_ENGINE_MAP.md` companion; see `DECOMPOSING_GENERATORS.md`
- `NODE_CATALOG.md` — node inventory (regenerate, don't hand-edit)
- `DEVELOPMENT_REFERENCE.md` — dev quick reference
- `DESIGN_DOC_STANDARD.md` — design doc contract
- `HEADLESS_UI_HARNESS.md` — headless UI harness usage
- `GLB_IMPORT_OPTIMIZATION_GUIDE.md` — photoscan import optimization

## Active design specs
- Water: `GPU_FLIP_END_GAME_DESIGN.md`, `GPU_FLIP_PRESSURE_SOLVE.md`, `WATER_CRATES_DESIGN.md`
- Rendering: `RENDERER_CRATE_SPLIT_DESIGN.md`, `RT_STAGE4_MOTION_DENOISE_DESIGN.md`
- Audio: `AUDIO_ENGINE_DIRECTION.md`, `AUDIO_INFRASTRUCTURE.md`
- UI: `SCENE_SURFACES_DESIGN.md`, `UI_RESPONSIVENESS_UNDER_LOAD_DESIGN.md`
- Cross-cutting: `ADDING_PRIMITIVES.md`, `ADDING_EFFECTS_AND_GENERATORS.md`

## Notes
- Shipped one-off wave/audit docs are deleted; git history is the archive.
- Add new entries here when a living reference doc lands.
