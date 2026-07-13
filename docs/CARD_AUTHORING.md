# Card Authoring — semantic intent → idiom, for the outer effect/generator card

**Status:** working guide (DESIGN_DOC_STANDARD §1) · 2026-07-13 · Sonnet (GRAPH_TOOLING_DESIGN P4, D9)
**Prerequisites:** `docs/GRAPH_TOOLING_DESIGN.md` D8–D9 decided every row and every lint below; this doc transcribes and formats — it adds none.

The "card" is the outer-facing surface of an effect or generator preset: the
sliders, toggles, and buttons a performer sees, built from
[`PresetMetadata`](../crates/manifold-core/src/effect_graph_def.rs) — `params`
(`ParamSpecDef`) routed to inner-graph node params via `bindings`
(`BindingDef`). This guide is the semantic-intent → idiom table an authoring
agent (or a human) reads before wiring a card, plus a short mechanical
reference for the less-obvious `ParamSpecDef` fields.

**Machine-check pointers, before you read further:** `graph_tool validate
<file.json> --kind effect|generator` runs every check in this doc mechanically
(the D8 card lints inside `node_graph::validate::validate_def`) — errors mean
the card lies to the performer (fix before shipping); warnings mean an idiom
row below was violated (fix in-session, per Peter's rule that agents don't
habituate across sessions). `graph_tool fusion <file.json>` is the sibling
tool for dispatch-cost questions, unrelated to card correctness. See
`docs/GRAPH_TOOLING_DESIGN.md` for the full mechanism.

## Intent → idiom table (D9, verbatim)

| Intent | Idiom | Notes |
|---|---|---|
| **Toggle between two looks** | mux select + `is_toggle` | **Never** blend-at-the-rails (setting a continuous blend param to 0 or 1 to fake a switch). Blend is right only for a continuous morph between the two looks — say so explicitly on the card, so an agent reading a blend param doesn't "correct" it into a mux it was never meant to be. |
| **N-way mode** | `whole_numbers` + `value_labels` → mux select | The card's integer range and its `value_labels` count must agree (D8 error c) — an N-way choice is N labels over an N-step integer range, wired to a mux's select input. |
| **Button that enables/fires** | `is_trigger` → a trigger-typed inner param | The binding must target a param whose registry-declared `ParamType` is `Trigger` (D8 error d) — binding a trigger button to an ordinary float silently does nothing meaningful on press. |
| **Full-rotation knob** | `wraps` | A periodic param (LFO/automation sweeping past `max` should wrap, not clamp and hitch at the rail). |
| **Momentary vs. latching** | momentary = `is_trigger` (fires once per press, no held state); latching = `is_toggle` (a persistent on/off) | Don't conflate the two — a momentary control backed by a toggle-typed param leaves the graph "stuck on" after one press; a latching control backed by a trigger-typed param never holds state at all. |
| **"Make it pulse / sync to the music"** | **Expose, don't bake.** Give it a modulatable card param; never an internal oscillator baked into the graph. | Peter's rule, verbatim (2026-07-13): *"Beat sync'd stuff shouldn't be baked into the graph, the user has modulation tools to sync sliders."* The card is what makes a preset syncable to the show's tempo; an oscillator wired inside the graph steals that control from the performer — there is no card param left to attach an LFO/Ableton clock to. |

New observed anti-patterns land here as additional rows (no code change
required) — see D9. A row that proves mechanically detectable graduates to a
D8 lint in `node_graph::validate::validate_def` (see the mux-vs-blend warning
below for the precedent: it started as an observed anti-pattern and is now a
warning-level lint).

### Mux vs. blend — why the distinction is a performance rule, not just style

A mux (`node.switch_value` / `node.switch_array`) only executes the selected
branch — the executor's per-frame liveness check skips the dead branch
entirely (`node_graph/execution.rs`, see `FREEZE_COMPILER_MAP.md`
§"execution"). A blend/mix node (`node.mix`, `node.masked_mix`,
`node.wet_dry`, `node.hdr_mix`, …) renders **both** inputs every frame and
crossfades the result — even at `amount = 0` or `amount = 1`, where the
"unused" branch's GPU cost is still paid. Wiring a discrete toggle or a
labeled-mode control to a blend node's crossfade param (instead of using a
mux) is a live-rig performance bug wearing a style choice's clothing: the
`graph_tool validate` warning for this (D8 lint f) says exactly this —
*"a mux switches branches and skips the dead one; blend renders both every
frame."*

## Mechanical reference — `ParamSpecDef` fields not obvious from serde

Anchors: `crates/manifold-core/src/effect_graph_def.rs`, struct `ParamSpecDef`
(~line 450 onward).

