# Audio Sends UX — make the routing legible, the tuning live, and the analysis pay-per-use

**Status:** APPROVED design, not built · approved by Peter 2026-07-04 · Fable · D5 word confirmed: "Source" · **baseline-reviewed 2026-07-05, cleared** (zero unlabeled forks; anchors spot-reverified EXACT — PANEL_W_FRAC :39, analyzers :129, MAX_SENDS :56, GainBank :72; D5-confirmation propagated into §2/P4 which still read "pending". §10 levels: P2/P3/P4 already gate on headless PNGs = L2; P1's eprintln count = L2 log trace; P3/P5 in-app checks = L4 today, L3-able via UI_AUTOMATION once landed since the panel is standard widgets.)
**Prerequisites:** none (all phases run against shipped audio-modulation code)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter, opening the session: "The sends work but they're awkward to use and tricky to understand," and "I'm worried about the performance problems if we have heaps and heaps of audio stems all being analysed." The audit found the model and the sample path sound; every problem is presentation-layer plus one perf gap. **The governing insight: a send sits at the center of a star (channels and layers feed it; params, triggers, and the scope listen to it), but the UI only ever shows one arm of the star at a time, across four surfaces.** This doc makes the whole star visible in one place, moves the tuning half of Audio Setup out from behind a show-dimming modal, and makes analysis cost proportional to what is actually consumed.

Companion docs: `AUDIO_MODULATION_DESIGN.md` (the feature this UX fronts), `AUDIO_INFRASTRUCTURE.md` (capture/analysis stack — **§3.2/§8 are stale**, corrected by Phase 1), `LAYER_CONTROLS_DESIGN.md` §5.3 (layer→send routing), `OVERLAY_SYSTEM_DESIGN.md` (modality machinery Phase 3 touches).

---

## 1. Audit — what exists (verified 2026-07-04)

| Piece | Where | State |
|---|---|---|
| `AudioSetup` / `AudioSend` model, sends vec, gain, floor, crossovers | `crates/manifold-core/src/audio_setup.rs:327` | Shipped |
| Audio Setup panel: device row, send rows (swatch/label/source-chip/channels/gain-stepper/stereo/delete), scope, trigger matrix | `crates/manifold-ui/src/panels/audio_setup_panel.rs` | Shipped, **dimming modal**, 80%×80% viewport (`PANEL_W_FRAC` line 39) |
| `AudioSendRow` view-model incl. `driven_count`, `source_label`, `routings`, `triggers` | `audio_setup_panel.rs:54-87`, built by `ui_bridge/state_sync.rs` | Shipped — routings/consumers are **read-only strings**, no jump |
| Per-slider audio drawer ("A" button; send/feature/band/toggles/amount/attack/release — 7 uniform rows) | `crates/manifold-ui/src/panels/param_slider_shared.rs:94-100`, `param_card.rs:2272-2322` | Shipped |
| Layer→send routing (audio layer picks the send it feeds) | `crates/manifold-ui/src/panels/layer_header.rs:654-658`, `PanelAction::AudioSendClicked` | Shipped — the ONLY place layer feeds are edited |
| Per-send analyzers on the **content thread**, one `StreamingSendAnalyzer` per send | `crates/manifold-app/src/audio_mod_runtime.rs:127-129`, loop at `:258` | Shipped. Capture gate is **global** (any mod ∨ any enabled trigger ∨ scope open, `:231`); once up, **every send** is analyzed every tick |
| Measured cost (release, M-series, probe 2026-07-04) | — | 16 sends ≈ **0.96 ms mean / 1.27 ms worst per tick** (~60 µs/send) against the 16.6 ms tick; `MAX_SENDS = 16` hard cap (`analysis.rs:56`) |
| Live no-glitch param banks (gain, crossovers) | `analysis.rs:72` (`GainBank`), crossover drag via `MutateProjectLive` + commit command (`AUDIO_MODULATION_DESIGN.md` §10.0.1) | Shipped — the precedent for all Phase 3 drags |
| Drawer builder (declarative `DrawerSpec` → rows) | `crates/manifold-ui/src/panels/drawer.rs` | Shipped — sole builder for all four drawers |

Extend, don't redesign. No new crates, no new threads, no new shared state anywhere in this doc.

## 2. Decisions

- **D1 — One send-path view, inside the existing panel.** Selecting a send (swatch click, as today) shows its full star: inputs (capture channels + feeding layers) → send (meter/gain/floor) → consumers (named params + trigger routes). Rationale: the four-surface hunt is the root of "tricky to understand"; on stage it is the debugging surface for "why isn't this visual moving." Rejected: a separate routing-graph window, because the panel already owns send selection and the data layer (`state_sync`) already aggregates per-send state — a new surface would duplicate both.
- **D2 — Layer feeds become editable from the panel, both directions.** The inputs section lists feeding layers with a remove (×) per row and an "+ layer" dropdown of audio layers. Uses the **same EditingService command** the layer header fires. Rejected: keeping the panel read-only ("source chip is status"), because it forces a timeline expedition mid-calibration to fix routing.
- **D3 — Consumers list is navigational, not editable.** Each consumer row (param: "LayerName · EffectName · ParamName"; trigger: "Low → LayerName") is clickable and selects the owning layer; editing stays in the drawer / trigger matrix. Rejected: inline editing of mods from the panel — it would duplicate the drawer's seven controls in a second surface and they would drift.
- **D4 — Per-send analysis gating.** A send is analyzed only if it has ≥1 enabled audio mod, ≥1 enabled trigger route, or is the scope-tapped send. Consumed-send set recomputed **only on project change** (DataVersion dirty-check), never per tick. Rationale: today one bound param makes all 16 sends pay ~60 µs each per tick on the content thread. Rejected: moving analysis back to the worker thread, because the content-thread placement is deliberate — a send must sum capture mono with audio-layer taps *before* one analysis ("what you hear is what modulates," `analysis.rs:18-21`).
- **D5 — Rename "Send" → "Source" in user-facing strings only.** Ableton's "send" is an amount feeding a return that carries audio; ours carries nothing — things listen to it. "Source" matches the drawer's existing row label. **Rust types, serde fields, command names, and `AudioSendId` keep their names** — a serialized-format rename is load-migration risk for zero user value. Rejected: "Stem", because a send can be a mic, a system tap, or an app tap — not always a stem of a mix. **Confirmed by Peter 2026-07-04: "Source"** (the status line recorded it; this body note lagged until the 2026-07-05 baseline review — P4 is NOT blocked).
- **D6 — The panel stops dimming the show and moves aside.** Audio Setup becomes a non-dimming overlay anchored to the viewport's right edge (~38% width, full height); the preview stays visible. Trigger sensitivity, gain, floor, and crossovers are calibration against live output — a dimming 80% modal makes the panel's own purpose impossible. Device picking doesn't need dimming either, so the panel stays one surface. Supersedes `AUDIO_INFRASTRUCTURE.md` §7 "stays modal" — that call predates the trigger matrix growing in here. Rejected: splitting into a modal Devices panel + docked Tuning panel — two surfaces for one concept re-creates the fragmentation D1 kills.
- **D7 — Calibration values get drag.** Gain and trigger sensitivity value labels become horizontal drag zones (steppers stay for fine clicks). Live drag via `MutateProjectLive`, one command committed on release — the exact crossover-drag pattern. `GainBank` already absorbs gain edits with no capture restart.
- **D8 — Drawer presets are one-shot fills.** A "Preset" segmented row atop the audio drawer: **Pump · Snap · Follow · Wobble**. Clicking one writes feature/band/attack/release/amount in a single command; nothing stores "which preset is active" — the matrix stays the truth. Rejected: persistent preset modes, because divergence-after-edit would need tracking state for no benefit.

## 3. Design body

### 3.1 Send-path view (D1–D3)

The selected send's detail area (today: scope + trigger matrix) gains two sections, laid out with the existing imperative row builders in `audio_setup_panel.rs` — no new widget kinds:

- **Inputs** (above the scope): one row per source. Capture row = the existing channel dropdown + stereo toggle, unchanged. Layer rows = layer name + remove (×). Final row = "+ layer" dropdown listing audio layers not already feeding this send.
- **Consumers** (below the trigger matrix): one row per audio mod and per enabled trigger route, label + jump. Rows are plain buttons; click emits a `PanelAction` that selects the owning layer.

View-model extension (in `manifold-ui`, filled by `state_sync` like every other `AudioSendRow` field):

```rust
pub struct SendConsumerRow {
    pub label: String,            // "Kick Layer · Bloom · Intensity" / "Low → Strobe Layer"
    pub layer_index: Option<usize>, // jump target; None if unresolvable
}
// AudioSendRow gains: pub consumers: Vec<SendConsumerRow>,
//                     pub feeding_layers: Vec<(usize, String)>,  // (layer_index, name)
```

⚠ VERIFY-AT-IMPL: the existing layer-select `PanelAction` variant and the layer→send command name — `rg "AudioSendClicked|fn.*audio_send" crates/manifold-app/src/ui_bridge/ crates/manifold-editing/src/` and reuse what's found; do not mint a parallel command.

**The plausible-wrong architecture, forbidden by name:** you will want the panel to reach into `Project` to enumerate consumers at draw time — no. `state_sync` is the sole boundary; the panel renders what `AudioSendRow` carries, same as `driven_count` today.

### 3.2 Per-send gating (D4)

In `audio_mod_runtime.rs`: cache `consumed: AHashSet<AudioSendId>` + the `DataVersion` it was built from; rebuild only when the version moves (walk `PresetInstance.audio_mods` with `enabled`, `AudioSetup` trigger routes with `enabled`). In the per-send loop (`:258`), skip sends not in `consumed` and not scope-tapped: no mono push, no analyzer entry. Analyzer entries for newly-unconsumed sends drop via the existing project-change retain (`:219-223`). A freshly-bound send warms in one 4096-sample window (~85 ms) — acceptable fade-in, same as capture start today.

**Hot-path discipline:** the set rebuild allocates only on project change; the per-tick path adds one hash lookup per send. No per-tick allocation.

### 3.3 Non-dimming right-anchored panel (D6)

The panel's `Overlay` impl changes `Modality` (no dim, clicks outside pass through or close — match whatever the overlay system's non-modal overlays do; ⚠ VERIFY-AT-IMPL: `rg "Modality::" crates/manifold-ui/src/panels/overlay.rs` for the available variants and one existing non-modal consumer as precedent) and its `anchor()`/`size_policy()` return right-edge, 38% width × full height. `resize_to_viewport` keeps the scope absorbing spare vertical space. Escape and the header Audio button still toggle it.

