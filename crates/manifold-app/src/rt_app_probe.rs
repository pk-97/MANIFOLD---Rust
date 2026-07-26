//! App-path RT probe: load a saved .manifold through the REAL
//! ContentCommand::LoadProject path (same as the winit app's File→Open).
//!
//! Builds a ContentThread with an empty project (simulating app startup),
//! then sends ContentCommand::LoadProject via handle_command — exactly the
//! same code path the winit app uses when the user opens a project.
//!
//! Ticks 120 frames. Reports whether RT dispatch_shadow_rays fires.
//!
//! Disposable, temporary — delete after the bug is fixed.
//!
//! Usage:
//!   cargo run --features perf-soak --bin manifold -- manifold rt-app-probe <project.manifold>

use std::sync::atomic::Ordering;

use manifold_renderer::node_graph::primitives::{
    RT_PROBE_ACCEL_BUILT, RT_PROBE_BUILD_COUNT, RT_PROBE_BUILD_ENQUEUED,
    RT_PROBE_DISPATCH_COUNT, RT_PROBE_DISPATCH_FIRED, RT_PROBE_ENABLED,
    RT_PROBE_HAS_CASTERS, RT_PROBE_PENDING_KEY,
    RT_PROBE_RT_READY, RT_PROBE_TOPO_KEY, RT_PROBE_UNIFORM_SCENE_W,
};

use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

const TOTAL_FRAMES: u32 = 120;
const SAMPLE_FRAMES: &[u32] = &[0, 1, 2, 3, 5, 10, 30, 60, 90, 119];

/// Entry point — never returns.
pub fn run(args: &[String]) -> ! {
    // Enable render_scene probe statics and env-gated probes.
    RT_PROBE_ENABLED.store(true, Ordering::Relaxed);
    // SAFETY: disposable diagnostic — safe single-threaded access at startup.
    unsafe { std::env::set_var("MANIFOLD_RT_PROBE", "1"); }

    let project_path = match args.get(1) {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            eprintln!("usage: manifold rt-app-probe <project.manifold>");
            std::process::exit(2);
        }
    };

    if !project_path.exists() {
        eprintln!("project not found: {}", project_path.display());
        std::process::exit(1);
    }

    println!("=== RT APP-PATH PROBE (real LoadProject cmd) ===");
    println!("path: {}", project_path.display());

    // ── Step 1: Load the real project (via load_project_with, like the app does) ──
    let real_project = match manifold_io::loader::load_project_with(
        &project_path,
        crate::project_io::install_embedded_presets,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAILED to load project: {e}");
            std::process::exit(1);
        }
    };

    let frame_rate = real_project.settings.frame_rate as f64;
    let w = real_project.settings.output_width.max(1) as u32;
    let h = real_project.settings.output_height.max(1) as u32;

    println!("bpm={} output={w}x{h} fps={frame_rate}", real_project.settings.bpm.0);
    println!("layers in project: {}", real_project.timeline.layers.len());

    // Print layer manifest BEFORE sending to content thread.
    for (i, layer) in real_project.timeline.layers.iter().enumerate() {
        let gp = layer.gen_params();
        let gen_type = gp.map(|g| g.generator_type().clone());
        let m = gp.map(|g| &g.params);
        let rt_en = m.and_then(|m| m.get("8_rt_enabled")).map(|p| p.value);
        let rt_ref = m.and_then(|m| m.get("8_rt_reflections")).map(|p| p.value);
        let rt_tu = m.and_then(|m| m.get("8_temporal_upscale")).map(|p| p.value);
        println!("  layer[{i}] type={:?} rt_en={rt_en:?} rt_ref={rt_ref:?} tu={rt_tu:?}",
            gen_type.as_ref().map(|t| t.as_str()));
    }

    // ── Step 2: Install the project's embedded presets into the catalog ──
    // The app also does this via `install_project_preset_overlay` in
    // `apply_project_io_action`, which runs BEFORE sending LoadProject.
    // We loaded with install_embedded_presets hook, which set the overlay.
    println!("  embedded presets installed via load_project_with hook");

    // ── Step 3: Build an EMPTY ContentThread (like winit app startup) ──
    println!();
    println!("=== Building ContentThread (empty project, like app startup) ===");

    // Create a minimal empty project for the ContentThread's initial engine.
    // This mirrors how the winit app initializes with a default project
    // before any user project load.
    let empty_project = manifold_core::project::Project::default();
    let mut ct = headless_content_thread(empty_project, w, h);
    ct.timer.set_target_fps(frame_rate);
    crate::content_thread::apply_realtime_thread_policy(frame_rate);

    // ── Step 4: Send LoadProject via handle_command (same as app File→Open) ──
    println!();
    println!("=== Sending ContentCommand::LoadProject (real app path) ===");
    ct.handle_command(ContentCommand::LoadProject(Box::new(real_project)));

    // ── Step 5: Start playback ──
    ct.handle_command(ContentCommand::Play);

    // ── Step 6: Tick 120 frames ──
    println!();
    println!("=== Ticking {TOTAL_FRAMES} frames ===");

    let (state_tx, state_rx) = crossbeam_channel::unbounded::<crate::content_state::ContentState>();
    let drain = std::thread::Builder::new()
        .name("rt-app-probe-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .expect("spawn drain thread");

    let mut frame_idx: u32 = 0;
    while frame_idx < TOTAL_FRAMES {
        ct.timer.wait_for_deadline();
        ct.tick_frame(&state_tx);

        if SAMPLE_FRAMES.contains(&frame_idx) {
            print_frame(&ct, frame_idx);
        }
        frame_idx += 1;
    }

    drop(state_tx);
    drain.join().expect("drain join");

    // ── Final report ──
    println!();
    println!("=== FINAL REPORT ===");
    let fired = RT_PROBE_DISPATCH_FIRED.load(Ordering::Relaxed);
    if fired {
        println!("  VERDICT: RT dispatch_shadow_rays EXECUTED. App-path LoadProject cmd is sound.");
    } else {
        let rt_ready = RT_PROBE_RT_READY.load(Ordering::Relaxed);
        let has_casters = RT_PROBE_HAS_CASTERS.load(Ordering::Relaxed);
        let scene_w = RT_PROBE_UNIFORM_SCENE_W.load(Ordering::Relaxed);
        let enqueued = RT_PROBE_BUILD_ENQUEUED.load(Ordering::Relaxed);
        println!("  VERDICT: RT NEVER dispatched after {TOTAL_FRAMES} frames.");
        println!("    rt_ready={rt_ready} has_casters={has_casters} scene_w={scene_w} build_enqueued={enqueued}");
        if !has_casters {
            println!("    BLOCKED: has_casters=false — no lights cast shadows.");
        } else if !rt_ready {
            println!("    BLOCKED: rt_ready=false — accel never latched ready.");
            println!("    pending_key=0x{:x} topo_key=0x{:x}",
                RT_PROBE_PENDING_KEY.load(Ordering::Relaxed),
                RT_PROBE_TOPO_KEY.load(Ordering::Relaxed));
        }
    }

    std::process::exit(0);
}

