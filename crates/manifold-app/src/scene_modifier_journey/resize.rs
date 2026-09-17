//! Resize failure keeps the real content pipeline and undo history usable.
use super::*;

#[test]
fn resolution_resize_is_atomic_and_retryable() {
    let project = math_view_project();
    let original = (project.settings.output_width as u32, project.settings.output_height as u32);
    let mut ct = headless_content_thread(Project::default(), original.0, original.1);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    ct.handle_command(ContentCommand::LoadProject(Box::new(project)));
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &state_tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    ct.tick_frame(&state_tx);
    assert!(ct.graph_edit_diagnostic.is_none(), "initial project rejected: {:?}", ct.graph_edit_diagnostic);
    let before_version = ct.editing_service.data_version();
    let before_undo = ct.editing_service.peek_undo_description().map(str::to_owned);
    let before_redo = ct.editing_service.peek_redo_description().map(str::to_owned);
    let before_settings = ct.engine.project().unwrap().settings.clone();
    let before = ct.content_pipeline.export_output_texture().clone();

    // 8 TB for just one RGBA16 target: rejected by the existing memory budget
    // before Metal sees the oversized descriptor. No global failure flag required.
    ct.handle_command(ContentCommand::SetDisplayResolution(1_000_000, 1_000_000));
    assert!(ct.graph_edit_diagnostic.is_some(), "oversized resize must report rejection");
    assert_eq!(ct.content_pipeline.dimensions(), original);
    assert!(ct.content_pipeline.export_output_texture().ptr_eq(&before));
    assert_eq!(ct.editing_service.data_version(), before_version);
    assert_eq!(ct.editing_service.peek_undo_description(), before_undo.as_deref());
    assert_eq!(ct.editing_service.peek_redo_description(), before_redo.as_deref());
    let settings = &ct.engine.project().unwrap().settings;
    assert_eq!((settings.output_width, settings.output_height, settings.render_scale),
        (before_settings.output_width, before_settings.output_height, before_settings.render_scale));
    ct.tick_frame(&state_tx);
    let desired = (240, 160);
    ct.handle_command(ContentCommand::SetDisplayResolution(desired.0 as i32, desired.1 as i32));
    assert_eq!(ct.content_pipeline.dimensions(), desired, "valid resize failed: {:?}", ct.graph_edit_diagnostic);
    ct.tick_frame(&state_tx);
    ct.handle_command(ContentCommand::Undo);
    assert_eq!(ct.content_pipeline.dimensions(), original);
    ct.tick_frame(&state_tx);
    ct.handle_command(ContentCommand::Redo);
    assert_eq!(ct.content_pipeline.dimensions(), desired);
    ct.tick_frame(&state_tx);
    ct.handle_command(ContentCommand::SetRenderScale(0.5));
    assert_eq!(ct.engine.project().unwrap().settings.render_scale, 0.5);
    assert_eq!(ct.content_pipeline.dimensions(), desired);
    ct.tick_frame(&state_tx);
    ct.handle_command(ContentCommand::Undo);
    assert_eq!(ct.engine.project().unwrap().settings.render_scale, 1.0);
    ct.tick_frame(&state_tx);
    let device = ct.content_pipeline.native_gpu_for_tests().unwrap();
    let encoder = device.create_encoder("resize-acceptance-completion");
    encoder.try_commit_and_wait_completed().expect("resize GPU completion");
    eprintln!("resize acceptance passed: {original:?} -> rejected oversized -> {desired:?} -> undo -> redo");
}
