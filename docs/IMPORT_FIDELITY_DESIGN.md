# Import Fidelity — imported PBR assets read like their authoring-tool previews

**Status: SHIPPED · F-P1 + F-P3 SHIPPED 2026-07-15 (orchestrator session 1 of 3, landing report `docs/landings/2026-07-15-import-fidelity-p1p3.md`) · F-P2 + F-P4 SHIPPED 2026-07-15 (orchestrator session 2 of 3, landing report `docs/landings/2026-07-15-import-fidelity-p2p4.md`) · F-P5 SHIPPED 2026-07-15 (orchestrator session 3 of 3, landing report `docs/landings/2026-07-15-import-fidelity-p5.md`) · approved by Peter 2026-07-15 ("Approved") · authored 2026-07-15 · Fable 5 (his product calls are quoted in the intro, D7, and D8; glass/F-P5, pure-black base, and sun coherence added same day at his direction). Execution: 3 orchestrator sessions — (1) F-P1 ∥ F-P3 DONE, (2) F-P2 + F-P4 DONE, (3) F-P5 DONE — all phases shipped. · F-P6 (material-map mip pipeline) + F-P7 (softbox dome fill + rig defaults) SHIPPED 2026-07-15 (session 4, same-day fix after Peter's helmet/AMG renders exposed LOD-0 map aliasing and the metals-in-a-black-void failure; his fill/strip look pass was waived 2026-07-16 in the verification-debt burn-down — look issues from here are BUG_BACKLOG entries).**
**Prerequisites: none — MATERIAL M1–M6, REALTIME_3D P1–P3/P8/P9, SCENE_BUILD P1–P5 and the shipped glTF assembler are all in-tree. IMPORT_DESIGN P1-remaining (lights/cameras/report surface) is independent and this doc outranks it in build order (Peter, 2026-07-15: "really critical infra").**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase.**

Peter's directives (2026-07-15, comparing an imported Mercedes-AMG GT3 .glb against
its store-page preview): "this sounds like really critical infra that should be
upgraded so models can be imported correctly and accurately", and on the look: "I
prefer the black void look rather than the pure white studio, can we have the pure
black void AND proper lighting[?]". The governing insight: a well-authored .glb
carries its look in per-pixel maps (normal / metallic-roughness / emissive /
occlusion) and expects image-based lighting to read them; MANIFOLD currently keeps
only the base-colour map and lights it with a single flat envmap sample, so the
asset's look is discarded before the graph ever sees it. **On stage this is the
difference between "drag a store-quality asset in and it plays tonight" and a
relight job the performer can't win.** The black void and proper lighting do NOT
conflict: `render_scene` uses the envmap for lighting only, never as background
(verified — no sky/miss draw exists in `render_scene.rs`), so the fix is a
mostly-black environment with bright emitter strips: dramatic light streaks on dark
metal, void stays void.

