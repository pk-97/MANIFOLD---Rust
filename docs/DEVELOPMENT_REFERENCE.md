# Development Reference

<!-- index: Quick reference grab-bag: texture formats, math gotchas, module layout, and the ui_bridge split. Facts you reach for mid-task. -->

## Texture Format Mapping

| Format | Metal (manifold-gpu) | Notes |
|---|---|---|
| Single-channel 32-bit | `R32Float` | NOT filterable — can't use `textureSample` |
| Two-channel 32-bit | `Rg32Float` | NOT filterable |
| Four-channel 32-bit | `Rgba32Float` | NOT filterable |
| Four-channel 16-bit | `Rgba16Float` | Always fine, filterable + storage |
| Single-channel 16-bit | `R16Float` | No STORAGE_BINDING on Metal |

## Math Gotchas

| Operation | Correct Rust | Trap |
|---|---|---|
| Round to int | `x.round() as i32` | NOT truncation (`as i32` alone) |
| Lerp | `a + (b - a) * t.clamp(0.0, 1.0)` | Lerp CLAMPS t |
| Repeat(t, len) | `t - (t / len).floor() * len` | NOT `t % len` (negative values differ) |
| Sign(0) | `1.0` | NOT `0.0` |

## UI Renderer Invariant

A draw command's clip (and depth) is **bound at enqueue, never inferred at flush**. All four command types carry it per command: rect `RectCommand::clip`, line `LineCommand::clip`, image `ImageCommand::clip`, text `clip_bounds`. Batches are DERIVED in `prepare()` by run-scanning consecutive equal `(clip, depth)` commands — there is no "pending run" whose scissor gets decided later. History: BUG-060 (hardest bug in repo history) existed because rect scissors were stamped per batch at flush time, and a transform/depth boundary mid-traversal flushed pending tree rects under the immediate clip (`None`); depth had the same class of bug earlier (`22c5d528`). If you add a new command type, give it `clip` + `depth` fields captured at the push site.

Corollary (2026-07-10, sibling of BUG-060): a tree node's **text/icon clip is the tree clip intersected with the node's own rect**, bound in `draw_node` at enqueue — a label longer than its widget cuts at the edge instead of overrunning the neighbour. Containment is structural, not a per-call-site elide. Proof: `manifold-renderer/tests/text_clip_to_node_bounds.rs` (pixel-asserted both ways).

## Key Module Splits

- `manifold-app/src/ui_bridge/` — 8 modules: mod, transport, editing, inspector, layer, project, state_sync, marker
- `manifold-app/src/` — `app.rs` + `app_render.rs` + `app_lifecycle.rs`
- `manifold-renderer/src/node_graph/` — the graph runtime: `primitive.rs`, `graph.rs`, `execution_plan.rs`, `state_store.rs`, `metal_backend.rs`, `bundled_presets.rs`, plus the `primitives/`, `atomic/`, and `composites/` subdirectories
- `manifold-renderer/src/node_graph/primitives/` — ~30 primitives (one file per primitive, auto-registered via `inventory::submit!`)
- `manifold-renderer/src/node_graph/atomic/` — irreducible complex kernels (FluidSim2D, FluidSim3D, Plasma, Glitch)
- `manifold-renderer/src/node_graph/composites/` — 6 Rust composite builders (Bloom, Halation, Infrared, Mirror, SoftFocus, StrobeOpacity) retained as dev fixtures for parity tests; new composites ship as JSON
- `crates/manifold-renderer/assets/effect-presets/` — 29 JSON-authoritative presets, codegened into `BUNDLED_PRESETS` by `build.rs`
- `manifold-renderer/src/effects/` — 6 legacy monolithic effect impls retained because their primitives wrap them (auto_gain, blob_tracking, depth_of_field, infrared, quad_mirror, wireframe_depth) + `compute_blit_helper` + `compute_dual_blit_helper`
- `manifold-renderer/src/generators/` — 23 generator impls + shared infrastructure (registry, line_pipeline, compute_common, stateful_base, generator_math). Still on the legacy `inventory::submit! { GeneratorMetadata, GeneratorFactory }` workflow; JSON migration pending.

