# PARAM_STEP_ACTIONS P1–P3 — landed 2026-07-08 @ `fd3f767e`

**Branch:** `wave/param-steps-p1` → `43a7f508` (+ hotfix `fix/param-step-load-test` → `2682f9f4`),
`wave/param-steps-p2` → `d9b46422`, `wave/param-steps-p3` → `fd3f767e`. Doc amendment (D6 dropped)
landed first at `ed5495a5` via `docs/param-steps-drop-every`.
**Level reached:** P1 L1 (unit-tested, no UI surface) · P2 L1 target-L4 (content-thread trace
reasoned not measured, VD-016) · P3 L3 (scripted UI flow + round-trip test), target L4 (Peter's
live feel-pass, VD-017). P4 not started — Peter's call this session, deferred before any code
was written.
**Doc status line (quoted verbatim):** "P1–P3 SHIPPED 2026-07-08 (`43a7f508`/`d9b46422`/
`fd3f767e`); P4 (Plasma re-author) DEFERRED — Peter's call this session, not started, no code
written. The full feature (Continuous/Step/Random on any param, audio- and clip-fired, drawer UI)
is live and usable on every preset without P4; P4 is cleanup on one preset's leftover graph
wiring."

## Gate results (verbatim)

Per-phase gates (each independently re-run by the orchestrator in the phase's own worktree,
never trusted solely from the worker's report):

**P1** — `cargo test -p manifold-core --lib`: 333 passed, 0 failed. `cargo test -p
manifold-playback --lib`: 180 passed, 0 failed. `cargo clippy --workspace -- -D warnings`: clean.
Negative gates (`thread_rng|SmallRng|rand::`, `struct ParamStepMod|step_mods`,
`every\s*:\s*u32|fire_count.*every|%\s*every`): zero real hits (one doc-comment coincidence on
the third, read and confirmed benign). **Found here:** `cargo check --workspace --all-targets`
failed — `crates/manifold-io/tests/load_project.rs:726` missing 3 new `ParameterAudioMod` fields
(E0063). Root cause: the standard `clippy --workspace -- -D warnings` gate never compiles
integration-test binaries. Hotfixed same session (`bf5d2c6d` → merged `2682f9f4`), all-targets
check re-verified clean after.

**P2** — `cargo test -p manifold-playback` (full, incl. new `param_step_clip_edge.rs`): 186+9+8+
19+7+5 = 234 passed, 0 failed. `cargo clippy --workspace -- -D warnings`: clean.
`cargo check --workspace --all-targets`: clean. Negative gate (`trigger_pulse|TriggerPulse` in
`generator_renderer.rs` diffed against pre-phase tip): zero-diff confirmed (`git diff 43a7f508 --
crates/manifold-renderer/src/generator_renderer.rs` → empty). Content-thread `MANIFOLD_RENDER_TRACE`
gate: NOT run — no headless path drives `content_pipeline.rs` (verified personally by reading
`ui_snapshot/render.rs`, confirmed it only spins a bare `GpuDevice`, never `ContentThread`).
Reasoned bound recorded in VD-016.

**P3** — `cargo test -p manifold-editing --lib`: 99 passed. `cargo test -p manifold-ui --lib`: 646
passed. `cargo test -p manifold-app`: 172 passed, 2 ignored. `cargo test -p manifold-playback --
--test-threads=1`: 234 passed (the default parallel runner flakes ~1-in-3 on an unrelated
pre-existing `audio_mixdown` test — BUG-074, not caused by this phase). `cargo clippy --workspace
-- -D warnings`: clean. `cargo check --workspace --all-targets`: clean. Round-trip test
`step_mod_resumes_from_committed_base_after_real_save_and_reload`: passed (independently re-run).
Acceptance flow `scripts/ui-flows/param-step-action.json` via `cargo run -p manifold-app --features
ui-snapshot -- ui-snap paramsteps --script ...`: 11/11 steps `ok`, independently re-run by the
orchestrator (not just the worker's report), including the badge assertion (`Count(1)` for
`{"S","Button","under_text":"Amount"}`). Three PNGs read personally: Plasma's `pattern` card with
Action=Step/Wrap=Wrap/Mode=Audio fully legible; Bloom's `amount` before (Action=Cont) and after
(Action=Step, badge swapped A→S) the real click.

**Final landing sweep** (run on `fd3f767e` before this report was written): `cargo check
--workspace --all-targets`: clean. `cargo clippy --workspace -- -D warnings`: clean (both, only
pre-existing unrelated `manifold-media` Obj-C deprecation warnings). `cargo test --workspace`:
every target green, 2,826 tests passed, 0 failed, across the full crate graph (includes doc-tests).

## Deviations from brief

- **P3's command shape**: one `SetAudioModActionCommand` over the whole `TriggerAction` field
  (not three parallel field commands) — `Step`'s `{amount, wrap}` already bundles both, so one
  command is the cleaner fit inside the committed family shape (D8 left this as the executor's
  call).
- **P3's badge**: the arm button's own glyph swaps A→S/R instead of a new header-badge column —
  avoids touching cross-cutting header-chip layout math for a badge that already had a home.
- **Gate strengthened mid-session**: every phase from P1 onward also gates on `cargo check
  --workspace --all-targets`, which is not what the doc originally specified — added after P1's
  miss (see BUG log below). This is a strengthening, not a scope change.
- **P4 not attempted**: Peter's explicit call this session, before any worker was briefed or any
  code written. Not a phase failure — a sequencing choice. Pre-flight audit findings preserved in
  the design doc's P4 section for whoever picks it up next.

## Shortcuts confessed (rolled up from phase reports)

- P1: extracted `DriverWaveform::Random`'s inline hash into shared `hash_u32`/`hash_to_float`
  functions (`effects.rs`) rather than duplicating the magic constants, with a pinning test
  proving the driver's own output is unchanged — a small, scoped refactor of one call site, not a
  scope widening.
- P2: the "live-slot/phantom clip launch fires" gate item was exercised via a session-grid slot
  launch instead of a MIDI/phantom live-clip trigger (driving a real phantom clip hits a pre-existing
  raw-pointer split-borrow pattern outside P2's scope); session slots merge into the identical
  code path with zero live-vs-session special-casing in the diff, so this is architecturally the
  same proof.
- P3: found (not fixed) a headless UI-flow harness gap — the `--script` driver never ticks the
  drawer-reveal animation, so a mod armed *live* by a script renders at zero height. Worked around
  by pre-arming the fixture (matching existing convention) rather than fixing the harness; logged
  as BUG-073.
- None of the above are stubs or hardcodes left in shipped code — all are documented, scoped
  design calls made explicit in their phase reports.

## Verification debt

- **VD-016 opened** — P2's content-thread trace gate reasoned, not measured (same wall as
  VD-014). Burn-down: `MANIFOLD_RENDER_TRACE=1` live against the 53-layer Liveschool fixture with
  a Clip-mode step mod armed.
- **VD-017 opened** — P3's performer gesture (Kick → BasicShapes variant, Step/Wrap, 4-bar loop)
  untried live. Burn-down: click-script below.
- **BUG-072 opened** (not fixed, out of scope) — pre-existing `--all-targets` clippy debt in
  `audio_mixdown.rs`, unrelated to this design.
- **BUG-073 opened** (not fixed, out of scope) — headless ui-flow `--script` driver never ticks
  drawer-reveal animation tweens.
- **BUG-074 opened** (not fixed, out of scope) — an unrelated `manifold-playback` test flakes
  ~1-in-3 under the default parallel test runner; green under `--test-threads=1`.

## Click-script for Peter (≤2 minutes)

1. Open any project, select a layer running BasicShapes (or any generator with a whole-numbers
   card, e.g. Plasma's `pattern`). Click the card's audio-mod button (the small "A"). — expect:
   the standard audio-mod drawer opens with Source/Feature/Band/Amount/Attack/Release, plus a new
   **Action** row (Cont/Step/Rand).
2. Click **Step**. — expect: an **Amount** stepper and a **Wrap** row (Wrap/Bounce/Clamp) appear;
   the collapsed card badge (the arm button glyph) changes from "A" to "S".
3. Point the drawer's Source at your Kick send, set Mode to **Audio**, hit Play with a beat
   coming through. — expect: the card's value visibly steps forward on each kick hit, wrapping
   back to 0 once it passes the top of its range.
4. Switch Mode to **Clip** and launch a different clip on the same layer. — expect: the value
   steps once on the clip launch, not on audio hits.
5. Save the project, reload it, hit Play again. — expect: stepping resumes correctly from wherever
   you last set the slider by hand (not from a stale mid-cycle position) — this is the reload
   contract (BUG-036's rule) proven in the round-trip test above; worth Peter's own eyes once.
