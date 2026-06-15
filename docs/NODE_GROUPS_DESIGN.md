# Node Groups — Implementation Spec

<!-- index: Implementation spec for node groups: the flattener as a pure EffectGraphDef transform, boundary nodes, constraints. The authoritative spec behind GROUPING_GRAPHS. -->

Status: **planned, not started.** Authored 2026-06-01 for a later ultracode build session.
Scope owner: Peter. Companion docs: `NODE_GRAPH_SYSTEM.md`, `EFFECT_RUNTIME_UNIFICATION.md`,
`PRIMITIVE_LIBRARY_DESIGN.md`.

---

## 1. What this is, and what it is deliberately not

A **node group** is a labeled box wrapping a slice of a graph: some inner nodes, a declared
interface (named inputs, named outputs, exposed params), and nothing else. It lets a complex
graph be tidied into readable boxes and lets a chunk of logic be wrapped behind an abstraction.

This spec covers **only the model + flattening foundation**. Everything that makes a group a
*reusable* artifact is explicitly out of scope here and deferred:

**In scope (this build):**
- A JSON schema that can express an **embedded** group (body lives inline in the same document).
- A pure **flattener** that expands groups into a flat graph the runtime already knows how to run.
- Wiring that into the existing load path so grouped graphs load, validate, and render correctly.
- Full test coverage proving a grouped graph is byte-for-byte equivalent to its hand-flat twin.

**Out of scope (later, do not build now):**
- Recipes / saving a group to its own file / a recipe library / disk-load of presets.
- Reference-based groups (`ref` to an external group) and versioning (pin vs track-latest).
- The editor UX: box-select → collapse-to-group, double-click-to-enter, breadcrumbs, the
  "update available" nudge.
- `from_graph` reconstruction of groups (round-tripping a *mutated* grouped graph back to nested
  JSON). See §9 — the hooks are designed in, the work is deferred.

The decided policy from the design conversation, recorded so it isn't relitigated: groups
**flatten at compile**, the runtime never sees them; a group's frozen-vs-live update behavior is a
recipe concern and does not exist yet; recipes (when they land) are **frozen until you choose to
update**.

---

## 2. The core architectural decision

**A group is a pure `EffectGraphDef → EffectGraphDef` preprocessing transform.**

The entire feature is:
1. A backward-compatible **schema addition** (a node may carry an embedded sub-graph).
2. One **pure function** `flatten_groups(def) -> Result<def_flat>` that removes all group nodes by
   inlining their bodies.
3. A **one-call-site insertion** of that function at the top of the existing loader.

Nothing downstream of the flat def changes. The live `Graph`, the executor, the chain splice, the
`persistence` load path, the performance surface (`param_values` / `user_param_bindings`), the
state caches, liveness/reachability, cycle-breaking — all of it receives a flat def exactly as it
does today, because by the time any of it runs, the groups are already gone.

This is why the design is low-risk: it is additive preprocessing, not a new runtime concept.

### Why this is cleaner than "generalize the splicer" (the earlier verbal sketch)

Reading the load path showed there are already **two distinct boundary layers**, and they must not
be conflated:

- **Effect boundary** — `system.source` / `system.final_output`. This is where the *host* splices
  an effect into the render chain (`chain_spec::splice_def_into_chain` →
  `instantiate_def(PerSplice, Splice{source_endpoint})`). One texture in, one texture out. This
  layer stays exactly as it is.
- **Group boundary** — `system.group_input` / `system.group_output` (new). This is folded by the
  flattener at the *def* level, before `instantiate_def` ever runs. Arbitrary named typed ports.

Because the group boundary is folded in a separate, earlier pass, the effect-boundary splice code
in `instantiate_def` is **untouched** — it still only ever sees `system.source` /
`system.final_output`, on an already-flattened def. Two layers, two passes, no entanglement.

### Free consequences of flattening at the def level (before handle interning)

- **Handles are owned `String`s in `EffectGraphDef`** (`EffectGraphNode.handle: Option<String>`).
  Namespacing an inner node is just string concat (`"soft_focus/blur"`) on owned strings. The
  `&'static str` interning happens later, inside `instantiate_def`'s `Box::leak` path, unchanged —
  so **no handle-type refactor is needed.** (The earlier concern about the `&'static str` handle
  map evaporates under this design.)
