# Audio Setup Dock & Trigger Unification — P3c landing

**Phase:** P3c — fire meter (D6) on every fire-mode drawer + BUG-082 fix · **Level reached: L2** (PNG + tree-dump, orchestrator-read: the meter track + 0.5 line render beside Amount on both fire-mode drawer kinds and are absent on continuous drawers). Live needle-crossing a real signal is **Peter L4**.
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `8c9d7050` · **Worker content:** `12fbc37d` (code) + `ba20f4db` (BUG_BACKLOG) · landing merge on `main`

## What shipped

Tuning a fire trigger is now visual and identical for both kinds of fire config: a live meter of the shaped `condition()` signal with the fixed 0.5 fire line drawn on it, beside the Amount row of every fire-mode drawer — clip triggers AND param gate cards. This is BUG-082's fix: level features (Amplitude/Centroid/…) were never near-dead in the engine, they were an invisible Schmitt trigger; the meter makes them tunable. No feature is restricted (U2 stands).

**The two-thread path (content → UI, no new shared state, no per-frame alloc):**
- **Capture (content thread):** `modulation.rs`'s `is_trigger_gate` arm and `live_trigger.rs`'s per-config loop each push `(fire_meter_key(identity), conditioned)` into a `&mut FireMeterCapture` threaded through the existing eval walks. `PlaybackEngine` owns one `fire_meters: FireMeterCapture`, reset at the top of each tick.
- **`FireMeterCapture`** (`manifold-core/src/audio_trigger.rs:280`): a fully `Copy` struct — `keys: [u64; 128]` + `levels: [f32; 128]` + `count` (`MAX_FIRE_METERS = 128`). Every push/reset/read is a stack write; **no heap, no per-frame allocation** (verified, not argued).
- **Keying:** param gate cards by `(EffectId, ParamId)` (matches the existing `automation_latched_params` identity shape); clip triggers by `(LayerId, index)`. Both hashed FNV-1a → `u64` (`manifold-foundation/src/hash.rs::fire_meter_key`, deterministic) so the identity crosses the crate boundary `manifold-ui` cannot.
- **`ContentState::fire_meters`** carries it content→UI (read in `content_thread.rs` right after `engine.tick()`).
- **UI:** `ui_root.rs::update_fire_meters` builds a `|key| content_state.fire_meters.get(key)` closure, called **unconditionally every UI tick** from `app_render.rs` — deliberately NOT routed through `configure()`/`AudioCardState` (that only refreshes on structural/selection change, so a live value there would go stale between syncs). The meter fill/style update in place via `set_bounds`/`set_style` (the deleted `update_trigger_levels` pattern), never a per-frame drawer rebuild. Amount is always the first Slider row → `DrawerIds.meters[0]`.

## Gate (orchestrator-verified, independently re-run)

- `-p manifold-ui --lib`: **356**. `-p manifold-foundation --lib`: **24**. `-p manifold-playback --lib`: **228**. `-p manifold-core --lib`: **664**. 0 failed. `-p manifold-app` build clean; clippy clean.
- **Negative gates (orchestrator re-ran):** no Feature-row restriction in `param_slider_shared.rs` (**0**); no new `Arc<Mutex>`/`Arc<RwLock>` in the touched files (**0**).
- **Visual (L2, orchestrator-read):** `frame00-strobe-gate-card-meter.png` (Strobe `is_trigger_gate` "Clip Trigger" drawer) and `frame11-clip-trigger-meter.png` (GLOW AUDIO TRIGGERS clip-trigger drawer) both show the meter track + centered 0.5 tick beneath Amount; **Bloom's continuous drawer (Action=Cont) has none** — correctly applied only to fire-mode configs, no over-application. Worker's tree-dump cross-check confirmed exact node coords/colors on both kinds and absence on the continuous drawer.
- **`FireMeterCapture` is `Copy`** confirmed by reading the derive; keys/levels are fixed arrays.

## Content-thread gate — honest gap (→ VD-025)

The live `MANIFOLD_RENDER_TRACE=1` full-frame trace was **not run**: it needs the app with an audio device + GPU, which neither the worker's nor the orchestrator's sandbox has. The worker substituted an honest isolated measurement of exactly the new work (worst case, 128 configs, 2000 iters): **0.19 µs/tick release, 13.14 µs/tick debug — ~1500× under the 20 ms frame budget**. Given the new work is a bounded set of stack writes on a `Copy` struct, the frame-budget risk is negligible, but the live full-frame trace is owed and logged as **VD-025**. The run that closes it: launch `manifold` with a project carrying both a `LayerClipTrigger` and an armed `is_trigger_gate` param mod, audio playing, `MANIFOLD_RENDER_TRACE=1`.

## Click-script for Peter (≤2 min — the L4 feel-pass)

1. Play a kick-heavy track; arm a clip trigger (or a param gate card) on Low → Amplitude.
2. Open its drawer. **Expect:** a meter strip with a vertical line at the middle, beneath Amount.
3. Crank Amount. **Expect:** the shaped level rises past the line and the one-shot fires — you can *see* the crossing you're tuning. (This is the whole point of BUG-082's fix.)
4. Switch the Feature to a level feature (Centroid, Presence). **Expect:** it's now tunable by watching the meter, not dead.

## Doc correction folded in (P3c evidence)

CLAUDE.md's crate-dependency paragraph said "`…/ui/… depend on `core`". Verified false: `manifold-ui/Cargo.toml` depends only on `manifold-foundation` (the zero-dep vocabulary crate; `manifold-core` re-exports foundation's types at their historical path) — this is where `fire_meter_key` had to live, and the stale line cost the worker a course-correction. Fixed the one line precisely. **Flagged, not fixed:** the same paragraph omits `manifold-foundation`, `manifold-audio`, and `manifold-spectral` entirely — a fuller crate-table refresh is owed as separate doc hygiene, not done mid-wave.

## Shortcuts / gaps

- Meter verified via a standalone script against the `--dump` JSON (exact node coords/colors) rather than a formal `Assert` step in the ui-flow DSL → called **L2** honestly, not L3. A meter-node `Assert` is a reasonable follow-up.
- The live crossing + the full-frame render-trace are the two owed items (Peter L4 + VD-025).

## Owed to P4

The final phase: readability/hygiene (D7/D8) — band-label chips, per-source meters, selection highlight, gain-stepper/send-fader double-click resets (BUG-070 remainder), missing-layer copy — plus the wave-closing full workspace sweep + `cargo clippy --workspace`.
