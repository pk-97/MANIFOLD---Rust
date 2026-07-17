# SCENE_OBJECT_AND_PANEL_V2 P1+P2 — landed 2026-07-17 @ `5c5dacfe`

**Branch:** wave/scene-object-v2 · **Level reached:** L1 (P1, plumbing, no
observable surface) / L2 (P2, before/after PNG pair + invisible-caster pair
read by the orchestrator) — target levels per the phase briefs, both met.
**Doc status line (quoted verbatim):** "IN PROGRESS — P1+P2 SHIPPED @
`5c5dacfe` (2026-07-17, landing report
`docs/landings/2026-07-17-scene-object-v2-p1-p2.md`); P3–P5 not implemented.
BUG-210 opened during the landing (AddSceneObjectCommand still emits
pre-migration wires — P3's own committed deliverable, not a regression)."

## Gate results (verbatim)

Focused, worker-run and orchestrator-independently-reverified after each
merge with origin/main (origin/main moved three times across this landing;
each move re-gated):

```
cargo nextest run -p manifold-renderer -p manifold-core --lib
     Summary [   4.938s] 1758 tests run: 1758 passed, 4 skipped

cargo test -p manifold-renderer --features gpu-proofs --lib
test result: ok. 1783 passed; 0 failed; 23 ignored; 0 measured; 0 filtered out; finished in 33.67s
(glb_conformance_sweep, a separate integration binary in the same feature,
is RED for reasons unrelated to this wave — pre-existing, tracked as
BUG-185/BUG-190; diff independently confirmed to touch no animation/skinning/
specular/volume code.)

cargo run -p manifold-renderer --bin check-presets
53 presets: 53 ok, 0 failed

cargo run -p manifold-renderer --bin graph-tool -- validate SceneStarter.json --kind generator
OK
cargo run -p manifold-renderer --bin graph-tool -- fusion SceneStarter.json
(node.scene_object present, correctly boundary:non_gpu, unfused as expected)

rg -n '"mesh_\{i\}"|format!\("mesh_' render_scene.rs        -> 0 hits
rg -l '"mesh_0"' crates/manifold-renderer/assets            -> 0 hits
rg -n "String" scene_object.rs                                -> 0 owned strings
rg -n "Arc<Mutex" scene_object*                                -> 0 hits
```

Full workspace sweep, run in the main checkout at landing time (twice — once
before the RemoveSceneObjectCommand collision was found, once after the fix):

```
cargo nextest run --workspace
     Summary [  50.949s] 3586 tests run: 3586 passed (1 leaky), 12 skipped

cargo clippy --workspace -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.24s   (clean)

cargo deny check bans
bans ok
```

## Deviations from brief

1. **D2's `SceneObject` struct grew from 9 to 21 resource fields mid-P2.**
   The design's §1 audit undercounted `render_scene.rs`'s real per-object
   port surface — it named 9 legacy port families
   (mesh/transform/material/4 PBR maps/instances) but the actual
   `ObjectPortNames` struct carried 21: 12 more came from
   `GLTF_MATERIAL_EXTENSIONS_DESIGN.md` E3–E6 (sheen/iridescence/clearcoat/
   specular×2/transmission/volume-thickness maps), landed before this
   design's audit but missed by it. Found by the P2 worker during read-back
   (correctly escalated rather than guessing), confirmed independently by
   the orchestrator (`ObjectPortNames` at render_scene.rs:606), ruled by
   Peter: extend `SceneObject` with all 12 missing fields, same pattern as
   the existing 5 map fields. No other part of the design changed.
2. **`RemoveSceneObjectCommand` (BUG-193's fix) collided with P2's port
   deletion.** A different, concurrent session shipped this command the
   same day (commit `ceacb025`, before this design's audit ran) against the
   *old* per-object port model. P2's migration deleted the ports it
   depended on, silently turning it into a no-op — caught by the mandatory
   post-merge full workspace sweep, not by any phase's own gate (neither
   worker touched or knew about this command). One Fable advisor consulted
   or a written verdict (root cause confirmed, full red-test list
   enumerated, recommended: fix now using the new-model lookup rather than
   defer to P3 or hold the whole landing — the fix IS P3's eventual Remove
   shape landing early, not throwaway). Orchestrator independently
   re-verified the diagnosis and applied the fix directly
   (`RemoveSceneObjectCommand` retargeted from `mesh_{k}` lookup to
   `object_{k}`), plus a design-token baseline bump
   (`COLOR_BASELINE` 209→210 for P1's `PORT_OBJECT_COLOR` const, also
   caught by the same sweep). `AddSceneObjectCommand` has the identical
   mirror-image gap (still emits legacy-shaped wires) but was NOT
   patched — it's P3's own already-committed deliverable, not a
   landing-caused regression; opened as BUG-210 instead.

## Shortcuts confessed (rolled up from phase reports)

- P1: none.
- P2: `Object` was added to `freeze/region.rs`'s Camera-exemption predicate
  structurally, but no install-time routing/recompute machinery was built
  for it (unreachable until a second Object consumer exists — the design's
  own escalation trigger). The GPU end-to-end test uses hand-built
  test-only `EffectNode` structs rather than real registered primitives
  (mirrors an existing precedent, `render_scene` doesn't exist as an
  Object-consuming primitive to test against at this phase). Two mechanical
  completions flagged though not treated as decisions: `PORT_OBJECT_COLOR`
  hue choice, and a `Cow::Borrowed("visible")` port-shadowed param default.
  D5's handle-inheritance rule was clarified during implementation (donor
  must be a literal `type_id == "group"` node, not any handle-bearing
  producer) and handle-deduplication was added (`unique_name`, reused from
  `group_edit.rs`) — both forced by real collisions the GPU gate surfaced,
  confessed and tested, not preference.
- Post-landing fix: none beyond the deviation above.

## Verification debt

None opened. The one gap named in the design itself — ungrouped hand-built
scene objects (loose `scene_object` nodes whose mesh/transform/material
producers aren't wrapped in a group) aren't fully cleaned up by
`RemoveSceneObjectCommand`'s one-hop delete, same limitation the
pre-migration command already had — is noted in that command's doc comment
as P3's to harden if a real ungrouped scene needs it; not a new gap this
landing introduced.

## Click-script for Peter (≤2 minutes)

1. Open any project using SceneStarter or a bundled reference preset that
   ships a scene (e.g. Garden, Lathe) — expect: it loads and renders
   identically to before (the migration is invisible at this stage; P5 is
   what surfaces it in the panel).
2. In the graph editor, find a `node.scene_object` node feeding
   `render_scene`'s `object_0` port — expect: one node per scene object,
   carrying the mesh/transform/material/map wires that used to fan out
   directly to render_scene.
3. Toggle that node's `visible` param off — expect: the object and its cast
   shadow both disappear from the render; toggle back on — both return.
4. Nothing else changes yet — there is no panel UI for any of this until
   P5 lands; this batch is invisible from the perform surface by design.
