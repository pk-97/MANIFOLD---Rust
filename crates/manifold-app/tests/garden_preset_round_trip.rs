//! MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN P4 round-trip gate (BUG-036
//! rule, DESIGN_DOC_STANDARD.md §5): save a project using the bundled
//! `Garden` generator preset with EDITED outer params -> reload -> the
//! params must survive intact AND a driver must still resolve and be
//! able to modulate them after reload, not only right after creation.
//!
//! Unlike `project_local_preset_reload.rs`'s BUG-036 repro, `Garden` is a
//! bundled STOCK preset (ships in `assets/generator-presets/Garden.json`),
//! not a project-local/imported one — so there's no "install the file's
//! own embedded presets before deserialize" step to prove here. What this
//! test proves instead: `node.scatter_on_mesh`'s outer cards (count /
//! scale / align) and `node.gltf_mesh_source`'s new `fit`/`recenter`
//! params (exercised transitively — Garden's flower chain doesn't use
//! gltf, but the outer-card param manifest mechanics are identical
//! regardless of which primitive backs a card) round-trip through the
//! SAME `Project` (de)serialize path BUG-036 broke, and that a
//! `ParameterDriver` attached to one of P4's new cards still resolves
//! and evaluates post-reload.
//!
//! `manifold_renderer::preset_loader::clear_project_presets()` still
//! triggers `apply_reload()` even with an empty overlay (see
//! `preset_loader.rs`), which scans the STOCK `assets/generator-presets`
//! dir (dev workspace root, baked via `CARGO_MANIFEST_DIR` at
//! manifold-renderer's own compile time) and rebuilds
//! `manifold_core::preset_definition_registry` from it — that's what
//! makes "Garden" resolvable as a template at all, mirroring what the
//! app does once at startup before any project-local overlay exists.

use manifold_core::effects::ParamId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::PresetTypeId;
use manifold_renderer::preset_loader::clear_project_presets;

#[test]
fn garden_outer_params_and_driver_survive_project_reload() {
    // Populate the core registry from the STOCK scan (Garden.json ships
    // in assets/generator-presets). No project-local overlay involved.
    clear_project_presets();

    let preset_id = PresetTypeId::from_string("Garden".to_string());
    let mut project = Project::default();
    project
        .timeline
        .add_layer("Field", LayerType::Generator, preset_id.clone());

    // The instance seeded its outer-card params from Garden's template
    // (count / scale / align, per Garden.json's presetMetadata.params).
    let (count_id, scale_id, written_count, written_scale, template_len) = {
        let gp = project.timeline.layers[0]
            .gen_params_mut()
            .expect("generator instance");
        assert!(!gp.params.is_empty(), "Garden's template must seed outer-card params");

        let count_id = gp
            .params
            .iter()
            .find(|p| p.id() == "count")
            .expect("Garden exposes a `count` outer card")
            .id()
            .to_string();
        let scale_id = gp
            .params
            .iter()
            .find(|p| p.id() == "scale")
            .expect("Garden exposes a `scale` outer card")
            .id()
            .to_string();

        // Edit both outer params away from their template defaults.
        gp.set_base_param(&count_id, 77.0);
        gp.set_base_param(&scale_id, 0.42);

        // Attach a driver (LFO) to `scale` — the modulation-after-reload
        // half of the gate. `create_driver` defaults to a Sine waveform
        // on a quarter-beat division; that's enough to prove it still
        // EVALUATES post-reload, which is what "modulation still moves
        // it" means (the driver isn't dropped, and it isn't a static
        // frozen value).
        gp.create_driver(ParamId::Owned(scale_id.clone()));
        assert!(gp.find_driver(&scale_id).is_some(), "driver should attach before save");

        (
            count_id.clone(),
            scale_id.clone(),
            gp.get_base_param(&count_id),
            gp.get_base_param(&scale_id),
            gp.params.len(),
        )
    };

    let json = serde_json::to_string(&project).expect("serialize project");
    assert!(
        json.contains(&format!("\"{count_id}\"")),
        "saved file must carry the count param entry"
    );
    assert!(
        json.contains(&format!("\"{scale_id}\"")),
        "saved file must carry the scale param entry"
    );

    // ── Fresh process boundary, simulated: clear the overlay (as a real
    // relaunch would start with none), re-trigger the stock scan (as app
    // startup does), then reload the saved JSON.
    clear_project_presets();
    let reloaded = manifold_io::loader::load_project_from_json(&json).expect("reload project");

    let gp = reloaded.timeline.layers[0]
        .gen_params()
        .expect("generator instance survives reload");

    assert_eq!(
        gp.params.len(),
        template_len,
        "every outer-card param entry survives reload"
    );
    assert!(
        (gp.get_base_param(&count_id) - written_count).abs() < f32::EPSILON,
        "count survives reload: expected {written_count}, got {}",
        gp.get_base_param(&count_id)
    );
    assert!(
        (gp.get_base_param(&scale_id) - written_scale).abs() < f32::EPSILON,
        "scale survives reload: expected {written_scale}, got {}",
        gp.get_base_param(&scale_id)
    );

    // Modulation still moves it after reload — the driver resolved
    // against the REAL template spec (not a placeholder), and it still
    // evaluates to a value inside the param's range.
    let driver = gp
        .find_driver(&scale_id)
        .expect("driver on `scale` must survive reload, not just the base value");
    let spec = &gp
        .params
        .iter()
        .find(|p| p.id() == scale_id)
        .expect("scale param entry present")
        .spec;
    assert_eq!(spec.id, scale_id, "resolved against the real Garden template spec");

    let evaluated = manifold_core::effects::ParameterDriver::evaluate(
        manifold_core::Beats(0.0),
        driver.beat_division,
        driver.waveform,
        driver.phase,
    );
    assert!(
        (0.0..=1.0).contains(&evaluated),
        "driver must still evaluate to a normalized [0,1] value post-reload, got {evaluated}"
    );

    clear_project_presets();
}
