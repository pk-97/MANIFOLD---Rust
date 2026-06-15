# Editor Reorg — Build Brief (autonomous session, 2026-06-01)

<!-- index: Historical build brief for the 2026-06 three-lane editor reorg (shipped). Superseded by GRAPH_EDITOR_UX_BUILD_BRIEF for current work. -->

> **Historical (as of 2026-06-13).** The three-lane layout this brief built has
> shipped. For the editor's current state and the remaining UX work, see
> `docs/GRAPH_EDITOR_UX_BUILD_BRIEF.md`. Kept for the reuse map and the affine-fold
> decision record.

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

- **WS2a — DECIDED: the editor card IS the card, with a sideways mapping drawer.**
  Peter's call after seeing the read-only mirror + right-click popover. Two
  settled decisions:
  - **(1) Not a mirror — the actual card.** Render the edited effect's real
    `ParamCardPanel` in the left lane, configured from the same `EffectInstance` as
    the timeline card. There is no separate "mirror" data and no sync to keep — both
    cards read the one `EffectInstance`, so they ARE the same card by construction.
    This is also what dissolves the "3000-line panel" reuse worry: we don't extract
    or reimplement anything, we *instantiate the whole working panel in a second
    spot*, and its event routing comes along because it's the same panel handling
    its own events. `ParamCardPanel` already draws both effects and generators
    (`ParamCardKind`). Build its `ParamCardConfig` from the edited effect by reusing
    the main-window builder in `ui_bridge/state_sync.rs` (keyed on the editor's
    `current_editor_target` ei). Slider drag already emits
    `EffectParamSnapshot`/`EffectParamChanged(ei, ParamId)` — the correct card write
    path (NOT inner-node `SetGraphNodeParam`, which the binding overwrites each
    frame). Suppress / swap the card-header chrome that makes no sense inside the
    editor (drag-reorder, the "open graph editor" cog).
  - **(2) Drawer direction split — settled.** *Drivers and modulators* (how the
    param MOVES — LFO, envelope, beat-sync) keep opening **DOWN** (the existing
    `build_driver_config` / `build_envelope_config` vertical drawers, unchanged).
    *Control params* (how the value MAPS — range, scale, offset, invert, curve) open
    **SIDEWAYS** (horizontal), so peeking at a param's mapping never reflows the
    vertical slider stack. Two axes of metadata on two axes of the UI. Reuse the
    `EffectMappingAffine*` emit + the `MappingPopover` scale/offset controls already
    built, but as a side-anchored drawer off the row, not a detached right-click
    popover. Open affordance: a subtle chevron at the row's right edge (NOT
    right-click — Peter flagged it). Suppress the affordance in perform mode so a
    show can't fat-finger it open. Prototype in the editor first (the canvas gives
    room to open right); the timeline (card at screen edge) is a later call.
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

**Per-affine analysis (FluidSimulation.json) — COMPLETE 2026-06-01, both folds shipped + visually confirmed:**
an affine folds iff its `.a` is a card binding target AND it has one consumer AND no other wired inputs.
- **FOLDED ✓** `rotation_rad_base` (id 24) — Curl binding → `rotation_final.a` + `scale 0.017453293`
  (deg→rad). Shipped (commit 74468be2; re-applied after the generator-scale fix below).
- **FOLDED ✓** `particle_count_calc` (id 20) — count_m binding → `active_count_calc.a` + `scale 1000000`
  (card 2.0 → 2M). Single consumer (wire 20→21.a), no other inputs. Byte-identical default 2.0×1e6 = 2e6
  matches the static default. Shipped (commit c4c8b819).
- **KEEP — canvas-responsive, NOT a single-value affine (brief CORRECTED here):** the blur chain
  `blur_radius_x_width/height` (60/62) → `blur_h/v_radius_final` (61/63). The blur radius is
  `canvas_dim × feather × (1/1280)` — `blur_radius_x_*.a` is **wired to the live canvas width/height**
  from `generator_input`, so it's a two-input computation, not a single-value affine. Folding feather into
  a binding scale would bake in 1080p and break the blur at other resolutions. The earlier "scale 1/1280,
  foldable" note was WRONG.
- **KEEP — derived (wire-fed `.a`, not a card target):** `scaled_energy_calc` (a ← active_count_calc),
  `intensity_calc` (a ← canvas_area_scale), `noise_z` (a ← time).

**The generator-scale gotcha (the real lesson, 2026-06-01):** the first Curl fold *broke on stage*. The
generator runtime (`json_graph_generator.rs`) hand-rolled its own `ResolvedBinding` with `reshape: None`,
silently dropping the binding scale — 72° went in as 72 **radians** (57× over-drive, unstable vortices).
The effect path was always fine; only generators were broken. Fixed by converging BOTH paths onto one
constructor (`ResolvedBinding::assemble` / `assemble_affine`) sharing one `scale_offset_reshape`, plus a
generator regression test (`generator_binding_scale_folds_into_inner_param`). The generator no longer has
its own binding literal, so this bug class cannot recur. **Any future preset fold rides this now-correct
path** — but note the lesson: a fold's gates (check-presets + execute) prove load + run, NOT that the
runtime applied the scale. Verify the value reaches the inner param (the regression test does), then
Peter's eyes.

FluidSimulation's fold campaign is **DONE**: Curl + particle count folded, everything else correctly kept.

## Editor card rebuild — status (2026-06-01, post-A.2)

The graph-editor left lane now renders the REAL `ParamCardPanel`, interactive and
target-correct by identity. Shipped:

- **A.1** (commit ed8e8273): real card in a widened 340px lane (the dead
  `GraphCardMirrorPanel` deleted). Configured from the edited target via
  `state_sync::editor_card_config` — effect via `current_editor_target`,
  generator via `watched_graph_target`, one surface.
