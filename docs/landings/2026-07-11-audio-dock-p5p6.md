# AUDIO_SETUP_DOCK Wave 2, batch 1 (P5+P6) — landed 2026-07-11 @ `2c12fc99`

**Branch:** `wave/audio-dock-wave2` · **Level reached:** L1 (unit tests) + L2 (synthetic-level PNG) for P5's plumbing; L1 for P6. **Target for P5 was explicitly NOT L4** — the brief forbids claiming the live crossing from this session (§7.1's own overclaim lesson); L4 is VD-025, owed to Peter.
**Doc status line (quoted verbatim):** `**Status:** **IN PROGRESS — WAVE 2 (P5+P6 SHIPPED 2026-07-11, P7–P8 not built)**`

## What shipped

**P5 — fire meter resurrection (BUG-109; re-closes BUG-082).** The D6 fire meter never
displayed a clip-trigger level in any transport state: while playing, the per-branch
`FireMeterCapture` reset ran *after* step 3b's clip-trigger push, silently wiping it every
tick; while stopped, clip triggers were never evaluated at all — dead exactly when a
performer tunes a trigger at soundcheck. Fixed:
- **One reset, one place:** `FireMeterCapture::default()` now writes once at the top of
  `PlaybackEngine::tick`, before either branch's evaluators run (`engine.rs`). The two
  mid-branch resets are deleted.
- **Stopped-tick conditioning walk:** `LiveTriggerState::evaluate` was split into a shared
  `walk` helper behind `evaluate` (fires) and a new `evaluate_meter_only` (pushes the
  identical `condition()` signal into the meter, never advances `TransientEdge`, never
  fires). `tick_non_playing` calls it whenever the project has an active clip trigger, so
  the meter breathes with the music while stopped without any risk of firing a clip.
- **UI-side peak-hold:** `MeterIds` (`drawer.rs`) grew `Cell<f32>` `held_level`/
  `hold_remaining` state — instant attack to a new peak, `PEAK_HOLD_SECONDS = 0.25` hold,
  `PEAK_DECAY_PER_SEC = 5.0` fall. `MeterIds::update` takes a `dt: f32`, threaded down
  through `update_fire_meters` on `ParamCardPanel` / `AudioTriggerSection` /
  `InspectorCompositePanel` / `UIRoot` from `app_render.rs`'s existing `frame_timer` value —
  no new timer, no content-thread smoothing (the capture stays the raw conditioned value the
  edge reads; hold/decay is display-only).
- Fixed the stale `tick_audio_triggers` doc comment (claimed "called after modulation";
  actually runs at step 3b, before `sync_clips_to_time`, before modulation) — the root cause
  of the original bug. Updated `CORE_ENGINE_MAP.md` §3/§6 to match.

**P6 — drawer cleanup: Sensitivity, Delta removal, Invert.**
- Display label "Amount" → "Sensitivity" on the shared audio-mod drawer (internal name
  `AudioShapeParam::Sensitivity` unchanged — display strings only).
- Delta (rate-of-change) toggle removed from the drawer for both `AudioModDrawerTarget`
  callers. `RowClick::AudioToggleRate` deleted along with its three now-unreachable
  click-routing sites (`param_card.rs` effect + generator cards, `audio_trigger_section.rs`);
  flat button-index math renumbered everywhere the toggle row shrank from two buttons to
  one. The runtime `AudioModShape::rate_of_change` field and its `condition()` arm stay
  compiled dormant for a possible future re-wire — only the button and its click routing
  are gone.
- `Project::clear_legacy_rate_on_flags` — a new load migration (`on_after_deserialize`,
  after `migrate_legacy_clip_triggers`): a saved `rate_on: true` on either carrier
  (`ParameterAudioMod.shape` via every `PresetInstance.audio_mods`, walked with the existing
  `for_each_preset_instance_mut`; `LayerClipTrigger.shape` on every layer) gets cleared,
  counted, and `eprintln!`'d.

## Gate results (verbatim)

