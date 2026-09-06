# Scene Loop Endless Corridor — windowed modulo-tiled instancing

**Status:** PROPOSED — direction ratified by Peter 2026-09-06, pending adversarial review · 2026-09-06 · k3 (lead)
**Prerequisites:** SCENE_LOOP (shipped, absorbed into SCENE_MODIFIER_FRAMEWORK — the atoms/commands contract this doc revises), RT_INSTANCING P0–P3 (shipped 2026-09-05 — the accel/stasis contract the windowed atom must preserve).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

The shipped Scene Loop lays a fixed row of N copies and flies the camera along
it. The world ends at copy N, and every seam artifact — the far-edge hole,
stride outrun, the copies-cap question — is the camera seeing that end. This
design replaces the fixed row with a **windowed modulo-tiled corridor**:
`node.scene_array` generates instances only for the cells visible around the
camera, each cell's transform comes from `pattern[cell mod pattern_length]`,
and the camera's per-loop travel is an integer number of whole patterns. The
world is endless and everywhere identical; wrap purity holds by construction
for any speed and any pattern length.

Peter's ratified direction, verbatim from the planning session: *"replace the
fixed row of N copies with a windowed modulo-tiled corridor — scene_array
generates instances for the cells visible around the camera (camera position
wired in, small per-frame window refill), transform = pattern[cell mod
pattern_length], jitter keyed on the same index; Stride becomes 'patterns per
loop' coupled to pattern_length."*

