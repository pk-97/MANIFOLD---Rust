//! Material facts adapted at the engine/UI boundary. No registry descriptor copies.
use manifold_core::material_inspector as core;
use manifold_ui::param_surface as ui;

/// An untextured unlit surface (including a new plane) takes its first
/// layer skin as base colour. Lit materials retain the emissive default.
pub(crate) fn default_skin_target(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    material: &manifold_renderer::node_graph::scene_vm::MaterialVm,
) -> manifold_ui::panels::scene_setup_panel::SkinTargetMap {
    use manifold_renderer::node_graph::scene_vm::MaterialVm;
    use manifold_ui::panels::scene_setup_panel::SkinTargetMap;
    if let (Some(def), MaterialVm::Known(row)) = (def, material)
        && node_at(def, &row.scope_path, row.node_doc_id)
            .is_some_and(|(node, _)| node.type_id == "node.unlit_material")
    {
        SkinTargetMap::BaseColor
    } else {
        SkinTargetMap::Emissive
    }
}

fn feature(value: core::MaterialFeature) -> ui::MaterialFeature {
    match value {
        core::MaterialFeature::Coat => ui::MaterialFeature::Coat,
        core::MaterialFeature::Iridescence => ui::MaterialFeature::Iridescence,
        core::MaterialFeature::Emission => ui::MaterialFeature::Emission,
        core::MaterialFeature::Glass => ui::MaterialFeature::Glass,
        core::MaterialFeature::Sheen => ui::MaterialFeature::Sheen,
        core::MaterialFeature::Anisotropy => ui::MaterialFeature::Anisotropy,
        core::MaterialFeature::Translucency => ui::MaterialFeature::Translucency,
    }
}

fn colour(value: core::MaterialColour) -> ui::MaterialColour {
    match value {
        core::MaterialColour::Base => ui::MaterialColour::Base,
        core::MaterialColour::Specular => ui::MaterialColour::Specular,
        core::MaterialColour::Emission => ui::MaterialColour::Emission,
        core::MaterialColour::Sheen => ui::MaterialColour::Sheen,
        core::MaterialColour::Attenuation => ui::MaterialColour::Attenuation,
        core::MaterialColour::Subsurface => ui::MaterialColour::Subsurface,
        core::MaterialColour::Translucency => ui::MaterialColour::Translucency,
    }
}