Focused, in the worktree, after each phase:
```
manifold-playback --lib:        356 passed (P5), 359 passed (P6, +rate_on tests)
manifold-playback --test engine_tick: 10 passed (incl. the 2 new BUG-109 regressions)
manifold-ui --lib:               50 passed
manifold-core --lib:            229 passed, 2 ignored → 356/359 depending on phase (see above)
manifold-io --lib:              667 passed
manifold-app (3 test bins):     162 + 10 + 1 passed
manifold-io --test load_project: 16 passed (incl. load_liveschool_live_show_v6,
                                  liveschool_v6_leds_clip_trigger_migration_round_trips_cleanly)
```
Batched landing sweep (worktree, after merging origin/main twice — two rounds of
`docs/BUG_BACKLOG.md` conflicts against concurrently-landing sessions, both resolved by
keeping every side's entries; see the two merge commits on `wave/audio-dock-wave2`):
```
cargo build --workspace:                    clean
cargo clippy --workspace -- -D warnings:    clean
cargo nextest run --workspace:              3023 tests run: 3023 passed, 8 skipped
cargo deny check bans:                      bans ok
```
Re-verified in the main checkout immediately before push: `cargo build --workspace` clean.

## Deviations from brief

- The brief's own P5 phase heading said "closes VD-025" — corrected in the design doc:
  P5 does not close VD-025 by design (its own "Explicitly not claimable" clause forbids
  it); only the live rig look does.
- P5's "headless PNG with a synthetic level" gate item didn't have existing harness support
  (`cargo xtask ui-snap` has no fire-meter injection). Added four lines to the `inspector`
  scene in `ui_snapshot/mod.rs`: after building the tree, call
  `ui.inspector.update_fire_meters(&mut ui.tree, &|_key| Some(0.9), 1.0/60.0)` — the exact
  public API the live app calls, fed a synthetic level instead of a real
  `FireMeterCapture`. This proves `MeterIds::update`'s rendering math (fill width,
  bright-over-0.5 accent), not the content-thread capture wiring — that half is the engine
  regression tests' job. Crop evidence: `target/ui-snapshots/inspector/inspector.crop.png`
  (Strobe card's Sensitivity meter, filled ~90%, bright green).

## Shortcuts confessed

- P5's decay-rate constant (`PEAK_DECAY_PER_SEC = 5.0`, full-scale fall in ~200ms) is a
  design choice, not a number the brief specified — chosen for a fast-but-visible fall so
  consecutive kicks read as distinct pulses. Peter's feel-pass may want to retune it.
- No live-audio-device confirmation of P5's fix (explicitly forbidden by the brief to claim
  from this session). VD-025 stays open.
- P6's decision to keep `PanelAction::AudioModSetRateOfChange` / `AudioTriggerSetRateOfChange`
  and its downstream EditingService dispatch alive (rather than deleting the whole command
  family) was a judgment call reading "runtime stays compiled for a future re-wire" as
  covering the command surface, not just the `AudioModShape` field. Verified no dead-code
  warning resulted (both variants stay reachable via `PanelAction`'s public API).

## Verification debt

- **VD-025 carried** (not closed by this landing): the live rig confirmation of the D6 fire
  meter — transport stopped, track through the tap, watch the meter move; transport playing,
  watch a real onset cross the 0.5 line. Closes when Peter confirms on the rig.
- No new VD opened.

## Click-script for Peter (≤2 minutes)

1. Load any project with a clip trigger or an armed fire-mode param mod (e.g. Strobe's
   Clip Trigger). Stop the transport, play the track through your audio tap.
   **Expect:** the drawer's Sensitivity meter breathes with the music even though nothing
   is playing on the timeline — this was dead before (BUG-109).
2. Hit Play. **Expect:** the meter keeps moving, and when it crosses the fixed line the
   clip/param fires — same as before, now provably live.
3. Open any fire-mode drawer. **Expect:** the row under Band reads "Invert" (one button,
   full width) — no "Delta" button anymore. The Sensitivity slider is labeled
   "Sensitivity", not "Amount".
4. Load an old project that had Delta armed on some config (if one exists on your rig).
   **Expect:** it loads fine, the config keeps its Sensitivity/Attack/Release tuning, Delta
   is just gone — nothing looks broken, nothing silently still "drives on rate of change"
   with no way to see or turn it off.
