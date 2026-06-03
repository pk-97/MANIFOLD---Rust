# Grouping Graphs — A Working Guide for Organizing Graph Topology

This is the "how to think" guide for taking a working flat graph and organizing it into
human-readable node groups — the thing that turns a 57-node hairball into ten labeled boxes you
can read at a glance. It is the sibling of [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md):
that guide is about *granularity* (what the atoms are), this one is about *legibility* (how the
atoms are arranged once they exist).

It is written to be followed by a human in the graph editor **or** an AI agent restructuring a
preset JSON directly. The procedure is the same either way.

Read alongside:
- [NODE_GROUPS_DESIGN.md](NODE_GROUPS_DESIGN.md) — the flattener mechanics + JSON schema (the
  authoritative spec for *what a group is*; this guide assumes it).
- [NODE_GROUPS_UI_DESIGN.md](NODE_GROUPS_UI_DESIGN.md) — the editor UX (collapse-to-group, enter,
  breadcrumbs).
- [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) §6.6 — naming nodes for what they do.

---

## 1. What grouping is, and what it is deliberately not

Grouping is a **presentation layer**. A group is a labeled box wrapping a slice of the graph
behind a named interface. At load it flattens away to a flat graph the runtime already knows how
to run — byte-for-byte equivalent to the hand-wired form. The runtime, the executor, the state
store, the performance surface never see a group.

Grouping is **not**:
- **Decomposition.** It does not change what the atoms are or how fine-grained they are. If a
  graph balloons with repeated math scaffolding, that's a *primitive* problem — build the
  primitive (see DECOMPOSING §6.1), don't paper over it with a group.
- **Performance optimization.** Groups do not fuse, reduce dispatches, or change cost. The
  freeze/bake fusion direction is a separate concern. Grouping a slow graph leaves it exactly as
  slow; it just makes it readable.
- **A new runtime concept.** It is additive preprocessing. This is why it is low-risk.

The litmus test: **if grouping changed any number the runtime computes, you did it wrong.** The
only thing that should change is how the graph reads.

---

## 2. The one invariant that makes grouping safe: `nodeId`

This is the load-bearing fact. Internalize it before touching anything.

Every node carries a stable `nodeId`, minted once and **preserved through grouping, ungrouping,
moving, and flattening at any nesting depth.** Only a node's *handle* gets prefixed when it enters
a group (`euler` → `Move Particles/euler`). The `nodeId` does not move.

The entire live performance surface — card bindings, drivers, Ableton, OSC, MIDI, envelopes —
addresses inner nodes by `nodeId`, never by handle or position. So:

> **As long as you preserve every node's `nodeId`, grouping cannot touch the instrument.**
> A slider that drove `turbulence_base` before still drives it after, no matter how many boxes
> you wrap around it.

(The NODE_GROUPS_DESIGN §6 line about bindings targeting "prefixed handles" predates the
node-id-targeting work — bindings target `nodeId` now, which is exactly why grouping is free.)

Corollary: never rewrite a `nodeId` while grouping. If you're transforming the JSON
programmatically, pull each node object **verbatim** from the original and change only its local
`id` and (via flatten) its handle.

---

## 3. The hard constraints the flattener enforces

These are not style — break one and the graph fails to load (see `manifold-core/src/flatten.rs`).

1. **Exactly one producer per group output port.** A `system.group_output` port may have one and
   only one inner node feeding it. Two producers → `AmbiguousGroupOutput`. (A single producer
   feeding *many* output ports, or one output port feeding many *external* consumers, is fine.)
2. **No input → output passthrough.** A wire straight from `group_input` to `group_output` is
   rejected. If a group needs to pass a value through untouched, it almost certainly shouldn't own
   that value — leave it at the parent level.
3. **A group must be named, uniquely among its siblings, with no `/`.** The handle is the
   namespace prefix for every inner node. `/` is reserved. Duplicate sibling handles collide in
   the flat handle map — keep them distinct.
4. **Interface port names must match the boundary wires.** A wire from `group_input` uses the
   interface *input* name as its `fromPort`; a wire into `group_output` uses the *output* name as
   its `toPort`. A name that isn't declared in the interface → `UnknownGroupPort`.