- **A.2** (commit 5d40066c): card is interactive. Pointer events map to the
  card's node-id methods. Edits resolve by IDENTITY via an additive
  `editor_override: Option<&EditorDispatchTarget>{tab, active_layer}` threaded
  through `dispatch → dispatch_inspector` (None = perform path, byte-identical,
  adversarially confirmed). Configure-gated on a config hash so drags/drawers
  survive. Clip-effect guard bails to an empty lane (no Clip variant in
  `EffectTarget`) — full clip support is step 3.

**NEXT = B: the sideways mapping drawer + `CardContext`.**
- Add a `CardContext` (Perform / Author) to `ParamCardPanel`. Author mode (the
  editor) shows the sideways control-param drawer and SUPPRESSES the cog
  ("open graph editor" — you're already in it), drag-reorder, and the
  perform-mapping label-right-click menu. Perform mode (inspector) is unchanged.
- Control params (range / scale / offset / invert / curve) open SIDEWAYS
  (horizontal), reusing the `EffectMappingAffine*` emit + the `MappingPopover`
  scale/offset controls; chevron affordance at the row's right edge (NOT
  right-click). Drivers + modulators keep opening DOWN. This is where the
  hardened scale/offset binding path gets its on-card surface.
- B also absorbs the deferred A.2 nits: the editor label-right-click menu
  (inert today; either suppress in Author mode or wire it with the override),
  and optionally a lighter configure-gate than the Debug-hash.

**DEFERRED (own pass): step 3** — migrate the inspector/perform path to the same
identity targeting and delete `EffectTarget` + the ambient resolution + the
clip guard + the `Effect*/Gen*` action fork. Full spec + grep-able done-criteria
in `docs/CARD_TARGET_UNIFICATION.md`; fork sites carry `CARD-TARGET-UNIFICATION`.
This is the workflow-shaped one (broad sweep + adversarial verify on the perform
path).

## Step 3 — status (2026-06-01): foundation built + stashed, app dispatch pending

Grounding the command layer corrected the design (the spec's "delete `EffectTarget`
entirely" was wrong) and split it into stages — see the rewritten
`docs/CARD_TARGET_UNIFICATION.md` (§ "Correction", "The migration (step 3)").

- **Stage A — editing layer: DONE, in `git stash@{0}`** (`git stash show -p
  stash@{0}` to restore). Compiles as a lib. Single-effect commands
  (`ToggleEffect`, `ChangeEffectParam`, both `Toggle*Expose`, `EditUserParamBinding`)
  + `DriverTarget::Effect` now take `EffectId` and resolve via
  `find_effect_by_id_mut` (reaches master/layer/clip); list commands keep
  `EffectTarget`; `Project::layer_id_for_effect` added for the envelope-cleanup
  reach. NOT committed — it can't build the workspace until Stage B converts the
  app call sites, and the editing-layer change forces them all in one atomic
  landing.
- **Stage B — app dispatch: NOT done.** ~25 `Effect*`/`Gen*` arms in
  `inspector.rs` + the `app_render` `EffectMapping*` arms convert to id-based
  commands; `editor_override{tab,active_layer}` → `editor_target: GraphTarget`
  (dispatch by identity, not ambient shadow); `current_editor_target` dropped for
  `watched_graph_target`; clip guard removed; `input_host.rs` + two test files
  fixed. Must land atomically and pass the adversarial perform-byte-identical gate
  before commit (the live mutation gateway — verify, don't YOLO).
- **Stage C — `Effect*`/`Gen*` enum collapse: optional final purity.**

Checkpointed here on purpose: the foundation is proven, the design is corrected +
precise, and the app-dispatch rewrite is a focused atomic block best done with a
full runway rather than half-landed under budget pressure.

**Scope expansion (2026-06-01): generator editable bindings fold into Step 3.**
The sideways mapping drawer is effect-only today because `GeneratorParamState`
has no `user_param_bindings` (generator exposure just flips an `exposed_params`
flag — no binding object to remap), so `mappable` is correctly false on
generator rows. Peter: generators "1000% need this." Decided to fold the fix
into Step 3 (not a separate pass) so the binding command + mapping dispatch
generalize to a graph target ONCE. This REVISES Stage A: the stashed
`EditUserParamBindingCommand(EffectId, …)` becomes target-generic
(`Effect(EffectId) | Generator(LayerId)`). The UI is already unified (chevron
gated on `mappable`), so once `gen_params_to_config` sets `mappable: true` for
generator bindings the chevron lights up with no UI work. Full plan in
`docs/CARD_TARGET_UNIFICATION.md` § "Stage B+ — generator editable bindings".

**Widened again (2026-06-01): the fork is THREE-WAY** (Peter: "go for this").
The mapping drawer being user-binding-only revealed built-in effect params
(static `param_def` + a preset `BindingDef`, e.g. ColorGrade Amount/Gain) ALSO
lack a per-instance editable mapping, so stock-effect cards show no chevron
either. End-game: every exposed card param (built-in effect, user effect,
generator) is a first-class per-instance editable binding → chevron on every
row. **HARD INVARIANT — Ableton/OSC/drivers/envelopes stay byte-identical:** they
address by stable `param_id` → value slot and write the card value into the slot;
the mapping is a downstream reshape (never rewrites the slot, applied after the
modulation write). The unification must never move a `param_id`/slot and must not
duplicate a built-in param into a user binding. Adversarial gate checks per-frame
writes byte-identical for a fixture with Ableton+OSC+driver+envelope bound. Spec
§ "Stage B++" + "Verification".
