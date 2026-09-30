//! Structural invariants every registered generator's param surface must hold.
//! Lives in `manifold-core` because that is where the `inventory` registrations
//! link; JSON-only generators register with the renderer and are covered there.

use std::collections::HashSet;

use manifold_core::preset_def::PresetKind;
use manifold_core::{preset_definition_registry, preset_type_registry};

#[test]
fn every_generator_param_surface_is_well_formed() {
    let generators: Vec<_> = preset_type_registry::all_of_kind(PresetKind::Generator)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(!generators.is_empty(), "no generators registered");

    let mut osc_addresses = HashSet::new();
    for type_id in &generators {
        let def = preset_definition_registry::get(type_id);
        assert!(!def.param_defs.is_empty(), "{type_id:?} has no params");

        let mut ids = HashSet::new();
        for pd in &def.param_defs {
            let s = &pd.spec;
            assert!(!s.id.is_empty(), "{type_id:?}: param with empty id");
            assert!(ids.insert(s.id.as_str()), "{type_id:?}: duplicate param id {:?}", s.id);
            assert!(!s.name.trim().is_empty(), "{type_id:?}.{}: empty name", s.id);
            assert!(
                s.min <= s.default_value && s.default_value <= s.max,
                "{type_id:?}.{}: default {} outside [{}, {}]",
                s.id,
                s.default_value,
                s.min,
                s.max
            );
            if let Some(addr) = preset_definition_registry::get_osc_address_by_id(type_id, &s.id) {
                assert!(osc_addresses.insert(addr.clone()), "duplicate OSC address {addr}");
            }
        }
    }
    assert!(!osc_addresses.is_empty(), "no generator exposes an OSC prefix; uniqueness went unchecked");
}
