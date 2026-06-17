# Audio Spectrogram & the Texture-Pane API — Plan

<!-- index: Plan to add a per-send VQT spectrogram to the Audio Setup panel as a calibration tool, reusing the Analyzer VST's CQT pipeline via a new manifold-spectral crate, and the end-game "live GPU texture in a UI region" API (TexturePane) it is the first consumer of. Single-owner, phased build order + verified findings. -->

Design determined in session 2026-06-17. **Status: implemented (2026-06-17).** Phases 0–5 shipped on `audio-modulation`: drawer cleanup, the `TexturePane` graphics API, per-send gain, the `manifold-spectral` crate (CPU VQT + GPU waterfall renderer), the worker column producer, and the Audio Setup scope (selected-send waterfall with band dividers + log-frequency axis). Builds on [Audio Modulation](AUDIO_MODULATION_DESIGN.md) (the modulation source + drawer) and [Audio Infrastructure](AUDIO_INFRASTRUCTURE.md) (capture, worker, device directory).

---

## 1. Goal, for the instrument

**Deliverable: a per-send VQT spectrogram in the Audio Setup panel** — with the modulation's band (and, if pitch tracking ever lands, the tracked ridge) drawn on it — so the performer can *see* where their kick/bass lands and calibrate against it. Plus the `TexturePane` graphics API it needs to get a live GPU texture onscreen.

This is a calibration tool. The deeper modulation *analysis* — making `onset` and the pitch features actually fire — is a **separate, later concern, out of scope here**; so is the band-energy scaling fix (§3). This plan ships the spectrogram and everything it requires, no more. The spectrogram and that future analysis converge on the same VQT engine, which is exactly why building it here (`manifold-spectral`, §4) lays the groundwork without committing to the analysis features now.

## 2. Decisions

- **Spectrogram lives only in Audio Setup**, as a per-send detail scope for the *selected* send (one at a time = natural cost gate). **No dedicated drawer button** — the panel is reachable from the header Audio button / ⌘⇧A, and the "A" button on a slider with no sends opens Audio Setup. Send selection is a new gesture *inside* the panel.
- **Reuse the Analyzer's CQT pipeline.** It already runs on `manifold-gpu` (path dep), including `GpuFft`. Factor the reusable DSP into a new `manifold-spectral` crate; the visualization (dB/colormap/scroll) ports as a WGSL shader, the egui-GL sampling does not.
- **Producer is worker-side, not present-pass.** The capture ring is single-consumer, so the VQT producer lives in/beside the existing audio worker, publishing magnitude columns latest-wins (§4.1). Heavy DSP never touches the vsync-timed present path.
- **The column stream is an internal interface** (we own both ends): raw linear per-bin magnitudes (length = VQT bin count) + bin freq metadata for log-y, published latest-wins, gated on the panel being open. dB/colormap happen in our shader, so the producer stays dumb.
- **Build the texture-in-UI API properly** (`TexturePane`, §5) — the "graphics API upgrade." Built fresh for the spectrogram. **Scope cut:** we do *not* retrofit the existing inline blits (node-preview, master-out, thumbnails), add a pane registry, make the bridge format-generic, or migrate the VST. Those stay as-is; `TexturePane` is the clean path forward, not a consolidation pass.

## 3. Verified findings (2026-06-17, checked against code)

- **Per-send gain does not exist.** `gain_db` is in no struct — not `AudioSend` (core), not `SendSpec` (worker), not the panel. The docs describe a per-send gain trim; it was never built. We add it (Phase 2), default 0 dB.
- **The capture ring is single-consumer** (`AudioCaptureDevice::take_consumer` → `Option`, taken once) — forces the VQT producer to live in/beside the worker. ([capture.rs](../crates/manifold-audio/src/capture.rs))
- **The Analyzer proves the producer shape:** the worker produces VQT columns off-thread; rendering is **one cheap fullscreen pass** sampling a history storage buffer. The FFT is never in the draw. (`N_FFT=65536`, hop 256 — heavier than we need; we pick a lighter pair.) ([spectrum_gpu.rs](../plugins/manifold-analyzer-gui/src/spectrum_gpu.rs))
- **Visualization is separable from the transform.** dB (`FLOOR_DB=-140`), colormap, log-y, scroll live in `spectrum_gpu.rs` + `spectrum_line.wgsl` — the WGSL shader + draw port cleanly; the egui-GL sampling (`gpu_bridge`/`gl_paint`) does not.
- **Two IOSurface bridges already exist** — the app's `SharedTextureBridge` and the Analyzer's `gpu_bridge::IoSurfaceMtlTexture`. `manifold-spectral` targets the app's; the Analyzer's stays put (no VST migration in scope).
- **The texture-in-UI path is copy-pasted, with a real hazard.** Node-preview, master-out, and thumbnail blits are near-identical inline blocks in the present pass. Cached `GpuTexture`s go stale on window resize; the only guard is a per-consumer `generation()` check that's easy to forget — a new consumer that skips it GPU-faults on first resize. `TexturePane`'s `current()` makes that unrepresentable for *our* consumer. ([app_render.rs](../crates/manifold-app/src/app_render.rs))
- **Band energy is mis-scaled** (`sqrt(Σ|X|²/N)` on unnormalized rustfft, no Hann power correction → not 0..1). Independent fix (÷N² + Hann), **out of scope here** but noted so it isn't lost. ([analysis.rs](../crates/manifold-audio/src/analysis.rs))

