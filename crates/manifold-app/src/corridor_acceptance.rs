//! D8.4 acceptance — Stone Effects v1/v2 as the held-out corridor inputs
//! (SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md D8.4, P3 phase).
//!
//! Deliberate native-GPU journey: run with `--features journey-proofs`
//! and `CORRIDOR_PROJECT_DIR` pointing at the two reference projects.
//! Use `--test-threads=1` because project preset overlays are process-global.
//! The three acceptance gates are:
//!
//! 1. `corridor_acceptance_migration_smoke` — both reference projects load
//!    through the real loader + the app's per-layer migration sequence
//!    (project_io.rs order); asserts the structural facts INV-EC5 demands
//!    on the REAL files: all three loop atoms traced, zero
//!    count/jitter_period/stride hits, the D2 camera wire present, and the
//!    card exposures showing Pattern + Stride rows with ids preserved.
//! 2. `corridor_acceptance_stone_effects_wrap_metric` — the BUG-b6iv
//!    (scene-loop-wrap-one-frame-object-blip) repro pattern: headless
//!    ContentThread + tick_frame with per-tick readback of the clip
//!    generator output mean alpha (the recorded metric oracle), across
//!    three complete loop wraps. Recorded pre-migration
//!    baseline: v1 mean alpha 0.0166 -> 0.0142 (14% coverage loss at the
//!    tick before wrap, |delta| 30-100x baseline); v2 a much larger flash
//!    (stride-7 outrun). The corridor claim: both collapse to baseline
//!    noise. Real imports are device-seed nondeterministic (BUG-twa6), so
//!    the gate compares wrap-adjacent WINDOW distributions against the
//!    in-run baseline distribution, not single-tick exact equality.
//! 3. `corridor_acceptance_crossing_spike_gate` — content-thread work gate
//!    (design P3, review finding 10): a synthetic corridor graph at the
//!    fastest crossing cadence (patterns_per_loop = 8, pattern_length = 1,
//!    one beat per loop) driven through the real engine tick +
//!    render_content path. Any measured frame > 20ms fails. Run with
//!    MANIFOLD_RENDER_TRACE=1 ... -- --nocapture for the per-section
//!    breakdown on slow frames (BUG-035 pattern).
//!
//! The Stone files live under Dropbox; if they are absent the two project
//! tests fail loudly with the path (they are the held-out gate inputs).
#![cfg(all(test, target_os = "macos"))]

use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::{Beats, Bpm};

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::content_thread::ContentThread;
use crate::headless_harness::headless_content_thread;

fn stone_path(name: &str) -> PathBuf {
    if let Ok(path) = std::env::var("CORRIDOR_PROJECT_PATH") {
        return PathBuf::from(path);
    }
    PathBuf::from(
        std::env::var("CORRIDOR_PROJECT_DIR")
            .expect("set CORRIDOR_PROJECT_DIR to the held-out Stone Effects v1/v2 directory"),
    )
    .join(name)
}

const STONE_V1: &str = "Stone Effects v1.manifold";
const STONE_V2: &str = "Stone Effects v2.manifold";

/// Load a project and run the app's per-layer load-migration sequence in
/// the exact order project_io.rs runs it at open — the corridor migration
/// (D7) plus its ordering neighbors.
fn load_migrated(path: &Path) -> manifold_core::project::Project {
    let mut project =
        manifold_io::loader::load_project_with(path, crate::project_io::install_embedded_presets)
            .unwrap_or_else(|e| {
                panic!("{} must load through the real loader: {e}", path.display())
            });
    for layer in &mut project.timeline.layers {
        if let Some(graph) = layer.gen_params_mut().and_then(|gp| gp.graph.as_mut()) {
            manifold_core::scene_object_migration::migrate_scene_object_wires(graph);
            manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(graph);
            manifold_renderer::node_graph::scene_modifier::migrate_pre_switch_scene_loops(graph);
            manifold_renderer::node_graph::scene_modifier::migrate_fixed_row_scene_loops(graph);
            manifold_renderer::node_graph::scene_modifier::migrate_loop_exposure_rows(graph);
        }
    }
    project
}

/// The layer graph carrying the loop, its layer index, and the loop-phase
/// period in beats. beat_ramp runs 1/bars cycles per beat (SCENE_LOOP_DESIGN
/// D6), so the wrap period IS `bars` beats, despite the name.
fn loop_graph_and_period(
    project: &manifold_core::project::Project,
) -> (usize, &EffectGraphDef, f64) {
    for (idx, layer) in project.timeline.layers.iter().enumerate() {
        let Some(graph) = layer.gen_params().and_then(|gp| gp.graph.as_ref()) else {
            continue;
        };
        let Some(phase) = graph
            .nodes
            .iter()
            .find(|n| n.type_id == "node.beat_ramp" && n.node_id.as_str() == "loop_phase")
        else {
            continue;
        };
        let bars = phase
            .params
            .get("bars")
            .and_then(|v| match v {
                manifold_core::effect_graph_def::SerializedParamValue::Float { value } => {
                    Some(*value as f64)
                }
                _ => None,
            })
            .unwrap_or(0.0);
        assert!(bars > 0.0, "loop_phase bars must be positive");
        return (idx, graph, bars);
    }
    panic!("no layer carries a loop_phase graph");
}