fn channel(value: core::RgbChannel) -> ui::RgbChannel {
    match value {
        core::RgbChannel::R => ui::RgbChannel::R,
        core::RgbChannel::G => ui::RgbChannel::G,
        core::RgbChannel::B => ui::RgbChannel::B,
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
        core::MaterialGroup::Subsurface => ui::MaterialGroup::Subsurface,
    }
}
pub(super) fn role(value: core::MaterialParamRole) -> ui::MaterialParamRole {
    match value {
        core::MaterialParamRole::Scalar(g) => ui::MaterialParamRole::Scalar(group(g)),
        core::MaterialParamRole::Colour(g, c, r) => {
            ui::MaterialParamRole::Colour(group(g), colour(c), channel(r))
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

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::effects::PresetInstance;
use manifold_core::{GraphTarget, NodeId};
use manifold_ui::param_surface::{ModifierObjectRef, ParamSurface};

fn node_at<'a>(
    def: &'a EffectGraphDef,
    scope: &[u32],
    id: u32,
) -> Option<(&'a EffectGraphNode, ModifierObjectRef)> {
    let mut nodes = def.nodes.as_slice();
    let mut stable_scope = Vec::with_capacity(scope.len());
    for group_id in scope {
        let group = nodes.iter().find(|n| n.id == *group_id)?;
        stable_scope.push(group.node_id.clone());
        nodes = &group.group.as_ref()?.nodes;
    }
    let node = nodes.iter().find(|n| n.id == id)?;
    Some((
        node,
        ModifierObjectRef {
            scope: stable_scope,
            node: node.node_id.clone(),
        },
    ))
}

pub(super) fn inspector_info(
    project: &manifold_core::project::Project,
    def: &EffectGraphDef,
    object: &manifold_renderer::node_graph::scene_vm::SceneObjectKnownRow,
) -> Option<manifold_ui::panels::scene_setup_panel::MaterialInspectorInfo> {
    use manifold_renderer::node_graph::scene_vm::{MaterialTextureSource, MaterialVm};
    use manifold_ui::panels::scene_setup_panel::{MaterialInspectorInfo, MaterialTextureInfo};
    let MaterialVm::Known(material) = &object.material else {
        return None;
    };
    if !material.is_pbr {
        return None;
    }
    let (_, object_ref) = node_at(def, &object.visible_addr.scope_path, object.object_node_id)?;
    let (_, material_ref) = node_at(def, &material.scope_path, material.node_doc_id)?;
    let textures = material
        .texture_slots
        .iter()
        .map(|slot| {
            let source_label = match &slot.source {
                MaterialTextureSource::Unconnected => "No texture".to_owned(),
                MaterialTextureSource::GraphSource => "Graph source".to_owned(),
                MaterialTextureSource::Known {
                    scope_path,
                    node_doc_id,
                    type_id,
                } => node_at(def, scope_path, *node_doc_id)
                    .map(|(node, _)| {
                        let detail =
                            ["layer", "path"]
                                .iter()
                                .find_map(|key| match node.params.get(*key) {
                                    Some(SerializedParamValue::String { value })
                                        if !value.is_empty() =>
                                    {
                                        Some(value.as_str())
                                    }
                                    _ => None,
                                });
                        match detail {
                            Some(layer) if type_id == "node.layer_source" => {
                                let label = project
                                    .timeline
                                    .find_layer_by_id(layer)
                                    .map(|(_, layer)| layer.name.as_str())
                                    .unwrap_or(layer);
                                format!("Connected · {label}")
                            }
                            Some(path) => {
                                format!("Connected · {}", path.rsplit('/').next().unwrap_or(path))
                            }
                            None => format!(
                                "Connected · {}",
                                node.title
                                    .as_deref()
                                    .or(node.handle.as_deref())
                                    .unwrap_or(type_id)
                            ),
                        }
                    })
                    .unwrap_or_else(|| "Graph source".to_owned()),
            };
            MaterialTextureInfo {
                port: slot.port.clone(),
                label: slot.port.trim_end_matches("_map").replace('_', " "),
                source_label,
                connected: !matches!(slot.source, MaterialTextureSource::Unconnected),
                graph_source: matches!(slot.source, MaterialTextureSource::GraphSource),
            }
        })
        .collect();
    Some(MaterialInspectorInfo {
        object_gain: def.preset_metadata.as_ref().into_iter().flat_map(|meta| &meta.bindings)
            .find_map(|binding| match &binding.target {
                BindingTarget::Node {node_id, param} if node_id == &object_ref.node && param == "emission_strength" => Some(binding.id.clone().into()),
                _ => None,
            }),
        object: object_ref,
        params: def
            .preset_metadata
            .as_ref()
            .into_iter()
            .flat_map(|meta| &meta.bindings)
            .filter_map(|binding| match &binding.target {
                BindingTarget::Node { node_id, param } if node_id == &material_ref.node => {
                    Some((param.clone(), binding.id.clone().into()))
                }
                _ => None,
            })
            .collect(),
        material: material_ref,
        shared_object_count: material.shared_object_count,
        textures,
    })
}

fn unique_node<'a>(
    def: &'a EffectGraphDef,
    id: &NodeId,
) -> Option<(&'a EffectGraphNode, &'a [EffectGraphWire])> {
    fn visit<'a>(
        nodes: &'a [EffectGraphNode],
        wires: &'a [EffectGraphWire],
        id: &NodeId,
        found: &mut Option<(&'a EffectGraphNode, &'a [EffectGraphWire])>,
    ) -> bool {
        for node in nodes {
            if node.node_id == *id {
                if found.is_some() {
                    return false;
                }
                *found = Some((node, wires));
            }
            if let Some(group) = &node.group
                && !visit(&group.nodes, &group.wires, id, found)
            {
                return false;
            }
        }
        true
    }
    let mut found = None;
    if visit(&def.nodes, &def.wires, id, &mut found) {
        found
    } else {
        None
    }
}

