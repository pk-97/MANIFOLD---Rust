//! TEMP diagnosis harness: PRESET_BROWSER_AUDITION live-audition GPU fault
//! repro (lane/audition-fault-repro). Peter's crash: GENERATORS browser open
//! on the layer tap with a generator layer playing → first faulting command
//! buffer blacklists the queue → exit(70). This module reproduces headlessly
//! and names the faulting preset(s). Diagnosis only — no fixes here.
//!
//! Run each test in its OWN process (the queue blacklist poisons the shared
//! device for every later test in the same process):
//!
//! ```text
//! MANIFOLD_RENDER_TRACE=1 cargo test -p manifold-app \
//!   --features journey-proofs --features gpu-proofs \
//!   audition_fault_full_generator_grid -- --nocapture
//! MANIFOLD_RENDER_TRACE=1 cargo test -p manifold-app \
//!   --features journey-proofs --features gpu-proofs \
//!   audition_fault_each_generator_alone -- --nocapture
//! ```

#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]

use manifold_core::preset_def::PresetKind;
use manifold_core::preset_type_registry;
use manifold_core::types::LayerType;
use manifold_core::{Beats, Bpm, PresetTypeId, Seconds};
use manifold_playback::engine::TickContext;

use crate::headless_harness::headless_content_thread;
use crate::journey_proof::star_field_generator_layer;

const BPM: f32 = 120.0;
const CLIP_BEATS: f64 = 96.0;
const FRAMES: u64 = 300; // 5s @ 60fps — same as p2_audition_trace

/// The D8/§3.4 layer-type gate from `ui_root/dropdowns.rs`, replicated: a
/// preset with `layer_types` set appears only for the listed layer types;
/// `None` invoking type (effect mode) disables the gate. Peter's crash came
/// from a video/generator lane's GENERATORS browser, so gate on Generator.
fn allowed_on_generator_lane(layer_types: &Option<Vec<LayerType>>) -> bool {
    match layer_types {
        None => true,
        Some(list) => list.contains(&LayerType::Generator),
    }
}

/// Every generator the browser would list for a video/generator lane —
/// the full factory list a performer sees at open.
fn browser_generator_ids() -> Vec<PresetTypeId> {
    let ids: Vec<String> = preset_type_registry::available_of_kind(PresetKind::Generator)
        .into_iter()
        .filter(|reg| allowed_on_generator_lane(&reg.layer_types))
        .map(|reg| reg.id.as_str().to_string())
        .collect();
    eprintln!("[audition-fault] generator grid ({} cells): {ids:?}", ids.len());
    ids.into_iter().map(PresetTypeId::from_string).collect()
}

/// Every effect the browser would list (effect mode runs the gate with
/// `None`, i.e. everything available).
fn browser_effect_ids() -> Vec<PresetTypeId> {
    let ids: Vec<String> = preset_type_registry::available_of_kind(PresetKind::Effect)
        .into_iter()
        .map(|reg| reg.id.as_str().to_string())
        .collect();
    eprintln!("[audition-fault] effect grid ({} cells): {ids:?}", ids.len());
    ids.into_iter().map(PresetTypeId::from_string).collect()
}

/// Render size for the headless thread — `AUDITION_REPRO_W/H` override the
/// 320×180 default. A loaded GPU keeps the previous frame's command buffer
/// in flight across an `ensure_cells`, which is the reopen-fault window.
fn repro_resolution() -> (u32, u32) {
    let w = std::env::var("AUDITION_REPRO_W").ok().and_then(|v| v.parse().ok()).unwrap_or(320);
    let h = std::env::var("AUDITION_REPRO_H").ok().and_then(|v| v.parse().ok()).unwrap_or(180);
    eprintln!("[audition-fault] resolution {w}x{h}");
    (w, h)
}

/// Playing StarField generator layer, identical to p2_audition_trace but the
/// tap is the LAYER (Peter's crash path: content_pipeline.rs layer tap).
fn layer_tap_project() -> (manifold_core::project::Project, manifold_core::LayerId) {
    let mut project = manifold_core::project::Project::default();
    project.settings.bpm = Bpm(BPM);
    let mut layer = star_field_generator_layer(0);
    layer.clips[0].duration_beats = Beats(CLIP_BEATS);
    let layer_id = layer.layer_id.clone();
    project.timeline.layers.push(layer);
    (project, layer_id)
}

