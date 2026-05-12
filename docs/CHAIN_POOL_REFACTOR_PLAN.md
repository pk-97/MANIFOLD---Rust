# Chain & Buffer Pool Refactor — Rekey by Semantic ID

**Status**: planned, not started. Audit complete, design agreed with user.
**Goal**: Make positional-indexing bug class **provably impossible** at the type level.
**Branch**: `node-graph-system`. Last committed: `5b77de38` (master_effect_chain fix).

## Background — what triggered this

The chain-graph migration introduced a class of bug where `Vec<EffectChain>` was indexed by an iteration counter that didn't actually carry semantic identity. Two separate manifestations hit production:

1. **`07bfa5d4`** — Layer chain pool indexed by `effect_chain_idx++` (iteration order). When the active-clip set shifted between frames, `effect_chains[0]` got fed layer A's effects one frame and layer B's the next → `ChainGraph` rebuilt every frame → every stateful primitive's first-evaluate allocation path ran every frame (~10ms of hidden GPU work). User saw "30 FPS at 4K, should be 100+". Diagnosed via `MANIFOLD_LOG_CHAIN_STATS` showing `rebuilds=120/sec`. Fixed by indexing by `layer_idx as usize`.

2. **`5b77de38`** — Master FX pass was pulling its chain via `self.effect_chains[0]`, which under fix #1 is layer 0's slot. When layer 0 had effects, both paths thrashed the same chain. User saw "state contamination between layers". Fixed by adding a dedicated `master_effect_chain: EffectChain` field.

Both bugs were the same root cause: **positional indexing where the index doesn't structurally encode the semantic identity it represents.**

Current state: rebuilds drop to 0–2/sec (only legitimate topology changes from clip activations). FPS is much closer to native.

## The user's ask

> "Once you look, we should do a more targeted audit and review for these types of structural bugs and fix them. It seems like we're using index positions that lead to really difficult to track and trace bugs. I need this developed in a way that bugs like this are 'provably impossible'."

User picked:
- **Refactor scope**: Full audit + rekey everything (all chain + buffer pools).
- **Regression guard**: Yes — tests that include layer-reorder scenarios assert each chain's `ChainGraph` survives.

## Why the current fix isn't sufficient

`effect_chains[layer_idx as usize]` is still positional. `layer_idx` is the layer's *position in the timeline*. If the user drags a layer up/down (reorder), every layer below it shifts position → every chain below it is now bound to a different layer's state → same bug class, just rarer.

## Audit inventory — all positional collections in `layer_compositor.rs`

| Field | Type | Indexed by | State-bearing? | Risk |
|---|---|---|---|---|
| `effect_chains` | `Vec<EffectChain>` | `layer_idx as usize` (my current fix) | YES (`ChainGraph` cache) | **HIGH — layer reorder shuffles state** |
| `master_effect_chain` | `EffectChain` | (singular field, fixed in 5b77de38) | YES | OK |
| `group_effect_chains` | `Vec<EffectChain>` | `gb_idx++` (iteration counter) | YES | **HIGH — same bug as #1, not yet user-visible because user doesn't use groups** |
| `led_group_effect_chains` | `Vec<EffectChain>` | `led_group_idx++` (iteration counter) | YES | **HIGH — same bug class** |
| `layer_bufs` | `Vec<PingPong>` | `layer_buf_idx++` | NO (cleared each frame) | LOW (transient frame of garbage at worst) |
| `group_bufs` | `Vec<PingPong>` | `gb_idx++` | NO | LOW |
| `led_group_bufs` | `Vec<PingPong>` | `led_group_idx++` | NO | LOW |

108 total touch-points across the file.

## Target design

```rust
pub struct LayerCompositor {
    // BEFORE: Vec<EffectChain>  — positional, confusable
    // AFTER:
    effect_chains: AHashMap<LayerId, EffectChain>,
    master_effect_chain: EffectChain,               // unchanged (already singular)
    group_effect_chains: AHashMap<LayerId, EffectChain>,  // keyed by group container's LayerId
    led_group_effect_chains: AHashMap<LayerId, EffectChain>,

    layer_bufs: AHashMap<LayerId, PingPong>,
    group_bufs: AHashMap<LayerId, PingPong>,
    led_group_bufs: AHashMap<LayerId, PingPong>,
    // ...
}
```

`LayerId` is `Arc<str>` (from `manifold-core::id`), already type-distinct from `EffectGroupId`, `EffectId`, `usize`. Cloning is one atomic retain — cheap. Same key type works for both layer chains and group chains because:
- Layer chains key by the layer's `LayerId`.
- Group chains key by the **group container layer**'s `LayerId` (groups are layers in MANIFOLD).
- The maps are different fields, so the compiler keeps them straight.

## Properties this gives us

1. **Iteration order can't index a chain anymore.** The map key is `LayerId`, not `usize`. Writing `chain_map[counter]` doesn't compile (`Index<usize>` not implemented).
2. **Layer reorder doesn't shuffle chains.** `LayerId` is stable for the layer's entire lifetime, independent of timeline position.
3. **Master chain physically can't collide with layer chains.** Different field, different type (`EffectChain` vs `AHashMap<LayerId, EffectChain>`).
4. **Layer chains and group chains can't be confused.** Different fields, even though both keyed by `LayerId`.
5. **Adding a new pool is explicit.** A new chain category needs a new field with a clear name.

Performance cost: one `ahash` lookup per chain access per frame (~50ns). Already negligible vs the chain's own work.

## Why the raw-pointer dance has to be reconsidered

