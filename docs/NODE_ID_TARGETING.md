# Node-Id Targeting — bindings reference identity, not name

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
