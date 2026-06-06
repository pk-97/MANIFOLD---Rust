//! Phase 4b: project-embedded presets resolve through the catalog overlay.
//!
//! Isolated in its own integration-test binary (own process) because it mutates
//! the process-global preset catalog — running it inside the renderer lib test
//! binary would race the other catalog-reading tests.

use manifold_core::PresetTypeId;
use manifold_renderer::preset_loader::{
    GENERATOR_CATALOG, clear_project_presets, set_project_presets,
};

/// A minimal but valid generator preset: id + display name + empty graph.
fn fake_generator_preset(id: &str) -> String {
    format!(
        r#"{{
            "version": 2,
            "presetMetadata": {{
                "id": "{id}",
                "displayName": "Test Fork {id}",
                "category": "",
                "oscPrefix": "{id}",
                "isLineBased": false,
                "params": [],
                "bindings": []
            }},
            "nodes": [],
            "wires": []
        }}"#
    )
}

#[test]
fn project_generator_preset_resolves_via_overlay_then_clears() {
    let id = "TestForkGen_overlay";
    let preset_id = PresetTypeId::from_string(id.to_string());

    // Not present before installing the overlay.
    assert!(
        GENERATOR_CATALOG.load().json(id).is_none(),
        "fixture id must not exist in the stock/user catalog"
    );
    assert!(
        manifold_core::preset_definition_registry::generator::try_get(&preset_id).is_none(),
        "fixture id must not be in the core registry yet"
    );

    // Install the project overlay.
    set_project_presets(Vec::new(), vec![(id.to_string(), fake_generator_preset(id))]);

    // Now resolvable through BOTH the renderer catalog (graph JSON) and the
    // core definition registry (PresetDef) — the single overlay feeds both.
    assert!(
        GENERATOR_CATALOG.load().json(id).is_some(),
        "project generator preset must resolve in the renderer catalog after overlay"
    );
    let def = manifold_core::preset_definition_registry::generator::try_get(&preset_id)
        .expect("project generator preset must be in the core registry after overlay");
    assert_eq!(def.display_name, format!("Test Fork {id}"));

    // Clearing the overlay removes it from both (no leak into the next project).
    clear_project_presets();
    assert!(
        GENERATOR_CATALOG.load().json(id).is_none(),
        "clearing the overlay must remove the project preset from the catalog"
    );
    assert!(
        manifold_core::preset_definition_registry::generator::try_get(&preset_id).is_none(),
        "clearing the overlay must remove the project preset from the core registry"
    );
}
