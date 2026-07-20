# Import — Blender/glTF Scenes, Baked Caches, Texture Sets, TD & Resolume Funnels

**Status: APPROVED design, P1 PARTIALLY SHIPPED — re-cut below · 2026-07-03 · Fable · baseline-reviewed 2026-07-05 · reality note refreshed 2026-07-10 (F2 coherence audit).**
**Reality note (refreshed 2026-07-10 — the 2026-07-05 version undersold what shipped):** the glTF wave landed *most of P1*, not a mesh-level door. Two stages ship in manifold-renderer: stage 1 `node_graph::gltf_load` (CPU-only `.glb` parse → `GltfImportSummary`) and stage 2 `node_graph::gltf_import::assemble_import_graph` (verified in-tree), a pure assembler that turns the summary into a full `node.render_scene` generator graph — **one object per distinct material** (material-filtered geometry via `node.gltf_mesh_source`, base-colour texture via `node.gltf_texture_source`, a `node.pbr_material` carrying the glTF PBR factors), a synthesized framing camera (`node.orbit_camera`), a sun (`node.light`), an IBL envmap (`node.bake_environment`), named+tinted node **groups** per object, an over-cap drop (smallest-by-vertex first), and an `ImportReport`. The production caller `Application::import_model_file` installs the result as one undo transaction via `manifold_editing::commands::layer::ImportModelLayerCommand`. **Placement question resolved by what shipped:** the importer lives in manifold-renderer because it emits renderer node types (`EffectGraphDef`), *not* manifold-io — D1/§3's manifold-io placement is superseded; do not move it. **P1 is therefore a re-cut of what remains (§5 P1), not a rebuild** — an executor who follows the original P1 brief will re-author the shipped assembler. Known in-app bugs against the shipped nodes are VD-003 in `docs/VERIFICATION_DEBT.md`; held-out fixtures are the CC0 photoscans already sitting untracked in `tests/fixtures/gltf/` (apricot, azalea, lowe — the §8 Stewartia was never downloaded; these replace it).
**Prerequisites (per phase): P1–P2 need REALTIME_3D P1 + MATERIAL M1–M5 **and M6**
(albedo/metallic maps + alpha cutout — MATERIAL §11; without M6 a textured glTF
imports colourless and foliage renders as opaque cards; see §8 addendum). P3 pairs with
MEDIA_BACKEND streaming discipline. P4 needs only MATERIAL. P5 needs SESSION_MODE +
MEDIA_BACKEND P2 (DXV). P6 needs VOCAB apply; its agent half needs MCP_INTERFACE.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting
any phase.**
**Ordering note (2026-07-15): `docs/IMPORT_FIDELITY_DESIGN.md` (full PBR map set,
split-sum IBL, softbox default look) outranks this doc's P1-remaining in build order —
Peter's call ("really critical infra"). Its F-P4 also absorbs the §8 normal-map
report-line scope; re-read §8 before briefing P1.**

Peter's directives (2026-07-03): Blender import "would be amazing and seriously open
up Manifold as a real contender … that beats TouchDesigner at this type of thing";
TD import: yes, explored — funnel, not fidelity promise; "Resolume import would be
amazing"; community materials/textures/looks wanted. The thesis: **artists author in
the world's best tools; MANIFOLD is the stage it all plays on** — same shape as
Ableton playing studio-produced stems.

