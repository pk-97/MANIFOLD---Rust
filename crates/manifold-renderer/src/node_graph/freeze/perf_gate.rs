//! Perf gate + per-device tuning (design §12.3 step 6).
//!
//! Fusion is never an unconditional win — collapsing N passes into one kernel
//! trades memory-bandwidth (fewer texture round-trips) against register
//! pressure / lost cross-dispatch overlap, and which side wins depends on the
//! chain *and* the GPU (see the design doc's perf-gate rationale). So fusion is
//! treated as a *candidate*, not a commitment: at startup this module renders
//! the fused and unfused version of each fusable effect through the real
//! executor, times both, and records a verdict. [`ChainGraph`] only renders
//! through the fused kernel when the verdict says it actually paid off.
//!
//! ## The never-worse guarantee
//!
//! The unfused graph always exists and is always correct, so the gate's
//! fallback is the path we already shipped. The verdict can only ever *veto* a
//! fusion (turn it off when measured slower) — it never forces one on. Net:
//! a fused effect is kept only if it measured faster by the margin on this
//! device; otherwise the operator gets the known-good unfused path. Fusion can
//! never make a frame slower than baseline.
//!
//! ## Why startup, set-once
//!
//! Measuring renders a few hundred frames, so it must not happen mid-show.
//! [`tune_all`] runs alongside the existing pipeline pre-warm at launch, on the
//! content device, and writes the verdicts into a set-once [`OnceLock`] — built
//! once, read lock-free forever after on the hot path (no ongoing shared
//! mutable state). Re-tuning on a device/resolution change is a follow-on; the
//! verdict is also a natural disk-cache key (`device_name`) for a future
//! measured-once-ever cache.
//!
//! v1 tunes whole-card fusable effects (just ColorGrade today). As partial
//! region fusion lands, the same gate generalises to "which partitioning is
//! fastest" by measuring more candidates — the structure here already isolates
//! the measure/decide step from the choice of candidates.

use std::sync::OnceLock;

use ahash::AHashMap;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, PresetTypeId, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::boundary_nodes::{
    FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID, SOURCE_TYPE_ID,
};
use crate::node_graph::freeze::install::{fused_generator_def_by_id, fused_view_by_id};
use crate::node_graph::{
    EffectGraphDefExt, Executor, FrameTime, MetalBackend, PrimitiveRegistry, StateStore, compile,
    loaded_preset_view_by_id,
};
use crate::render_target::RenderTarget;

const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
/// Tune at a representative HD canvas. The fuse/keep ratio for pointwise atoms
/// is near-flat across resolution (both fused and unfused scale with pixels),
/// so a verdict measured here holds at 4K; the margin below has the slack.
const TUNE_W: u32 = 1920;
const TUNE_H: u32 = 1080;
const WARMUP: u32 = 8;
const FRAMES: u32 = 60;
/// §12.5 accepted defaults: fuse only a region of ≥3 atoms that runs ≥1.3×
/// faster. Below 3 atoms the bandwidth saved rarely beats the lost overlap;
/// below 1.3× the win is inside measurement noise and not worth forfeiting the
/// unfused path's debuggability.
const MIN_ATOMS: usize = 3;
const MIN_SPEEDUP: f64 = 1.3;

/// `type_id → fuse?`. Set once by [`tune_all`]; read lock-free thereafter.
static VERDICTS: OnceLock<AHashMap<PresetTypeId, bool>> = OnceLock::new();
/// Generator twin of [`VERDICTS`] — same set-once lifecycle, keyed by generator
/// type. Separate map because generators and effects use distinct id newtypes.
static GEN_VERDICTS: OnceLock<AHashMap<PresetTypeId, bool>> = OnceLock::new();

/// Whether to render `type_id` through its fused kernel.
///
/// Until [`tune_all`] has run, this is optimistic (`true`) — matching the
/// pre-gate behavior so tests and the very first frames still fuse. Once tuned,
/// it returns the measured verdict: `true` only if the fused kernel was faster
/// by the margin on this device, else `false` (fall back to unfused). In
/// production `tune_all` completes during startup pre-warm, before any frame
/// renders, so the optimistic window is empty.
pub fn should_fuse(type_id: &PresetTypeId) -> bool {
    match VERDICTS.get() {
        Some(map) => map.get(type_id).copied().unwrap_or(true),
        None => true,
    }
}

/// Generator twin of [`should_fuse`] — optimistic (`true`) until [`tune_all`]
/// records a measured verdict, then the device's fuse/keep decision.
pub fn should_fuse_generator(type_id: &PresetTypeId) -> bool {
    match GEN_VERDICTS.get() {
        Some(map) => map.get(type_id).copied().unwrap_or(true),
        None => true,
    }
}

/// Whether the perf gate has finished tuning this process.
pub fn is_tuned() -> bool {
    VERDICTS.get().is_some()
}