- **The performance surface needs zero new mechanism.** A preset binding that drives an inner-node
  param targets the node by handle. After flattening that handle is `"soft_focus/mix"` — a real,
  addressable handle in the flat graph. So Ableton / MIDI / envelopes / drivers reach into a group
  by targeting its prefixed handle, through the exact `user_param_bindings` path that exists today.
- **Validation is inherited.** `instantiate_def` already type-checks every wire
  (`resolve_input_port` / `resolve_output_port`) and `validate` runs channel/type/cycle checks on
  the whole graph. Run them on the post-flatten flat def and groups get full type-checking for
  free. The flattener itself does only *structural* rewriting and never needs port types or the
  registry.

---

## 3. JSON schema (manifold-core)

All additions go in `crates/manifold-core/src/effect_graph_def.rs`. Every new field is
`#[serde(default, skip_serializing_if = ...)]` so existing flat documents deserialize unchanged and
re-serialize byte-identically.

```rust
/// Type-id sentinels for group boundary nodes. They live in core because the
/// flattener (also in core) keys on them. Mirrors how `system.source` /
/// `system.final_output` work for the effect boundary, but folded one layer earlier.
pub const GROUP_INPUT_TYPE_ID: &str  = "system.group_input";
pub const GROUP_OUTPUT_TYPE_ID: &str = "system.group_output";
/// Marker type-id for a node that carries an embedded group body.
pub const GROUP_TYPE_ID: &str = "group";

/// One declared port on a group's outward interface — the "label on the box".
/// `port_type` is advisory at flatten time (a readability/editor aid for humans and
/// AI); the authoritative type-check happens post-flatten against the inner node's
/// real port. String form matches the renderer's `PortType` debug tags
/// ("Texture2D", "Scalar(F32)", "Array(...)", "Material", ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfacePortDef {
    pub name: String,
    pub port_type: String,
}

/// One exposed param on the group's interface. Phase-1: a single inner target.
/// (Fan-out to multiple inner params is deferred to match how `BindingDef` fans
/// out; see §10.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupParamDef {
    pub name: String,            // external param name, e.g. "amount"
    pub target_handle: String,   // inner node handle inside the body
    pub target_param: String,    // inner param name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<SerializedParamValue>,
}

/// The outward-facing contract of a group: what crosses its boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInterface {
    pub inputs:  Vec<InterfacePortDef>,
    pub outputs: Vec<InterfacePortDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params:  Vec<GroupParamDef>,
}

/// An embedded group body. `nodes`/`wires` reuse the existing node/wire types,
/// so groups nest naturally (a body node may itself carry a `group`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDef {
    pub interface: GroupInterface,
    pub nodes: Vec<EffectGraphNode>,
    pub wires: Vec<EffectGraphWire>,
}

// EffectGraphNode gains exactly one field:
//
//     /// When present, this node is a group instance: `type_id` is `GROUP_TYPE_ID`,
//     /// its external ports are `group.interface.{inputs,outputs}`, and its `params`
//     /// override `group.interface.params` by name. Boxed to keep `EffectGraphNode`
//     /// small (recursive type). `None` for every ordinary node.
//     #[serde(default, skip_serializing_if = "Option::is_none")]
//     pub group: Option<Box<GroupDef>>,
```

Inside a group body, `system.group_input` and `system.group_output` are plain
`EffectGraphNode`s (no params). The flattener treats them as fold points; their port *names* are
defined by `interface.inputs` / `interface.outputs`. (They become real registered runtime nodes
only in Phase 2, for standalone group editing — see §10. Phase 1 never instantiates them; they are
folded at the def level, exactly like `system.source`/`system.final_output` in the splice path.)

The version constant does **not** need to bump: groups ride inside the existing `nodes` array of a
v1/v2 document. An old binary loading a grouped document fails cleanly at `UnknownTypeId("group")`
rather than silently mis-rendering — acceptable, since groups are a forward feature.

---

## 4. The flattener (the one new piece of logic)

New module `crates/manifold-core/src/flatten.rs`. Pure data transform, **no GPU, no renderer
types, no registry** — fully unit-testable in `manifold-core`'s fast test path.