Companions: `IMPORT_DESIGN.md` (owns the import funnels; its §8 tangent-space skip
is superseded here), `MATERIAL_SYSTEM_DESIGN.md` (M6-D5's revival trigger fired —
this doc is the "own designed slice" it called for), `REALTIME_3D_DESIGN.md` (owns
`render_scene`; this doc grows its per-object surface the way §10/P8 did),
`RENDER_SCENE_UNBOUNDED_LIGHTS_DESIGN.md` (precedent for a single-aspect
render_scene doc). The pending void-haze design (bounded haze volume, Peter's go
pending) is orthogonal and complementary — haze adds atmosphere; this doc makes the
subject itself read correctly. Neither touches BUG-118 (fog wash — Peter: "I don't
want bug-118 worked on").

---

## 1. Audit — what exists (verified 2026-07-15)

| Piece | Where | State |
|---|---|---|
| glTF material parse | `gltf_load.rs:405-418` (`GltfMaterialInfo`) | base_color factor+texture, metallic/roughness **scalars**, emissive **factor**, alphaMode/cutoff. NO normal / metallic-roughness / occlusion / emissive texture indices, no KHR extensions |
| gltf crate | `manifold-renderer/Cargo.toml:31` (`gltf = "1"`, v1.4.1, default features) | Extension features exist but are OFF: `KHR_lights_punctual`, `KHR_materials_emissive_strength`, `KHR_materials_specular`, `KHR_materials_transmission`, `KHR_materials_ior`. **No typed clearcoat support in 1.4.1** (needs the raw `extensions` feature + manual JSON, or a crate bump — checked in the registry source) |
| Per-object texture ports | `render_scene.rs:194,493` | `base_color_map_n` ONLY — "no normal_map/roughness_map/metallic_map inputs per object yet" (the file says it itself) |
| Texture decode + colour space | `gltf_texture_source.rs:197-202` | `color_space` param already selects `Rgba8UnormSrgb` vs linear — reusable as-is for the new map types |
| IBL in `fs_pbr` | `render_scene.wgsl:648-651` | ONE lod-0 equirect sample along `reflect(-V, N)`, dimmed by heuristic `ibl_strength = 1.0 - roughness*0.7`. No prefiltered mips, no diffuse irradiance, no split-sum BRDF LUT — rough metal gets a sharp reflection faded to grey |
| Shared BRDF helpers | `shaders/pbr_brdf.wgsl` | D_GGX / G_Smith / F_Schlick / equirect UV — correct and reusable; nothing IBL-specific beyond the UV mapping |
| Envmap bake | `bake_equirect_envmap.rs:47-…` (`node.bake_environment`) | Procedural gradient studio (horizon_strength / azimuth_variation / intensity). No discrete emitters; import wires it at **intensity 0** (`gltf_import.rs:374`) — the deliberate "model on black", which also means zero IBL |
| MeshVertex | `mesh_common.rs:34-43` | 48-byte position/normal/uv. **No tangents**; stride pinned by test + `MESH_VERTEX_SPECS` Channels signature — growing it is a workspace-wide ABI change |
| Normal-map contract (render_mesh) | MATERIAL §11.1 / `render_3d_mesh.rs:66` | Existing `normal_map` is **world-space** (procedural heightfield chains); glTF maps are tangent-space — M6-D5 deferred them with trigger "a hero import that visibly needs them". **The trigger has fired** |
| Exposure | `gltf_import.rs:396` wires `node.camera_lens` (`exposure_ev` port-shadowed) | Exposure exists; HDR→SDR happens at composite (MetallicGlass precedent). No new tone-map infra needed |
| Texture-set / HDRI drop | `IMPORT_DESIGN.md` D5 / P4 | Already designed there — HDRI file loading is NOT this doc's scope |
| Instancing / always-bind stub pattern | REALTIME_3D §10 D11, `render_scene.rs:874` | The precedent every new optional per-object port copies (unwired = dummy bind + flag 0 = byte-identical output) |

Classification: the loader fields and importer wiring are *one wire away from
existing* (the parse loop, texture-source atom, and port plumbing all exist);
split-sum IBL and the softbox bake mode are *genuinely new*; everything else is
*exists, extend*.

## 2. Decisions

- **D1 — Scope is `render_scene` (the import path), not `render_mesh`/`render_copies`.**
  The single-object renderers keep their flat IBL and world-space `normal_map`
  contract untouched (their consumers are procedural presets tuned against it —
  `feedback_shared_shader_topology`: fork, don't change boundary behaviour).
  *Consequences, stated honestly:* two PBR qualities coexist in the app until a
  MetallicGlass-class look-pass fires the migration trigger; the shared
  `pbr_brdf.wgsl` helpers grow the IBL functions so that migration is mechanical
  later. Rejected: upgrading all three renderers in one wave — triples the parity
  surface for zero import-path win.
- **D2 — IBL becomes split-sum, computed inside `render_scene`, invisible to the
  graph.** Three cached GPU resources, all derived from whatever `Texture2D` is
  wired to `envmap`:
  1. **Prefiltered specular chain** — the equirect map GGX-importance-convolved
     into a mip chain (base 512×256, `rgba16float`); `fs_pbr` samples
     `textureSampleLevel(prefiltered, uv, roughness * max_mip)`.
  2. **Diffuse irradiance map** — 32×16 cosine-convolved equirect, sampled with N;
     replaces nothing (there is no diffuse IBL today) and is multiplied by
     `kd * albedo` and the occlusion term.
  3. **BRDF LUT** — 128×128 `rg16float` split-sum scale/bias, computed once per
     device, keyed in the texture pool. Specular IBL becomes
     `prefiltered * (F0 * lut.x + lut.y)`, deleting the `ibl_strength` heuristic.
  Rebuild rule: the chain re-convolves when the wired envmap's `DataVersion`
  changes (house dirty-check pattern), pooled textures keyed `(node, size)` — no
  per-frame allocation. *Consequences, stated honestly:* an **animated** envmap
  (any Texture2D can be wired, including video) re-prefilters every frame — a
  fixed, small cost (convolving 512×256 + mips), visible in the perf HUD, not a
  correctness hazard. Rejected: a user-facing `node.prefilter_environment` atom
  emitting a new port type — every import graph would carry plumbing the renderer
  can do invisibly, and a new port type reopens shipped plumbing for zero
  authoring win.
- **D3 — Each object group grows four optional texture ports:** `normal_map_n`
  (tangent-space, glTF convention), `mr_map_n` (glTF metallic-roughness packing:
  G = roughness, B = metallic), `occlusion_map_n` (R channel), `emissive_map_n`
  (sRGB, multiplied by the material's emission factor). Port group becomes 9 wide;
  the importer's collapsed per-object node groups keep the editor legible (the
  existing base_color pattern at `gltf_import.rs:605-640` is the template). Each
  new port copies the P8 always-bind stub pattern: unwired binds a dummy, its flag
  stays 0, output byte-identical. The `texture_flags` vec4 is full → a second
  `texture_flags2` vec4 joins the uniform block (mind naga uniform sizing —
  `feedback_naga_uniform_size_rule`; the block grew 272→320 in P3, precedent for
  growing it again). New WGSL bindings for the three new textures (`mr` / `occlusion`
  / `emissive`) plus prefiltered/irradiance/LUT — ⚠ VERIFY-AT-IMPL: read the
  current binding table in `render_scene.rs` end-to-end before assigning indices
  (PCSS reserved 15/16-adjacent slots; Metal's 31-texture argument limit has
  ample headroom). Rejected: carrying texture refs on the Material wire
  (MATERIAL §7 "Path B") — Material is a CPU `Copy` struct on a CPU wire;
  imports are machine-assembled so the "UX wart" Path B exists for never
  materialised; reopening a shipped contract for it fails dont-cascade-redesign.
  Rejected: reusing the existing single-channel `roughness_map`/`metallic_map`
  binding contract with a channel-select mode flag — a mode flag on a shared
  resolve function is the hidden-fallback shape (`feedback_no_silent_fallbacks`);
  a dedicated `mr_map` binding with its own resolve function is executor-clear.
- **D4 — Tangent-space normal mapping via screen-space cotangent frame, NOT
  MeshVertex tangents.** `fs_*` computes the TBN per fragment from
  `dpdx/dpdy(world_pos)` and `dpdx/dpdy(uv)` (Mikkelsen's cotangent-frame
  derivation — the technique three.js/filament use when tangents are absent).
  `MeshVertex` stays 48 bytes: growing it is a workspace ABI change touching every
  mesh producer, the Channels signature, codegen, and the stride tests — priced
  and rejected for a per-fragment computation that costs a handful of ALU ops.
  glTF `normalTexture.scale` imports as a multiplier. *Consequences, stated
  honestly:* derivative-based TBN is slightly faceted across UV seams and mirrored
  UVs on low-poly meshes; on photoscan/production assets (dense, well-unwrapped)
  it is visually indistinguishable. Trigger to revisit: a hero asset whose normal
  detail visibly breaks → import-time tangent generation into a **separate
  optional buffer port**, never MeshVertex growth.
- **D5 — Loader parses the full material, importer wires it, everything unmapped
  is a report line (IMPORT D9 doctrine).** `GltfMaterialInfo` gains:
  `normal_texture + normal_scale`, `mr_texture`, `occlusion_texture +
  occlusion_strength`, `emissive_texture`, `emissive_strength`
  (KHR, feature-gated), and parse-for-report fields `transmission: bool`,
  `clearcoat: bool` (raw `extensions` JSON presence check — no typed support in
  gltf 1.4.1). Cargo features to enable: `KHR_materials_emissive_strength`,
  `KHR_materials_transmission` (report only), `extensions` (clearcoat presence),
  and `KHR_lights_punctual` (IMPORT P1 needs it; enabling here costs nothing).
  Importer maps each texture through its own `node.gltf_texture_source` with the
  correct colour space (D6) into the matching `*_map_n` port; ORM-packed files
  (occlusion index == mr index) wire the same source node into both ports.
- **D6 — Colour-space discipline:** base-colour and emissive maps decode as sRGB
  (`color_space = 0`, existing `Rgba8UnormSrgb` path); normal / metallic-roughness /
  occlusion maps decode linear. The importer sets this per map; a unit test pins
  the assignment. ⚠ VERIFY-AT-IMPL: `node.bake_environment`'s output format —
  confirm it is HDR float (read `bake_equirect_envmap.rs` allocation); the
  prefilter chain inherits it.
- **D7 — The default import look becomes "black-void studio": `node.bake_environment`
  gains a `mode` enum — `gradient` (default, byte-identical legacy behaviour) |
  `softbox` — and imports wire `softbox` at intensity 1.0 (today: gradient at 0.0).**
  Softbox = **exact-zero black base** (Peter, 2026-07-15: "I want it PURE black
  void so it looks good for hero shots on stage" — base texels are 0.0, not
  near-black; the only light in the environment is the strips, so nothing lifts
  the shadows) with N bright horizontal emitter strips (soft falloff at strip
  edges is permitted — falloff belongs to the strips, never the base); committed
  params: `mode`, `emitter_count` (default 3), `emitter_intensity`, `emitter_elevation`,
  `emitter_width` — strip math is executor-free within those params, gated by
  F-P3's numeric readback (luminance histogram + strip count), never a look.
  (The on-screen background was never at issue — the envmap is lighting-only and
  is not drawn; the visible void is the clear colour, pure black regardless.)
  **Sun coherence (added 2026-07-15, Peter: "Can we place these fake strips and
  lights in the same positions as the real scene lights so it looks coherent and
  'makes sense'?"):** `softbox` mode additionally paints ONE bright sun disc at
  the direction given by new params `sun_x/sun_y/sun_z` + `sun_disc_intensity` /
  `sun_disc_size` (all defaulted to 0 = no disc; direction params bind 1:1 —
  no conversion math in a binding). The importer binds the SAME card macros that
  drive the sun `node.light`'s direction into these params, so one gesture moves
  the sun's illumination, its shadows, AND its reflection together. Sun only:
  a sun is directional (infinitely far), which an envmap represents exactly;
  point lights are near-field (their reflections need parallax an envmap cannot
  express) and keep their correct specular-dot reflections — do NOT paint point
  lights into the envmap. *Consequences, stated honestly:* while the sun
  direction is being performed, the envmap re-bakes and (post F-P1) re-prefilters
  every frame it changes — the fixed cost F-P1's gate measures, paid only during
  the gesture. This
  is Peter's call, quoted: "I prefer the black void look rather than the pure white
  studio … the pure black void AND proper lighting". Background stays the clear
  colour (the envmap is lighting-only — audit table); chrome reflects light streaks,
  the void stays void. The Environment macro card keeps its 0–4 range and now
  defaults to 1.0. *Consequences, stated honestly:* existing presets are untouched
  (mode defaults to `gradient`, intensity semantics unchanged), but freshly imported
  cards look different from pre-design imports, and — as with BUG-149's fog scaling —
  **already-imported projects need a re-import to pick up the new defaults.**
- **D8 (added 2026-07-15, Peter: "I think it makes sense to add it") — Transparency
  v1 is a sorted per-object blend pass in `render_scene`, not order-independent
  transparency.** `AlphaMode` (MATERIAL M6-D2's enum, `material.rs`) gains a `Blend`
  variant — the §7 "new fields/variants, defaulted, no version-break" seam; all four
  material atoms expose it in their existing `alpha_mode` param. `render_scene`
  splits its object list: `Blend`-material objects skip the opaque pass and every
  shadow-caster pass (a window must not throw an opaque shadow), then draw in a
  second pass after all opaque objects, **sorted back-to-front by view-space depth
  of the transformed bounding-box centroid, depth test ON / depth write OFF**,
  classic straight-alpha over blending (`src_alpha / one_minus_src_alpha`; the
  scene target is straight-alpha per the P3 fog precedent and the
  alpha-standardisation contract — never premultiply in the shader). Lighting,
  IBL, and fog run identically in both passes — glass is mostly reflection, which
  is why this phase orders after the IBL upgrade. Importer mapping changes:
  glTF `BLEND` materials and `KHR_materials_transmission` materials become `Blend`
  (transmission: `alpha = base_color.a × (1 − transmission_factor)`), replacing
  the F-P4 Mask-plus-report-line stopgap and superseding MATERIAL M6-D3's import
  mapping (its revival trigger — "a hero asset that genuinely reads wrong as
  cutout" — fired on the AMG's windows). *Consequences, stated honestly:* two
  transparent surfaces inside ONE object can blend in the wrong order from some
  angles (per-object sorting can't see triangles), instanced transparent objects
  sort as one object, and there is no refraction or frosted blur — glass tints
  and reflects, it doesn't bend light. Perf: no new geometry work — the same
  draws split across two passes plus a CPU sort of a few dozen objects; blend
  fill costs only over glass pixels. Rejected: OIT (weighted-blended or
  per-pixel lists) — a real design of its own, not smuggled in; per-triangle
  sorting — CPU cost scales with mesh density for an artifact class stage
  content rarely hits.

## 3. What it buys on stage

- Drag the AMG GT3 in: chrome reads as chrome (streak reflections off the softbox
  strips), livery and panel detail come from the maps, headlights glow into bloom —
  in the black void, on the first beat, no relight session.
- `emitter_elevation`/`emitter_intensity` are performable: the studio lighting rig
  itself rides a macro. Sweep the emitters while the camera orbits and the
  reflections travel across the body.
- Windows are windows: glass tints and reflects the softbox streaks, you see the
  cockpit through it, and its opacity is a fader (solid → ghost mid-set).
- Every skipped feature (clearcoat paint, refraction) is a report line, so what
  the asset can't yet do is known at import time, not discovered on stage.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| Unwired new ports change nothing: a pre-design scene renders byte-identical | gpu-proof `render_scene_ibl` parity case + bundled 3D preset PNG diff (zero) — the P8 identity-parity pattern |
| IBL responds to roughness: rough ≠ mirror | gpu-proof numeric case (F-P1 gate) — reflection gradient width ratio, no eyeballing |
| BRDF LUT (envmap-independent) built once per device, never rebuilt | gpu-proof: dispatch-count assert, built exactly once (SHIPPED F-P1, `cddc618f`) |
| Prefiltered specular chain + diffuse irradiance re-convolve whenever the wired envmap is present, every frame — no stale-content skip | **Corrected 2026-07-15 at F-P1 landing: this row previously said "same params → cached"; that contradicted D2's own consequence prose ("an animated envmap re-prefilters every frame — a fixed, small cost, not a correctness hazard") and was unbuildable besides — no `DataVersion`/generation-counter signal exists on `EffectNodeContext` inputs, and `bake_equirect_envmap` mutates its output texture in place every frame regardless of param change, so a pointer/size-keyed skip would treat the D7 sun-sweep gesture's animated envmap as "unchanged" and go stale (a correctness regression on the design's own showcase gesture). F-P1 built the two resources to re-convolve unconditionally per D2's prose instead; full reasoning in the F-P1 landing report. A generation-counter signal for `EffectNodeContext` is real infrastructure, deferred — see §7 Deferred #6 below.** Enforcement: `prefilter_and_irradiance_cost_is_measured_and_reported` gpu-proof reports the fixed per-frame cost as a number (3.09ms/frame measured 2026-07-15, well under the 10ms re-tune trigger). |
| Colour space per map type never regresses | unit test on the importer's `color_space` assignments per map kind |
| No unmapped feature is silently dropped | importer unit test: over-featured fixture → report enumerates clearcoat etc. (transmission until F-P5 lands, then it maps instead) |
| `mode = gradient` is byte-identical legacy | gpu-proof: bake with explicit `gradient` vs build-of-record readback |
| Zero-`Blend` scenes never pay for the glass pass | gpu-proof (F-P5): byte-identical output + dispatch-count assert (no second pass) |
| Transparent objects cast no shadows, write no depth | gpu-proof (F-P5): glass pane between sun and ground → ground asserts lit; pipeline state pinned in test |

## 5. Phasing (Sonnet-executable, one session each)

**Verification protocol for orchestrated (Sonnet → Sonnet) execution.** Every
worker pass/fail criterion in this section is mechanical — numeric readbacks,
byte-parity diffs, dispatch-count asserts, `rg` hit counts, named green tests. No
worker or orchestrator gate in this doc requires looking at an image or judging a
look. **No PNG demo artifacts are produced anywhere in this wave** — Peter's
directive (2026-07-15): "I will use the live app, not look at PNGs." Verification
above test level is Peter in the live app (L4), reached through each landing's
≤2-minute click-script; that click-script replaces the standard's L2 demo for
every phase here. (The bundled-preset pixel-parity gates stay — those are machine
byte comparisons, not images anyone reviews.) gpu-proofs gate runs
are **serialized across the wave** — never run two phases' `--features gpu-proofs`
gates concurrently (device contention flakes them; the in-process `test_device`
lock only serializes within one process).

**Execution order:** F-P3 is independent of F-P1/F-P2 (it touches only
`bake_equirect_envmap.rs`) and may run in parallel with them in its own worktree.
F-P2 needs F-P1's bindings landed. F-P4 needs F-P1–F-P3. F-P5 needs F-P1 + F-P4.
Landings batch 2–3 phases per the repo protocol.

Forbidden, all phases: touching `render_mesh`/`render_copies` behaviour (D1) ·
growing `MeshVertex` (D4) · a user-facing prefilter atom or new port type (D2) ·
channel-select mode flags on shared resolve functions (D3) · touching fog/BUG-118 ·
`Arc<Mutex>` anywhere · synthesizing the uniform block or binding table from memory
instead of reading it (`feedback_synthesis_drift`).

- **F-P1 — SHIPPED 2026-07-15, `cddc618f`.** Split-sum IBL in `render_scene`. Prefiltered chain + irradiance map +
  BRDF LUT (D2), `fs_pbr` rewritten to consume them, `ibl_strength` heuristic
  deleted. Convolution sample counts are DEFAULTED, not open: 256
  importance-samples per prefiltered-mip texel, 512 per irradiance texel, 1024
  per LUT texel — change only if the F-P1 cost measurement exceeds 10ms for the
  512×256 chain, and record the change in the phase report (no other trigger).
  Read-back: this doc whole; `render_scene.rs` envmap plumbing + binding
  table end-to-end; `pbr_brdf.wgsl`; MANIFOLD_GPU_ARCHITECTURE uniform rules;
  the two-cache rule (`feedback_effect_chain_state_caches`). Gate (positive,
  gpu-proofs): roughness-response — reflection of a bright emitter across a
  roughness 0 → 1 sweep widens monotonically (gradient-width ratio ≥3×, PCSS-gate
  pattern); irradiance — uniform white env, zero lights → lit result ≈ albedo
  within tolerance (value-level); cache — re-convolve only on version change
  (dispatch-count assert); prefilter cost measured once and reported as a number
  (the D2 animated-envmap consequence gets a price, not an argument). Gate
  (negative): no-envmap presets byte-identical;
  `rg 'ibl_strength'` → zero hits; existing `render_scene_*` proofs green
  unmodified. Demo: none — numeric gates above; Peter's check is in-app via the
  landing click-script (mirror sphere vs rough sphere scene named in it). Test
  scope: focused + `--features gpu-proofs render_scene`; workspace sweep at landing.
- **F-P2 — SHIPPED 2026-07-15, `c778dbe3`.** Per-object map set + tangent-space normals. D3 ports + resolve
  functions + `texture_flags2`, D4 cotangent frame, emissive/occlusion terms in
  all lit entry points (emissive in `fs_unlit` too, matching M6-D1's albedo
  precedent). Read-back: D3/D4; `render_scene.rs` rebuild + `resolve_*` family;
  P8's stub pattern at `render_scene.rs:874`. Gate (positive, gpu-proofs): per
  map, a known texel produces the expected shading delta (value-level: normal map
  tilts N — lit value shifts by a computed amount; MR map's B channel drives F0;
  emissive adds after lighting; occlusion darkens the IBL term only). Gate
  (negative): unwired parity (byte-identical), port-rebuild tests (the
  `base_color_map_n` test family at `render_scene.rs:2098` extended), `rg` zero
  hits for `texture_flags2` reads outside the resolve functions. Demo: none —
  Peter's check is in-app (normal-mapped cube vs flat cube, named in the landing
  click-script). Performer gesture: emissive
  material's emission intensity on a fader → glow pulses through bloom.
- **F-P3 — SHIPPED 2026-07-15, `9e4b0b7f`+`c0df7921`.** Softbox bake mode. D7 params on `node.bake_environment`; `gradient`
  byte-identity; strip math free within the committed param names. Gate:
  gpu-proof — `gradient` mode byte-identical to build-of-record; `softbox`
  readback: every texel outside the strips and their falloff bands is EXACTLY
  0.0 (D7 pure-black base — assert max luminance over the non-strip region == 0,
  not merely small), emitter rows above 1.0 (HDR),
  emitter_count changes the strip count (counted, not eyeballed); sun disc — with
  `sun_x/y/z` set and `sun_disc_intensity` > 0, the brightest texel sits within a
  committed pixel radius of the direction's computed equirect coordinates
  (numeric position assert), and `sun_disc_intensity = 0` is byte-identical to
  no-disc. Demo: none —
  Peter's check is in-app (chrome sphere under softbox, named in the landing
  click-script). Test scope: focused.
- **F-P4 — SHIPPED 2026-07-15, `a96e8167`.** Loader + importer + defaults. D5 parse fields + Cargo features, D6
  colour spaces, importer wiring of all four map ports, report lines
  (clearcoat/transmission/BLEND-as-Mask — the transmission and BLEND lines are
  the stopgap F-P5 replaces), import defaults flip to
  `softbox @ 1.0` (D7), Environment card default 1.0, and the D7 sun-coherence
  bindings — the card's sun-direction macros bind to BOTH the sun `node.light`
  AND the envmap's `sun_x/y/z` (unit test asserts each sun macro carries both
  binding targets). Read-back: D5–D7;
  `gltf_load.rs` + `gltf_import.rs` end-to-end; IMPORT_DESIGN D9/§8. Gate
  (positive): unit tests — a synthetic summary with all texture kinds wires all
  ports with correct colour spaces; **held-out fixture** — Khronos DamagedHelmet
  (all five maps; CC-BY, add attribution line) imports with every map port wired
  (asserted by port name), renders headless without error, and the render is
  non-degenerate (mean luminance above 0.02 AND below 0.98 — catches both
  all-black and blown-out without judging the look); the AMG GT3 .glb
  (already local in `tests/fixtures/gltf/`, untracked — **stays untracked**:
  vecarz licensing unverified, never commit it) renders and is the Peter-facing
  look check (L4, his call, not any agent's). Gate (negative):
  report enumerates every unmapped feature of an over-featured fixture; existing
  assembler tests green; `check-presets` clean. Round-trip gate: save an imported
  project, reload, maps still bound (BUG-036 rule). Demo: the ≤2-minute
  click-script for Peter — import the AMG, confirm chrome + void + glow.

- **F-P5 — SHIPPED 2026-07-15, `61400029`.** Glass (sorted blend pass). D8 whole: `AlphaMode::Blend` variant +
  atom param arm; opaque/transparent object split; back-to-front centroid sort;
  blend pipelines (per MaterialKind, depth write OFF) alongside the existing
  opaque set; `Blend` objects skipped in every shadow-caster pass; importer flips
  `BLEND`/transmission materials from Mask-plus-report to `Blend` and drops those
  report lines. Entry state: F-P1 + F-P4 landed (glass reads via IBL; the importer
  flags transmission). Read-back: D8; both draw loops in `render_scene.rs`; the
  alpha-standardisation memory; MATERIAL M6-D2/D3. Gate (positive, gpu-proofs):
  see-through — a glass quad over a textured plane blends to the computed value
  (value-level); sort — two stacked glass panes show the far pane through the
  near one, and swapping their positions swaps the blend order; occlusion — glass
  fully behind an opaque object contributes nothing; shadow — a glass pane
  between sun and ground leaves the ground UNshadowed (assert lit). Gate
  (negative): a scene with zero `Blend` materials renders byte-identical to
  pre-F-P5 (no second pass runs — dispatch-count assert); existing
  `render_scene_*` proofs green unmodified; `rg -i 'oit|per_triangle_sort'` on
  touched files → zero hits. Round-trip: `Blend` alpha_mode survives
  save/reload with modulation live after reload. Demo: none — the AMG's windows
  are Peter's in-app check (L4). Performer gesture: a glass
  object's opacity (material alpha) on a fader — solid to ghost mid-set without
  the object popping wrongly through geometry. Test scope: focused +
  `--features gpu-proofs render_scene`; workspace sweep at landing (blend
  pipeline set + Material enum growth = infra). Forbidden: OIT or per-triangle
  sorting "while at it" (D8 rejected them) · depth write in the blend pass ·
  premultiplying in the shader · touching the Mask/cutout path.

- **F-P6 — SHIPPED 2026-07-15 (same-day fidelity fix).** Material-map mip
  pipeline. Diagnosis (probe A/B renders, session of 2026-07-15): every map
  was uploaded flat (`mip_levels: 1`) and sampled at forced LOD 0
  (`textureSampleLevel(..., 0.0)`), so a 2048² map minified onto a few
  hundred pixels aliased into coherent metallic-looking bands — poisoning
  albedo, roughness, metallic, normals, occlusion and emissive at once (the
  DamagedHelmet "chrome stripes"). Mechanism: (a) `EffectNode::
  output_mipmapped` / `Primitive::output_mipmapped` compile-time hook →
  `ExecutionPlan::mipmapped_resources` → `Backend::declare_mipmapped`
  (installed by the executor before any acquire); `MetalBackend` keys the
  slot pool on mippedness so mipped/flat slots never recycle into each
  other, and lazy-alloc builds the full chain (`RenderTarget::
  new_mipmapped`, direct-device — deliberately bypasses the heap
  `TexturePool`, which recycles by `(w,h,format)` only). (b)
  `node.gltf_texture_source` declares `out` mipmapped and runs
  `generate_mipmaps` after its blit — only on fresh upload or output
  identity change, plus the black-clear path (stale-tail guard). (c)
  `render_scene.wgsl`'s five map resolves switch to `textureSample`
  (derivative LOD; the shared sampler was already trilinear). Gates:
  `declared_mipmapped_resource_allocates_a_mip_chain_and_pools_separately`
  (backend, gpu-proofs); the full `render_scene` map-set/PCSS/shadow proof
  suite green unmodified; uniform-dome probe render shows the banding gone.
- **F-P7 — SHIPPED 2026-07-15 (same-day fidelity fix).** Softbox dome fill +
  rig defaults. Diagnosis: metals have no diffuse term — they are lit
  exclusively by the environment — so D7's pure-black void made every
  metallic import read as dark chrome regardless of albedo (helmet + AMG,
  Peter's screenshots). Mechanism: `fill` param on `node.bake_environment`
  (softbox mode only; uniform slot reuses the pad, stays 64 B): a broad
  neutral dome (`fill * (0.55 + 0.45·up)`, never zero anywhere) added under
  the strips; strips accumulate separately so `emitter_intensity` scales
  strips ONLY (first-cut bug: it multiplied the fill too — Strip Lights at 0
  blacked out the world; caught by probe K, locked by
  `softbox_fill_lights_every_texel_and_ignores_strip_intensity`). `fill = 0`
  keeps the D7 pure-black contract byte-identical (existing zero-outside-
  strips gate bakes at fill 0). Importer defaults: `IMPORT_FILL_DEFAULT =
  0.6`, `IMPORT_STRIPS_DEFAULT = 3.0` (half the primitive default — full
  strips dominate every curved reflection once the fill exists), plus two
  new card faders (Fill Light, Strip Lights) under Environment. The
  environment is never drawn as a backdrop, so imports still composite over
  black. **Peter's look pass owed**: fill/strip levels were tuned against
  the probe harness's Reinhard, not the app's display transform. Sun
  verdict from the same session: not a bug — intensity 3.5 side-on is just
  a dim key; it reads fine once the fill exists, and it stays on the card.

Full workspace sweep gates F-P1, F-P2, F-P5, and F-P6 at landing (shader ABI +
port surface + Material enum + executor/backend allocation contract = infra);
F-P3/F-P4/F-P7 focused per the scope rule.

## 6. Decided — do not reopen

1. Scope = `render_scene`; single-object renderers migrate later on their own
   trigger (D1).
2. IBL prefiltering is renderer-internal, cache-keyed, no new atom, no new port
   type (D2).
3. Four new per-object ports with glTF channel conventions; textures never ride
   the Material wire (D3).
4. No MeshVertex tangents — cotangent frame in the fragment shader (D4).
5. Default import look = softbox black studio at intensity 1.0; `gradient` mode
   stays byte-identical for existing presets (D7, Peter's quoted call).
6. Clearcoat stays a report line in v1 (Deferred #1). Glass ships as F-P5's
   sorted per-object blend pass (D8, Peter's call 2026-07-15); OIT and
   per-triangle sorting stay out.
7. HDRI file loading belongs to IMPORT_DESIGN P4, not here.
8. Transparent objects cast no shadows and never write depth (D8).

## 7. Deferred (with triggers)

1. **Clearcoat lobe** (second GGX specular on `fs_pbr`, Material fields
   `clearcoat`/`clearcoat_roughness` via the M §7 "new fields, defaulted" seam) —
   trigger: a hero asset whose painted surfaces read flat after F-P1–F-P4 land.
   Peter's AMG may fire this immediately — the report line makes it visible.
   Needs typed parse (gltf crate bump or manual JSON), priced then.
2. **~~Transmission/glass~~ — PROMOTED 2026-07-15 → D8 + F-P5** (Peter: "I think
   it makes sense to add it"; the predicted trigger — car glass — fired on day
   one). What REMAINS deferred: OIT / per-triangle sorting (trigger: a hero
   asset whose intra-object glass visibly mis-sorts) and refraction/frosted
   transmission (trigger: a look that needs light-bending, not tint+reflection).
3. **`render_mesh`/`render_copies` IBL upgrade + MetallicGlass re-tune** —
   trigger: the next look-pass on a `render_mesh` preset; mechanical once the
   `pbr_brdf.wgsl` IBL helpers exist.
4. **Import-time tangent generation** (separate buffer port) — trigger: D4's
   quality consequence visibly bites on a hero asset.
5. **KHR_materials_specular / IOR mapping** — parse features are enabled by F-P4;
   mapping into Material waits for an asset that needs non-default F0.
6. **Per-input generation-counter signal on `EffectNodeContext`** (found at F-P1
   landing, 2026-07-15) — a genuine "did this input's producer change its
   output since I last ran" signal, which would let the prefiltered/irradiance
   IBL resources skip re-convolution when the wired envmap is truly unchanged
   frame-to-frame (today they re-convolve unconditionally whenever `envmap` is
   wired — see the Invariants table row above). Not built here: it's executor
   infrastructure bigger than one primitive and belongs in its own design.
   Trigger: the fixed per-frame IBL cost (3.09ms measured) becomes a real
   budget problem on the live rig, or a second primitive independently wants
   the same signal.
7. **`normal_texture.scale` / `occlusion_texture.strength` wiring** (found at
   F-P2+F-P4 landing, 2026-07-15) — D5 parses both, D4 says scale "imports as
   a multiplier," but neither F-P2's `resolve_normal`/`resolve_occlusion` nor
   F-P4's importer actually carries the value through: no shader-ABI param
   exists for either yet, so a non-default value on an imported asset becomes
   a report line (D9 doctrine) rather than a visible effect. Both fields
   default to 1.0 in most authored assets (including DamagedHelmet and the
   AMG), so this is inert on today's held-out fixtures. Trigger: a hero asset
   whose report enumerates a non-default scale/strength and visibly needs it —
   fix shape is one uniform field each plus a one-line multiply in the two
   resolve functions, no ABI growth.
