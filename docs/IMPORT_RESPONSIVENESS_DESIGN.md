# Import Responsiveness — big model drops stop freezing (and crashing) the app

**Status:** IN PROGRESS — P1 done (BUG-219, unreproduced on this env, RSS evidence recorded), P2 SHIPPED 2026-07-17 (`lane/import-responsiveness`, this session) — D2's duplicate-device deletion landed, negative gate green; P3 (background worker + progress toasts) remains · 2026-07-17 · Fable
**Prerequisites:** none (BUG-219's diagnosis on `lane/glb-triage` lands independently; this design consumes it)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter, 2026-07-17, live-testing GLB imports: *"We also need a loading bar or something when importing the glb files initially as it takes time for the scenes to load in with large complex files"* and, on `ABeautifulGame.glb` (43 MB): *"I think we're also missing the safety check on large scenes. … Just crashed trying to load this in."* One design, because it's one seam: everything between the file drop and the layer appearing runs synchronously on the UI thread today, and one of those steps also allocates a second GPU universe. On stage: a mid-set model drop must never beachball the rig, and a big file must degrade to "slow, with feedback," never to a crash.

Binding constraints: no hot path (import is an event, not per-frame — but it currently BLOCKS the per-frame thread, which is the bug); thread residency (the worker introduced here is a one-shot background thread feeding the UI thread through a drained channel — a new thread, so it is explicitly this design's approval to add ONE, shaped like the existing decode workers); no persistence changes; performance surface only in the "don't freeze during a set" sense.

## 1. Audit — what exists (verified 2026-07-17, against `2bf3f21f`)

| Piece | Where | State |
|---|---|---|
| Interactive import path | `Application::import_model_file` (app_lifecycle.rs:584-720) | ALL blocking on the UI thread: optional Blender subprocess convert (:615-641), `assemble_import_graph` full CPU parse (:649), `validate_def` (:667), then command dispatch |
| The second GPU universe | app_lifecycle.rs:666 — `GpuDevice::new()` per import | `validate_def` (validate.rs:233) uses the device solely for a trial `PresetRuntime::from_def_with_device` build (validate.rs:275) — a full runtime constructed and thrown away, on a device that exists only for this call |
| App-owned device | app.rs:2104 (`native_device` inside the `gpu` context) | The app already owns a UI-side `Arc<GpuDevice>` — the validation device is pure duplication |
| Crash evidence | BUG-219 (docs/BUG_BACKLOG.md) | Headless import of the same file does NOT crash (`render-import` converged, correct render) — the crash is app-path-specific; actual backtrace NOT yet captured. BUG-180 (open) is the adjacent uncapped-decode-memory bug |
| Background→UI precedent | content_state receiver drained per frame (app_render.rs); `gltf_mesh_source` re-parses on its own background threads at render time | The channel-drain pattern exists; no import-shaped background worker exists yet |
| Progress surface | `ToastPanel` (toast.rs: `show`, `show_with_accent`, `tick`) | Static-text toasts, re-showable — sufficient for stage-level progress; no percent-bar widget exists anywhere |
| Import report channel | `ImportReport::report_lines` → the BUG-063 "opened with repairs" toast mechanism | The existing end-of-import reporting path; progress stages extend the same user surface |

Out of scope, owned elsewhere: BUG-180 (decode memory caps), BUG-213/214 (extension reporting), the conformance xfails.

## 2. Decisions

**D1 — Reproduce before fixing; the crash fix is whatever the backtrace convicts.** Lane E proved the import PATH is innocent headlessly; the app-path divergences are the validation build and the doubled command execution. Neither is convicted yet. P1 reproduces the crash through the real app surface (ui-flow driving Import Model with `ABeautifulGame.glb`) with `RUST_BACKTRACE=full` captured, and P2 fixes what the backtrace names. Derivation is not observation; a fix shipped against an unobserved crash is a guess wearing a fix's clothing.

**D2 — Zero new GPU devices on the import path, regardless of P1's findings.** The per-import `GpuDevice::new()` at app_lifecycle.rs:666 is deleted in favor of the app's existing device handle (app.rs:2104's `Arc<GpuDevice>`, plumbed to `import_model_file` — it's a method on `Application`, the handle is already reachable). The trial-build validation itself stays (GRAPH_TOOLING D6: never a silent partial import) — it just stops paying for a private Metal device. This is correct even if P1 convicts something else: a throwaway device per drop is waste with no defender.
*Rejected: skipping GPU validation at import and letting the content pipeline surface build failures* — that re-opens the "malformed def surfaces later as wrong pixels, far from the cause" hole D6 closed.

**D3 — Import moves to a one-shot background worker; the UI thread drains an event channel.** `import_model_file` becomes: spawn one `std::thread` (named, one per in-flight import; concurrent drops queue — a second drop while one runs is enqueued, not parallel) that runs convert → parse → validate and sends `ImportProgress` events over a crossbeam channel the app drains once per frame exactly where it drains content state. The final event carries the ready `EffectGraphDef` + report; the UI thread then runs the existing command dispatch (unchanged — commands stay UI-thread-dispatched, so undo/redo and the local-echo pattern are untouched).

```rust
// manifold-app (module next to app_lifecycle.rs)
pub(crate) enum ImportProgress {
    Stage { path: PathBuf, stage: ImportStage },          // Converting | Parsing | Validating
    Done  { path: PathBuf, graph: EffectGraphDef, report: ImportReport,
            drop_beat: f32, layer_under_cursor: Option<usize> },
    Failed { path: PathBuf, message: String },            // one toast, never silent
}
```
Consequences, stated honestly: the Blender subprocess and the parse become cancel-less background work — closing the app mid-import abandons the thread (acceptable: it holds no locks and writes nothing until Done is drained on the UI thread). The def crosses a thread boundary by value; it's already `Send` plain data.
*Rejected: doing the work on the content thread* — import is editor-side assembly; the content thread's frame budget is the show's, and BUG-035's lesson (measure, don't argue) applies squarely. *Rejected: a thread pool / async runtime* — one drop at a time is the real workload; a pool is infrastructure without a customer.

**D4 — Progress is stage-level toasts, not a percent bar.** Each `Stage` event re-shows the toast ("Importing chessboard.glb — parsing…", accent color). `Done` shows the existing report toast; `Failed` shows the error. The gltf crate exposes no meaningful percent mid-parse, so a percent bar would be theater; stage text is honest and reuses `ToastPanel` verbatim. If Peter wants a real bar later, that's a widget design with a genuine data source (per-texture decode counts), deferred.
*Rejected: a modal progress dialog* — the whole point is the rig stays playable during an import.

**D5 — No file-size rejection guard.** Peter said "safety check," but a size threshold that refuses a legitimate 43 MB show asset is the wrong safety. The safety this design ships is: the UI thread never blocks (D3), no duplicate GPU universe (D2), the crash's actual cause fixed (P2), and failures always land as a visible toast (D3's `Failed`). Memory caps for decoded images belong to BUG-180 and are not duplicated here.

