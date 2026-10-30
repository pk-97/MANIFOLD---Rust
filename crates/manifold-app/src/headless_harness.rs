//! A headless [`ContentThread`]: the app's own engine and content thread
//! constructors (`content_thread::playback_engine`, `ContentThread::new`)
//! on the test GPU device, minus every UI-surface concern (no window, no
//! preview bridges, no MIDI device open, no LED controller, no OSC
//! listening). `perf-soak`, the journey proofs and the app's GPU regression
//! tests boot through it.

use manifold_core::project::Project;

use crate::content_pipeline::ContentPipeline;
use crate::content_thread::ContentThread;

/// Build a minimal but real headless [`ContentThread`]. See the module doc
/// for what's real vs. inert-defaulted.
pub(crate) fn headless_content_thread(project: Project, w: u32, h: u32) -> ContentThread {
    // Light unit tests must not wait behind proof runs; proofs queue.
    let native_device = manifold_node_engine::gpu::context::test_gpu_device("app headless harness");
    // BUG-olp9: load the same pipeline caches the GUI path loads (app.rs).
    // Without them every headless run cold-compiles all ~150+ pipelines, and
    // each runtime MSL compile leaves slab-scale IOAccelerator allocations —
    // the dominant term in the 15-25GB startup spike that panicked the
    // machine when three lane renders ran concurrently (2026-08-14).
    manifold_gpu::load_disk_shader_caches(&native_device);
    let engine = crate::content_thread::playback_engine(&native_device, project, w, h);
    let mut content_pipeline = ContentPipeline::new(Box::new(
        manifold_compositor::layer_compositor::LayerCompositor::new(&native_device, w, h),
    ));
    content_pipeline.set_native_gpu(native_device);
    ContentThread::new(
        engine,
        content_pipeline,
        // Unqueued unless the renderer is built with gpu-proofs.
        manifold_node_engine::gpu::context::GpuContext::new_for_tests("app headless harness"),
        60.0,
    )
}

/// Follow `install_project` with the production loop's bounded warmup;
/// installing alone skips the asynchronous GLB decode.
#[cfg(all(test, feature = "journey-proofs"))]
pub(crate) fn warm_project(ct: &mut ContentThread, state_tx: &crate::content_state::StateTx) {
    let (_command_tx, command_rx) = crossbeam_channel::unbounded();
    ct.run_warmup(&command_rx, state_tx);
}

/// Set one generator param through the real undoable command.
#[cfg(all(test, feature = "journey-proofs"))]
pub(crate) fn set_generator_param(
    ct: &mut ContentThread,
    layer_id: &manifold_core::LayerId,
    param_id: &str,
    value: f32,
) {
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let old = ct
        .engine
        .project()
        .expect("journey project")
        .preset_instance(&target)
        .and_then(|instance| {
            instance
                .params
                .contains(param_id)
                .then(|| instance.get_base_param(param_id))
        })
        .unwrap_or_else(|| panic!("missing generator parameter {param_id}"));
    assert!(
        !ct.handle_command(crate::content_command::ContentCommand::Execute(Box::new(
            manifold_editing::commands::effects::ChangeGraphParamCommand::new(
                target,
                param_id.to_string(),
                old,
                value
            ),
        )))
    );
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "parameter edit rejected"
    );
}