/// Measure one def's mean GPU time (ms/frame) through the real executor at the
/// tuning resolution: warm up, then average real GPU time over [`FRAMES`].
/// `None` if the graph can't be built or compiled. `input_boundary` is the type
/// id of the node the host pre-binds an input texture to — `system.source` for an
/// effect, `system.generator_input` for a generator. If that boundary's output
/// isn't a live resource (a pure-generation graph that ignores its input), the
/// pre-bind is skipped and the graph renders from scratch.
fn measure_def(
    device: &GpuDevice,
    registry: &PrimitiveRegistry,
    def: &EffectGraphDef,
    input_boundary: &str,
) -> Option<f64> {
    let mut graph = def.clone().into_graph(registry).ok()?;
    let plan = compile(&graph).ok()?;
    let input_res = graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == input_boundary)
        .map(|n| n.id)
        .and_then(|boundary_id| {
            plan.steps().iter().find_map(|step| {
                if step.node == boundary_id {
                    step.outputs.iter().find(|(name, _)| *name == "out").map(|(_, id)| *id)
                } else {
                    None
                }
            })
        });

    let mut backend = MetalBackend::new(device, TUNE_W, TUNE_H, FMT);
    if let Some(res) = input_res {
        let input = RenderTarget::new(device, TUNE_W, TUNE_H, FMT, "freeze-tune-input");
        backend.pre_bind_texture_2d(res, input);
    }
    let mut exec = Executor::new(Box::new(backend));
    let frame_time = FrameTime {
        beats: Beats(1.0),
        seconds: Seconds(1.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    };

    // Dispatch through the StateStore-aware path so a def whose unfused
    // canonical graph contains a stateful primitive (Feedback / Smoothing /
    // EnvelopeFollower / Temporal) measures instead of panicking. Generator
    // presets routinely carry such nodes; effects so far don't, but the
    // state-aware entry is a superset of `execute_frame_with_gpu`, so this is
    // correct for both. owner_key=0 mirrors the generator render path.
    let mut state = StateStore::new();

    for _ in 0..WARMUP {
        let mut enc = device.create_encoder("freeze-tune-warmup");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, frame_time, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }
    let mut secs = 0.0_f64;
    for _ in 0..FRAMES {
        let mut enc = device.create_encoder("freeze-tune-timed");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, frame_time, &mut gpu, &mut state, 0);
        }
        secs += enc.commit_and_wait_completed_timed();
    }
    Some(secs * 1000.0 / f64::from(FRAMES))
}

/// The pure fuse/keep decision: fuse only a region of ≥[`MIN_ATOMS`] atoms that
/// ran ≥[`MIN_SPEEDUP`]× faster fused than unfused. Split out from [`tune_all`]
/// so the margin logic is unit-testable without a GPU. A non-positive fused
/// time is treated as a failed measurement → keep unfused.
fn decides_fuse(unfused_ms: f64, fused_ms: f64, atoms: usize) -> bool {
    if fused_ms <= 0.0 || !unfused_ms.is_finite() || !fused_ms.is_finite() {
        return false;
    }
    atoms >= MIN_ATOMS && (unfused_ms / fused_ms) >= MIN_SPEEDUP
}

/// Count of fusable worker atoms in a canonical def — every node that isn't a
/// `system.source` / `system.final_output` boundary. This is the region size
/// the [`MIN_ATOMS`] threshold checks.
fn worker_atom_count(def: &EffectGraphDef) -> usize {
    def.nodes
        .iter()
        .filter(|n| n.type_id != SOURCE_TYPE_ID && n.type_id != FINAL_OUTPUT_TYPE_ID)
        .count()
}

