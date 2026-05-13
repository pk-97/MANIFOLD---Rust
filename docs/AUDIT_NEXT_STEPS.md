# Graph-Runtime Integration Audit — Bug Families & Candidate Targets

**Status**: handoff before compaction. Next session: discuss → audit.

This session (May 12-13, 2026) found a recurring family of bugs at the boundary between the legacy effect dispatcher and the chain-graph fast path. Each fix tightened one specific contract; the meta-pattern is that **the graph-runtime was layered on top of legacy code, inheriting the API surface but not all the implicit semantics**.

---

## The 7 bug patterns we identified

### Pattern 1: Hardcoded sentinel values left over from refactor
A value that's "real" upstream gets defaulted to `0` (or similar) at a runtime boundary. Compiles clean, semantically broken when anything depends on the real value.

**Fixed this session**:
- `frame_count: 0` in `legacy_adapter.rs` (commit `5886315c`)
- `frame_count: 0` in `build_effect_context` / `auto_gain.rs` (commit `900c28bc`)
- (Pre-session) `owner_key: 0` in `execute_frame_with_gpu`

### Pattern 2: Parallel state caches drifting on reset
Two storage locations for the same concept; reset paths only update one. Symptom: stale content bleeds across resets.

**Fixed this session**:
- `clear_all_effect_state` cleared legacy `EffectRegistry` but not `ChainGraph` primitive instances (commit `1e55ca5c`)

### Pattern 3: Missing lifecycle trigger for state reset
API exists to reset state, nothing calls it at the right moment.

**Fixed this session**:
- No "layer goes idle" trigger fired `clear_state` on its chain (commit `e31ebdc9`)

### Pattern 4: Latent landmines from incomplete cross-system integration
Code that compiles but panics when first exercised. Often new mechanism wired into one path but not another.

**Fixed this session**:
- `temporal::Feedback` panicked under `execute_frame_with_gpu` (StateStore=None). Wired `ChainGraph::run` to use `execute_frame_with_state` (commit `47f08354`)

### Pattern 5: Asymmetric semantics between two dispatchers
Same source-level API behaves differently in legacy vs graph runtime because of underlying mechanics differences.

