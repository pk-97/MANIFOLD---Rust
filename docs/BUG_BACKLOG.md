# Bug Backlog

<!-- index: Live, human-and-agent-facing tracker for known bugs not yet fixed. Each entry has a stable ID, a root-cause location, the user-visible symptom, a fix shape, and (when one exists) an #[ignore]'d test that goes green when fixed. -->

The repo had no bug tracker — bug knowledge lived only in agent memory, git history, and
session context. This file is the durable, in-repo home. It travels with the code, any agent
or human can read it, and it needs no external tool.

## How to use this file

- One entry per known bug, with a stable ID (`BUG-NNN`). Never renumber — IDs are referenced
  from commits, tests, and memory.
- The strongest form of an open entry is an **executable** one: an `#[ignore = "BUG-NNN"]`
  test that fails for the right reason. The bug is then self-documenting and self-closing —
  remove the `#[ignore]` when the fix lands and the suite enforces it forever.
- When you fix an entry, move it to **Fixed** with the commit SHA. Don't delete it — the
  history is the point.
- Severity is about the **instrument on stage**, not code aesthetics: `HIGH` = wrong output
  or silent data corruption a performer would hit; `MED` = reachable but narrow; `LOW` =
  latent / cosmetic / needs an unusual setup.

---

## Open

BUG-006–014 come from the **freeze-compiler adversarial bug hunt, 2026-07-03**
(40-agent Sonnet workflow `wf_73bb4ddf-885`; 10 finder lenses → every finding attacked by 2
independent skeptics). BUG-006–012 were **confirmed by both skeptics** with line-level
evidence; BUG-013/014 got split verdicts (judgment recorded per entry). Full verifier
transcripts: the workflow journal at
`~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/18511d71-15ae-4119-81cc-894a3f83d247/subagents/workflows/wf_73bb4ddf-885/journal.jsonl`.
System context for all of them: [FREEZE_COMPILER_MAP.md](FREEZE_COMPILER_MAP.md).

### BUG-006 — Param edits/undo on fused-away nodes silently no-op until an unrelated rebuild — HIGH

