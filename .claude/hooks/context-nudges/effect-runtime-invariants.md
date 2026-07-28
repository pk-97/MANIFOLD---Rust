
# Effect/generator invariants

- Read the actual shader and uniform code — never write from descriptions or memory. Applies to new effects, bug fixes, any shader work.
- Constants, texture formats, math ops, param indices, and defaults must be exact — visual correctness depends on precise values, no approximation.
- Visibility/blend modes must NEVER affect simulation, generator, or effect-chain state — blend-skip elision only (hysteresis fudge rejected).
- Effect chain state lives ONLY in the ChainGraph path (legacy EffectRegistry state storage is deleted) — `clear_all_effect_state` walks every chain's chain_graph; one cache to reset, the dual-cache bug class is structurally impossible.
- Pools holding cached per-entity state (chains, primitive caches, mip pyramids) are keyed by semantic identity (LayerId/ClipId/EffectGroupId), never by iteration counter or layer_index — position keys cause silent re-binding when the active set shifts.
- When two generators share a scatter/sample/wrap shader, don't change boundary behavior for one without checking the other — fork the shader instead.
- Removing a field from a data struct compiles cleanly but silently breaks sort keys, AHashMap keys, and identity checks that used it.
- Service logic belongs in dedicated modules, not scattered inline across event handlers or app.rs.
---
name: Random envelope design lessons
description: How the Random Walk/Jump envelope mode works — trigger mechanism, sample-and-hold, step sizing, and what NOT to do
type: feedback
---

Random envelope mode is a **sample & hold** modulator, NOT an ADSR variant.

**Trigger mechanism:** Uses elapsed-decrease detection (`elapsed_f < last_elapsed`), which fires on:
1. Clip becoming active after inactivity
2. Sequential clips (each new clip resets elapsed to 0)
3. Loop restarts (elapsed wraps)
This matches the same events that naturally restart an ADSR envelope.

**Why:** Simple `clip_active && !was_active` rising edge detection MISSES sequential clips and loop restarts because the layer stays "active" between them. The elapsed-based trigger is essential.

**How to apply:**
- NEVER remove the loop/elapsed detection — it's what makes sequential clips work
- The `last_elapsed` field on ParamEnvelope tracks this (runtime-only, not serialized)
- Step size is 15% of normalized range (0.15), NOT target_normalized (which defaults to 1.0 and traps at boundaries)
- The walk value is written to `param_values[idx]` every frame (sample & hold), not just on trigger
- First activation uses a forced random jump (walk_value sentinel = -1.0)

**What went wrong during development:**
1. Initially bypassed ADSR entirely → user said "it should override the slider's value" → sample & hold
2. Bounce math was symmetric (step=1.0 from center always returns to center) → use clamp
3. Rising edge detection didn't fire after mode switch (was_clip_active stayed true) → reset on toggle
4. Per-frame `[ENV]` debug logs drowned out click/trigger logs → only use targeted, infrequent logs
5. Tried to reuse ADSR evaluation → wrong, it's a completely separate modulation path
