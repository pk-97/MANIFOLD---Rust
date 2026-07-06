//! BUG-036 regression (dead-LFO-on-reload): params of a project-local
//! (imported / forked) preset must survive project reload.
//!
//! The V1.4 param loader resolves each instance's params against the global
//! preset-definition registry while `Project` deserializes. A project-local
//! preset's template only enters that registry via the catalog overlay — and
//! the app used to install the overlay AFTER deserialize, so every param
//! keyed to a project-local preset type resolved to "no template" and was
//! dropped (LFO/driver targets vanished; re-importing the same .glb revived
//! them because import registers the template first).
//!
//! Two independent defenses are proven here:
//! 1. Ordering (root fix): `load_project_from_json_with` hands the file's
//!    embedded presets to an installer BEFORE the typed deserialize.
//! 2. Keep-don't-drop (class-kill backstop): even with NO template
//!    resolvable, `build_param_manifest` keeps the entry on a placeholder
//!    spec instead of silently losing state.
//!
//! Own integration-test binary (own process): it mutates the process-global
//! preset catalog via `set_project_presets`, which would race other
//! catalog-reading tests in a shared binary. Keep any future tests here
//! behind a shared lock (see `project_preset_overlay.rs` in the renderer).

use manifold_core::PresetTypeId;
use manifold_core::preset_def::PresetKind;
use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};
use manifold_core::types::LayerType;
use manifold_renderer::preset_loader::{clear_project_presets, set_project_presets};

/// Test-side mirror of the app's `install_embedded_presets` glue (manifold-app
/// is bin-only, so the 5-line loop isn't linkable from an integration test):
/// serialize each embedded def and install it into the overlay, which rebuilds
/// the catalog AND the core definition registry.
fn install(presets: &[EmbeddedPreset]) {
    let mut effect = Vec::new();
    let mut generator = Vec::new();
    for p in presets {
        let Some(id) = p.id() else { continue };
        let json = serde_json::to_string(&p.def).expect("serialize embedded def");
        match p.kind {
            PresetKind::Effect => effect.push((id.as_str().to_string(), json, p.origin)),
            PresetKind::Generator => generator.push((id.as_str().to_string(), json, p.origin)),
        }
    }
    set_project_presets(effect, generator);
}

/// A real generator graph (Tesseract's) re-stamped with an overlay-only id —
/// the exact state a `.glb` import leaves: the layer TRACKS an embedded
/// preset by id with `graph: None`.
fn fake_imported_generator(id: &PresetTypeId) -> EmbeddedPreset {
    let tess_json = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../manifold-renderer/assets/generator-presets/Tesseract.json"),
    )
    .expect("read Tesseract.json");
    let mut def: manifold_core::effect_graph_def::EffectGraphDef =
        serde_json::from_str(&tess_json).expect("parse Tesseract def");
    if let Some(m) = def.preset_metadata.as_mut() {
        m.id = id.clone();
        m.display_name = "Imported Repro".to_string();
    }
    EmbeddedPreset {
        kind: PresetKind::Generator,
        def,
        origin: EmbeddedOrigin::Saved,
    }
}

#[test]
fn project_local_generator_params_survive_reload() {
    let preset_id = PresetTypeId::from_string("TestLocalGen_bug036".to_string());
    let embedded = fake_imported_generator(&preset_id);

    // ── Import time: template registered FIRST, then the layer is created
    // (mirrors the glTF import door's install-before-create ordering).
    install(std::slice::from_ref(&embedded));
    let mut project = Project::default();
    project.upsert_embedded_preset(embedded);
    project
        .timeline
        .add_layer("Imported", LayerType::Generator, preset_id.clone());

    // The instance seeded its card params from the registered template.
    let (param_id, written_value, template_len, template_min, template_max) = {
        let gp = project.timeline.layers[0]
            .gen_params_mut()
            .expect("generator instance");
        assert!(gp.params.len() > 0, "template must seed card params");
        let param_id = gp.params.iter().next().unwrap().id().to_string();
        let spec = gp.params.iter().next().unwrap().spec.clone();
        gp.set_base_param(&param_id, 0.37);
        // Read back rather than assuming 0.37 — the setter may quantize.
        (
            param_id.clone(),
            gp.get_param(&param_id),
            gp.params.len(),
            spec.min,
            spec.max,
        )
    };

    let json = serde_json::to_string(&project).expect("serialize project");
    assert!(
        json.contains(&format!("\"{param_id}\"")),
        "saved file must carry the param entry"
    );

    // ── Fresh launch: overlay empty, template unknown.
    clear_project_presets();

    // Backstop path (no pre-install hook): the params must still survive —
    // kept on placeholder specs, never silently dropped (the BUG-036 class).
    let reloaded =
        manifold_io::loader::load_project_from_json(&json).expect("reload without hook");
    {
        let gp = reloaded.timeline.layers[0]
            .gen_params()
            .expect("generator instance");
        assert_eq!(
            gp.params.len(),
            template_len,
            "keep-don't-drop: every saved param survives even with no template"
        );
        assert!(
            (gp.get_param(&param_id) - written_value).abs() < f32::EPSILON,
            "written value survives the template-less reload"
        );
    }

    // Root-fix path: the loader hands embedded presets to the installer
    // BEFORE deserialize, so params resolve against the REAL template.
    clear_project_presets();
    let reloaded = manifold_io::loader::load_project_from_json_with(&json, install)
        .expect("reload with pre-install hook");
    {
        let gp = reloaded.timeline.layers[0]
            .gen_params()
            .expect("generator instance");
        assert_eq!(gp.params.len(), template_len, "full template manifest");
        assert!(
            (gp.get_param(&param_id) - written_value).abs() < f32::EPSILON,
            "written value survives an ordered reload"
        );
        let spec = &gp.params.iter().next().unwrap().spec;
        assert_eq!(spec.id, param_id, "card order preserved");
        assert!(
            (spec.min - template_min).abs() < f32::EPSILON
                && (spec.max - template_max).abs() < f32::EPSILON,
            "param resolved against the real template spec, not a placeholder"
        );
    }

    clear_project_presets();
}