fn print_frame(ct: &crate::content_thread::ContentThread, frame: u32) {
    println!();
    println!("  ----- Frame {frame} -----");

    if let Some(proj) = ct.engine.project() {
        let gen_layers: usize = proj.timeline.layers.iter()
            .filter(|l| l.gen_params().is_some())
            .count();
        println!("  layers_with_generators={gen_layers}");
    }

    // render_scene probe statics.
    let r = RT_PROBE_ACCEL_BUILT.load(Ordering::Relaxed);
    let rd = RT_PROBE_RT_READY.load(Ordering::Relaxed);
    let c = RT_PROBE_HAS_CASTERS.load(Ordering::Relaxed);
    let sw = RT_PROBE_UNIFORM_SCENE_W.load(Ordering::Relaxed);
    let d = RT_PROBE_DISPATCH_COUNT.load(Ordering::Relaxed);
    let b = RT_PROBE_BUILD_COUNT.load(Ordering::Relaxed);
    let be = RT_PROBE_BUILD_ENQUEUED.load(Ordering::Relaxed);
    let df = RT_PROBE_DISPATCH_FIRED.load(Ordering::Relaxed);
    let pk = RT_PROBE_PENDING_KEY.load(Ordering::Relaxed);
    let tk = RT_PROBE_TOPO_KEY.load(Ordering::Relaxed);
    println!("  render_scene: rt_accel_built={r} rt_ready={rd} has_casters={c}");
    println!("  uniform: scene_w={sw} dispatch_ever={df}");
    println!("  dispatch_count={d} builds={b} enqueued={be}");
    println!("  topo_key=0x{tk:x} pending_key=0x{pk:x}");
}
