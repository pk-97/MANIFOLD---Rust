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
    println!("primitive count: {}", registry.known_type_ids().count());
}