pub(crate) fn external_attachment(inst: &PresetInstance, id: &str) -> bool {
    inst.drivers.iter().flatten().any(|x| x.param_id == id)
        || inst.envelopes.iter().flatten().any(|x| x.param_id == id)
        || inst.audio_mods.iter().flatten().any(|x| x.param_id == id)
        || inst
            .ableton_mappings
            .iter()
            .flatten()
            .any(|x| x.param_id == id)
        || inst
            .automation_lanes
            .iter()
            .flatten()
            .any(|x| x.param_id == id)
}

fn direct_binding<'a>(
    inst: &PresetInstance,
    def: &'a EffectGraphDef,
    id: &str,
) -> Option<(&'a NodeId, &'a str)> {
    let spec = &inst.params.get(id)?.spec;
    if spec.invert || spec.curve != manifold_core::macro_bank::MacroCurve::Linear {
        return None;
    }
    let meta = def.preset_metadata.as_ref()?;
    let mut bindings = meta.bindings.iter().filter(|b| b.id == id);
    let binding = bindings.next()?;
    if bindings.next().is_some() || binding.scale != 1.0 || binding.offset != 0.0 {
        return None;
    }
    if !matches!(
        binding.convert,
        manifold_core::effects::ParamConvert::Float
            | manifold_core::effects::ParamConvert::EnumRound
    ) {
        return None;
    }
    let BindingTarget::Node { node_id, param } = &binding.target else {
        return None;
    };
    Some((node_id, param))
}

pub(super) fn enrich_surface(
    surface: &mut ParamSurface,
    inst: &PresetInstance,
    def: &EffectGraphDef,
) {
    use ui::{MaterialParamRole as Role, RgbChannel};
    let feature_modes: Vec<_> = surface
        .rows
        .iter()
        .filter_map(|row| {
            let Some(Role::FeatureMode(feature)) = row.spec.material_role else {
                return None;
            };
            let (node, _) = direct_binding(inst, def, &row.id)?;
            Some((node.clone(), feature, row.value.base))
        })
        .collect();
    let baked: Vec<_> = def
        .preset_metadata
        .as_ref()
        .into_iter()
        .flat_map(|m| &m.bindings)
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param }
                if param == "baked_look" && inst.get_param(&binding.id) > 0.5 =>
            {
                Some(node_id.clone())
            }
            _ => None,
        })
        .collect();
    for row in &mut surface.rows {
        if row.spec.material_role.is_none() {
            continue;
        }
        row.material_attached = external_attachment(inst, &row.id);
        if let Some((id, param)) = direct_binding(inst, def, &row.id) {
            if let Some((node, wires)) = unique_node(def, id) {
                row.value.driven |= wires
                    .iter()
                    .any(|w| w.to_node == node.id && w.to_port == param);
                if row.value.driven {
                    row.spec.inactive_reason = Some("Driven by a graph wire".into());
                } else if matches!(row.spec.material_role, Some(Role::Placement(..)))
                    && external_attachment(inst, &row.id)
                {
                    row.spec.inactive_reason =
                        Some("Mapped placement — edit individual matrix values".into());
                } else if let Some(
                    Role::Scalar(ui::MaterialGroup::Feature(feature))
                    | Role::Colour(ui::MaterialGroup::Feature(feature), ..),
                ) = row.spec.material_role
                {
                    if feature_modes.iter().any(|(owner, f, mode)| {
                        owner == id && *f == feature && (*mode == 1.0 || *mode == 3.0)
                    }) {
                        row.spec.inactive_reason = Some("Off — settings are retained".into());
                    } else if feature != ui::MaterialFeature::Emission && baked.contains(id) {
                        row.spec.inactive_reason = Some("Bypassed by Baked Look".into());
                    }
                } else if matches!(param, "metallic" | "roughness" | "specular")
                    && baked.contains(id)
                {
                    row.spec.inactive_reason = Some("Bypassed by Baked Look".into());
                }
            } else {
                row.spec.inactive_reason =
                    Some("Ambiguous material binding — edit individual parameters".into());
            }
        } else {
            row.spec.inactive_reason = Some("Custom binding — edit individual parameters".into());
        }
    }
    for index in 0..surface.rows.len() {
        let Some(Role::Colour(_, colour, RgbChannel::R)) = surface.rows[index].spec.material_role
        else {
            continue;
        };
        let Some((node, _)) = direct_binding(inst, def, &surface.rows[index].id) else {
            continue;
        };
        let mut members: [Option<manifold_core::effects::ParamId>; 3] = [None, None, None];
        let mut valid = unique_node(def, node).is_some();
        for row in &surface.rows {
            let Some(Role::Colour(_, c, channel)) = row.spec.material_role else {
                continue;
            };
            if c != colour {
                continue;
            }
            if !direct_binding(inst, def, &row.id).is_some_and(|(n, _)| n == node) {
                continue;
            }
            let slot = match channel {
                RgbChannel::R => 0,
                RgbChannel::G => 1,
                RgbChannel::B => 2,
            };
            if members[slot].is_some() || row.value.driven || external_attachment(inst, &row.id) {
                valid = false;
            }
            members[slot] = Some(row.id.clone());
        }
        if valid && let [Some(r), Some(g), Some(b)] = members {
            surface.rows[index].rgb_members = Some([r, g, b]);
        } else if surface.rows[index].spec.inactive_reason.is_none() {
            surface.rows[index].spec.inactive_reason =
                Some("Colour channels have separate control — edit them individually".into());
        }
    }
}

