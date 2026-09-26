# Fragment-cut audit — BUG-9q34

Base: 7328598c1. Diagnostic source only; no production fix or app landing.

All six stock fragment modifiers rendered on practice_head_sculpt.glb at 960x960 in unfused and fused PresetRuntime paths. New cut edges have fractional alpha; geometry/remap proofs passed 23 tests, and existing fragment_cut_scene passed its four structural/live-control tests. Both new audit tests passed. Fused/unfused alpha is byte-identical; maximum RGB channel difference is 0.001953125, mean RGBA difference below 0.000054. MaskedPeel has visible ragged/perforated feather boundaries in both paths. See BUG-9q34 for diagnosis and persistent local evidence paths.

The audit uses the actual note_modifier_clip_event route after import warmup; generic trigger_count alone does not drive these modifier events. It has a 60-frame per-case cap. MANIFOLD_CUT_AUDIT_OUT writes PNGs and raw RGBA16F. MANIFOLD_CUT_MODEL can select another model. The default head fixture is gitignored and must be available before execution.

Focused reproduction: cargo test -p manifold-renderer --features gpu-proofs --lib preset_runtime::fragment_cut_edges:: -- --nocapture --test-threads=1. Use --manifest-path for an isolated checkout. Rustfmt and diff checks passed. Full landing gate and clippy were not run; this is an archived audit harness, not a qualified application change.

Remaining: fix geometry continuity through feathered masks without losing authored sampling semantics, saved snapshots, live controls or fusion. Five recipes use the face-sampled mask; SurfacePeelHit has no mask. Directional per-face stagger also needs a coherence check. Reuse existing field/blend infrastructure and update the relevant mathematical-field contract if semantics change. Raising MSAA will not repair geometric discontinuities.
