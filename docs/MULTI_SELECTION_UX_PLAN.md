# Multi-Selection UX — Implementation Plan

> Status: planned, not started. Raised 2026-06-24, planned 2026-06-26.
> Memory pointer: `project_multi_selection_ux`.

Selection state stays where it already lives — the multi-capable `HashSet` channels in `manifold-ui`'s `UIState` for clips/layers/markers, and the per-tab `HashSet<EffectId>` already inside `InspectorCompositePanel`. We add no new selection container and no new shared state; we extend the existing sets and drive every bulk edit through the **already-built** `CompositeCommand` → `EditingService::execute_batch` → `ContentCommand::ExecuteBatch` path (one undo step, one cache/OSC refresh). Mixed-value inspector cells render Ableton-style dashes by adding an `is_mixed` flag that rides the existing per-slot `sync_values` dirty-check; the edit path fans a single `(ParamId, value)` over the resolved selection set on the UI side, building one unit command per object.

The whole feature is UI-side fan-out plus a mixed-value display flag. The model, the command primitives, and the batch channel already exist — nothing below the dispatch sites changes. **The mixed-value inspector (Phase 1) is the real deliverable**; the structural fan-out (Phase 2) is a two-arm cleanup of the only paths that still single-target.

---

## Phase 1 — Mixed-value param edits in the inspector (effects + generators)

**Goal:** Editing a shared param (opacity, blend, a common effect param) writes to all selected objects in one undo step; the inspector shows a dash when the selected objects' values differ.

This is the heart of the feature and splits into a display half and an apply half. Land them together — a mixed display with a single-target apply would be a lie.

**Reachable scope (what the existing selection infra can actually address — do not exceed it):**
- N **effects within one layer chain** (or N master effects) via the inspector's own per-tab `selected_layer_ids`/`selected_master_ids: HashSet<EffectId>`. The inspector renders exactly one scope, and `resolve_graph_target`/`resolve_effect_id` (inspector.rs:180) resolve every index against one `active_layer` + tab — so co-selected effects are within a single chain by construction.
- N **layers' opacity/blend** via `selected_layer_ids: HashSet<LayerId>` in UIState.
- **Effects across multiple layers is explicitly out of scope** — it needs a cross-scope effect-selection container the rest of this plan forbids (see Out of Scope). Do not promise it in any done-criterion.

### 1a — Mixed-value display

