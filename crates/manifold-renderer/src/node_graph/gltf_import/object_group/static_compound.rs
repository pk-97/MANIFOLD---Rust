use super::*;

/// Build one top-level group for a static multi-material asset. Each material
/// still owns its mesh/material/map/scene-object nodes and its own Object
/// output; only the presentation boundary and rigid transform are shared.
///
/// This deliberately composes [`build_object_group`] instead of introducing a
/// second material assembly path. The caller only selects this helper when all
/// materials are static rigid objects, so animated, skinned, morphed, and
/// node-slot animated assets retain the established per-material groups.
pub(in super::super) fn build_static_compound_group(
    ctx: &mut ImportCtx<'_>,
    local_k_offset: usize,
    port_index_offset: usize,
    materials: &[gltf_load::GltfMaterialInfo],
    asset_name: &str,
    anim_prefix: &str,
) -> ObjectGroupOutput {
    debug_assert!(materials.len() > 1);
    let compound_name = unique_group_name(
        Some(asset_name),
        0,
        local_k_offset,
        &mut *ctx.used_group_names,
    );
    let visibility_id = format!("{}_visible", sanitize_identifier(&compound_name).to_lowercase());

    let mut parts = Vec::with_capacity(materials.len());
    for (i, material) in materials.iter().enumerate() {
        parts.push(build_object_group(
            ctx,
            local_k_offset + i,
            i,
            port_index_offset + i,
            material,
            anim_prefix,
        ));
    }

    let mut primary = parts.remove(0);
    let primary_group_id = primary.group_node.id;
    let primary_transform_id = {
        let body = primary
            .group_node
            .group
            .as_mut()
            .expect("object group builder always creates a group body");
        let transform_id = body
            .nodes
            .iter()
            .find(|node| node.type_id == "node.transform_3d")
            .map(|node| node.id)
            .expect("static object group always creates a transform");
        // Use the whole asset center as the one rotation/scale pivot.
        for node in &mut body.nodes {
            if node.type_id == "node.gltf_mesh_source" {
                node.params.insert("translate_x".to_string(), float(-ctx.center[0]));
                node.params.insert("translate_y".to_string(), float(-ctx.center[1]));
                node.params.insert("translate_z".to_string(), float(-ctx.center[2]));
            }
            if node.id == transform_id {
                node.params.insert("pos_x".to_string(), float(0.0));
                node.params.insert("pos_y".to_string(), float(0.0));
                node.params.insert("pos_z".to_string(), float(0.0));
            }
        }
        transform_id
    };

    let (transform_node_id, transform_node_params) = {
        let body = primary
            .group_node
            .group
            .as_ref()
            .expect("object group builder always creates a group body");
        let transform = body
            .nodes
            .iter()
            .find(|node| node.id == primary_transform_id)
            .expect("shared transform has a stable node id");
        (transform.node_id.clone(), transform.params.clone())
    };
    let transform_node_id_string = transform_node_id.as_str().to_string();

    // Re-stamp the one retained transform exposure so its default mirrors the
    // shared transform's asset-wide recenter. Remove every per-material
    // transform exposure first; otherwise the card would retain dangling
    // targets for the discarded transform nodes.
    let primary_transform_binding_ids: std::collections::HashSet<String> = primary
        .card_bindings
        .iter()
        .filter_map(|binding| {
            matches!(
                &binding.target,
                BindingTarget::Node { node_id, .. } if node_id.as_str().starts_with("transform_")
            )
            .then_some(binding.id.clone())
        })
        .collect();
    primary.card_bindings.retain(|binding| {
        !matches!(
                &binding.target,
                BindingTarget::Node { node_id, .. }
                    if node_id.as_str().starts_with("transform_")
                && node_id.as_str() != transform_node_id_string.as_str()
        )
    });
    // The retained transform's position defaults changed with the shared
    // transform; replace all generated transform exposures to keep defaults
    // coherent and avoid dangling targets for discarded transforms.
    primary
        .card_params
        .retain(|param| !primary_transform_binding_ids.contains(&param.id));
    primary.card_bindings.retain(|binding| {
        !matches!(
            &binding.target,
            BindingTarget::Node { node_id, .. }
                if node_id.as_str() == transform_node_id_string.as_str()
        )
    });
    stamp_scene_node_exposures_into(
        &mut primary.card_params,
        &mut primary.card_bindings,
        primary_transform_id,
        &transform_node_id,
        "node.transform_3d",
        &compound_name,
        &metadata_for_node_type("node.transform_3d"),
        &transform_node_params,
    );

    let mut outputs = vec![InterfacePortDef {
        name: "object".to_string(),
        port_type: "Object".to_string(),
    }];
    let mut render_wires = vec![wire(primary_group_id, "object", ctx.render_id, &format!("object_{port_index_offset}"))];
    let primary_scene_object_id = primary
        .group_node
        .group
        .as_ref()
        .expect("group body")
        .nodes
        .iter()
        .find(|node| node.type_id == "node.scene_object")
        .map(|node| node.id)
        .expect("object group always has a scene object");
    let primary_output_id = primary
        .group_node
        .group
        .as_ref()
        .expect("group body")
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
        .map(|node| node.id)
        .expect("object group always has a group output");
    let mut boundary_pairs = vec![(primary_scene_object_id, primary_output_id)];
    let mut shared_visible_bindings = vec![card_binding(
        &visibility_id,
        "Visible",
        1.0,
        &format!("object_{}_bind", local_k_offset),
        "visible",
        1.0,
    )];

    // The group names the asset; children keep material-derived names.
    if let Some(scene_object) = primary
        .group_node
        .group
        .as_mut()
        .expect("group body")
        .nodes
        .iter_mut()
        .find(|node| node.type_id == "node.scene_object")
    {
        scene_object.handle = Some(materials[0].name.clone().unwrap_or_else(|| "Submesh 1".into()));
    }

    for (i, mut part) in parts.into_iter().enumerate() {
        let output_name = format!("object_{}", i + 1);
        let body = part
            .group_node
            .group
            .take()
            .expect("object group builder always creates a group body");
        let ObjectGroupOutput {
            group_node: _,
            wires_to_render: _,
            card_params: part_card_params,
            card_bindings: mut part_card_bindings,
            string_bindings: part_string_bindings,
            report_lines: part_report_lines,
            textures_wired: part_textures_wired,
            animated: part_animated,
        } = part;
        debug_assert!(!part_animated);

        let mut body = *body;
        let extra_transform_id = body
            .nodes
            .iter()
            .find(|node| node.type_id == "node.transform_3d")
            .map(|node| node.id)
            .expect("static object group always creates a transform");
        let extra_scene_object_id = body
            .nodes
            .iter()
            .find(|node| node.type_id == "node.scene_object")
            .map(|node| node.id)
            .expect("object group always creates a scene object");

        for node in &mut body.nodes {
            if node.type_id == "node.gltf_mesh_source" {
                node.params.insert("translate_x".to_string(), float(-ctx.center[0]));
                node.params.insert("translate_y".to_string(), float(-ctx.center[1]));
                node.params.insert("translate_z".to_string(), float(-ctx.center[2]));
            }
            if node.type_id == GROUP_OUTPUT_TYPE_ID {
                node.handle = Some(format!("output_{}", i + 1));
            }
        }
        let output_id = body
            .nodes
            .iter()
            .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
            .map(|node| node.id)
            .expect("object group always has a group output");
        body.wires.retain(|w| {
            w.from_node != extra_transform_id
                && w.to_node != extra_transform_id
                && w.to_node != output_id
        });
        body.wires.push(wire(primary_transform_id, "transform", extra_scene_object_id, "transform"));
        body.wires.push(wire(extra_scene_object_id, "object", output_id, &output_name));
        body.interface.outputs = vec![InterfacePortDef {
            name: output_name.clone(),
            port_type: "Object".to_string(),
        }];

        body.nodes.retain(|node| node.id != extra_transform_id);
        let primary_body = primary
            .group_node
            .group
            .as_mut()
            .expect("group body");
        primary_body.nodes.extend(body.nodes);
        primary_body.wires.extend(body.wires);
        boundary_pairs.push((extra_scene_object_id, output_id));
        outputs.push(InterfacePortDef { name: output_name.clone(), port_type: "Object".to_string() });
        render_wires.push(wire(primary_group_id, &output_name, ctx.render_id, &format!("object_{}", port_index_offset + i + 1)));

        let part_transform_binding_ids: std::collections::HashSet<String> = part_card_bindings
            .iter()
            .filter_map(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::Node { node_id, .. } if node_id.as_str().starts_with("transform_")
                )
                .then_some(binding.id.clone())
            })
            .collect();
        part_card_bindings.retain(|binding| {
            !matches!(
                &binding.target,
                BindingTarget::Node { node_id, .. }
                    if node_id.as_str().starts_with("transform_")
            )
        });
        shared_visible_bindings.push(card_binding(
            &visibility_id,
            "Visible",
            1.0,
            &format!("object_{}_bind", local_k_offset + i + 1),
            "visible",
            1.0,
        ));
        primary.card_params.extend(
            part_card_params
                .into_iter()
                .filter(|param| !part_transform_binding_ids.contains(&param.id)),
        );
        primary.card_bindings.extend(part_card_bindings);
        primary.string_bindings.extend(part_string_bindings);
        primary.report_lines.extend(part_report_lines);
        primary.textures_wired += part_textures_wired;
    }

    // Keep each material draw editable below the asset's shared transform.
    let body = primary.group_node.group.as_mut().expect("group body");
    for (index, (object_id, _)) in boundary_pairs.iter().enumerate() {
        for edge in &mut body.wires {
            if edge.to_node == *object_id && edge.to_port == "transform" {
                edge.to_port = "parent_transform".into();
            }
        }
        let local = plain_node(
            (ctx.fresh_id)(),
            &format!("part_transform_{}", local_k_offset + index),
            "node.transform_3d",
            &format!("part_transform_{}", local_k_offset + index),
        );
        stamp_scene_node_exposures_into(
            &mut primary.card_params, &mut primary.card_bindings, local.id,
            &local.node_id, &local.type_id, &materials[index].name.clone().unwrap_or_else(|| format!("Submesh {}", index + 1)),
            &metadata_for_node_type("node.transform_3d"), &local.params,
        );
        body.wires.push(wire(local.id, "transform", *object_id, "transform"));
        body.nodes.push(local);
    }
    for binding in &mut shared_visible_bindings {
        if let BindingTarget::Node { param, .. } = &mut binding.target {
            *param = "parent_visible".into();
        }
    }
    primary.card_params.push(card_param(
        &visibility_id,
        "Visible",
        0.0,
        1.0,
        1.0,
        false,
        &compound_name,
    ));
    primary.card_bindings.extend(shared_visible_bindings);
    primary.group_node.handle = Some(compound_name);
    let primary_body = primary.group_node.group.as_mut().expect("group body");
    primary_body.interface.outputs = outputs;
    let boundary_ids: std::collections::HashSet<u32> =
        boundary_pairs.iter().map(|(_, output_id)| *output_id).collect();
    primary_body.wires.retain(|wire| !boundary_ids.contains(&wire.to_node));
    for (index, (scene_object_id, output_id)) in boundary_pairs.into_iter().enumerate() {
        let port = if index == 0 { "object".to_string() } else { format!("object_{index}") };
        primary_body.wires.push(wire(scene_object_id, "object", output_id, &port));
    }
    primary.wires_to_render = render_wires;
    primary
}
