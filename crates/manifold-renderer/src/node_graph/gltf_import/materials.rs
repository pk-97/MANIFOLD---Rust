//! Material map-texture wiring: one glTF texture map (normal / MR /
//! occlusion / emissive / …) into an object's `node.scene_object` input.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphNode, EffectGraphWire, StringBindingDef};

use super::MODEL_FILE_PARAM_ID;
use super::assembly::{enum_val, int, plain_node, wire};

/// Wire one glTF map texture (normal / metallic-roughness / occlusion /
/// emissive / …) into this object's group: creates a `node.gltf_texture_source`
/// (or reuses one already created for the same `texture_index` within this
/// object — D5's ORM-packing case, where the occlusion and
/// metallic-roughness maps are the same physical image), wires the source
/// directly into the object's `node.scene_object` input named `port_name`
/// (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D1/D3 — snake_case, matching
/// `SceneObjectNode`'s port names, e.g. `normal_map`/`mr_map`), and adds the
/// outer-card Model File → source-node `path` string binding (the same
/// convention `assemble_import_graph`'s base-color wiring above uses).
/// `cache` is scoped to ONE object (`k`) — keyed by glTF `texture_index` so
/// a second map wired from the same physical image reuses the first map's
/// decode rather than doubling the GPU decode + memory cost.
#[allow(clippy::too_many_arguments)]
pub(super) fn wire_map_texture(
    tex_index: u32,
    color_space: u32,
    node_prefix: &str,
    port_name: &str,
    k: usize,
    path_str: &str,
    fresh_id: &mut impl FnMut() -> u32,
    group_nodes: &mut Vec<EffectGraphNode>,
    group_wires: &mut Vec<EffectGraphWire>,
    string_bindings: &mut Vec<StringBindingDef>,
    cache: &mut std::collections::HashMap<(u32, u32, u32), (u32, String)>,
    scene_object_id: u32,
    channel_mode: u32,
) {
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 bugfix: the cache key MUST
    // include `color_space`/`channel_mode`, not just `tex_index` — the base
    // five maps only ever reuse a shared texture index under the SAME
    // decode (e.g. ORM occlusion+mr both linear), but `KHR_materials_
    // specular`'s `specularTexture` (linear, alpha channel) and
    // `specularColorTexture` (sRGB, rgb channels) can legally reference the
    // SAME physical image with DIFFERENT decodes (CompareSpecular.glb does
    // exactly this) — a tex_index-only key would silently reuse the first
    // decode for both ports and corrupt the second one.
    let cache_key = (tex_index, color_space, channel_mode);
    let (node_numeric_id, _node_id_str) = if let Some(existing) = cache.get(&cache_key) {
        existing.clone()
    } else {
        let node_id_str = format!("{node_prefix}_{k}");
        let tid = fresh_id();
        let mut node = plain_node(tid, &node_id_str, "node.gltf_texture_source", &node_id_str);
        node.params.insert("texture_index".to_string(), int(tex_index as i32));
        node.params.insert("color_space".to_string(), enum_val(color_space));
        // GLB_XFAIL_BURNDOWN_DESIGN.md D2: 1 = gloss_to_roughness, wired
        // only for a specularGlossinessTexture standing in for `mrMap`
        // (see the mr_texture call site below); every other call passes 0
        // (passthrough), byte-identical to before this param existed.
        node.params.insert("mode".to_string(), enum_val(channel_mode));
        // Same v1 default the base-color wiring uses — see its TODO about
        // threading real per-texture dimensions through the summary.
        node.params.insert("width".to_string(), int(1024));
        node.params.insert("height".to_string(), int(1024));
        group_nodes.push(node);

        string_bindings.push(StringBindingDef {
            id: MODEL_FILE_PARAM_ID.to_string(),
            label: "Model File".to_string(),
            default_value: path_str.to_string(),
            target: BindingTarget::Node {
                node_id: NodeId::new(&node_id_str),
                param: "path".to_string(),
            },
        });

        let entry = (tid, node_id_str);
        cache.insert(cache_key, entry.clone());
        entry
    };

    group_wires.push(wire(node_numeric_id, "out", scene_object_id, port_name));
}