fn param_f32(def: &EffectGraphDef, node_id: &str, param: &str) -> Option<f32> {
    def.nodes
        .iter()
        .find(|n| n.node_id.as_str() == node_id)
        .and_then(|n| n.params.get(param))
        .and_then(|v| match v {
            manifold_core::effect_graph_def::SerializedParamValue::Float { value } => Some(*value),
            _ => None,
        })
}

// ─── Gate 1: migration smoke on the real projects ─────────────────────

#[test]
fn corridor_acceptance_migration_smoke() {
    // (file, expected pattern_length J, expected patterns_per_loop K)
    let cases = [(STONE_V1, 1.0, 1.0), (STONE_V2, 7.0, 1.0)];
    for (file, want_j, want_k) in cases {
        if std::env::var("CORRIDOR_PROJECT").is_ok_and(|name| name != file) {
            continue;
        }
        let path = stone_path(file);
        assert!(path.exists(), "held-out input missing: {}", path.display());
        let project = load_migrated(&path);
        let (layer_idx, def, _) = loop_graph_and_period(&project);

        // All three loop atoms trace (the descriptor requires loop_phase,
        // scene_array, loop_camera).
        let traced = manifold_renderer::node_graph::scene_modifier::trace_modifier(
            &manifold_renderer::node_graph::scene_modifier::SCENE_LOOP_DESCRIPTOR,
            &def.nodes,
        );
        assert!(
            traced.applied(&manifold_renderer::node_graph::scene_modifier::SCENE_LOOP_DESCRIPTOR),
            "{file}: the loop atoms must all trace after migration"
        );

        // Negative gate: zero count/jitter_period/stride hits anywhere in
        // the migrated def — node params, exposed sets, and every exposure
        // target (INV-EC5).
        for node in &def.nodes {
            for dead in ["count", "jitter_period", "stride"] {
                assert!(
                    !node.params.contains_key(dead),
                    "{file}: node {:?} still carries {dead}",
                    node.node_id.as_str()
                );
                assert!(
                    !node.exposed_params.contains(dead),
                    "{file}: node {:?} still exposes {dead}",
                    node.node_id.as_str()
                );
            }
        }
        if let Some(meta) = def.preset_metadata.as_ref() {
            for binding in &meta.bindings {
                if let BindingTarget::Node { node_id, param } = &binding.target {
                    for dead in ["count", "jitter_period", "stride"] {
                        assert_ne!(
                            (node_id.as_str(), param.as_str()),
                            if dead == "stride" {
                                ("loop_camera", dead)
                            } else {
                                ("scene_array", dead)
                            },
                            "{file}: exposure {} still targets {dead}",
                            binding.id
                        );
                    }
                }
            }
        }

        // The D7 arithmetic lands where the design rules it must. v2's
        // on-disk stride is 1 (count 8, jitter_period 7), so K = round(1/7)
        // = 0 -> clamped to 1: travel 7 cells/loop either way — the same
        // shape the recorded stride-7/J=7 state would migrate to.
        assert_eq!(
            param_f32(def, "scene_array", "pattern_length"),
            Some(want_j),
            "{file}: scene_array.pattern_length"
        );
        assert_eq!(
            param_f32(def, "loop_camera", "patterns_per_loop"),
            Some(want_k),
            "{file}: loop_camera.patterns_per_loop"
        );
        // Both period consumers must bind to the SAME preserved card slot;
        // comparing def defaults misses stored values and modulation.
        let gp = project.timeline.layers[layer_idx].gen_params().unwrap();
        let array = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "scene_array")
            .unwrap();
        let camera = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "loop_camera")
            .unwrap();
        let pattern_slot = gp
            .binding_id_for_node_param(array.id, "pattern_length")
            .unwrap();
        assert_eq!(
            gp.binding_id_for_node_param(camera.id, "pattern_length"),
            Some(pattern_slot)
        );
        let spacing_slot = gp
            .binding_id_for_node_param(camera.id, "cell_size")
            .unwrap();
        assert_eq!(
            gp.binding_id_for_node_param(array.id, "cell_size"),
            Some(spacing_slot)
        );

        // D2 wire: the loop camera feeds the corridor window.
        let array_doc = def
            .nodes
            .iter()
            .find(|n| n.node_id.as_str() == "scene_array")
            .map(|n| n.id)
            .expect("scene_array present");
        let camera_doc = def
            .nodes
            .iter()
            .find(|n| n.node_id.as_str() == "loop_camera")
            .map(|n| n.id)
            .expect("loop_camera present");
        assert!(
            def.wires.iter().any(|w| {
                w.from_node == camera_doc && w.to_node == array_doc && w.to_port == "camera"
            }),
            "{file}: loop_camera.out -> scene_array.camera must exist post-migration"
        );

        // Exposures: Pattern + Stride rows visible, ids preserved (the
        // binding rewrite ruling — saved mappings stay alive).
        let meta = def
            .preset_metadata
            .as_ref()
            .expect("exposure metadata present");
        let pattern_binding = meta
            .bindings
            .iter()
            .find(|b| {
                matches!(&b.target, BindingTarget::Node { node_id, param }
                    if node_id == "scene_array" && param == "pattern_length")
            })
            .unwrap_or_else(|| panic!("{file}: no Pattern exposure row"));
        let stride_binding = meta
            .bindings
            .iter()
            .find(|b| {
                matches!(&b.target, BindingTarget::Node { node_id, param }
                    if node_id == "loop_camera" && param == "patterns_per_loop")
            })
            .unwrap_or_else(|| panic!("{file}: no Stride exposure row"));
        let pattern_spec = meta
            .params
            .iter()
            .find(|p| p.id == pattern_binding.id)
            .unwrap_or_else(|| panic!("{file}: Pattern row lost its spec"));
        let stride_spec = meta
            .params
            .iter()
            .find(|p| p.id == stride_binding.id)
            .unwrap_or_else(|| panic!("{file}: Stride row lost its spec"));
        assert_eq!(pattern_spec.name, "Pattern", "{file}: renamed spec");
        assert_eq!(stride_spec.name, "Stride", "{file}: renamed spec");

        eprintln!(
            "[corridor-acceptance] migration smoke {file}: J={want_j} K={want_k} \
             pattern_row={:?} stride_row={:?} OK",
            pattern_binding.id, stride_binding.id
        );
    }
}

