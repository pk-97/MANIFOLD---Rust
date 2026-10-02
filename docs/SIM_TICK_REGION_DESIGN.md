# Sim Tick Region — one fixed-rate clock for every stateful graph loop

<!-- index: One shared fixed-tick region for stateful graph loops (feedback, array_feedback, Gray-Scott) so every non-liquid sim gives the same result at any frame rate, live or export; plus the closed-form rule for loops that hold no other state. -->

**Status:** PROPOSED · 2026-10-03 · Opus 5.5 design lane · owes Peter calls C1–C4 (section 9 (Calls only Peter can make))
**Prerequisites:** none to start P1; P2 onward needs C1 answered.
**Execution contract:** read DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

Peter, 2026-10-03: every simulation runs "100% independent of display frame time and frame rate" — the same project gives the same sim at 24/30/60/120 fps, live or export, fast machine or slow. Today only the liquids have a tick clock. Every other stateful loop advances once per display frame, so trails are half as long at 120 fps, Gray-Scott coral grows twice as fast, and particle random walks change variance.

The governing insight: every one of those loops already has the shape the substep compiler contracts. A `node.feedback` or `node.array_feedback` is a back edge with a seed, a capture and a state output — exactly a `SubstepBoundaryPorts`. So the fix is not one clock per primitive. Give those two boundary nodes a clock, let the existing region compiler contract their bodies, and run each body once per fixed tick from a shared clock with `LiquidClock` semantics. Loops whose body is a pure pointwise decay take an exact per-frame closed form instead, because a region would buy nothing.

On stage: a feedback trail, a particle swarm or a reaction-diffusion dish looks the same in the 24 fps export as it did live at 60, and a slow venue laptop shows the same motion as the studio machine (up to the stall policy, C1).

Companions: LIQUID_SOLVER_SEAM_DESIGN.md section 3.4 (Clock, pause, speed, reset, export) — the clock semantics reused here; FREEZE_COMPILER_MAP.md — substep regions under freeze; DECOMPOSING_GENERATORS.md — stateful primitives; audit `/tmp/fps_coupling_audit.md` (2026-10-03, not committed).

## 1. Audit — what exists (verified 2026-10-03 at `e1d8efe8e`)

R = `crates/manifold-renderer/src`. Extend, don't redesign.

| Piece | Where | State |
|---|---|---|
| Fixed 60 Hz clock with epoch, restart, held, speed anchor, live cap 3 + drop/re-anchor, `tick_starts` | `R/node_graph/liquid/clock.rs` (`LiquidClock`, `ClockFrame`, `MAX_LIVE_TICKS`) | Shipped, liquids only |
| Tick length constant | `R/node_graph/fluid.rs` (`TICK`) | Fixed 1/60 |
| Substep region compiler: boundary + body contracted at compile time, `count` repeats, per-iteration scalars, optional clock port, no nesting | `R/node_graph/substeps.rs` (`SubstepBoundaryPorts`, `SubstepRegion`) | Shipped, used by SWASH and MPM |
| Per-iteration hooks | `R/node_graph/effect_node.rs:984` (`substep_iteration`), `:994` (`substep_host_sync`) | Shipped |
| Per-tick CPU input replay (force field sampled at each tick start) | `R/preset_runtime/physics_sampling.rs` (`retain_physics_setup_outputs`, tick times) | Shipped for GPU liquids |
| Box3D accumulator | `R/node_graph/physics.rs:1020-1234` | Frame-exact, own clock, out of scope |
| Texture feedback back edge | `R/node_graph/primitives/temporal.rs` (`node.feedback`) | Per frame — BUG-m5fg (texture feedback trails decay per frame) |
| Particle back edge | `R/node_graph/primitives/array_feedback.rs` | Per frame — BUG-n7qb (particle generators integrate per frame) |
| Frame-count seeds | `anti_clump_particles.rs:97,109,158` (`derived_uniforms: frame_count`), `array_diffuse_particles.rs:137`, `inject_burst.rs:169` | Frame-indexed — BUG-uc7p (inject_burst seed and one-euro alpha) |
| Gray-Scott: four hand-copied substep kernels per frame | `assets/reference-presets/ReactionDiffusion.json:729-753` | Per frame — BUG-9wa4 (ReactionDiffusion 4 substeps per frame) |
| Exact per-frame closed forms | `envelope_decay`, `envelope_follower_ar`, `smoothing`, `compressor_envelope` | Frame-exact for held input |
| Frame-clocked dt fed as target when frames are missed | `manifold-app/src/frame_timer.rs:368` | BUG-gm15 (frame-clocked timer feeds target dt) |

