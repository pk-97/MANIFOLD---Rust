//! Family augmentation of prepared graphs and parameter targets.
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::{RelightField, RelightParams};
use crate::persistence::PrimitiveRegistry;

pub struct RelightTarget {
    pub node_handle: &'static str,
    pub param_name: &'static str,
    pub scale: f32,
}

pub struct RelightAugmentation {
    pub augment: fn(&EffectGraphDef, &PrimitiveRegistry, &RelightParams) -> EffectGraphDef,
    pub targets: fn(RelightField) -> &'static [RelightTarget],
}
inventory::collect!(RelightAugmentation);

fn source() -> &'static RelightAugmentation {
    let mut sources = inventory::iter::<RelightAugmentation>.into_iter();
    let source = sources.next().expect("relight augmentation provider must be linked");
    assert!(sources.next().is_none(), "duplicate relight augmentation provider");
    source
}

pub fn relight_augment(def: &EffectGraphDef, registry: &PrimitiveRegistry, params: &RelightParams) -> EffectGraphDef {
    (source().augment)(def, registry, params)
}

pub fn relight_field_targets(field: RelightField) -> &'static [RelightTarget] {
    (source().targets)(field)
}
