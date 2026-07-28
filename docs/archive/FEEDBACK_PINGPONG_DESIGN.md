# Feedback ping-pong: zero-copy state for `node.feedback` / `node.array_feedback`

Status: **SHIPPED** — landed in `22e02f6e` (zero-copy persistent-slot ping-pong + direct bridge; kill switch `MANIFOLD_FEEDBACK_PINGPONG`, default on). **`docs/FREEZE_COMPILER_MAP.md` is the authoritative current state.** Header corrected 2026-07-16 — it still read "design for review" a month after landing. Author pass 2026-06-09.

## The problem

Both delay primitives implement a one-frame delay by owning **one** persistent
resource and doing **two full copies every frame**:

- `node.array_feedback` ([array_feedback.rs:211](../crates/manifold-renderer/src/node_graph/primitives/array_feedback.rs#L211), [:245](../crates/manifold-renderer/src/node_graph/primitives/array_feedback.rs#L245)):
  `copy_buffer_to_buffer(prev -> out)` in `run`, `copy_buffer_to_buffer(in -> prev)` in `late_capture`.
- `node.feedback` ([temporal.rs:219](../crates/manifold-renderer/src/node_graph/primitives/temporal.rs#L219), [:258](../crates/manifold-renderer/src/node_graph/primitives/temporal.rs#L258)):
  the texture twin, `copy_texture_to_texture` both directions.

The cost scales with the payload. A 1080p texture is ~16 MB, so the texture
twin's two copies are ~0.15 ms — invisible. The FluidSim particle buffer is up
to ~512 MB (8M particles x 64 B), so `array_feedback`'s two copies **measured
6.37 ms — 54% of the FluidSimulation frame** (per-dispatch profile, freeze-profile
`perdispatch`). Same code shape, ~30x the data.

This is the only millisecond-scale instance, but the pattern is shared, so the
fix should be one capability both primitives use.

## Root cause

The node owns `prev` (a `GpuBuffer`/`GpuTexture` in its `StateStore` entry). The
graph hands it `in` and `out` as **pooled slot resources** the executor manages
and recycles. A one-frame delay needs the old and new state in separate memory,
and with a single owned buffer plus pooled in/out the only bridge is to copy:
`prev -> out` (give downstream a readable snapshot) and `in -> prev` (save this
frame for next). The copies exist to bridge **node-owned persistent** <->
**executor-pooled transient**, nothing more.

## What already exists (build on this, don't invent)

The backend already exposes the binding primitives a zero-copy version needs:

- `pre_bind_array(id, buffer)` ([metal_backend.rs:372](../crates/manifold-renderer/src/node_graph/metal_backend.rs#L372)) — pin an Array `ResourceId` to a host/node-owned `GpuBuffer`; `release` is a no-op for it (persistent).
- `alias_array_resource(dst, src_slot)` ([:392](../crates/manifold-renderer/src/node_graph/metal_backend.rs#L392)) — make two Array resources resolve to the same physical buffer.
- `replace_texture_2d(slot, texture)` ([:277](../crates/manifold-renderer/src/node_graph/metal_backend.rs#L277)) / `borrowed_2d` — install a borrowed texture at a slot for a frame (used today to feed Source frames and the skip-passthrough alias).
- `bind_resource_to_slot(id, slot)` ([:327](../crates/manifold-renderer/src/node_graph/metal_backend.rs#L327)).
- `aliased_array_io()` ([effect_node.rs:483](../crates/manifold-renderer/src/node_graph/effect_node.rs#L483)) — the **in-place** contract: a node declares an (in, out) Array pair that resolve to **one** buffer it mutates in place. Used by every particle-sim atom (`euler_step_particles`, `wrap_particles_torus`, `anti_clump_particles`, the force atoms, ...).
- `persistent_resources` + `state_capture_input_ports` + `late_capture` — the cross-frame back-edge machinery feedback already rides on.

The gap: there is no mechanism for a **stateful** node to bind a resource it
owns into its `out` slot (and have the back-edge write land in node-owned
memory) and rotate that binding per frame. `aliased_array_io` is *in-place*
(out == in, one buffer), not a ping-pong; the texture `borrowed_2d` alias path
only supports **unchanged pass-through**, not a writing delay node.

## The capability

> Let a stateful node own its persistent state resource(s) and have the executor
> **bind them directly into the node's graph slots** — eliminating the
> pooled<->persistent copy bridge.

Concretely, a new `EffectNode` hook (working name `persistent_state_io`) lets a
node declare, per (output_port, capture_port) pair, that it owns a set of `K`
persistent buffers/textures and wants the executor to:

1. bind `buffer[role]` into the `out` resource (downstream reads last frame), and
2. arrange for the back-edge producer's write to land in `buffer[role ^ 1]`
   (this frame's new state), then
3. advance `role` at end of frame.

The node still **allocates** the buffer(s) exactly as it sizes `prev` today; it
just registers them with the backend (via `pre_bind_array` / `replace_texture_2d`)
instead of copying through them. The executor owns the rotation and slot binding.

### Two instantiations

**A. Single persistent buffer, fully in-place (`K = 1`, no swap).** When the
feedback's loop is a chain of `aliased_array_io` in-place mutations, the new
state is computed *in the same buffer* as the old, so there is no second buffer
and no swap — the node just owns `P`, binds it into `out`, and `late_capture`
becomes a no-op (the in-place chain already left the result in `P`, which
persists). **This is the FluidSim case** — and the biggest, simplest win.

**B. True A/B double buffer (`K = 2`, swap each frame).** When the loop writes
the new state into a *distinct* target while readers need a stable snapshot of
the previous frame (trail effects, reaction-diffusion on a grid — the texture
`node.feedback` case): own `A` and `B`, bind `A` into `out`, bind `B` as the
back-edge write target, swap. Zero copies, two buffers.

## Step 0 before any code: trace the FluidSim buffer topology

The FluidSimulation loop ([FluidSimulation.json]): `particles_feedback.out`
**fans out** to both `Move Particles.particles` (the in-place sim chain) and
`Render Density.particles`; `Move Particles.particlesOut` feeds `.in`. Because
the sim chain is fully `aliased_array_io`, `out`, the whole chain, and `in` may
already alias to **one** pre-bound buffer (the chain builder pre-binds the
particle buffer at `item_size x max_capacity` via `pre_bind_array`, which is
*persistent*). If so, `array_feedback`'s `prev` is a **second** buffer the copies
bridge to — and the fix is instantiation A (collapse to the one persistent
buffer, drop both copies). It is even possible `array_feedback` is partly
redundant given that pre-bound persistence.

**This must be traced first** — it decides A vs B vs "partly redundant":
- Is `feedback.out`'s resource the same pre-bound buffer as `feedback.in`'s, or two?
- Does the working buffer already persist across frames (pinned), or get pool-recycled?
- Does any reader of `out` (e.g. `Render Density`) require the *pre*-mutation snapshot, or does it run after `Move Particles` in topo order and already see the mutated state? (The current copy version's behavior here is the spec to preserve.)

Trace via the compiled plan's `ResourceId` -> slot bindings for the loop nodes,
plus the chain builder's `pre_bind_array` / `alias_array_resource` calls.

## Executor mechanism

- New hook on `EffectNode`: `persistent_state_io(&self) -> &[PersistentIo]`,
  where `PersistentIo { out_port, capture_port, kind: Array|Texture2D, buffers: K }`.
  Default empty (every existing node unaffected).
- At chain build / first frame: the node allocates its `K` resources; the
  executor `pre_bind`s `buffer[0]` to the `out` resource and (for `K=2`)
  `buffer[1]` to the capture/back-edge resource.
- Per frame: the executor advances `role`, re-pointing the two bindings (a
  pointer swap in the `bound`/`borrowed_2d` maps — no GPU work). For `K=1`
  (in-place) there is no swap; the binding is stable and `late_capture` is skipped.
- The node's `run`/`late_capture` lose their copies; `array_feedback`/`feedback`
  become thin: own the buffers, declare the hook, no per-frame `copy_*`.

## Correctness

- **Fan-out / lifetime.** `out` aliased to `A` may be read by several downstream
  nodes; they all read `A` (the previous frame) — correct. The pinned/persistent
  binding already keeps `A` alive across frames; this is the same lifetime the
  `last_reader` extension protects for the skip-passthrough alias
  ([execution_plan.rs:560](../crates/manifold-renderer/src/node_graph/execution_plan.rs#L560)). Verify no pool path recycles a role buffer mid-frame.
- **Seed / reset.** First-frame seed and `reset_trigger` edges still copy
  (`seed -> buffer`) — rare, off the steady-state path, keep as-is.
- **Ordering.** `late_capture` runs post-frame; for `K=2` the swap happens there
  (or at frame end) so next frame's `run` emits the rotated buffer. For `K=1`
  it's a no-op. Preserve the existing `state_capture_input_ports` cycle-break.
- **The aliased-output assert.** The `aliased_array_io` debug assert
  ([execution.rs:657](../crates/manifold-renderer/src/node_graph/execution.rs#L657)) expects a GPU dispatch when aliased IO is declared; a copy-free feedback declares the new hook, not `aliased_array_io`, so it must be exempt (it legitimately does no dispatch).

## Texture case (`node.feedback`)

Instantiation B, and it needs the **writing-texture-alias** extension: today
`borrowed_2d`/`replace_texture_2d` installs a borrowed texture at a slot, but the
only writing path that aliases is in-place skip-passthrough. A delay node writing
into `B` while `out` reads `A` needs the executor to bind two node-owned textures
into the out/capture slots and swap — the texture analog of `pre_bind_array` +
rotation. Lower priority: the texture payload is ~16 MB, so the win is ~0.15 ms.
Worth doing for symmetry once the Array path lands, not before.

## Risks & validation

- Feedback/state-capture is **show-critical** and the ordering is subtle
  (per-port cycle-break, late-capture timing). Treat like a broken instrument:
  keep every existing feedback parity/state test green, add a ping-pong
  equivalence test (fused-feedback output == copy-feedback output for N frames).
- Validation: `freeze-profile perdispatch` must show `node.array_feedback` drop
  from ~6.4 ms to ~0 on FluidSimulation, with identical rendered output.
- The oversized 8M pool (flat-vs-capacity sweep) is a **separate** finding — even
  after zero-copy, the per-particle dispatches still run at pool capacity. Right-
  sizing the pool stacks on top of this.

## Sequencing

1. Trace the FluidSim buffer topology (Step 0) — decides A vs B vs redundant.
2. Implement instantiation A for `node.array_feedback` (the measured 6.4 ms win).
3. Equivalence test + freeze-profile confirmation.
4. (Later, low priority) instantiation B + writing-texture-alias for `node.feedback`.
5. Overlays (`render_value_overlay`, `render_filled_rects`) — likely **not**
   fixable this way (writing into a pooled upstream texture with fan-out
   consumers corrupts them); leave as-is.
