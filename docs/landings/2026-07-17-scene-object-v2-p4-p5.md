# SCENE_OBJECT_AND_PANEL_V2 P4+P5 — landed 2026-07-17 (wave close)

**Branch:** wave/scene-object-v2 · **Level reached:** L3 for both phases
(multiple UI-flow scripts driving the real input path, PNGs read by the
orchestrator, not just green tests).
**Doc status line (quoted verbatim):** see the design doc header, updated
in the same commit as this report — SHIPPED, all 5 phases landed.

## Gate results (verbatim)

P4 (focused, orchestrator-reverified):
```
cargo nextest run -p manifold-ui -p manifold-app --lib
     Summary [   2.835s] 1043 tests run: 1043 passed, 2 skipped

rg -n "Gesture::DoubleClick" scene_setup_panel.rs
2379:  crate::value_cell::ValueCellGesture::DoubleClick,   (exactly one hit,
                                                             inside intent_for)

cargo clippy -p manifold-ui -p manifold-app -- -D warnings   clean
```
Demo PNG read directly: the Intensity value cell shows "3.50" with a visible
text cursor and orange highlight — reads as active text entry, not a bare
rectangle. Camera rows show degree readouts (40.1°, 17.2°, 51.6°) — D10
confirmed live.

P5 (focused, orchestrator-reverified):
```
cargo nextest run -p manifold-ui -p manifold-app -p manifold-renderer --lib
     Summary [  16.259s] 2188 tests run: 2188 passed, 4 skipped

rg -n "SceneSelection" crates/manifold-io crates/manifold-core        -> 0 hits
rg -n "MutateProject|Arc<Mutex|Arc<RwLock" scene_setup_panel.rs        -> 0 hits
rg -n "Project\b" scene_vm.rs                                          -> 0 hits

cargo clippy -p manifold-ui -p manifold-app -p manifold-renderer -- -D warnings   clean
```
Demo PNGs read directly: the eye-toggle pair shows the icon genuinely
flipping (red/off → grey/on across two clicks); the held-out 38-item
(37 objects + 1 light) merged warehouse+skull scene's outliner renders with
a consistent, recognizable eye-icon column — reads as real UI chrome, not
bare text (the AUDIO_SENDS P2 failure mode this design's standard explicitly
guards against).

Full workspace sweep, run in the WARM MAIN CHECKOUT at landing time (after
merging origin/main twice more — it moved during this phase):
```
cargo nextest run --workspace
     Summary [  33.662s] 3619 tests run: 3619 passed, 12 skipped

cargo clippy --workspace -- -D warnings    clean
cargo deny check bans                       bans ok
```

## Deviations from brief

1. **Two ID collisions with concurrent lanes**, both caught and resolved
   before landing (same pattern as P1+P2's and P3's landings — a recurring,
   now well-understood hazard of this many-session repo, not a new failure
   mode): P5's own BUG-216 clashed with a different lane's BUG-216
   (unrelated feedback-loop bug); renumbered to BUG-218. Also resolved a
   merge conflict in `docs/BUG_BACKLOG.md` that additionally exposed a
   pre-existing formatting bug in the OTHER lane's own commit (a swallowed
   `###` heading) — fixed in passing, not otherwise touched.
2. **A stale `docs/node_catalog.json` after merging origin/main** (the
   concurrent SSAO/HDRI lane added primitives) — regenerated via
   `cargo run -p manifold-renderer --bin gen_node_catalog`, one commit.