**Root cause** — [bound_graph.rs:114-133](../crates/manifold-renderer/src/node_graph/bound_graph.rs#L114-L133):
`apply_inner_param_overrides` looks each node's `node_id` up in `slot.node_map` and silently
`continue`s on a miss. For a fused card, `node_map` is built from the FUSED def
([preset_runtime.rs:1285-1288](../crates/manifold-renderer/src/preset_runtime.rs#L1285-L1288)),
so fused-away members (e.g. `gain`) aren't in it. The path never consults the fused view's
`fused_retarget` map (which knows `gain.gain` → `fused_region_0.n0_gain`). Value-only edits
bump only `graph_version`, which is deliberately not in `compute_topology_hash`, so no rebuild
fires.

**Symptom** — edit a param in the editor, close it (re-fuses, bakes the value), then Undo
while viewing another effect: the def reverts but the fused kernel keeps rendering the OLD
value indefinitely, until a resize/editor-open/unrelated edit forces a rebuild. Live control
stranded, zero errors. `CHAIN_FUSION_DESIGN.md` §6 already flags this as an open item.

**Fix shape** — thread the fused view's `fused_retarget` into `apply_inner_param_overrides`
(or into `node_map` construction): on a `node_map` miss, translate `(node_id, param)` through
the retarget map to `(fused node, n{i}_field)` and apply there. Test: fuse, value-edit,
assert the fused node's param moved without a rebuild.

### BUG-007 — Particle-loop fusion exclusion is blind to configured `node.wgsl_compute` shapes — HIGH

**Root cause** — [region.rs:834](../crates/manifold-renderer/src/node_graph/freeze/region.rs#L834):
`cycle_contains_array` uses a bare `registry.construct(type_id)` — the ONE hold-out in the
file; every other classification call site uses `configured_construct`, whose own doc comment
states why the bare form is wrong. A full-kernel `node.wgsl_compute` with a
`var<storage, read_write> array<Particle>` output (StrangeAttractor's "simulate" node is a
shipped instance) introspects as the DEFAULT kernel (no Array output) under the bare
construct, so the cycle scan can't see the particle stage.

**Symptom** — a texture atom on a feedback loop whose only Array producer is such a node
passes cut rule 12 and fuses tier-A f16 in-loop, where the bit-exact induction argument does
not hold across a particle/scatter stage (FluidSim precedent: max_abs ~0.73 over ~31% of
pixels). Fused render visibly diverges from the editor.

**Fix shape** — one line: use `configured_construct(registry, node)` in
`cycle_contains_array`. Sweep the file for any other bare-construct hold-outs
(`node_is_buffer_atom` / `region_is_buffer` at
[region.rs:1885-1905](../crates/manifold-renderer/src/node_graph/freeze/region.rs#L1885-L1905)
have the same pattern — audit while there). Test: a loop through a configured wgsl_compute
particle node must classify its texture atoms Boundary.

### BUG-008 — Fused buffer region with mismatched array lengths reads out of bounds — HIGH

**Root cause** — [codegen.rs:1777-1813](../crates/manifold-renderer/src/node_graph/freeze/codegen.rs#L1777-L1813):
`generate_fused_buffer` anchors the dispatch guard to the FIRST array external's
`arrayLength`, then unconditionally pre-reads EVERY array external at that index. Nothing
anywhere (classify, union, `build_region`, `fused_def_builds`) checks that a buffer region's
array externals agree on length — the tier-6 uniformity gate is texture-only. The unfused
atom (e.g. `LerpInstanceFields`) explicitly clamps to `min(a_cap, b_cap, out_cap)`.

**Symptom** — two array inputs of different lengths fuse; for indices past the shorter
buffer the kernel does an out-of-bounds Metal storage read and writes garbage
instances/particles to the output — silent visual corruption. Shipped presets happen to share
lengths today; user graphs are unprotected.

**Fix shape** — either refuse at `build_region` when a buffer region has >1 array external
(conservative, fail-closed, cheapest), or emit a per-external in-bounds guard
(`idx < arrayLength(&src_e)` with a defined fallback element). Pair with BUG-011.

### BUG-009 — Segment "stateless" gate misses StateStore-held scalar state; harvest skip resets it — HIGH

**Root cause** — [segment.rs:153-171](../crates/manifold-renderer/src/node_graph/freeze/segment.rs#L153-L171):
`def_is_segment_stateless` checks only `state_capture_input_ports` + `aliased_array_io`.
Primitives that hold real cross-frame state in the StateStore without declaring either —
`sample_and_hold`, `envelope_decay`, `trigger_ease_to`, `compressor_envelope`,
`envelope_follower_ar`, `inject_burst` — pass as stateless. Segment member slots get
`def_content_key: 0` ([preset_runtime.rs:1105](../crates/manifold-renderer/src/preset_runtime.rs#L1105))
and `harvest_state_from` skips them
([preset_runtime.rs:1693](../crates/manifold-renderer/src/preset_runtime.rs#L1693)), so any
chain rebuild drops their state.

**Symptom** — AutoGain (shipped: `compressor_envelope` next to pointwise atoms) joins a
segment; any rebuild while it's a member — editor open/close elsewhere, an unrelated card
edit, or the fused-segment swap-in itself — resets the envelope: gain snaps to unity, a
visible/audible pop mid-show. Violates the chain-fusion design's own "never resets state"
invariant.

**Fix shape** — the root fix is a truthful statefulness signal: a `NodeRequires`-style
`uses_state_store` flag (or derive it from `ctx.state` usage) that `def_is_segment_stateless`
also checks. Stop-gap is a hard-coded exclusion list, which is exactly the pattern the freeze
module refuses everywhere else — prefer the flag.

### BUG-010 — `wgsl_compute` silently dispatches the first of multiple entry points — MED

**Root cause** — [wgsl_compute.rs:615-624](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L615-L624):
`introspect()` takes `module.entry_points[0]` with no `len() == 1` check (the module doc at
lines 29-31 claims multiple entry points fail validation — they don't). The pipeline compile
independently picks the same first entry. A fragment-form node embeds the author's raw text
BEFORE the synthesized `cs_main`, so any leftover `@compute fn` in the fragment becomes
entry 0 and is what actually runs. Verified empirically by a skeptic (scratch test:
`compile_failed=false`, `debug_pass` dispatched, real kernel never runs).

**Symptom** — a user kernel/fragment with a stray second `@compute` function (debug leftover,
copy-paste) renders stale/blank output with no warning; downstream wires read it as if it
worked. Authoring-time surface, so MED — but it's the exact silent-wrong-output class.

**Fix shape** — in `introspect()`: if the module has >1 compute entry point, prefer `cs_main`
by name; if absent, fail validation with the warning the doc already promises. Keep the
dispatch-side pick in lockstep.

### BUG-011 — Fused `@fused_output` buffer sized to max of ALL array inputs, not the member's own rule — MED

**Root cause** — [wgsl_compute.rs:1828-1829](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L1828-L1829):
the fresh-output branch of `array_output_capacity` returns
`input_capacities.max()` generically, overriding the fused output member's own semantic
capacity rule (e.g. `LerpInstanceFields` follows only input `a`). Downstream consumers
(`render_instanced_3d_mesh` computes capacity from physical buffer size) can then draw ghost
instances from the never-written tail.

**Symptom** — with mismatched input lengths (same shape as BUG-008), the fused output buffer
is larger than the unfused chain's, and its tail is uninitialized pooled VRAM — potential
stale-data ghosting across preset/frame boundaries.

**Fix shape** — falls out of BUG-008's decision: if multi-external buffer regions are
refused, this is unreachable; if guarded instead, size `dst` from the anchor external and
zero-fill or guard the tail.

### BUG-012 — Fragment `tex_` port-rename corrupts scalar params named `tex_*` — LOW

**Root cause** — [wgsl_compute.rs:544-548](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L544-L548):
the fragment-form rename loop strips a literal `tex_` prefix from EVERY input port name with
no type filter (the sibling texture-binding rename at 549-561 IS filtered to
`SampledTexture`). A scalar `@param: tex_speed` exposes port `speed` while the uniform layout
and params stay keyed `tex_speed`; the dispatch-time wire lookup misses and the live wire is
silently ignored.

**Symptom** — a wired LFO/Ableton control on such a param renders as connected but never
moves the value. Latent — no shipped preset uses a `tex_`-prefixed param name.

**Fix shape** — filter the rename to texture-typed ports, mirroring lines 549-561. One-line.

### BUG-013 — `commit_and_wait_completed` never checks command-buffer status (likely the GPU-proof flake mechanism) — MED

**Root cause** — [encoder.rs:1655-1662](../crates/manifold-gpu/src/metal/encoder.rs#L1655-L1662):
`waitUntilCompleted()` returns on ANY terminal state including `Error`; no caller checks
`status()`/`error()`. Every heavy freeze proof and `TextureDiff::compare` submit through this
call and read the result back as if it succeeded. Under cross-binary GPU contention
(documented in `.config/nextest.toml` and the `GPU_TEST_LOCK` comment; three call sites build
unlocked devices), a transiently failed buffer reads back stale/partial → spurious large diff.

**Status** — split verdict, judged REAL-as-flake-mechanism: it precisely explains the
observed signature (several heavy tests, random divergence sizes, never reproducing
isolated). It is test-infra, not a compiler miscompile — but it gates trust in the entire
oracle suite, so it blocks using the suite as a hard gate for agent work.

**Fix shape** — check the buffer's terminal status in `commit_and_wait_completed`; on error,
panic in tests (fail loudly, retryable) and log in production. Then re-baseline the flake:
if red runs now report command-buffer errors instead of pixel diffs, the mechanism is
confirmed; if divergences persist with clean status, keep hunting.

### BUG-014 — Content key collapses NaN/±Inf param values to one hash — LOW (parked)

**Root cause** — [install.rs:205-215](../crates/manifold-renderer/src/node_graph/freeze/install.rs#L205-L215):
`def_content_key` hashes `serde_json::to_vec(def)`, and serde_json writes non-finite floats
as `null`, so defs differing only in a non-finite param share a key while the fuse bakes the
raw f32.

**Status** — split verdict, judged UNREACHABLE today: the second skeptic traced every write
path into node params (scrub handlers clamp to finite ranges; JSON round-trips reject
non-finite). Parked as a hardening note — if a new param write path ever skips the clamp,
this becomes live. Cheapest closure: reject non-finite values at the `SerializedParamValue`
boundary (the eliminate-bug-class-at-storage-layer pattern).

### BUG-015 — Inspector sections render overlapping / at stale offsets after scroll — MED (repro needed)

**Symptom** — observed once by Peter, 2026-07-04, right after the timeline-P0 / multi-select
UX changes landed: the layer inspector drew its sections interleaved — the MIDI block
(MIDI / CHANNEL / DEVICE) and the audio-send block (send dropdown, +0.0 dB) overlapping
each other with a dead band between them, and the "No audio input" header clipped mid-panel.
Described as "a scrolling bug with the UI timeline updates". Screenshot lives in the
2026-07-04 session transcript.

**Root cause** — unknown. Suspect surface: inspector section Y-layout vs. scroll offset
(the `single-source-y-layout` invariant) or a stale subregion scissor
(`subregion-scissor-invariant`) going stale when timeline updates force a rebuild while the
inspector is scrolled.

**Repro** — not yet pinned. First step is reproducing: select a generator layer, scroll the
inspector, then trigger timeline churn (clip drag / multi-select updates) and watch for
section overlap.

**Fix shape** — TBD after repro. If it's the known invariant class, the fix is at the layout
single-source, not per-section patches.

### BUG-016 — Imported .glb layers are black boxes: no card params, no Model File picker, edit paths silently no-op — FIXED 2026-07-04 (`2d5e4dc6`)

**Resolution** — PRESET_LIBRARY P0 (D9) shipped: the drop now registers the assembled
graph as a project-embedded preset (`origin: Saved`) and the layer TRACKS it (`graph:
None`); the assembler emits a curated 13-slider card (camera/sun/envmap/per-object
material) with real bindings; the app installs the catalog overlay before the layer is
created, so the process-global preset registry seeds `init_defaults` consistently on both
threads. The `graph_def_mut` override install is deleted. verify-at-impl #4 resolved
(`bundled_preset_json` reads the overlay-merged catalog, no change needed). Assembler +
command tests + GPU render proofs green. **Still owed: the live drag-drop manual gate** in
a running app (card sliders move pixels, editor opens on the cog, save/reload intact) — the
one thing only Peter can eyeball. Original analysis below for reference.

**Root cause** — the glTF Stage-4 install mints a preset id that resolves in no catalog and
stashes the def only on the layer
([app_lifecycle.rs:506](../crates/manifold-app/src/app_lifecycle.rs#L506),
[layer.rs:100](../crates/manifold-editing/src/commands/layer.rs#L100)). Every type-keyed
surface then fails independently: the assembler emits empty `params`/`bindings`
([gltf_import.rs](../crates/manifold-renderer/src/node_graph/gltf_import.rs), metadata block)
so the card is empty; generator string params are sourced from the registry only
([inspector.rs:2251](../crates/manifold-app/src/ui_bridge/inspector.rs#L2251)) so the Model
File picker never shows; the editor's catalog default is `None`, which gates several edit
dispatch arms into silent no-ops (e.g. [app.rs:1356](../crates/manifold-app/src/app.rs#L1356)).
The reported empty editor canvas is NOT fully root-caused: `GraphSnapshot::from_def` on the
assembled def is proven good (12 nodes / 10 wires), so the entry path loses the watch target —
observe at repro.

**Fix shape** — `PRESET_LIBRARY_DESIGN.md` P0 (D9): the drop registers an `EmbeddedPreset`
and the layer tracks it; assembler emits curated performance bindings. Not per-consumer
fallbacks.

### BUG-017 — `docs_index_is_in_sync_with_docs_dir` red on main: two design docs never regenerated the index — LOW

**Symptom** — found 2026-07-04 running the full workspace sweep for the automation-P4
landing (unrelated to that work — pre-existing on origin/main before the landing branch
touched anything, confirmed via `git show 90ab8531:docs/README.md`).
`cargo test -p manifold-core --test docs_index_sync` fails:
`docs/README.md is out of sync with docs/. Missing from the index: ["AUDIO_SENDS_UX_DESIGN.md",
"TIMELINE_INGEST_DESIGN.md"]`.

**Root cause** — two sessions added design docs (`AUDIO_SENDS_UX_DESIGN.md`,
`TIMELINE_INGEST_DESIGN.md`) without re-running the generator afterward.

**Fix shape** — mechanical: `python3 scripts/gen_docs_index.py`, commit the regenerated
`docs/README.md`. Not fixed this session because other sessions were actively adding more
docs concurrently — regenerating now risked going stale again within the hour. Whichever
session next touches `docs/` and finds the tree quiet should run the generator and close
this out.

### BUG-018 — `node_graph::catalog_gen::tests::regenerates_in_sync` red on main: `docs/node_catalog.json` stale against the node registry — LOW

**Symptom** — found 2026-07-04, same full-workspace sweep as BUG-017, same shape: confirmed
pre-existing on origin/main (`90ab8531`) before the automation-P4 landing branch touched
anything — reproduced standalone in a disposable worktree at that exact commit.
`cargo test -p manifold-renderer --lib node_graph::catalog_gen::tests::regenerates_in_sync`
fails with `docs/node_catalog.json is stale`.

**Root cause** — not investigated; some session added/changed a node-graph primitive without
re-running `cargo run -p manifold-renderer --bin gen_node_catalog` afterward. Given `node_count`
sits at 214 in the checked-in file, worth diffing against the live-generated output to see
which node(s) are missing/changed before just overwriting.

**Fix shape** — mechanical: `cargo run -p manifold-renderer --bin gen_node_catalog`, commit
the regenerated `docs/node_catalog.json`. Same reasoning as BUG-017 for not fixing it this
session (unrelated to the work at hand, and worth doing once rather than mid-churn).

### BUG-019 — Motion "group fold" (D17) has no UI surface to fold — DESIGN GAP (deferred)

**Symptom** — found 2026-07-04 completing UI motion P2. D17 lists "group fold: children
collapse into header," but the animation has nothing to animate: `EffectGroup.collapsed`
exists at the model layer (`crates/manifold-core/src/effects.rs:3194`) with zero rendering
surface — no group header, no collapse toggle, no child-card grouping by `group_id` in the
inspector (`rg EffectGroup crates/manifold-ui/src` → 0 hits).

**Root cause** — the design assumed a foldable effect-group UI in the inspector that was
never built. Group fold is a *new feature* (group header + child-card filtering + collapse
toggle), not an animation retrofit — correctly out of the motion layer's scope.

**Fix shape** — build the effect-group inspector UI first (own small design: header row,
`group_id`-keyed child filtering, collapse toggle), THEN the fold animation is a `FlipList`
+ exit-state retrofit like the other P2 collapses. Needs a design/build decision from Peter.

### BUG-020 — Card collapse animates effect cards but not generator cards — LOW (deferred)

**Symptom** — found 2026-07-04 (UI motion P2 batch 1). Effect cards collapse/expand with the
`collapse_anim` reflow; generator cards do not — their rows parent at root (`None`) in
`ParamCardPanel::build_generator`, so there is no `ClipRegion` seam to clip the collapsing
body the way `build_effect` has.

**Fix shape** — give `build_generator` the same parent/clip-region seam `build_effect` uses,
then reuse the existing `collapse_anim`. Small, localized to `param_card.rs`.

### BUG-021 — Value snap-back is Perform-inspector only, not the graph-editor param cards — LOW (deferred)

**Symptom** — found 2026-07-04 (UI motion P2 closer). Right-click value-reset eases the fill
(EASE_SNAP) on Perform-context inspector cards; the graph editor owns a separate
`ParamCardPanel` instance not reachable from the `ParamRightClick` dispatch site
(`ui_bridge/inspector.rs:1140`), so its value resets snap without the settle.

**Fix shape** — thread the snap-back trigger to the graph-editor's `ParamCardPanel` too, or
lift the reset-with-settle into shared `ParamCardPanel` logic both dispatch sites reach.

### BUG-022 — Main-window browser popup: Escape while the search field is focused cancels the text session but leaves the popup open — LOW (found during OVERLAY_SESSIONS_AND_PICKER_DESIGN P1/P2, not fixed this session)

**Symptom** — found 2026-07-04 auditing `window_input.rs`'s keyboard routing while
implementing `docs/OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`. For the MAIN window (effect/
generator browser), once the search field has focus (`self.text_input.active &&
field == SearchFilter`), every keystroke is intercepted by the `if self.text_input.active { ... }`
block in `window_input.rs` (`primary_keyboard_input`, ~line 1593) before it ever reaches
`UIRoot::process_events`/`route_overlay_event`. Its `Key::Named(NamedKey::Escape)` arm calls
only `self.text_input.cancel()` — it never touches `self.ws.ui_root.browser_popup`. So Escape
while typing clears the search text and ends the text session, but the popup itself stays
open; a second Escape (now routed normally, since `text_input.active` is false) is needed to
actually dismiss it. This is plausibly the exact mechanism behind Peter's original report
("the search and text seems to stay after you search and need to click elsewhere again to
close it properly") — P1's stash-and-drain fix (`TextSessionOwner`/`take_closed_overlays`)
closes the *orphaned-session-after-popup-closes-elsewhere* class, but this is the inverse:
popup not closing when the session ends.

Note the EDITOR window's analogous bespoke branch (`window_input.rs` ~1145, node picker) does
NOT have this gap — its Escape arm already calls `browser_popup.handle_escape()` directly
alongside cancelling the text input (now also wired through `note_overlay_closed_if` as part
of this session's P1 work).

**Root cause** — the main-window `text_input.active` Escape arm was written before the browser
popup existed as an `Overlay`-driven modal; it only ever needed to cancel a plain text field.
Nothing updated it when `BrowserPopupPanel` started hosting a `SearchFilter` session.

**Fix shape** — in the main-window Escape arm, when `self.text_input.field == SearchFilter`,
also call `self.ws.ui_root.browser_popup.handle_escape()` (mirroring the editor's branch) instead
of only `self.text_input.cancel()`. Small, localized to `window_input.rs`'s
`if self.text_input.active` block — no design-doc scope change, since this is a pre-existing
gap outside P1/P2's stated deliverables (which target orphaned-session-on-close, not
missing-close-on-cancel).

### BUG-023 — `no_new_raw_color_literals` red on main: real count (201) one above baseline (200) — FIXED 2026-07-05 (in the P6 landing)

**Resolution** — the extra raw literal was localized (not a "prior session" — it was THIS
orchestration's own P5 landing `0d6e857e`): `browser_popup.rs` carried
`const BADGE_TEXT: Color32 = Color32::new(130, 130, 134, 255)` for the origin-badge text,
added by P5 and missed because that phase ran clippy + focused tests but not the
`design_tokens` integration guard. Fixed by tokenizing it into `color::BROWSER_CELL_BADGE_TEXT`
(color.rs is the scan's exempt token home), dropping the counted set back to 200. Guard green.
Lesson for the orchestration: run `-p manifold-ui --test design_tokens` on any phase that
adds UI color, not just clippy. Original analysis below.

**Symptom** — found 2026-07-05 running the full gate for `PRESET_LIBRARY_DESIGN.md` P6
(thumbnails). `cargo test -p manifold-ui --test design_tokens no_new_raw_color_literals` fails:
`Raw Color32::new( count rose to 201 (baseline 200)`. Confirmed pre-existing and unrelated to
P6: re-ran the same scan logic against `git show HEAD:<path>` for every file under
`crates/manifold-ui/src` (a standalone Python re-implementation of `scan()`/`classify()`) and got
201 on HEAD alone, before any P6 edit — the P6 changes to `browser_popup.rs`/`color.rs` net to
**zero** new raw literals (three new cells' worth of `Color32::new(` were added to `color.rs`,
which the scan excludes as the token home, and the matching local consts in `browser_popup.rs`
were pointed at those new tokens instead of a raw literal — no net change to the counted set).

**Root cause** — not investigated; some prior session's commit added exactly one raw
`Color32::new(` line somewhere under `crates/manifold-ui/src` without bumping
`COLOR_BASELINE` in `crates/manifold-ui/tests/design_tokens.rs` (or without using a
`// design-token-exempt:` comment for a genuine one-off). `git bisect`/`git log -S"Color32::new("`
over the file list the scan touches would localize it quickly; not run this session since it's
orthogonal to P6 and risked burning session budget chasing an unrelated one-line drift.

**Fix shape** — mechanical, one of: (a) find the extra raw literal and tokenize it (count back to
200, no baseline change), or (b) if it's a genuine one-off, add `// design-token-exempt: <reason>`
on that line (count back to 200), or (c) bump `COLOR_BASELINE` to 201 if it's accepted debt. Not
fixed this session — the gate confirms the diff at hand is P6-clean; picking apart an unrelated
pre-existing count belongs to whoever next touches `manifold-ui/src`'s colour call sites.

## Fixed

All five entries below were fixed 2026-06-23, with a test per path:
- BUG-001–004 — commit `2e3dc4f3` (`PresetInstance::duplicated()`, both paste paths, `Clip::clone_with_new_id`, `Layer::clone_with_new_ids`).
- BUG-005 — commit `9f43f183` (macros address effects by `EffectId`; versioned load migration).

The fresh-copy carry-rule (id always fresh; drop Ableton/MIDI + audio mods; drop cross-chain group; keep drivers/envelopes) is settled and lives in `PresetInstance::duplicated()`.

### BUG-001 — Pasting an effect shares the source's `EffectId` — HIGH — ✅ FIXED (`2e3dc4f3`)

Copy/paste of an effect card clones the `PresetInstance` verbatim and keeps the original's
`EffectId`. Nothing mints a fresh id. The two cards then share one identity, and the whole
system addresses effects by id with **first-match-wins** resolution, so they collide.

**Root cause**
- Clipboard clones verbatim: [clipboard.rs:32-34](../crates/manifold-editing/src/clipboard.rs#L32-L34) (`get_paste_clones` is a bare `.clone()`; `.clone()` copies the `id` field).
- Paste path 1: [input_host.rs:263-273](../crates/manifold-app/src/input_host.rs#L263-L273) (`handle_effect_paste`) — feeds the clone to `AddEffectCommand`, no `regenerate_id()`.
- Paste path 2: [app_render.rs:1907-1918](../crates/manifold-app/src/app_render.rs#L1907-L1918) (PanelAction paste) — same omission.

**Symptom (user-visible)**
- Move a slider on one card → the other card's value moves too.
- Undo/redo of an edit to one card hits the other (or the wrong one).
- The two cards share GPU/visual state (feedback trails, sim buffers) — see blast radius below.

**Why each symptom happens**
- Edits resolve via `Project::find_effect_by_id_mut` ([project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947)) and `set_base_param_by_id` — first match by id wins, so card B's edit lands on card A.
- Undo/redo commands store an `EffectId` and re-resolve the same way.
- The renderer's per-frame chain rebuild `harvest_state_from` ([preset_runtime.rs:1667-1743](../crates/manifold-renderer/src/preset_runtime.rs#L1667-L1743)) matches cards by first-match `EffectId` (lines 1684, 1697-1701). Two same-id slots in one chain both match the *same* prior slot → GPU node impls + `StateStore` buckets migrate to the wrong/shared card.

**Correct pattern to mirror**
`Layer::clone_with_new_ids` already does this right — it calls `effect.regenerate_id()` on
every cloned effect ([layer.rs:886-900](../crates/manifold-core/src/layer.rs#L886-L900)).
`PresetInstance::regenerate_id` is at [effects.rs:1768](../crates/manifold-core/src/effects.rs#L1768).

**Fix shape**
Call `fx.regenerate_id()` before building the `AddEffectCommand` in both paste paths. Decide
the `group_id` question (see BUG-003) and the carried-binding question (see BUG-004) in the
same pass. Add a paste test mirroring the graph-node one.

**Test:** none yet. Add `effect_paste_assigns_fresh_id` to `manifold-editing`.

---

### BUG-002 — `Clip::clone_with_new_id` doesn't regenerate nested effect ids — MED — ✅ FIXED (`2e3dc4f3`)

Same class as BUG-001, one layer down. `Clip::clone_with_new_id` mints a fresh `ClipId` but
bare-`.clone()`s everything else, including `effects: Vec<PresetInstance>`
([clip.rs:105](../crates/manifold-core/src/clip.rs#L105)). So a duplicated clip's effects keep
the **source clip's** `EffectId`s. Clip effects share the same first-match namespace
([project.rs:938-944](../crates/manifold-core/src/project.rs#L938-L944)).

**Root cause**
[clip.rs:168-172](../crates/manifold-core/src/clip.rs#L168-L172) — shallow clone of nested effects.

**Every clip-duplication path inherits it** (all funnel through that one function):
- Paste clip — [service.rs:452](../crates/manifold-editing/src/service.rs#L452)
- Duplicate clip — [service.rs:740](../crates/manifold-editing/src/service.rs#L740)
- Split clip (overlap-driven + explicit) — [layer.rs:616](../crates/manifold-core/src/layer.rs#L616), [SplitClipCommand](../crates/manifold-editing/src/commands/clip.rs#L599)
- Trim / copy-in-region — [service.rs:628](../crates/manifold-editing/src/service.rs#L628)
- Duplicate layer — [layer.rs:871](../crates/manifold-core/src/layer.rs#L871) (clones clips, never touches their effect ids)

**Symptom**
Editing an effect on a duplicated/split clip crosstalks with the source clip's effect.
**Split is the surprising trigger** — a user doesn't think of splitting a clip as
"duplicating," but it produces two clips silently sharing effect ids.

**Scope note:** only bites clips that carry effects (effects usually sit on layers, so this is
the less-traveled path — hence MED, not HIGH). Renderer state does **not** collide across
clips: clip chains have distinct `OwnerKey` per clip ([state_store.rs:30-34](../crates/manifold-renderer/src/node_graph/state_store.rs#L30-L34)), so the model-layer collision is the whole bug here.

**Fix shape**
Make `Clip::clone_with_new_id` deep-regenerate `cloned.effects[*].id` (and clip-effect
`group_id` if any). One function fixes all six entry points, including the layer-dup gap.

**Test:** none yet. Add `clip_clone_assigns_fresh_effect_ids` to `manifold-core`.

---

### BUG-003 — Duplicating a grouped effect leaves `group_id` pointing at the source's group — LOW — ✅ FIXED (`2e3dc4f3`)

A pasted/duplicated effect keeps its `group_id`, which still references a group on the
**source's** chain. `Layer::clone_with_new_ids` remaps this for layer effects
([layer.rs:889-893](../crates/manifold-core/src/layer.rs#L889-L893)), but the effect-paste
path (BUG-001) and the clip-effect path (BUG-002) don't. Fixing BUG-001/002 by regenerating
ids must also decide the `group_id` remap, or you trade an id collision for a dangling group
ref.

**Status:** rolled into the BUG-001/BUG-002 fix; tracked separately so it isn't forgotten.

---

### BUG-004 — Effect paste carries Ableton/automation bindings; generator paste drops them — LOW — ✅ FIXED (`2e3dc4f3`)

Effect paste clones the whole `PresetInstance`, so `ableton_mappings`, `drivers`, `envelopes`,
and `audio_mods` all ride along — a pasted effect ends up mapped to the **same Ableton
control** as the source, and one knob drives both. Generator paste does the opposite: its
`GeneratorSnapshot` carries `drivers` + `envelopes` but **not** `ableton_mappings` or
`audio_mods` ([clipboard.rs:54-95](../crates/manifold-editing/src/clipboard.rs#L54-L95)).

This is an inconsistency, not strictly a crash. Per the effect/generator binding-parity
principle the two paste paths should agree. Decide the intended behavior (most DAWs do **not**
carry hardware/MIDI mappings onto a paste) and make both paths match.

**Status:** design decision to settle alongside BUG-001.

---

### BUG-005 — Macro targets can't disambiguate two same-type effects on one layer — LOW — ✅ FIXED (`9f43f183`)

`MacroMappingTarget` addresses an effect param by `(layer_id | master, effect_type, param_id)`
([macro_bank.rs:64-82](../crates/manifold-core/src/macro_bank.rs#L64-L82)) — **not** by
`EffectId`. So duplicating an effect (trivially producing two `Blur`s on one layer) makes any
macro mapping to that `(layer, Blur, param)` ambiguous; resolution can't tell the copies
apart. Distinct from the id-collision class (macros are immune to that because they don't key
on `EffectId`), but the same root trigger — duplication — exposes it.

**Fix shape:** address macro targets by stable `EffectId` like single-card edits already do
(`docs/CARD_TARGET_UNIFICATION.md`). Larger than a one-liner; parked here so it's recorded.

---

## Checked and safe (coverage proof)

Audited during the 2026-06-23 duplication sweep; these duplicate correctly. Recorded so the
audit boundary is auditable.

- **Graph-node copy/paste** — `PasteNodesCommand` ([graph.rs:1985-2110](../crates/manifold-editing/src/commands/graph.rs#L1985-L2110)) mints fresh runtime ids + fresh `NodeId`s, remaps internal wires, starts pasted nodes un-exposed. Has regression tests (`paste_node_clones_with_fresh_identity_and_undo_removes`, `paste_remaps_internal_wires_to_the_new_node_ids`). **This is the reference implementation** for the BUG-001/002 fixes.
- **Generator paste** — `PasteGeneratorCommand` overwrites the target layer's single generator in place, addressed by `LayerId`. No id minted, no collision.
- **Markers** — created fresh via `TimelineMarker::new` (fresh `MarkerId`, [marker.rs:20-27](../crates/manifold-core/src/marker.rs#L20-L27)); no copy/paste/duplicate-marker path exists (markers are timeline-level, untouched by layer/clip dup).
- **New-clip-from-scratch paths** (MIDI/percussion/live-trigger/browser-drop) — construct fresh clips, not duplicates of existing ones.

## Blast radius — id-keyed resolvers that a duplicate `EffectId` breaks

All first-match-wins; all used by both editing and undo/redo:
- `Project::find_effect_by_id_mut` — [project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947) (master + layer + clip effects)
- `Project::find_effect_by_id` — [project.rs:711](../crates/manifold-core/src/project.rs#L711)
- `GraphTarget::Effect` / `set_base_param_by_id` paths that wrap them
- Renderer chain rebuild `harvest_state_from` — [preset_runtime.rs:1667](../crates/manifold-renderer/src/preset_runtime.rs#L1667) (per-card GPU state migration)

**Not** in the blast radius: macros (`(layer, type, param)`-addressed — see BUG-005),
markers, generators (`LayerId`-addressed).

## The pattern behind all of this

Duplicating an id-bearing entity must mint a fresh identity for itself **and** every nested
id-bearing child, or id-keyed first-match resolution collides. The graph-node path enforces
this with a test and never regressed; the paths without a test (effect paste, clip clone)
did. The durable fix for the class is a test per duplication path, not a doc note.

Related agent-memory notes: `feedback_hidden_field_dependencies` (the mirror — removing a
field silently breaks identity), and `project_invariant_audit` (its "Positional identity"
category is marked *already fixed*; BUG-001/002 are live counterexamples — correct that claim
when one is fixed).

