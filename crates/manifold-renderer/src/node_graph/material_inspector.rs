//! Renderer-owned semantic roles for the material inspector.
//!
//! The primitive descriptor remains the source of ranges, defaults, and
//! labels.  This table only classifies those existing names so every surface
//! (card, scene panel, and graph inspector) can use the same vocabulary.

use manifold_core::material_inspector::{
    MaterialColour, MaterialFeature, MaterialGroup, MaterialMapFamily, MaterialParamRole,
    RgbChannel, SamplerComponent, UvComponent,
};

/// Classify one PBR descriptor for the material inspector.
///
/// This intentionally returns `None` for non-PBR nodes and unknown future
/// parameters.  The metadata coverage test keeps additions from silently
/// disappearing from the Advanced section.
pub fn material_param_role(type_id: &str, param_name: &str) -> Option<MaterialParamRole> {
    if type_id != "node.pbr_material" {
        return None;
    }

    let feature_mode = match param_name {
        "coat_mode" => Some(MaterialFeature::Coat),
        "iridescence_mode" => Some(MaterialFeature::Iridescence),
        "emission_mode" => Some(MaterialFeature::Emission),
        "glass_mode" => Some(MaterialFeature::Glass),
        "sheen_mode" => Some(MaterialFeature::Sheen),
        "anisotropy_mode" => Some(MaterialFeature::Anisotropy),
        "translucency_mode" => Some(MaterialFeature::Translucency),
        _ => None,
    };
    if let Some(feature) = feature_mode {
        return Some(MaterialParamRole::FeatureMode(feature));
    }

    let scalar_group = match param_name {
        "color_a" | "alpha_mode" | "alpha_cutoff" => Some(MaterialGroup::Opacity),
        "color_r" | "color_g" | "color_b" | "metallic" | "roughness" => {
            Some(MaterialGroup::Surface)
        }
        "ambient" | "specular" | "baked_look" | "normal_scale" | "occlusion_strength" => Some(MaterialGroup::Advanced),
        "clearcoat_normal_scale" => Some(MaterialGroup::Feature(MaterialFeature::Coat)),
        "subsurface_weight" | "subsurface_radius_r" | "subsurface_radius_g"
        | "subsurface_radius_b" | "subsurface_color_r" | "subsurface_color_g"
        | "subsurface_color_b" | "subsurface_anisotropy" | "subsurface_mode"
        | "subsurface_samples" => Some(MaterialGroup::Subsurface),
        "translucency_color_r" | "translucency_color_g" | "translucency_color_b" => {
            Some(MaterialGroup::Feature(MaterialFeature::Translucency))
        }
        "emission_intensity"
        | "clearcoat"
        | "clearcoat_roughness"
        | "iridescence"
        | "iridescence_ior"
        | "iridescence_thickness_min"
        | "iridescence_thickness_max"
        | "transmission"
        | "ior"
        | "volume_thickness"
        | "volume_attenuation_distance"
        | "dispersion"
        | "sheen_roughness"
        | "anisotropy_strength"
        | "anisotropy_rotation"
        | "translucency" => Some(MaterialGroup::Feature(feature_for_scalar(param_name))),
        "emission_r" | "emission_g" | "emission_b" => {
            Some(MaterialGroup::Feature(MaterialFeature::Emission))
        }
        "sheen_color_r" | "sheen_color_g" | "sheen_color_b" => {
            Some(MaterialGroup::Feature(MaterialFeature::Sheen))
        }
        "volume_attenuation_color_r"
        | "volume_attenuation_color_g"
        | "volume_attenuation_color_b" => Some(MaterialGroup::Feature(MaterialFeature::Glass)),
        "specular_tint_r" | "specular_tint_g" | "specular_tint_b" => Some(MaterialGroup::Advanced),
        _ => None,
    };
    if let Some(group) = scalar_group {
        if let Some((colour, channel)) = colour_channel(param_name) {
            return Some(MaterialParamRole::Colour(group, colour, channel));
        }
        return Some(MaterialParamRole::Scalar(group));
    }

    if let Some((family, component)) = placement_component(param_name) {
        return Some(MaterialParamRole::Placement(family, component));
    }
    if let Some((family, component)) = sampler_component(param_name) {
        return Some(MaterialParamRole::Sampler(family, component));
    }
    if advanced_map_metadata(param_name) {
        return Some(MaterialParamRole::Scalar(MaterialGroup::Advanced));
    }
    None
}

