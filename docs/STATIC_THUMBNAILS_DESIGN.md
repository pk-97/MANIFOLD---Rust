# Static Browser Thumbnails — a picker that opens instantly and tells the truth

**Status:** APPROVED design, not built · 2026-09-11 · k3 (lead)
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs)–section 6 (seam briefs) before starting any phase.

The effect/generator browser is a wall of live GPU renders that pop in over seconds, render black on empty layers, composite raw alpha over the UI, and overlap a "missing from library" badge with the preset name. The fix is not a faster live preview — it is removing live preview from the picker. Every cell is a static PNG rendered once against a proper test card, at declared defaults, composited over black, cached on disk. The browser opens instantly, costs zero GPU while open, and any preset whose defaults look broken announces itself in the grid.

Peter's directives, verbatim:
- "maybe we are being too ambitious here and should just go for a stock static card for now that is easy for the user to see and understand"
- "all effects and generators should default to 100% amount param though"
- On the stencil default showing broken in the thumbnail: the default is the bug — fix the default, and the grid becomes a standing audit of every preset.

Companion docs: [PRESET_BROWSER_AUDITION_DESIGN.md](PRESET_BROWSER_AUDITION_DESIGN.md) (the live-preview design this supersedes), `docs/archive/PRESET_LIBRARY_DESIGN.md` (P6/D7 built the static path this design finishes).

## 1. Audit — what exists (verified 2026-09-11)

| Piece | Where | State |
|---|---|---|
| Headless preset renderer | `crates/manifold-renderer/src/preset_thumbnail.rs` | Works. Effects render over a test input at defaults; generators self-render |
| Test input | `preset_thumbnail.rs:258` `build_gradient_input` | Pure math gradient R=x,G=y,B=(x+y)/2 — no edges, no detail, no hue spread. This is why old thumbnails looked bad |
| Generator capture state | `preset_thumbnail.rs:159-219` | Already deterministic: 60 warm-up frames at dt=1/60, 120bpm, anim sweep 0→1, IO/warmup settle wait. Generators-at-sensible-state is solved |
| Factory thumbnail cache | `assets/preset-thumbnails/{effects,generators}/<id>.png` via `factory_thumbnail_path` (`preset_thumbnail.rs:129`) | Committed PNGs; resolution packaged-bundle else dev workspace |
| Regeneration bin | `crates/manifold-renderer/src/bin/generate_preset_thumbnails.rs` | One-shot dev bin; renders every factory preset, writes PNGs to commit |
| Coverage | `ls` counts 2026-09-11 | 26 effect + 49 generator presets; only 15 + 14 thumbnails committed. ~60% of the grid has no static image |
| Browser cell precedence | `crates/manifold-ui/src/panels/browser_popup.rs:783-802` | Live audition atlas wins, then static thumbnail, then flat. The black live cells hide whatever statics exist |
| Live audition pool | `crates/manifold-renderer/src/audition/mod.rs` (696 lines), wired through `content_pipeline.rs:812-843`, snapshot fields `content_thread.rs:1464-1467`, panel hooks `browser_popup.rs:283-457` | Effect cells tap the layer's real source or master composite (`audition/mod.rs:62-66`); empty layer → black fallback → "half the effects don't render". Frame-budget throttle → the pop-in "load time" |
| Missing-from-library entries | `crates/manifold-app/src/ui_root/dropdowns.rs:297-308` | Snapshot-origin embedded presets surface in the picker when their library file is gone, badged "missing from library" in the same 14px strip as the name (`browser_popup.rs:830-845`) — the overlap |
| Amount defaults | audit script output below | 8 presets have wet/dry-style defaults below 1.0 |

Amount/mix defaults below 1.0 (2026-09-11, every param with id `amount` or `mix`):
AutoGain 0.5 · BlobTracking 0.5 · Bloom 0.5 · FilmGrain 0.35 · SoftFocus 0.5 · StylizedFeedback 0.5 · Watercolor 0.5 · Breathe 0.2. (`intensity` params — ColorCompass 5.0, LED Strip Fire 0.8 — are strengths, not wet/dry; untouched.)

