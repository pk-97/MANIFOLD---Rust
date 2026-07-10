# Audio Setup Dock & Trigger Unification — P2 landing

**Phase:** P2 — `LayerClipTrigger` model + load migration + evaluator + analysis-gating arm · **Level reached: L1** (model/migration/evaluator phase; no user-visible surface — P3 lands the UI in the same wave)
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `52c44be1` · **Worker content:** `e4aa01bf` · merged `origin/main` `c4ae2d4c` in before landing (clean, no conflicts) · landing merge on `main`

## What shipped

The last parallel trigger system is gone from the model. A clip trigger is now the same object as a param trigger — `LayerClipTrigger` embeds `AudioModSource` + `AudioModShape` and fires through the identical `TransientEdge`-at-0.5 chassis. It's owned by the layer, migrated off the old send-owned matrix at load, and the evaluator reads it from layers only.

- **Data model (`manifold-core`)** — `LayerClipTrigger { enabled, source, shape, one_shot_beats }` (`audio_trigger.rs:207`); `Layer.clip_triggers: Vec<LayerClipTrigger>` (`layer.rs:167`, `#[serde(default, skip_serializing_if=Vec::is_empty)]`), beside the MIDI clip-launch block. Runtime edge/follower state is NOT on the struct — it lives in the evaluator keyed `(LayerId, usize)`.
- **Legacy field + migration** — `AudioSend.triggers` is now `#[serde(default, skip_serializing)]` (deserialize-only, never written back). `Project::migrate_legacy_clip_triggers` (`project.rs:268`), called from `on_after_deserialize` (`project.rs:190`) — the project-level post-deserialize seam where both sends and layers are resolvable. Per legacy route: resolve `target_layer`, else auto-route by send-label name match; push a `LayerClipTrigger` (Transients feature, route band, sensitivity→Amount U5-verbatim, `one_shot_beats`+`enabled` preserved); drain the send. Unresolvable → counted `eprintln` per route + a `log::warn` summary (never silent).
- **Evaluator (`manifold-playback`)** — `LiveTriggerState::evaluate` (`live_trigger.rs:76`) walks layers with non-empty `clip_triggers`; `FireRequest.target_layer: LayerId` (the `send_label` auto-route is deleted — the target IS the owning layer). Per-analysis-block, no new allocation class.
- **Analysis-gating arm (§3.4)** — `Project::has_active_clip_triggers` + `analysis_consumed_sends` layer walk (`project.rs:333`/`:1297`); re-pointed readers `audio_mod_runtime.rs:266`/`:564`, `engine.rs:1699`. A project whose only audio consumer is a clip trigger now starts capture.
- **EditingService commands** — `AddLayerClipTriggerCommand` / `RemoveLayerClipTriggerCommand` / `SetLayerClipTriggerCommand` (`commands/layer.rs`), `LayerId`-addressed, whole-value-replace (mirrors `SetAudioModTriggerModeCommand`).

## Two worker judgment calls — both verified correct by the orchestrator

- **`condition()` not `apply()` (deviation from D3 prose).** The worker fired on `shape.condition()` (pre-range-map), not the doc's literal `shape.apply()`/`out_norm`. **Orchestrator verified against `modulation.rs:527`:** the real param-trigger path edge-detects on `conditioned`, exactly as the worker did — the doc's `apply()`/`out_norm` wording is imprecise and would have reintroduced the documented range-trim firing bug ("range_min ≥ 0.5 fired once and never re-armed"). This IS the byte-identical-to-param-triggers property D3 asserts; the worker followed intent over the imprecise literal and flagged it. D3 amended in this landing with an AS-BUILT correction.
- **Round-trip gate split across crates.** `manifold-io` cannot depend on `manifold-playback` (siblings under `core`). Rather than cross that boundary unbriefed, the worker split the proof: io proves the JSON round-trip (migration + `skip_serializing` drain + byte-identical config after save→reload); playback proves a `LayerClipTrigger` with those values fires. Together they cover the design's "load→fire→save→reload→fire" claim without an undocumented crate dependency. Correct escalation-avoidance; accepted.

## Gate (orchestrator-verified, post-merge on the branch)

- `-p manifold-core --lib`: **347 passed**. `-p manifold-editing --lib`: **102** (3 new command tests). `-p manifold-playback --lib`: **228** (7 rewritten `live_trigger` tests). `-p manifold-io` lib+integration: **50 + 7 + 5 + 16 + 3 = 81 passed**, 0 failed.
- **Round-trip migration tests (the load-bearing gate):** `legacy_route_migrates_and_the_round_trip_survives_save_and_reload`, `legacy_route_auto_routes_by_send_label_when_no_explicit_target`, `legacy_route_with_no_resolvable_target_is_dropped_but_still_drains` — all pass.
- **Real-fixture round-trip:** `liveschool_v6_leds_clip_trigger_migration_round_trips_cleanly` (the canonical Liveschool project) — pass.
- `cargo clippy -p manifold-core -p manifold-playback -p manifold-io -p manifold-editing -p manifold-app -- -D warnings`: clean (only pre-existing manifold-media C deprecations). `manifold-app` builds against the concurrently-merged harness-seam refactor.
- **Negative gates (orchestrator re-ran):** `send_label` in `live_trigger.rs` → **0**; every surviving `TriggerRoute` site is the legacy struct/field/accessors, the migration, the crate re-export, or the P3-doomed matrix UI + its fixtures — **no new authorable use**.

## Demo

**none — L1.** P2 has no user-visible surface by design; P3 (layer-side authoring UI + fire meter) lands the surface in the same wave. Nothing to look at yet — the migration and firing are proven by the named tests above.

## Shortcuts / honest gaps

- Sensitivity→Amount migration is a direct copy (`shape.sensitivity = route.sensitivity`), U5-verbatim; exact-feel fidelity explicitly not owed (feature is weeks old, Peter's projects only).
- No workspace sweep (per phase scope — that's P4, the wave's last code phase).
- The §3.4 count I briefed (42) was miscounted with `\b`; the command I pasted (no `\b`) yields 47 — the 5-count gap is `.triggers_with_route` matrix calls (P3-doomed), not a missed gating arm. My briefing inconsistency; the worker reconciled it correctly and proceeded. No action.
- No new bug found; `BUG_BACKLOG.md` untouched.

## Owed to P3

`SetLayerClipTriggerCommand` is whole-value-replace; P3's drawer builds against that (no per-field commands). P3's worker should sanity-check that shape against the actual drawer row layout once written — flagged, not a blocker.
