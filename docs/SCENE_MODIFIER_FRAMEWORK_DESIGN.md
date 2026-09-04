# Scene Modifier Framework — 3D scene behaviors as first-class cards

**Status:** IN PROGRESS — P1 + P2 LANDED 2026-09-04 (framework core + loop-as-kind + scene_fog generality proof; gates 7/7) · P3 (inspector cards) next · k3 (lead)
**Prerequisites:** SCENE_LOOP P1–P4 (on main — the loop is this framework's first kind), WIDGET_TREE_DESIGN P1–P5 (the `ParamSurface` card layer), SCENE_PANEL_EXPOSURE_CONVERGENCE (scene rows are card rows). All on main; nothing unbuilt.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

A scene modifier is to a 3D scene what an effect is to a 2D layer: a named,
carded, triggerable behavior the performer adds, tunes, enables, and removes
without seeing the graph. Scene Loop was the prototype; this design generalizes
it into a framework where every modifier kind is a descriptor (plan builder +
trace + row curation + bypass declaration) hosted by ONE generic apply/remove
command pair, with cards in the inspector riding the same `ParamSurface`
machinery as 2D effects. The audit found the unification is mostly already
true: scene exposures are real bindings, scene rows are real card rows, scene
writes ride the same `ChangeGraphParamCommand`, and modulation/OSC/Ableton
address scene params today. What is genuinely new is small: a modifier
registry, a generalized plan/command pair, a modifier list derived by trace,
and the inspector card host.

Peter's directives that shaped this design, verbatim:

- "I think we should think of the 'scene modifiers' as post processing effects
  but instead of 2D textures, it's 3D scenes. Clip triggered behaviours, etc,
  effect cards, full inspector."
- "there's likely things in the 3D scenes that don't need ordering or order
  doesn't matter? Maybe they have fixed slots and thigns that CAN reorder are
  cards."
- "We MUST get this infrastructure correct though, the infra is critical for
  any of this to work well. The effect cards and inspectors took awhile to
  nail but they're decent now. You must learn and reuse as much as possible,
  no parallel paths, no sepearte dual architecture, no hackjob bandaid designs."
- "This includes the UI and UX systems for the cards and inspector. It must
  all be modular and easy to work with." (2026-09-04)
- Standing, from SCENE_LOOP: "This stuff should be done at the 'scene panel'
  level. The graph editor is waaayy too granular for this type of
  modification." — cards and panel, never graph-editor reading required.

On stage: the imported corridor/forest/scan becomes an instrument layer —
loop it, fog it, stack behaviors, map them to MIDI, trigger them from clips —
with the same muscle memory as dropping Bloom on a layer. When the infra is
wrong it looks like: a modifier that applies but shows no card, a card whose
slider writes the wrong layer (BUG-292 (scene-panel-wrong-layer-target)'s class), a removed modifier leaving a
dangling mapping, or two surfaces showing the same rows that drift apart.

Companion docs: `docs/SCENE_LOOP_DESIGN.md` (the loop's behavior contract —
wrap purity, gap rule, invariants; absorbed here as kind #1, its atom/trace
contract unchanged), `docs/WIDGET_TREE_DESIGN.md` (the card layer + section 5b (agent contract) this design leans on), `docs/GROUPING_GRAPHS.md` (group
mechanics the splices use), `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md`
(the exposure machinery cards ride), `docs/ADDING_PRIMITIVES.md` (the one new
primitive in this design).

---

## 1. Audit — what exists (verified 2026-09-04)

Verified by lead read of the modifier path, the card layer docs, and two
read-only recon lanes (clip-trigger mechanism, 2D effect card data model).
Anchors are the authority; a moved anchor at execution time is an escalation.

### The scene modifier path today (Scene Loop = the template instance)

| Piece | Where | State |
|---|---|---|
| Plan data (core) | `manifold-core/src/scene_loop.rs:35` (`SceneLoopPlan`) | Nodes/wires/splices/metadata as plain core fields — the renderer-builds/editing-applies split. **The shape to generalize.** |
| Plan builder (renderer) | `gltf_import/scene_loop.rs:29` (`assemble_scene_loop_plan`) | Computes cell_size from `scene_bounds` (D4 gap rule), finds lens/repoint target by type, fans out per-group `instances` splices, curates the 4-row whitelist (`loop_row_label` :240). **One fn per kind is the authoring model.** |
| Apply/remove commands | `manifold-editing/src/commands/graph/scene_loop.rs:37,270` | Composite, level-snapshot undo, inverse-of-plan removal, exposure stripping by node_id. **Replaced by the generic pair (D1); semantics preserved.** |
| Structural trace | `scene_vm.rs:1149` (`trace_scene_loop`) | All-or-nothing on three (type_id, nodeId) pairs. **Generalizes to a signature table (D3).** |
| Exposure stamping | `manifold-core/src/scene_exposure.rs:174` (`stamp_scene_node_exposures_into`) | Mints `ParamSpecDef` + `BindingDef{target: Node{node_id, param}}` per curated param, `section = "Scene Loop"`, idempotent by binding target (the D11 lesson). Per-node manifests only (INV-6). |
| Scene row writes | `ui_bridge/project.rs:996` (`apply_scene_param_write`) | Row → binding id → `ChangeGraphParamCommand` (the SAME command the 2D generator card uses) with `SetGraphNodeParamCommand` fallback. **Fully unified already.** |
| Scene panel rows | `scene_setup_panel.rs:1705` (`build_filtered_properties`) | `ParamSurface` rows filtered by `spec.section`, written at `GeneratorOf(layer_id)` (BUG-292 (scene-panel-wrong-layer-target) net). Scene rows ARE card rows — convergence landed. |
| Panel section UI | `scene_setup_loop.rs` | Hardcoded widget keys (pre-WIDGET_TREE vintage), one section, three states. **Superseded by inspector cards (D4).** |
| Apply dispatch | `ui_bridge/project.rs:337` | Plan built app-side from the live project, command executed locally + sent to content thread. The mutation pattern all modifier dispatches copy. |

### The 2D effect model (the UX target)

| Piece | Where | State |
|---|---|---|
| Instance storage | `manifold-core/src/effects/instance.rs:35` (`PresetInstance`) | `enabled`, `params: ParamManifest` (id-keyed), drivers/envelopes/ableton/audio_mods/automation, `graph: Option<EffectGraphDef>`. Lists: `Layer.effects`, `Clip.effects`, `Settings.master_effects` — Vec order = chain order. |
| Effect commands | `manifold-editing/src/commands/effects.rs` | `AddEffectCommand:28`, `RemoveEffectCommand:68`, `ReorderEffectCommand:111`, `ToggleEffectCommand:164`, `ChangeGraphParamCommand:255` (by ParamId, both kinds), `ToggleEffectParamExposeCommand:518` (ReverseState prunes dangling drivers/mappings on un-expose — the removal-prune precedent). |
| Card surface | `manifold-ui/src/param_surface.rs:104` (`ParamSurface`) | One projection (`ui_bridge/projection/cards.rs:193`), `ParamRow{id, spec, value, modulation, mapping}`, `RowRole`, `RowIndex`. `SurfaceVisibility::{CuratedCard, All}` (:28). |
| Card host | `manifold-ui/src/panels/param_card/` + `panels/inspector/` | `ParamCardPanel` chrome (enable toggle, badges, cog, collapse, delete-collapse) + `InspectorCompositePanel` scope-keyed card vecs, `reconcile_cards` config-driven. **Modifier cards join this host (D4).** |
| Modulation attachment | `effects/instance.rs:774` (`append_user_binding`) | Scene exposures become manifest params (origin `UserAdded`) keyed by binding id — drivers/envelopes/Ableton/OSC/audio-mods address them with no curation. **Works today.** |

### Clip triggering (recon lane, verified 2026-09-04)

| Piece | Where | State |
|---|---|---|
| Phantom clips | `manifold-playback/src/live_clip_manager.rs:87,355,681` | NoteOn → live slot only; NoteOff commits via `AddClipCommand` only when recording; 5ms guard `:40`. |
| Envelope path | `manifold-core/src/effects/envelope.rs:30`, `playback/src/modulation.rs:414` | `ParamEnvelope` on any manifest param — **including scene exposures**; rising edge armed per-frame, writes effective value directly. Caveat: timing reads `layer.clips` only — **live MIDI phantom clips never arm envelopes** (timeline clips do). Engine-wide gap, logged as a bead (D7). |
| Graph trigger stream | `generator_renderer.rs:503,105` → `PresetContext.trigger_count` | `clip_count + audio_count` fed to the graph as `system.generator_input.trigger_count`, bumped on every clip start incl. live slots. FluidSim2D's "Clip Triggers" group (`trigger_gate → envelope_decay → sample_and_hold`) is the authored-wiring reference. |
| Bypass atoms | `node.value` (`value.rs:22`), `node.math` (`math.rs:35`) | Exist. The fog enable/amount math (D5) is one wire away. |
| Camera mux | — | **Does not exist** (`rg 'camera_switch\|mux_camera' crates/` → 0). One genuinely new primitive (D5, section 3.5 (node.camera_switch)). |

Classification: **exists** — exposure stamping, ParamSurface rows, the write
command, modulation addressing, phantom clips, the trigger-count stream, the
card host, the composite-command pattern. **One wire away** — a generic
plan/command pair generalizing the loop pair; a signature-driven trace
generalizing `trace_scene_loop`; the camera mux; fog bypass math on existing
atoms. **Genuinely new** — the modifier descriptor registry, the inspector
modifier-card region + add-modifier picker, the enable/bypass declarations.

Negative claims, checked: no stored modifier list anywhere
(`rg 'scene_modifier|modifier_list' crates/manifold-core` → 0); no camera-typed
mux/switch primitive (surveyed all 8 camera-family type_ids); no per-frame
work introduced by this design (all mutations are edit-time commands; the
runtime cost of an applied modifier is the atoms it already mints).

---

## 2. Decisions

- **D1 — A modifier kind is a renderer-side descriptor; one generic command
  pair serves all kinds.** Descriptor: `{ kind_id, display_name, slot_group,
  plan_builder, trace_signature, row_whitelist, applicability, enable_decl }`
  (section 3.1 (The modifier descriptor)). The plan builder is plain Rust with the full graph API — the
  same authoring model as `assemble_scene_loop_plan` — composing existing palette
  atoms; the framework supplies splice helpers (group fan-out, port repoint
  with restore, exposure stamping, bypass wiring). Commands:
  `ApplySceneModifierCommand` / `RemoveSceneModifierCommand` consume the
  descriptor-produced `SceneModifierPlan` (section 3.2 (The plan)) — semantics
  byte-identical to the loop pair they replace.
  Rejected: per-kind command types (N copies of one mechanism; the
  never-phase-by-family rule).
  Rejected: JSON-declarative splice templates. 2D presets are JSON because
  they are whole self-contained graphs; a modifier is a *delta* on a live
  graph — computed constants (cell_size from `scene_bounds` ×2), conditional
  structure (lens fallback), per-group iteration. Expressing deltas
  declaratively is a new mini-language — a new identity/dispatch system, which
  the zero-new-systems test forbids. The atoms remain JSON-expressible
  primitives; only the splice logic is Rust, one small fn per kind.
  **Consequences, stated honestly:** a kind is code, so a performer-authored
  modifier is not v1 (Deferred, trigger named); and the registry is a
  compile-time set — kinds cannot be added at runtime.

- **D2 — Fixed slots; the modifier list is DERIVED from the graph, never
  stored.** Slot order = registry order within a canonical slot-group order
  (v1: Camera, Atmosphere — section 3.1 (The modifier descriptor)). Presence, order, and identity all come from
  the structural trace; enable is a real graph param (D5). There is no
  `preset_metadata.modifiers` list.
  Rejected: a stored modifier list. A list is a second source of truth next
  to the graph — after a hand-edit deletes a modifier's node, or the
  flattener renumbers ids, the list and the graph disagree, and reconciling
  them on load is exactly the translation layer D11's incident documented.
  The loop's own history is the proof: every desync class (BUG-237 (scene-setup-camera-world-light-param-scrub), the
  double-stamp) came from a stored copy drifting from the derived truth.
  **Consequences, stated honestly:** "order" is a build-time constant — a
  kind that genuinely needs performer-controlled order forces a reopened
  decision (Deferred, trigger named). Slot-group exclusivity (one kind per
  group applied at a time) is the price of no reorder; it is also the honest
  v1 semantics, since no two v1 kinds compose.

- **D3 — Identity = declarative trace signature, all-or-nothing.** Each kind
  declares `&[TraceNode { type_id, node_id, required }]`; the generic trace
  (generalizing `trace_scene_loop`, `scene_vm.rs:1149`) returns applied /
  not-applied plus doc ids for writes. Partial (hand-deleted node) = not
  applied — same honesty as the loop's v1 state; the card disappears, orphan
  exposures stay inert and invisible (`card_visible = false` for loop atoms by
  the default-deny table, `scene_exposure.rs:61`) until Remove strips them by
  node_id.

- **D4 — Modifier cards are ordinary card surfaces; the inspector is their
  only home.** Rows come from the existing exposure machinery filtered by the
  kind's `section` string (= display name) — the exact mechanism the scene
  panel uses (`build_filtered_properties`). Cards render in the inspector's
  layer scope below the scene's generator card through `reconcile_cards`, reusing
  `ParamCardPanel` chrome minus the drag handle (fixed slots). Row writes use
  the existing scene write path (`SceneSetupParamChanged` →
  `apply_scene_param_write`), addressed by `scene_vm::ParamAddr`
  (`scene_vm.rs:95`). Badges (DRV/ENV/ABL) and modulation drawers work
  unmodified because the rows are ordinary manifest rows.
  Rejected: a new card type or row machinery — INV-8 (`no_bespoke_row_infra`)
  bans it, and nothing here needs it.
  Rejected: keeping the Scene Setup panel's Scene Loop section as a parallel
  surface. One modifier, two renderings, is the transcription disease this
  repo has paid for five times (WIDGET_TREE's audit). The panel keeps scene
  *content* (camera/world/lights/objects); modifiers live on inspector cards
  only. Peter's "full inspector" directive decides the home.

- **D5 — Enable is a stamped toggle driving kind-declared bypass wiring,
  applied at apply time; toggling is a param write.** Two bypass families:
  **switch** (camera-path kinds): the apply mints a `node.camera_switch`
  (section 3.5 (node.camera_switch)) between the previous camera producer and the lens; `enabled` →
  `select`. Off = the original camera, seamlessly, with zero structural
  churn. **gate** (value kinds — fog): `enabled` and amount are `node.value`
  atoms multiplied by a `node.math` Mul into the target param
  (`atmosphere.fog_density`); the Enabled row is an `is_toggle` row on the
  enabled value atom, Density a slider on the amount atom. Both families:
  toggle = one undoable param write, no graph rebuild, survives save/load as
  ordinary params.
  **Consequences, stated honestly:** this deliberately DIVERGES from 2D
  effect enable, which is structural elision (`ToggleEffectCommand` → topology
  hash → rebuild; a disabled effect never enters the plan). A scene-graph
  rebuild per toggle would churn mesh/trace state for zero visual benefit —
  the param write is the cheaper and correct-shaped mechanism here. Executors:
  do not "fix" the divergence toward 2D semantics.
  Rejected: enable-by-structural-remove (toggle would churn the graph,
  invalidate the trace mid-off, and make "off but configured" unrepresentable
  — an effect card's OFF never deletes the effect).
  Rejected: v1 without enable (2D card parity is the point of the exercise).

- **D6 — Scene Loop migrates to kind `scene_loop` with zero wire change.**
  nodeIds (`loop_phase`/`scene_array`/`loop_camera`), the `"Scene Loop"`
  section string, node params, and graph shape stay byte-identical; old
  projects load and trace unchanged. The loop-specific command pair and
  `trace_scene_loop` are deleted (compiler-driven migration, section 3.3 (Generic commands + the loop seam) seam brief).
  Commands are runtime-only (never serialized) — no project migration. All
  SCENE_LOOP invariants (INV-1..6, wrap purity) remain green through the
  generic path; that is P1's central gate.

- **D7 — Triggering rides existing machinery; nothing new is built for it.**
  Exposures are manifest params: envelopes, drivers, Ableton, OSC, audio-mods
  address modifier rows today (lane-verified) — **while the modifier is
  applied**. Scoping, stated plainly: on a tracking instance (`graph: None`,
  a freshly imported model before apply) the synthesized-id translation
  resolves no binding (BUG-249 (scene-panel-modulation-is-decorative-synth-pids-…)'s shape), so
  modulation arms are dead until apply stamps the bindings — after apply the
  graph is `Some` and everything addresses normally. The graph `trigger_count`
  stream reaches modifier atoms when a kind's plan wires it (a kind-level
  choice per row, e.g. a future loop "retrigger" — Deferred). The one gap —
  live MIDI phantom slots don't arm decay envelopes (timing reads
  `layer.clips` only) — is engine-wide, not modifier-specific; it gets a
  `bug` bead at P1 and is out of scope here.

- **D8 — The framework lands before the loop's control enrichment.** P1–P3
  build the infra with the loop's existing four rows; P4 adds the movement
  controls (Flow/Stride/Sway/Jitter/Spacing) as atom params + whitelist rows
  once rows are data. Sequencing the enrichment after the framework means
  each new control is a ParamDef + one whitelist line + gates, never panel
  code — the modularity Peter asked for is exercised by P4, not just claimed.

---

## 3. Design body

### 3.1 The modifier descriptor (committed signature)

New module `crates/manifold-renderer/src/node_graph/scene_modifier.rs`.
Registered via `inventory::submit!` (the primitive-registration convention —
one file per kind, no central edit). Kind ids are public API once shipped:
`"scene_loop"`, `"scene_fog"`.

```rust
/// One scene modifier kind. Renderer-side (reads primitive manifests and
/// scene_bounds; builds plans against a live EffectGraphDef).
pub struct SceneModifierDescriptor {
    /// Stable kind id — public API forever (D6 makes "scene_loop" the first).
    pub kind_id: &'static str,
    /// Card title, exposure section string, picker label.
    pub display_name: &'static str,
    /// Fixed-slot group; card order = SLOT_GROUP_ORDER, then registry order.
    pub slot_group: SlotGroup,
    /// Build the apply plan against the CURRENT graph. None = not applicable
    /// (the picker greys the kind; apply refuses).
    pub plan_builder: fn(&EffectGraphDef, render_scene_node_id: u32) -> Option<SceneModifierPlan>,
    /// Applicability pre-check for the picker (cheaper than plan_builder;
    /// may be plan_builder itself for cheap kinds).
    pub applicable: fn(&EffectGraphDef, render_scene_node_id: u32) -> bool,
    /// Identity: required/optional (type_id, nodeId) pairs, top level.
    pub trace: &'static [TraceNode],
    /// Which stamped params become card rows (None = full manifest).
    pub row_whitelist: Option<&'static [(&'static str /*node_id*/, &'static str /*param*/, &'static str /*label*/)]>,
    /// How the enable toggle wires (D5).
    pub enable: EnableDecl,
}

pub enum SlotGroup { Camera, Atmosphere, Objects, Lights, Environment }
/// Canonical card/picker order; v1 uses Camera, Atmosphere.
pub const SLOT_GROUP_ORDER: &[SlotGroup] = &[Camera, Atmosphere, Objects, Lights, Environment];

pub struct TraceNode { pub type_id: &'static str, pub node_id: &'static str, pub required: bool }

pub enum EnableDecl {
    /// Camera-path kinds: apply mints node.camera_switch between the previous
    /// producer of the repointed port and the modifier's camera.
    /// The toggle row targets `select` on the named node_id.
    Switch { node_id: &'static str /* e.g. "loop_cam_switch" */ },
    /// Value kinds: toggle + amount node.value atoms multiplied into
    /// `target_param` on `target_node_id`. Toggle row → `enabled_node`,
    /// amount row(s) → `amount_node`.
    Gate { enabled_node: &'static str, amount_node: &'static str,
           target_node: &'static str, target_param: &'static str },
}
```

### 3.2 The plan (committed signature)

New module `crates/manifold-core/src/scene_modifier.rs` — crate-neutral data,
same role as `scene_loop.rs:35`. The descriptor's builder produces one of
these per apply; remove re-derives it from the current graph.

```rust
pub struct SceneModifierPlan {
    pub kind_id: String,
    /// Card title == exposure section string.
    pub display_name: String,
    pub new_nodes: Vec<EffectGraphNode>,
    pub new_wires: Vec<EffectGraphWire>,
    /// Per-object-group interface splices (generalizes InstanceWiring).
    pub group_splices: Vec<GroupSplice>,
    /// Port take-overs with declarative restore (generalizes the lens.camera
    /// re-point): apply drops other producers of (node, port); remove re-wires
    /// the first non-mine producer of `restore_types` back in.
    pub repoints: Vec<PortRepoint>,
    /// Per-node exposure curation (INV-6: each node its own manifest only).
    pub exposures: Vec<NodeExposure>,
    pub enable: EnablePlan,
}

pub struct GroupSplice { pub group_node_id: u32, pub inner_node_type: &'static str,
                         pub inner_port: &'static str, pub source_doc_id: u32, pub source_port: String }
pub struct PortRepoint { pub target_node_id: u32, pub target_port: String,
                         pub new_producer_doc_id: u32, pub restore_types: &'static [&'static str] }
pub struct NodeExposure { pub node_doc_id: u32, pub node_id: NodeId, pub type_id: String,
                          pub params: BTreeMap<String, SerializedParamValue>,
                          pub metadata: Vec<SceneParamMetadata> }
pub struct EnablePlan { pub toggle: ToggleDecl, pub extra_nodes: Vec<EffectGraphNode>,
                         pub extra_wires: Vec<EffectGraphWire> }
pub enum ToggleDecl {
    /// Switch: row targets this node param directly.
    NodeParam { node_doc_hint: NodeId, param: String, on: f32, off: f32 },
    /// Gate: row targets the enabled value atom's `value` param.
    ValueAtom { node_id: NodeId },
}
```

Builder helpers the framework provides (so kind fns stay small): `fan_out_to_object_groups`,
`repoint_camera_port`, `stamp_exposures`, `wire_switch_bypass`, `wire_gate_bypass`.

### 3.3 Generic commands + the loop seam (seam brief)

New: `manifold-editing/src/commands/graph/scene_modifier.rs` —
`ApplySceneModifierCommand` / `RemoveSceneModifierCommand`, semantics
byte-identical to `commands/graph/scene_loop.rs:37,270` (level-snapshot undo;
apply = extend nodes/wires + repoint (drop displaced producers) + splices +
stamp exposures; remove = drop by stable node_id, restore repoint, strip
splices, strip exposures by node_id, `refresh_target_manifest`).
**Remove prunes three layers, not two** (K3 review major 1): the graph +
`preset_metadata` are stripped as today, AND the remove command prunes
(a) `PresetInstance.params` `UserAdded` entries whose `BindingDef` was
stripped (`refresh_manifest_from_graph` does NOT prune — `build_param_manifest`
re-pushes UserAdded params from their wire spec, `instance_serde.rs:376`),
and (b) any drivers/envelopes/Ableton mappings targeting removed ids —
`ToggleEffectParamExposeCommand`'s ReverseState pattern (`effects.rs:518`).
This prune is the general fix for a gap likely live on main today (the loop
remove leaves orphan manifest params); P1 verifies and files the bead.

Old → new, written out:
- `ApplySceneLoopCommand{target, scope_path, plan: SceneLoopPlan, catalog_default}`
  → `ApplySceneModifierCommand{target, scope_path, plan: SceneModifierPlan, catalog_default}`.
- `RemoveSceneLoopCommand{target, scope_path, plan}` → `RemoveSceneModifierCommand` (same shape).
- `trace_scene_loop` → generic `trace_modifier(kind, level)`; `SceneLoopInfo` →
  `ModifierVm { kind_id, doc_ids, enabled }` inside `SceneVm.modifiers: Vec<ModifierVm>`.

Call-site inventory (re-derivation command, run at execution time —
`rg -n 'ApplySceneLoopCommand|RemoveSceneLoopCommand|trace_scene_loop|SceneLoopInfo|SceneLoopRow' crates/`):
the two dispatch arms (`ui_bridge/project.rs:337,362`), the panel section
(`scene_setup_loop.rs` — deleted at P3, not adapted), the VM field consumers,
and the editing round-trip tests. Compiler-driven migration: rename the old
symbols first; red is the checklist. Deletion gate: `rg 'ApplySceneLoopCommand|RemoveSceneLoopCommand' crates/` → 0.

### 3.4 VM: `SceneVm.modifiers`

`SceneVm` gains `modifiers: Vec<ModifierVm>` — one entry per REGISTRY kind,
in slot order, applied or not (never filtered out: the inspector renders
"not applied" kinds only inside the add-picker, not as dead cards).
`ModifierVm { kind_id, display_name, applied: bool, doc_ids: AHashMap<&str, u32>,
enabled: Option<bool>, multiple_scenes_blocked: bool }`. `enabled` is read
off the enable toggle's node param at VM build (the state is the graph's, not
a UI flag — the wrap-debug lesson, `scene_setup_loop.rs:105`).

### 3.5 The one new primitive: `node.camera_switch`

Section 2.5 audit statement: camera-family primitives surveyed —
`node.orbit_camera`, `node.free_camera`, `node.look_at_camera`,
`node.loop_camera`, `node.camera_lens`, `node.draw_particles_camera`,
`node.flatten_to_camera_plane` — all are camera *sources* or consumers; none
selects between two `Camera` inputs (`rg 'camera_switch|mux_camera' crates/` →
0). Nearest reference preset: the loop's own camera re-point
(`SCENE_LOOP_DESIGN.md` section 3.1 (What the apply-command inserts)). Finding: **genuinely new — one wire away from
impossible without it** (enable-by-restore would need structural churn per
toggle, D5's rejection). Verdict: one composable CPU atom, same family as
`node.loop_camera` (NonGpu, Terminal).

```rust
crate::primitive! {
    name: CameraSwitch, type_id: "node.camera_switch",
    inputs: { a: Camera optional, b: Camera optional },
    outputs: { out: Camera },
    params: [ select: Enum ["A", "B"] ],
    // select=A → pass `a` through; B → `b`; unwired input falls back to the
    // other. CPU-only; Camera is a value type (like loop_camera's outputs).
}
```

### 3.6 Fog kind (`scene_fog`) — the generality proof

Applicability: exactly one render_scene (INV-1) AND no existing producer wired
to `render_scene.atmosphere` (else refuse — the picker greys it). Plan: mint
`node.atmosphere` ("fog_atm", wired `out → render_scene.atmosphere`),
`node.value` atoms `fog_enabled` (1.0) and `fog_amount`, `node.math` Mul
(`fog_enabled × fog_amount → fog_atm.fog_density` via the wire). Enable: Gate
decl. Rows (whitelist): Enabled (toggle), Density, Color (as stamped), Height
Falloff. Remove: drop minted nodes/wires, strip exposures; nothing to restore
(applicability guaranteed no prior atmosphere). Wrap purity: fog is static
per-cell in v1 — no loop-phase driver (Deferred).

### 3.7 Inspector card region + picker

In the inspector's layer scope, below the scene generator card: modifier cards
in slot order (applied kinds only), then a full-width "+ Add Modifier" button
(`add_effect_button_view()` is the chrome precedent, `inspector/mod.rs:85`).
The picker lists all registry kinds: applied → disabled; inapplicable →
disabled with reason; else clickable. Card chrome: title, collapse chevron,
enable toggle (switch kinds) / enabled toggle row (gate kinds put it in the
rows), remove × (with delete-collapse animation, existing), badges. NO drag
handle. Wrap-debug (loop) moves to a card-chrome button with the
stash-resume logic relocated from `SceneLoopUi` to the card state
(`param_card/state.rs` is the home; interior free).

Modifier card rows come from a NAMED adapter — `modifier_surfaces()` beside
`gen_params_to_surface` (`ui_bridge/projection/cards.rs:426`, which already
takes `SurfaceVisibility` at :431): one All-visibility projection of the
layer's generator surface → section filter per kind → per-row `ParamAddr`
join resolved from `preset_metadata.bindings` (`BindingDef.target =
Node{node_id, param}`) joined to the trace's `doc_ids` map. The inspector
host keeps a
`modifier_surfaces: Vec<(kind_id, ParamSurface, Vec<ParamAddr>)>` built by that
adapter — one projection, one row truth, no lane-invented producer.

### 3.8 Loop kind migration

Descriptor for `scene_loop`: same three atoms, same params, same
`loop_row_label` whitelist moved into `row_whitelist`, trace = the three
required nodes, enable = Switch (the apply additionally mints
`loop_cam_switch`: previous camera producer → `a`, `loop_camera` → `b`,
`out → lens.camera`; the old direct `loop_camera → lens.camera` wire is
replaced by the switch path). One wire-shape change on the loop's graph at
apply time (new node + two wires replacing one) — old projects that predate
the switch are handled by an **automatic-at-load migration** (precedent
`migrate_scene_exposures`, `scene_exposure.rs:367`): when the trace finds the
loop WITHOUT a switch, load mints the switch and re-points through it, once,
through the same generic command machinery — never a manual "migrate" button
(a hidden pre-migration enable state would be a second surface). Downgrade
honesty: an old binary opening a switch-node project sees an unknown node
type — normal forward-only serialization, stated plainly. Wrap-debug park
(bars=0) semantics unchanged.

### 3.9 Triggering

Nothing new (D7). Rows expose E/D/ABL/A chrome through the existing RowMod
facts. A performer can: map a modifier row to Ableton/OSC/MIDI today; arm a
decay envelope on a row (timeline clips); a kind may wire
`generator_input.trigger_count` into its atoms for live-clip reactivity
(authored per kind, Deferred for v1 kinds). The phantom-envelope bead is
filed at P1.

---

## 4. Invariants & enforcement

| # | Invariant | Enforcement |
|---|---|---|
| INV-M1 | Trace is all-or-nothing per kind, AND the trace set == the minted-node set (every `new_nodes` + enable-extra node is traced) | Unit over fixture graphs per kind: full → applied; delete any required node → not applied (`trace_modifier_*` tests). The set-equality half is a kind-authoring rule (P2 finding): an untraced minted node re-mints its stable nodeId onto surviving debris at re-apply, past INV-M9's trace-only refusal — the fog kind's four-node trace (`fog_mul` included) is the reference |
| INV-M2 | Apply/remove are exact inverses across THREE layers: graph + `preset_metadata` + `PresetInstance.params`/modulation vecs | Property test per kind: apply → remove → `flatten`-equal graph AND no manifest params carrying the kind's section AND no drivers/envelopes/mappings targeting removed ids (the round-trip gate pattern, `scene_loop_roundtrip.rs`, extended per K3 review major 1) |
| INV-M3 | Stamped rows == kind whitelist exactly — no atom internals leak | Whitelist-exactness test: after apply, the section's `ParamSpecDef` ids == the curated set, count-matched (catches the pre-P4 duplicate-Axis class) |
| INV-M4 | Modifier row writes land on `GeneratorOf(owning layer)` | Dispatch-level test + ui-flow (the INV-5/BUG-292 (scene-panel-wrong-layer-target) net, extended) |
| INV-M5 | No bespoke row infra | Existing `no_bespoke_row_infra` allowlist scan — new files land in it automatically |
| INV-M6 | One `node.render_scene` per looped/modified graph | Carried from INV-1; apply refuses (existing test pattern) |
| INV-M7 | Enable toggle = exactly one param write; no structural diff | Test: graph (nodes, wires) equal before/after toggle; undo restores value |
| INV-M8 | Old projects (loop without switch) still trace + migrate once, automatically at load | Load-migration test on a pre-switch fixture graph (`migrate_scene_exposures` precedent) |
| INV-M9 | Apply refuses on a PARTIAL trace — any trace node present but not all (hand-edit debris) | Test: delete one loop node → apply attempt refused with "remove the broken modifier first"; no nodeIds duplicated (K3 review major 2 — otherwise re-apply re-mints nodeIds that surviving debris already carries) |

---

## 5. Phasing

Entry state for every phase: re-verify the section 1 anchors it touches.
Forbidden across all phases: a stored modifier list (D2) · per-kind command
types (D1) · a second rendering surface for modifier rows (D4) · synthesized
row ids (INV-5/8) · gating on anyone looking at a PNG (all gates numeric or
exit-code; Peter 2026-09-02).

### P1 — Framework core + loop-as-kind (engine; no UX change)

- **Entry:** SCENE_LOOP P4 on main (status line says so); anchors `scene_loop.rs:35`, `gltf_import/scene_loop.rs:29`, `commands/graph/scene_loop.rs:37`, `scene_vm.rs:1149` re-hit.
- **Read-back:** this doc section 1 (Audit)–section 3.3 (Generic commands + the loop seam), section 3.8 (Loop kind migration); restate D1–D3, D6, the seam brief, and the entry findings.
- **Deliverables:** `manifold-core/src/scene_modifier.rs` (plan types); `manifold-renderer/src/node_graph/scene_modifier.rs` (descriptor, registry, generic trace, builder helpers, `scene_loop` descriptor); `node.camera_switch` primitive (section 3.5 (node.camera_switch); NonGpu — no gpu_tests scope, per ADDING_PRIMITIVES' exemption for barrier-free CPU atoms — verify the scope-test line at impl); generic `ApplySceneModifierCommand`/`RemoveSceneModifierCommand` (editing) with the THREE-LAYER remove prune (section 3.3 — manifest params + drivers/envelopes/mappings, ToggleEffectParamExposeCommand-style); the seam brief executed (section 3.3 (Generic commands + the loop seam), compiler-driven, deletion gate); `SceneVm.modifiers` (loop trace behind the generic signature); the automatic-at-load pre-switch migration (section 3.8); apply-side partial-trace refusal (INV-M9); tests: INV-M1 (loop), INV-M2 (loop, three-layer), INV-M3, INV-M6, INV-M7, INV-M8, INV-M9, layer-duplication round-trip (duplicate the layer, both traces independent); the phantom-envelope `bug` bead (BUG id into the title); **verify-and-bead the live-on-main gap**: does today's `RemoveSceneLoopCommand` leave orphan `PresetInstance.params` after remove? (K3 review evidence says yes — `instance_serde.rs:376` re-pushes UserAdded params from wire spec); the bead carries the P1 fix as its fix shape.
- **Gate (positive):** every existing SCENE_LOOP gate green untouched (wrap parity, round-trip, INV-1..6 nets, e2e import); new INV-M tests green; `cargo nextest run -p manifold-editing -p manifold-renderer` + `cargo clippy -p manifold-renderer -p manifold-editing -p manifold-app -- -D warnings`. **Gate (negative):** `rg 'ApplySceneLoopCommand|RemoveSceneLoopCommand' crates/` → 0.
- **Acceptance demo:** none — **L1** (engine refactor; the loop's visible behavior is unchanged, pinned by the untouched suite + a wrap-parity number reported, not looked at). **Performer gesture:** none this phase.
- **Forbidden moves:** adapting the old commands instead of deleting them · changing the loop's node params/whitelist · wiring the switch into the apply without the load-migration arm.
- **Test scope:** touched-crate nextest + clippy (editing, renderer, app).

### P2 — Fog kind (engine; generality proof)

- **Entry:** P1 landed; `rg 'ApplySceneLoopCommand' crates/` → 0.
- **Read-back:** section 3.6 (Fog kind); restate D1 (one fn per kind), INV-M1–M3.
- **Deliverables:** `scene_fog` descriptor + builder (section 3.6 (Fog kind) exact); applicability = INV-1 + no existing atmosphere **+ slot-group free** (a same-group applied kind — e.g. a second camera-path modifier — makes the picker grey it and apply refuse; the applicability fn receives the applied-state map, K3 review minor 6); fog bypass stamps `ambient_tint` explicitly at 1.0 (atmosphere's neutral default, `atmosphere.rs:88` — pinned so a future default change can't tint the bypass); Gate bypass wiring on `node.value`/`node.math`; tests: INV-M1/2/3 for fog (INV-M2 rides P1's three-layer prune); applicability refusals (existing-atmosphere graph → None; multi-scene → None; same-group-applied → None); remove-with-driver-on-exposure → no dangling driver (pinned by INV-M2's three-layer test).
- **Gate (positive):** new tests green + P1 suite green. **Gate (negative):** `rg 'scene_fog' crates/manifold-ui` → 0 (no UI in P2 — the generality proof is engine-side).
- **Acceptance demo:** none — **L1**. **Performer gesture:** none (no surface yet).
- **Forbidden moves:** any card/panel code · wiring a loop-phased fog driver (Deferred).
- **Test scope:** editing + renderer.

### P3 — Inspector modifier cards (the vertical slice)

- **Entry:** P1+P2 landed; `param_surface.rs`, `panels/param_card/`, `panels/inspector/`, `scene_setup_loop.rs`, `ui-flows/scene-setup-loop.json` read.
- **Read-back:** section 3.7 (Inspector card region + picker); restate D4/D5; the WIDGET_TREE section 5b (agent contract) recipe (rows are added by data, never by widget code); the BUG-252 (eight-scene-flow-scripts-dead-at-step-2-on-stale…) flow-accounting rule.
- **Deliverables:** modifier card region in the inspector layer scope (section 3.7 (Inspector card region + picker)); `modifier_surfaces()` projection adapter (named above); the VM Gate arm — `ModifierVm.enabled` for gate kinds reads the `fog_enabled` value atom at VM build (P1 ships the Switch arm only; P2 finding); add-modifier picker; enable toggle + remove × chrome; wrap-debug relocated to card chrome; `SceneSetupPanel`'s Scene Loop section + tree entry deleted; new `scripts/ui-flows/scene-setup-modifier.json` (the `scene-setup-*` prefix is what maps the flow to the `gltfscene` fixture in `scripts/ui-flows/manifest.json` — a differently-named flow matches no rule and runs nothing, the BUG-252 (eight-scene-flow-scripts-dead-at-step-2-on-stale…) class; select scene layer → add modifier via picker → rows visible → edit a row → assert write landed on `GeneratorOf(layer)`; enable toggle; remove) with `path_triggers` row for every new file; the old `scene-setup-loop.json` removed (replaced, count-match accounted); INV-M4 test + flow. P3 tripwires from the P2 generality proof: `node.value` rows are `card_visible = false` under the default-deny table (`scene_exposure.rs:61`) — the modifier adapter surfaces rows by SECTION, never by `card_visible` (they would vanish); the Scene Fog rows' is_toggle comes from kind curation on the stamped metadata, not the manifest (P2's option-(a) ruling).
- **Gate (positive):** the new flow exits 0; the whole `scripts/ui-flows/` manifest count-matches (BUG-252 (eight-scene-flow-scripts-dead-at-step-2-on-stale…) rule); existing inspector/card suites green; `cargo xtask ui-snap gltfscene --script scene-setup-modifier.json` exit 0 with the scripted asserts. **Gate (negative):** `rg 'SceneSetupApplyLoop|SceneSetupRemoveLoop|scene_setup_loop' crates/` → 0 (P3 deletion gate); `rg 'scene_loop' crates/manifold-ui/src/panels` shows no `format!`-built param ids (INV-6 net).
- **Acceptance demo:** the L3 flow + a PNG byproduct (Peter looks; no agent gates on it). **Affordance legibility:** the + button, picker entries, toggle, and × are all chrome-rendered (distinguishable as clickable in the PNG by construction). **Performer gesture:** mid-set, add a loop to the playing scene layer, drag Bars, toggle it off — all without opening the Scene Setup panel.
- **Forbidden moves:** rendering modifier rows in the Scene Setup panel too · a drag handle · bespoke row widgets · leaving the old flow file on disk.
- **Test scope:** ui + app (+ renderer/editing compile).

### P4 — Loop controls enrichment (the "interesting" upgrade)

- **Entry:** P3 landed; `loop_camera.rs`, `scene_array.rs`, `scene_array_body.wgsl`, `ADDING_PRIMITIVES.md`, `FREEZE_COMPILER_MAP.md` read. Wrap-safety math (all phase-periodic, D8/SCENE_LOOP INV-3):
  - **Flow** — `travel = phase − A·sin(2π·phase)/(2π)`: equal slope at both seams by construction. ParamDef range pinned **0.0..0.95** — A ≥ 1 reverses seam velocity (position purity survives, the motion kinks).
  - **Stride** — integer cells per loop; travel `K·cell`. Coupled to copies: travel `K·cell` outruns the instance array unless count scales — Stride's dual-write command writes `stride` AND `scene_array.count` (`K + 2`: behind + current + ahead) in one undoable unit, the same single-source pattern as Spacing.
  - **Sway** — lateral/height offsets += amp · sin(2π·cycles·phase), **integer cycles only** (param is whole_numbers).
  - **Look sweep** — look target lateral sweep, integer cycles.
  - **Zoom pulse** — fov_y += amp · window(phase) (sine window, zero at seams).
  - **Jitter** — `scene_array` per-instance rotation/scale from a deterministic hash of the index (no time dependence — trivially wrap-safe).
  - **Spacing** — ONE command writing BOTH `cell_size` params (the single-source write path D6 owed; a `SetGraphNodeParamCommand` batch, undoable as one unit — same batch pattern as Stride's stride+count write).
- **Read-back:** this doc D8; SCENE_LOOP_DESIGN section 4 (Invariants & enforcement) INV-3; restate the seam-slope constraint.
- **Deliverables:** new params on `node.loop_camera` (flow, stride, sway_amp, sway_cycles, look_sweep_amp, look_sweep_cycles, zoom_pulse_amp) and `node.scene_array` (jitter_seed, jitter_amount); `scene_array` WGSL body extension (freeze-path atom — `wgsl_body` stays the runtime kernel; gpu_tests value proof vs CPU-computed expected, fused-vs-unfused per CLAUDE.md); whitelist rows for all of the above (performer labels: Flow, Stride, Sway, Sway Rate, Look Sway, Zoom Pulse, Jitter, Spacing); Spacing dual-write command.
- **Gate (positive):** wrap-parity gate EXTENDED: phase-0 vs phase-0.99999 pixel diff == 0 with flow=0.8, sway amp>0 cycles=2, look sweep, zoom pulse live (the INV-3 gate must see red on a deliberately broken driver before green); gpu_proofs suite for the touched atoms (`scripts/gpu_proofs_gate.py`); value tests vs CPU reference for jitter; performer-gesture gate: bars 8→16 mid-playback stays position-continuous (phase-continuity assert).
- **Gate (negative):** `rg 'create_compute_pipeline' crates/manifold-renderer/src/node_graph/primitives/scene_array.rs` → 0.
- **Acceptance demo:** numeric (wrap-parity diff == 0 reported, copies-gate diff > threshold) — **L1+L3 flow re-run** (the rows edit through the P3 cards; the flow asserts one new row write). **Performer gesture:** dial Flow up mid-set — the flight eases through the drop without a seam.
- **Forbidden moves:** any non-integer cycles param · a Spacing row that writes only one cell_size · non-windowed audio-reactive coupling (not in this phase).
- **Test scope:** renderer (gpu-proofs via the gate script) + editing; app clippy.

### P5 — Trigger surfacing + debt burn

- **Entry:** P4 landed.
- **Deliverables:** verify E/D/ABL/A chrome on modifier rows end-to-end (flow driving an envelope arm on a modifier row); burn the phantom-envelope bead or carry it explicitly; close the supersession sweep (status lines, doc cross-refs, memory index).
- **Gate:** flow green; sweep `rg 'Scene Loop' docs/ + memory` hits all current. **Demo:** L3. **Performer gesture:** arm an envelope on Fog Density from a timeline clip; fire the clip.

---

## 6. Decided — do not reopen

1. Descriptor registry + one generic command pair; kind = Rust plan builder composing palette atoms (D1).
2. Fixed slots, slot-group exclusivity, list derived by trace — no stored modifier list (D2).
3. Trace identity, all-or-nothing (D3).
4. Inspector-only card home; Scene Setup panel keeps content, loses modifiers (D4).
5. Enable = stamped toggle + kind-declared bypass (switch/gate); toggle is a param write (D5).
6. Loop migrates wire-compat; old projects trace + one-time migrate (D6).
7. Triggering reuses existing machinery; the phantom-envelope gap is a bead, not this design's scope (D7).
8. Framework before enrichment (D8).

## 7. Deferred

- **Performer-authored / user-saved modifiers** — trigger: Peter authors a splice he wants to reuse; would build a save format over the plan types.
- **Dynamic modifier reordering** — trigger: a shipped kind that genuinely composes with another in the same group and Peter asks for order control.
- **Multiple instances of one kind** (per-object loops) — trigger: per-object modifier scoping request.
- **Mirror-tiling kind** — trigger: winding-flip instance flag lands (carried from SCENE_LOOP).
- **Loop-phased fog / audio-reactive windowed params** — trigger: Peter wants fog breathing with the loop (the window math is in P4's design body).
- **Live-trigger wiring for v1 kinds** (`trigger_count` → e.g. loop retrigger row) — trigger: a concrete live use.
- **RT compatibility** — trigger: RT path audit for `instances_n` (carried from SCENE_LOOP D9).
- **Frustum culling** — trigger: P4 demo timing shows behind-camera vertex cost matters.
