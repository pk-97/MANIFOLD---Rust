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
use crate::node_graph::freeze::install::{
    canonical_region_count, fused_generator_def_by_id, fused_view_by_id,
    masked_generator_def_by_id, masked_view_by_id, override_canonical_fused_generator_def,
    override_canonical_fused_view,
};
use crate::node_graph::{
    EffectGraphDefExt, Executor, FrameTime, MetalBackend, PrimitiveRegistry, StateStore, compile,
    loaded_preset_view_by_id,
};
use crate::render_target::RenderTarget;

const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
/// Tune at 4K — the show canvas. Verdicts were previously measured at 1080p on
/// the assumption that the fuse/keep ratio is resolution-flat, but fusion's win
/// is texture-bandwidth, and bandwidth dominates harder at 4K: a region that
/// measures below the margin at 1080p can be a clear win at 4K. Measuring at
/// the resolution the show actually renders keeps the verdicts truthful.
const TUNE_W: u32 = 3840;
const TUNE_H: u32 = 2160;
const WARMUP: u32 = 8;
const FRAMES: u32 = 60;
/// Fuse on ANY measured improvement beyond noise. The original §12.5 defaults
/// (≥3 atoms, ≥1.3× whole-card speedup) predate trustworthy measurement; with
/// the gate now measuring real workloads at 4K, the never-worse guarantee IS
/// the measurement, so the bar only needs to clear noise. The whole-card ratio
/// also punished Amdahl-limited cards: FluidSim's fused region saves a real
/// ~1.05 ms/frame but the card ratio is 1.18× because scatter + blurs dominate
/// — a 1.3× bar threw ~3.3 ms/frame away across the Liveschool card types.
/// Decided with Peter 2026-06-10: always fuse if it measurably helps.
const MIN_SPEEDUP: f64 = 1.05;
const MIN_SAVING_MS: f64 = 0.05;

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
    // Allocate the full-size Array (particle) + Texture3D resources exactly
    // like the production generator path. Without this, particle dispatches
    // run on empty buffers and measure as ~free, and `node.wgsl_compute`
    // can't resolve its dispatch port (the buffer doesn't exist) so it skips
    // entirely — which is how FluidSimulation tuned at 0.6ms against a real
    // cost of ~4.6ms and earned a bogus keep-unfused verdict.
    crate::node_graph::pre_allocate_resources(&graph, &plan, device, &mut backend).ok()?;
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

/// The pure fuse/keep decision: fuse whenever the fused variant measurably
/// beats unfused — ≥[`MIN_SPEEDUP`]× faster AND ≥[`MIN_SAVING_MS`] saved per
/// frame, both noise guards rather than quality bars. No atom-count rule: with
/// real measurement the measurement decides. Split out from [`tune_all`] so
/// the margin logic is unit-testable without a GPU. A non-positive fused time
/// is treated as a failed measurement → keep unfused.
fn decides_fuse(unfused_ms: f64, fused_ms: f64) -> bool {
    if fused_ms <= 0.0 || !unfused_ms.is_finite() || !fused_ms.is_finite() {
        return false;
    }
    (unfused_ms / fused_ms) >= MIN_SPEEDUP && (unfused_ms - fused_ms) >= MIN_SAVING_MS
}

/// Chain-segment gate (docs/CHAIN_FUSION_DESIGN.md §4): fused segment def vs
/// the unfused concatenation (whose dispatches are identical to the per-card
/// chain). Same measurement harness and margins as every card verdict. Runs on
/// the chain-fusion worker's own device, never the render thread. `false` on
/// any failed measurement — keep per-card, never-worse.
pub(crate) fn measure_segment(
    device: &GpuDevice,
    registry: &PrimitiveRegistry,
    unfused: &EffectGraphDef,
    fused: &EffectGraphDef,
) -> bool {
    let Some(u) = measure_def(device, registry, unfused, SOURCE_TYPE_ID) else {
        return false;
    };
    let Some(f) = measure_def(device, registry, fused, SOURCE_TYPE_ID) else {
        return false;
    };
    let verdict = decides_fuse(u, f);
    log::info!(
        "[freeze] segment gate on {}: unfused {u:.3}ms fused {f:.3}ms -> {}",
        device.device_name(),
        if verdict { "FUSE" } else { "keep per-card" },
    );
    verdict
}

/// Minimum measured drop (ms/frame) for the greedy mask search to accept
/// disabling a region — a noise guard on the leave-one-out comparisons, well
/// under [`MIN_SAVING_MS`] because each step compares two FUSED variants of
/// the same card (shared warmup state, same resources, low variance).
const GREEDY_MIN_DROP_MS: f64 = 0.03;
/// Only run the per-region mask search on cards where it can matter: more than
/// one region AND a baseline big enough that a region-sized saving clears the
/// noise floor. Below this, all-or-nothing is measured exactly as before.
const GREEDY_MIN_UNFUSED_MS: f64 = 2.0;