5. **`portType` on the interface is advisory.** The flattener ignores it and the editor renders
   every group pin the same way; the real type-check runs post-flatten against the inner node's
   actual port. Still — **write the accurate tag** (`Texture2D`, `Scalar(F32)`, `Array(Particle)`,
   `Material`, …). It costs nothing and it's documentation for the next reader.
6. **Keep the boundary nodes at the top level.** `system.generator_input` / `system.source` /
   `system.final_output` must stay at the document's top level — the loader checks for them
   *before* flattening. Burying `final_output` inside a group fails the boundary check.
7. **You do not need `interface.params`.** For pure reorganization, leave each inner node's
   `params` exactly as they were and omit the interface `params` block entirely (as the shipping
   presets do). The param-override mechanism is for *reusable* groups with knobs on the box — a
   later concern, not reorganization.

---

## 4. How to choose the groups

The goal is a top level a stranger can read top-to-bottom and understand the instrument. Heuristics,
in priority order:

- **Find the spine and keep it visible.** Every graph has a main flow — a chain, or a feedback
  loop. Put the spine's stages at the top level as boxes, and keep the one or two nodes that *are*
  the spine's pivot (the stateful/feedback node, the IO boundary) as visible top-level nodes, not
  buried in a group. In Fluid Sim 2D the spine is the particle feedback loop, so `Particle State`
  (the one `array_feedback`) sits at the top level between Spawn and Move — you can see the loop
  close.