// ─── Gate 2: the BUG-b6iv wrap metric on the real projects ────────────

/// One recorded tick: mean alpha of the clip generator output plus frame wall time.
#[derive(Clone, Copy)]
struct TickSample {
    clip_alpha: f64,
    wall_ms: f64,
}

fn capture_frame(
    device: &manifold_gpu::GpuDevice,
    tex: &manifold_gpu::GpuTexture,
    frame: u64,
    beat: f64,
) {
    assert_eq!(tex.format.bytes_per_pixel(), 8);
    let row_bytes = tex.width * 8;
    let buffer = device.create_buffer_shared((row_bytes * tex.height) as u64);
    let mut encoder = device.create_encoder("corridor-capture");
    encoder.copy_texture_to_buffer(tex, &buffer, tex.width, tex.height, row_bytes);
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().unwrap();
    let bytes = unsafe { std::slice::from_raw_parts(ptr, (row_bytes * tex.height) as usize) };
    let rgba: Vec<u8> = bytes
        .chunks_exact(2)
        .map(|b| {
            (half::f16::from_bits(u16::from_le_bytes([b[0], b[1]]))
                .to_f32()
                .clamp(0.0, 1.0)
                * 255.0)
                .round() as u8
        })
        .collect();
    let variant = std::env::var("CORRIDOR_VARIANT").unwrap_or_else(|_| "baseline".into());
    let dir = PathBuf::from("/tmp/corridor_acceptance").join(variant);
    std::fs::create_dir_all(&dir).unwrap();
    let file =
        std::fs::File::create(dir.join(format!("frame-{frame:04}-beat-{beat:.6}.png"))).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), tex.width, tex.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgba)
        .unwrap();
}

fn readback_mean_rgba(
    device: &manifold_gpu::GpuDevice,
    tex: &manifold_gpu::GpuTexture,
) -> [f64; 4] {
    assert_eq!(tex.format, manifold_gpu::GpuTextureFormat::Rgba16Float);
    let row_bytes = tex.width * tex.format.bytes_per_pixel();
    let size = row_bytes as usize * tex.height as usize;
    let buffer = device.create_buffer_shared(size as u64);
    let mut encoder = device.create_encoder("corridor-acceptance-readback");
    encoder.copy_texture_to_buffer(tex, &buffer, tex.width, tex.height, row_bytes);
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().expect("shared readback buffer");
    // GPU completion above makes the mapped bytes available to this thread.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size) };
    let mut sums = [0.0; 4];
    for pixel in bytes.chunks_exact(8) {
        for (channel, sum) in sums.iter_mut().enumerate() {
            *sum += half::f16::from_le_bytes([pixel[channel * 2], pixel[channel * 2 + 1]]).to_f64();
        }
    }
    sums.map(|sum| sum / (tex.width * tex.height) as f64)
}

