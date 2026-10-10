//! D11's hub-owned primitives. The pipeline helper has no registry factory.

const BUILTINS: &[(&str, Option<&str>)] = &[
    ("wgsl_compute", Some("node.wgsl_compute")),
    ("standalone_pipeline", None),
    ("Mix", Some("node.mix")),
    ("MaskedMix", Some("node.masked_mix")),
    ("MuxTexture", Some("node.switch_texture")),
    ("Value", Some("node.value")),
    ("Gain", Some("node.exposure")),
];

#[test]
fn builtins_match_registry() {
    let registry = super::persistence::PrimitiveRegistry::with_builtin();
    for &(name, type_id) in BUILTINS {
        if let Some(type_id) = type_id {
            assert!(registry.contains(type_id), "missing hub built-in {name}: {type_id}");
            assert!(registry.construct(type_id).is_some(), "cannot construct {name}");
        }
    }
    // System boundaries are graph vocabulary; node.__* factories are macro
    // and validation fixtures compiled only into the test harness.
    let mut actual: Vec<_> = registry.known_type_ids()
        .filter(|id| id.starts_with("node.") && !id.starts_with("node.__"))
        .collect();
    let mut expected: Vec<_> = BUILTINS.iter().filter_map(|(_, id)| *id).collect();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected, "engine node factories must equal the D11 list");
}