fn tick_frame(ct: &mut crate::content_thread::ContentThread, frame: u64, dt: f64) {
    let ctx = TickContext {
        dt_seconds: Seconds(dt),
        realtime_now: Seconds(frame as f64 * dt),
        pre_render_dt: Seconds(dt),
        frame_count: frame,
        export_fixed_dt: Seconds::ZERO,
    };
    let tick_result = ct.engine.tick(ctx);
    ct.content_pipeline.render_content(
        &ct.gpu,
        &mut ct.engine,
        &tick_result,
        dt,
        frame,
        false,
        ct.editing_service.data_version(),
    );
    ct.engine.reclaim_tick_result(tick_result);
}

/// Drive `frames` frames, polling the GPU fault registry every frame. Returns
/// (renders_completed, first_fault_frame). Stops early once the driver
/// blacklists the queue — every later buffer is "Ignored" noise.
fn drive_and_watch(
    ct: &mut crate::content_thread::ContentThread,
    frames: u64,
    label: &str,
) -> (u64, Option<u64>) {
    let mut first_fault_frame = None;
    let dt = 1.0 / 60.0;
    for frame in 0..frames {
        tick_frame(ct, frame, dt);
        if manifold_gpu::gpu_fault::fault_count() > 0 && first_fault_frame.is_none() {
            first_fault_frame = Some(frame);
            eprintln!(
                "[audition-fault] {label}: FIRST fault observed after frame {frame} \
                 (fault_count={}, ignored={})",
                manifold_gpu::gpu_fault::fault_count(),
                manifold_gpu::gpu_fault::submissions_ignored()
            );
        }
        if manifold_gpu::gpu_fault::submissions_ignored() {
            eprintln!(
                "[audition-fault] {label}: queue blacklisted at frame {frame} — stopping"
            );
            break;
        }
    }
    (
        ct.content_pipeline.audition_renders_completed(),
        first_fault_frame,
    )
}

/// Peter's crash shape, headless: full GENERATORS grid on the layer tap of a
/// playing generator layer. A fault here names the session; the singles loop
/// below attributes it to a preset.
#[test]
fn audition_fault_full_generator_grid() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
    let (project, layer_id) = layer_tap_project();
    let res = repro_resolution();
    let mut ct = headless_content_thread(project, res.0, res.1);

    let items: Vec<(PresetTypeId, PresetKind)> = browser_generator_ids()
        .into_iter()
        .map(|id| (id, PresetKind::Generator))
        .collect();
    let tap = if std::env::var("AUDITION_REPRO_TAP").as_deref() == Ok("master") {
        eprintln!("[audition-fault] tap: MASTER");
        manifold_renderer::audition::AuditionTapTarget::Master
    } else {
        manifold_renderer::audition::AuditionTapTarget::Layer(layer_id.clone())
    };
    ct.content_pipeline
        .audition_ensure_cells(items.clone(), tap);
    ct.content_pipeline
        .audition_set_render_list(items.iter().map(|(id, _)| id.clone()).collect());

    // The layer tap needs the layer's scratch texture live; if it never
    // appears the cells silently render against the black fallback and this
    // run proves nothing about the tap path.
    ct.engine.play();
    tick_frame(&mut ct, 0, 1.0 / 60.0);
    let scratch = ct.content_pipeline.layer_scratch_texture(layer_id.as_str());
    eprintln!(
        "[audition-fault] layer scratch texture after frame 0: {}",
        scratch.is_some()
    );

    let (rendered, fault_frame) = drive_and_watch(&mut ct, FRAMES - 1, "full-generator-grid");
    eprintln!(
        "[audition-fault] full grid: rendered={rendered} first_fault_frame={fault_frame:?} \
         fault_count={} ignored={}",
        manifold_gpu::gpu_fault::fault_count(),
        manifold_gpu::gpu_fault::submissions_ignored()
    );
}