fn mean_alpha(rgba: [f64; 4]) -> f64 {
    rgba[3]
}

fn drive_and_sample(
    ct: &mut ContentThread,
    state_tx: &crossbeam_channel::Sender<ContentState>,
    ticks: usize,
    clip_id: &str,
    loop_ticks: usize,
) -> Vec<TickSample> {
    let mut out = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        let t0 = std::time::Instant::now();
        ct.tick_frame(state_tx);
        let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let device = ct
            .content_pipeline
            .native_device()
            .expect("native device available");
        let clip_mean = ct
            .engine
            .renderers()
            .iter()
            .find_map(|r| {
                r.as_any()
                    .downcast_ref::<manifold_renderer::generator_renderer::GeneratorRenderer>()
                    .and_then(|gr| gr.get_clip_texture(clip_id))
            })
            .map(|tex| {
                if std::env::var_os("CORRIDOR_CAPTURE").is_some()
                    && (ct.frame_count as usize % loop_ticks <= 2
                        || ct.frame_count as usize % loop_ticks >= loop_ticks.saturating_sub(2))
                {
                    capture_frame(device, tex, ct.frame_count, ct.engine.current_beat_f64());
                }
                readback_mean_rgba(device, tex)
            })
            .unwrap_or([f64::NAN; 4]);
        out.push(TickSample {
            clip_alpha: mean_alpha(clip_mean),
            wall_ms,
        });
    }
    out
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Wrap-window stats vs the in-run baseline. Returns the worst drop
/// fraction and the worst flash ratio across windows, and prints the
/// distribution table.
fn wrap_report(
    tag: &str,
    samples: &[TickSample],
    wrap_ticks: &[usize],
    window: usize,
) -> (f64, f64) {
    let n = samples.len();
    let in_window = |t: usize| {
        wrap_ticks
            .iter()
            .any(|&w| t + window >= w && t <= w + window)
    };
    assert!(!samples.is_empty(), "{tag}: no samples recorded");
    assert!(
        samples.iter().all(|s| s.clip_alpha.is_finite()),
        "{tag}: nonfinite or missing clip output"
    );
    let valid = |v: f64| v.is_finite();

    let baseline_alphas: Vec<f64> = (0..n)
        .filter(|&t| !in_window(t))
        .map(|t| samples[t].clip_alpha)
        .filter(|&v| valid(v))
        .collect();
    assert!(
        !baseline_alphas.is_empty(),
        "{tag}: no baseline outside wrap windows"
    );
    let base_mean = baseline_alphas.iter().sum::<f64>() / baseline_alphas.len() as f64;
    assert!(base_mean > 0.0, "{tag}: missing visible geometry");
    let mut sorted = baseline_alphas.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let base_p05 = percentile(&sorted, 0.05);

    // Adjacent-tick |delta| over the baseline (the noise floor the corridor
    // claim says the wrap collapses into).
    let mut base_deltas: Vec<f64> = (1..n)
        .filter(|&t| !in_window(t) && !in_window(t - 1))
        .filter_map(|t| {
            let (a, b) = (samples[t - 1].clip_alpha, samples[t].clip_alpha);
            valid(a).then(|| (b - a).abs())
        })
        .collect();
    assert!(
        !base_deltas.is_empty(),
        "{tag}: no adjacent baseline samples"
    );
    base_deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let delta_med = percentile(&base_deltas, 0.50);
    let delta_p95 = percentile(&base_deltas, 0.95);
    let delta_max = base_deltas.last().copied().unwrap_or(f64::NAN);

    eprintln!(
        "[corridor-acceptance] {tag}: baseline clip-alpha mean={base_mean:.6} p05={base_p05:.6} \
         |delta| med={delta_med:.2e} p95={delta_p95:.2e} max={delta_max:.2e} \
         (n_base={} n_delta={})",
        baseline_alphas.len(),
        base_deltas.len()
    );

    let mut worst_drop = 0.0f64;
    let mut worst_flash = 0.0f64;
    for &w in wrap_ticks {
        let lo = w.saturating_sub(window);
        let hi = (w + window).min(n.saturating_sub(1));
        let alphas: Vec<f64> = (lo..=hi).map(|t| samples[t].clip_alpha).collect();
        let valid_alphas: Vec<f64> = alphas.iter().copied().filter(|&v| valid(v)).collect();
        if valid_alphas.is_empty() {
            continue;
        }
        let win_mean = valid_alphas.iter().sum::<f64>() / valid_alphas.len() as f64;
        let mut win_max_delta = 0.0f64;
        let mut win_max_drop = 0.0f64;
        for t in (lo + 1)..=hi {
            let (a, b) = (samples[t - 1].clip_alpha, samples[t].clip_alpha);
            if valid(a) && valid(b) {
                win_max_delta = win_max_delta.max((b - a).abs());
                if a > 0.0 {
                    win_max_drop = win_max_drop.max(((a - b) / a).max(0.0));
                }
            }
        }
        let drop = win_max_drop;
        let flash = if delta_p95 > 0.0 {
            win_max_delta / delta_p95
        } else if win_max_delta > 0.0 {
            f64::INFINITY
        } else {
            0.0
        };
        worst_drop = worst_drop.max(drop);
        worst_flash = worst_flash.max(flash);
        eprintln!(
            "[corridor-acceptance] {tag}: wrap@{w:5} ticks[{lo}..={hi}] \
             alpha mean={win_mean:.6} | max|delta|={win_max_delta:.2e} \
             (p95 ratio {flash:.2}x, drop fraction {drop:.3})"
        );
    }
    (worst_drop, worst_flash)
}