**Fixed this session**:
- `should_skip()` worked in legacy (skip = don't swap ping/pong) but broke in graph (skip = leave assigned output slot stale). Fixed first with blit (`0a37e06a`), then properly with slot aliasing (`a0a3a417`).

### Pattern 6: Interaction effects unmasked by upstream fix
Bug A was being accidentally mitigated by bug B. Fixing B exposes A.

**Pre-session chain-pool refactor unmasked**: missing clear-on-load (Pattern 2), missing clear-on-idle (Pattern 3), skip-without-write (Pattern 5), `frame_count: 0` throttle issues (Pattern 1). All of these were latent before but masked by frequent chain rebuilds.

### Pattern 7: Architectural mismatch as performance regression
The *correct* fix is expensive because the new architecture lacks an old optimization.

**Fixed this session**:
- Skip-passthrough required a 600μs blit (`0a37e06a`). The graph runtime's per-effect-slot model didn't have a "shadow this slot's read" mechanism. Added one (`a0a3a417`).

---

## Candidate audit targets — where these patterns might hide elsewhere

### Pattern 1 audit targets (hardcoded sentinels)

- **Other defaults in adapter glue**: `is_clip_level: false`, `edge_stretch_width: 0.0`, `LED_MASTER_OWNER_KEY: i64::MIN+1`. Are these correct in every code path that builds an `EffectContext`?
- **Generator dispatch**: does the generator runtime have analogous `frame_count: 0` or `owner_key: 0` glue?
- **Audio analyzer plugin context**: if it threads a context, what defaults?
- **MIDI/OSC parameter routing**: any "placeholder for now" values at the bridge?

### Pattern 2 audit targets (parallel state caches)

- **Generator state**: is there a generator equivalent of "registry + primitive instance" dual storage? E.g. fluid-sim density grids, mycelium history, stateful_base.rs?
- **LED-specific resources**: `led_master_ec`, `led_tonemap`, `led_tap`, `led_main`, `led_black_tex` — are all these reset together on relevant events?
- **Plugin state**: native FFI plugins (DepthEstimator, BlobDetector) — do they have parallel state that needs synchronized cleanup?
- **Master FX vs LED master FX**: two separate `master_effect_chain` and `led_master_ec` fields. Reset paths cover both?

### Pattern 3 audit targets (missing lifecycle triggers)

- **Generator state on layer idle**: does it reset? Generators with persistent state (fluid sim, mycelium) — what happens when their layer goes idle?
- **Master FX state on certain events**: project load fires clear_all_effect_state which we just fixed. What about resolution change? FPS change? Tonemap curve change?
- **Plugin state on layer mute/unmute**: depth estimator etc. — do they cache results across mutes?
- **Effect groups**: when a group is disabled/re-enabled at runtime, does any effect inside it get a reset signal?

### Pattern 4 audit targets (latent landmines)

- **Other primitives requiring runtime services**: any primitive expects `state: Some(...)` other than Feedback? Future Vec2/Vec3/Color params via `ParamType::Vec*`?
- **The `compile` validation pass**: does it catch all runtime-dependency mismatches at chain-build time, or only some?
- **Effect group wet_dry < 1.0 with non-contiguous positions**: the legacy fallback handles this; chain-graph bails. Currently flagged as "rare"; verify users haven't been creating these accidentally.
- **Generator/effect crossover**: any path where a generator's output flows through an effect-like state mechanism?

### Pattern 5 audit targets (dispatcher asymmetry)

- **Effect group transitions in chain-graph**: wet_dry handling via Mix sub-graphs — does it handle edge cases identically to legacy `WetDryLerpPipeline`?
- **Encoder boundaries**: legacy effect chain ended/restarted the compute encoder freely; chain-graph keeps it alive. Are there effects that *depend* on a fresh encoder (cache invalidation, etc.)?
- **`should_skip` is one implicit contract — are there others?** What else does the legacy dispatcher do "for free" that chain-graph requires explicit handling?
- **Per-effect ping-pong vs shared chain ping-pong**: legacy effects might assume their source/target are the chain's ping/pong (with specific properties). Graph-runtime gives them dedicated slots with potentially different format/properties.
- **Wet/dry on a group containing skipping effects**: if all effects in a group skip, does the Mix sub-graph still produce sane output?

### Pattern 6 audit targets (bugs masked by other fixes)

The chain-pool refactor was a big "unmask" event. What other recent stability fixes might have unmasked latent bugs?

- **Pre-session texture pool fixes (`borrowed_2d`/`textures_2d` split)** — did stability there unmask any other latent issue?
- **The "always emit Mix sub-graph for enabled groups"** fix (in `effect_chain_graph.rs`) — did that mask any latent wet/dry handling bug?
- **VSync / frame-pacing stability** — if frame pacing was previously unstable, downstream code might depend on jitter. Now stable, those dependencies are visible.

### Pattern 7 audit targets (architectural perf gaps)

- **Other places where graph-runtime pays for what legacy got free**: per-effect pipeline binding (Metal binding cache loss on encoder boundary), uniform buffer setup, etc.
- **Pool allocation on chain rebuild**: rebuilds are now rare (post-refactor), but they still hit `RenderTarget::new_pooled`. Is the pool warm enough?
- **Slot lifetime planner overhead**: how much is the `assign_texture2d_slots` walk costing per rebuild? Worth caching?

---

## High-priority specific audits to discuss

Before doing the full sweep, these are the most likely places to find another bug of the same families:

1. **The generator runtime** — completely separate dispatch tree from effects. If it has primitive-style and legacy-style dispatchers like effects do, the same bugs probably exist there.
2. **The LED path** — `led_master_ec`, LED group chains, LED tonemap. Less audited; lifecycle is parallel-but-distinct from screen path.
3. **Master FX chain (`master_effect_chain`)** — its own dedicated `EffectChain` instance. Does it participate in all the lifecycle hooks correctly?
4. **Plugin lifecycle (`DepthEstimator`, `BlobDetector`)** — native FFI plugins with their own internal state. How does it interact with chain rebuild / project load / layer idle?

---

## Discussion questions for post-compaction

1. Which of the 7 patterns is most likely to recur in MANIFOLD's current state? (Probably patterns 2, 3, 5.)
2. Should we audit by *system* (effects, generators, LED, plugins) or by *pattern* (sweep for all hardcoded sentinels first, then all dual-cache reset paths, etc.)?
3. Are there visible symptoms in the live show that haven't been root-caused yet — those are likely candidates for the audit's first hit.
4. The plugin lifecycle (Pattern 2 + 3 candidate) — is it worth a deep dive, or are plugins stable in your shows?

---

## Recent commits in this session (for context)

```
a0a3a417 perf(node_graph): zero-cost skip-passthrough via slot aliasing
0a37e06a fix(legacy_adapter): skip-on-amount=0 must passthrough, not no-op
e31ebdc9 feat(compositor): clear chain state when layer is idle (no active clip)
47f08354 fix(chain_graph): thread StateStore + owner_key through fast path
900c28bc fix(primitives): forward frame_count in build_effect_context
5886315c fix(effects): plumb frame_count through ChainGraph fast path
1e55ca5c fix(compositor): clear chain_graph state on reset + hybrid pool eviction
```

Plus the Stage 1-5 chain-pool refactor (pre-session).

All committed and pushed to `node-graph-system`.