/// Attribution: one generator cell at a time on the layer tap. The first
/// preset whose solo run faults is the culprit. Run in its own process —
/// a real fault blacklists the queue for everything after it.
#[test]
fn audition_fault_each_generator_alone() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
    let ids = browser_generator_ids();
    let res = repro_resolution();
    for id in &ids {
        let (project, layer_id) = layer_tap_project();
        let mut ct = headless_content_thread(project, res.0, res.1);
        let items = vec![(id.clone(), PresetKind::Generator)];
        ct.content_pipeline
            .audition_ensure_cells(items.clone(), manifold_renderer::audition::AuditionTapTarget::Layer(layer_id.clone()));
        ct.content_pipeline
            .audition_set_render_list(items.iter().map(|(id, _)| id.clone()).collect());
        ct.engine.play();
        let (rendered, fault_frame) = drive_and_watch(&mut ct, 40, id.as_str());
        eprintln!(
            "[audition-fault] solo {id}: rendered={rendered} fault={fault_frame:?}"
        );
        if fault_frame.is_some() || manifold_gpu::gpu_fault::submissions_ignored() {
            panic!("[audition-fault] CULPRIT: {id} faulted at {fault_frame:?}");
        }
    }
}

/// Widen step: every generator AND every effect in one grid on the layer tap
/// (the mixed grid the browser never shows, but it maximizes atlas pressure
/// and cross-preset interaction).
#[test]
fn audition_fault_mixed_grid_layer_tap() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
    let (project, layer_id) = layer_tap_project();
    let res = repro_resolution();
    let mut ct = headless_content_thread(project, res.0, res.1);

    let mut items: Vec<(PresetTypeId, PresetKind)> = browser_generator_ids()
        .into_iter()
        .map(|id| (id, PresetKind::Generator))
        .collect();
    items.extend(browser_effect_ids().into_iter().map(|id| (id, PresetKind::Effect)));
    eprintln!("[audition-fault] mixed grid: {} cells", items.len());
    ct.content_pipeline
        .audition_ensure_cells(items.clone(), manifold_renderer::audition::AuditionTapTarget::Layer(layer_id));
    ct.content_pipeline
        .audition_set_render_list(items.iter().map(|(id, _)| id.clone()).collect());

    ct.engine.play();
    let (rendered, fault_frame) = drive_and_watch(&mut ct, FRAMES, "mixed-grid");
    eprintln!(
        "[audition-fault] mixed grid: rendered={rendered} first_fault_frame={fault_frame:?}"
    );
}