3. **Two real bugs found by the post-merge full workspace sweep, both
   fixed in this landing rather than deferred** (per the house rule: "a
   red test is either fixed before landing or gets a BUG entry + explicit
   Peter ping — this is our own landing's job, not another session's"):
   - **D5's migration was never actually wired into the real project-load
     path.** `migrate_scene_object_wires` existed and was unit-tested in
     isolation, but `manifold-app/src/project_io.rs` — the D5-named call
     site — never called it, despite P2's landing report claiming it did.
     The Scene Setup panel reads a generator layer's raw stored graph
     directly (`Layer::generator_graph()`), never through the separate
     `instantiate_def` migration hook, so every existing saved project
     with legacy per-object wiring would have shown a broken panel after
     this wave shipped. Fixed: wired the call into `project_io.rs`,
     mirroring the existing `migrate_user_param_bindings_to_node_id` call
     right beside it.
   - **`SceneVm`'s transform/material/vertex-count tracing didn't
     understand the migrated-project topology.** Once (1) was fixed, a
     pre-existing regression test (predating this whole design,
     `scene_setup_round_trip.rs`) surfaced a second gap: the migration
     mints `scene_object` as a ROOT-level sibling of the mesh producer's
     group (D5's "same-scope re-point"), with `vertices`/`material`/
     `transform` wired straight from the group's own boundary port — a
     DIFFERENT topology than a fresh glTF import produces (scene_object
     nested inside the group). P5's own synthetic tests only covered the
     fresh-import shape. Confirmed against the real, shipped
     `SceneStarter.json` (not just the test fixture): every migrated
     object's transform, material, and vertex count silently failed to
     resolve in the panel — new `resolve_producer_through_group()`
     transparently crosses one group boundary, threading the crossed
     group's id into `ParamAddr::scope_path` so param writes still target
     the correct scope. New permanent regression test
     (`bundled_scene_starter_preset_resolves_transform_material_and_vertex_count`)
     loads the real shipped preset, not a synthetic fixture, specifically
     so this class of gap can't hide behind a fixture shaped only like the
     common case.
4. **Design-token baseline bumps** (P4: 209→210 for a new port-pin color;
   P5: 210→214 for outliner chrome colors) — same established pattern as
   every prior phase in this wave, dated comments added.

## Shortcuts confessed (rolled up)

P4: none on committed scope. Right-click `ResetToDefault` is built in the
contract module (`value_cell.rs`, tested) but not wired into either dock —
the phase brief's concrete deliverable list only required Scrub+EditValue
registration parity; wiring reset would need a `default` value on
`RowValue`/dock DTOs that doesn't exist yet, left as a gap rather than
fabricated. Environment `mode` has no editable enum row today (still a
static chip) — D9's dropdown wiring covers every *existing* enum row
generically; nothing to wire for a row that isn't editable yet.

P5: BUG-218 confessed and logged (not fixed, correctly out of blast
radius). The headless snapshot harness never renders live GPU content, so
"object vanishes from the render" is verified at the command/icon level,
not pixel level — an honest tooling limit, not a shortcut.

Post-landing fixes (this report's own work): none beyond what's described
in Deviations above — both fixes are complete, not partial.

## Verification debt

None newly opened. BUG-212 and BUG-218 are real product debt, logged,
tracked in `docs/BUG_BACKLOG.md` — not verification gaps, actual bugs with
their own fix shapes named.

## Click-script for Peter (≤2 minutes)

1. Open any project with a Scene Setup dock (SceneStarter or any bundled
   reference preset with a scene) — expect: the outliner lists Camera,
   World, every light, every object, each with a glyph + name; objects
   carry a clickable eye icon.
2. Click an object row — expect: its transform, material, and modifier
   controls appear in the Properties region below (this now works for
   BOTH freshly-imported objects AND every pre-existing/migrated project's
   objects — the second case was broken until this landing's fix).
3. Click that object's eye — expect: it disappears from the render;
   click again — it's back.
4. Double-click any numeric value cell (e.g. Sun Intensity) — expect: it
   opens for direct keyboard entry; type a number, press Enter, it commits
   with no clamping.
5. Shift-drag a value cell — expect: the applied delta is visibly finer
   (0.1×) than a plain drag.
6. Click "Add modifier" on any real imported/added object's Properties —
   expect: **currently a silent no-op** (BUG-218, known, tracked, not part
   of this wave's committed scope — flagging so it doesn't read as a fresh
   surprise).