Presets with a `node.feedback`/`node.array_feedback` back edge: StylizedFeedback, Watercolor, WireframeDepth, DataMosh, MotionMosh (effects); OilyFluid, Lightning, Cymatics, FluidSim2D, FluidSim3D, ParticleText, MetallicGlass (generators); ReactionDiffusion (reference).

Primitive audit per DECOMPOSING_GENERATORS.md section 2.5 (primitive audit): no new primitive. The clock generalizes `LiquidClock`; the region is the existing substep region; the boundaries are the existing feedback nodes gaining ports. Exists / one wire away.

## 2. Decisions

**D1 — The tick region is a substep region whose boundary is a feedback node.** `node.feedback` and `node.array_feedback` declare `SubstepBoundaryPorts` with `clock: Some(...)` when their `Clock` param is `Tick`. The compiler contracts body = descendants of the boundary that are ancestors of its capture, as today. Count per frame = ticks due from the clock. Rejected: a per-primitive `dt` fix in each kernel, because it fixes one integrator and leaves the warps, kicks and seeds inside the loop rate-dependent — explicit Euler with a variable dt is itself not rate-independent. Rejected: a new `node.sim_region` group node, because the back edge already marks the loop and a second marker can disagree with it.

**D2 — One clock type: `SimClock`, which is `LiquidClock` moved and given a rate.** `R/node_graph/sim_clock.rs`. `LiquidClock` becomes an alias constructed at 60 Hz, so liquid code and tests do not change. `TICK` becomes `SimClock::tick_seconds()`. Rejected: a second clock beside `LiquidClock`, because two clocks with the same job drift apart (the audit's whole finding).

**D3 — Opt-in lives on the boundary node, never the primitive inside.** A body primitive does not know it is ticked; it sees `ctx.time.delta` = one tick and `ctx.time.tick_index`. A graph opts in by setting the boundary's `Clock` param to `Tick`. Default for new nodes: `Tick`. Migrated presets get `Tick` explicitly in their JSON (P3–P5). `Frame` stays only for the closed-form loops of D7, and invariant I1 requires each to pass the pure-decay check.

**D4 — Rate: param `Sim Rate` (Hz) on the boundary, default 60, no cap.** A rate edit restarts the clock (new epoch, reseed), like a setup change under LIQUID_SOLVER_SEAM_DESIGN.md section 3.4 (Clock, pause, speed, reset, export). Any positive value is accepted; cost scales linearly (see Cost below) and is the user's to spend.

**D5 — Per-tick input sampling reuses the physics replay.** Every wire entering a tick region body from outside is classified by the compiler. CPU-only ancestry (params, LFOs, envelopes, audio features, modulation) is re-evaluated at each tick's transport time through the same ancestry replay `physics_sampling.rs` runs for `acceleration_field`, and written into the body as a per-iteration scalar via `substep_iteration`. GPU ancestry (a texture or array produced outside the region, such as the video under a feedback effect) is sampled once per frame and held for that frame's ticks. Rejected: re-rendering GPU inputs per tick, because a video frame does not exist between frames and the upstream graph would itself run N times. Consequence, stated honestly: an effect loop fed by live video is tick-exact in its own dynamics but sees its input change on frame boundaries; at 24 vs 60 fps the input is a different staircase. That is a property of the input, not the sim (C3).

**D6 — Seeds are tick-indexed or event-indexed, never frame-indexed.** `TimeContext` gains `tick_index: u64` (ticks since the region's epoch). `derived_uniforms: frame_count` in `anti_clump_particles` and `array_diffuse_particles` becomes `tick_index`; inside a region one kick per tick is correct by construction, so no dt scaling is added. `inject_burst` seeds by trigger count (the Nth burst lands at the same place in any run), because a burst is an event, not a tick. Rejected: scaling the kick by dt, because it fixes the mean step but not the noise sequence.

**D7 — Closed form per frame only when the loop body is a pure pointwise decay of its own state.** If the captured value is `mix(input, prev, a)` with nothing else touching `prev` (no warp, blur, displacement or nonlinear op), then `a_frame = a^(dt·rate)` is exact for held input and costs one dispatch. Any spatial or nonlinear op on `prev` makes it a tick region. The compiler checks this structurally (I1). `dt` is the clock's transport delta × Speed for the frame, never the frame timer's target, which fixes BUG-gm15 (frame-clocked timer feeds target dt) for every closed form.

**D8 — Regions do not nest (unchanged).** A tick region body may not contain a liquid substep region. Gray-Scott's four copies collapse into one step kernel; count = ticks × `Steps Per Tick` (param, default 4), same as SWASH's D10 in the liquid seam doc.

**D9 — Live stall policy is `LiquidClock`'s until Peter says otherwise (C1).** Live: at most `ceil(3 × rate / 60)` ticks per frame, one tick of debt kept, the rest dropped and re-anchored, reported in `dropped_seconds`. Offline: every due tick. This keeps live real-time but means live differs from export after a hitch — the one place the ruling is not met.

## 3. Data model and seams

```rust
// R/node_graph/sim_clock.rs — LiquidClock moved; advance/set_tick_cap/tick_starts unchanged
pub struct SimClock { rate_hz: f64, /* LiquidClock's fields */ }
impl SimClock {
    pub fn new(rate_hz: f64) -> Self;          // param range enforces rate_hz > 0
    pub fn tick_seconds(&self) -> f64;         // 1 / rate_hz
    pub fn live_tick_cap(&self) -> u32;        // ceil(MAX_LIVE_TICKS * rate_hz / 60)
}
pub type LiquidClock = SimClock;              // SimClock::new(60.0) at every liquid site
// ClockFrame unchanged.

// TimeContext
pub tick_index: u64,                          // 0 outside a tick region

// temporal.rs / array_feedback.rs
// params: Clock { Tick, Frame } (default Tick), Sim Rate (Hz, default 60), Speed (default 1), Reset
// SubstepBoundaryPorts { seed: "seed", capture: "capture", state: "out", clock: Some("clock"), .. }
```

The boundary owns its `SimClock` in its `StateStore` entry; `clear_state` calls `restart()`. Only the boundary's `out` (and declared `results`) leave the region. A preset that reads a body node's output from outside fails compile naming both NodeIds; the migration rewires it to read `out`.

Display: the region publishes its state at `simulation_time`; no inter-tick interpolation in v1 (C4).

## 4. Per-primitive split

| Class | Presets / nodes | Path | Reason |
|---|---|---|---|
| Particle loops | FluidSim2D, FluidSim3D, ParticleText, Cymatics | Tick region on `array_feedback` | Euler integration plus per-step kicks; no closed form exists |
| Warping texture feedback | StylizedFeedback, Watercolor, MetallicGlass, Lightning, OilyFluid, WireframeDepth | Tick region on `feedback` | Zoom, warp or blur on `prev` compounds per step |
| Gray-Scott | ReactionDiffusion | Tick region, one step kernel, count = ticks × Steps Per Tick | Nonlinear PDE; the four copies are a hand-unrolled substep loop |
| Mosh persistence | DataMosh, MotionMosh | Closed form `a^(dt·60)` on the persistence mix; displacement stays per input frame | Motion vectors exist only per input frame; ticking would replay one frame's motion N times (C2) |
| Pure decay trails | any `feedback` whose body passes the D7 check | Closed form | Exact, one dispatch |
| Envelopes, smoothing, compressor | `envelope_decay` and kin | Already closed form; switch dt to clock delta | Frame-exact today except the missed-frame dt |
| One-euro filter | `one_euro_filter.rs:24` | Closed form `alpha = 1 − exp(−dt/tau)` | Exact for held input; drops the first-order approximation |
| Burst seed | `inject_burst.rs:169` | Trigger-count seed | Event, not tick |

## 5. Cost

A tick region runs its body `rate / fps` times per frame: 2.5× at 24 fps, 1× at 60, 0.5× at 120 (a tick on alternate frames). Live is bounded by the D9 stall cap, which is not a param cap — it bounds how much late time one frame repays, not what rate a user may choose. Offline runs every tick; export is slower, never wrong. Freeze fusion applies inside the body as for any substep region, so N ticks cost N fused passes, not N × node count. Honest cost: a heavy particle preset at 24 fps live costs 2.5× today's GPU time; if that blows the frame, D9 drops time and live diverges from export. Whether to degrade quality instead is C1.

## 6. Invariants & enforcement

| ID | Invariant | Enforcement |
|---|---|---|
| I1 | Every `feedback`/`array_feedback` in a shipped preset is `Tick`, or `Frame` with a body that passes the D7 pure-decay check | test `stateful_loops_declare_clock` (walks every preset; fails naming the node) |
| I2 | No sim primitive seeds from `frame_count` | test `sim_seeds_never_frame_indexed` (negative `rg` over anti_clump, array_diffuse, inject_burst) |
| I3 | Same project, same sim at 24/30/60/120 fps offline | one identity test per class (below) |
| I4 | A rate edit restarts the epoch | `sim_clock_rate_edit_restarts` |
| I5 | `LiquidClock` behaviour unchanged | existing `liquid::clock` tests pass unmodified against the alias |
| I6 | Closed forms use clock delta, never frame-timer target dt | `closed_forms_use_transport_delta` (missed-frame fixture) |

Identity tests run offline over 2 s of transport and read back state at t = 2 s. Tick regions: bit-identical across 24/30/60/120 (same tick count, same per-tick inputs). Closed forms: within 1e-5 (float `pow`). One per class: `fps_identity_particles` (FluidSim2D), `fps_identity_warp_feedback` (StylizedFeedback), `fps_identity_gray_scott`, `fps_identity_decay_closed_form`, `fps_identity_anti_clump_seed`, `fps_identity_burst_trigger`.

## 7. Phasing

**P1 — `SimClock` (seam brief).** Entry: HEAD on main. Move `LiquidClock` to `R/node_graph/sim_clock.rs` as `SimClock` with `rate_hz`; alias `LiquidClock`; `TICK` call sites take `tick_seconds()`. Gate: I4, I5, clippy. Forbidden: changing `advance` semantics.

**P2 — Tick boundaries (blocked on C1).** `feedback`/`array_feedback` gain the params and `SubstepBoundaryPorts`; `TimeContext.tick_index`; D5 input classification and per-tick replay through `physics_sampling.rs`. Gate: compiler escape-error tests; I3 on a minimal synthetic loop.

**P3 — Particles.** Migrate the four particle presets and the two seeds. Gate: I1, I2, `fps_identity_particles`, `fps_identity_anti_clump_seed`.

**P4 — Texture feedback and Gray-Scott.** Migrate the six warping presets; collapse ReactionDiffusion. Gate: `fps_identity_warp_feedback`, `fps_identity_gray_scott`, headless PNG at 24 and 60 fps compared.

**P5 — Closed forms.** Mosh persistence (after C2), one-euro, inject_burst, the frame-timer dt source. Gate: I6, `fps_identity_decay_closed_form`, `fps_identity_burst_trigger`.

## 8. Decided — do not reopen

1. The tick region is the substep region with a feedback boundary. 2. One clock type; `LiquidClock` is its alias. 3. Opt-in on the boundary. 4. Default 60 Hz, param, no cap. 5. CPU inputs replay per tick; GPU inputs hold per frame. 6. Seeds by tick or event. 7. Closed form only for pure decay. 8. No nesting.

## 9. Calls only Peter can make

- **C1 — Live stall policy.** Keep `LiquidClock`'s drop and re-anchor (live stays real-time, diverges from export after a hitch), never drop and let the sim lag the music until it catches up, or hold time and degrade quality. Blocks P2. Default if unanswered: drop, as liquids do today.
- **C2 — Mosh.** Treat mosh as per-input-frame (closed-form persistence only, proposed), or tick it and accept repeated motion vectors.
- **C3 — Live-video inputs held per frame inside a tick region** (proposed). The alternative needs frame interpolation; confirm the ruling allows it.
- **C4 — 120 fps display with a 60 Hz sim** shows each state for two frames. Accept the stepping (proposed for v1), or pay a texture copy per region for inter-tick interpolation.

## 10. Deferred

- Inter-tick display interpolation — revived by C4.
- Box3D and FLIP on `SimClock` — revived if their accumulators ever disagree with the ruling.
- Nested regions (a liquid inside a tick region) — revived by a preset that needs both.
- Adaptive rate under load — revived by C1 choosing degrade.