## Effect Pipeline

Effects run through the node graph: every preset is a `ChainGraph` of typed primitives, walked by an `ExecutionPlan` once per frame. The graph runtime is the sole dispatcher; the legacy linear chain dispatcher was deleted in the May 2026 migration.

Primitives use compute dispatches via the `Primitive` trait (each primitive's `run` method binds inputs/outputs/params and submits its work to a `GpuEncoder`). The legacy `ComputeBlitHelper` (single source) and `ComputeDualBlitHelper` (dual source) helpers still back the 6 retained legacy effect impls. Render passes (`draw_fullscreen`) are only for non-effect paths: output presenter blit, UI atlas blit, line/dot rendering.

- Async compute: independent layers generate in parallel `MTLCommandBuffer`s, compositor waits via `MTLEvent`
- Texture pool: frame-stamped recycling, zero per-frame allocations after 3-frame warmup
- Function constants: specialized Metal pipelines per effect mode
- MTLBinaryArchive: compiled pipeline cache on disk
- `set_fast_math_enabled(true)` globally
- Skip-passthrough via slot aliasing: when an effect's skip condition is met (e.g. amount=0), no GPU work runs and the output slot aliases to the input — zero-cost bypass

## Resolution Scaling

All generators render at full output resolution. The per-generator `internal_resolution_scale()` trait method and the `UpscaleMode` enum were removed (the infrastructure was wired but the default `Native` mode disabled it, so it was dead code in practice). If a specific generator needs internal downscaling for performance, it allocates its own reduced-resolution intermediate inside `render()` and stretches to the output — same pattern Bloom uses for its mip chain.

The pipeline-wide `render_scale` setting (FSR / MetalFX full-frame upscaling) is separate and still active.

## Recorder soundcheck

The day before a gig, on the rig, run the pre-gig soak — the instrument check for the live
show recorder:

```
cargo run --release -p manifold-recording --features recording-proofs --bin recording-soak
```

With no flags this drives the actual show configuration through the real
`LiveRecordingSession` API into the real native AVAssetWriter encoder: 4K60 SDR ProRes, 20
media-minutes (72,000 frames, ~17.5 GB), synthetic 48kHz stereo audio, encoding as fast as the
hardware allows (100% encoder duty — the video pool is never throttled to wall clock). At the
end it prints exactly one line:

```
SOAK PASS: 72000 frames, 0 dropped, PTS monotonic, gap-free indices, 17.4 GB, audio 1200.0s
```

or, if something's wrong:

```
SOAK FAIL: <first failed check, with numbers>
```

**A PASS means the recorder survives a full take on this machine, this OS build, this disk** —
not "the code compiles," not "stop() returned Ok" (two of the three worst live-recording
failures this system has actually produced returned `Ok` from `stop()` while the file was
unusable). macOS updates have already changed hardware encoder behavior once without any
MANIFOLD change being involved — this is the check that catches the next one before it costs a
show, the same way you'd line-check the LEDs before a set.

Useful flags:

- `--width`/`--height`/`--fps`/`--minutes` — override the take size (a shorter run at a lower
  resolution is a fast sanity check, not a substitute for the full-scale run before a real gig).
- `--no-audio` — video only.
- `--realtime` — paces submissions to wall clock instead of running flat-out; a true dress
  rehearsal, but gates on file validity only (drops are reported, not failed on — keep-up under
  a loaded rig is a separate concern from "did the file come out valid").
- `--keep` — keep the output file even on PASS (default: PASS deletes it, FAIL always keeps it
  and prints its path — the failed file is the evidence).
- `--output <path>` — write somewhere other than a temp path.
- `--hdr` — refuses immediately; HDR live recording is BUG-053 (structurally broken today).

See `docs/LIVE_RECORDING_PROOFS_DESIGN.md` §5 for the full design and gate semantics, and
`docs/BUG_BACKLOG.md` BUG-085/BUG-086 for two accounting caveats found building this suite
(Rust's own frame/sample counters can be optimistic under real backpressure — the soak's PASS
decision is anchored to what ffprobe actually decodes out of the file, not to those counters).