- **One group = one responsibility you can name in plain language.** If you can't say what a box
  does in a short phrase about the output ("builds the force that pushes particles", "renders
  particles to a density image"), the boundary is wrong.
- **Gather control/scalar plumbing into named control boxes.** The math that turns sliders +
  canvas size into the numbers the pipeline needs is not the spine, but it's also not clutter that
  belongs loose. One box named `Resolution Scaling` with six output wires beats ten naked `math`
  nodes. Same for trigger/envelope logic → `Clip Triggers`.
- **Compute once, fan at the top level — never duplicate.** A value used by three groups should be
  produced in one group, exposed as one output, and fanned out by three top-level wires. Do not
  recompute it inside each consumer to avoid a crossing wire. Crossing wires are cheap; duplicated
  logic drifts.
- **Don't group across the spine to shave node count.** A box that straddles two pipeline stages
  hides the very flow you're trying to reveal. Boundaries should fall *between* conceptual stages,
  not through them.

A blunt sizing instinct: aim for a top level of roughly 6–12 boxes. Fewer and the boxes are
probably doing too much; many more and you haven't really organized it.

---

## 5. Flat vs. nested — when nesting earns its keep

Nesting is supported to depth 64 and the flattener recurses for free. But nesting is not free to
*read* or to *wire*, so use it deliberately.

**Nest when the subsystem is genuinely hierarchical:** one shared resource feeding N parallel
branches. Fluid Sim 2D's `Clip Triggers` is the canonical case — one decay envelope and one mode
dial route to five behaviors (Turbulence Burst, Rotation Flip, Flow Reversal, Pattern Reset,
Inject Burst). Nesting those five as sub-groups makes the "five modes off one envelope" structure
legible in a way a flat 24-node box never could. The shared envelope + selector live at the parent
level and fan into each child.

**Stay flat for linear chains.** Wrapping a straight sequence of well-named atoms in a sub-group
hides nothing — it just adds a box you have to open. `Move Particles` is six atoms in a line; it
stays flat.

**The cost model that decides borderline cases:** every signal crossing a group boundary needs an
interface port *at each level it crosses*. A value produced two levels deep and consumed outside
must be exposed through the sub-group interface *and* the parent interface — two ports, two boundary
wires, for one signal. Inject Burst's four outputs each pay this twice. So the test is:

> Nest only when the inner box has a name a performer would recognize **and** its own internal
> fan-out. If it's a straight chain, or it leaks many signals across the boundary, keep it flat.

---

## 6. Naming

Group names and interface port names are a UX surface, the same as node names. Apply
DECOMPOSING §6.6 wholesale:

- **Name for what it does to the output**, in plain language. `Flow Field`, `Render Density`,
  `Clip Triggers` — not `gradient_rotate_block` or `subsystem_3`.
- **No underscores or hyphens in group names.** Title Case with spaces (`Move Particles`).
- **No implementation or math jargon.** A performer loading the preset shouldn't need to have read
  a graphics paper. `Flow Field`, not `Central Difference Gradient`.
- **Interface port names: camelCase, no spaces, no underscores** (`blurredDensity`, `injectPointX`,
  `triggerCount`). They're identifiers on the box pins; keep them clean and predictable so the next
  author can guess them.

---

## 7. Verification — non-negotiable

This is live-show code; a rewiring slip becomes the show. A grouped graph that loads is not proof
it's *equivalent*. Prove equivalence directly.

**The equivalence recipe (the gold standard).** Flatten both the grouped graph and the
pre-grouping baseline, then compare **in `nodeId` space** (which survives the id-renumbering and
handle-prefixing the flattener does):

1. **Connectivity set** — `{(fromNodeId, fromPort, toNodeId, toPort)}` for every wire. Must be
   set-equal.
2. **Per-node facts** — `{(nodeId, typeId, params, wgslSource)}`. Must be set-equal. (Catches a
   dropped param or a mangled shader.)
3. **Binding targets** — every `presetMetadata.bindings[*].target.nodeId` still exists in the
   flattened graph.

If all three match, behavior is byte-identical. `manifold_core::flatten::flatten_groups` is the
function; a throwaway integration test that reads both JSON files, flattens, and asserts the three
sets is ~80 lines and runs in milliseconds (pure CPU, no GPU).

**Then the runtime gates:**
- `cargo run -p manifold-renderer --bin check-presets` — every preset loads, flattens, and
  compiles. Sub-second.
- `cargo test -p manifold-renderer --lib bundled_generator_presets` (or `bundled_presets` for
  effects) — builds the chain, resolves bindings, and **executes one frame on the real Metal
  backend.** This is the only gate that catches WGSL/binding errors that surface only at first
  execute (`check-presets` does not run a frame).

**Build the grouped JSON programmatically.** Don't retype params or `wgslSource` — load the
original, index nodes by id, and emit the grouped structure by pulling each node object verbatim
and changing only its local `id`. This makes a transcription error on a large shader source
impossible by construction.

---

## 8. Worked example — Fluid Sim 2D

`crates/manifold-renderer/assets/generator-presets/FluidSimulation.json` — 57 flat nodes
reorganized into ten top-level boxes:

- Top level (the visible feedback loop): `Inputs → Spawn Particles → Particle State → Move
  Particles` and back; `Particle State → Render Density → Display → Output`; `Render Density →
  Flow Field → Move Particles`.
- Eight groups: **Spawn Particles**, **Move Particles**, **Flow Field**, **Render Density**,
  **Display**, **Resolution Scaling**, and **Clip Triggers** — which nests **Turbulence Burst /
  Rotation Flip / Flow Reversal / Pattern Reset / Inject Burst** under one shared envelope + mode
  selector.
- 36 top-level wires. Verified flatten-equivalent to the pre-grouping graph; passes check-presets
  (46/46) and the generator one-frame-execute test.

[Glitch.json](../crates/manifold-renderer/assets/effect-presets/Glitch.json) is the simpler
reference (three flat groups, no nesting).

---

## 9. The procedure, as a checklist (humans and agents)

1. **Read the flat graph end to end.** Identify the spine (chain or loop), the stateful/IO nodes,
   the shared signals, and the control plumbing. Don't group what you don't understand.
2. **Draft the partition.** Assign every node to exactly one group or to the top level. Name each
   group for what it does to the output (§6). Keep the spine pivot and IO boundary nodes at the top
   level (§3.6, §4).
3. **Decide flat vs. nested per group** using the §5 cost test. Default to flat.
4. **Define each interface** — the inputs (signals entering) and outputs (signals leaving),
   exactly one producer per output (§3.1), accurate `portType` tags (§3.5), camelCase names (§6).
5. **Build it programmatically**, preserving every `nodeId`, `params`, and `wgslSource` verbatim
   (§2, §7). Omit `interface.params` for pure reorganization (§3.7).
6. **Verify equivalence** with the three-set `nodeId`-space comparison, then `check-presets`, then
   the one-frame-execute test (§7). Do not skip the frame execute.
7. **Update the preset `description`** to narrate the groups (what each box does, why anything is
   nested), and state plainly that the groups flatten to the same behavior. Glitch and Fluid Sim 2D
   both do this.

If any step can't be satisfied — an output needs two producers, a value wants to pass straight
through, a group can't be named in a phrase — the partition is wrong, not the constraint. Redraw the
boundaries.
