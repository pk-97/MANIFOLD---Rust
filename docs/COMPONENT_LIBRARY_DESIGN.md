# Component Library — Design

**Status:** Approved design, not implemented. Sonnet-executable.
**Decided:** 2026-07-02. Decided questions in §12 — do not reopen them.
**Companions:** `NODE_GROUPS_DESIGN.md` (the substrate), `GROUPING_GRAPHS.md`, `MCP_INTERFACE_DESIGN.md` (the main consumer), `NODE_CATALOG.md`.
**Prerequisites:** NODE_VOCABULARY_AUDIT apply pass (components are named in the post-rename vocabulary — building them on old ids doubles the migration). Sequencing: `docs/DESIGN_BUILD_ORDER.md` wave 3.
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase. Conformance-hardened: run the §8.3 pre-flight before each phase — node-groups backend and catalog will have moved by execution time.

---

## 1. What a component is

The library's real unit of reuse. Today the vocabulary has two tiers and a gap in the middle:

| Tier | Count | What it is | Who composes with it |
|---|---|---|---|
| Atoms | ~185 | one dispatch / one operation | experts, and agents only in small well-understood stretches |
| **Components** | **new** | **named, documented, reusable subgraph with a typed boundary and macro params** | **everyone — the main composition unit** |
| Presets | ~45 | finished instruments | selected and performed, not raw material to mutate |

A component is a node group promoted to a library citizen: a `GroupDef` plus metadata (name, purpose, tags, macros with ranges). The group boundary is its API — typed input/output ports, a handful of named knobs. Ableton's rack, TouchDesigner's COMP: the tier where all real reuse happens in those tools.

Dual audience, one artifact:
- **Humans** drag "Feedback Decay" or "Bloom Chain" into a graph instead of hand-wiring the same 7 nodes again. Components collapse to single boxes on the canvas — this is also the structural fix for wiring legibility.
- **Agents** compose typed contracts instead of wiring 185 atoms cold or mutating a 48-node preset they don't understand. Composing five components with clear signatures is the shape of task models are actually good at.

## 2. By-value, always

Inserting a component **deep-copies** its `GroupDef` into the host document as an ordinary group node. There are no references, no linking, no library lookups at load time. A `.manifold` file on a gig laptop is self-contained, period.

