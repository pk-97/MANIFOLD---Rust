# Node Catalog

**Status:** Settled spec. Last sweep 2026-05-19. The May 14 rename pass is closed (§7); the May 18-19 additions added the texture→scalar bridge family and the control-rate primitive family, which now live as their own subsections under atoms.

## 0. Design invariants

These constraints make "split an Effect into atoms later" a non-event:

1. **Type IDs are flat.** `node.<name>` — no category prefix. The category is metadata on the node, not part of its identity. Moving Bloom from monolithic shader to atom-composite preset keeps the same `node.bloom` ID, so old saves load unchanged.
2. **Category is presentation, not structure.** Atoms and Effects share the same `Primitive` trait. The split is purely how the palette groups them for users + AI agents.
3. **Effects can be either monolithic OR composite presets.** Today's shipping presets are JSON files under `assets/effect-presets/`; future user-saved composites land at `graphs/custom_composites/<id>.json` inside the project ZIP. Both look identical in the palette.
4. **Most atoms are pure.** Stateful atoms today: `Feedback` (previous frame texture), `Smoothing` (previous scalar value), `BlobTracking` (background worker), `Watercolor` (pigment ping-pong), `DepthOfField` depth mode (MiDaS worker). State is keyed by `(owner_key, node_id)` in the `StateStore`.
5. **Port-shadows-param.** When a primitive declares a scalar input port with the same name as a `ParamDef`, the wire wins when present; the param is the fallback. Standard pattern for control-rate modulation on `gain`, `wet_dry`, `feedback.amount`, `affine_transform.{rotation,translate_x,translate_y}`, `smoothing.time_constant`, `chromatic_aberration.amount`. The graph editor disables the expose checkbox + value cell on wire-driven rows so users can't double-bind the same param.

## 1. Atoms — generic composable building blocks

Small primitives users (and AI agents) chain to build custom looks. Sub-grouped by what they operate on:

### 1.1 Image atoms

| Display Name | Type ID | Inputs → Outputs | Params (key ones) | Purpose |
|---|---|---|---|---|
| **Mix** | `node.mix` | (a, b) → out | `amount`, `mode: Lerp/Screen/Add/Max/Multiply/Difference/Overlay` | Blend two images with one of 7 modes |
| **Masked Mix** | `node.masked_mix` | (a, b, mask) → out | `amount` | Per-pixel weighted blend; `mask.r * amount` drives the lerp |
| **Wet/Dry** | `node.wet_dry` | (dry, wet) → out | `amount [0,1]` (port-shadow) | Crossfade a processed signal against the original |
| **Threshold** | `node.threshold` | (in) → out | `level`, `softness`, `gain`, `mode: Hard/SoftKnee` | Keep pixels above a brightness cutoff |
| **Gaussian Blur** | `node.gaussian_blur` | (in) → out | `kernel: 9/17/25`, `sigma`, `axis: H/V` | Separable Gaussian blur, one axis per pass |
| **Sample** | `node.sample` | (in, uv) → rgba | `filter: Bilinear/Nearest`, `address: Clamp/Repeat/Mirror` | Read a texture at a UV coordinate |
| **Transform** | `node.transform` | (in) → out | `translate: vec2`, `scale: vec2`, `rotate: f32`, `fold: None/X/Y/XY` | Translate, scale, rotate, optionally mirror-fold |
| **Affine Transform** | `node.affine_transform` | (in) → out | `translate_x`, `translate_y`, `rotation` (all port-shadow) | UV-space affine with three scalar input ports — the canonical port-shadow demo |
| **Brightness** | `node.brightness` | (in) → out | (none) | Extract per-pixel luminance to RGB |
| **Color Ramp** | `node.color_ramp` | (in, gradient) → out | `gradient: Texture1D` | Remap luminance through a color gradient |
| **Channel Mix** | `node.channel_mix` | (in) → out | `matrix: mat4` | 4×4 channel matrix multiplication |
| **Color LUT** | `node.color_lut` | (in, lut) → out | `lut: Texture1D`, `range [0,2]` | Color-correction look-up table |
| **Chroma Key** | `node.chroma_key` | (in) → mask | `key_color: vec3`, `tolerance`, `softness` | Per-pixel colour-proximity mask (R channel = match strength) |
| **Gain** | `node.gain` | (in) → out | `gain` (port-shadow) | Scalar-driven RGB multiplier — pairs with control-rate sources |
| **Feedback** | `node.feedback` | (in) → out | `amount` (port-shadow), `zoom`, `rotation`, `mode: Screen/Add/Max` | Accumulate previous frames (stateful) |
| **Smoothing** (image-side) | — | — | — | (none today — Smoothing operates on scalars; see §1.3) |