```rust
pub fn flatten_groups(def: &EffectGraphDef) -> Result<EffectGraphDef, FlattenError>;

#[derive(Debug, Clone, PartialEq)]
pub enum FlattenError {
    /// An outer wire references a group port not declared in its interface.
    UnknownGroupPort { group_handle: String, port: String, side: WireSide },
    /// A `system.group_output` port has zero or >1 inner producers.
    AmbiguousGroupOutput { group_handle: String, port: String, producers: usize },
    /// Two interface ports (or two params) share a name.
    DuplicateInterfaceName { group_handle: String, name: String },
    /// A group node's `params` key is not declared in `interface.params`.
    UnknownGroupParam { group_handle: String, param: String },
    /// A reserved character ('/') appears in a user handle.
    ReservedHandleChar { handle: String },
    /// Reference cycle (only reachable once `ref`-groups land; guarded now).
    GroupCycle { path: Vec<String> },
}
```

### Algorithm

`flatten_groups` walks `def.nodes`. Ordinary nodes pass through unchanged (keeping their ids).
Each group node is expanded by `expand_group`, which returns the inner nodes (renumbered, handles
prefixed) plus two routing maps. Then the outer wires are rewritten against those maps.

Use a single monotonic id counter for the whole output and a per-scope `old_id → new_id` remap.
Numeric ids are only wire keys; identity for bindings lives in handles, so renumbering is safe as
long as every wire is remapped consistently.

**`expand_group(group_node, &mut counter) -> ExpandedGroup`:**

1. If any body node itself carries a `group`, **recurse first** (flatten the body to a fixpoint),
   so `expand_group` only ever splices a group whose body is already flat. Track a visited set of
   group identities along the recursion path → `GroupCycle` on revisit. (Embedded-by-value bodies
   cannot recurse infinitely; the guard is for the `ref` future and pathological depth.)
2. Let `prefix = group_node.handle` (require one; a group instance must be named — it is the
   namespace root). Reject prefixes/handles containing `/`.
3. For every body node **except** `group_input`/`group_output`: assign a fresh id; set its handle
   to `format!("{prefix}/{}", body_handle_or_synthesized)`; copy params/exposed/formats/etc.
   Record `body_id → new_id`.
4. Walk body wires:
   - Wire `group_input.<name> → (inner, port)`: record in `input_map[name].push((new_inner_id, port))`.
     Do **not** emit the wire.
   - Wire `(inner, port) → group_output.<name>`: record `output_map[name] = (new_inner_id, port)`.
     Enforce exactly one producer per output name → else `AmbiguousGroupOutput`. Do **not** emit.
   - Wire `(inner_a, p) → (inner_b, q)` (neither is a boundary): emit it with remapped ids.
5. Apply param overrides: for each `(name, value)` in `group_node.params`, look up
   `interface.params[name]` → `(target_handle, target_param)`, and set `value` on the flattened
   inner node whose handle is `format!("{prefix}/{target_handle}")`, param `target_param`.
   Unknown name → `UnknownGroupParam`. Also seed any `GroupParamDef.default` not overridden.
6. Return `ExpandedGroup { nodes, internal_wires, input_map, output_map }`.

**Outer wire rewrite** (after all groups expanded, with a global `group_id → ExpandedGroup` table):

- `(A.out) → (group#G.in)`: replace with a fan-out — `(A.out) → each entry in input_map["in"]`.
  Unknown `"in"` → `UnknownGroupPort`. Zero consumers → drop the wire (interface port wired
  outside but unused inside is legal: a no-op input).
- `(group#G.out) → (B.in)`: replace with `(output_map["out"]) → (B.in)`. Unknown → `UnknownGroupPort`.
- `(group#G.out) → (group#H.in)`: resolve both ends via the respective maps (and fan out on H).
- `(A.out) → (B.in)` with neither a group: emit with remapped ids.

The output is a `EffectGraphDef` containing **no** group nodes and no `group_input`/`group_output`
nodes — structurally identical to a hand-authored flat document.

### Worked example — a `soft_focus` group (Blur + Mix(source, blurred))

Authoring form (the body lives inline; this is the whole document an author or AI writes):