## 3. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| No `GpuDevice::new()` on any import path | negative gate: `rg -n 'GpuDevice::new' crates/manifold-app/src/app_lifecycle.rs` → 0 hits (P2) |
| UI thread never runs convert/parse/validate after P3 | `import_model_file`'s body after P3 is spawn+enqueue only; negative gate: `rg -n 'assemble_import_graph|convert_via_blender' crates/manifold-app/src/app_lifecycle.rs` hits only inside the worker fn |
| Import failure is never silent | `Failed` arm's toast has a unit test; flow script asserts the toast text on a deliberately-corrupt fixture |
| Commands still dispatch on the UI thread only | code shape: `Done` handling lives in the per-frame drain; no `ContentCommand` send from the worker (rg gate on the worker module) |

## 4. Phasing

- **P1 — Reproduce (one session).** Build the ui-flow (or extend the harness if Import Model isn't drivable — state the gap precisely if so) that drops `tests/fixtures/gltf/khronos/ABeautifulGame.glb` through `import_model_file` in the real app shell, `RUST_BACKTRACE=full`, capture the crash. Deliverable: the backtrace + one-paragraph conviction in BUG-219's entry. If it does NOT reproduce on current main, say so — D2/D3/D4 proceed anyway (they're justified independently), and BUG-219 gets the "unreproduced on <SHA>" note with Peter's build asked for. Gate: the artifact exists and BUG-219 is updated. Demo: none — L1 (evidence artifact).
- **P2 — Kill the duplicate device + fix the convicted cause (one session). SHIPPED 2026-07-17.** D2's device reuse; whatever P1 convicted, fixed at its level. Gate: P1's repro flow now completes without crashing (held-out input = the same file); negative gates from §3 rows 1; focused nextest `-p manifold-app -p manifold-renderer`; full GPU suite only if validate.rs's device usage changed shape. **Built:** `Application::validation_gpu_device()` (app.rs) replaces the per-import `GpuDevice::new()` — reuses `self.gpu`'s existing device (the normal case, `resumed()` already ran) or, when `self.gpu` is still `None` (P1 proved this reachable — a drop before `resumed()`), lazily creates and caches ONE fallback device on `Application` for the process's lifetime, never per-import. `app_lifecycle.rs`'s `import_model_file` now calls `self.validation_gpu_device()`; the literal `GpuDevice::new()` no longer appears anywhere in that file (gate green). validate.rs / `validate_def` untouched — the trial-build behavior itself is unchanged, per D2's own text: only the private device disappears. **P1 re-run, RSS recorded (BUG-219):** measured RSS growth across 3 sequential imports is statistically indistinguishable from P1's pre-fix baseline in both the `gpu: None` and `gpu: Some(..)` (pre-populated, the realistic shape) cases — see BUG-219's entry for the full before/after table and why: D2 removes the redundant Metal *device object* (command queue, shader/pipeline caches), which is small next to the trial build's own texture/buffer allocation for a 15-material asset; D6 (never a silent partial import) keeps that trial-build resource cost, deliberately, so it isn't what D2 was ever going to shrink. The crash's proximate cause remains the untested UI-thread-stall/watchdog hypothesis P1 named — P3 is what actually removes that.
- **P3 — Background worker + progress toasts (one session).** D3 + D4 wholesale. Gate: L3 flow — drop a large fixture, assert the stage toast appears while the import is in flight (the harness can assert between frames), assert the layer lands and plays; the corrupt-fixture Failed toast asserted; round-trip gate n/a (no persistence change); `MANIFOLD_RENDER_TRACE=1` statement that the content thread gained zero work. Performer gesture: drop a 43 MB model mid-playback and keep scrubbing the timeline — the flow asserts playback frames keep advancing during the import (transport time strictly increases across snapshots).

Forbidden moves (all phases): a size-threshold reject (D5's rejected alternative); a progress dialog; moving command dispatch off the UI thread; an async runtime dependency; touching BUG-180's decode paths "while we're here."

## 5. Decided — do not reopen
1. Crash fix waits for the backtrace; D2/D3/D4 don't (D1).
2. Import validation reuses the app device — no new `GpuDevice` anywhere in import (D2).
3. One background thread, channel drained per frame, commands stay UI-thread (D3).
4. Stage toasts, no percent bar, no modal (D4).
5. No file-size rejection (D5).

## 6. Deferred
- **Percent progress bar** — revive when a per-texture/per-mesh decode counter exists as a real data source (likely alongside BUG-180's decode accounting).
- **Import cancellation** — revive if Peter ever wants to abort a wrong drop mid-import; needs the worker to check a flag between stages, trivial then.
- **Parallel imports** — revive if drag-dropping a folder of models becomes a workflow; the queue becomes a small scheduler then.
