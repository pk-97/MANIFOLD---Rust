# Open Triage Summary (post-cleanup)

1056 open issues remain (61 epics). Grouped below; Peter can skim in 2 min.

## Water / Physics
- BUG-l2h3 GPU FLIP end game (fastest real-time water on consumer HW)
- BUG-l2h3.26–.31 measurement, simulation, memory, timing/display, rendering, instrument
- BUG-g75v Water campaign landing queue
- BUG-651ho GPU FLIP surface (level set, mesh only on export)
- BUG-imy3 GPU FLIP whitewater (spray/foam/bubbles)
- BUG-8iwsn Qualify MPM and Matter solver correctness/cost
- BUG-mi4qf GPU FLIP coupled bodies (light/floating boxes rest)
- BUG-rpf5b Fire and smoke on shared sparse grid
- BUG-xyqfq GPU FLIP water with no air (sealed tanks)
- BUG-vglg Shared liquid physics (authoring, input takes, coupling)
- BUG-hkbdp.6 Post-T2 water cleanup (engine seams, crate internals)
- BUG-tqhp1 Qualify generator/effect behaviour across presets
- BUG-gm927 Water setup UI: out-of-domain feedback, gizmos

## 3D / Rendering
- BUG-0r2ok 3D scene engine internals (nodes at user boundaries)
- BUG-hkbdp Renderer crate split (engine hub + leaf crates)
- BUG-bfktg Rendering-side decomposition (subsystem crates, god-file split)
- BUG-a47pp Qualify scene import, camera and raster rendering
- BUG-qjis1 Qualify ray tracing, reflections, temporal denoising
- BUG-5l4o Material fidelity: SSS qualification, hybrid RT boundaries
- BUG-e3p6 Scene modifier programme qualification
- BUG-zge Textured roughness (R3)
- BUG-ss94h Qualify graph execution, freeze and fusion contracts
- BUG-8i0 Param descriptor unification

## Audio / Timeline
- BUG-ylvfo Audio clip timing, playback and export agreement
- BUG-1eo Audio event classifier continuation
- BUG-9ngk8 (+ .1–.4, .19) Trained audio detectors
- BUG-9engt Qualify transport sync and MIDI/LED behaviour
- BUG-mmct0 Playback pre-Fable code audit pilot
- BUG-5bwzr Automation lanes: pro-grade editing UX

## UI / Editor
- BUG-g6p6m Scene refinement: simple, honest, never frozen
- BUG-8k8ko Scene groups (imports as groups; Cmd+G)
- BUG-elwvu Scene document: scenes are source of truth
- BUG-brw4f App/UI decomposition (boundaries, crates, god-files)
- BUG-0mhwf Editor windows, selection, menus reliability
- BUG-df2 bitmap-UI clip/z ownership
- BUG-osn Input ownership beyond pointer
- BUG-3be ROW_MODEL_EDGES row end-states + RowHost routing
- BUG-3v6 GESTURE_ENTRY one gesture entry path
- BUG-y4b Finish param binding/modulation unification
- BUG-wp1 CHROME_PARAMS built-in performance params manifest

## Tooling / Infra (not in the four areas)
- BUG-e29g Test suite audit + upgrade campaign
- BUG-e2ddn Make build and landing verdicts reliable
- BUG-7p6 Adversarial regression soak suite
- BUG-gml VERIFICATION_INFRA flow-driver oracle
- BUG-06olb Qualify frame pacing and runtime resource lifetime
- BUG-x5sho Reconcile dev automation and agent guidance
- BUG-z95fl Concurrent worktree ownership/storage safety
- BUG-zezjf Small landings in minutes
- BUG-obnf7 Dependency packaging and releases reproducible
- BUG-yn979 Simplification pass (cut over-engineering)