- A **provenance stamp** rides on the copy: `GroupDef.source: Option<String>` (additive serde field), value `"component-id@version"` — e.g. `"bloom_chain@3"`. It means "descended from", nothing more. It enables a future, optional "component updated — refresh this instance?" nudge; it never auto-updates anything.
- Editing inside your copy is normal graph editing. No locks, no "detach" ceremony. Provenance survives edits (it's a lineage note, not a contract).
- Ungroup works as normal and drops the stamp.

## 3. Data model

One component = one JSON file. New types in `manifold-core` (serde camelCase, mirroring `PresetMetadata`'s conventions):

```rust
pub struct ComponentDef {
    pub id: String,                 // stable, filename-matching: "bloom_chain"
    pub display_name: String,       // "Bloom Chain"
    pub category: String,           // picker grouping — reuse/extend preset categories
    pub purpose: String,            // one line, shown in pickers AND served to agents
    #[serde(default)]
    pub description: String,        // usage notes: when to use, what the macros do
    #[serde(default)]
    pub tags: Vec<String>,
    pub class: ComponentClass,      // Bundled | User | Agent (§7)
    #[serde(default = "one_u32")]
    pub version: u32,               // informational; bumped on bundled edits
    pub group: GroupDef,            // the entire body — interface, nodes, wires
}
```

Components are **kind-agnostic**: a component is a subgraph, not an effect or a generator. The same "Polar Warp Stage" drops into either document kind; the post-flatten type-check (which already validates every wire) is the authority on whether it fits where it was placed.

## 4. The macro layer

`GroupParamDef` exists but is phase-1 thin: single inner target, no display metadata, and it is a **load-time override**, not a live control. Components need real macros — the rack-knob experience. All changes are additive.

### 4a. Schema extensions (`manifold-core`)

```rust
pub struct GroupParamDef {
    pub name: String,
    pub target_handle: String,
    pub target_param: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<SerializedParamValue>,
    // ── new, all additive ──
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_targets: Vec<GroupParamTarget>,   // fan-out: one knob, N inner params
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,                  // display name; None → prettified `name`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f32; 2]>,                // slider min/max for float macros
}

pub struct GroupParamTarget {
    pub target_handle: String,
    pub target_param: String,
    #[serde(default = "one_f32", skip_serializing_if = "is_one")]
    pub scale: f32,                             // per-target remap, mirrors BindingDef
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f32,
}
```

Existing grouped documents deserialize unchanged and re-serialize byte-identically (every new field defaults + skips).

### 4b. How macros become live (the key design move)

**Macros lower onto the existing card-binding surface. No new runtime mechanism.**

The perform surface today: a card param = `BindingDef { id, label, default_value, target: BindingTarget::Node { node_id, param }, scale, offset }`, driven through `param_values` / `user_param_bindings` by MIDI, Ableton, LFOs, audio. `BindingTarget` addresses inner nodes by stable `NodeId`, which is **invariant under group/ungroup/flatten** — the addressing layer was already built for this.

When the user (or an agent) exposes a component macro on the host card:

1. The editor resolves each macro target (`target_handle`/`target_param`, document-level and instance-independent) to the **freshly minted `NodeId`s** of this instance's inner nodes.
2. It emits one `BindingDef` per macro with `user_added` semantics, carrying the macro's label, default, and per-target scale/offset.
3. Fan-out macros need `BindingDef` to carry multiple targets. Extend additively: `extra_targets: Vec<BindingTarget>` on `BindingDef` (default empty, skipped when empty — bundled preset JSON stays byte-identical), applied at the same renderer write boundary as the primary target. *Implementation note: verify against the actual write-boundary code path when building; the shape of the change is pinned, the exact splice point is not.*

Consequences, all free:
- MIDI / Ableton / LFO / audio modulation reach component macros through the exact path they use today.
- The graph editor stays authoring-only (`graph-editor-is-authoring-not-perform`): macros are *performed* on the card, *defined* in the editor.
- Un-exposing follows the existing `user_added` removal semantics.

The load-time behavior of `GroupParamDef` (defaults + instance overrides baked at flatten) is unchanged; macros are the same declaration made live-addressable on demand.

## 5. Storage & registry

- **Bundled:** `crates/manifold-renderer/assets/components/*.json`, compiled in alongside preset JSON, riding the same hot-reload watcher pattern. Curated; shipping a bundled component is a deliberate act.
- **User + agent:** the user library directory, mirroring wherever user-saved presets live today (one file per component). Same mechanism, different directory.
- **Registry:** `component_registry` in `manifold-core`, mirroring `preset_definition_registry`'s shape (`ArcSwap<HashMap<String, Arc<ComponentDef>>>`, rebuild on hot-reload). Fully separate from `PRESET_DEFINITIONS` — components are not presets and never enter the preset picker or `PresetTypeId` space.

## 6. Insertion & authoring flow

**Insert (picker → graph):** copy the `GroupDef` by value; assign a uniquified instance handle from the component id (`bloom_chain`, `bloom_chain_2`); mint fresh `NodeId`s for every inner node (two instances must never share ids — same rule as `TimelineClip::duplicated()`); stamp `source`. The result is an ordinary group node; the flattener and runtime never know it came from the library.

**Save-selection-as-component (the authoring loop):** the pieces already exist in `group_edit.rs` — `group_selection` + `infer_interface`. The flow: box-select → group → name the group's params/macros → "Save to Library" → write a `ComponentDef` (class = User) to the user directory. This is how the library grows from real work: idioms get extracted the moment the author notices they've built the same thing twice.

**Depends on:** the node-groups *canvas* work (collapsed group rendering, enter/exit) — still pending from the groups effort. Insertion without canvas support would drop an invisible box into the editor; C3 below sequences after it.

## 7. Curation classes

`ComponentClass::{Bundled, User, Agent}` — visibly distinct in every picker and library view.

- **Bundled** ships with MANIFOLD. Curated instrument collection; quality bar is "Peter would use it in a set."
- **User** is yours.
- **Agent** is where every MCP `save_component` lands, without exception. Promotion to User (or Bundled) is a deliberate human act in the library UI.

The junk-drawer guard: forty near-identical agent-made "glow chains" can exist without polluting anything, because class boundaries are hard and promotion is manual.

## 8. MCP integration

Amends `MCP_INTERFACE_DESIGN.md` §5 (noted there):

- `list_nodes` gains `tier: "atom" | "component"`; component entries carry purpose + macro summary. Agents are instructed to **compose components first, atoms to glue, raw WGSL last**.
- `get_node_docs` accepts component ids and serves the full interface: ports, macros (label/range/default), purpose, description.
- New tool `save_component(graph_json_fragment, interface, name, purpose)` — gated by `allow_structure_edits`, always lands class = Agent.
- The vocabulary stress-test reframes accordingly: the question is "can Sonnet compose from *components*," with atoms as the fallback vocabulary — a fair bar, and the one the product actually depends on.

## 9. Seeding v1 — the mining pass

The 45 shipping presets already contain the idioms, hand-wired repeatedly. A curation pass extracts them. **Extraction criteria** (all must hold):

1. Recurs in ≥2 presets, or is a judged-canonical idiom of the medium.
2. Roughly 3–12 nodes — below that it's an atom, above that it's probably a preset.
3. Clean typed boundary: typically 1–3 inputs, 1 output.
4. Purpose statable in one line without "and".
5. 2–6 macros that a performer would actually turn.
6. **No audio analysis inside** — audio stays on the perform surface (`audio-stays-on-perform-surface`) and reaches components by binding to their macros from outside.

Candidate seeds (from known recurring idioms — **unvalidated until the pass reads every preset**): feedback loop w/ decay + transform, bloom chain (threshold → blur → add), polar/kaleidoscope warp stage, RGB-split chromatic aberration, displacement warp, edge-detect → colorize, filmic finisher (vignette + grain + tone), UV transform stack, mask compositor, particle advect + splat pair; 3D camera + light rig once the material system lands.

Process per candidate: extract → author as `ComponentDef` → verify with the existing grouped-vs-flat parity pattern (`grouped_equals_handwired`) → land. **Do not rewrite shipping presets to use the new components** — presets stay as they are; retrofitting is optional later work with no v1 payoff and real churn risk.

## 10. What does NOT change

- The flattener, loader, executor, state stores, liveness, cycle-breaking: untouched. Components vanish at flatten like any group.
- Presets: same format, same registry, same picker. The preset tier keeps its meaning (finished instruments).
- The perform surface: `param_values` / `user_param_bindings` / drivers — macros arrive as ordinary bindings.
- `EditingService` as sole mutation gateway; library file writes go through the same service layer as user-preset saves.

## 11. Phasing

- **C1 — Schema + registry.** `ComponentDef`, `ComponentClass`, `GroupDef.source`, component registry, bundled-dir loading. *Gate:* `cargo test -p manifold-core --lib`; a fixture component loads, round-trips byte-identically, and inserts (in-memory) into a host def that then flattens clean.
- **C2 — Macro layer.** §4a schema extensions; §4b lowering incl. `BindingDef.extra_targets` + write-boundary fan-out. *Gate:* old grouped/preset JSON byte-identical on re-serialize; a fan-out macro drives two inner params through the card path in a lib test; clippy clean.
- **C3 — Editor flow.** Component picker + insert (after node-groups canvas lands); save-selection-as-component. *Gate:* insert two instances of one component → distinct handles/NodeIds, independent macros; expose a macro → turn it from the card.
- **C4 — Mining pass.** Read all 45 presets against §9 criteria; author the v1 bundled set (~10–15); parity-verify each. *Gate:* every bundled component passes `check-presets`-equivalent load + the grouped-vs-flat structural parity test; Peter approves the set by eye.
- **C5 — MCP tier.** §8, riding the MCP build (its P1/P4 phases). *Gate:* an agent lists components, reads docs, composes a graph from two components + glue atoms, saves as class Agent.

C1/C2 are independent of the MCP build and of the canvas work — they can land first and alone.

## 12. Decided questions — do not reopen

1. **By-value only.** No references, no linking. Provenance stamp for lineage; any future "refresh" is manual and per-instance; nothing ever auto-updates.
2. Components are **kind-agnostic subgraphs** — same component works in effect and generator documents; post-flatten validation is the authority.
3. **Macros lower onto the existing card-binding surface** (stable-`NodeId` targets). No runtime group params, no second modulation path.
4. **No audio analysis inside components** — audio binds to macros from the perform surface.
5. Every MCP-saved component lands **class = Agent**; promotion is a human act.
6. The tier is named **Component**.
7. Editing an inserted copy is normal editing — no locks, no detach step; provenance is a note, not a contract.
8. Shipping presets are **not** retrofitted onto components in v1.
