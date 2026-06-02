# Node-Id Targeting — bindings reference identity, not name

## STATUS: COMPLETE (2026-06-02)

Landed on `node-graph-system` (commits `ba0c9f4f` → `265b7b68`). The cutover is done: bindings
address inner nodes by stable `NodeId`, the handle is display-only, and the handle-resolution path is
gone. No runtime fallback anywhere; the only legacy handling is a versioned, unit-tested **load
migration**.

**The one convention that ties it together:** *a node's `node_id` defaults to its handle.* The
bundled-preset stamp uses it (`nodeId == authoring handle`), the load-time override normalization uses
it, and the tolerant `BindingTarget` deserialize uses it — so a handle-targeted binding from an old
document lands on exactly the node that normalizes to the same id.

**What shipped:**
- Foundation (additive): `NodeId(Arc<str>)`; `node_id` on every `EffectGraphNode` (minted at creation,
  preserved by `flatten`); runtime `NodeInstance.node_id` + `Graph::instance_by_node_id`.
- Resolution: `BindingTarget::Node{node_id,param}`; `ParamTarget::Node{node_id,param}`;
  `from_static`/`from_user` resolve via a per-effect `(NodeId,NodeInstanceId)` `node_map`;
  `loaded_preset_view`, `persistence::into_graph`, `json_graph_generator`, `snapshot.node_id`.
- Per-instance: `UserParamBinding.node_id` (+ load-only `legacy_node_handle` shim that captures the old
  `nodeHandle` key for migration, skip-serialized once cleared).
- Editor expose flow: `ToggleNodeParamExpose` / `ToggleEffectParamExpose` carry node_id; def-side
  exposure helpers key by node_id; readable `user.<handle>.<param>.<n>` ids still minted from the
  handle. PanelAction / `GraphEditorNodeView` / `resolve_canvas_binding` carry/match node_id.
- Removed the obsolete `EffectNodeAliasMetadata` / `legacy_node_aliases` / `node_aliases` /
  `resolve_user_param_binding_node_handles` handle-rename machinery.
- Presets: all 46 stamped (`nodeId == handle`, minimal-diff string edit). `check-presets` 46/46.

**Load migration (versioned, deterministic, no fallback) — the convention applied at four points:**
1. `BindingTarget` Deserialize accepts legacy `handleNode` → `Node{node_id == handle}`; serialize only
   ever emits `node`/`composite`.
2. `graph_loader::instantiate_def` (the **runtime chokepoint** every def→graph path funnels through —
   effect splice AND generator `into_graph`) defaults a node's runtime `node_id` to its handle when the
   document carries none. This is what makes resolution hold for ANY def, including hand-authored JSON
   loaded via `from_json_str` that never went through `Project` normalization.
3. `Project::normalize_override_node_ids` (core `on_after_deserialize`) stamps `node_id == handle` on
   every override-def node with an empty id (master/layer/clip `graph` + `generator_graph`, recursing
   into groups) — for doc-level persistence + editor snapshots. Idempotent.
4. `migrate_user_param_bindings_to_node_id` (renderer, at project open) copies the resolved id onto
   each `UserParamBinding` whose `legacy_node_handle` is set — pure read off the normalized graph /
   stamped preset.

**Tests:** flatten `grouping_prefixes_handle_but_preserves_node_id` (the bug); core
`normalize_override_node_ids` + `legacy_handle_node_target_deserializes_as_node_keyed_by_handle`;
renderer `binding_migration` (graph:None / override / already-migrated / unresolved). Verified
`graphTests.manifold` (V2 zip, 22 `handleNode` targets in overrides) deserializes and every binding
target resolves. manifold-core 219/219; renderer lib 945/2 (both failures pre-existing).

**Full-sweep triage (vs base `f3627fe1`):** the cutover introduced exactly ONE regression —
`generator_binding_scale_folds_into_inner_param` (un-stamped `from_json_str` JSON couldn't resolve;
fixed by point 2 above). Every other sweep failure reproduces at base with byte-identical messages and
is untouched by this diff (zero `.wgsl` / factory / harness changes): FluidSim Ableton mapping + param
counts, DoF prewarm, WireframeDepthGraph first-frame `copy_texture` 42×42→256 panic (the open legacy
effect), GeneratorFactory inventory in the parity binary, lut1d / watercolor parity, wgsl
`WEIGHTING_MODE` / `simplex3d`. Landed `ba0c9f4f` → `53f64832`.