On stage: a scanned corridor, tunnel, or forest flies past forever, locked to
the track, and the performer can push the flythrough as fast as the set
wants — the world keeps up because there is no end to outrun. When it's
broken it looks like: a one-frame jump at the wrap (a non-phase-periodic
driver — the D8 class, unchanged), or a pop at the far edge when a hand-set
Far plane exceeds the window (bounded, named in D5's honest costs).

Companion docs: `docs/SCENE_LOOP_DESIGN.md` (the shipped loop contract —
D1–D11 remain cited; this doc revises the instance model), `docs/RT_INSTANCING_DESIGN.md` (INV-RTI4 stasis the windowed atom must preserve), `docs/SCENE_MODIFIER_FRAMEWORK_DESIGN.md` (the modifier-kind surface the card rows ride).

---

## 1. Audit — what exists (verified 2026-09-06)

### 1.1 The current instance model

| Piece | Where | State |
|---|---|---|
| `node.scene_array` | `crates/manifold-renderer/src/node_graph/primitives/scene_array.rs:72-159` | Source atom, NO inputs. `count` (1..8) copies at `i * cell_size` along `axis`; optional jitter from a hash of `index % jitter_period` (`scene_array_body.wgsl`). Output buffer capacity = count's range max 8 (`array_output_capacity` `:162-184`); surplus slots masked zero-scale. `SceneArrayStasisKey` (`:62-70`) skips the rewrite when {count, axis, cell_size, jitter_seed, jitter_amount, rebuild_epoch} hold — INV-RTI4 producer stasis. |
| `node.loop_camera` | `crates/manifold-renderer/src/node_graph/primitives/loop_camera.rs:235-339` | Emits `Camera` + `pos_x/pos_y/pos_z`. Travel = `home + d(phase)·stride·cell_size`, `d(p) = p − flow·sin(2πp)/(2π)` (`:292-294`). `stride` = whole cells per loop, range 1..8 (`:164-171`). All movement terms phase-periodic (INV-3). |
| Plan builder | `crates/manifold-renderer/src/node_graph/scene_modifier.rs:569-756` (`build_scene_loop_plan`) | Mints loop_phase/scene_array/loop_camera (+switch); cell_size = 2× Z-extent (D4 gap rule, `:586-588`); home = −cell/2; wires `loop_phase.out → loop_camera.phase` ONLY — scene_array takes no input. |
| Stride coupling | `scene_modifier.rs:450-492` | Stride row writes {loop_camera.stride, scene_array.count = K+2 clamped 8, scene_array.jitter_period = K}. **K ≥ 7 outruns the array** — the cap the corridor dissolves. |
| Spacing coupling | `scene_modifier.rs:471-485` | cell_size row writes both nodes' cell_size + home = −cell/2 (INV-4). Unchanged by this design. |
| Card rows | `scene_modifier.rs:421-441` (`LOOP_ROW_WHITELIST`) | 18 rows incl. ("scene_array","count","Copies"), ("loop_camera","stride","Stride"), ("scene_array","jitter_amount","Jitter"). |
| Load migrations | `scene_modifier.rs:766-896` | `migrate_pre_switch_scene_loops`, `migrate_loop_exposure_rows` — the per-layer load loop precedent the corridor migration extends. |
| Wrap-parity gates | `crates/manifold-renderer/tests/scene_loop_wrap_parity.rs` | INV-3 pixel gates: exact seam (beat 0 vs 8, diff == 0), near-seam bounded (phase 0.99999, ≤8 px / ≤48 delta), bars-change continuity. **The near-seam gate today needs far=22 clipping** (`:283-303`) because the finite array's far-edge hole otherwise confounds the measurement — the corridor removes that crutch. |
| RT instancing | `docs/RT_INSTANCING_DESIGN.md` D1/D9/INV-RTI4/5 | Instance buffers GPU-resident; `instance_count = buffer_size/32` (CAPACITY, not live count — `render_scene.rs:4909-4919`); capacity rides the topo key (rebuild), values ride refit; INV-RTI4: static instance buffers trigger no descriptor dispatch/refit beyond the transform-driven cadence. |
| Camera struct | `crates/manifold-renderer/src/node_graph/camera.rs:83-101` | Carries `pos`, `near`, `far` — enough to derive the window from a wired camera. |
| Camera-input codegen atom | `crates/manifold-renderer/src/node_graph/primitives/project_3d.rs:65-127` | Precedent: `fusion_kind: Pointwise` + `wgsl_body` atom with an optional `camera: Camera` input resolved CPU-side into uniforms (`cam_pos`/`cam_right`/`cam_up`/`cam_fwd`/`cam_near`/`use_camera`). The corridor atom copies this seam shape exactly. |

### 1.2 Section 2.5 audit statement (DECOMPOSING_GENERATORS.md)

**This extends `node.scene_array` in place — no new primitive is proposed.**
The primitive survey (`rg 'purpose: "' …/primitives/`) finds no existing
windowed/modulo instance generator: the tiling-adjacent atoms are
`torus_wrap_field`/`cylinder_wrap_field` (curve→transform lifts),
`generate_instance_transforms` (procedural layouts), `wrap_particles_torus`
(2D particle wrap) — none generate a camera-windowed corridor. The nearest
reference shapes are the shipped loop graph itself (section 1.1) and
`project_3d`'s Camera-input codegen pattern. Classification: **exists**
(scene_array, loop_camera, plan/command pair, card surface, migrations,
pixel gates) / **extended** (scene_array becomes camera-driven; loop_camera
gains `pattern_length`, `stride`→`patterns_per_loop`) / **genuinely new:
nothing**.

---

## 2. Decisions

- **D1 — `scene_array` becomes a camera-driven windowed generator.**
  New optional input `camera: Camera` (precedent: `project_3d`). Each frame
  the atom resolves `base_cell = floor(axis_component(camera.pos) / cell_size)`
  and refills a fixed-capacity buffer with one `InstanceTransform` per window
  slot: slot `w` ↔ corridor cell `c = base_cell − BEHIND + w`, transform =
  translation `c · cell_size` along `axis` plus jitter keyed on
  `c mod pattern_length`. `BEHIND = 8` (constant; covers shadow-map
  look-behind — honest cost in section 3.4). Capacity = 32 slots fixed
  (value-level constant, never a param — the BUG-757c (scene-loop-copies-param-inert) class).
  **Rejected: a new `node.corridor` primitive**, because the 2.5 audit shows
  scene_array IS the corridor atom with a window instead of a count — a
  second atom would split the vocabulary for one concept.
  **Rejected: per-frame unconditional refill**, because a moving camera
  inside one cell changes nothing (cell content is cell-indexed, not
  position-indexed) — the stasis key in D6 makes the rewrite happen only on
  cell-boundary crossings.

- **D2 — the camera arrives as a `Camera` port, not scalar position wires.**
  `loop_camera.out` already fans out (to `loop_cam_switch.b`); the plan adds
  one wire `loop_camera.out → scene_array.camera`. The atom picks the axis
  component from its own `axis` param, so axis and position can never
  desync. The window span derives from the SAME camera's `far` (D5).
  **Rejected: three scalar inputs `pos_x/pos_y/pos_z`** (loop_camera already
  emits them), because wiring the matching component is the performer's
  responsibility in the hand-built case — the axis-match mistake (axis +X,
  pos_z wired) is silent and produces an empty corridor.

- **D3 — Stride becomes `patterns_per_loop`, coupled to `pattern_length`.**
  `scene_array.count` (Copies) → `scene_array.pattern_length` (card row
  **"Pattern"**, range 1..8): how many distinct cells before the pattern
  repeats. `loop_camera.stride` → `loop_camera.patterns_per_loop` (card row
  **"Stride"**, range 1..8): how many whole patterns per loop. Travel per
  loop = `d(1) · patterns_per_loop · pattern_length · cell_size`. Since
  travel is an integer multiple of `pattern_length` cells **for any integer
  K, P**, wrap purity is by construction — the purity constraint
  "jitter_period divides stride" (the BUG-jvlq (scene-loop-jitter-wrap-snap) coupling) stops existing as a
  coupling and becomes arithmetic. `loop_camera` gains an internal
  `pattern_length` param (never a card row; written by the plan builder, the
  Pattern row's coupled secondary, and load migration — the Spacing/cell_size
  dual-stamp precedent, `scene_modifier.rs:471-485`).
  **Rejected: keeping `stride` as cells-per-loop with a validation warning**,
  because a validity check that fires after the performer already desynced
  the loop is a worse instrument than params that cannot desync.

- **D4 — `jitter_period` dies; jitter keys on the same modulo index as the
  pattern.** The hash input becomes `(cell mod pattern_length)` — the
  generalization of the BUG-jvlq mechanism (its shipped form was period =
  stride; the corridor makes period = pattern_length and purity is
  unconditional). One index drives both the cell's jitter and its place in
  the pattern, so "variation" and "wrap safety" can no longer be set
  independently — they were never independent.

- **D5 — the window derives from the camera's far plane; the far-edge hole
  dies by construction, not by fog.** Per frame:
  `ahead = clamp(ceil(camera.far / cell_size) + 2, 4, CAPACITY − BEHIND − 2)`.
  Every cell that can possibly render (within `far`) exists in the buffer;
  the +2 margin cells sit beyond `far` and are clipped without rasterizing.
  The old copies-cap question disappears: vertex/descriptor cost is fixed at
  capacity regardless of speed. **Consequences, stated honestly:** the
  corridor is endless *within the window* — instances outside
  `[base−8, base+22]` cells do not exist, so their shadows don't either
   (BEHIND=8 cells = 16 object-depths under the D4 gap rule — generous, but
  not infinite); a hand-edited `far` beyond the curated 20×cell band clamps
  `ahead` at 22 and the distant hole returns past that (the card's curated
  Far range prevents this on the supported surface); window margin cells are
  submitted every frame and rejected by the far plane (fragment cost flat,
  per the D10 analysis).

- **D6 — stasis is the RT contract, and the window is stasis-friendly.**
  `SceneArrayStasisKey` becomes `{base_cell, ahead, pattern_length,
  axis, cell_size, jitter_seed, jitter_amount, rebuild_epoch}` — every input
  that can change the output. Camera motion inside a cell changes none of
  them → no buffer rewrite → output generation holds → the RT accel key
  holds (INV-RTI4). A cell-boundary crossing (≤ `patterns_per_loop ·
  pattern_length` times per loop — e.g. once per bar at 8 bars/loop) rewrites
  32 slots in one dispatch; capacity never changes, so the accel takes the
  refit path, not a rebuild (INV-RTI5), on top of the refit the moving
  camera already drives.
  **Rejected: ring-buffer slot rotation** (write only the entering cell and
  rotate the base), because the consumer reads slots `[0, capacity)` linearly
  and rotation would need either a consumer-side indirection (new machinery)
  or a full remap anyway — a 32-slot rewrite is one trivial dispatch at
  crossing cadence.

- **D7 — saved loops migrate at load; the migration is arithmetic, not
  lossy.** Old purity already required `jitter_period | stride`, so the
  shipped coupled writes guarantee the division is exact:
  - `scene_array.jitter_period J` (absent → 1) → `scene_array.pattern_length = J`
  - `scene_array.count` → **deleted** (the window replaces it; capacity is fixed)
  - `loop_camera.stride S` → `loop_camera.patterns_per_loop = S / J`
  - `loop_camera.pattern_length = J` (new internal param)
  - Card row ("scene_array","count","Copies") → ("scene_array","pattern_length","Pattern"); the Stride row keeps its label, new semantics.
  Hand-edited pre-migration graphs where J ∤ S were wrap-impure already;
  migration rounds S/J and the loop stays impure (no worse). The migration
  runs in the same per-layer load loop as `migrate_loop_exposure_rows`
  (`scene_modifier.rs:829-896`); structural precedent `migrate_pre_switch_scene_loops` (`:766-817`).
  **Rejected: dual-semantics param reading** (detect old vs new by a version
  flag), because a param whose meaning depends on a flag is the transitional
  state the house rule forbids — load migration upgrades to the single new
  semantics and the old names disappear from the manifest.

- **D8 — acceptance is wrap-parity pixel gates plus the Stone Effects
  projects as the held-out inputs.**
  1. **Exact seam, unclipped, at multiple shapes:** beat 0 vs beat = bars
     (fract → phase 0) max pixel diff == 0 on the minimal corridor graph at
     (patterns, pattern) ∈ {(1,1), (1,3), (2,4), (8,1)} — the last is the
     old K≥7 outrun case the shipped model cannot represent.
  2. **Near-seam, unclipped:** phase 0 vs 0.99999 bounded by the jitter
     rasterization floor (≤8 px, ≤48 delta at 64×64) — **without** the
     far=22 crutch today's gate requires; the far-edge hole no longer
     confounds the measurement.
  3. **Stone Effects v1/v2** (under Dropbox …/Interim/Head Noise/): load
     (migration fires) → headless real-app-path render → v1's per-loop alpha
     coverage blip (the BUG-b6iv (scene-loop-wrap-one-frame-object-blip)
     metric: mean alpha 0.0166→0.0142 at the tick
     before wrap) collapses to baseline; v2's stride-7 outrun flash is gone.
     Real imports are device-seed nondeterministic
     (BUG-twa6 (real-import-device-seed-nondeterminism)), so this gate
     is a bounded statistical comparison across wrap-adjacent ticks, not an
     exact-zero demand.

---

## 3. Design body

### 3.1 `node.scene_array` — committed surface

```rust
// crates/manifold-renderer/src/node_graph/primitives/scene_array.rs
inputs:  { camera: Camera optional }
outputs: { out: Array(InstanceTransform) }
params: [
    pattern_length:  Int, 1..8,  default 1   // card row "Pattern"
    axis:            Enum, AXIS_LABELS, default 4 (+Z)
    cell_size:       Float, 0.01..1000.0, default 10.0
    jitter_seed:     Int, 0..32767, default 0 // internal re-roll knob
    jitter_amount:   Float, 0..1, default 0.0 // card row "Jitter"
]
// D4 (shipped): jitter_period REMOVED. count REMOVED.
```

- `fusion_kind: Pointwise` (was `Source` — it has an input now; per-element
  over output slots, camera resolved CPU-side to uniforms exactly like
  `project_3d`). `wgsl_body` + `wgsl_includes: [NOISE_COMMON]` unchanged in
  mechanism.
- `WINDOW_CAPACITY: u32 = 32` — value-level constant.
  `array_output_capacity` returns `Some(WINDOW_CAPACITY)` for `out`
  unconditionally (never param-derived — BUG-757c (scene-loop-copies-param-inert)).
- Uniform layout (generated-codegen order):
  `pattern_length(i32), axis(u32), cell_size(f32), jitter_seed(i32),
  jitter_amount(f32), base_cell(i32), behind(u32), ahead(u32),
  dispatch_count(u32), _pad` — 10 words, 40 bytes.
- WGSL body (value-level contract): slot `w` computes
  `c = base_cell − behind + w`; `w ≥ behind + ahead + 1` OR
  `c mod pattern_length` out of range → zero-scale (the BUG-757c (scene-loop-copies-param-inert)
  mask); otherwise `pos = axis_vec · (c · cell_size)` and the jitter branch
  hashes `(c mod pattern_length)` exactly as today's body hashes
  `(index % jitter_period)` (same `hash_u32`, same ±amount rotation, same
  `1 ± amount/2` scale). CPU oracle in `gpu_tests` mirrors it field-for-field.
- `run` (Rust): resolve camera input; unwired → `use_camera = 0`,
  `base_cell = 0`, `ahead = 22` (standalone default: corridor from the
  origin). Wired → axis component of `camera.pos`, `ahead` per D5's formula
  from `camera.far`. Stasis key per D6; on miss, one compute dispatch over
  `WINDOW_CAPACITY` threads.

### 3.2 `node.loop_camera` — committed deltas

Two params change, the travel formula gains one multiply, everything else
(including every phase-periodic control) is untouched:

```rust
// REMOVED: stride: Int 1..8
// ADDED:
patterns_per_loop: Int, 1..8, default 1   // card row "Stride"
pattern_length:    Int, 1..8, default 1   // internal — never a card row
// travel (:292-294 becomes):
let travel = home + eased * (patterns_per_loop * pattern_length) as f32 * cell_size;
```

### 3.3 Plan builder, card surface, coupling — committed deltas

`build_scene_loop_plan` (`scene_modifier.rs:569-756`):
- scene_array mint params: `pattern_length = 1, axis, cell_size,
  jitter_seed = 0, jitter_amount = 0` (no count, no jitter_period).
- loop_camera mint params: `patterns_per_loop = 1, pattern_length = 1`
  (plus the unchanged set).
- One new wire: `loop_camera.out → scene_array.camera`.

`LOOP_ROW_WHITELIST` (`:421-441`): `("scene_array","count","Copies")` →
`("scene_array","pattern_length","Pattern")`;
`("loop_camera","stride","Stride")` → `("loop_camera","patterns_per_loop","Stride")`.
All other rows unchanged.

`LOOP_COUPLED_WRITES` (`:450-492`) becomes:
- Pattern primary ("scene_array","pattern_length") → secondary
  ("loop_camera","pattern_length", identity). *Purity cannot desync: any
  integer pair is pure (D3), so there is no count secondary and no
  jitter_period secondary.*
- Spacing primary ("loop_camera","cell_size") → unchanged secondaries
  (scene_array.cell_size, home = −cell/2).
- **The Stride coupling (count = K+2, jitter_period = K) is deleted — the
  class it patched (outrun, wrap-snap) no longer exists.**

New load migration `migrate_fixed_row_scene_loops(def) -> bool` (shape like
`migrate_loop_exposure_rows`, same call site): the D7 arithmetic, then
re-stamp the loop exposures through the new whitelist. The exposure stamper
is idempotent by (node_id, param) — the migration must FIRST drop stamped
rows whose (node_id, param) is no longer whitelisted ((scene_array,count) and
(loop_camera,stride)) or the dead rows linger (the P4 double-stamp class).
Deletion proof: after migration, `rg '"count"'` over a migrated fixture's
exposures is empty.

### 3.4 Data-model answers (the four questions)

Owner: the generator layer's `graph_def` on the content thread — unchanged.
Thread: mutated by the composite command through EditingService — unchanged.
Serialization: ordinary layer graph_def; the param renames ride the
load-migration path (D7), no format change. Mutation: `with_target_graph_mut`
level snapshot — unchanged. **No new shared state, no new thread, no new
channel.** The per-frame window refill is a GPU dispatch inside the existing
atom `run` — hot-path cost is one `floor` + one key comparison per frame,
zero allocations.

### 3.5 BUG-cb2k (scene-loop-seam-cut-under-live-param-modulation) hooks (seam cuts under live modulation — explicitly out of scope)

The corridor leaves the seam-cut question
(BUG-cb2k (scene-loop-seam-cut-under-live-param-modulation)) mechanically
checkable. The purity contract is now a two-clause predicate: **(1) every
visible cell is a function of `cell mod pattern_length`, and (2) the camera
travel per loop is an integer multiple of `pattern_length` cells.** A
future binding-time warning (option a) needs only to ask: is this modulated
row one of {pattern_length, patterns_per_loop, jitter_amount, cell_size,
camera path terms}, and is its driver phase-periodic? The design leaves the
predicate as the doc-level contract; no code lands for it here.

---

## 4. Invariants & enforcement

| # | Invariant | Enforcement |
|---|---|---|
| INV-EC1 | Wrap purity by construction: travel per loop = K·P·cells ≡ 0 (mod P); all visible content is a function of `cell mod P` | `wrap_parity_phase_0_vs_phase_1` extended to the four (K,P) shapes of D8.1 (exact 0, unclipped); a CPU test asserts `patterns_per_loop · pattern_length ≡ 0 (mod pattern_length)` for the full 1..8 × 1..8 grid. |
| INV-EC2 | Output capacity is the constant 32, never a live value | Existing BUG-757c (scene-loop-copies-param-inert) capacity test repurposed: `array_output_capacity` returns 32 for any params. |
| INV-EC3 | The window always covers the visible range: `ahead ≥ ceil(far/cell) + 1` and `BEHIND + ahead + 1 ≤ CAPACITY` | Unit test over the curated far band (0.25×..20× cell) asserts the D5 formula's bound; clamp path asserted at hand-set far beyond the band. |
| INV-EC4 | Stasis key completeness: every frame-varying input is in the key | RT_INSTANCING's INV-RTI4 gpu_proofs (`rt_noise_gate.py` + instancing proofs) stay green on a corridor graph with the camera parked AND at speed; code review rule + a debug_assert that camera-wired and key fields move together. |
| INV-EC5 | Migrated saved loops load, trace, and wrap pure | `scene_loop_roundtrip.rs` + `scene_loop_roundtrip_gate.rs` extended: a pre-corridor fixture (count/stride/jitter_period shape) loads → trace finds all three atoms → exposures are exactly the new whitelist → wrap-parity gate green on the migrated graph. Negative gate: zero `count`/`jitter_period`/`stride` param hits in the migrated def. |

## 5. Phasing

Entry state for every phase: re-verify the section 1 anchors it touches.
Forbidden across all phases: reintroducing a copies/window-size card row
(the window is derived, D5) · a second source of truth for `pattern_length`
beyond the plan builder + coupled secondary + migration (D3) · gating on a
human looking at a PNG (every gate is a computed number) · dual param
semantics for migration (D7) · touching per-object mesh modifier chains
(the curated-list trap, unchanged from SCENE_LOOP).

- **P1 — The windowed atom.** Deliverables: section 3.1 surface
  (scene_array), section 3.2 deltas (loop_camera), WGSL body, stasis key,
  capacity constant; `gpu_tests` value proofs (CPU oracle: window placement
  at multiple base_cells, `(cell mod P)` jitter parity across crossings,
  mask beyond `behind + ahead + 1`); the D8.1 exact-seam pixel gates at four
  (K,P) shapes; INV-EC1 CPU grid test, INV-EC2/EC3 unit tests.
  Gate: new gates green; `scene_loop_wrap_parity.rs` reds first on a
  deliberately raw-index-keyed jitter (one-frame source change, run, revert —
  the red-first protocol); gpu-proofs suite green.
  Round-trip: none (no serialized change yet — the old param names still
  exist in the manifest). Test scope: `manifold-renderer` nextest + lib
  gpu_tests; clippy `-p manifold-renderer`.
  Demo: none — L1 (atom-level phase; the observable surface arrives in P2).
- **P2 — Plan builder, card surface, migration.** Deliverables: section 3.3
  (plan builder deltas, whitelist, coupled writes, `migrate_fixed_row_scene_loops`
  + exposure-row drop, both load call sites); `scene_loop_roundtrip` /
  `scene_loop_roundtrip_gate` migration extensions (INV-EC5); D8.2 unclipped
  near-seam gate; ui-snap flow `scene-setup-loop.json` re-run (Pattern row
  visible, writes land, Stride row writes patterns_per_loop).
  Seam brief (section 6 applies to the param renames): old → new written out
  in section 3.3; call-site inventory = the two whitelist entries, the two
  coupled-write entries, the mint params, and the migration fns — the
  compiler plus the INV-EC5 negative gate are the migration; there is no
  parallel old path.
  Gate: gates green; negative `rg` (zero `stride`/`count`/`jitter_period`
  hits in migrated fixtures + zero in the whitelist/coupled tables);
  round-trip gate green.
  Demo: ui-snap flow — L3. Performer gesture: dial Pattern 1→4 mid-set — the
  corridor's variety changes, the wrap stays invisible (asserted by D8.1 at
  the new shape). Test scope: `manifold-renderer` + `manifold-editing`
  nextest, `manifold-app` compile/clippy (ui-bridge card consumers), ui-snap
  flow.