**Files / types touched:**
- `crates/manifold-ui/src/panels/param_card.rs` — add `is_mixed: bool` to `UiParamSlot` (view.rs:94 — it is `Copy`/`Default`, trivial); `sync_values_effect`/`sync_values_generator` (param_card.rs:1879/1996) compute "all-equal across the resolved target set → value, else mixed" and set the flag; `BitmapSlider::update_value` / `format_param_value` render `—` and a neutral handle position when mixed. The reduction is over the **small same-scope N** (one chain, or N layers' opacity) — not the 128-effect worst case — so it rides the existing per-slot `sync_values` dirty-check with no new per-frame machinery.
- `crates/manifold-app/src/ui_bridge/state_sync.rs` — `sync_inspector_data` (1076) and `push_state` (739) read the resolved selected set rather than the single active layer / primary, and reduce per-`ParamId` across the N `PresetInstance.param_values`. Use the existing id->index caches; no per-frame `Vec` rescans.
- Aggregate by **shared `ParamId`** (via `param_id_to_value_index`), so heterogeneous instances contribute only the params they share. Objects lacking a param are skipped.

### 1b — Mixed-value apply (the batched-undo fan-out)

**This is `EffectToggle` with `ChangeGraphParamCommand` swapped for `ToggleEffectCommand`** (inspector.rs:973-1033 is the template): loop the selected targets, build one command per target with its own captured `old_value`, skip unchanged, send one `ExecuteBatch`, apply the `len==1` bare-`Execute` short-circuit (editing_host.rs:541).

**Files / types touched:**
- `crates/manifold-app/src/ui_bridge/inspector.rs` — `dispatch_inspector`'s `ParamChanged` (1194) / `ParamCommit` (1224) arms call `resolve_graph_target` once **per selected object** to get a `Vec<GraphTarget>`, then:
  - **Live drag:** loop `MutateProjectLive` + the local-mirror `with_preset_graph_mut` write over **all N targets, with NO unchanged-skip** — every selected object must get both writes each move or some snap back on the next snapshot. The unchanged-skip happens only at commit.
  - **Commit:** build one `ChangeGraphParamCommand` per target, each capturing **its own** `old_value` (read per object at drag start — never a shared old value), skip unchanged objects, and send one `ContentCommand::ExecuteBatch`.
- `crates/manifold-ui/src/panels/param_card.rs` — store the per-object old values in `ActiveInspectorDrag::Param` (app.rs:69) by adding an `old_values: Vec<(GraphTarget, f32)>` field captured at `ParamSnapshot`. **Do NOT touch the shared `drag_snapshot: Option<f32>` (mod.rs:128)** — it is threaded into every drag type (macros/master/LED/layer opacity/clip slip/loop) and stays as-is for the unchanged-value short-circuit.
- `GraphTarget::Generator(LayerId)` already addresses generators uniformly with effects, so N generators reached via the existing layer selection fan out the same way.

**Done looks like:** Select 3 effects of the same type **in one layer chain**, drag the Amount slider → all 3 move live, drag-end is one undo entry, Cmd-Z restores each to its own prior value. With differing starting values the cell reads `—` until you commit; absolute-set then applies the dragged value to all. Across N layers, dragging opacity moves all N as one undo step.

---

## Phase 2 — The two structural arms that still single-target

**Goal:** Close the two remaining structural actions that do not yet fan over the selection. Everything else is already done — **do not touch it.**

**Already done (reference only, do not modify):**
- Clip delete/cut/duplicate/nudge — `input_host.rs:522-666` already resolves `get_selected_clip_ids()` (region-aware) and sends one `ExecuteBatch`.
- Layer delete — `input_host.rs:911` already fans + batches.
- Marker delete — `input_host.rs:1391-1409` already batches.
- Layer mute/solo/blend/header-collapse — `layer.rs:37-380` already fan over `selected_layer_ids`.

**The two genuinely-missing arms:**
- `crates/manifold-app/src/ui_bridge/layer.rs` — `CollapseLayer`/`ExpandLayer` (layer.rs:310) currently operates on one index via a bare `MutateProject` (no undo, no fan-out). Make it fan over `selected_layer_ids` and route through a command so collapse becomes undoable and multi.
- `crates/manifold-app/src/ui_bridge/editing.rs` — `ContextDeleteClip` (editing.rs:151) deletes only the right-clicked clip and loops bare `Execute` (N undo steps). When the right-clicked clip is in the current selection, resolve `get_selected_clip_ids()` and send one `ContentCommand::ExecuteBatch`; otherwise keep the single-clip path.

Apply the `len==1` short-circuit convention (editing_host.rs:541) so a single-object edit stays a bare `Execute` and undo labels are byte-for-byte unchanged.

**Done looks like:** Collapse over a multi-layer selection is one undoable step that hits all selected layers. Right-click-delete on a clip that's part of a multi-selection removes all selected clips in one undo step.

---

## Deferred / not a phase

- **Marquee materializing a clip set.** `box_select` (`clip_hit_tester.rs:136`) is written, unit-tested, group-skipping, and has **no production caller**. Structural ops already work on a marquee via the lazy region resolver (`input_host.rs::get_selected_clip_ids` → `get_clips_in_region`), so the only gap is the visual/inspector lag where boxed clips aren't highlighted until an op runs. Wire `box_select` into `update_region_drag` only if that lag is felt on stage.

---

## Open design questions — recommended answers

**(a) How does the inspector render/edit a mixed-value set?**
**Recommendation: one aggregate card per scope with a per-slot `is_mixed` flag — not N stacked cards.** Reuse the single active card (`master_effects`/`layer_effects`/`gen_params` Vecs), keep `reconcile_cards`' identity keying, and aggregate values by shared `ParamId`. A mixed slot renders `—` (Ableton convention) and sits the handle neutrally; type-in / double-click on a mixed cell opens **empty** rather than prefilling one object's value (stale-prefill is a correctness trap). Editing absolute-sets the new value to all. *Relative-nudge-preserving-spread is deferred — see Out of Scope.* Stacked N-cards would multiply the per-frame node-build cost and reintroduce the index-based `Effect(usize)` addressing problem the aggregate avoids.

**(b) How is the bulk edit one undo step?**
**Recommendation: reuse the existing `CompositeCommand`. Do not build a new batch/transaction command.** `CompositeCommand` (command.rs:24) already groups `Vec<Box<dyn Command>>` into one undo entry (in-order execute, reverse undo, one slot against the 200 cap). The UI builds the `Vec` of per-object `ChangeGraphParamCommand`s (each with its own `old_value`), sends one `ContentCommand::ExecuteBatch`, and the content handler (content_commands.rs:202) runs `execute_batch` plus the post-mutation maintenance exactly once. Crucially the `Vec` is assembled UI-side and sent as **one** channel message — never one `ContentCommand` per object (that floods the bounded-64 channel and creates N undo steps). Apply the `len==1` short-circuit so a one-survivor edit stays a bare `Execute`.

---

## Out of scope / not now

- **No unified `SelectedObject` enum / no merging the four selection stores.** The graph-canvas node set stays independent; the inspector effect sets stay per-tab. Consolidation is a refactor the feature doesn't need.
- **No effects across multiple layers.** The inspector renders one scope and resolves every effect index against one `active_layer`+tab, so co-selected effects are within a single chain. True cross-layer effect multi-edit needs a new cross-scope effect-selection container — exactly the new shared selection model this plan forbids. It is a separate, larger follow-up, not part of this feature.
- **No cross-kind mixed selection** (clips AND layers selected together). Relaxing UIState's clip/layer mutual-exclusion is load-bearing for tab derivation and `is_layer_active`; leave it.
- **No generator *selection set*.** Generators remain addressed via `GraphTarget::Generator(LayerId)`; Phase 1 reaches N generators only through the existing layer selection.
- **No relative-nudge-preserving-spread edit mode yet.** Ship absolute-set first; relative is a second edit mode to add only once absolute is proven on stage.
- **No new SelectableObject in core, no new shared `Arc<Mutex>`, no new composite command.** All three already exist or are unnecessary.
- **No N-stacked-card inspector layout.** Aggregate card only.
- **Per-clip effects (legacy `TimelineClip.effects`) stay untouched** — they are `skip_serializing` and must not be surfaced.