```jsonc
// outer graph, abbreviated to the relevant nodes
{ "id": 4, "type_id": "system.source" },
{
  "id": 5, "type_id": "group", "handle": "soft_focus",
  "params": { "amount": { "type": "Float", "value": 0.7 } },
  "group": {
    "interface": {
      "inputs":  [{ "name": "src", "portType": "Texture2D" }],
      "outputs": [{ "name": "out", "portType": "Texture2D" }],
      "params":  [{ "name": "amount", "targetHandle": "mix", "targetParam": "t" }]
    },
    "nodes": [
      { "id": 0, "type_id": "system.group_input" },
      { "id": 1, "type_id": "node.blur", "handle": "blur" },
      { "id": 2, "type_id": "node.mix",  "handle": "mix" },
      { "id": 3, "type_id": "system.group_output" }
    ],
    "wires": [
      { "fromNode": 0, "fromPort": "src", "toNode": 1, "toPort": "src" },
      { "fromNode": 0, "fromPort": "src", "toNode": 2, "toPort": "a" },
      { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "b" },
      { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "out" }
    ]
  }
},
{ "id": 6, "type_id": "system.final_output" }
// outer wires: (4.out -> 5.src), (5.out -> 6.in)
```

After `flatten_groups` (renumbered; the runtime sees exactly this):

```jsonc
{ "id": 100, "type_id": "system.source" },
{ "id": 101, "type_id": "node.blur", "handle": "soft_focus/blur" },
{ "id": 102, "type_id": "node.mix",  "handle": "soft_focus/mix",
  "params": { "t": { "type": "Float", "value": 0.7 } } },
{ "id": 103, "type_id": "system.final_output" }
// wires:
//   (100.out -> 101.src)   // source fanned to blur (was source->group.src->blur)
//   (100.out -> 102.a)     // source fanned to mix.a  (the second group_input consumer)
//   (101.out -> 102.b)     // internal blur->mix
//   (102.out -> 103.in)    // group.out resolved to mix.out
```

Identical to what you'd get hand-wiring Blur+Mix. That equivalence is the headline test (§7).

---

## 5. Integration point

Single insertion in `crates/manifold-renderer/src/node_graph/graph_loader.rs`, at the very top of
`instantiate_def`, before the Source/FinalOutput scan:

```rust
// Fold any node groups before anything else runs. After this, the def contains
// no `group` nodes, so every existing path below (splice boundary scan, per-node
// construction, wire translation) operates on a flat document unchanged.
let flattened;
let def = if def.nodes.iter().any(|n| n.group.is_some()) {
    flattened = manifold_core::flatten::flatten_groups(def)
        .map_err(GraphBuildError::Flatten)?;
    &flattened
} else {
    def
};
```

Add `GraphBuildError::Flatten(manifold_core::flatten::FlattenError)`. Both consumers —
`persistence::into_graph` (Standalone) and `chain_spec::splice_def_into_chain` (Splice) — inherit
group support with no changes of their own, because both route through `instantiate_def`.

`check-presets` (`crates/manifold-renderer/src/bin/check_presets.rs`) loads through the same path,
so it validates grouped presets for free.

---

## 6. What explicitly does NOT change

State this plainly so the build session doesn't go looking for work that isn't there:

- **Live `Graph`, `NodeInstance`, `NodeWire`** — unchanged. They only ever hold flat nodes.
- **Executor / execution plan / resource binding / state store** — unchanged.
- **`chain_spec` splice + `SpliceResult`** — unchanged (effect boundary, separate layer).
- **`persistence::from_graph`** — unchanged. It serializes a flat live graph to a flat def. (This
  is the round-trip caveat — see §9.)
- **Performance surface** (`EffectInstance.param_values`, `user_param_bindings`, drivers/Ableton)
  — unchanged. Bindings target prefixed handles that exist post-flatten.
- **`validate` / `validate_connection`** — unchanged; they run post-flatten and cover groups.

---

## 7. Testing plan

Gate each phase on its tests before moving on. Test scope per `CLAUDE.md`: core flatten tests are
pure CPU (fast, run constantly); the grouped-fixture render/parity test is GPU and runs at phase
gates; the Liveschool load is the regression backstop.

**Unit (manifold-core, `flatten.rs` `#[cfg(test)]`) — pure, no GPU:**
- `flattens_single_group_to_expected_flat_def` — the §4 worked example, asserting exact
  nodes/wires/handles/params.