#[cfg(test)]
mod metric_tests {
    use super::{TickSample, wrap_report};

    fn samples(values: &[f64]) -> Vec<TickSample> {
        values
            .iter()
            .map(|&clip_alpha| TickSample {
                clip_alpha,
                wall_ms: 1.0,
            })
            .collect()
    }

    #[test]
    fn smooth_periodic_coverage_passes() {
        let values: Vec<f64> = (0..48)
            .map(|t| 1.0 + (t as f64 * 0.2).sin() * 0.01)
            .collect();
        let (drop, flash) = wrap_report("smooth", &samples(&values), &[16, 32], 2);
        assert!(drop < 0.05);
        assert!(flash <= 5.0);
    }

    #[test]
    fn isolated_seam_dropout_fails() {
        let mut values = vec![1.0; 32];
        values[16] = 0.8;
        let (drop, _) = wrap_report("dropout", &samples(&values), &[16], 1);
        assert!(drop >= 0.05);
    }

    #[test]
    fn nonfinite_samples_are_rejected() {
        let result = std::panic::catch_unwind(|| {
            wrap_report("nonfinite", &samples(&[1.0, f64::NAN]), &[1], 1)
        });
        assert!(result.is_err());
    }
}

fn has_import(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> bool {
    nodes.iter().any(|node| {
        node.type_id == "node.gltf_mesh_source"
            || node
                .group
                .as_ref()
                .is_some_and(|group| has_import(&group.nodes))
    })
}

#[test]
fn corridor_acceptance_stone_effects_wrap_metric() {
    for (file, recorded) in [
        (
            STONE_V1,
            "recorded pre-migration: mean alpha 0.0166 -> 0.0142 (14% drop, \
             |delta| 30-100x baseline)",
        ),
        (
            STONE_V2,
            "recorded pre-migration: much larger wrap flash (stride-7 outrun)",
        ),
    ] {
        if std::env::var("CORRIDOR_PROJECT").is_ok_and(|name| name != file) {
            continue;
        }
        let path = stone_path(file);
        assert!(path.exists(), "held-out input missing: {}", path.display());
        eprintln!("[corridor-acceptance] === {file} — {recorded}");

        let mut project = load_migrated(&path);
        let layer_idx = loop_graph_and_period(&project).0;
        let variant = std::env::var("CORRIDOR_VARIANT").unwrap_or_else(|_| "baseline".into());
        if let Ok(overrides) = std::env::var("CORRIDOR_PARAMS") {
            let gp = project.timeline.layers[layer_idx].gen_params_mut().unwrap();
            for assignment in overrides.split(',') {
                let (id, value) = assignment.split_once('=').expect("id=value override");
                assert!(
                    gp.set_base_param(id, value.parse().expect("numeric override")),
                    "unknown param {id}"
                );
                eprintln!("[corridor-probe] override {id}={value}");
            }
        }
        if let Ok(source) = std::env::var("CORRIDOR_OUTPUT") {
            let graph = project.timeline.layers[layer_idx]
                .gen_params_mut()
                .unwrap()
                .graph
                .as_mut()
                .unwrap();
            let render = graph
                .nodes
                .iter()
                .find(|n| n.node_id.as_str() == source)
                .expect("output node")
                .id;
            let final_id = graph
                .nodes
                .iter()
                .find(|n| n.type_id == "system.final_output")
                .unwrap()
                .id;
            let output = graph
                .wires
                .iter_mut()
                .find(|w| w.to_node == final_id && w.to_port == "in")
                .unwrap();
            output.from_node = render;
            output.from_port = if source == "render" { "color" } else { "out" }.into();
        }
        let (_, def, _) = loop_graph_and_period(&project);
        // The saved instance value overrides the graph default (v2: 4 vs 8).
        let gp = project.timeline.layers[layer_idx].gen_params().unwrap();
        let phase_node = def
            .nodes
            .iter()
            .find(|n| n.node_id.as_str() == "loop_phase")
            .unwrap();
        let phase_binding = gp
            .binding_id_for_node_param(phase_node.id, "bars")
            .expect("loop bars binding");
        let loop_beats = gp.get_base_param(&phase_binding) as f64;
        eprintln!("[corridor-probe] variant={variant} actual bars={loop_beats}");
        let bpm = project.settings.bpm.0 as f64;
        let ticks_per_beat = 60.0 * 60.0 / bpm; // 60fps frame clock
        let loop_ticks = (loop_beats * ticks_per_beat).round() as usize;
        eprintln!(
            "[corridor-acceptance] {file}: bpm={bpm} loop={loop_beats} beats \
             = {loop_ticks} ticks/loop"
        );

        // The clip + layer the metric reads.
        let layer = &project.timeline.layers[layer_idx];
        let clip_id = layer.clips[0].id.to_string();
        assert!(
            has_import(&def.nodes),
            "{file}: expects the rosetta stone real import"
        );

        let mut ct: ContentThread = headless_content_thread(project, 320, 180);
        let (state_tx, _rx) = crossbeam_channel::unbounded::<ContentState>();
        ct.timer.set_frame_clocked(true);
        ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
        ct.handle_command(ContentCommand::Play);

        // Warm-in one full loop (pipeline compiles, first accel build), then
        // three loops of samples -> two interior wraps plus boundary margin.
        drive_and_sample(&mut ct, &state_tx, loop_ticks, &clip_id, loop_ticks);
        let samples =
            drive_and_sample(&mut ct, &state_tx, 3 * loop_ticks + 9, &clip_id, loop_ticks);

        // Per-tick CSV for the report/plots.
        let out_dir = PathBuf::from("/tmp/corridor_acceptance");
        std::fs::create_dir_all(&out_dir).unwrap();
        let csv_path = out_dir.join(format!(
            "{}-{variant}.csv",
            file.trim_end_matches(".manifold")
        ));
        let mut csv = String::from("tick,clip_alpha,wall_ms\n");
        for (t, s) in samples.iter().enumerate() {
            csv.push_str(&format!("{},{:.8},{:.2}\n", t, s.clip_alpha, s.wall_ms));
        }
        std::fs::write(&csv_path, csv).unwrap();
        eprintln!(
            "[corridor-acceptance] {file}: per-tick CSV at {}",
            csv_path.display()
        );

        let wraps: Vec<usize> = (1..=3).map(|k| k * loop_ticks).collect();
        let window = 8;
        let (worst_drop, worst_flash) = wrap_report(file, &samples, &wraps, window);

        // The corridor claims. Recorded failure was a 14% coverage drop with
        // |delta| 30-100x baseline; the corridor must sit inside its own
        // noise floor. Windows and both sides of the comparison come from
        // this run, so device-seed noise (BUG-twa6) cancels out of the ratio.
        assert!(
            worst_drop < 0.05,
            "{file}: wrap-adjacent coverage drop fraction {worst_drop:.3} — the BUG-b6iv blip class survives"
        );
        assert!(
            worst_flash <= 5.0,
            "{file}: wrap-adjacent flash {worst_flash:.2}x baseline p95 — the outrun flash class survives"
        );
    }
}

// ─── Gate 3: content-thread spike gate at crossing cadence ────────────

/// Minimal corridor graph at the fastest crossing shape: one beat per loop,
/// 8 cells of travel per loop -> a cell-boundary crossing every 3.75 ticks.
/// RT on (the corridor's descriptor refit cost is part of the gate).
fn spike_corridor_def() -> EffectGraphDef {
    use manifold_core::effect_graph_def::{
        EffectGraphNode, EffectGraphWire, PresetMetadata, SerializedParamValue,
    };
    use manifold_core::preset_type_id::PresetTypeId;
    use std::collections::BTreeMap;

    fn node(
        id: u32,
        node_id: &str,
        type_id: &str,
        params: BTreeMap<String, SerializedParamValue>,
    ) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::new(node_id),
            type_id: type_id.to_string(),
            handle: Some(node_id.to_string()),
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }
    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.to_string(),
            to_node,
            to_port: to_port.to_string(),
        }
    }
    fn f(v: f32) -> SerializedParamValue {
        SerializedParamValue::Float { value: v }
    }

    let mut params_phase = BTreeMap::new();
    params_phase.insert("bars".to_string(), f(1.0));
    params_phase.insert("attack".to_string(), f(1.0));
    let mut params_array = BTreeMap::new();
    params_array.insert("pattern_length".to_string(), f(1.0));
    params_array.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
    params_array.insert("cell_size".to_string(), f(10.0));
    let mut params_camera = BTreeMap::new();
    params_camera.insert("patterns_per_loop".to_string(), f(8.0));
    params_camera.insert("pattern_length".to_string(), f(1.0));
    params_camera.insert("cell_size".to_string(), f(10.0));
    params_camera.insert("home".to_string(), f(-5.0));
    params_camera.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
    params_camera.insert("fov_y".to_string(), f(0.9));
    let mut params_scene = BTreeMap::new();
    params_scene.insert("objects".to_string(), f(1.0));
    params_scene.insert("lights".to_string(), f(0.0));
    params_scene.insert(
        "rt_enabled".to_string(),
        SerializedParamValue::Bool { value: true },
    );
    let mut params_mat = BTreeMap::new();
    params_mat.insert("color_r".to_string(), f(0.8));
    params_mat.insert("color_g".to_string(), f(0.3));
    params_mat.insert("color_b".to_string(), f(0.3));

    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("CorridorSpikeGate".to_string()),
            display_name: "Corridor Spike Gate".to_string(),
            category: "Test".to_string(),
            osc_prefix: "test".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_bounds: None,
        }),
        nodes: vec![
            node(0, "input", "system.generator_input", BTreeMap::new()),
            node(1, "loop_phase", "node.beat_ramp", params_phase),
            node(2, "scene_array", "node.scene_array", params_array),
            node(3, "loop_camera", "node.loop_camera", params_camera),
            node(4, "cube_mesh", "node.cube_mesh", BTreeMap::new()),
            node(5, "mat", "node.unlit_material", params_mat),
            node(6, "scene_object", "node.scene_object", BTreeMap::new()),
            node(7, "scene", "node.render_scene", params_scene),
            node(8, "out", "system.final_output", BTreeMap::new()),
        ],
        wires: vec![
            wire(1, "out", 3, "phase"),
            wire(3, "out", 7, "camera"),
            wire(3, "out", 2, "camera"),
            wire(4, "vertices", 6, "vertices"),
            wire(5, "out", 6, "material"),
            wire(2, "out", 6, "instances"),
            wire(6, "object", 7, "object_0"),
            wire(7, "color", 8, "in"),
        ],
    }
}