/// Greedy leave-one-out over region masks: start all-on, try disabling one
/// region at a time (single ascending pass), keep any drop that measurably
/// helps. `measure` returns the mean ms/frame for a mask, `None` when that
/// variant doesn't build (skipped, never chosen). Returns the best mask and
/// its time; time is `None` only if even the all-on candidate failed.
fn greedy_region_mask(
    n: usize,
    measure: &mut dyn FnMut(&[bool]) -> Option<f64>,
) -> (Vec<bool>, Option<f64>) {
    let mut mask = vec![true; n];
    let all_on = measure(&mask);
    let Some(mut best_ms) = all_on else {
        return (mask, None);
    };
    if n > 1 {
        for i in 0..n {
            let mut cand = mask.clone();
            cand[i] = false;
            if cand.iter().all(|on| !on) {
                continue; // fully-off IS the unfused baseline, measured separately
            }
            if let Some(ms) = measure(&cand)
                && ms + GREEDY_MIN_DROP_MS < best_ms
            {
                mask = cand;
                best_ms = ms;
            }
        }
    }
    (mask, Some(best_ms))
}

/// Render a mask as `11010` for the tune log.
fn mask_str(mask: &[bool]) -> String {
    mask.iter().map(|&on| if on { '1' } else { '0' }).collect()
}