/// Extended map coordinates and sampling stay editable on the shared
/// Advanced surface. Match only the implemented catalog so unknown future
/// controls still fail the schema-coverage test.
fn advanced_map_metadata(name: &str) -> bool {
    if ["", "nrm_", "mr_", "occ_", "em_"].iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|suffix| matches!(suffix, "uv_set" | "mip_filter"))
    }) {
        return true;
    }
    ["sheen_color_", "sheen_roughness_", "iridescence_", "iridescence_thickness_",
        "anisotropy_", "clearcoat_", "clearcoat_roughness_", "clearcoat_normal_",
        "specular_", "specular_color_", "transmission_", "volume_thickness_",
        "diffuse_transmission_", "diffuse_transmission_color_"].iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|suffix| matches!(suffix,
            "uv_m00" | "uv_m01" | "uv_m10" | "uv_m11" | "uv_tx" | "uv_ty"
            | "tex_coord" | "wrap_u" | "wrap_v" | "mag_filter" | "min_filter" | "mip_filter"))
    })
}

fn feature_for_scalar(name: &str) -> MaterialFeature {
    match name {
        "clearcoat" | "clearcoat_roughness" => MaterialFeature::Coat,
        "iridescence"
        | "iridescence_ior"
        | "iridescence_thickness_min"
        | "iridescence_thickness_max" => MaterialFeature::Iridescence,
        "emission_intensity" => MaterialFeature::Emission,
        "transmission"
        | "ior"
        | "volume_thickness"
        | "volume_attenuation_distance"
        | "dispersion" => {
            MaterialFeature::Glass
        }
        "sheen_roughness" => MaterialFeature::Sheen,
        "anisotropy_strength" | "anisotropy_rotation" => MaterialFeature::Anisotropy,
        "translucency" => MaterialFeature::Translucency,
        _ => unreachable!("unclassified material feature scalar: {name}"),
    }
}

fn colour_channel(name: &str) -> Option<(MaterialColour, RgbChannel)> {
    let (colour, suffix) = if let Some(suffix) = name.strip_prefix("color_") {
        (MaterialColour::Base, suffix)
    } else if let Some(suffix) = name.strip_prefix("emission_") {
        (MaterialColour::Emission, suffix)
    } else if let Some(suffix) = name.strip_prefix("sheen_color_") {
        (MaterialColour::Sheen, suffix)
    } else if let Some(suffix) = name.strip_prefix("volume_attenuation_color_") {
        (MaterialColour::Attenuation, suffix)
    } else if let Some(suffix) = name.strip_prefix("specular_tint_") {
        (MaterialColour::Specular, suffix)
    } else if let Some(suffix) = name.strip_prefix("subsurface_color_") {
        (MaterialColour::Subsurface, suffix)
    } else if let Some(suffix) = name.strip_prefix("translucency_color_") {
        (MaterialColour::Translucency, suffix)
    } else {
        return None;
    };
    let channel = match suffix {
        "r" => RgbChannel::R,
        "g" => RgbChannel::G,
        "b" => RgbChannel::B,
        _ => return None,
    };
    Some((colour, channel))
}