### 1.2 Texture→Scalar bridges

A small family that closes the loop between image content and scalar modulation via a shared-mode `MTLBuffer` readback. One-frame latency. Use these to drive `Gain`, `Math`, `Feedback.amount`, or any other scalar input port.

| Display Name | Type ID | Inputs → Outputs | Params | Purpose |
|---|---|---|---|---|
| **Luminance (scalar)** | `node.luminance` | (in: Texture2D) → (out: Scalar(F32)) | (none) | Average Rec.709 luma of the whole image |
| **Peak** | `node.peak` | (in: Texture2D) → (out: Scalar(F32)) | (none) | Maximum luma across the image |
| **Color Sample** | `node.color_sample` | (in: Texture2D) → (out: Vec3, luma: Scalar(F32)) | `uv: vec2`, `radius_px: int` | Region-averaged RGB at a configurable UV plus its luma |

### 1.3 Control-rate primitives

Scalar-only primitives that don't touch textures. The graph runtime walks these the same way as image primitives, but their evaluation is essentially free (no GPU dispatch).

| Display Name | Type ID | Inputs → Outputs | Params | Purpose |
|---|---|---|---|---|
| **Value** | `node.value` | () → (out: Scalar(F32)) | `value` | Constant scalar source — exposed via card sliders as the standard parameter wire |
| **Math** | `node.math` | (a: Scalar, b: Scalar) → (out: Scalar) | `op: Add/Subtract/Multiply/Divide/Min/Max/Atan2` | Two-input scalar math |
| **LFO** | `node.lfo` | () → (out: Scalar) | `rate`, `phase`, `amount`, `waveform: Sine/Tri/Saw/Square/SH` | Low-frequency oscillator |
| **Beat Gate** | `node.beat_gate` | () → (out: Scalar) | `rate`, `amount` | Beat-synced 0/amount gate (drives Strobe Opacity) |
| **Smoothing** | `node.smoothing` | (in: Scalar) → (out: Scalar) | `time_constant` (port-shadow) | One-pole low-pass filter (stateful) |

### Atom renames from current code

| Was | Becomes | Notes |
|---|---|---|
| `primitive.luminance` | `node.brightness` | sounds less like a measurement |
| `primitive.color_matrix` | `node.channel_mix` | hides the matrix math from users |
| `primitive.gradient_map` | `node.color_ramp` | matches DAW / paint-program vocabulary |
| `primitive.separable_gaussian` | `node.gaussian_blur` | the "separable" detail is an implementation choice |
| `primitive.uv_transform` | `node.transform` | "UV" is GPU jargon; users just want a transform |
| `primitive.affine_transform` | (deleted) | redundant with `node.transform` |
| `primitive.blend` | (deleted) | redundant with `node.mix`; only the Bloom test referenced it |
| `primitive.wet_dry_mix` | `node.wet_dry` | tighter |
| `primitive.lut1d` | `node.color_lut` | "1D" is implementation noise |
| `primitive.sample` | `node.sample` | already fine |
| `primitive.mix` | `node.mix` | already fine |
| `primitive.threshold` | `node.threshold` | already fine |
| `primitive.feedback` | `node.feedback` | already fine |

