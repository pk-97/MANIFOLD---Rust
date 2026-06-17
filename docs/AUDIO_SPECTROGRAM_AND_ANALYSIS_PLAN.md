# Audio Analysis, Spectrogram & the Texture-Pane API — Plan

<!-- index: Forward plan to deepen audio-modulation analysis (band-energy fix, onset, synchrosqueeze pitch) and add a per-send VQT spectrogram to the Audio Setup panel as a calibration tool, reusing the Analyzer VST's CQT pipeline. Also defines the end-game "live GPU texture in a UI region" API (TexturePane) the spectrogram is the first consumer of. Phased build order + verified findings. -->

Design determined in session 2026-06-17. **Status: plan only — nothing here is built yet.** Builds on [Audio Modulation](AUDIO_MODULATION_DESIGN.md) (the modulation source + drawer) and [Audio Infrastructure](AUDIO_INFRASTRUCTURE.md) (capture, worker, device directory).

---

## 1. Goal, for the instrument

Two intertwined goals:

1. **Make audio modulation actually usable and intelligent.** Today the worker computes only `amplitude` (RMS) and `band_energy` (a mis-scaled 3-band FFT). `onset` and all pitch fields exist but read zero — so the headline thesis (a bassline's *pitch movement* drives a visual parameter) is unbuilt, and the drawer's `On` button does nothing. We build it out: fix band energy, add onset, then port synchrosqueeze pitch tracking.
2. **Give the performer a way to dial it in.** A per-send **VQT spectrogram** in the Audio Setup panel, with the modulation's band (and later the tracked pitch ridge) drawn on it, so you can *see* where your kick/bass lands and calibrate against it.

The spectrogram is the forcing function; the analysis depth is the end goal. They converge: the same VQT that draws the spectrogram is the engine that extracts pitch in v2.

## 2. Decisions

- **Direction: "option 2".** The VQT becomes both the calibration view *and*, in v2, the analysis basis (band energy → sum VQT bins per band, naturally log-spaced/perceptual; onset → spectral flux; pitch → synchrosqueeze ridge). Staged — spectrogram first.
- **Spectrogram lives only in Audio Setup**, as a per-send detail scope for the *selected* send (one at a time = natural cost gate). **No dedicated drawer button** — the panel is reachable from the header Audio button / ⌘⇧A, and the "A" button on a slider with no sends already opens Audio Setup. Send selection is a new gesture *inside* the panel.
- **Reuse the Analyzer's CQT pipeline.** It already runs on `manifold-gpu` (path dep), including `GpuFft` and a full synchrosqueeze ridge tracker. Factor the reusable parts into a shared `manifold-spectral` crate.
- **Producer is worker-side, not present-pass.** (See §4.1.)
- **Build the "live texture in a UI region" API properly** (`TexturePane`, §5) — this is the "graphics API upgrade." Built fresh for the spectrogram (its first consumer). **Scope cut:** we do *not* retrofit the existing inline blits (node-preview, master-out, thumbnails) or migrate the VST onto it — those keep their current copy-pasted form and their latent resize hazard for now (§3). `TexturePane` is the clean path forward for new consumers, not a consolidation pass over old ones.

## 3. Verified findings (2026-06-17, checked against code)

- **Band energy is mis-scaled.** `BandEnergyAnalyzer::analyze` computes `sqrt(Σ|X|²/N)` on unnormalized rustfft output with no Hann power correction → scales ~√N, not bounded 0..1. That's why `sensitivity` is load-bearing — it papers over a scaling bug, not taste. Fix: ÷N² + Hann power correction (÷0.375) → true band-limited RMS in the same 0..1 units as `amplitude`. Small, independent, shippable alone. ([analysis.rs](../crates/manifold-audio/src/analysis.rs))
- **Per-send gain does not exist.** `gain_db` is in no struct — not `AudioSend` (core), not `SendSpec` (worker), not the panel. The docs describe a per-send gain trim as the calibration surface; it was never built. Calibration today has only the per-mod `sensitivity`. A spectrogram you calibrate against presumes a per-send gain we must add.
- **The capture ring is single-consumer** (`AudioCaptureDevice::take_consumer` → `Option`, taken once). Nothing can add a second sample tap, so the spectrogram's VQT producer *must* live in/beside the existing audio worker — not on the UI device. ([capture.rs](../crates/manifold-audio/src/capture.rs))
- **The Analyzer proves the producer shape:** the worker produces VQT columns off-thread; rendering is **one cheap fullscreen pass** sampling a history storage buffer. The FFT is never in the draw. (`N_FFT=65536`, hop 256 ≈ 188 cols/s — heavier than a modulation use needs; we'll pick a smaller N_FFT/hop.) ([spectrum_gpu.rs](../plugins/manifold-analyzer-gui/src/spectrum_gpu.rs))
- **Visualization is separable from the transform.** dB conversion (`FLOOR_DB=-140`), colormap, log-y mapping, and scroll live in `spectrum_gpu.rs` + `spectrum_line.wgsl` — the WGSL shader + draw port cleanly; the egui-GL sampling (`gpu_bridge`/`gl_paint`) does **not**.
- **Two IOSurface bridges already exist** — the app's `SharedTextureBridge` and the Analyzer's `gpu_bridge::IoSurfaceMtlTexture`. `manifold-spectral` should target the app's; the VST migrates onto it later, retiring the second.
- **The texture-in-UI path is copy-pasted, with a real hazard.** Node-preview, master-out, and thumbnail blits are near-identical inline blocks in the present pass. Cached `GpuTexture`s go stale on window resize; the only guard is a per-consumer `generation()` check that's easy to forget — a new consumer that skips it GPU-faults on first resize. ([app_render.rs](../crates/manifold-app/src/app_render.rs))

## 4. The spectrogram

### 4.1 Producer (worker-side)

The audio worker already owns the capture consumer and runs an FFT per send. Extend it to compute **VQT columns for the selected send only**, gated on the Audio Setup panel being open, published latest-wins into a small column ring. This keeps all heavy DSP off the vsync-timed present path (which has the mach_wait_until + spin discipline — see [VSYNC_AND_FRAME_PACING](VSYNC_AND_FRAME_PACING.md)) and converges with v2, where the worker runs the VQT for features anyway.

### 4.2 Render & the calibration overlays

A cheap fullscreen pass samples the column history into a `GpuTexture` (port `spectrum_line.wgsl`). The panel reserves a scope rect; the texture blits in via `TexturePane` (§5). Overlaid, as UI atlas nodes on top:

- **Band dividers** (the low/mid/high split the modulation reads) on a log-frequency axis.
- **Frequency axis labels.**
- **(v2) The synchrosqueeze pitch ridge** — the line the modulation actually follows.

The scope must show the **post-gain** analyzed mono signal (what feeds analysis), not raw input, or it can't calibrate gain.

## 5. The Texture-Pane API (end-game)

The underlying `SharedTextureBridge` (IOSurface triple-buffer, generation counter, atomic front-index) is sound and perform-critical — **wrap it, don't replace it.** What's missing is one owning type so the unsafe re-import lifecycle and the build/present geometry stop being copy-pasted per consumer.

```rust
/// A rectangular UI region whose pixels come from a live GPU texture.
pub struct TexturePane { /* id, rect, desired_size, source */ }

enum Source {
    /// UI-produced, same device (the spectrogram). No bridge, no triple-buffer,
    /// no publish_front fence discipline — the simple case, first-class.
    Local(GpuTexture),
    /// Cross-device, via the existing IOSurface bridge (node-preview, master-out).
    Bridged(Arc<SharedTextureBridge>),
}

impl TexturePane {
    /// The keystone. Never hands out a cached texture, so the caller can't hold
    /// a stale one across a resize. Internally caches per-surface keyed by
    /// generation() and re-imports only on change — owns invalidation.
    fn current(&mut self, ui_device: &GpuDevice) -> Option<&GpuTexture>;
}
```

The bottom primitive is a single `blit_texture_pane` helper (collapses the per-consumer blit boilerplate). A pane registry — panels register + a generic present loop blits all visible panes — is the natural next step *if a second consumer arrives*, but with only the spectrogram on the API it's premature; build the helper + `TexturePane`, blit the one pane directly. Lives UI-side — no new `Arc<Mutex>`, doesn't touch the two-thread model; the bridge stays the same Arc-shared object.

## 6. Build plan — phases in optimal order

**Critical path: 3 → 5 → 7 → 8.** Phases 0, 4(gain), and 5 hang off it without blocking. The spectrogram ships at Phase 4; v2 is an independent follow-on.

### Phase 0 — Independent quick wins (no deps)
1. Band-energy scaling fix (÷N² + Hann); update tests.
2. Drawer cleanup: remove the "+" send button; "A"-with-no-sends → `OpenAudioSetup`; delete the dead `AudioNewSend`/`AudioModNewSend` path. Unit-test the drawer hit-test.

### Phase 1 — Foundations (parallelizable)
3. Extract `manifold-spectral`: `cqt.rs` + `gpu_cqt.rs` + `spectrum_line.wgsl` + dB/colormap/column logic; reuse the Analyzer's CPU unit tests. Leave egui-GL behind. VST not migrated yet.
4. Per-send **gain**: `gain_db` on `AudioSend` + `SendSpec` + worker downmix; panel control; `EditingService` command; serialization.

### Phase 2 — Worker-side producer (deps: 3)
5. Extend the worker to compute VQT columns for the selected send, gated on Audio Setup open, published latest-wins. Tune N_FFT/hop for modulation use.

### Phase 3 — Texture-pane API, de-risked on one consumer
6. `blit_texture_pane` helper — collapse the three existing inline blits' boilerplate.
7. `TexturePane` + safe `current()` (`Local` + `Bridged`). Spectrogram is first user as `Local`/worker-fed.

### Phase 4 — Spectrogram in Audio Setup (deps: 5, 7; + 4 for usefulness)
8. Panel: send-selection state + reserved scope rect + cheap fullscreen draw → `GpuTexture` → blit via `TexturePane`.
9. Overlays: band dividers + log-freq axis; show the post-gain signal. Verify with the app running.

### Phase 5 — v2 intelligence (deps: 3, 5)
10. Onset: spectral flux off the transform; wire the `On` feature live.
11. Pitch / pitch-delta / confidence: port the synchrosqueeze ridge; per-send opt-in via `SendAnalysisConfig`; ridge overlay on the waterfall.

**Out of scope (deliberately cut):** retrofitting the existing inline blits onto `TexturePane`, a pane registry, a format-generic bridge, and migrating the VST onto `manifold-spectral`. The focus is the spectrogram + the `TexturePane` graphics API it needs — not a consolidation pass over the rest of the app. These remain available as a future cleanup if a second `TexturePane` consumer ever justifies them.

## 7. Open questions

- **N_FFT / hop for modulation use.** The Analyzer's 65536/256 is tuned for a wide visual window; pick a lighter pair for per-send calibration.
- **Where the cheap render pass runs.** Worker-side into an IOSurface (needs the bridge) vs UI-side from published columns (`Local`, no bridge). The column ring crossing threads is the deciding factor; lean `Local` if columns are small enough to publish latest-wins.
- **Gain as project content vs rig preference** — same question the device selection faced; default to project content with the send.
- **Per-send cost ceiling for v2** — how many sends can run the ridge tracker before the worker falls behind (bounded by per-send opt-in).