**Why the override case mattered:** the first real regression (`graphTests.manifold`) had override
preset bindings in `handleNode` form but **zero** user bindings — so the renderer migration's
"any user binding needs migrating?" gate would have skipped it. That's why node-id normalization is a
core `on_after_deserialize` step (runs on every load), not folded into the renderer user-binding pass.

## Why

Card sliders bind to an inner graph node by its **handle** string. A binding resolves the handle to a
runtime node by exact match against the *flattened* graph (`ResolvedBinding::from_static` /
`from_user`, `param_binding.rs`). Grouping a node prefixes its handle at flatten time
(`blur` → `softgroup/blur`, `flatten.rs`), so the stored handle no longer matches and the slider goes
dead. The handle is the one piece of a node's identity that grouping changes — so bindings must stop
keying off it.

Every other entity in the model already references identity by a stable id: `LayerId` / `EffectId` /
`ClipId` are short-UUID `Arc<str>` newtypes (`manifold-core/src/id.rs`). Graph nodes are the only thing
addressed by a mutable name. This change brings them in line.

## The change (one atomic cutover — no fallback, no transitional path)

- New `NodeId(Arc<str>)` newtype in `id.rs` (same macro as the others). Minted once at node creation;
  **never** changes under group / ungroup / move / flatten.
- `EffectGraphNode` gains `node_id: NodeId`. `handle` stays, demoted to a display/search name with no
  addressing role.
- Bindings target `NodeId`:
  - `ParamTarget::HandleNode { handle }` → `ParamTarget::Node { node_id }` (preset / static bindings).
  - `UserParamBinding.node_handle` → `UserParamBinding.node_id` (per-instance user bindings).
  - The flatten/splice step builds a `node_id → runtime NodeInstanceId` map; the resolver matches by
    `node_id` and **only** by `node_id`. There is no handle path left in the resolver.
- Flatten carries each source node's `node_id` onto the runtime node it produces, so the map exists at
  build time at every nesting depth.
- Group / ungroup / move commands preserve `node_id` and touch **zero** bindings. The whole reason for
  the change: grouping stops needing to know bindings exist.

The old handle-resolution path is deleted in the same change. No "resolve by id, else by handle"
fallback ever exists — that two-path ambiguity is exactly what we're removing.

## Migration (versioned, deterministic, unit-tested — runs once at load)

This is a load-time schema upgrade, like any version bump. It is **not** a runtime fallback.

1. **Stamp the bundled presets.** Every node in every `assets/effect-presets/*.json` and
   `generator-presets/*.json` gets a stable `nodeId` baked into the JSON (one deterministic pass,
   committed). These ids are the canonical identity for each preset's nodes forever.
2. **Bump the document version.** Files below the new version trigger the upgrade on load.
3. **Upgrade old files deterministically:**
   - Graph-less instances (`graph: None`): for each binding, look up the node whose handle matches in
     the now-stamped bundled preset → take its `node_id` → write it onto the binding. Deterministic
     because the bundled preset is fixed.
   - Per-instance overrides (`graph: Some(def)`): assign `node_id`s to the override's nodes in a
     deterministic order, resolve each binding's handle against that def, write the id. Baked on next
     save.
   - `EffectNodeAliasMetadata` (the existing handle-rename map) feeds the handle lookup so already-
     renamed handles still resolve.
4. After upgrade, save writes the new version with ids; the upgrade never re-triggers.

## Tests (the change is fully unit-testable)

- `id.rs`: NodeId round-trips through serde (transparent).
- flatten: a grouped def's inner node keeps its `node_id` in the flattened graph at depth ≥ 1.
- resolver: a binding targeting a `node_id` resolves to the right node whether that node is at root or
  nested inside one/many groups.
- migration: load an old-format fixture (handle-targeted) → assert every binding now targets the
  correct `node_id` and drives the same inner param it did before. Run against the canonical
  `Liveschool` and `Burn` project fixtures end to end.
- grouping: group a node that drives a card slider → the slider still drives the same inner param
  (the bug this whole change fixes), with working undo; ungroup restores.

## Touch points (coordinate — this lives with the binding model, not around it)

`manifold-core`: `id.rs`, `effect_graph_def.rs` (EffectGraphNode), `effects.rs` (UserParamBinding),
the loader/migration. `manifold-renderer`: `flatten.rs` is core, but the node_id→runtime map +
`param_binding.rs` (ParamTarget / ResolvedBinding / resolution) are the binding-model surface the
other agent just reworked. `manifold-editing`: the expose command writes id targets; group/ungroup/
move preserve ids. `manifold-app`: editor expose UI. The `assets/*-presets/*.json` stamping pass.