`primitive.blur` and `primitive.mip_chain` are V1 stubs that aren't wired into any chain — they get deleted during the rename pass; `node.gaussian_blur` is the real blur atom and `MipChainDown/Up` are deferred until something needs them.

## 2. Effects (24) — named, recognizable visual looks

Each is one node in the palette. Implementation may be monolithic, a thin preset of one atom, or a future composite of several atoms. Type IDs stay stable across implementation swaps.

Implementation kinds:
- **shader** — one WGSL kernel, no decomposition planned
- **preset** — a thin wrap of one atom with curated defaults
- **composite** — multi-pass kernel today, will become an atom subgraph later
- **monolith** — custom pipeline (CPU envelope / native plugin / DNN); stays monolithic forever

| # | Display Name | Type ID | Impl | Purpose |
|---|---|---|---|---|
| 1 | **Auto Gain** | `node.auto_gain` | monolith | Per-clip auto-leveling driven by a CPU envelope follower |
| 2 | **Blob Track** | `node.blob_track` | monolith | Detect and track bright blobs; render overlays (native plugin) |
| 3 | **Bloom** | `node.bloom` | composite | Soft halo glow on bright pixels (mip pyramid + blur + composite) |
| 4 | **Chromatic Aberration** | `node.chromatic_aberration` | shader | RGB channel offset for prism / lens fringing |
| 5 | **Color Grade** | `node.color_grade` | shader | Gain, saturation, hue, contrast, colorize |
| 6 | **Depth of Field** | `node.depth_of_field` | monolith | Selective blur driven by geometric or DNN depth |
| 7 | **Dither** | `node.dither` | shader | Halftone, Bayer, lines, crosshatch, noise, diamond |
| 8 | **Edge Detect** | `node.edge_detect` | shader | Sobel / Laplacian / Frei-Chen edge extraction |
| 9 | **Edge Stretch** | `node.edge_stretch` | shader | Stretch image edges outward (centered band) |
| 10 | **Glitch** | `node.glitch` | shader | Scanlines, RGB shift, block displacement |
| 11 | **Halation** | `node.halation` | composite | Warm bleed around highlights (threshold + tint + blur) |
| 12 | **Highlight Boost** | `node.highlight_boost` | shader | Boost highlights without the soft halo of Bloom |
| 13 | **Infrared** | `node.infrared` | shader | False-color palette mapping (10 palette LUTs) |
| 14 | **Invert** | `node.invert` | shader | Invert RGB channels |
| 15 | **Kaleidoscope** | `node.kaleidoscope` | shader | Radial mirror folding into N segments |
| 16 | **Mirror** | `node.mirror` | preset | Horizontal / vertical mirror (one Transform atom) |
| 17 | **Quad Mirror** | `node.quad_mirror` | shader | Four-way mirror fold with fill-quadrant zoom + additive crossfade |
| 18 | **Soft Focus** | `node.soft_focus` | preset | Dreamy Gaussian-blurred look (one Gaussian Blur atom) |
| 19 | **Strobe** | `node.strobe` | shader | Beat-synced flicker / flash / gain pulse |
| 20 | **Stylized Feedback** | `node.stylized_feedback` | preset | Trailing echo / motion smear (one Feedback atom) |
| 21 | **Transform** | `node.transform_effect` | shader | Translate / scale / rotate with aspect-correct rotation and hard-OOB clipping (legacy semantics; the Transform atom does plain UV-space math) |
| 22 | **Voronoi Prism** | `node.voronoi_prism` | shader | Voronoi-cell shatter / pop on beat |
| 23 | **Watercolor** | `node.watercolor` | composite | Painterly bleed (flow map + displace + blur + edge + feedback) |
| 24 | **Wireframe Depth** | `node.wireframe_depth` | monolith | Wireframe overlay from DNN depth estimation (15 passes + 3 workers) |

### Effect renames from current code

