//! Material facts adapted at the engine/UI boundary. No registry descriptor copies.
use manifold_core::material_inspector as core;
use manifold_core::effects::{ParamId, PresetInstance};
use manifold_core::project::Project;
use manifold_core::scene_doc::split_param_id;
use manifold_core::GraphTarget;
use manifold_ui::param_surface as ui;

pub(crate) fn feature(value: core::MaterialFeature) -> ui::MaterialFeature {
    match value {
        core::MaterialFeature::Coat => ui::MaterialFeature::Coat,
        core::MaterialFeature::Iridescence => ui::MaterialFeature::Iridescence,
        core::MaterialFeature::Emission => ui::MaterialFeature::Emission,
        core::MaterialFeature::Glass => ui::MaterialFeature::Glass,
        core::MaterialFeature::Sheen => ui::MaterialFeature::Sheen,
        core::MaterialFeature::Anisotropy => ui::MaterialFeature::Anisotropy,
        core::MaterialFeature::Translucency => ui::MaterialFeature::Translucency,
        core::MaterialFeature::Subsurface => ui::MaterialFeature::Subsurface,
    }
}

fn colour(value: core::MaterialColour) -> ui::MaterialColour {
    match value {
        core::MaterialColour::Base => ui::MaterialColour::Base,
        core::MaterialColour::Specular => ui::MaterialColour::Specular,
        core::MaterialColour::Emission => ui::MaterialColour::Emission,
        core::MaterialColour::Sheen => ui::MaterialColour::Sheen,
        core::MaterialColour::Attenuation => ui::MaterialColour::Attenuation,
        core::MaterialColour::VolumeScattering => ui::MaterialColour::VolumeScattering,
        core::MaterialColour::Subsurface => ui::MaterialColour::Subsurface,
        core::MaterialColour::Translucency => ui::MaterialColour::Translucency,
    }
}

fn family(value: core::MaterialMapFamily) -> ui::MaterialMapFamily {
    match value {
        core::MaterialMapFamily::Base => ui::MaterialMapFamily::Base,
        core::MaterialMapFamily::Normal => ui::MaterialMapFamily::Normal,
        core::MaterialMapFamily::MetallicRoughness => ui::MaterialMapFamily::MetallicRoughness,
        core::MaterialMapFamily::Occlusion => ui::MaterialMapFamily::Occlusion,
        core::MaterialMapFamily::Emission => ui::MaterialMapFamily::Emission,
    }
}

fn uv(value: core::UvComponent) -> ui::UvComponent {
    match value {
        core::UvComponent::M00 => ui::UvComponent::M00,
        core::UvComponent::M01 => ui::UvComponent::M01,
        core::UvComponent::M10 => ui::UvComponent::M10,
        core::UvComponent::M11 => ui::UvComponent::M11,
        core::UvComponent::Tx => ui::UvComponent::Tx,
        core::UvComponent::Ty => ui::UvComponent::Ty,
    }
}

fn sampler(value: core::SamplerComponent) -> ui::SamplerComponent {
    match value {
        core::SamplerComponent::WrapU => ui::SamplerComponent::WrapU,
        core::SamplerComponent::WrapV => ui::SamplerComponent::WrapV,
        core::SamplerComponent::MagFilter => ui::SamplerComponent::MagFilter,
        core::SamplerComponent::MinFilter => ui::SamplerComponent::MinFilter,
    }
}

fn group(value: core::MaterialGroup) -> ui::MaterialGroup {
    match value {
        core::MaterialGroup::Surface => ui::MaterialGroup::Surface,
        core::MaterialGroup::Opacity => ui::MaterialGroup::Opacity,
        core::MaterialGroup::Feature(f) => ui::MaterialGroup::Feature(feature(f)),
        core::MaterialGroup::Advanced => ui::MaterialGroup::Advanced,
    }
}
pub(super) fn role(value: core::MaterialParamRole) -> ui::MaterialParamRole {
    match value {
        core::MaterialParamRole::Scalar(g) => ui::MaterialParamRole::Scalar(group(g)),
        core::MaterialParamRole::Colour(g, c, r) => {
            ui::MaterialParamRole::Colour(group(g), colour(c), r)
        }
        core::MaterialParamRole::FeatureMode(f) => ui::MaterialParamRole::FeatureMode(feature(f)),
        core::MaterialParamRole::Placement(f, c) => {
            ui::MaterialParamRole::Placement(family(f), uv(c))
        }
        core::MaterialParamRole::Sampler(f, c) => {
            ui::MaterialParamRole::Sampler(family(f), sampler(c))
        }
    }
}