#[test]
fn corridor_acceptance_crossing_spike_gate() {
    let mut project = manifold_core::project::Project::default();
    project.settings.bpm = Bpm(120.0);
    let mut layer = manifold_core::layer::Layer::new_generator(
        "CorridorSpike".to_string(),
        manifold_core::PresetTypeId::from_string("CorridorSpikeGate".to_string()),
        0,
    );
    layer.gen_params_or_init().graph = Some(spike_corridor_def());
    layer
        .clips
        .push(manifold_core::clip::TimelineClip::new_generator(
            Beats(0.0),
            Beats(480.0),
        ));
    project.timeline.layers.push(layer);

    let mut ct = headless_content_thread(project, 320, 180);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded();
    ct.timer.set_frame_clocked(true);
    ct.handle_command(ContentCommand::Play);

    // One beat per loop at 120 BPM = 30 ticks/loop; 8 cells of travel per
    // loop -> a window refill every 3.75 ticks. Warm-in one loop (pipeline
    // compiles, first accel build, first crossings), then measure four.
    const WARM: u64 = 30;
    const MEASURE: u64 = 120;

    let mut slowest: Vec<(u64, f64)> = Vec::new();
    let mut failures: Vec<(u64, f64)> = Vec::new();
    for frame in 0..(WARM + MEASURE) {
        let t0 = std::time::Instant::now();
        ct.tick_frame(&state_tx);
        let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if frame >= WARM {
            slowest.push((frame, wall_ms));
            if wall_ms > 20.0 {
                failures.push((frame, wall_ms));
            }
        }
    }

    slowest.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    eprintln!(
        "[corridor-acceptance] spike gate: slowest measured frames (tick, ms, crossing = tick%4 in [0..2] of 3.75):"
    );
    for (frame, ms) in slowest.iter().take(10) {
        eprintln!(
            "[corridor-acceptance]   tick {frame:3}  {ms:7.2} ms  {}",
            if frame % 4 <= 1 {
                "crossing-adjacent"
            } else {
                ""
            }
        );
    }
    assert!(
        failures.is_empty(),
        "spike gate: {} frame(s) over 20ms: {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

/// Real modifier-card drag dispatch into a live headless content thread.
#[test]
fn modifier_live_scrub_keeps_runtime_couplings_through_undo_redo() {
    use manifold_ui::panels::{GraphParamTarget, PanelAction, ScrubPhase, ScrubValue, ValueRef};
    for file in [STONE_V1, STONE_V2] {
        let path = stone_path(file);
        let mut local = load_migrated(&path);
        let layer_idx = loop_graph_and_period(&local).0;
        let layer_id = local.timeline.layers[layer_idx].layer_id.clone();
        let gp = local.timeline.layers[layer_idx].gen_params().unwrap();
        let graph = gp.graph.as_ref().unwrap();
        let cam_id = graph
            .nodes
            .iter()
            .find(|n| n.node_id.as_str() == "loop_camera")
            .unwrap()
            .id;
        let array_id = graph
            .nodes
            .iter()
            .find(|n| n.node_id.as_str() == "scene_array")
            .unwrap()
            .id;
        let spacing_id = gp.binding_id_for_node_param(cam_id, "cell_size").unwrap();
        let pattern_id = gp
            .binding_id_for_node_param(array_id, "pattern_length")
            .unwrap();
        let mut ct = headless_content_thread(local.clone(), 320, 180);
        let (state_tx, _state_rx) = crossbeam_channel::unbounded();
        ct.timer.set_frame_clocked(true);
        ct.handle_command(ContentCommand::SeekToBeat(Beats(1.0)));
        ct.handle_command(ContentCommand::Play);
        for _ in 0..5 {
            ct.tick_frame(&state_tx);
        }
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let state = ContentState::default();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut active_layer = Some(layer_id.clone());
        let mut prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let live_values = |ct: &ContentThread| {
            ct.engine
                .renderers()
                .iter()
                .find_map(|r| {
                    r.as_any()
                        .downcast_ref::<manifold_renderer::generator_renderer::GeneratorRenderer>()
                })
                .unwrap()
                .live_node_params(&layer_id)
        };
        let read = |ct: &ContentThread, node: &str, param: &str| {
            live_values(ct)
                .iter()
                .find(|(n, _)| n.as_str() == node)
                .unwrap_or_else(|| panic!("live node {node} absent"))
                .1
                .iter()
                .find(|(p, _)| *p == param)
                .unwrap()
                .1
        };
        let assert_coupled = |ct: &ContentThread| {
            assert_eq!(
                read(ct, "loop_camera", "cell_size"),
                read(ct, "scene_array", "cell_size"),
                "runtime Spacing must agree before rendering"
            );
            assert_eq!(
                read(ct, "loop_camera", "pattern_length"),
                read(ct, "scene_array", "pattern_length"),
                "runtime Pattern must agree before rendering"
            );
            assert_eq!(
                ct.engine.project().unwrap().timeline.layers[layer_idx]
                    .generator_graph_structure_version(),
                0,
                "a live slider must not change topology"
            );
        };
        assert_coupled(&ct);
        for (id, value) in [(spacing_id, 1.4), (pattern_id, 3.0)] {
            let old_value = ct.engine.project().unwrap().timeline.layers[layer_idx]
                .gen_params()
                .unwrap()
                .get_base_param(&id);
            for (label, phase) in [
                ("begin", ScrubPhase::Begin),
                ("move", ScrubPhase::Move(ScrubValue::Scalar(value))),
                ("commit", ScrubPhase::Commit),
            ] {
                eprintln!("[modifier-scrub] {id} {label} -> {value}");
                let action = PanelAction::Scrub(
                    ValueRef::Param(
                        GraphParamTarget::GeneratorOf(layer_id.clone()),
                        id.clone().into(),
                    ),
                    phase,
                );
                crate::ui_bridge::dispatch(
                    &action,
                    &mut crate::ui_bridge::DispatchCtx {
                        project: &mut local,
                        content_tx: &content_tx,
                        content_state: &state,
                        ui: &mut ui,
                        selection: &mut selection,
                        active_layer: &mut active_layer,
                        user_prefs: &mut prefs,
                        editor_target: None,
                        scrub: &mut scrub,
                    },
                );
                let commands: Vec<_> = content_rx.try_iter().collect();
                eprintln!("[modifier-scrub] commands={}", commands.len());
                if label == "move" {
                    assert_eq!(
                        commands.len(),
                        1,
                        "primary and linked values must be atomic"
                    );
                }
                for command in commands {
                    ct.handle_command(command);
                }
                let layer = &ct.engine.project().unwrap().timeline.layers[layer_idx];
                eprintln!(
                    "[modifier-scrub] value_version={} structure_version={}",
                    layer.generator_graph_version(),
                    layer.generator_graph_structure_version()
                );
                for _ in 0..2 {
                    ct.tick_frame(&state_tx);
                    assert_coupled(&ct);
                }
            }
            for (command, expected) in [
                (ContentCommand::Undo, old_value),
                (ContentCommand::Redo, value),
            ] {
                ct.handle_command(command);
                ct.tick_frame(&state_tx);
                assert_coupled(&ct);
                assert_eq!(
                    ct.engine.project().unwrap().timeline.layers[layer_idx]
                        .gen_params()
                        .unwrap()
                        .get_base_param(&id),
                    expected
                );
            }
            local = ct.engine.project().unwrap().clone();
        }
    }
}