- `fans_out_group_input_to_all_inner_consumers` — the `source → {blur, mix.a}` fan-out.
- `resolves_group_output_to_inner_producer`.
- `two_instances_of_same_group_get_distinct_prefixes` — `a/blur` vs `b/blur`, no collision, two
  independent param slots.
- `nested_groups_flatten_recursively` — group inside a group → `outer/inner/leaf`.
- `routes_group_param_override_to_inner_target`.
- Error cases: `unknown_group_port`, `ambiguous_group_output`, `duplicate_interface_name`,
  `unknown_group_param`, `reserved_handle_char`, `group_cycle` (synthetic ref).
- `flatten_is_identity_on_groupless_def` — a flat def in → equal def out (modulo renumbering;
  assert topological equivalence, not literal ids).

**Parity (the headline — manifold-renderer):**
- `grouped_equals_handwired` — build one effect two ways: a hand-flat `Blur+Mix` def, and the
  grouped def from §4. Assert the flattened grouped def is topologically identical to the hand-flat
  def (same node type_ids, same wire connectivity after handle-normalization). This is the proof
  that grouping is purely cosmetic at runtime. Pure CPU if compared at the def level — no GPU
  needed, so it can live in core or in renderer's lib tests.
- Optional GPU confirmation: render both through the existing parity harness and assert
  pixel-identical output, if a render-level guarantee is wanted beyond structural equality.

**Integration (manifold-renderer):**
- A grouped bundled preset fixture loads through `into_graph` and executes one frame without error
  (extend the existing `bundled_presets` lib test with a grouped entry — note `check-presets` alone
  does **not** execute a GPU frame, per `feedback_check_presets_is_not_runtime`).
- `check-presets` passes on the grouped fixture (structural load+compile).

**Regression:**
- The canonical `Liveschool Live Show V6 LEDS.manifold` fixture still loads and renders identically
  (it contains no groups, so this proves the additive change is truly additive).

---

## 8. Build sequence for the ultracode session

Each phase is independently committable and test-gated.

- **Phase 0 — baseline & confirm.** Run `cargo test -p manifold-renderer --lib bundled_presets` and
  the Liveschool load test green *before* any change. Confirm `from_graph(into_graph(preset))` is a
  topological identity on one existing flat preset (it should be). This establishes the
  before-picture the parity test compares against.
- **Phase 1 — schema.** Add the types in §3 to `effect_graph_def.rs`; add the `group` field to
  `EffectGraphNode`; serde round-trip tests (grouped doc → JSON → doc equality; old flat doc
  unchanged). No behavior yet. Gate: `cargo test -p manifold-core --lib`.
- **Phase 2 — flattener.** Implement `flatten.rs` + all §7 unit tests. Pure, fast, no GPU. Gate:
  `cargo test -p manifold-core --lib flatten`.
- **Phase 3 — integrate.** Wire `flatten_groups` into `instantiate_def` (§5) + the `GraphBuildError`
  variant. Gate: the grouped-fixture integration test + `check-presets`.
- **Phase 4 — parity & regression.** The `grouped_equals_handwired` parity test; extend
  `bundled_presets`; run the Liveschool regression. Gate: those tests + `cargo clippy --workspace
  -- -D warnings`.
- **Phase 5 — author the reference fixture.** Re-author one existing multi-node effect as a grouped
  document, land it under `assets/effect-presets/` (or a test fixtures dir), and confirm
  render-identical to the flat original. This doubles as the authoring example humans and AI copy
  from. Because the blast radius touches the shared loader, run the **full** `cargo test --workspace`
  once at the end per `CLAUDE.md`'s infrastructure-change rule.

---

## 9. The save / round-trip decision (read before Phase 5)

`persistence::from_graph` serializes the **flat** live graph and has no group awareness. So:

- **Loading** a nested document and rendering it: fully supported by this spec.
- **Re-saving** a grouped document via `from_graph` after the runtime has flattened it: would emit
  a *flat* def and **lose the grouping.**

For this build that is acceptable and intended, because the only thing that mutates a grouped graph
— the editor — is out of scope. The contract for Phase 1 is: **the nested JSON document is the
source of truth; flattening is a one-way compile step; grouped documents are authored as JSON (by
hand or AI) and never round-tripped through `from_graph`.**