Current code:
```rust
let effect_chains_ptr = self.effect_chains.as_mut_ptr();
let layer_bufs_ptr = self.layer_bufs.as_mut_ptr();
while i < clips.len() {
    // ...
    let effect_chain = unsafe { &mut *effect_chains_ptr.add(ec_idx) };
    let layer_buf = unsafe { &mut *layer_bufs_ptr.add(lb_idx) };
    // Plus self.blend, self.uniform_arena, self.effect_registry borrows.
}
```

The Vec raw pointer is safe IF no insertions happen during the loop (no reallocation → stable addresses). The original author used this to hold multiple `&mut` to different fields without fighting the borrow checker.

For HashMap:
- HashMap entries' addresses are stable BETWEEN insertions, but `entry().or_insert_with()` may rehash and invalidate.
- We need to **pre-insert all needed entries before the loop** (sized by pre-scanning `active_layer_ids`), then use safe `get_mut(&id)` inside the loop.
- Rust's split-borrow rules will handle the multi-field disjoint access, IF the code stops calling `self.method(&mut self, …)` patterns mid-loop. Most current code already uses `Self::apply_effects(specific_args, …)` so this should be fine.

Plan: pre-scan to find active layer IDs, pre-insert entries via `.entry().or_insert_with()` BEFORE the loop body, then use safe `get_mut` throughout. If the borrow checker fights, fall back to taking raw pointers AFTER pre-insertion (when addresses are stable).

## Refactor plan — staged commits

Each stage: build → clippy → `cargo test -p manifold-renderer` → commit. If a stage fails, the previous stage's commit is the safe rollback point.

### Stage 1 — Convert `effect_chains` to HashMap
- Change field type and constructor.
- Replace `ensure_effect_chains(count)` with pre-scan + `entry()` pre-insertion.
- Update `generate_layers` loop to look up by `LayerId`.
- Update `composite_parallel` loop same way.
- Update `trim_excess_buffers` to drop entries for inactive layers (with HEADROOM/grace period).
- Update `resize` to iterate `.values_mut()`.
- Audit for any remaining `effect_chains[idx]` patterns.

### Stage 2 — Convert `group_effect_chains` similarly
- Same shape. Key by group container's `LayerId` (the `group_desc.layer_id`).
- Touch sites: `composite_serial` group fold, anywhere else using `gb_idx`.

### Stage 3 — Convert `led_group_effect_chains`
- Same shape. Key by group container's `LayerId`.
- Touch sites: LED group fold loop.

### Stage 4 — Convert `layer_bufs`, `group_bufs`, `led_group_bufs` (PingPong pools)
- Stateless across frames so risk is low, but doing them for consistency and to remove the iteration-counter pattern entirely.
- These need to handle resolution changes (existing code resizes when `width != ld.width`).

### Stage 5 — Regression tests
The user explicitly wants tests that prove chain state survives layer reorder.

Add to `crates/manifold-renderer/tests/` or `layer_compositor.rs::tests`:

1. **`chain_pool_stable_across_clip_changes`**:
   - Build a `LayerCompositor` with 3 layers.
   - Run frames with clip-set `[A, B]`, then `[B, C]`, then `[A, B, C]`.
   - Assert `take_chain_dispatch_stats().rebuilds == 0` after warmup.

2. **`chain_state_survives_layer_reorder`**:
   - Build compositor, run a frame with layer "X" at position 0 with a Bloom effect.
   - Verify `effect_chains[&LayerId("X")].chain_graph.is_some()`.
   - Reorder layers (put "X" at position 5).
   - Run another frame.
   - Verify `effect_chains[&LayerId("X")].chain_graph.is_some()` AND it's the **same instance** (state preserved).

3. **`master_chain_independent_of_layer_chains`**:
   - Build compositor with layer at LayerId("any") with effects AND master FX.
   - Run a frame.
   - Assert `master_effect_chain.chain_graph` and `effect_chains[&LayerId("any")].chain_graph` are **separate** instances.

May need a test fixture that constructs `LayerCompositor` with a real `GpuDevice` (already exists pattern in other renderer tests — see `gpu_tests` mod in `primitives/wet_dry_mix.rs`).

## Risks & mitigations

- **Rendering pipeline regression**: stage-and-commit each conversion separately; full workspace `cargo test` between stages; clippy clean.
- **Borrow checker fights**: pre-insert entries before loop, use safe `get_mut`. Fall back to raw pointers POST-INSERTION if needed.
- **Test infrastructure**: real-GPU tests need `GpuDevice::new()`. Existing primitive tests show the pattern.
- **Performance**: one hash lookup per chain. Already negligible (~50ns).

## Where to resume after compaction

1. Re-read this doc.
2. Pick up at Stage 1.
3. The first change is `effect_chains: Vec<EffectChain>` → `effect_chains: AHashMap<LayerId, EffectChain>` in `layer_compositor.rs` line ~350.
4. Use `cargo test --release -p manifold-renderer` to verify no regressions between stages.
5. User's diagnostic command for live verification: `MANIFOLD_LOG_CHAIN_STATS=1 cargo run --release 2> chain-stats.log` — should still show `rebuilds=0` after refactor.

## Out of scope (deferred)

- Generator chain pools (separate system, audit later if same pattern exists).
- Effect registry processor instances (legacy path, lower priority).
- Texture pool itself (works correctly — only issue was caller misuse, now fixed by `borrowed_2d`).

## Reference — recent commits

- `cbbaa7f8` — env-gated chain-stats logger
- `d64e0e8b` — env-gated rebuild-reason diagnostic
- `07bfa5d4` — first fix: layer_idx-indexed chains (REPLACED by this refactor when stage 1 lands)
- `1275cd3a` — gitignore runtime logs
- `5b77de38` — master_effect_chain separation