StylizedFeedback (`assets/effect-presets/StylizedFeedback.json:49-60`): `mode` defaults to 2.0 = "Stencil", which Peter observes renders broken. Both facts confirmed at the data level.

## 2. Decisions

**D1 — Static thumbnails are the only browser imagery; live audition is deleted, not mothballed.** The pool, the atlas IOSurface, the frame-budget signal, and the app/UI plumbing all go. House rules forbid a parallel kept-alive path, and the pool is the entire cost and half the bugs. Revival trigger for hover-live-single-cell is in Deferred.
Rejected: keep audition as hover-only now, because it preserves the frame-budget coupling and the atlas machinery for a nicety nobody has asked for twice.
Rejected: pre-warm pipelines + burst-render on open (discussed 2026-09-11), because it still renders 30+ chains against live input on every open and keeps the throttle policy; Peter called it too ambitious for what the picker needs.

**D2 — The test card is a designed synthetic frame, generated in code.** One function replacing `build_gradient_input`, same f16 CPU-upload shape, producing four regions: a smooth diagonal gradient (tonal/color effects), saturated hue bars (color grading), fine stripes plus a checker block (blur/sharpen/edge/glitch), and a circle on mid-gray (spatial effects, exposure-neutral reference). Deterministic, no asset file, no licensing. A photo still from show footage is a one-function swap later if Peter wants it (Deferred).
Rejected: keep the math gradient, because it is the documented reason the old thumbnails were illegible.

**D3 — Alpha composites source-over black, always.** The stage bottoms out on black; the thumbnail must match. No transparency reaches a cell; the readback outputs opaque PNGs.

**D4 — Capture state is the existing generator recipe, extended to everything.** Defaults as declared, 60-frame warm-up at dt=1/60 and 120bpm, anim sweep 0→1, IO settle, then capture. Effects get the same warm-up (stateful effects — feedback, trails — develop identically). Deterministic by construction: same preset JSON → byte-identical PNG.

**D5 — Every wet/dry-style param (id `amount` or `mix`) defaults to 1.0 in factory presets.** Peter's rule: "all effects and generators should default to 100% amount param". The eight offenders in the audit are set to 1.0. This also fixes the no-op-add class at the root, so no `thumbnail_params` override mechanism is built (Deferred).
Consequences, stated honestly: adding an effect mid-show now hits at full strength immediately. That is the intent — a fat-fingered add on master is now visible at 100%, and Peter accepted that trade.

**D6 — StylizedFeedback's default mode changes from Stencil to a working mode.** The Stencil mode itself is a separate bug (logged in beads at design time); the default stops showcasing it.

**D7 — Freshness is machine-checked, not remembered.** A CPU-only test hashes every factory preset JSON and asserts the committed thumbnail exists and its recorded hash matches. Editing a preset without re-running the bin fails the default test suite. User-library presets keep the existing save-time render; a stale or missing user thumbnail falls back to the flat labeled cell — no browse-time render (existing rule, `dropdowns.rs:259-260`).