fn placement_component(name: &str) -> Option<(MaterialMapFamily, UvComponent)> {
    let (family, suffix) = if let Some(suffix) = name.strip_prefix("uv_") {
        (MaterialMapFamily::Base, suffix)
    } else if let Some(suffix) = name.strip_prefix("nrm_uv_") {
        (MaterialMapFamily::Normal, suffix)
    } else if let Some(suffix) = name.strip_prefix("mr_uv_") {
        (MaterialMapFamily::MetallicRoughness, suffix)
    } else if let Some(suffix) = name.strip_prefix("occ_uv_") {
        (MaterialMapFamily::Occlusion, suffix)
    } else if let Some(suffix) = name.strip_prefix("em_uv_") {
        (MaterialMapFamily::Emission, suffix)
    } else {
        return None;
    };
    let component = match suffix {
        "m00" => UvComponent::M00,
        "m01" => UvComponent::M01,
        "m10" => UvComponent::M10,
        "m11" => UvComponent::M11,
        "tx" => UvComponent::Tx,
        "ty" => UvComponent::Ty,
        _ => return None,
    };
    Some((family, component))
}

fn sampler_component(name: &str) -> Option<(MaterialMapFamily, SamplerComponent)> {
    let (family, suffix) = match name {
        "wrap_u" => (MaterialMapFamily::Base, "wrap_u"),
        "wrap_v" => (MaterialMapFamily::Base, "wrap_v"),
        "mag_filter" => (MaterialMapFamily::Base, "mag_filter"),
        "min_filter" => (MaterialMapFamily::Base, "min_filter"),
        _ => {
            let (prefix, family) = [
                ("nrm_", MaterialMapFamily::Normal),
                ("mr_", MaterialMapFamily::MetallicRoughness),
                ("occ_", MaterialMapFamily::Occlusion),
                ("em_", MaterialMapFamily::Emission),
            ]
            .into_iter()
            .find(|(prefix, _)| name.starts_with(prefix))?;
            (family, name.strip_prefix(prefix)?)
        }
    };
    let component = match suffix {
        "wrap_u" => SamplerComponent::WrapU,
        "wrap_v" => SamplerComponent::WrapV,
        "mag_filter" => SamplerComponent::MagFilter,
        "min_filter" => SamplerComponent::MinFilter,
        _ => return None,
    };
    Some((family, component))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::PbrMaterial;

    #[test]
    fn material_inspector_schema_covers_pbr() {
        let params = PbrMaterial::PARAMS;
        assert_eq!(params.len(), 290);
        let mut names = std::collections::HashSet::<&str>::new();
        let mut feature_modes = 0;
        let mut map_families = [0usize; 5];
        for param in params {
            assert!(names.insert(param.name.as_ref()), "duplicate {}", param.name);
            let role = material_param_role("node.pbr_material", param.name.as_ref())
                .unwrap_or_else(|| panic!("unclassified {}", param.name));
            match role {
                MaterialParamRole::FeatureMode(_) => feature_modes += 1,
                MaterialParamRole::Placement(family, _) | MaterialParamRole::Sampler(family, _) => {
                    map_families[match family {
                        MaterialMapFamily::Base => 0,
                        MaterialMapFamily::Normal => 1,
                        MaterialMapFamily::MetallicRoughness => 2,
                        MaterialMapFamily::Occlusion => 3,
                        MaterialMapFamily::Emission => 4,
                    }] += 1;
                }
                _ => {}
            }
        }
        assert_eq!(feature_modes, 7);
        assert_eq!(map_families, [10, 10, 10, 10, 10]);
        assert!(material_param_role("node.phong_material", "color_r").is_none());
        assert!(material_param_role("node.pbr_material", "subsurface_unknown").is_none());
        assert_eq!(material_param_role("node.pbr_material", "subsurface_mode"),
            Some(MaterialParamRole::Scalar(MaterialGroup::Subsurface)));
        assert_eq!(material_param_role("node.pbr_material", "subsurface_color_r"),
            Some(MaterialParamRole::Colour(MaterialGroup::Subsurface, MaterialColour::Subsurface, RgbChannel::R)));
    }
}