/// Count of fusable worker atoms in a canonical def — every node that isn't a
/// `system.source` / `system.final_output` boundary. Reported in the tune log
/// line for context (the fuse decision itself is purely measured).
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

        let Some(u) = unfused else {
            log::warn!(
                "[freeze] tune {} on {dev}: unfused measurement failed -> keep unfused",
                type_id.as_str(),
            );
            map.insert(type_id, false);
            continue;
        };

        // Per-region exploration on big multi-region cards; otherwise the
        // classic all-or-nothing measurement (the all-on candidate is the
        // pre-warmed canonical fused view either way).
        let n = canonical_region_count(base_view.canonical_def, &registry);
        let explore = n > 1 && u >= GREEDY_MIN_UNFUSED_MS;
        let mut measure = |mask: &[bool]| -> Option<f64> {
            let view = if mask.iter().all(|&on| on) {
                fused_view
            } else {
                masked_view_by_id(&type_id, &registry, mask)?
            };
            measure_def(device, &registry, view.canonical_def, SOURCE_TYPE_ID)
        };
        let (mask, fused) = if explore {
            greedy_region_mask(n, &mut measure)
        } else {
            let all = vec![true; n.max(1)];
            let t = measure(&all);
            (all, t)
        };

        let verdict = match fused {
            Some(f) => {
                let pays = decides_fuse(u, f);
                let speedup = if f > 0.0 { u / f } else { 0.0 };
                let partial = !mask.iter().all(|&on| on);
                let outcome = if pays { "FUSE" } else { "keep unfused (below margin)" };
                let name = type_id.as_str();
                let mask_note = if partial {
                    format!(" [regions {}]", mask_str(&mask))
                } else {
                    String::new()
                };
                log::info!(
                    "[freeze] tune {name} on {dev}: unfused {u:.3}ms / fused {f:.3}ms = \
                     {speedup:.2}x, {atoms} atoms{mask_note} -> {outcome}"
                );
                // A winning PARTIAL mask becomes the canonical fused view, so
                // the live path renders exactly the measured winner.
                if pays && partial {
                    override_canonical_fused_view(
                        &type_id,
                        masked_view_by_id(&type_id, &registry, &mask),
                    );
                }
                pays
            }
            None => {
                log::warn!(
                    "[freeze] tune {} on {dev}: fused measurement failed -> keep unfused",
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

        let Some(u) = unfused else {
            log::warn!(
                "[freeze] tune generator {} on {dev}: unfused measurement failed -> keep unfused",
                type_id.as_str(),
            );
            gen_map.insert(type_id, false);
            continue;
        };

        let n = canonical_region_count(&canonical, &registry);
        let explore = n > 1 && u >= GREEDY_MIN_UNFUSED_MS;
        let mut measure = |mask: &[bool]| -> Option<f64> {
            let def = if mask.iter().all(|&on| on) {
                fused_def
            } else {
                masked_generator_def_by_id(&type_id, &registry, mask)?
            };
            measure_def(device, &registry, def, GENERATOR_INPUT_TYPE_ID)
        };
        let (mask, fused) = if explore {
            greedy_region_mask(n, &mut measure)
        } else {
            let all = vec![true; n.max(1)];
            let t = measure(&all);
            (all, t)
        };

        let verdict = match fused {
            Some(f) => {
                let pays = decides_fuse(u, f);
                let speedup = if f > 0.0 { u / f } else { 0.0 };
                let partial = !mask.iter().all(|&on| on);
                let outcome = if pays { "FUSE" } else { "keep unfused (below margin)" };
                let mask_note = if partial {
                    format!(" [regions {}]", mask_str(&mask))
                } else {
                    String::new()
                };
                log::info!(
                    "[freeze] tune generator {} on {dev}: unfused {u:.3}ms / fused {f:.3}ms = \
                     {speedup:.2}x, {atoms} atoms{mask_note} -> {outcome}",
                    type_id.as_str()
                );
                if pays && partial {
                    override_canonical_fused_generator_def(
                        &type_id,
                        masked_generator_def_by_id(&type_id, &registry, &mask),
                    );
                }
                pays
            }
            None => {
                log::warn!(
                    "[freeze] tune generator {} on {dev}: fused measurement failed -> keep unfused",
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
        // ColorGrade's measured shape: ~7× faster fused.
        assert!(decides_fuse(2.65, 0.36));
        assert!(decides_fuse(0.50, 0.06));
    }

    #[test]
    fn amdahl_limited_card_with_real_saving_fuses() {
        // FluidSim's measured shape at 4K: whole-card ratio only 1.18×
        // (scatter + blurs dominate) but a real ~1.05 ms/frame saving.
        // The old 1.3× whole-card bar vetoed this; any-improvement fuses it.
        assert!(decides_fuse(6.96, 5.91));
        // ParticleText: 1.10×, ~0.8 ms saved.
        assert!(decides_fuse(8.81, 8.00));
    }

    #[test]
    fn inside_noise_keeps_unfused() {
        // Below the 1.05× ratio guard — measurement noise, not a win.
        assert!(!decides_fuse(1.0, 0.97));
        assert!(!decides_fuse(1.0, 1.0));
        // Ratio clears 1.05× but absolute saving under 0.05 ms — a sub-noise
        // micro-card; not worth forfeiting the unfused path's debuggability.
        assert!(!decides_fuse(0.10, 0.06));
        // Slower fused → definitely keep unfused (the never-worse case).
        assert!(!decides_fuse(1.0, 1.4));
    }

    #[test]
    fn failed_or_degenerate_measurement_keeps_unfused() {
        assert!(!decides_fuse(2.0, 0.0)); // zero fused time = failed measure
        assert!(!decides_fuse(f64::NAN, 0.3));
        assert!(!decides_fuse(2.0, f64::INFINITY));
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
    fn greedy_mask_drops_only_regions_that_measurably_help() {
        // Region 1 is a net loss (disabling it saves 0.5ms); region 0 and 2
        // help (disabling them costs time). One ascending pass finds [1,0,1].
        let mut measure = |mask: &[bool]| -> Option<f64> {
            let mut ms = 3.0;
            if mask[0] { ms -= 0.4 } // region 0 saves 0.4
            if mask[1] { ms += 0.5 } // region 1 COSTS 0.5
            if mask[2] { ms -= 0.2 } // region 2 saves 0.2
            Some(ms)
        };
        let (mask, best) = greedy_region_mask(3, &mut measure);
        assert_eq!(mask, vec![true, false, true]);
        assert!((best.unwrap() - 2.4).abs() < 1e-9);
    }

    #[test]
    fn greedy_mask_keeps_all_on_when_every_region_helps() {
        let mut measure = |mask: &[bool]| -> Option<f64> {
            Some(3.0 - mask.iter().filter(|&&on| on).count() as f64 * 0.3)
        };
        let (mask, best) = greedy_region_mask(4, &mut measure);
        assert_eq!(mask, vec![true; 4]);
        assert!((best.unwrap() - 1.8).abs() < 1e-9);
    }

    #[test]
    fn greedy_mask_ignores_sub_noise_drops_and_failed_builds() {
        // Disabling region 0 "helps" by 0.01ms — under the noise guard, keep
        // it. Region 1's masked variant fails to build → never chosen.
        let mut measure = |mask: &[bool]| -> Option<f64> {
            if !mask[1] {
                return None;
            }
            Some(if mask[0] { 2.00 } else { 1.99 })
        };
        let (mask, best) = greedy_region_mask(2, &mut measure);
        assert_eq!(mask, vec![true, true]);
        assert!((best.unwrap() - 2.0).abs() < 1e-9);
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