- **`wraps` is not implied by `is_angle`** (`effect_graph_def.rs:509`). Angle
  presentation (`is_angle`, `effect_graph_def.rs:490`, degrees-in-the-UI /
  radians-in-storage) and periodicity (`wraps`) are orthogonal flags. FOV is
  angle-typed but must stay clamped; a ±89° tilt or an arc extent must too. A
  card wanting "spins forever without hitching at the rail" sets `wraps`
  explicitly — it is never inferred from `is_angle`.
- **`curve` / `invert`** (`effect_graph_def.rs:474`, `:478`) — the slider's
  response curve (`MacroCurve`, Linear by default) and whether card-left
  drives the param's max instead of its min. Both are part of the
  preset-authored slider surface; the preset JSON is the single home for
  range + curve + invert (no separate runtime override layer).
- **`section`** (`effect_graph_def.rs:523`) — card-bundling group name.
  Contiguous runs of params sharing the same `section` string render under
  one collapsible header on the card. `None` renders as a flat slider list.
  Seeded from the innermost enclosing node-group's display name at expose
  time, or (glTF import) the imported object's group name / a shared
  `"Camera"`/`"Sun"`/`"Environment"` bucket — never derived from graph
  structure at display time; the manifest is the single source.
- **`is_trigger_gate`** (`effect_graph_def.rs:499`) — a narrower flag than
  `is_trigger`: marks the specific `clip_trigger` card param that drives the
  "Clip / Audio / Both" mode row on trigger-responsive generators. An
  explicit tag, not a match on the id string `"clip_trigger"` — don't infer
  it from naming.
- **`osc_suffix`** — the OSC address suffix for this param. Must be unique
  within one card's params when non-empty (D8 error e) — a duplicate silently
  makes one OSC address control two sliders, with only the last-bound one
  visibly moving.

## Hints vs. contracts — what a node's `min`/`max` actually means

`PARAM_RANGE_CONTRACT_DESIGN.md` (2026-07-13) splits a fact that used to be
conflated on every inner-node `ParamDef`: its declared `min`/`max` is a
**display hint** — the default slider travel, nothing more. A card, text
entry, modulation, or OSC value is always free to exceed it; a lerp/blend
factor going past 1 legitimately extrapolates (Bloom's [0,5] `mix.amount`
drive is the canonical legitimate case — "it lets you blow out the image if
you want," Peter's ruling). **Cards own the creative envelope** — curve,
invert, remap, the card's own `[min, max]` — precisely because inner hints
must not restrict it.

A small minority of params instead carry a **contract**
(`manifold_core::effects::RangeContract`, next to `ParamDef`): a named,
real physical/mathematical boundary — an index that must address a real
input slot, a count that sizes an allocation, a floor a kernel divides by
zero at, a shader-side clamp beyond which the input is dead. A contract is
never added without kernel/shader evidence, and every one that exists is
pinned to its reason in a curated test table
(`freeze::classify::tests::every_range_contract_names_a_real_boundary`) —
see that table's comments for the file:line proof behind each entry.
Contracts are what `graph_tool validate`'s lint (h) below actually
enforces; a card range that merely disagrees with a hint is not a finding
— that's cards doing their job, not a bug.

## What `graph_tool validate` checks on a card (D8, mechanized)

**Errors** — structural breakage; the card lies to the performer:

1. A binding's target `node_id`/`param` doesn't resolve to a real node+param
   in this graph (after flatten — a target may legitimately live inside an
   embedded group body).
2. A card param with no binding referencing its id — a dead slider.
3. A mode param (`whole_numbers` + non-empty `value_labels`) whose label
   count disagrees with its integer range's step count.
4. An `is_trigger` card param whose binding targets an inner param that isn't
   trigger-typed.
5. Two params on the same card reusing a non-empty `osc_suffix`.
6. A card's `[min, max]`, mapped through the binding's `scale`/`offset`,
   landing outside the inner param's declared **contract** — see "Hints vs.
   contracts" above. A card disagreeing with the inner node's `min`/`max`
   *hint* is not this check; only an escaped contract is an error.

**Warnings** — idiom/consistency; fix in-session, don't suppress:

7. A discrete control (`is_toggle`, or `whole_numbers` + `value_labels`)
   bound to a continuous blend param on a mix/blend-family node — see "Mux
   vs. blend" above.
8. A card param's `default_value` disagreeing with its binding's
   `default_value`.

`BindingTarget::Composite` targets are not statically checkable (composite
routing is built at live-graph construction time from a runtime handle) — no
bundled preset uses one today, so this is a documented gap, not an observed
miss.