- **P3 — Acceptance on the reference projects + supersession sweep.**
  Deliverables: D8.3 Stone Effects v1/v2 headless acceptance script
  (`headless_content_thread` + per-tick GeneratorRenderer readback, the
  BUG-b6iv repro pattern) run pre/post migration; `rt_noise_gate.py` +
  gpu-proofs on corridor shapes; RT corridor proof at speed (INV-EC4);
  migration of the two reference projects verified in-app.
  Gate: v1 blip metric at baseline across wrap-adjacent ticks; v2 no flash;
  RT gates green; D8.1/8.2 green on the migrated graphs.
  Demo: the acceptance numbers table + wrap-adjacent tick plots — L2 (Peter
  reads the numbers; the real-import visuals remain his in-app run, the
  BUG-bgcr (headless-harness-cannot-light-real-imports) lighting caveat unchanged).
  Test scope: `manifold-renderer` + `manifold-app`; landing via the standard
  protocol. **Supersession sweep (same session):** SCENE_LOOP_DESIGN.md
  status header and D2/D10/Deferred entries annotated as revised-by-this-doc;
  BUG-ejeq (scene-loop-endless-corridor-redesign) closed;
  BUG-aepq (scene-loop-far-edge-hole-fog-default) closed (moot per its own
  description); BUG-b6iv verified-fixed or re-pointed with its metric
  outcome; BUG-nkxg (scene-loop-copies-gate-VD) folded into the P3 acceptance
  (its pixel-on-real-import gap is D8.3's deliverable).

## 6. Decided — do not reopen

1. The corridor extends `node.scene_array` in place; no new primitive (D1, 2.5 audit).
2. Camera arrives as a `Camera` port; no scalar-position wires (D2).
3. Stride = patterns_per_loop, travel = K·P·cell, purity by construction for any integers K,P (D3).
4. jitter_period is gone; jitter keys on `cell mod pattern_length` (D4).
5. The window derives from camera.far per frame; capacity is the constant 32; no window/copies card row ever (D5).
6. Migration upgrades old params at load by exact arithmetic (J | S under shipped couplings); no dual semantics (D7).
7. Acceptance = wrap-parity pixel gates + Stone Effects v1/v2 as held-out inputs (D8).
8. BUG-cb2k (scene-loop-seam-cut-under-live-param-modulation) (live-modulation seam cuts) is out of scope; the design leaves the two-clause purity predicate as its hook (section 3.5).

## 7. Deferred

- **Live-modulation seam-cut warning** (BUG-cb2k (scene-loop-seam-cut-under-live-param-modulation) option a) — trigger: a binding to a loop row exists in a shipped project and Peter wants the guard. Hook: section 3.5 predicate.
- **Frustum culling for instances** — trigger unchanged from SCENE_LOOP (vertex cost at real copy counts); the corridor's fixed 32-slot window makes this LESS urgent, not more.
- **Mirror tiling** — trigger unchanged (negative-scale winding verification).
- **Per-cell material variation** — trigger unchanged; modulo-tiled cells share materials by construction.
- **RT beyond corridor correctness** — the
  BUG-326 (rt-depth-snapshot-wrong-on-imported-glb-scenes) precedent (RT +
  imported-scene traps) stands; corridor work does not extend RT to shapes RT
  doesn't already handle.