| Was (display name) | Becomes | Was (type ID) | Becomes |
|---|---|---|---|
| Blob Tracking | **Blob Track** | `primitive.blob_tracking` | `node.blob_track` |
| HDR Boost | **Highlight Boost** | `primitive.highlight_boost` | `node.highlight_boost` (display name change only) |
| Invert Colors | **Invert** | `primitive.invert` | `node.invert` |
| (kaleido_fold) | **Kaleidoscope** | `primitive.kaleido_fold` | `node.kaleidoscope` |
| (clamp_stretch) | **Edge Stretch** | `primitive.clamp_stretch` | `node.edge_stretch` |
| (chromatic_offset) | **Chromatic Aberration** | `primitive.chromatic_offset` | `node.chromatic_aberration` |
| (dither_pattern) | **Dither** | `primitive.dither_pattern` | `node.dither` |
| Soft Focus (Graph) | **Soft Focus** | (effect-only) | `node.soft_focus` |

`Node Graph Test` is dropped from the palette (it's a test-only fixture).

### Why thin presets exist

Three effects (Mirror, Soft Focus, Stylized Feedback) are genuinely just an atom with curated defaults. They are **alias presets** — separate palette entries that instantiate the same atom with different default params. Implementation is one shader; palette is three entries.

| Preset effect | Atom | Curated defaults |
|---|---|---|
| Mirror | `node.transform` | `mode = FoldX` (matches legacy) |
| Soft Focus | `node.gaussian_blur` | curated sigma |
| Stylized Feedback | `node.feedback` | curated decay / transform / blend |

Why keep them as separate Effects rather than collapsing into their atom:

- **Discoverability.** A user looking for "mirror" should find it by that name, not have to know it's a `node.transform` with `fold = X`.
- **AI surface.** An agent generating a graph from a high-level intent ("add a mirror") benefits from a named entry.
- **Zero cost.** A preset is one registry entry pointing at existing shader code — no new shader, no new EffectNode impl.

**Transform and Quad Mirror are NOT presets.** Investigation 2026-05-14 found their legacy shaders have semantics the Transform atom doesn't reproduce: aspect-ratio-correct rotation, legacy-subtract-translate, hard-OOB rejection (Transform), and fill-quadrant zoom plus additive piecewise blend (Quad Mirror). Migrating to atom-presets would visually regress existing projects. They stay classified as monolithic shader effects — same category as Glitch, Strobe, Voronoi Prism.

## 3. Generators — partially decomposed

Generator decomposition is now in flight. Three generators ship as JSON-defined graphs built from curated primitives; the rest stay Rust-defined for now. See [`GENERATOR_DECOMPOSITION_PLAN.md`](GENERATOR_DECOMPOSITION_PLAN.md) for the strategic roadmap and [`DECOMPOSING_GENERATORS.md`](DECOMPOSING_GENERATORS.md) for the authoring guide.

| Generator | Status | Notes |
|---|---|---|
| Plasma | JSON-defined | 3-node graph: `system.generator_input → node.plasma_pattern_2d → system.final_output`. Eight pattern variants packed behind one curated primitive. |
| Lissajous | JSON-defined | 9-node graph using `generate_lissajous`, `frequency_ratio`, `lfo`, `mux_scalar`, `render_lines`. |
| WireframeZoo (display name "Wireframe") | JSON-defined | 9-node graph using `wireframe_shape`, three `math` nodes, `rotate_3d`, `project_3d`, `render_lines` (with the new `edges` input wired). |
| 20 others | Rust-defined | Sequenced via the decomposition plan. |

## 4. The 4 monolithic effects — won't decompose

Per design principle: "Monolithic remainders are first-class library members." These four stay as one node forever because their pipelines aren't blur/threshold/mix math:

- **Auto Gain** — CPU envelope follower with transient detection
- **Blob Track** — native plugin + One-Euro filter + font atlas
- **Wireframe Depth** — 15 passes + 3 DNN workers
- **Depth of Field** (DNN variant) — MiDaS-based selective blur

The remaining 20 effects either are decomposed already (Mirror, Soft Focus, Stylized Feedback — 3 presets), have their own monolithic shaders (the 14 single-shader effects including Transform and Quad Mirror, whose legacy semantics don't reduce to the atom), or will decompose when the missing atoms land (Bloom, Halation, Watercolor — composites).

## 5. What's deferred

- **Shader-shipped Effects → atom composites** (Bloom, Halation, Watercolor) — composites with their existing monolithic shaders; will decompose as the atom set fills out.
- **Buffer ports** (audio waveforms outside the Array<T> story) — deferred to V2.
- **3D volume primitives** (Sample3D, SliceVolume → Texture2D, per-voxel math) — needed by FluidSim3D and any future volumetric work.
- **MipChainDown / MipChainUp** — defer until Bloom decomposes from its current composite shader.
- **Atomic primitives** (Sobel3, DisplacementMap, VoronoiCells, PerlinFBM) — defer until a composite needs them.

Items that *did* ship since the original deferred list:
- **Array<T> port type** — particle / mesh / line buffers flow through wires. Producers declare capacity via `EffectNode::array_output_capacity`; backing buffers are shared MTLBuffer (CPU + GPU visible). See `crates/manifold-renderer/src/generators/mesh_common.rs` for the POD types (`MeshVertex`, `LinePoint`, `EdgePair`, `Vec4Vertex`, `InstanceTransform`).
- **3D infrastructure** — `node.rotate_3d`, `node.project_3d`, `node.render_lines` (with optional explicit-edges input as of the WireframeZoo decomposition), plus `node.generate_cube_mesh`, `node.generate_platonic_solid → node.wireframe_shape`, `node.generate_tesseract_vertices`, `node.generate_duocylinder_vertices`. Drives WireframeZoo (3D wireframes), upcoming Tesseract / Duocylinder (4D), and any future user-imported mesh.
- **Particle / Array primitives** — `node.seed_particles`, `node.seed_particles_from_texture`, `node.integrate_particles`, `node.integrate_particles_attractor`, `node.scatter_particles`, `node.scatter_particles_3d`, `node.resolve_accumulator`, `node.resolve_3d_accumulator`, `node.array_feedback`. Drives the planned StrangeAttractor + fluid-family decompositions.
- **Procedural source atoms** — `node.plasma_pattern_2d` (8-variant family), `node.generate_lissajous`, `node.frequency_ratio`, `node.checkerboard`. Single-purpose siblings of the larger family primitives.
- **BeatGate** shipped (`node.beat_gate`) — drives the decomposed Strobe Opacity composite.
- **Texture→Scalar bridges** shipped (`luminance`, `peak`, `color_sample`) — see §1.2.
- **Control-rate primitives** shipped (`value`, `math`, `lfo`, `beat_gate`, `smoothing`, `envelope_follower_ar`, `affine_scalar`, `mux_scalar`) — see §1.3.

## 6. Migration of existing projects

The current rename pass keeps the legacy `EffectTypeId` save-format strings (PascalCase: `"Bloom"`, `"Mirror"`, `"Transform"`, etc.) untouched. The new `node.*` type IDs are internal to the node-graph runtime. Old projects load without any migration shim.

If a future pass renames the legacy `EffectTypeId` strings or removes a legacy effect, the V1→V2 loader in `manifold-io` will need a HashMap mapping old → new. None of that is required today.

## 7. Done definition

The rename pass is shipped when:

- Atom type IDs match the catalog (renamed in `crates/manifold-renderer/src/node_graph/primitives/`). ✅ Phase A
- Effect type IDs match the catalog. ✅ Phase A (internal `node.*` strings; legacy `EffectTypeId` save keys unchanged).
- Display names on effects match the catalog. ✅ Phase B1.
- Rust struct/const names match the catalog. ✅ Phase B2.
- Preset effects (Mirror, Soft Focus, Stylized Feedback) are graph-backed via atom composites. ✅ pre-existing from the foundation pass.
- Transform and Quad Mirror remain monolithic shaders (legacy semantics not reducible to atom composites — see §2.3 note).
- `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` are green. ✅

All criteria met as of 2026-05-14.
