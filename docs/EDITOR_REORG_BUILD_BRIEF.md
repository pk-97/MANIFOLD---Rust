# Editor Reorg — Build Brief (autonomous session, 2026-06-01)

Working brief for the WS1/WS2/WS3 session. Pins every locked decision so neither
the main context nor any agent drifts. Peter authorized an autonomous run: make
the judgment calls here, log them in commit messages, surface only when the UI is
ready for his full visual pass-over.

## The target layout

The graph editor becomes three lanes plus an on-demand spawn menu:

- **Left column = effect-card mirror.** One row per *exposed* binding
  (`EffectInstance.user_param_bindings` + live `param_values`): friendly label +
  current live value + knob/slider. Rows are **compact by default**; the granular
  mapping meta (range trim min/max, invert, response curve) opens on a per-row
  expand/flyout. This is the same surface the real effect card performs from, with
  the mapping metadata the card hides exposed for authoring. Reuse the
  `mapping_popover.rs` controls (trim handles, INV button, curve dropdown).
- **Center = graph canvas.** Unchanged in role; gains the WS1 scissor clip.
- **Right column = selected-node inspector.** All params of the clicked node
  (label, value, range, tooltip), its ports, and the **expose toggle**. Ticking
  expose promotes the param into the left card. Right = "what this node offers,"
  left = "what I've promoted to my instrument."
- **Node browser = popup only.** Remove the permanent left palette column; its
  content already lives in the `browser_popup` Node mode. Spawn at cursor via the
  existing double-click **and** a new **Tab** shortcut (Spacebar is reserved for
  transport/play later).

## Invariants (do not violate)

1. **One source for node params.** The node face (`graph_canvas.rs` `NodeView`)
   and the right inspector (`graph_editor.rs` `GraphEditorNodeView`) both project
   from the single `ParamSnapshot`. Two views, one source — never fork the data.
2. **All mutations through `EditingService`.** Exposing, value edits, mapping
   edits route through `ContentCommand` → `Command` (e.g.
   `ToggleNodeParamExposeCommand`, `EditUserParamBindingCommand`). No direct model
   writes from UI.
3. **`param_values` + `user_param_bindings` are the live instrument.** The left
   column reads/writes the same model the card and drivers/Ableton/envelopes touch
   every frame; it updates live. Don't snapshot-freeze it.
4. **Wires render under panels.** After WS1 the canvas is scissored to its rect and
   the panels draw last over opaque backgrounds. Keep it that way for any new panel.

## Reuse map (build on these, don't reinvent)

- `crates/manifold-app/src/mapping_popover.rs` — trim/invert/curve controls for the
  left-column expanded rows.
- `crates/manifold-ui/src/panels/browser_popup.rs` — Node mode + alias search for
  the spawn popup (already built).
- `ParamSnapshot` (`node_graph/snapshot.rs`) — the single node-param source.
- `UserParamBinding` (`manifold-core`) + `ResolvedBinding` — the binding model
  (label/min/max/default/convert/invert/curve), already wired end to end.
- `PALETTE_WIDTH` / `SIDEBAR_WIDTH` (panels) — the lane widths.

## WS3 affine fold criteria (for the migration workflow)

An `affine_scalar` ("Scale + Offset (value)") node is **foldable** into a card
binding iff ALL hold:
- It sits directly between a single card-exposed param and a single inner-node
  param (a pure `out = in * scale + offset` remap), and
- it has exactly one consumer (no fan-out to other nodes), and
- it performs no other computation (no extra wired inputs).

Foldable → move `scale`/`offset` into the binding's `min`/`max` (and curve if the
mapping is non-linear), rewire the card binding straight to the inner param, delete
the affine node. **Keep** (do not delete) any affine that does real graph math, has
multiple consumers, or is one leg of a one-card-param-to-many-targets fan-out
(those stay as graph nodes; binding fan-out via shared `source_index` is a separate
question, not this pass). When in doubt, KEEP — a surviving node is harmless; a
wrong fold breaks a shipped look.

## WS3 finding (2026-06-01) — STOPPED at audit, NOT auto-applied

Grounding the actual presets before firing the migration changed the picture.
The `affine_scalar` nodes in the generators are **not** redundant card-mapping
passthroughs — they are real in-graph computation. In `FluidSimulation.json`:

- `rotation_rad_base`: scale `0.017453293` = π/180 — a **degrees→radians**
  conversion. Card `Curl` (85) wires to its `.a`.
- `particle_count_calc`: scale `1_000_000` — card `Particle Count` (2.0) → 2M.
- `blur_h/v_radius_final`: scale `1/1280` — pixel-radius normalization.
- `scaled_energy_calc`, `intensity_calc`: derived scaling.

