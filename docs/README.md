# MANIFOLD docs

## Maps of the current code
- `CORE_ENGINE_MAP.md`: playback, transport, timeline, MIDI and OSC
- `FREEZE_COMPILER_MAP.md`: freeze and fusion compiler, graph runtime
- `MANIFOLD_GPU_ARCHITECTURE.md`: native Metal backend, uniforms, textures
- `MEDIA_EXPORT_MAP.md`: decode, encode and export
- `PROJECT_IO_MAP.md`: project files, loading and saving
- `NODE_GRAPH_SYSTEM.md`: how the node graph fits together
- `NODE_CATALOG.md`: how nodes are grouped and used; `scripts/dev.py node-catalog` prints every registered node
- `AUDIO_INFRASTRUCTURE.md`: audio input, analysis and routing
- `VSYNC_AND_FRAME_PACING.md`: display links and frame pacing
- `EFFECT_CHAIN_LIFECYCLE.md`: effect chain state across rebuilds
- `SCENE_RENDER_MODE_DESIGN.md`: 3D render mode lifecycle
- `SCENE_PANEL_VS_INSPECTOR_CARDS.md`: what the scene panel and inspector each own
- `DEVELOPMENT_REFERENCE.md`: crates and common traps

## Guides
- `ADDING_PRIMITIVES.md`, `ADDING_EFFECTS_AND_GENERATORS.md`, `DECOMPOSING_GENERATORS.md`
- `AUTHORING_GRAPHS.md`, `GROUPING_GRAPHS.md`, `CARD_AUTHORING.md`
- `HEADLESS_UI_HARNESS.md`: headless UI snapshots and flows
- `GLB_IMPORT_OPTIMIZATION_GUIDE.md`, `GLB_CONFORMANCE_STATUS.md`
- `AUDIO_EVAL_HARNESS_GUIDE.md`

## Work in progress
- Water: `GPU_FLIP_END_GAME_DESIGN.md`, `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md`, `GPU_FLUID_SURFACE_DESIGN.md`, `LIVE_SIM_CLOCK_DESIGN.md`
- 3D scene: `SCENE_DOCUMENT_DESIGN.md`, `REALTIME_3D_DESIGN.md`, `RAYTRACING_DESIGN.md`
- Audio detectors: `AUDIO_EVENT_CLASSIFIER_DESIGN.md`, `DETECTOR_LABEL_REVIEW_DESIGN.md`, `ROLL_DETECTOR_DESIGN.md`
- Transport: `FRAME_EXACT_TRANSPORT_DESIGN.md`
- GPU backend: `VULKAN_BACKEND_DESIGN.md`

## Ideas and business
- `IDEAS.md`: features worth building that nobody is working on
- `BUSINESS_PLAN.md`, `CAPABILITY_ROADMAP.md`, `COMPETITIVE_STEAL_PASS.md`, `AUDIO_ENGINE_DIRECTION.md`

Delete a design doc when its work ships. Add a map or guide here when one lands.