/// Group each colour's three channel rows: the R row of a colour whose G and
/// B rows sit on the same scene item carries all three as `rgb_members`, so a
/// scene surface draws it as the one colour row. Its `editable` says whether
/// one RGB write owns them ([`rgb_ids_editable`]). A colour with a missing,
/// doubled or split channel stays three rows.
pub(super) fn colour_rows(rows: &mut [ui::ParamRow], inst: &PresetInstance) {
    for index in 0..rows.len() {
        let Some((colour, ui::RgbChannel::R)) = row_rgb_channel(&rows[index].spec) else {
            continue;
        };
        let Some(item) = split_param_id(&rows[index].id).map(|(item, _)| item) else {
            continue;
        };
        let mut members: [Option<&ui::ParamRow>; 3] = [None, None, None];
        let mut single = true;
        for row in rows.iter() {
            let Some((c, channel)) = row_rgb_channel(&row.spec) else {
                continue;
            };
            if c != colour || split_param_id(&row.id).map(|(other, _)| other) != Some(item) {
                continue;
            }
            single &= members[channel.index()].is_none();
            members[channel.index()] = Some(row);
        }
        let group = colour.map_or(ui::ColourGroup::Light, ui::ColourGroup::Material);
        let grouped = match members {
            [Some(r), Some(g), Some(b)] if single => {
                let ids = [r, g, b].map(|row| row.id.clone());
                Some(ui::RgbMembers {
                    group,
                    editable: [r, g, b].iter().all(|row| !row.value.driven) && rgb_ids_editable(inst, &ids),
                    ids,
                    values: [r, g, b].map(|row| row.value.base),
                })
            }
            _ => None,
        };
        rows[index].rgb_members = grouped;
    }
}

/// A row's colour and channel, from its UI descriptor: a material colour
/// channel, or a scene item's colour channel (`None` colour).
fn row_rgb_channel(spec: &ui::RowSpec) -> Option<(Option<ui::MaterialColour>, ui::RgbChannel)> {
    match (spec.material_role, spec.scene.and_then(|scene| scene.colour)) {
        (Some(ui::MaterialParamRole::Colour(_, colour, channel)), _) => Some((Some(colour), channel)),
        (None, Some(channel)) => Some((None, channel)),
        _ => None,
    }
}

/// One swatch over three channel rows, or `None` when the channels cannot
/// take a single RGB write ([`rgb_ids_editable`]).
pub(super) fn rgb_members(inst: &PresetInstance, group: ui::ColourGroup, rows: [&ui::ParamRow; 3]) -> Option<ui::RgbMembers> {
    let ids = rows.map(|row| row.id.clone());
    rgb_ids_editable(inst, &ids).then(|| ui::RgbMembers { group, ids, values: rows.map(|row| row.value.base), editable: true })
}

/// Which colour and channel a param is: a material colour channel, or a
/// scene item's colour channel (`None` colour). `None` for any other param.
fn rgb_channel(spec: &manifold_core::effect_graph_def::ParamSpecDef) -> Option<(Option<core::MaterialColour>, core::RgbChannel)> {
    match (spec.material_role, spec.scene.and_then(|scene| scene.colour)) {
        (Some(core::MaterialParamRole::Colour(_, colour, channel)), _) => Some((Some(colour), channel)),
        (None, Some(channel)) => Some((None, channel)),
        _ => None,
    }
}

/// The three ids are one colour's R, G and B on one scene item with no
/// driver, envelope, audio mod, automation or mapping on them, so one RGB
/// write owns all three.
fn rgb_ids_editable(inst: &PresetInstance, ids: &[ParamId; 3]) -> bool {
    let mut owner: Option<&str> = None;
    let mut colour = None;
    for (index, id) in ids.iter().enumerate() {
        if ids[..index].contains(id) || inst.modulator_stored(id) {
            return false;
        }
        let Some((c, channel)) = inst.params.get(id).and_then(|p| rgb_channel(&p.spec)) else {
            return false;
        };
        if channel.index() != index || colour.is_some_and(|previous| previous != c) {
            return false;
        }
        colour = Some(c);
        let Some((item, _)) = split_param_id(id) else {
            return false;
        };
        if owner.is_some_and(|known| known != item) {
            return false;
        }
        owner = Some(item);
    }
    true
}

/// Shared structural/gesture preflight. IDs must still refer to one editable RGB group.
pub(crate) fn rgb_editable(project: &Project, target: &GraphTarget, ids: &[ParamId; 3]) -> bool {
    let Some(inst) = project.preset_instance(target) else {
        return false;
    };
    rgb_ids_editable(inst, ids)
        && ids.iter().all(|id| crate::scene_modifier_edit::macro_parameter_lock_reason(project, target, id).is_none())
}
