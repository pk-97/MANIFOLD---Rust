# F-P1 Escalation — cache-invalidation mechanism the doc names doesn't exist

**Status: phase proceeded past this — everything NOT blocked by it is
implemented and gated below. This file documents the one real judgment
call the doc's D2 rebuild rule needed and couldn't resolve itself.**

## What the doc expects

D2's rebuild rule: "the chain re-convolves when the wired envmap's
`DataVersion` changes (house dirty-check pattern)". The phase brief's Gate
(positive) list includes: "cache — re-convolve only on version change
(dispatch-count assert)". §4's Invariants table has: "Prefilter cache
invalidates on envmap change | gpu-proof: re-bake with different params →
readback changes; same params → cached (no re-convolve, asserted via
dispatch counter)".

## What I found

1. **No `DataVersion` type exists anywhere in the workspace.** `rg -i
   dataversion` across every crate returns exactly two hits, both prose
   comments in `manifold-playback/src/automation.rs`, unrelated to
   textures. CLAUDE.md's "dirty-checking via DataVersion" is describing a
   general technique this codebase uses (the repeated `ensure_*`
   cached-field-compare-and-rebuild idiom already in `render_scene.rs` six
   times over — `ensure_msaa_targets`, `ensure_shadow_map`, etc.), not a
   literal type any primitive can read off its GPU inputs. `EffectNodeContext`
   (`effect_node.rs`) carries no per-input "did this change since last
   frame" signal at all.

2. **`node.bake_environment`'s `run()` mutates the SAME persistent output
   texture in place every frame, unconditionally** — I read it end to end
   (`bake_equirect_envmap.rs`): no internal caching, no early-out on
   unchanged params, it re-dispatches its compute shader every single
   `evaluate()` regardless of whether `width`/`height`/`intensity`/etc.
   actually changed. This means the GPU texture OBJECT bound to
   `render_scene`'s `envmap` input is pointer-stable across frames even
   when its CONTENT is being animated frame-to-frame (exactly D7's
   sun-coherence gesture: the sun direction rides a macro, the envmap
   re-bakes every frame it moves, same texture object throughout).

## Why this blocks a literal "skip re-convolve when unchanged" cache

The only identity signal available to a primitive reading its own inputs
is the GPU texture object's pointer (I added `GpuTexture::ptr_eq` /
`identity_key` to make this concrete) plus its width/height — the same
"cheap cached key, rebuild if different" idiom `ensure_shadow_map` already
uses. But per finding #2, that identity is stable even when content is
animating. A pointer/size-keyed skip would treat every animated envmap —
including the D7 sun-sweep gesture this whole design exists to keep
coherent — as "unchanged" and silently go stale. That is a correctness
regression on the exact showcase gesture, not a missed optimization.

## What I did instead (and why it's not silently choosing for you)

- **BRDF LUT** genuinely IS envmap-independent (view/roughness only), so
  "computed once per device, never rebuilt" is unambiguous and correct —
  implemented as a real cache (`brdf_lut_built: bool`, gated dispatch).
  This is the one resource where the doc's "dispatch-count" framing is
  literally true and I built it that way.
- **Prefiltered specular chain + diffuse irradiance map** (the two
  envmap-DEPENDENT resources) re-convolve every `evaluate()` call where
  `envmap` is wired — no skip, no identity check. This matches D2's OWN
  stated consequence verbatim ("an animated envmap ... re-prefilters every
  frame — a fixed, small cost ... not a correctness hazard") rather than
  the Invariants-table row's "same params → cached" framing for these two
  resources specifically. Where the two disagree, I followed the
  consequence prose (correctness-preserving) over the invariant-table
  wording (an optimization I can't safely build without the missing
  signal).

## The smallest question that unblocks a real fix

Is a genuine per-primitive "this input's producer actually changed its
output since the last frame I ran" signal worth adding to
`EffectNodeContext`/the executor (e.g. a monotonic generation counter
bumped by the executor whenever a node's own params or upstream wiring
changed this step)? If yes, that's infrastructure bigger than this
primitive and belongs in its own design/phase, not smuggled into F-P1. If
no (the fixed per-frame cost is accepted permanently, not just for now),
the Invariants-table row for these two resources should be corrected to
match D2's consequence prose so a future phase doesn't reopen this as a
bug.

## What is NOT escalated, for clarity

Everything else in F-P1 proceeded without a fork: the binding-table
anchor (`render_scene.wgsl:648-651` for the old IBL sample, bindings
0-15 all occupied) still held exactly as the audit table said; `pbr_brdf.wgsl`
was unconsumed anywhere in the tree (I am its first consumer, per its own
header's stated intent); the mip-level-view capability needed for the
prefiltered chain didn't exist in `manifold-gpu` and I added it
(`GpuTexture::mip_level_view`) as a small, scoped, well-precedented
addition (mirrors `GpuBuffer::ptr_eq`'s existing "expose the cheap Metal
op" pattern) — not itself a judgment call the doc left open, just missing
plumbing a phase implementing D2 has to build.
