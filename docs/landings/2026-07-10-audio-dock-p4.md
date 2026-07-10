# Audio Setup Dock & Trigger Unification — P4 landing (WAVE COMPLETE)

**Phase:** P4 — readability (D7) + hygiene (D8), the wave-closer · **Level reached: L3** (hygiene flow drives the real reset path; two-width readability PNGs orchestrator-read via crop/probe)
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `95a9939e` · **Worker content:** `133d34b6` (pre-task-0) + `f1a35270` (D7/D8) + `a649f62a` (housekeeping) · landing merge on `main`

## What shipped

**D7 — readability (Peter's "still easily readable overlaid on the spectrogram" condition):**
- **Band labels → chips on their divider lines** (`audio_setup_panel.rs:1119-1157`, `:1645`). The real collision (confirmed by render, not the doc's guess) was the onset tick-lane legend packed into a 1.4%-tall strip; "Low/Mid/High" now anchor to their actual divider-line y (`scope_line_y`, the same helper the drag hit-test uses) as small backed chips over the waterfall's left edge. Orchestrator-read: High(blue)/Mid(green)/Low(red) sit at their frequencies, legible and non-overlapping.
- **Selected-source-row highlight** (`:709-724`): identity-coloured background + border, painted under the row controls. Probe-verified (`#020202` bare bg vs `#070d1a`/`#091124` inside the selected row — real compositing).
- **Per-source-row inline meter:** already shipped a month prior (commit `44febffb`, "Phase 5.3") — the worker verified via `git log -S` + render, built nothing, and corrected the stale "deferred to a later phase" comment (`:210-216`) so the next reader isn't misled.

**D8 — hygiene:**
- **Gain-stepper + gain-drag-zone reset** (`:1984-2016`, BUG-070 remainder → **FIXED**): implemented as **right-click**, matching the house convention — the sibling drawer sliders all reset on right-click (`PanelAction::slider_reset`), and the panel's only double-click precedent resets a layout *width*, a different affordance. Replays the existing `AudioSendGainDrag{Begin,Changed,Commit}` at 0.0 dB, no new command.
- **"(missing layer)" repair copy** (`:1268-1287`, `MISSING_LAYER_LABEL` const shared with `state_sync.rs:1277` so the two can't drift): "Input layer was deleted — choose a replacement".

## Gate (orchestrator-verified — the WAVE CLOSE)

- **`cargo test --workspace`: GREEN** (orchestrator re-ran). One failure surfaced and was fixed: `manifold-core --test docs_index_sync` — the origin merge pulled two new design docs (`UI_LAYOUT_INVARIANT_LINTS_PROPOSAL`, `UI_WIDGET_UNIFICATION_DESIGN`) plus this wave's landing reports without regenerating the index; fixed by `python3 scripts/gen_docs_index.py` (145 docs), drift guard now passes. Not a P4 code defect.
- **`cargo clippy --workspace -- -D warnings`: clean** (orchestrator re-ran; only the pre-existing manifold-media native ObjC deprecations).
- **Negative gate:** `spectrogram.*fire|fire.*spectrogram` in `audio_setup_panel.rs` → **0** (no fire feedback on the spectrogram; it lives in the P3c drawer meter).
- **L3 flow** `scripts/ui-flows/audio-setup-hygiene.json`: exit 0, 11/11 steps, count-based asserts proving the reset fires.
- **Readability PNG** (`audiosends` scene, dock opened): orchestrator-read at 460px — which is BOTH default and minimum (`PANEL_W_MIN == DEFAULT_AUDIO_SETUP_WIDTH == 460`, no narrower state), so the hardest case is covered by construction. Band-label chips legible + non-overlapping; selection highlight visible.
- Design-token ratchet raised `COLOR_BASELINE` 190→198 (5 new literals from the chip/highlight work + 3 inherited pre-existing drift), the test's own established precedent.

## Two items the worker correctly surfaced instead of guessing

- **Reset gesture = right-click, not the doc's "double-click".** Verified against the house convention (sibling sliders use right-click; double-click is a width affordance). Defensible and consistent — but it contradicts D8's literal word, so it's a **Peter feel-pass confirmation** (does right-click-to-reset feel right on the gain controls, or did you mean double-click?).
- **Cap+N/St/Mo tooltips — NOT built, escalated → Deferred.** No retained-mode tooltip primitive exists in `manifold-ui` (only an immediate-mode one bespoke to `graph_canvas`); building one crosses the design's own "no new widget kinds" audit and needs per-frame cursor plumbing. Note: `UI_WIDGET_UNIFICATION_DESIGN` (landed on main this same day) is the likely home for a real tooltip primitive — revive there, not here.

## Click-script for Peter (≤2 min)

1. Open Audio Setup, select a source. **Expect:** its row is highlighted; the spectrogram's Low/Mid/High labels sit as chips on their divider lines, not stacked in a corner.
2. Right-click a source's gain stepper (and the gain drag-zone). **Expect:** it resets to 0.0 dB. *(Confirm this gesture feels right — it's right-click, matching the app's sliders; say if you wanted double-click.)*
3. Delete a layer that a source routes to, reopen the panel. **Expect:** "Input layer was deleted — choose a replacement" instead of a bare "(missing layer)".

## Shortcuts / owed

- Cap+N/St/Mo tooltips deferred (infra gap, above).
- Chip legibility + highlight strength + meter readability at real window sizes with live audio = **Peter L4 feel-pass** (orchestrator verified via probe/crop, not human eyes).
- Worker noted a `git stash`/`stash pop` self-inflicted scare mid-session (CLAUDE.md flags this tool as hazardous here) — caught immediately, every edit verified restored, no loss.

## WAVE COMPLETE

P1 (dock) · P2 (model + migration + evaluator) · P3a (matrix deletion) · P3b (authoring UI) · P3c (fire meter/BUG-082) · P4 (readability/hygiene) — all shipped on `main` 2026-07-10. Owns closed: BUG-047, BUG-070, BUG-082. Open debt: VD-024 (section unit tests), VD-025 (live render-trace). Peter feel-pass list: narrow-width dock, dock-width persistence, live fire-meter crossing, trigger-row wording, reset-gesture confirmation, tooltip decision.