**D8 — Missing-from-library snapshot entries leave the picker.** They are self-containment plumbing (the project's embedded copies), not user-manageable choices — right-click already gives them no menu. The layers that use them still show the preset on the layer card, so nothing becomes unfindable that was findable. This deletes the badge and its overlap outright.

## 3. Design body

### 3.1 The test card

`build_test_card_input(device, w, h, format) -> RenderTarget` in `preset_thumbnail.rs`, replacing `build_gradient_input` (delete the old one; its test callers migrate). Pixel layout, all computed per-pixel in the same f16 CPU-upload loop:

- Left third: diagonal RGB gradient (R=x, G=y, B=(x+y)/2 — the old gradient, demoted to a region).
- Middle third: six vertical hue bars (red, yellow, green, cyan, blue, magenta) at 80% saturation over a 50% gray floor.
- Right third, top half: horizontal 2px stripes alternating black/white; bottom half: 8px checker.
- Centered over the seam of all three: a white circle at 15% of frame height radius, 100% white — the hard edge every blur/edge effect needs.

One function, ~60 lines. The 16:9 cell crop shows all four regions.

### 3.2 Renderer changes

- `render_generator` warm-up recipe (60 frames, dt 1/60, 120bpm, sweep, IO settle) becomes the shared `capture_preset_frame` path; the effect path runs the same loop over the test-card input. Feedback/trail effects develop state across the warm-up exactly like generators.
- Readback composites over black before PNG encode: `out.rgb = src.rgb * src.a` onto opaque black, alpha forced 255. (Premultiplied-then-flatten; the current readbacks are straight-alpha — the flatten is new, ~10 lines in `headless_readback` or at the thumbnail call site, author's choice.)
- Determinism: fixed seed wherever the runtime takes one; no wall-clock reads in the capture path (the warm-up already synthesizes time).

### 3.3 Freshness sidecar

Each committed thumbnail gets `<id>.hash` beside it (the SHA-256 of the preset JSON bytes, written by the bin). Test `factory_thumbnails_fresh` in `manifold-renderer` (default suite, CPU-only): for every id in the factory registry, the PNG exists, the `.hash` exists, and the hash matches the JSON on disk. The bin is re-run by the phase that changes any preset; the test is the enforcement.

### 3.4 Browser simplification (P3)

Delete, don't demote: `AuditionPool` and `audition/mod.rs`, the atlas IOSurface + generation counter (`content_pipeline.rs:812-843`, `content_thread.rs:1464-1467`), the frame-budget signal (`content_pipeline.rs` `set_audition_frame_signal`), the panel's `audition_src`/`take_audition_*`/`last_render_list` hooks (`browser_popup.rs:278-305, 371-383, 397-457`), and the app pump that forwards them. Cell rendering collapses to: thumbnail if the file exists, else the flat labeled cell. The caption strip keeps name-only — the badge path goes away with D8.

`dropdowns.rs` stops emitting PickerItems for unresolvable Snapshot-origin presets (the `EmbeddedOrigin::Snapshot` arm at `dropdowns.rs:303-308` is deleted; resolvable snapshots were already skipped). `PickerItem.missing_from_library` and `CellMeta.missing_from_library` are deleted with it.

Consequences, stated honestly: the picker loses "what does this effect do to my actual footage right now". That was the audition design's real magic, and it only worked when the target layer had content playing. Statics trade it for instant open, universal coverage, and legibility. If Peter misses it, the revival is hover-live (Deferred), not the atlas.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| Every factory preset has a committed, fresh thumbnail | `factory_thumbnails_fresh` test (P2) — default suite, fails on missing PNG or stale hash |
| Wet/dry defaults are 1.0 | `factory_amount_defaults_full` test (P1) — walks factory JSONs, asserts every `amount`/`mix` id defaults to 1.0 |
| Thumbnails are deterministic | `thumbnail_render_deterministic` (gpu-proofs, P2) — renders Bloom + FluidSim twice, asserts byte-identical PNGs |
| No transparency reaches a cell | Same gpu-proofs test asserts output alpha == 255 on every pixel |
| Live audition is gone | P3 negative gates: `rg "AuditionPool\|audition_src\|take_audition_render_list" crates/` returns zero hits |

## 5. Phasing

### P1 — Defaults sweep

- **Entry state:** the audit's amount list re-derives clean — re-run the preset-JSON walk; if new offenders exist, list them and include them.
- **Read-back:** D5, D6; the preset JSON schema in `docs/GRAPH_TOOLING_DESIGN.md`.
- **Deliverables:** the eight listed presets' `amount`/`mix` defaults → 1.0; StylizedFeedback `mode` default → 0.0 (first working mode); `factory_amount_defaults_full` test; `graph-tool validate` clean on every touched file.
- **Gate:** `cargo nextest run -p manifold-renderer factory_amount_defaults` green; validate clean.
- **Demo:** none — L1.
- **Forbidden moves:** touching `intensity` params; "fixing" the Stencil mode itself in this phase (separate bead); changing any other default because it looks nicer.
- **Test scope:** `-p manifold-renderer` only.

### P2 — Test card, capture unification, regenerate everything

- **Entry state:** P1 landed (thumbnails must render the new defaults).
- **Read-back:** D2-D4; section 3.1 (test card)–3.3 (freshness sidecar); the warm-up loop at `preset_thumbnail.rs:159-219`.
- **Deliverables:** `build_test_card_input`; effect path on the shared warm-up capture; over-black flatten; `.hash` sidecars in the bin; `factory_thumbnails_fresh` test; `thumbnail_render_deterministic` (gpu-proofs); all 75 factory thumbnails regenerated and committed.
- **Gate:** `cargo nextest run -p manifold-renderer` green incl. freshness; `scripts/gpu_proofs_gate.py` green; 75 PNGs + 75 hashes on disk.
- **Acceptance demo (L2):** a contact-sheet montage of all 75 PNGs (the bin writes one) — Peter looks at it. This is the moment the card design is judged.
- **Forbidden moves:** tuning the card per-preset; shipping a card Peter hasn't seen; rendering at non-default params.
- **Test scope:** `-p manifold-renderer` + gpu-proofs (GPU path touched).

### P3 — Browser goes static-only; audition deleted

- **Entry state:** P2 landed; all cells have statics available.
- **Read-back:** D1, D7, D8; section 3.4 (browser simplification); the deletion inventory it names.
- **Deliverables:** deletions per section 3.4 (pool, surface, signals, panel hooks, app pump, snapshot-entry emission, badge path); cell render = thumbnail-or-flat; PRESET_BROWSER_AUDITION_DESIGN.md status → SUPERSEDED with a one-line pointer here.
- **Gate:** `cargo nextest run -p manifold-ui -p manifold-app` green; negative gate `rg "AuditionPool|audition_src|take_audition_render_list|missing_from_library" crates/` zero hits; `scripts/landing_gate.py`.
- **Acceptance demo (L3):** a `scripts/ui-flows/` flow that opens the effect browser and asserts cell count + that every visible cell is an image node; PNG artifact for Peter.
- **Content-thread gate:** the deletion removes per-frame work; run `MANIFOLD_RENDER_TRACE=1` once to confirm no regression from the browser open path.
- **Forbidden moves:** keeping the pool behind a flag; leaving `audition` as an empty module; "temporary" retention of the atlas surface.
- **Test scope:** `-p manifold-ui -p manifold-app -p manifold-renderer`.

## 6. Decided — do not reopen

1. Statics only in the picker; live audition is deleted (D1).
2. Synthetic four-region test card, generated in code (D2).
3. Alpha over black, always (D3).
4. One deterministic capture recipe for effects and generators (D4).
5. `amount`/`mix` factory defaults are 1.0 (D5). `intensity` params untouched.
6. StylizedFeedback defaults off the broken Stencil mode (D6).
7. Freshness is a hash test in the default suite (D7).
8. Snapshot/missing-library entries never appear in the picker (D8).

## 7. Deferred

- **Hover-live single cell** — the one hovered cell renders live (against the test card, not the layer). Trigger: Peter misses motion after statics ship. Never the whole-grid atlas.
- **Photo test card** — a real still from show footage replacing the synthetic card. One-function swap. Trigger: the synthetic card reads as lab equipment in the P2 contact sheet.
- **`thumbnail_params` per-preset render overrides** — declared in JSON, used only by the thumbnail. Trigger: a preset whose honest default is legitimately unphotogenic (D5's 100%-amount rule killed the main class).
- **Orphaned-preset section** — a dim "only in this project" browser section for library-less snapshots. Trigger: someone needs to find one from the picker (today they find it on the layer card).