Companions: `REALTIME_3D_DESIGN.md` (the scene imports land in; supersedes its §8
import bullet), `SIMULATIONS_DESIGN.md` (its lane 1 = P3 here), `MEDIA_BACKEND_DESIGN.md`
(DXV, streaming), `SESSION_MODE_DESIGN.md` (Resolume grid target),
`MCP_INTERFACE_DESIGN.md` (TD agent path), `ABLETON_SHOW_SYNC_DESIGN.md` (the
precedent: we already parse another app's format — .als — for interop).

---

## 1. Audit — what exists (verified 2026-07-03)

| Piece | Where | State |
|---|---|---|
| Scene model shaped for import | `REALTIME_3D_DESIGN.md` D1 | Object list + TRS + material = what a .glb unpacks into. Built for this |
| PBR texture inputs | `MATERIAL_SYSTEM_DESIGN.md` §5 | `base_color_map/normal_map/roughness_map/metallic_map/envmap` — exactly the community-texture-set shape |
| DXV/HAP native decode | `MEDIA_BACKEND_DESIGN.md` §4 | A Resolume user's library plays natively |
| Session grid | `SESSION_MODE_DESIGN.md` | Layer × scene ≅ Resolume layer × column |
| Cross-tool alias dictionary | `archive/NODE_VOCABULARY_AUDIT.md` §8b | TD/Resolume/AE names per atom — seeds the op-mapping tables |
| .als interop parsing | `ABLETON_SHOW_SYNC_DESIGN.md` | The in-house precedent for parsing user-owned third-party files |
| Beat atoms | `beat_ramp` etc. | The retimed-playhead drivers |

`⚠ VERIFY-AT-IMPL`: everything above is design-stage except the alias dictionary and
beat atoms — this doc executes late in the build order; run the §8.3 pre-flight and
re-verify against as-built code.

## 2. Decisions

- **D1 — glTF (.glb) is the one 3D door.** Blender exports it natively; Unity/Unreal
  and everything else reach us through it. No FBX (proprietary SDK swamp), no USD v1
  (huge), no OBJ (dead end). Parser: the mature pure-Rust `gltf` crate. Import maps:
  mesh nodes → `render_scene` object groups (flat v1, hierarchy transforms
  pre-composed until parenting lands); **Principled BSDF → `node.pbr_material`** +
  texture wiring; glTF cameras → `node.free_camera` params; glTF lights →
  `node.light` params. Over-cap scenes (more than `OBJECT_SLIDER_MAX = 64` objects; the
  shipped assembler's `MAX_RENDER_SCENE_OBJECTS = 8` is a stale mirror to be lifted to 64
  per SCENE_BUILD D9 — F6) import with a visible warning and merged-by-material fallback,
  never a silent drop.
- **D2 — Animation tiers, smallest first:** rigid TRS (v1, P2) → vertex caches
  (P3) → skeletal/skinned (**deferred** — the cathedral; morph targets ride along
  when it lands).
- **D3 — The beat-retimed playhead is the differentiator.** An imported animation's
  clock (seconds @ authored FPS) becomes a **beat-addressable parameter**: a
  `beat_ramp` scrubs progress, a trigger freezes it, sub-ranges loop on the bar,
  scrub runs backwards. The collapse hits the downbeat because you're scrubbing it.
  Only possible because MANIFOLD is beats-native; no other tool has this.
- **D4 — Vertex caches: MDD + PC2 in v1.** Trivial binary formats (per-frame vertex
  positions), pure-Rust parsers, Blender exports both natively; Houdini reaches us
  via Blender or a one-hop conversion. **Streamed from disk with lookahead**
  (media-backend prefetch discipline — caches can exceed RAM; never fully resident,
  never decoded on the content tick). Alembic and VDB are deferred with triggers
  (§8). This is `SIMULATIONS_DESIGN.md` lane 1 — photoreal Houdini water on stage,
  beat-retimed.
- **D5 — Texture sets are first-class drops.** Drop a PolyHaven/ambientCG-style
  folder → maps auto-wire to a `pbr_material` by filename convention
  (albedo/basecolor, normal, roughness, metallic, AO; case/underscore variants).
  Drop an equirect HDR → envmap. This is most of "community materials" in practice,
  and it's nearly free.
- **D6 — Blender procedural shader graphs never import.** They're Cycles/Eevee
  programs and don't survive Blender's own export (glTF carries baked Principled
  only). Guidance, documented in-product: bake looks to textures, use Principled.
  MANIFOLD's own look-sharing = components/presets (by-value) and later Authored
  materials — our ecosystem, not an emulation of theirs.
- **D7 — TouchDesigner: a migration funnel, not a compatibility promise.** The user
  expands their own `.toe` with TD's bundled `toeexpand` (we never ship or touch
  TD's binaries — interop parsing of user-owned files, the .als precedent). Then:
  **deterministic core** — mapping table for the top TOPs/CHOPs seeded by the alias
  dictionary; wires import; unmapped ops become `node.placeholder` (identity
  passthrough carrying the original op name/params as metadata, visibly flagged).
  **Agent path** — an MCP agent reads the expanded text + the node catalog and
  re-authors the long tail (fuzzy translation is agent work, not parser work).
  Python expressions and COMP semantics are explicitly out of scope, stated in the
  import report. Expect 60–80% of a texture network to stand; the report lists the
  rest.
- **D8 — Resolume: the deterministic funnel.** `.avc` is XML. Composition layers →
  layers (blend modes mapped); the clip grid → **session grid** (layer × column →
  layer × scene, near 1:1); clip media relinked by path — **DXV plays natively**
  (D-day detail for switchers); built-in effects → mapping table, unmapped →
  placeholders; FFGL/autopilot skipped, listed in the report. Net: open your comp,
  your library is launchable in MANIFOLD tonight.
- **D9 — Every import produces a report.** What mapped, what's a placeholder, what
  was skipped and why — a visible document (import log panel + text file), never a
  silent best-effort. Same doctrine as the show-sync report and the unmapped-cue
  warnings.
- **D10 — Imported assets are referenced, not copied** — same model as video/audio
  media today. Collect-for-show is an existing project-archive concern, not this
  design's.

## 3. New pieces (small)

- `node.placeholder` — identity passthrough + metadata (source tool, op name,
  original params as a string table). One primitive; the editor shows it flagged.
  §2.5 audit at impl (expect: genuinely new, nothing adjacent).
- Vertex-cache playback: a mesh-sequence source atom (`node.mesh_sequence` sketch
  name) — path + playhead param (beat-addressable per D3) → `Array(MeshVertex)`
  into `render_scene`. Streaming reader behind it (D4).
- Importers live in `manifold-io` (parsers are serialization work; glTF/MDD/PC2/
  avc/toe-expanded readers), emitting ordinary `EditingService` command batches —
  an import is one undoable transaction.

## 4. What it buys on stage

- Author a set piece in Blender, drag the .glb in, it's lit by your scene, its
  animation scrubs on a beat ramp.
- A Houdini ocean bake plays as a mesh sequence — the wave breaks on the drop, every
  night, because the playhead is beats.
- A Resolume refugee opens their comp and performs from the session grid the same
  week, DXV library untouched.
- A TD patch arrives 70% rebuilt with the gaps flagged; the agent closes most of the
  rest.

## 5. Phasing (Sonnet-executable)

Forbidden, all phases: silent drops (D9 — everything unmapped is reported) ·
shipping/linking any third-party tool's binaries (D7) · fully-resident vertex
caches (D4) · import writing to the model outside one `EditingService` transaction ·
FBX/USD scope creep.

- **P1 — glTF static scenes.** **[RE-CUT 2026-07-10 (F2 audit) — the assembler
  (`gltf_import::assemble_import_graph`), per-material object groups, Principled →
  pbr_material, base-colour texture wiring, synthesized camera + sun + IBL, over-cap drop,
  `ImportReport` struct, and the single-undo-transaction install
  (`ImportModelLayerCommand`) already SHIP. Do not re-author them.]** What remains for P1:
  1. **glTF light mapping** — glTF punctual lights → `node.light` params (today: one
     synthesized sun regardless of the file's lights).
  2. **glTF camera consumption** — embedded cameras → `node.free_camera` (today: always a
     synthesized `orbit_camera`; `ImportReport::camera_synthesized` already exists to
     report `false` when this lands).
  3. **Import report surfaced to the user** — the `ImportReport` struct + logs exist;
     needs a user-visible surface (over-cap drop + skipped-feature warnings on import).
  4. **`alphaMode` / tangent-space-normal / `doubleSided` report lines** per §8 (audit
     which of these already emit; each un-mapped feature is a report line, D9).
  5. **Hierarchy transform pre-compose** — verify/ensure parent-matrix pre-composition for
     the flat-v1 claim (D1).
  6. **Object cap 8→64 (F6)** — the assembler's `MAX_RENDER_SCENE_OBJECTS = 8`
     (`gltf_import.rs:49`) is a stale mirror of the retired `render_scene` `MAX_OBJECTS`;
     the renderer is now `OBJECT_SLIDER_MAX = 64`. Track it (SCENE_BUILD D9 makes the
     importer threshold `OBJECT_SLIDER_MAX`).
  7. **Khronos conformance fixtures + gate**, plus the §8 Stewartia/photoscan visual gate.

  Fixtures: Khronos glTF sample models + the `tests/fixtures/gltf/` photoscans. Gate: a
  known .glb with lights + an embedded camera imports with both mapped (not synthesized);
  the report lists every un-mapped node/feature of a deliberately over-featured .glb;
  existing assembler tests stay green.
- **P2 — Rigid animation + beat playhead.** TRS keyframe sampling; playhead as
  beat-addressable param (D3); freeze/loop/reverse semantics. Gate: a beat_ramp
  scrubs a known animation to exact keyframe values (unit test on sampled TRS);
  loop-on-bar demo preset.
- **P3 — Vertex caches (MDD/PC2) + `node.mesh_sequence`.** Pure-Rust parsers,
  streaming reader with lookahead, memory budget cap. Gate: parser unit tests
  against Blender-exported fixtures (byte-level known values); a baked cloth sim
  plays at 60fps with bounded memory (measured, reported); playhead beat-scrubs.
- **P4 — Texture-set auto-wire + HDRI drop.** Filename-convention mapping, ambiguity
  → picker not guess. Gate: PolyHaven set fixture wires all maps correctly; a
  miss-named map lands in the picker, never the wrong slot.
- **P5 — Resolume funnel.** `.avc` XML reader → layers/blend/grid/media relink;
  effect mapping table + placeholders; report. Gate: a real composition fixture →
  session grid matches its column layout; DXV clips play; report enumerates every
  unmapped effect.
- **P6 — TD funnel.** Expanded-.toe parser, core op-mapping table (alias-dictionary
  seeded), placeholder emission, report; agent-assist flow once MCP is live. Gate:
  a sample expanded network imports with ≥N core TOPs mapped (table-driven test) and
  every unmapped op present as a flagged placeholder — zero silent drops.

Full workspace sweep gates P1 (new asset path through io + renderer = infra);
P2–P6 focused per the scope rule.

## 6. Decided — do not reopen

1. glTF is the only v1 3D format; no FBX, no USD, no OBJ.
2. Vertex caches v1 = MDD + PC2, streamed never resident; Alembic/VDB deferred.
3. The beat-retimed playhead ships with animation import — it IS the feature.
4. Blender shader node trees never import; bake-to-texture is the documented path.
5. TD = funnel (toeexpand + core table + placeholders + agent), never a fidelity
   promise; Python/COMP semantics out of scope. No TD binaries shipped, ever.
6. Resolume grid → session grid; effects best-effort with placeholders; DXV via the
   native TextureCodec backend.
7. Every import emits a report; placeholders over silent drops, everywhere.
8. Imports are single undoable transactions; assets are referenced, not copied.

## 7. Deferred (with triggers)

- **Skeletal/skinned animation + morph targets** — when a rigged character look is
  actually wanted on stage; GPU skinning design rides then.
- **Alembic** — when a workflow can't route through Blender/MDD; **VDB volumes** —
  with SIMULATIONS lane 3 (volume rendering).
- **Hierarchy/parenting on import** — currently pre-composed flat; unlocks with
  REALTIME_3D's parenting deferral.
- **MaterialX** — possible future door for Authored materials; watch the ecosystem.
- **Resolume advanced (dashboards, autopilot sequences)** — demand-driven.
- **Import in reverse (export .glb of MANIFOLD scenes)** — different product
  question; not queued.

## 8. Addendum 2026-07-04 — material-mapping corrections + fixtures (pre-execution)

Verified against the shipped Material system (as-built record: MATERIAL §11.1).
Corrections to this doc's §1 audit and P1 scope:

- **§1 audit row "PBR texture inputs" was design-stage optimism.** As-built,
  `node.render_mesh` has `envmap` / `normal_map` / `roughness_map` only
  (`render_3d_mesh.rs:67–75`) — no `base_color_map`, no `metallic_map`. **MATERIAL
  M6 is therefore a hard P1 prerequisite** (now in the header and
  DESIGN_BUILD_ORDER).
- **glTF `alphaMode` mapping (P1):** `OPAQUE` → `Opaque`; `MASK` → `Mask` +
  `alphaCutoff` → `alpha_cutoff`; `BLEND` → `Mask` (cutoff 0.5) **with an
  import-report warning** (MATERIAL M6-D3 — smooth transparency deferred).
  **[SUPERSEDED for scene imports 2026-07-15: IMPORT_FIDELITY D8/F-P5 (SHIPPED
  2026-07-15, `61400029`) maps `BLEND` and `KHR_materials_transmission` to
  `AlphaMode::Blend` in `render_scene`; the Mask mapping stays correct for
  `render_mesh`/`render_copies` (D1 scope fence — untouched by this doc).]**
  `doubleSided` imports as a no-op with a report note: the engine rasterizes both
  faces already and back-face lighting is corrected by M6-D4.
- **glTF normal maps are tangent-space; the engine's `normal_map` is world-space**
  (MATERIAL M6-D5). ~~P1 skips them — each skip is a report line (D9), not a silent
  drop. Revive per M6-D5's trigger.~~ **[SUPERSEDED 2026-07-15: the trigger fired;
  `docs/IMPORT_FIDELITY_DESIGN.md` owns tangent-space normals, the full per-object
  map set (normal/MR/occlusion/emissive), split-sum IBL, and the softbox default
  look. Its F-P4 absorbs this doc's map-wiring and report-line scope; P1-remaining
  here (lights, cameras, report surface, cap) is unchanged and now ORDERS AFTER
  IMPORT_FIDELITY per Peter's 2026-07-15 priority call.]**
- **Fixtures live in `tests/fixtures/gltf/`** (sibling of the `.manifold` fixtures).
  Two tiers: (a) Khronos glTF sample models for conformance, per the P1 brief;
  (b) the **canonical real-world fixture**: the CC0 Stewartia monadelpha photoscan
  (sketchfab.com/3d-models/cc0-himesyara-stewartia-monadelpha-cae7436738674d3586930c206f51073b)
  — multi-material, alpha-masked foliage, real photoscan vertex counts; exactly the
  asset class the wave exists for. CC0 = committable. **Sketchfab downloads need an
  account login, so Peter downloads it by hand** (glTF format) into that directory
  before P1 starts; P1's gate renders it and eyeballs against Sketchfab's own
  preview (per the visual-question oracle: a PNG, not a green test).