## 4. The spectrogram

### 4.1 Producer (worker-side)

The audio worker already owns the capture consumer and runs an FFT per send. Extend it to compute **VQT columns for the selected send only** (via `manifold-spectral`), gated on the Audio Setup panel being open, published latest-wins into a small column ring. Keeps all heavy DSP off the vsync-timed present path (see [VSYNC_AND_FRAME_PACING](VSYNC_AND_FRAME_PACING.md)).

### 4.2 Render & overlays

A cheap fullscreen pass samples the column history into a `GpuTexture` (port `spectrum_line.wgsl`). The panel reserves a scope rect; the texture blits in via `TexturePane` (§5). Overlaid as UI atlas nodes: **band dividers** (the low/mid/high split the modulation reads) on a log-frequency axis, **frequency labels**, and the scope shows the **post-gain** analyzed mono signal (what feeds analysis), not raw input.

## 5. The Texture-Pane API

The underlying `SharedTextureBridge` (IOSurface triple-buffer, generation counter, atomic front-index) is sound and perform-critical — **wrap it, don't replace it.** One owning type so the unsafe re-import lifecycle and the build/present geometry stop being copy-pasted:

```rust
/// A rectangular UI region whose pixels come from a live GPU texture.
pub struct TexturePane { /* id, rect, desired_size, source */ }

enum Source {
    /// UI-produced, same device (the spectrogram). No bridge, no triple-buffer,
    /// no publish_front fence discipline — the simple, first-class case.
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

Bottom primitive: a `blit_texture_pane` helper that collapses the per-consumer blit boilerplate. With only the spectrogram on the API, blit the one pane directly — a registry is the natural next step *if a second consumer arrives*, not now. Lives UI-side — no new `Arc<Mutex>`, doesn't touch the two-thread model.

## 6. Build plan — phases in optimal order

Critical path: **3 → 4 → 5** (the DSP chain) and **1 → 5** (graphics). Phases 0 and 2 are independent and unblock immediately. The spectrogram ships at the end of Phase 5.

### Phase 0 — Drawer cleanup (isolated, no deps) — *in progress*
1. Remove the "+" send button from the audio drawer; "A"-with-no-sends → `OpenAudioSetup` (not create-a-send); delete the dead `AudioNewSend`/`AudioModNewSend` path. Unit-test the drawer hit-test.

### Phase 1 — Texture-pane graphics API
2. `blit_texture_pane` helper — collapse the inline blit boilerplate into one call.
3. `TexturePane` + safe `current()` (`Local` + `Bridged` sources). Spectrogram is its first user as `Local`.

### Phase 2 — Per-send gain (calibration prerequisite)
4. `gain_db` on `AudioSend` + `SendSpec` + worker downmix; panel control; `EditingService` command; serialization. **Defaults to 0 dB (unity)** — opt-in trim, not a required step.

### Phase 3 — `manifold-spectral` crate
5. Extract `cqt.rs` + `gpu_cqt.rs` + `spectrum_line.wgsl` + the dB/colormap/column logic from the Analyzer; reuse its CPU unit tests; leave egui-GL behind. Pick a lighter N_FFT/hop than the Analyzer's 65536/256.

### Phase 4 — Worker column producer (deps: 5)
6. Extend the worker to compute VQT columns for the selected send via `manifold-spectral`, gated on Audio Setup open, published latest-wins.

### Phase 5 — Spectrogram in Audio Setup (deps: 3, 6; + 4 for usefulness)
7. Panel: send-selection state + reserved scope rect + cheap fullscreen draw of published columns → `GpuTexture` → blit via `TexturePane`.
8. Overlays: band dividers + log-freq axis; show the post-gain signal. Verify with the app running.

**Out of scope:** v2 modulation analysis (onset, pitch, synchrosqueeze); the band-energy scaling fix; retrofitting existing blits onto `TexturePane`; a pane registry; a format-generic bridge; VST migration. All remain future work.

## 7. Open questions

- **N_FFT / hop for this use** — lighter than the Analyzer's 65536/256; pick for per-send calibration legibility vs cost.
- **Where the cheap render pass runs** — UI-side from published columns (`Local`, no bridge) vs worker-side into an IOSurface (needs the bridge). Lean `Local` if columns are small enough to publish latest-wins.
- **Gain as project content vs rig preference** — default to project content with the send, like device selection.
