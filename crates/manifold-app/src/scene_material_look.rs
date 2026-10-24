//! A Material's starting looks (Default, Matte, Coated, Brushed Metal,
//! Glass): which features each switches on and which factors it writes. A
//! look is an action on the Material item, not a stored setting; the block
//! shows the look the Material's values match, or Custom.

use manifold_core::effects::PresetInstance;
use manifold_core::material_inspector::MaterialParamRole;
use manifold_core::scene_doc::{ItemId, MaterialFields, param_id, split_param_id};
use manifold_ui::param_surface::{MaterialFeature, MaterialLook};

pub(crate) struct Recipe {
    pub features: &'static [MaterialFeature],
    /// Factors by material param name. Default writes every factor's own
    /// default instead.
    pub values: &'static [(&'static str, f32)],
}

pub(crate) fn recipe(look: MaterialLook) -> Recipe {
    match look {
        MaterialLook::Default => Recipe {
            features: &[],
            values: &[],
        },
        MaterialLook::Matte => Recipe {
            features: &[],
            values: &[("metallic", 0.0), ("roughness", 0.8)],
        },
        MaterialLook::Coated => Recipe {
            features: &[MaterialFeature::Coat],
            values: &[
                ("metallic", 0.0),
                ("roughness", 0.35),
                ("clearcoat", 1.0),
                ("clearcoat_roughness", 0.1),
            ],
        },
        MaterialLook::BrushedMetal => Recipe {
            features: &[MaterialFeature::Anisotropy],
            values: &[
                ("metallic", 1.0),
                ("roughness", 0.3),
                ("anisotropy_strength", 0.6),
            ],
        },
        MaterialLook::Glass => Recipe {
            features: &[MaterialFeature::Glass],
            values: &[
                ("metallic", 0.0),
                ("roughness", 0.05),
                ("transmission", 1.0),
                ("ior", 1.5),
            ],
        },
    }
}

/// Whether a param is one of a material's factors (a scalar or colour on its
/// surface node), the params Default puts back.
fn is_factor(role: Option<MaterialParamRole>) -> bool {
    matches!(
        role,
        Some(MaterialParamRole::Scalar(_) | MaterialParamRole::Colour(..))
    )
}

/// Every param id `material` owns.
pub(crate) fn param_ids(inst: &PresetInstance, material: &ItemId) -> Vec<String> {
    inst.params
        .iter()
        .filter(|param| {
            split_param_id(param.id()).is_some_and(|(owner, _)| owner == material.as_str())
        })
        .map(|param| param.id().to_owned())
        .collect()
}

/// The factor writes `look` makes on a material whose params are `ids`, by
/// property.
pub(crate) fn writes(
    inst: &PresetInstance,
    ids: &[String],
    look: MaterialLook,
) -> Vec<(String, f32)> {
    if look != MaterialLook::Default {
        return recipe(look)
            .values
            .iter()
            .map(|(property, value)| ((*property).to_owned(), *value))
            .collect();
    }
    ids.iter()
        .filter_map(|id| {
            let param = inst
                .params
                .get(id)
                .filter(|param| is_factor(param.spec.material_role))?;
            Some((split_param_id(id)?.1.to_owned(), param.spec.default_value))
        })
        .collect()
}

/// The look `material` (whose params are `ids`) matches by its features and
/// values, if any.
pub(crate) fn matching(
    inst: &PresetInstance,
    material: &ItemId,
    ids: &[String],
    fields: &MaterialFields,
) -> Option<MaterialLook> {
    let features = || {
        fields
            .features
            .iter()
            .filter(|setting| !setting.removed())
            .map(|setting| setting.feature)
    };
    MaterialLook::ALL.into_iter().find(|&look| {
        recipe(look)
            .features
            .iter()
            .map(|feature| crate::scene_edit::core_feature(*feature))
            .eq(features())
            && writes(inst, ids, look).iter().all(|(property, value)| {
                inst.params
                    .get(&param_id(material, property))
                    .is_none_or(|param| (param.base - value).abs() < 1e-4)
            })
    })
}