/// Tune every fusable effect on this device once, recording a fuse/keep-unfused
/// verdict. Idempotent — a second call is a no-op (verdicts are set-once). Call
/// from startup pre-warm so the measurement never stalls a live frame.
pub fn tune_all(device: &GpuDevice) {
    if VERDICTS.get().is_some() {
        return;
    }
    let registry = PrimitiveRegistry::with_builtin();
    let dev = device.device_name();
    let mut map: AHashMap<PresetTypeId, bool> = AHashMap::default();

    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect) {
        // Only effects that actually produce a fused view have anything to gate.
        let Some(fused_view) = fused_view_by_id(&type_id) else {
            continue;
        };
        let Some(base_view) = loaded_preset_view_by_id(&type_id) else {
            continue;
        };
        let atoms = worker_atom_count(base_view.canonical_def);
        let unfused = measure_def(device, &registry, base_view.canonical_def, SOURCE_TYPE_ID);
        let fused = measure_def(device, &registry, fused_view.canonical_def, SOURCE_TYPE_ID);

        let verdict = match (unfused, fused) {
            (Some(u), Some(f)) => {
                let pays = decides_fuse(u, f, atoms);
                let speedup = if f > 0.0 { u / f } else { 0.0 };
                let outcome = if pays { "FUSE" } else { "keep unfused (below margin)" };
                let name = type_id.as_str();
                log::info!(
                    "[freeze] tune {name} on {dev}: unfused {u:.3}ms / fused {f:.3}ms = \
                     {speedup:.2}x, {atoms} atoms -> {outcome}"
                );
                pays
            }
            _ => {
                log::warn!(
                    "[freeze] tune {} on {dev}: measurement failed -> keep unfused",
                    type_id.as_str(),
                );
                false
            }
        };
        map.insert(type_id, verdict);
    }

    log::info!("[freeze] perf-gate tuned {} fusable effect(s) on {dev}", map.len());
    let _ = VERDICTS.set(map);

    // ── Generators: same measure/decide, against the fused generator def. ──
    let mut gen_map: AHashMap<PresetTypeId, bool> = AHashMap::default();
    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Generator) {
        let Some(fused_def) = fused_generator_def_by_id(&type_id) else {
            continue; // no fusable region
        };
        let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id) else {
            continue;
        };
        let Ok(canonical) = serde_json::from_str::<EffectGraphDef>(&json) else {
            continue;
        };
        let atoms = worker_atom_count(&canonical);
        let unfused = measure_def(device, &registry, &canonical, GENERATOR_INPUT_TYPE_ID);
        let fused = measure_def(device, &registry, fused_def, GENERATOR_INPUT_TYPE_ID);
        let verdict = match (unfused, fused) {
            (Some(u), Some(f)) => {
                let pays = decides_fuse(u, f, atoms);
                let speedup = if f > 0.0 { u / f } else { 0.0 };
                let outcome = if pays { "FUSE" } else { "keep unfused (below margin)" };
                log::info!(
                    "[freeze] tune generator {} on {dev}: unfused {u:.3}ms / fused {f:.3}ms = \
                     {speedup:.2}x, {atoms} atoms -> {outcome}",
                    type_id.as_str()
                );
                pays
            }
            _ => {
                log::warn!(
                    "[freeze] tune generator {} on {dev}: measurement failed -> keep unfused",
                    type_id.as_str(),
                );
                false
            }
        };
        gen_map.insert(type_id, verdict);
    }
    log::info!("[freeze] perf-gate tuned {} fusable generator(s) on {dev}", gen_map.len());
    let _ = GEN_VERDICTS.set(gen_map);
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: these tests deliberately never call `tune_all` — it writes the
    // process-global set-once `VERDICTS`, which would perturb other tests
    // (e.g. the chain-graph main-path test relies on `should_fuse` defaulting
    // to `true` while untuned). The GPU measure/tune path is exercised at
    // runtime (startup) and via the `freeze-profile` bench; here we pin only
    // the pure decision logic + the never-worse defaults.

    #[test]
    fn colorgrade_shaped_measurement_fuses() {
        // ColorGrade's measured shape: 7 atoms, ~7× faster fused.
        assert!(decides_fuse(2.65, 0.36, 7));
        assert!(decides_fuse(0.50, 0.06, 7));
    }

    #[test]
    fn below_speedup_margin_keeps_unfused() {
        // Faster, but not by the 1.3× margin — inside noise, not worth losing
        // the unfused path's debuggability + the lost cross-dispatch overlap.
        assert!(!decides_fuse(1.0, 0.85, 7));
        assert!(!decides_fuse(1.0, 1.0, 7));
        // Slower fused → definitely keep unfused (the never-worse case).
        assert!(!decides_fuse(1.0, 1.4, 7));
    }

    #[test]
    fn below_min_atoms_keeps_unfused() {
        // Even a big speedup on a 2-atom region doesn't clear the min-atoms
        // bar — too little bandwidth saved to beat the lost overlap in general.
        assert!(!decides_fuse(2.0, 0.1, 2));
        assert!(!decides_fuse(2.0, 0.1, 1));
        assert!(decides_fuse(2.0, 0.1, 3)); // exactly at the bar, clears
    }

    #[test]
    fn failed_or_degenerate_measurement_keeps_unfused() {
        assert!(!decides_fuse(2.0, 0.0, 7)); // zero fused time = failed measure
        assert!(!decides_fuse(f64::NAN, 0.3, 7));
        assert!(!decides_fuse(2.0, f64::INFINITY, 7));
    }

    #[test]
    fn untuned_should_fuse_is_optimistic() {
        // Until tuning runs, the gate defers to the optimistic default so the
        // pre-gate behavior (and tests) still fuse. We can only assert this
        // holds when VERDICTS hasn't been set in this process — guard on it so
        // the test is correct regardless of whether a sibling test triggered a
        // tune. (No test here calls tune_all, so in practice it's unset.)
        if !is_tuned() {
            assert!(should_fuse(&PresetTypeId::new("ColorGrade")));
            assert!(should_fuse(&PresetTypeId::new("AnythingUntuned")));
        }
    }

    #[test]
    fn worker_atom_count_excludes_boundaries() {
        let json = r#"{
            "version": 1, "name": "t",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                { "id": 1, "typeId": "node.gain" },
                { "id": 2, "typeId": "node.contrast" },
                { "id": 3, "typeId": "system.final_output" }
            ],
            "wires": []
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert_eq!(worker_atom_count(&def), 2, "boundaries excluded, 2 workers");
    }
}
