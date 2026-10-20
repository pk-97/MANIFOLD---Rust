//! Registry-backed macro addressing: a macro mapping drives the exact effect
//! it names by id.
//!
//! Lives in `manifold-app/tests/` because the registry depends on
//! `inventory::submit!` entries from `manifold-nodes`; `manifold-app` is the
//! one crate that links all of them outside a running binary.

// Force the linker to keep manifold-nodes's `inventory::submit!`
// blocks. Without a reference into the crate, dead-code elimination
// can drop the entire compilation unit and silently empty the
// effect / generator registries.
use manifold_nodes as _;

/// BUG-005: a macro mapping addresses its effect by stable `EffectId`, so with
/// two effects of the same type on one layer it drives the exact one mapped —
/// not whichever comes first by type (the old `find(effect_type)` bug).
#[test]
fn macro_drives_the_exact_effect_among_two_of_the_same_type() {
    use manifold_core::PresetTypeId;
    use manifold_core::layer::Layer;
    use manifold_core::macro_bank::{MacroBank, MacroCurve, MacroMapping, MacroMappingTarget};
    use manifold_core::preset_definition_registry;
    use manifold_core::project::Project;
    use manifold_core::types::LayerType;

    let ty = PresetTypeId::new("Bloom");
    let pid = preset_definition_registry::get(&ty)
        .param_defs
        .first()
        .map(|pd| pd.spec.id.clone())
        .expect("Bloom must have a param 0 in the live registry");

    // Two same-type effects, both seeded to a sentinel so a stray write is
    // visible. A first-match-by-type resolver would hit `fx_a` (the first).
    let mut fx_a = preset_definition_registry::create_default(&ty);
    fx_a.set_base_param(&pid, 0.25);
    let mut fx_b = preset_definition_registry::create_default(&ty);
    fx_b.set_base_param(&pid, 0.25);
    let id_a = fx_a.id.clone();
    let id_b = fx_b.id.clone();

    let mut project = Project::default();
    let mut layer = Layer::new("L".into(), LayerType::Video, 0);
    layer.effects = Some(vec![fx_a, fx_b]);
    project.timeline.layers.push(layer);

    // Map macro slot 0 to the SECOND Bloom's param.
    project.settings.macro_bank.slots[0]
        .mappings
        .push(MacroMapping {
            target: MacroMappingTarget::Effect {
                effect_id: id_b.clone(),
                param_id: std::borrow::Cow::Owned(pid.to_string()),
            },
            range_min: 0.0,
            range_max: 1.0,
            curve: MacroCurve::Linear,
            legacy_param_index: None,
            legacy_effect_addr: None,
        });

    MacroBank::apply_macro(&mut project, 0, 1.0);

    let after_a = project.find_effect_by_id(&id_a).unwrap().get_base_param(&pid);
    let after_b = project.find_effect_by_id(&id_b).unwrap().get_base_param(&pid);
    assert!(
        (after_a - 0.25).abs() < 1e-6,
        "the OTHER same-type effect must be untouched, got {after_a}"
    );
    assert!(
        (after_b - 1.0).abs() < 1e-6,
        "the mapped effect must receive the macro value, got {after_b}"
    );
}