/// Mechanism bisect for the ParticleText page fault. Renders the preset's
/// standalone runtime directly (the same build `AuditionPool::build_cell`
/// uses) with per-variant param overrides and an env-selected frame cadence.
/// `MODE` env: `wait` (commit_and_wait each frame — isolates graph-internal
/// faults from in-flight races) or `async` (fire-and-forget, the real
/// audition shape). Variants each get a FRESH runtime; the queue blacklist
/// poisons everything after a fault, so one variant per process run via
/// `VARIANT` env.
#[test]
fn audition_fault_particletext_bisect() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
    use manifold_core::params::{Param, ParamManifest};

    let mode = std::env::var("BISECT_MODE").unwrap_or_else(|_| "wait".into());
    let variant = std::env::var("BISECT_VARIANT").unwrap_or_else(|_| "default".into());
    let frames: u64 = std::env::var("BISECT_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    eprintln!("[bisect] mode={mode} variant={variant} frames={frames}");

    // Same construction as AuditionPool::build_cell's generator arm.
    let device = std::sync::Arc::new(manifold_gpu::GpuDevice::new());
    let registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    let view = manifold_renderer::node_graph::loaded_preset_view_by_id(
        &PresetTypeId::from_string("ParticleText".to_string()),
    )
    .expect("ParticleText view");
    let mut runtime = manifold_renderer::preset_runtime::PresetRuntime::from_def_with_device(
        (*view.canonical_def).clone(),
        &registry,
        std::sync::Arc::clone(&device),
        manifold_renderer::audition::CELL_W,
        manifold_renderer::audition::CELL_H,
        manifold_gpu::GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("runtime builds");
    let target = manifold_renderer::render_target::RenderTarget::new(
        &device,
        manifold_renderer::audition::CELL_W,
        manifold_renderer::audition::CELL_H,
        manifold_gpu::GpuTextureFormat::Rgba16Float,
        "bisect-target",
    );

    // Variant param overrides on the OUTER card ids (applied through the
    // manifest exactly like a committed card would carry them).
    let overrides: &[(&str, f32)] = match variant.as_str() {
        "default" => &[],
        "low_count" => &[("count_m", 0.1)],
        "no_text" => &[("text_strength", 0.0)],
        "no_turbulence" => &[("turbulence", 0.0)],
        "no_force" => &[("force", 0.0)],
        "no_flow" => &[("flow", -0.1)],
        "min_fill" => &[("fill", 0.1)],
        other => panic!("unknown BISECT_VARIANT {other}"),
    };
    let manifest = {
        let base = (*view.canonical_def).clone();
        let meta = base.preset_metadata.as_ref().expect("ParticleText metadata");
        let mut params = Vec::new();
        let mut specs: Vec<(String, f32)> = Vec::new();
        for p in &meta.params {
            specs.push((p.id.clone(), p.default_value));
        }
        for (id, v) in overrides {
            specs.retain(|(sid, _)| sid != id);
            specs.push((id.to_string(), *v));
        }
        // Param::explicit needs a ParamSpecDef — rebuild minimal specs from
        // the def's own metadata so ranges/defaults stay honest.
        for p in &meta.params {
            let value = specs.iter().find(|(sid, _)| sid == &p.id).map(|(_, v)| *v).unwrap_or(p.default_value);
            let spec = manifold_core::effect_graph_def::ParamSpecDef {
                id: p.id.clone(),
                name: p.name.clone(),
                min: p.min,
                max: p.max,
                default_value: value,
                whole_numbers: p.whole_numbers,
                is_toggle: p.is_toggle,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: None,
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
                card_visible: true,
            };
            params.push(Param::bundled(spec));
        }
        ParamManifest::from_params(params)
    };

    let ctx = manifold_renderer::preset_context::PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: manifold_renderer::audition::CELL_W,
        height: manifold_renderer::audition::CELL_H,
        output_width: 1920,
        output_height: 1080,
        aspect: 16.0 / 9.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    let mut first_fault = None;
    for frame in 0..frames {
        let mut ctx = ctx.clone();
        ctx.time = frame as f64 / 60.0;
        ctx.beat = frame as f64 / 30.0;
        ctx.frame_count = frame as i64;
        let mut enc = device.create_encoder("bisect");
        {
            let mut gpu = manifold_renderer::gpu_encoder::GpuEncoder::new(&mut enc, &device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        if mode == "wait" {
            enc.commit_and_wait_completed();
        } else {
            enc.commit();
        }
        if manifold_gpu::gpu_fault::fault_count() > 0 && first_fault.is_none() {
            first_fault = Some(frame);
            eprintln!(
                "[bisect] FIRST fault at frame {frame} (count={} ignored={})",
                manifold_gpu::gpu_fault::fault_count(),
                manifold_gpu::gpu_fault::submissions_ignored()
            );
        }
        if manifold_gpu::gpu_fault::submissions_ignored() {
            break;
        }
    }
    eprintln!(
        "[bisect] variant={variant} first_fault={first_fault:?} total_faults={}",
        manifold_gpu::gpu_fault::fault_count()
    );
}

/// Does the ParticleText fault need the concurrent generator command buffer?
/// Same solo-cell shape as `audition_fault_each_generator_alone` but with an
/// EMPTY timeline — the Compositor CB carries only the audition cell, no
/// 'Generators' CB exists. `SOLO_LAYER=1` restores the StarField layer
/// (control — must fault).
#[test]
fn audition_fault_particletext_needs_layer() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
    let with_layer = std::env::var("SOLO_LAYER").is_ok();
    let mut project = manifold_core::project::Project::default();
    project.settings.bpm = Bpm(BPM);
    let mut tap = manifold_renderer::audition::AuditionTapTarget::Master;
    if with_layer {
        let mut layer = star_field_generator_layer(0);
        layer.clips[0].duration_beats = Beats(CLIP_BEATS);
        tap = manifold_renderer::audition::AuditionTapTarget::Layer(layer.layer_id.clone());
        project.timeline.layers.push(layer);
    }
    let res = repro_resolution();
    let mut ct = headless_content_thread(project, res.0, res.1);
    let id = PresetTypeId::from_string("ParticleText".to_string());
    let items = vec![(id.clone(), PresetKind::Generator)];
    ct.content_pipeline.audition_ensure_cells(items.clone(), tap);
    ct.content_pipeline
        .audition_set_render_list(items.iter().map(|(id, _)| id.clone()).collect());
    ct.engine.play();
    let (rendered, fault) = drive_and_watch(&mut ct, 40, "pt-needs-layer");
    eprintln!(
        "[audition-fault] with_layer={with_layer} rendered={rendered} fault={fault:?} \
         faults={} ignored={}",
        manifold_gpu::gpu_fault::fault_count(),
        manifold_gpu::gpu_fault::submissions_ignored()
    );
}

/// Reopen shape — the BUG-rnnr class (realloc textures under in-flight
/// frames). First browser open allocates a fresh pool (nothing in flight can
/// reference it), but CLOSE + REOPEN calls `ensure_cells` again, which drops
/// every cell's PresetRuntime (feedback/particle textures), each gen target,
/// and — when the item count changes — the atlas AND the audition surface
/// texture, all while the previous frame's ASYNC command buffer (which
/// sampled/blitted/wrote exactly those textures) is still executing. No
/// retire-before-release anywhere: `GpuTexture` is a plain `Retained` drop.
///
/// `same_size`: reopen with an equal cell count (no atlas realloc — isolates
/// the cell-runtime/gen-target drop) vs a different count (adds atlas +
/// surface realloc).
#[test]
fn audition_fault_reopen_during_playback() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
    let (project, layer_id) = layer_tap_project();
    let res = repro_resolution();
    let mut ct = headless_content_thread(project, res.0, res.1);
    ct.engine.play();

    let all: Vec<(PresetTypeId, PresetKind)> = browser_generator_ids()
        .into_iter()
        .map(|id| (id, PresetKind::Generator))
        .collect();
    let subset: Vec<(PresetTypeId, PresetKind)> = all.iter().take(4).cloned().collect();
    let same_size: Vec<(PresetTypeId, PresetKind)> = all.iter().take(27.min(all.len())).cloned().collect();
    // Different list, same cell count as the first open (27): no atlas
    // realloc, but every cell runtime + gen target is dropped and rebuilt.
    let same_count_permuted: Vec<(PresetTypeId, PresetKind)> = {
        let mut v = same_size.clone();
        v.rotate_left(1);
        v
    };

    // Open 1: full grid.
    ct.content_pipeline
        .audition_ensure_cells(all.clone(), manifold_renderer::audition::AuditionTapTarget::Layer(layer_id.clone()));
    ct.content_pipeline
        .audition_set_render_list(all.iter().map(|(id, _)| id.clone()).collect());
    let (r1, f1) = drive_and_watch(&mut ct, 30, "open-1-full-grid");
    eprintln!("[audition-fault] open 1: rendered={r1} fault={f1:?}");

    // Close (browser close = empty render list; pool is KEPT).
    ct.content_pipeline.audition_set_render_list(Vec::new());
    drive_and_watch(&mut ct, 5, "closed");

    // Reopen A: different cell count (27 → 4) → atlas + surface realloc
    // PLUS cell runtime/gen-target drops — immediately after a commit, so
    // the last grid frame's command buffer is still in flight.
    ct.content_pipeline
        .audition_ensure_cells(subset.clone(), manifold_renderer::audition::AuditionTapTarget::Layer(layer_id.clone()));
    ct.content_pipeline
        .audition_set_render_list(subset.iter().map(|(id, _)| id.clone()).collect());
    let (r2, f2) = drive_and_watch(&mut ct, 30, "reopen-A-smaller-grid");
    eprintln!("[audition-fault] reopen A (atlas realloc): rendered={r2} fault={f2:?}");

    // Close again.
    ct.content_pipeline.audition_set_render_list(Vec::new());
    drive_and_watch(&mut ct, 5, "closed-2");

    // Reopen B: same cell count, permuted order → no atlas realloc (only
    // cell runtime/gen-target drops under in-flight frames).
    ct.content_pipeline
        .audition_ensure_cells(same_count_permuted.clone(), manifold_renderer::audition::AuditionTapTarget::Layer(layer_id.clone()));
    ct.content_pipeline.audition_set_render_list(
        same_count_permuted.iter().map(|(id, _)| id.clone()).collect(),
    );
    let (r3, f3) = drive_and_watch(&mut ct, 30, "reopen-B-same-size");
    eprintln!("[audition-fault] reopen B (same size): rendered={r3} fault={f3:?}");

    eprintln!(
        "[audition-fault] reopen sweep done: fault_count={} ignored={}",
        manifold_gpu::gpu_fault::fault_count(),
        manifold_gpu::gpu_fault::submissions_ignored()
    );
}
