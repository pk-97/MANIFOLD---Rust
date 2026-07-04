//! Phase 4b: project-embedded presets resolve through the catalog overlay.
//!
//! Isolated in its own integration-test binary (own process) because it mutates
//! the process-global preset catalog — running it inside the renderer lib test
//! binary would race the other catalog-reading tests.

use std::sync::Mutex;

use manifold_core::PresetTypeId;
use manifold_core::project::EmbeddedOrigin;
use manifold_renderer::preset_loader::{
    EFFECT_CATALOG, GENERATOR_CATALOG, clear_project_presets, set_project_presets,
};

/// The test harness runs `#[test]` fns in this binary on separate threads by
/// default; every test here mutates the same process-global overlay statics
/// via `set_project_presets`/`clear_project_presets`, so without this lock two
/// tests could interleave their install/clear calls. Held for a test's full
/// body (not just the mutation) so the assertions that follow an install see
/// a stable catalog.
static TEST_LOCK: Mutex<()> = Mutex::new(());

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
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let id = "TestForkGen_overlay";
    let preset_id = PresetTypeId::from_string(id.to_string());

    // Not present before installing the overlay.
    assert!(
        GENERATOR_CATALOG.load().json(id).is_none(),
        "fixture id must not exist in the stock/user catalog"
    );
    assert!(
        manifold_core::preset_definition_registry::try_get(&preset_id).is_none(),
        "fixture id must not be in the core registry yet"
    );

    // Install the project overlay.
    set_project_presets(
        Vec::new(),
        vec![(id.to_string(), fake_generator_preset(id), EmbeddedOrigin::Saved)],
    );

    // Now resolvable through BOTH the renderer catalog (graph JSON) and the
    // core definition registry (PresetDef) — the single overlay feeds both.
    assert!(
        GENERATOR_CATALOG.load().json(id).is_some(),
        "project generator preset must resolve in the renderer catalog after overlay"
    );
    let def = manifold_core::preset_definition_registry::try_get(&preset_id)
        .expect("project generator preset must be in the core registry after overlay");
    assert_eq!(def.display_name, format!("Test Fork {id}"));

    // Clearing the overlay removes it from both (no leak into the next project).
    clear_project_presets();
    assert!(
        GENERATOR_CATALOG.load().json(id).is_none(),
        "clearing the overlay must remove the project preset from the catalog"
    );
    assert!(
        manifold_core::preset_definition_registry::try_get(&preset_id).is_none(),
        "clearing the overlay must remove the project preset from the core registry"
    );
}

/// A minimal but valid effect preset carrying a marker string in its
/// display name, so a test can prove which JSON actually resolved.
fn fake_effect_preset(id: &str, marker: &str) -> String {
    format!(
        r#"{{
            "version": 2,
            "presetMetadata": {{
                "id": "{id}",
                "displayName": "{marker}",
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

/// D5 (PRESET_LIBRARY_DESIGN P2): a `Snapshot`-origin overlay entry sits
/// BELOW disk — a real stock preset's file always wins over a stale
/// snapshot cached in the project. "Bloom" ships as a real stock effect
/// preset (`assets/effect-presets/Bloom.json`); installing a bogus Snapshot
/// entry under the same id must NOT shadow it.
#[test]
fn snapshot_tier_never_shadows_a_real_disk_preset() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let marker = "SNAPSHOT_MUST_NOT_WIN_OVER_DISK";
    set_project_presets(
        vec![("Bloom".to_string(), fake_effect_preset("Bloom", marker), EmbeddedOrigin::Snapshot)],
        Vec::new(),
    );

    let resolved = EFFECT_CATALOG
        .load()
        .json("Bloom")
        .expect("Bloom must still resolve");
    assert!(
        !resolved.contains(marker),
        "disk must win over a Snapshot entry for the same id, got: {resolved}"
    );

    clear_project_presets();
}

/// D5: a `Snapshot`-origin overlay entry for an id with NO disk file at all
/// (stock or user) resolves from the snapshot — the exact self-containment
/// fallback that keeps a saved project from stranding when its library
/// file is later deleted.
#[test]
fn snapshot_tier_resolves_when_disk_is_absent() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let id = "TestSnapshotOnly_overlay";
    let marker = "SNAPSHOT_FALLBACK_MARKER";

    assert!(
        EFFECT_CATALOG.load().json(id).is_none(),
        "fixture id must not exist in the stock/user catalog"
    );

    set_project_presets(
        vec![(id.to_string(), fake_effect_preset(id, marker), EmbeddedOrigin::Snapshot)],
        Vec::new(),
    );

    let resolved = EFFECT_CATALOG
        .load()
        .json(id)
        .expect("a Snapshot entry with no disk file must still resolve (D5 fallback)");
    assert!(resolved.contains(marker), "resolved value must be the snapshot's own content");

    clear_project_presets();
}

/// D2/D5 negative gate: `Saved`-tier entries keep today's on-top-of-disk
/// behavior — installing a `Saved` override for "Bloom" DOES shadow the
/// real stock file (unlike `Snapshot`, which must not).
#[test]
fn saved_tier_still_overrides_disk_like_before_p2() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let marker = "SAVED_TIER_ON_TOP_MARKER";
    set_project_presets(
        vec![("Bloom".to_string(), fake_effect_preset("Bloom", marker), EmbeddedOrigin::Saved)],
        Vec::new(),
    );

    let resolved = EFFECT_CATALOG
        .load()
        .json("Bloom")
        .expect("Bloom must resolve");
    assert!(
        resolved.contains(marker),
        "Saved entries must still win over disk (unchanged P1 behavior), got: {resolved}"
    );

    clear_project_presets();
}
