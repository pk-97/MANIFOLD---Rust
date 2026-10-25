//! Each outer control's runtime reshape and name, by binding id.
//!
//! The reshape normalises over the declared slider range, never the user's slider range, so a build
//! with no manifest and a build with one give the same reshape. Curve and
//! invert are user choices, read from the manifest when there is one.

use ahash::AHashMap;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::macro_bank::MacroCurve;
use manifold_core::params::ParamManifest;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BindingShape {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub curve: MacroCurve,
    pub invert: bool,
}

pub(crate) fn binding_shapes(
    doc: &EffectGraphDef,
    manifest: Option<&ParamManifest>,
) -> AHashMap<String, BindingShape> {
    let mut out = AHashMap::new();
    let Some(meta) = doc.preset_metadata.as_ref() else {
        return out;
    };
    let Some(mut declared) = manifold_core::param_lookup::Declared::new(doc) else {
        return out;
    };
    for saved in &meta.params {
        let Some(d) = declared.spec(&saved.id) else {
            continue;
        };
        let live = manifest.and_then(|m| m.get(&saved.id)).map(|p| &p.spec);
        out.insert(
            saved.id.clone(),
            BindingShape {
                name: live.map_or_else(|| d.name.clone(), |s| s.name.clone()),
                min: d.min,
                max: d.max,
                curve: live.map_or(d.curve, |s| s.curve),
                invert: live.map_or(d.invert, |s| s.invert),
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
    };
    use manifold_core::effects::ParamConvert;
    use manifold_core::params::{Param, ParamChoices};

    fn doc() -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                shape: None,
                id: PresetTypeId::from_string("test.binding_shape".into()),
                display_name: String::new(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                layer_types: None,
                params: vec![ParamSpecDef {
                    id: "amount".into(),
                    name: "Amount".into(),
                    min: 0.0,
                    max: 1.0,
                    ..ParamSpecDef::default()
                }],
                bindings: vec![BindingDef {
                    id: "amount".into(),
                    label: String::new(),
                    default_value: 0.5,
                    target: BindingTarget::Composite {
                        outer_name: "amount".into(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                    default_mirrors_node_param: false,
                }],
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
                scene_modifier: None,
                scene_bounds: None,
            }),
            scene_modifiers: Vec::new(),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    /// INV-S18: the user's slider range never reaches the reshape, while the
    /// user's curve and invert do.
    #[test]
    fn reshape_ignores_user_slider_range() {
        let doc = doc();
        let choices = ParamChoices {
            slider: Some((-3.0, 7.0)),
            invert: Some(true),
            ..ParamChoices::default()
        };
        let mut spec = doc.preset_metadata.as_ref().unwrap().params[0].clone();
        choices.apply(&mut spec);
        let mut param = Param::bundled(spec);
        param.choices = choices;
        assert_eq!(param.slider_span(), (-3.0, 7.0));
        let built = binding_shapes(&doc, Some(&ParamManifest::from_params(vec![param])));
        assert_eq!((built["amount"].min, built["amount"].max), (0.0, 1.0));
        assert!(
            built["amount"].invert,
            "invert is the user's choice and reaches the reshape"
        );
    }

    /// A user's slider range is display only: the manifest build reshapes
    /// over the declared range, exactly as the standalone build does.
    #[test]
    fn standalone_build_reshape_matches_manifest_build() {
        let doc = doc();
        let choices = ParamChoices {
            slider: Some((0.0, 10.0)),
            ..ParamChoices::default()
        };
        let mut spec = doc.preset_metadata.as_ref().unwrap().params[0].clone();
        choices.apply(&mut spec);
        let mut param = Param::bundled(spec);
        param.choices = choices;
        let manifest = ParamManifest::from_params(vec![param]);

        let standalone = binding_shapes(&doc, None);
        let built = binding_shapes(&doc, Some(&manifest));
        assert_eq!(standalone, built);
        assert_eq!((built["amount"].min, built["amount"].max), (0.0, 1.0));
    }
}