/// Shared structural/gesture preflight. IDs must still refer to one editable RGB group.
pub(crate) fn rgb_editable(
    project: &manifold_core::project::Project,
    target: &GraphTarget,
    ids: &[manifold_core::effects::ParamId; 3],
) -> bool {
    let Some(inst) = project.preset_instance(target) else {
        return false;
    };
    let default = manifold_renderer::node_graph::loaded_preset_view_by_id(inst.effect_type());
    let Some(def) = project.graph_for_target(target, None).or_else(|| {
        project.graph_for_target(target, default.as_ref().map(|v| v.canonical_def.as_ref()))
    }) else {
        return false;
    };
    let mut owner: Option<&NodeId> = None;
    let mut colour = None;
    for (index, id) in ids.iter().enumerate() {
        if ids[..index].contains(id) || external_attachment(inst, id) {
            return false;
        }
        let Some(core::MaterialParamRole::Colour(_, c, channel)) =
            inst.params.get(id).and_then(|p| p.spec.material_role)
        else {
            return false;
        };
        if channel
            != [
                core::RgbChannel::R,
                core::RgbChannel::G,
                core::RgbChannel::B,
            ][index]
            || colour.is_some_and(|previous| previous != c)
        {
            return false;
        }
        colour = Some(c);
        let Some((node, param)) = direct_binding(inst, def, id) else {
            return false;
        };
        if owner.is_some_and(|n| n != node) {
            return false;
        }
        owner = Some(node);
        let Some((n, wires)) = unique_node(def, node) else {
            return false;
        };
        if n.type_id != "node.pbr_material"
            || wires
                .iter()
                .any(|w| w.to_node == n.id && w.to_port == param)
        {
            return false;
        }
        if crate::scene_modifier_edit::macro_parameter_lock_reason(project, target, id).is_some() {
            return false;
        }
    }
    true
}

/// Resolve authored/embedded topology before falling back to the catalog.
pub(crate) fn graph_def(
    project: &manifold_core::project::Project,
    target: &GraphTarget,
) -> Option<EffectGraphDef> {
    if let Some(def) = project.graph_for_target(target, None) {
        return Some(def.clone());
    }
    let inst = project.preset_instance(target)?;
    let view = manifold_renderer::node_graph::loaded_preset_view_by_id(inst.effect_type())?;
    project
        .graph_for_target(target, Some(view.canonical_def.as_ref()))
        .cloned()
}
