# Buffer-chain fusion: collapse FluidSim's per-particle dispatch chain

Status: **SHIPPED / SUPERSEDED** — derived-uniform fusion landed and this doc's blocker table is resolved; FluidSim buffer regions fuse today (proof: `fluidsim_buffer_fusion_renders_like_unfused`). **`docs/FREEZE_COMPILER_MAP.md` is the authoritative current state** (it lists this doc as pre-derived-uniform-fusion history). Header corrected 2026-07-16. Scoped 2026-06-09 after the
`array_feedback` zero-copy fix took FluidSimulation 11.8 → 5.5 ms.

## The target, measured

Post-`array_feedback`, FluidSimulation @ 1080p is **5.5 ms**, and the
per-dispatch profile (`freeze-profile perdispatch`) shows **~87% is per-particle
atoms**: seed 0.85, euler 0.72, radial_burst 0.68, wrap 0.67, anti_clump 0.66,
scatter 0.51, noise_force 0.42, sample_at_particles 0.37 ms. Texture work
(blurs/tonemap/resolve/downsample) is only ~0.7 ms.

Critically, the pool-capacity sweep is **flat 1M → 8M particles** (slope ≈ 0).
The atoms already dispatch at the live `active_count` slider, not `max_capacity`
(the pool ceiling) — confirmed in [euler_step_particles.rs:169]. So the cost is
**not** particle compute; it's **fixed per-dispatch overhead + serialization**:
~8 dependent dispatches over the shared particle buffer, each Metal-hazard-
barriered against the last, each paying launch + uniform-setup latency. The
synthetic buffer bench (`profile_synthetic_buffer`) proves the fix pays linearly:
N separate in-place dispatches vs 1 fused N-op kernel → Nx speedup.

## Why the chain doesn't fuse today

Buffer fusion is LIVE (ships for DigitalPlants — `digitalplants_buffer_fusion_
renders_like_unfused` is bit-exact). But every atom in FluidSim's hot chain is
individually excluded by [`classify_buffer_node`](../crates/manifold-renderer/src/node_graph/freeze/region.rs):

| atom | exclusion |
|---|---|
| `euler_step_particles` | **derived uniforms** (`dt_scaled`) — region.rs:446 |
| `simplex_noise_force_*`, `apply_radial_burst_*` | derived uniforms / `wgsl_includes` |
| `sample_texture_at_particles` | texture I/O — region.rs:433 |
| `anti_clump_particles` | `BufferGather` (neighbour reads) — region.rs:440 |
| `scatter_particles` | atomic accumulator output — region.rs:452 |

The dominant, highest-leverage blocker is **derived uniforms**: the fused buffer
codegen calls each member's `wgsl_body` but never runs the per-frame CPU
computation the standalone atom's `run()` does (it has "no per-member `run()`"),
so any atom that derives a uniform from frame state (dt, frame_count) is a hard
boundary. That excludes the integrator AND the forces — the bulk of the chain.

## The build

**Phase 1 — derived uniforms in fused buffer regions (the unblock).** Teach the
fused buffer codegen to, for each member with `derived_uniforms()`, compute those
values CPU-side at dispatch time (the same arithmetic the standalone `run()`
uses) and pass them into the fused kernel as a per-member uniform slice. This
unblocks `euler_step` + the force atoms. Expected fusable consecutive run:
`noise_force → euler → wrap → radial_burst` (the chain's tail, steps 54–57) once
they stop being derived-uniform boundaries — modulo the texture/gather cuts.

**Phase 2 — cut cleanly around the irreducible boundaries.** `sample_at_particles`
(texture-gather), `anti_clump` (buffer-gather), and `scatter` (atomic) stay
boundaries in v1 — that's correct, they genuinely read outside their own element
or write atomically. The region grower already cuts at them; the win is fusing
the *runs between* the cuts, not across them. Even fusing the 3–4 consecutive
derived-uniform atoms collapses ~3 dispatches → 1.

**Estimated payoff:** the fusable per-particle sub-chain is ~2 ms of the 4.8 ms
per-particle cost; collapsing it toward one dispatch targets ~1.4 ms off the
frame (5.5 → ~4 ms), with more if the grower can reorder/group the chain so more
atoms land in one region. Measure, don't assume — the synthetic bench says the
overhead is real, but the exact region boundaries set the ceiling.

## Validation (non-negotiable — this is show-critical codegen)

- The freeze **diff oracle** (`freeze::diff`) must show the fused FluidSim
  renders bit-exact (or within the established tolerance) vs unfused, every step.
- `cargo test -p manifold-renderer --lib bundled_presets` (executes a real GPU
  frame for FluidSim/FluidSim3D/ParticleText) must stay green.
- `freeze-profile perdispatch` must show the fused atoms collapse and the frame
  drop, with identical output.
- The **perf gate** already vetoes a fusion that doesn't pay on-device, so a
  bad-margin region falls back to unfused automatically — but correctness is on
  us, not the gate.

## Risks & sequencing

- Derived-uniform codegen is intricate (per-member uniform layout, the CPU-side
  recompute must match the atom's `run()` exactly). Do it incrementally:
  one atom (euler) fused against its standalone through the oracle first, then
  the forces, then the consecutive run.
- This generalizes to **every** particle/instance generator (FluidSim3D,
  ParticleText, DigitalPlants's sim atoms) — not a FluidSim point-fix.
- Stop-stack: the OilyFluid texture/gather case (stencil fusion) is the OTHER
  big compiler and is independent of this; sequence after, it's harder.

## Open question to resolve first

Confirm by running the classifier on the live FluidSim def which atoms are
`Eligible` vs `Boundary` and why (instrument `classify_buffer_node`), so Phase 1
targets the real boundary set rather than this static read of the gates.
