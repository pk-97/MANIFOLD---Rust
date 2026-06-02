# Node-Id Targeting — bindings reference identity, not name

## STATUS / RESUME (2026-06-02)

Work lives on branch **`node-groups`** in worktree `/Users/peterkiemann/MANIFOLD-node-groups` (off
trunk `node-graph-system` — do NOT land until the cutover is complete + fixture-validated). Rule:
**no silent fallback, one atomic cutover, one versioned unit-tested load migration** (see memory
`feedback_no_silent_fallbacks_or_interim_stopgaps`).

**DONE + tested (commits ba0c9f4f → d5b9c698), all ADDITIVE / behavior-preserving:**
- `NodeId(Arc<str>)` newtype in `id.rs` (same macro as LayerId/EffectId).
- `node_id` on `EffectGraphNode`, minted once at creation (group_edit group node + sentinels;
  `AddGraphNodeCommand` mints + reuses across undo/redo via `minted_node_id`), preserved by
  `flatten` (it clones nodes) and through every construction site.
- Runtime: `NodeInstance.node_id` set by `graph_loader::instantiate_def`; `from_graph` preserves it;
  `Graph::instance_by_node_id` is the global unambiguous lookup (node ids are unique uuids) — the
  successor to `node_id_by_handle`. Nothing calls it yet.

**REMAINING = the atomic cutover (the hard half):** Resolution moves off handle to node_id, handle
becomes display-only, the handle-resolution path is deleted. Sites (mapped, ~40 `node_handle`
refs + more):
- core `BindingTarget::HandleNode { handle, param }` → `Node { node_id, param }` (JSON
  `kind:"node"`, `nodeId`). `effect_graph_def.rs:527`.
- renderer `ParamTarget::HandleNode { handle }` → node-id variant; `from_static` / `from_user`
  (`param_binding.rs`) resolve via `graph.instance_by_node_id`, dropping the `handles` arg.
  Callers in `effect_chain_graph.rs` (~646, 663, 965).
- `loaded_preset_view.rs::binding_def_to_runtime` (~118) maps the new target.
- `into_graph` binding-convert validation + exposure seeding (`persistence.rs` ~595, ~650) match the
  new target.
- core `UserParamBinding.node_handle: String` → `node_id: NodeId` (`effects.rs:239`); ripples to
  `content_thread.rs:1294`, `app_render.rs` expose/mapping-drawer (~2097/3005/3047/3062/3145/3188),
  `commands/effects.rs` + `commands/graph.rs` expose flow, and `NodeSnapshot` (add `node_id` so the
  editor's expose checkbox can write it).
- the `EffectNodeAliasMetadata` handle-rename mechanism (`project.rs:621`) becomes obsolete — node_id
  is the stable identity, so the alias path can be removed (not kept as a fallback).
- **Preset stamp:** one-shot pass over the 46 `assets/{effect,generator}-presets/*.json` — stamp
  `nodeId` on each node + rewrite each binding target to `node`/`nodeId` matching by current handle.
  Cleanest as a throwaway bin (run once, commit) so it's deterministic.
- **Migration:** version bump; old-format files (handle targets, no node_id) upgrade once at load via
  v1 schema types → resolve handle→node_id deterministically → store. Unit-test with synthetic
  old-format defs HERE; then **run the `Liveschool` + `Burn` fixture tests from the MAIN checkout**
  (those `.manifold` files are gitignored / absent in this worktree) before landing on trunk.
- Final: group-a-bound-node test (the bug this fixes), full sweep, land on trunk.

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