### 3.4 Drags (D7) and presets (D8)

Drag: pointer-down on the gain / sensitivity value label arms a horizontal drag (1 px = 0.1 dB / 0.5%), `MutateProjectLive` while moving, single command on release — transcribe the crossover-drag arm/commit code path in `audio_setup_panel.rs::on_event`.

Presets (values committed here so the executor doesn't tune):

| Preset | Feature | Band | Attack | Release | Amount |
|---|---|---|---|---|---|
| Pump | Amplitude | Low | 5 ms | 250 ms | 1.0 |
| Snap | Transients | Full | 0 ms | 80 ms | 1.0 |
| Follow | Amplitude | Full | 60 ms | 400 ms | 0.8 |
| Wobble | Amplitude | Low | 20 ms | 120 ms | 1.0 |

One new `DrawerControl::Segmented` row in the audio `DrawerSpec`; click maps to one EditingService command writing all five fields. ⚠ VERIFY-AT-IMPL: exact field names on `AudioModShape` — read `manifold-core/src/audio_mod.rs` first.

## 4. Phasing

### Phase 1 — Per-send gating + infrastructure-doc truth pass
- **Entry state:** `rg -n "analyzers: AHashMap" crates/manifold-app/src/audio_mod_runtime.rs` hits (~:129); `rg -n "MAX_SENDS" crates/manifold-audio/src/analysis.rs` hits :56.
- **Read-back:** this doc §2 D4 + §3.2; `audio_mod_runtime.rs` module header + reconcile/tick fn; CLAUDE.md hot-path rules. Restate the gate condition and the no-per-tick-alloc constraint.
- **Deliverables:** consumed-set cache in `audio_mod_runtime.rs`; skip logic in the per-send loop; `AUDIO_INFRASTRUCTURE.md` §3.2/§8 rewritten to content-thread reality with the measured numbers (16 sends ≈ 0.96 ms mean / 1.27 ms worst per tick, release).
- **Gate:** *Positive:* with 16 sends and one bound param, an `eprintln!` count logs 1 analyzed send (2 with scope open); Peter runs it. `cargo clippy --workspace -- -D warnings`. *Negative:* `rg "collect|AHashSet::new" ` in the per-tick path of the diff returns hits only inside the version-gated rebuild.
- **Forbidden moves:** moving analysis off the content thread; gating capture (already handled) instead of analysis; a "temporary" always-analyze flag.
- **Test scope:** `cargo test -p manifold-app --lib` if the crate has lib tests, else the eprintln proof; no workspace sweep.

### Phase 2 — Send-path view
- **Entry state:** `rg -n "routings: Vec<String>" crates/manifold-ui/src/panels/audio_setup_panel.rs` hits (~:81); the §3.1 VERIFY-AT-IMPL sweep, results written into session notes.
- **Read-back:** §2 D1–D3, §3.1, and the state_sync builder for `AudioSendRow`. Restate the state_sync-only boundary rule.
- **Deliverables:** `SendConsumerRow`, `consumers` + `feeding_layers` on `AudioSendRow`; Inputs + Consumers sections in the panel; layer add/remove wired to the existing command; consumer click → layer select.
- **Gate:** *Positive:* headless PNG of the open panel with a 2-send fixture (per `reference_ui_headless_png_verification` memory), Read and eyeball: both sections render, labels resolve. Focused `cargo test -p manifold-ui --lib`. *Negative:* `rg "\.project\(\)|&Project" crates/manifold-ui/src/panels/audio_setup_panel.rs` → zero hits (panel never touches the model).
- **Forbidden moves:** a second mutation path bypassing the layer-header command; editing mods inline; widening into panel visual redesign.
- **Test scope:** focused only.

### Phase 3 — Non-dimming right-anchor + calibration drags
- **Entry state:** `rg -n "PANEL_W_FRAC" crates/manifold-ui/src/panels/audio_setup_panel.rs` hits :39; the §3.3 Modality sweep done and written down.
- **Read-back:** §2 D6–D7, §3.3–3.4 drag paragraph, the crossover-drag code path, `OVERLAY_SYSTEM_DESIGN.md` modality section.
- **Deliverables:** right-anchored non-dim overlay; gain + sensitivity drag (live + commit-on-release).
- **Gate:** *Positive:* headless PNG shows panel right-anchored, content behind undimmed; in-app: drag gain while audio plays — meter follows, no capture restart (no audible/visual glitch), one undo step per drag. Peter runs the in-app check. *Negative:* `rg "dim" ` on the panel's modality returns no dimming path.
- **Forbidden moves:** splitting into two panels; making it a dockable window; per-tick command spam during drag (live-mutate + single commit only).
- **Test scope:** focused `cargo test -p manifold-ui --lib`.

### Phase 4 — "Source" rename (strings only; D5 confirmed 2026-07-04 — not blocked)
- **Entry state:** D5 word confirmed ("Source", Peter 2026-07-04). `rg -in '"[^"]*send[^"]*"' crates/manifold-ui/src crates/manifold-app/src` re-derived at execution (the baked count WILL be stale).
- **Read-back:** D5. Restate: user-visible strings only; types/serde/commands untouched.
- **Deliverables:** every user-visible "Send"/"send" → "Source"/"source" (panel title rows, layer-header "No send", drawer row label, notices, tooltips).
- **Gate:** *Positive:* headless PNGs of panel + layer header + drawer show the new word. *Negative:* `rg 'add_label|add_button' crates/manifold-ui/src -A1 | rg -i '"send'` → zero hits; `git diff --stat` touches no `manifold-core`/`manifold-io` files.
- **Forbidden moves:** renaming `AudioSend`/`AudioSendId`/serde fields/command names; "while I'm here" copy edits beyond the word.
- **Test scope:** `cargo test -p manifold-io --lib` (proves save format untouched) + focused ui lib.

### Phase 5 — Drawer presets
- **Entry state:** `AudioModShape` field names read from `manifold-core` (§3.4 marker); drawer builder API skimmed (`panels/drawer.rs`).
- **Read-back:** D8 + the §3.4 table. Restate: one-shot fill, no stored preset state.
- **Deliverables:** Preset segmented row in the audio `DrawerSpec`; one EditingService command writing the five fields; the four presets per the table.
- **Gate:** *Positive:* in-app — click Pump on a param with a kick send: slider pumps; undo reverts all five fields in one step. *Negative:* `rg "preset" crates/manifold-core/src/audio_mod.rs` → zero hits (no persisted preset field).
- **Forbidden moves:** persisting preset identity; per-field commands (must be one undo step); inventing new features/bands for a preset.
- **Test scope:** focused `cargo test -p manifold-editing --lib` for the command + in-app check.

## 5. Decided — do not reopen

1. Analysis stays on the content thread (mix-before-analyze is the point).
2. `MAX_SENDS = 16` stands; per-send gating is the scaling answer, not a bigger cap.
3. One panel, not a Devices/Tuning split.
4. Rename is strings-only; serialized names never change.
5. Consumers list navigates; it does not edit.
6. Presets are one-shot fills with no stored identity.
7. Scope shows one send at a time (a per-send grid was considered and rejected: N× VQT + N textures for a calibration surface you use one send at a time).

## 6. Deferred

- **Per-send attack/release in the setup panel** — already rejected in `AUDIO_INFRASTRUCTURE.md` §7; belongs in the drawer if ever.
- **Jump-opens-the-drawer** (consumer click auto-expands the exact param drawer) — revive if layer-select proves too coarse in use.
- **v2 pitch features in presets** — when the ridge tracker ships.
- **User-defined presets** — revive if the four built-ins see real use and Peter asks.
