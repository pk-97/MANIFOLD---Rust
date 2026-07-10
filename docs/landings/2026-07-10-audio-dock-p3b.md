# Audio Setup Dock & Trigger Unification — P3b landing

**Phase:** P3b — inspector AUDIO TRIGGERS authoring section + clip-trigger drawer (D5) + command family + state_sync · **Level reached: L3** (add-trigger flow drives the real UI→PanelAction→EditingService→Project path, 14/14 steps ok; collapsed + expanded PNGs orchestrator-read)
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `3869d81f` · **Worker content:** `5c4fbcca` · landing merge on `main`

## What shipped

Clip triggers are now authored where Peter chose them to be: **one collapsible "AUDIO TRIGGERS" section pinned at the top of the selected layer's inspector, default-collapsed** (2026-07-10 AskUserQuestion). Each trigger expands to the same audio-mod drawer a param uses — plus a Length row, no Mode row. This closes P3a's interim authoring gap.

- **New `crates/manifold-ui/src/panels/audio_trigger_section.rs`** (471 lines) — the section: header + collapse chevron (default-collapsed, mirroring `macros_panel.rs`), a row per `layer.clip_triggers` entry (ON/OFF toggle, "band → feature" label, × remove), "+ Add Trigger", per-row expandable drawer. Built at the top of the layer column in `inspector.rs`.
- **D5 seam (one builder, parameterized — NOT forked):** `build_audio_mod_drawer` (`param_slider_shared.rs:1638`) takes a new `AudioModDrawerTarget { Param(GraphParamTarget) | ClipTrigger(LayerId, usize) }` in place of the bare graph-param target; the `shape_reset` closure matches on it to emit either the `AudioModShape*` or the new `AudioTrigger*` PanelAction family. The section is a third caller of the same builder (like `ParamCardPanel`), not a fork. Also fixed `audio_config_height` to account for the Length row (P3a shipped the row without updating the height helper, since no caller passed `Some` yet).
- **Command family:** 12 new `PanelAction::AudioTrigger*` variants (`panels/mod.rs`) → dispatch handlers in `ui_bridge/inspector.rs` (`clip_trigger_shape_dual_edit`, addresses `Layer.clip_triggers` by `LayerId`) → P2's `Add/Remove/SetLayerClipTriggerCommand` (whole-value-replace). All through EditingService.
- **state_sync:** builds `AudioTriggerSectionConfig` from `layer.clip_triggers` each sync (`ui_bridge/state_sync.rs`) — the inspector reads the view-model, never `Project`.
- **No fire meter** (verified by negative gate) — D6/BUG-082 is P3c.

## Gate (orchestrator-verified, independently re-run)

- `-p manifold-ui --lib`: **658 passed**. `-p manifold-editing --lib`: **107 passed**. 0 failed (re-run by orchestrator).
- `-p manifold-app` build clean; `clippy -p manifold-ui -p manifold-app -p manifold-editing --features manifold-app/ui-snapshot -- -D warnings` clean.
- **Negative gates (orchestrator re-ran):** `build_audio_trigger|clip_trigger_drawer` → **0** (no forked drawer); `meter|threshold_line|fire_level` (word-boundary) in `param_slider_shared.rs` → **0** (no meter this phase); `AudioModDrawerTarget` enum present → 1 (one parameterized builder).
- **L3 flow** `scripts/ui-flows/audio-clip-trigger-add.json`: orchestrator parsed `result.json` — **all 14 steps `status: ok`, zero non-ok**. Steps: assert "AUDIO TRIGGERS" label (collapsed) → click ▶ chevron → assert "▸ Low → Kick" row → click "+ Add Trigger" → expand row → `AudioTriggerSetSource(glow, 1, …, {Amplitude, Low})`. Real end-to-end path.

## Demo (L3) — PNGs orchestrator-read

- `run-audio-clip-trigger-add/00.png` — **default-collapsed**: "AUDIO TRIGGERS ▶" is a single header line at the top of the GLOW inspector, above Mirror/Bloom/Strobe, no rows. Matches Peter's spec exactly.
- `run-audio-clip-trigger-add/11.png` — **expanded**: the clip-trigger drawer shows Source/Feature/Band/Amount/Attack/Release/**Length** (1/4·1/2·1b·2b·4b·8b), **no Mode row**, **no meter**; byte-identical to the effect audio-mod drawers below it. Every row reads as clickable chrome (ON/OFF, chevron label, × remove) — affordance-legible.

## Click-script for Peter (≤2 min)

1. Select a layer in the inspector. **Expect:** an "AUDIO TRIGGERS ▶" header at the very top, collapsed.
2. Click the chevron. **Expect:** it expands; existing triggers (migrated from your projects) show as "band → feature" rows.
3. Click "+ Add Trigger", expand the new row. **Expect:** the same drawer your effect params use — Source/Feature/Band/Amount/Attack/Release — plus a **Length** row, and **no** Mode row.
4. Set a band / feature. **Expect:** it sticks (whole-value-replace through the undo stack).
   *(Tuning against the visible fire line is P3c — not here yet.)*

## Shortcuts / honest gaps

- **No dedicated unit tests on `AudioTriggerSection`** (macros_panel/layer_chrome carry collapse/click unit modules; this doesn't). Covered instead by compile + the 658-test suite (no regressions) + the L3 flow. Real coverage debt — logged to `VERIFICATION_DEBT.md`; a follow-up (P3c or P4) should add a test module mirroring `macros_panel.rs`. Not a blocker: the vertical path is proven by the L3 flow.
- Row label wording "band → feature kind" is the orchestrator-brief's own example, not verbatim-specified — **Peter feel-pass**.
- Per-row ON/OFF toggle placement inferred from `LayerClipTrigger::new`'s "disabled by default" doc — reasonable, feel-pass.
- The collapsed-row `▸` chevron glyph renders slightly heavy in the bitmap font (visible in 11.png row 1) — cosmetic, **P4 readability / feel-pass**.
- Section visibility gated on `!layer_chrome.is_collapsed()` (shows only when the layer's own section is expanded) — placement judgment, brief didn't specify the relationship.
- Harness quirk observed: the flow writes `NN.fail.tree.json` diagnostic dumps even for passing Assert/Pointer steps (`result.json` is authoritative — all ok). FINDING for the harness doc.

## Owed to P3c

Fire meter (D6) on every fire-mode drawer + BUG-082 (still OPEN). The Amount row is currently a plain slider; P3c adds the 0.5 threshold line + live shaped-signal meter.