The forward hook so this doesn't wall off the editor: when the editor lands (Phase 2 of the larger
effort), each flattened node should carry a `group_path` breadcrumb (`["soft_focus"]`,
`["outer","inner"]`) recorded during `expand_group`, and a future `from_graph` reconstructs the
nested document by partitioning nodes on that path. Do **not** build reconstruction now — but the
flattener should keep the structure (it already has it in the prefixed handles: split on `/`) so
the data is recoverable later. Note this explicitly in the flattener so it isn't lost.

---

## 10. Forward-compatibility hooks (designed in, not built)

These shape Phase-1 choices so the deferred work is additive, nothing more:

- **Reference groups / recipes.** Replace the embedded `group: Box<GroupDef>` with a
  `ref: String` resolved against a recipe store. The flattener takes a resolver
  `Fn(&str) -> Option<GroupDef>`; Phase 1 passes an embedded-only resolver. The expand/splice logic
  is identical once the body is in hand — recipes are "resolve a ref, then run §4." The
  `GroupCycle` guard already exists for this.
- **Standalone group editing.** `system.group_input`/`system.group_output` become real registered
  runtime nodes (trivial, like `boundary_nodes.rs`) with **dynamic ports** built from the interface
  via the existing `reconfigure` hook (the `mux_texture` variadic-port pattern). Needed only to
  open a group on its own canvas. Not needed to flatten-and-run.
- **Param fan-out.** `GroupParamDef` is single-target now. To match `BindingDef`'s one-to-many
  fan-out later, make `target_*` a `Vec`. Additive.
- **Explicit interface validation.** `InterfacePortDef.port_type` is advisory in Phase 1. A later
  pass can assert it against the inner port's real type at flatten time for earlier, clearer errors
  (today they surface post-flatten at wire resolution, which is correct but less specific).

---

## 11. Risks & open questions

- **Handle delimiter.** This spec reserves `/`. **Confirmed safe 2026-06-01:** a grep of every
  preset under `crates/manifold-renderer/assets` and every `add_node_named` literal found zero
  handles containing `/` (handles are author-chosen identifiers like `uv_transform`, `feedback`,
  `mix`). The flattener should still reject a `/` in a user handle (`ReservedHandleChar`) so the
  invariant is enforced, not just currently-true.
- **Interface port with no inner consumer.** Spec says drop the outer wire (no-op input). Confirm
  that matches authoring intent vs. erroring; dropping is the more forgiving choice for AI authors.
- **`group_output` fed by a passthrough/skip node.** The skip-passthrough machinery
  (`skip_passthrough_ports`) lives on real nodes and runs post-flatten, so it composes — but add a
  test with a skippable node directly on a `group_output` to be sure.
- **Renumbering vs. preserving ids.** Spec renumbers everything from one counter. If any tooling
  keys on stable numeric ids across loads (it shouldn't — handles are the stable identity), that
  surfaces here. Phase 0 confirms nothing depends on numeric-id stability.
- **Performance.** Flattening is O(nodes+wires) per load, off the hot path (load time only). No
  per-frame cost. No concern.

---

## 12. File-by-file change checklist

- `crates/manifold-core/src/effect_graph_def.rs` — add `InterfacePortDef`, `GroupParamDef`,
  `GroupInterface`, `GroupDef`, the three `*_TYPE_ID` constants, and the `group` field on
  `EffectGraphNode`. Serde round-trip tests.
- `crates/manifold-core/src/flatten.rs` *(new)* — `flatten_groups`, `FlattenError`, the full §7
  unit suite, and the `group_path`/`/`-split note from §9.
- `crates/manifold-core/src/lib.rs` — `pub mod flatten;`.
- `crates/manifold-renderer/src/node_graph/graph_loader.rs` — the §5 insertion + the
  `GraphBuildError::Flatten` variant + its `Display`/mapping.
- `crates/manifold-renderer/tests/` (or lib) — `grouped_equals_handwired` parity test; grouped
  entry in the `bundled_presets` integration test.
- `assets/effect-presets/<GroupedExample>.json` *(Phase 5)* — the reference grouped document.
- **Deferred (do not touch now):** `boundary_nodes.rs` (Phase-2 runtime group nodes),
  `persistence.rs` `from_graph` reconstruction, any editor/canvas file.