Two blockers, both fatal to a blind fold:
1. By the fold criteria above these are **KEEP** (real graph math), not fold.
2. **`UserParamBinding` has no affine (scale/offset) transform** — only range
   (value passes through), invert, and curve. It literally cannot reproduce
   `out = a*scale + offset`, so even a "pure mapping" affine can't fold into the
   current binding model. Folding `rotation_rad_base` would either feed the
   consumer un-converted degrees (broken) or force the card to store radians and
   lose the friendly degree value, and would ripple to any driver/Ableton mapping
   on that card param.

**RESOLVED 2026-06-01 (Peter):** option (a) — extend `UserParamBinding` with
`scale`/`offset` and fold the card→consumer affines into the binding. Correcting
my earlier worry: there is **NO value-semantics ripple**. The card already stores
the friendly value (Curl 85°, Particle Count 2.0) and the affine sits *downstream*
of the card, so moving its scale into the binding leaves the stored card value and
every driver/Ableton/envelope write unchanged. It is byte-identical (copy the
affine's exact `scale`/`offset` into the binding). This unifies WS2's left card
mirror with WS3: same mapping surface. The affines only exist because the binding
couldn't remap yet.

Plan:
1. Add `scale`/`offset` to `UserParamBinding` (serde-default 1.0/0.0 = passthrough
   = every shipped binding byte-identical), applied at `ResolvedBinding::apply`
   after reshape, before wrap/convert, with an identity early-skip. Same shape as
   the shipped invert/curve.
2. Surface scale/offset in the left card-mirror mapping controls (WS2a).
3. Migrate **card → single-consumer** affines (deg→rad at a card, ×1e6 particle
   count, pixel-norm) into the binding's scale/offset; delete the nodes; verify
   per preset. KEEP affines that feed multiple consumers or derive from other
   computed values (the energy scalar) — genuine graph math.

The `node.convert` idea is **dropped** for card-boundary conversions (they're just
the binding's scale). It only returns if the audit finds a genuinely mid-graph
conversion (two computed values, no card between). None seen so far.

## Verify bar

- `cargo clippy -p <crate> --all-targets -- -D warnings` before each commit.
- `cargo run -p manifold-renderer --bin check-presets` after any preset JSON edit.
- `cargo test -p manifold-renderer --lib bundled_presets` after WS3 folds (GPU
  one-frame execute). The only acceptable red is the **known pre-existing
  WireframeDepthGraph** blit-size failure — anything else is a regression.
- Liveschool fixture (`Liveschool Live Show V6 LEDS.manifold`) must load + render
  byte-identical after WS3.

## Voice (any user-facing copy)

Natural, readable, professional (Ableton/TD/Resolume grade). No em-dashes, no
semicolons, no AI-speak tells, not choppy. Say what the control does and the one
gotcha that matters. See `feedback_product_copy_voice`.

## Autonomous-run protocol

- No check-ins. Resolve forks against this brief + sensible defaults; log the call
  in the commit message.
- Commit + push per milestone (durable authorization).
- The single human checkpoint is the final UI pass-over. Because the UI is built
  without seeing pixels, that pass may surface real layout/feel changes, not just
  polish — expected.

## Progress — 2026-06-01 (the layout shape is built; pass-over due here)

Shipped and pushed on `node-graph-system`:

- **WS1** (`38f65ed2`): canvas scissor clip — wires/nodes/labels sit under panels.
- **Binding scale/offset infra** (`027c8dd6`): `UserParamBinding.scale/offset`,
  folded into `Reshape`, byte-identical. The WS3-fold enabler.
- **Tooltips coverage** (`9161fcdd`): 131 nodes / 471 knobs, house-voiced, in
  `param_tooltips_bulk.rs`; catalog regenerated, drift guard green. (Not part of
  the reorg proper, but the same UX push.)
- **WS2 step 1 — card mirror in the left lane** (`c42ec6f5`): new
  `GraphCardMirrorPanel`. The node palette left the left lane (it lives in the
  spawn popup now, double-click); the lane shows the effect card's exposed params
  with live values, kind-formatted (deg / Hz / enum). Lane keeps the palette width
  so the canvas origin and coordinate mapping are untouched. **Read-only.**
- **WS2 step 2 — sidebar is the inspector only** (`aba85762`): dropped the
  duplicated card list from `GraphEditorPanel`'s top. Right = clicked-node
  inspector, left = card mirror, center = clipped canvas, palette = popup.

**Pass-over is due now.** The target three-lane shape is built and committable;
get Peter's reaction to the layout/feel *before* building the editable knobs on
top (building them first risks rework if the lane sizing / placement changes).
Launch the editor: open an effect → cog icon (`OpenGraphEditor`).

### Remaining (sequence after the layout pass-over)

- **WS2a editable mirror — DECIDED: reuse the effect card, not a popover.**
  Peter's call after seeing the read-only mirror + right-click popover: the rows
  should draw exactly like the effect card (slider, live value, trim inline), and
  there's reusable infra for it. The popover is the wrong surface for a panel.
  - **Plan:** render the edited effect's real `ParamCardPanel` in the left lane.
    `ParamCardPanel` handles both effects and generators (`ParamCardKind`), so it's
    one path. Build its `ParamCardConfig` from the edited effect by reusing the
    main-window builder in `ui_bridge/state_sync.rs` (keyed on the editor's
    `current_editor_target` ei). Route its press/drag/release through the panel's
    own handlers — slider drag already emits `EffectParamSnapshot`/`EffectParamChanged(ei, ParamId)`,
    the correct card write path (NOT the inner-node `SetGraphNodeParam`, which the
    binding would overwrite each frame). `build_param_row` + the trim/driver/envelope
    builders in `param_slider_shared.rs` are all `pub(crate)` — usable directly since
    the mirror is in `manifold-ui` — if a lighter row build is preferred over the
    whole panel.
  - Scale/offset (the affine fields) live in an expandable mapping drawer on the
    row, same pattern as the existing driver/envelope config drawers
    (`build_driver_config` etc.). `EffectMappingAffine*` + `MappingPopover` scale/offset
    already exist; reuse the emit, not the popover surface.
  - **Done already:** fan-out dedup (one row per `outer_param_id`).
  - The right-click `MappingPopover` stays only for the on-canvas node rows (a
    different, immediate-mode surface), or is dropped there later.
- **WS2c Tab shortcut.** Palette is already popup-only via double-click; add Tab to
  open it. The open block to replicate on a keypress is in `app_render.rs` ~660-748
  (`browser_popup.open(BrowserPopupRequest { mode: Node, … })` with item
  names/categories/type_ids from `palette_atoms_cache` + a center `graph_pos`).
- **Structural builds** (gate-verifiable, parity-tested): noise/blur/tone-map
  merges, multi-blend dynamic N-input, scale+offset label splits. NOTE: a merge
  reworks the just-shipped tooltips for the folded nodes (e.g. `reinhard_tone_map`
  → `tone_map`); update the bulk tooltip file when folding.
- **DEFERRED — degrees-everywhere.** Changing node param UNITS (radians→degrees on
  the node) re-means wired params and would double-convert with a fold (FluidSim's
  Curl already does deg→rad via an affine). Separate from the fold below; hold.

## The affine fold — scoped + exemplar validated (2026-06-01)

Peter's goal: replace the in-graph `affine_scalar` "user-mapping" nodes with the
card binding's `scale`/`offset`. **Infrastructure SHIPPED this session:**
runtime `UserParamBinding.scale/offset` (byte-identical), `BindingMappingEdit`
scale/offset + `EffectMappingAffine{Snapshot,Changed,Commit}` + dispatch, and the
`MappingPopover` Scale/Offset controls (usable now: right-click an exposed param
row on the canvas). The binding can now hold + edit the affine.

**What the fold still needs (the preset-load path must carry scale/offset, or a
folded preset feeds the consumer the UNSCALED value → broken look):**
1. `BindingDef` (`effect_graph_def.rs`) — add `scale` (serde default 1.0) / `offset`
   (default 0.0). Additive, every shipped preset stays byte-identical.
2. `binding_def_to_runtime` (`node_graph/loaded_preset_view.rs`) — carry scale/offset
   into the runtime `ParamBinding`, and confirm the `ParamBinding → ResolvedBinding`
   reshape applies them (the runtime path is distinct from `from_user`, which already
   does). This is the load-bearing correctness step — verify with a one-frame execute.

**Per-affine analysis (FluidSimulation.json):** an affine folds iff its `.a` is a
card binding target AND it has one consumer AND no other wired inputs.
- **FOLDABLE:** `rotation_rad_base` (id 24) — Curl binding → `.a`, scale `0.017453293`
  (deg→rad), single consumer `rotation_final.a` (node 47, `node.math`). Fold = retarget
  Curl binding to `rotation_final.a` + `scale: 0.017453293`, delete node 24, delete
  wire 24→47.a. Byte-identical (binding writes 85, reshape ×0.0174 = 1.4835 rad, same
  value the affine produced). `particle_count_calc` (×1e6) is the same shape.
- **KEEP (derived, not card→consumer):** `scaled_energy_calc` (a = computed 2e6),
  `intensity_calc` (a = computed). Their `.a` is NOT a binding target.
- Check blur radius + others case-by-case against the criterion before folding.

**Why surfaced, not auto-applied:** FluidSimulation is the load-bearing show fixture
(Liveschool). The fold is byte-identical BY CONSTRUCTION, but the GPU gates prove
execute, not look, and a wrong retarget/rewire breaks the show silently. Do the
loader change (safe), then exemplar-fold `rotation_rad_base`, verify (check-presets +
`bundled_presets` + Liveschool load), show Peter, then fold the rest.
