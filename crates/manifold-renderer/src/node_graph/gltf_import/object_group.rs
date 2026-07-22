//! Per-object node group assembly: build ONE glTF material's node group
//! (mesh / skin / morph sources + PBR material + texture maps + transform,
//! wrapped in a named `GroupDef`) and its wiring into `render_scene`.

use std::collections::BTreeMap;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID,
    GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef, ParamSpecDef, StringBindingDef,
};
use manifold_core::scene_exposure::stamp_scene_node_exposures_into;

use crate::node_graph::gltf_load;
use crate::node_graph::scene_exposure::metadata_for_node_type;

use super::MODEL_FILE_PARAM_ID;
use super::animation::*;
use super::assembly::*;
use super::cards::*;
use super::materials::*;

/// Output of building one object's node group + its wiring into
/// `render_scene` — the reusable core of the per-object loop, factored out
/// of [`build_import_graph`] so [`merge_import_into_graph`] (D5) can build
/// the SAME per-object shape against an EXISTING scene's `render_scene`
/// node and object-index range, without re-running the whole assembler —
/// no camera/envmap/lights/lens, chrome the merge path never touches.
pub(super) struct ObjectGroupOutput {
    /// The named, tinted group node (`GROUP_TYPE_ID`) — push directly onto
    /// the target level's `nodes`.
    pub(super) group_node: EffectGraphNode,
    /// Top-level wires from this group's outputs to `render_scene`'s
    /// `mesh_{port_index}` / `material_{port_index}` / … ports — push
    /// directly onto the target level's `wires`.
    pub(super) wires_to_render: Vec<EffectGraphWire>,
    pub(super) card_params: Vec<ParamSpecDef>,
    pub(super) card_bindings: Vec<BindingDef>,
    pub(super) string_bindings: Vec<StringBindingDef>,
    pub(super) report_lines: Vec<String>,
    pub(super) textures_wired: usize,
    /// True when this object contributed animation-clock bindings against
    /// the caller's shared `anim_prefix` — the caller pushes the four
    /// per-glb card PARAMS (one "Animation" section) iff any object did.
    pub(super) animated: bool,
}

/// Build ONE object's group (mesh source + material + optional skin/morph/
/// animation + texture maps + transform, wrapped in a named `GroupDef`)
/// and its top-level wiring into `render_scene`. `local_k` numbers this
/// object's INNER handles (`mesh_{local_k}`, `mat_{local_k}`, …) —
/// purely cosmetic, namespaced away by the group-name-prefixing flattener
/// (`docs/GROUPING_GRAPHS.md` §2), so it always starts at 0 for a fresh
/// call — a merge's incoming materials get their own local numbering,
/// never the target scene's. `port_index` is the render_scene OBJECT SLOT
/// this group wires into (`mesh_{port_index}` etc. on `render_scene`
/// itself) — for a single import these are the same number; for a merge,
/// `port_index` is offset by the target scene's existing `objects` count
/// while `local_k` restarts at 0.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_object_group(
    local_k: usize,
    port_index: usize,
    render_id: u32,
    m: &gltf_load::GltfMaterialInfo,
    path_str: &str,
    center: [f32; 3],
    node_anims_by_clip: &[BTreeMap<usize, gltf_load::GltfNodeAnimation>],
    used_group_names: &mut std::collections::HashSet<String>,
    fresh_id: &mut impl FnMut() -> u32,
    // BUG-194/BUG-195: the whole-scene bbox radius from THIS parse (build_import_graph's
    // `radius` / merge_import_into_graph's `incoming_radius`) — stamped onto every
    // mesh-source node this call creates as `source_bbox_radius`, so SceneVm's
    // header and a future merge's scale-sanity have a real per-node provenance
    // fact to read instead of BUG-195's orbit-camera proxy.
    bbox_radius: f32,
    // The shared per-glb animation card id prefix (see
    // `animation_card_params`) — "anim" for a fresh import; the merge path
    // uniquifies against the target scene's existing card ids.
    anim_prefix: &str,
) -> ObjectGroupOutput {
    let k = local_k;
    let mut animated = false;
    let mut card_params: Vec<ParamSpecDef> = Vec::new();
    let mut card_bindings: Vec<BindingDef> = Vec::new();
    let mut string_bindings: Vec<StringBindingDef> = Vec::new();
    let mut report_lines: Vec<String> = Vec::new();
    let mut textures_wired = 0usize;

        let mesh_node_id = format!("mesh_{k}");
        let mat_node_id = format!("mat_{k}");
        // Computed up front (not just before the group box below) so the
        // per-object card knobs pushed further down can stamp it as their
        // `section` (D5/D9) — the section now carries the per-object
        // identity the old " 2"-style label suffix used to.
        let group_name = unique_group_name(m.name.as_deref(), k, used_group_names);

        // D9 — every unmapped feature this material carries is a report
        // line, never a silent drop. GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6
        // (D1 revised — full spec surface): clearcoat/specular/
        // transmission/volume-thickness textures are now REAL mappings
        // (wired below, same doctrine as sheen/iridescence/anisotropy in
        // E3/E4/E5) — no report line for any of them any more.
        //
        // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_texture_transform is
        // applied per-map (all five families) — the only variant still
        // unmapped is a texCoord index override (v1 imports TEXCOORD_0
        // only), which is reported rather than silently dropped.
        if m.uv_tex_coord_override {
            report_lines.push(format!(
                "{group_name}: KHR_texture_transform.texCoord override — only TEXCOORD_0 is imported in v1, the override is ignored (report-only; the transform itself IS applied)"
            ));
        }
        // `render_scene` shipped no per-object normal-scale / occlusion-strength
        // uniform (F-P2's texture ports carry no multiplier) — a non-neutral
        // value is genuinely unmapped, not silently dropped, so it's a report
        // line rather than an applied effect. Neutral (1.0, or no texture
        // wired) produces no line — the common case stays quiet.
        if m.normal_texture.is_some() && (m.normal_scale - 1.0).abs() > 1e-4 {
            report_lines.push(format!(
                "{group_name}: normalTexture.scale = {:.2} (≠1.0) not applied — render_scene has no per-object normal-scale port yet (report-only)",
                m.normal_scale
            ));
        }
        if m.occlusion_texture.is_some() && (m.occlusion_strength - 1.0).abs() > 1e-4 {
            report_lines.push(format!(
                "{group_name}: occlusionTexture.strength = {:.2} (≠1.0) not applied — render_scene has no per-object occlusion-strength port yet (report-only)",
                m.occlusion_strength
            ));
        }
        // IMPORT_FIDELITY_DESIGN.md D8: glTF BLEND and
        // KHR_materials_transmission both become a real `Blend` material.
        //
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2b/D3: `effective_alpha` used
        // to darken base_color.a by `(1 - transmission_factor)` — that WAS
        // the D8/F-P5 alpha-blend approximation for transmission (fake
        // see-through via low opacity). Real screen-space refraction ships
        // in `fs_pbr` (render_scene.wgsl's `transmission_diffuse`) as of
        // E2b, and D3 explicitly rejects keeping the approximation active
        // once the real thing exists — the two must never both apply (that
        // would double-darken: alpha-composited over the background AND
        // shader-mixed with it). So `effective_alpha` is now just the
        // material's own authored base_color.a — Blend/glass alpha keeps
        // its normal meaning (author/performer-controlled fade via the
        // Opacity card below), while transmission's see-through is carried
        // entirely by the shader's diffuse substitution, not by alpha.
        let is_glass = m.was_blend || m.transmission_factor > 0.0;
        let effective_alpha = m.base_color_factor[3].clamp(0.0, 1.0);

        // This object's producer nodes live INSIDE its group; only the group box
        // and the shared render / camera / lights / boundaries sit at the top
        // level (the spine). Every inner node keeps its stable `node_id`, so the
        // card + string bindings below still resolve after the load-time flatten.
        let mut group_nodes: Vec<EffectGraphNode> = Vec::new();
        let mut group_wires: Vec<EffectGraphWire> = Vec::new();

        let mesh_id = fresh_id();
        // GLTF_ANIMATION_DESIGN.md A2 (D2): a skinned object's positioning
        // comes ENTIRELY from its joint hierarchy (glTF 2.0 §3.7.3.3) — the
        // rigid-object `node.gltf_mesh_source` Material selector, which
        // world-transforms vertices by the mesh-owning node's OWN
        // transform, would double-transform a skinned mesh. BUG-207: `m.skin`
        // now resolves for the synthetic default-material entry too — a
        // materialless skinned rig is exactly as valid as a materialed one.
        let skinned_vertices_source: Option<u32> = if let Some(obj_skin) = &m.skin {
            let skinned_src = {
                let mut n = plain_node(
                    mesh_id,
                    &mesh_node_id,
                    "node.gltf_skinned_mesh_source",
                    &mesh_node_id,
                );
                // BUG-207: `m.material_index == DEFAULT_MATERIAL_SENTINEL`
                // (u32::MAX) marks the synthetic default-material entry —
                // translate it to `gltf_skinned_mesh_source`'s own reserved
                // param sentinel, mirroring the static branch below
                // (`u32::MAX as i32` would collide with -1, the param's
                // pre-existing "unset" value).
                let skinned_material_param = if m.material_index == gltf_load::DEFAULT_MATERIAL_SENTINEL {
                    gltf_load::DEFAULT_MATERIAL_MESH_PARAM
                } else {
                    m.material_index as i32
                };
                n.params
                    .insert("material_index".to_string(), int(skinned_material_param));
                n.params
                    .insert("max_capacity".to_string(), int(m.vertex_count.max(1) as i32));
                // BUG-194/BUG-195: import-time provenance, read by SceneVm
                // (header vertex count) + merge_import_into_graph (scale
                // sanity's reference radius). Never read by evaluate()/run().
                n.params
                    .insert("source_vertex_count".to_string(), int(m.vertex_count as i32));
                n.params
                    .insert("source_bbox_radius".to_string(), float(bbox_radius));
                n
            };
            group_nodes.push(skinned_src);

            let pose_node_id = format!("pose_{k}");
            let pose_id = fresh_id();
            let joint_count = obj_skin.info.joint_node_indices.len() as u32;
            let (clip_durations_rows, duration_s) =
                skeleton_pose_clip_durations(&obj_skin.info, node_anims_by_clip);
            let mut pose_node =
                plain_node(pose_id, &pose_node_id, "node.gltf_skeleton_pose", &pose_node_id);
            pose_node.params.insert("joint_count".to_string(), int(joint_count as i32));
            pose_node.params.insert("duration_s".to_string(), float(duration_s));
            pose_node.params.insert("clip_durations".to_string(), table(clip_durations_rows));
            // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: `path`/`skin_index` are
            // the ONLY selectors the importer stamps — keyframe/topology
            // payload lives entirely in the shared `gltf_anim_cache`
            // (loaded from `path`), never in the six now-deleted Table
            // params (`node.gltf_skeleton_pose`'s params still DECLARE
            // them for D5 round-trip/migration; nothing reads them).
            pose_node
                .params
                .insert("skin_index".to_string(), int(obj_skin.skin_index as i32));
            group_nodes.push(pose_node);
            string_bindings.push(StringBindingDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                label: "Model File".to_string(),
                default_value: path_str.to_string(),
                target: BindingTarget::Node {
                    node_id: NodeId::new(&pose_node_id),
                    param: "path".to_string(),
                },
            });
            animation_card_bindings(&mut card_bindings, anim_prefix, &pose_node_id);
            animated = true;

            let skinmesh_node_id = format!("skinmesh_{k}");
            let skinmesh_id = fresh_id();
            let mut skinmesh_node =
                plain_node(skinmesh_id, &skinmesh_node_id, "node.skin_mesh", &skinmesh_node_id);
            skinmesh_node.params.insert("joint_count".to_string(), int(joint_count as i32));
            group_nodes.push(skinmesh_node);

            // BUG-208: skin_mesh's `in` wire is deferred until AFTER the
            // morph block below decides whether this object is ALSO
            // morphed — a skin+morph combination chains
            // node.morph_targets_blend between this node's `vertices` and
            // skin_mesh's `in` (glTF applies morph then skin, §3.7.2); a
            // skin-only object wires directly, same as before.
            group_wires.push(wire(mesh_id, "joints", skinmesh_id, "joints"));
            group_wires.push(wire(mesh_id, "weights", skinmesh_id, "weights"));
            group_wires.push(wire(pose_id, "joint_matrices", skinmesh_id, "matrices"));
            report_lines.push(format!(
                "{group_name}: skinned (glTF skin index {}, {joint_count} joints) — \
                 node.gltf_skinned_mesh_source + node.gltf_skeleton_pose + node.skin_mesh",
                obj_skin.skin_index
            ));
            Some(skinmesh_id)
        } else if let Some(rmn) = &m.rigid_multi_node {
            // GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3): rigid multi-node
            // objects route through the SAME node.gltf_skinned_mesh_source
            // + node.gltf_skeleton_pose + node.skin_mesh trio a real skin
            // uses — `load_gltf_skinned_mesh`'s D4 fallback
            // (`find_material_contributing_nodes`) re-derives the IDENTICAL
            // slot order at runtime, so the `node_slots` Table stamped on
            // the pose node below always agrees with the vertex source's
            // own per-vertex slot indices without a shared import-time
            // handle.
            let rigid_src = {
                let mut n = plain_node(
                    mesh_id,
                    &mesh_node_id,
                    "node.gltf_skinned_mesh_source",
                    &mesh_node_id,
                );
                let rigid_material_param = if m.material_index == gltf_load::DEFAULT_MATERIAL_SENTINEL {
                    gltf_load::DEFAULT_MATERIAL_MESH_PARAM
                } else {
                    m.material_index as i32
                };
                n.params
                    .insert("material_index".to_string(), int(rigid_material_param));
                n.params
                    .insert("max_capacity".to_string(), int(m.vertex_count.max(1) as i32));
                n.params
                    .insert("source_vertex_count".to_string(), int(m.vertex_count as i32));
                n.params
                    .insert("source_bbox_radius".to_string(), float(bbox_radius));
                n
            };
            group_nodes.push(rigid_src);

            let pose_node_id = format!("pose_{k}");
            let pose_id = fresh_id();
            let joint_count = rmn.slot_nodes.len() as u32;
            let (clip_durations_rows, duration_s) =
                rigid_multi_node_clip_durations(&rmn.slot_nodes, node_anims_by_clip);
            let mut pose_node =
                plain_node(pose_id, &pose_node_id, "node.gltf_skeleton_pose", &pose_node_id);
            pose_node.params.insert("joint_count".to_string(), int(joint_count as i32));
            pose_node.params.insert("duration_s".to_string(), float(duration_s));
            pose_node.params.insert("clip_durations".to_string(), table(clip_durations_rows));
            // D4: `-2` sentinel selects node-slot mode over a real skin;
            // `node_slots` rows are `[scene_node_index]`, row i = slot i.
            pose_node.params.insert("skin_index".to_string(), int(-2));
            let node_slots_rows: Vec<Vec<f32>> =
                rmn.slot_nodes.iter().map(|&n| vec![n as f32]).collect();
            pose_node.params.insert("node_slots".to_string(), table(node_slots_rows));
            group_nodes.push(pose_node);
            string_bindings.push(StringBindingDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                label: "Model File".to_string(),
                default_value: path_str.to_string(),
                target: BindingTarget::Node {
                    node_id: NodeId::new(&pose_node_id),
                    param: "path".to_string(),
                },
            });
            animation_card_bindings(&mut card_bindings, anim_prefix, &pose_node_id);
            animated = true;

            let skinmesh_node_id = format!("skinmesh_{k}");
            let skinmesh_id = fresh_id();
            let mut skinmesh_node =
                plain_node(skinmesh_id, &skinmesh_node_id, "node.skin_mesh", &skinmesh_node_id);
            skinmesh_node.params.insert("joint_count".to_string(), int(joint_count as i32));
            group_nodes.push(skinmesh_node);

            group_wires.push(wire(mesh_id, "joints", skinmesh_id, "joints"));
            group_wires.push(wire(mesh_id, "weights", skinmesh_id, "weights"));
            group_wires.push(wire(pose_id, "joint_matrices", skinmesh_id, "matrices"));
            report_lines.push(format!(
                "{group_name}: rigid animation composed across {joint_count} nodes via the \
                 node-slot palette (GLTF_ANIM_RUNTIME_V2_DESIGN.md D4) — \
                 node.gltf_skinned_mesh_source + node.gltf_skeleton_pose (node-slot mode) + \
                 node.skin_mesh",
            ));
            Some(skinmesh_id)
        } else {
            let mut mesh_node =
                plain_node(mesh_id, &mesh_node_id, "node.gltf_mesh_source", &mesh_node_id);
            // GLB_XFAIL_BURNDOWN_DESIGN.md D4/§3: `m.material_index ==
            // DEFAULT_MATERIAL_SENTINEL` (u32::MAX) marks the synthetic
            // default-material entry — never re-queried as a document
            // material index. Translate it to `gltf_mesh_source`'s own
            // reserved param sentinel instead (`u32::MAX as i32` would
            // collide with -1, the param's pre-existing "unset" value).
            let mesh_material_param = if m.material_index == gltf_load::DEFAULT_MATERIAL_SENTINEL {
                gltf_load::DEFAULT_MATERIAL_MESH_PARAM
            } else {
                m.material_index as i32
            };
            mesh_node
                .params
                .insert("material_index".to_string(), int(mesh_material_param));
            mesh_node
                .params
                .insert("max_capacity".to_string(), int(m.vertex_count.max(1) as i32));
            // BUG-194/BUG-195: import-time provenance, read by SceneVm
            // (header vertex count) + merge_import_into_graph (scale
            // sanity's reference radius). Never read by evaluate()/run().
            mesh_node
                .params
                .insert("source_vertex_count".to_string(), int(m.vertex_count as i32));
            mesh_node
                .params
                .insert("source_bbox_radius".to_string(), float(bbox_radius));
            // BUG-221: shift this object's OWN mesh so local (0,0,0)
            // lands on ITS OWN bbox center (m.own_center), not the
            // shared whole-scene center below — see build_object_group's
            // doc comment on `transform_node` for the matching outer-
            // transform compensation that keeps net world placement
            // unchanged.
            mesh_node.params.insert("translate_x".to_string(), float(-m.own_center[0]));
            mesh_node.params.insert("translate_y".to_string(), float(-m.own_center[1]));
            mesh_node.params.insert("translate_z".to_string(), float(-m.own_center[2]));
            group_nodes.push(mesh_node);
            None
        };

        // GLTF_ANIMATION_DESIGN.md A3: a morphed object's base geometry
        // comes from the EXISTING `node.gltf_mesh_source` (rigid case) or
        // `node.gltf_skinned_mesh_source` (BUG-208: skin+morph case) built
        // just above (the `mesh_id`/`mesh_node_id` slot) — unlike skinning,
        // ordinary node transforms DO position a morphed mesh, so there is
        // no separate "morph mesh source" analogous to
        // `node.gltf_skinned_mesh_source` for the rigid path. When the
        // object is ALSO skinned, glTF applies morph THEN skin (§3.7.2):
        // the blend is chained between the skinned source's vertices and
        // node.skin_mesh's `in` further below, and
        // node.gltf_morph_deltas_source's `skinned` param routes the
        // loader into the SAME untransformed bind-pose space
        // node.gltf_skinned_mesh_source already uses (see that param's doc
        // comment) — without it the deltas would be world-transformed
        // while the base bind-pose vertices are not, a space mismatch.
        let morphed_vertices_source: Option<u32> = if let Some(morph) = &m.morph {
            let weights_node_id = format!("morphweights_{k}");
            let weights_id = fresh_id();
            let (static_weights_rows, weights_clip_durations_rows, weights_duration_s) =
                morph_weights_topology(morph, node_anims_by_clip);
            let mut weights_node =
                plain_node(weights_id, &weights_node_id, "node.gltf_morph_weights", &weights_node_id);
            weights_node
                .params
                .insert("target_count".to_string(), int(morph.target_count as i32));
            weights_node
                .params
                .insert("duration_s".to_string(), float(weights_duration_s));
            weights_node
                .params
                .insert("static_weights".to_string(), table(static_weights_rows));
            weights_node
                .params
                .insert("clip_durations".to_string(), table(weights_clip_durations_rows));
            // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: `path`/`target_node`
            // select the shared `gltf_anim_cache` entry — a `weights`
            // channel targets the mesh-owning node directly, no
            // ancestor-chain ambiguity.
            weights_node
                .params
                .insert("target_node".to_string(), int(morph.mesh_node_index as i32));
            group_nodes.push(weights_node);
            string_bindings.push(StringBindingDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                label: "Model File".to_string(),
                default_value: path_str.to_string(),
                target: BindingTarget::Node {
                    node_id: NodeId::new(&weights_node_id),
                    param: "path".to_string(),
                },
            });
            animation_card_bindings(&mut card_bindings, anim_prefix, &weights_node_id);
            animated = true;

            let deltas_node_id = format!("morphdeltas_{k}");
            let deltas_id = fresh_id();
            let mut deltas_node =
                plain_node(deltas_id, &deltas_node_id, "node.gltf_morph_deltas_source", &deltas_node_id);
            // BUG-207: same sentinel translation as the skinned/static
            // branches above — `u32::MAX` never re-queried as a document
            // material index.
            let deltas_material_param = if m.material_index == gltf_load::DEFAULT_MATERIAL_SENTINEL {
                gltf_load::DEFAULT_MATERIAL_MESH_PARAM
            } else {
                m.material_index as i32
            };
            deltas_node
                .params
                .insert("material_index".to_string(), int(deltas_material_param));
            deltas_node.params.insert(
                "max_capacity".to_string(),
                int((morph.target_count.max(1) * m.vertex_count.max(1)) as i32),
            );
            // BUG-208: see node.gltf_morph_deltas_source's `skinned` param
            // doc comment — must match whether `mesh_id` above resolved to
            // node.gltf_skinned_mesh_source or node.gltf_mesh_source.
            deltas_node
                .params
                .insert("skinned".to_string(), bool_val(skinned_vertices_source.is_some()));
            group_nodes.push(deltas_node);
            string_bindings.push(StringBindingDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                label: "Model File".to_string(),
                default_value: path_str.to_string(),
                target: BindingTarget::Node {
                    node_id: NodeId::new(&deltas_node_id),
                    param: "path".to_string(),
                },
            });

            let blend_node_id = format!("morphblend_{k}");
            let blend_id = fresh_id();
            let mut blend_node =
                plain_node(blend_id, &blend_node_id, "node.morph_targets_blend", &blend_node_id);
            blend_node
                .params
                .insert("target_count".to_string(), int(morph.target_count as i32));
            group_nodes.push(blend_node);

            group_wires.push(wire(mesh_id, "vertices", blend_id, "in"));
            group_wires.push(wire(deltas_id, "deltas", blend_id, "deltas"));
            group_wires.push(wire(weights_id, "weights", blend_id, "weights"));
            if let Some(skinmesh_id) = skinned_vertices_source {
                // BUG-208: morph applied BEFORE skin (glTF 2.0 §3.7.2) —
                // the blend's output feeds skin_mesh's `in` instead of the
                // group output directly (the group-output match further
                // below already routes through skinmesh_id whenever
                // skinned_vertices_source is Some, regardless of this
                // node's value).
                group_wires.push(wire(blend_id, "out", skinmesh_id, "in"));
                report_lines.push(format!(
                    "{group_name}: morphed ({} targets) on a skinned object — \
                     node.gltf_skinned_mesh_source + node.gltf_morph_deltas_source + \
                     node.gltf_morph_weights + node.morph_targets_blend + node.skin_mesh \
                     (morph applied before skin, glTF 2.0 §3.7.2 — BUG-208)",
                    morph.target_count
                ));
            } else {
                report_lines.push(format!(
                    "{group_name}: morphed ({} targets) — node.gltf_mesh_source + \
                     node.gltf_morph_deltas_source + node.gltf_morph_weights + node.morph_targets_blend",
                    morph.target_count
                ));
            }
            Some(blend_id)
        } else {
            None
        };

        // BUG-208: a skinned object with NO morph targets still needs its
        // skin_mesh `in` wired directly from the skinned source (the
        // morph-branch above only wires it when a blend node exists).
        if let (Some(skinmesh_id), None) = (skinned_vertices_source, morphed_vertices_source) {
            group_wires.push(wire(mesh_id, "vertices", skinmesh_id, "in"));
        }

        let mat_id = fresh_id();
        let mut mat_node = plain_node(mat_id, &mat_node_id, "node.pbr_material", &mat_node_id);
        // Author the glTF material's own name as the node's display title
        // when present, so the graph editor reads as "Leaf" / "Bark" rather
        // than the anonymous "mat_0" / "mat_1" handle.
        mat_node.title = m.name.clone();
        mat_node
            .params
            .insert("color_r".to_string(), float(m.base_color_factor[0]));
        mat_node
            .params
            .insert("color_g".to_string(), float(m.base_color_factor[1]));
        mat_node
            .params
            .insert("color_b".to_string(), float(m.base_color_factor[2]));
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2b: `effective_alpha` is now
        // exactly `base_color.a` (see its definition above for why the old
        // transmission-darkening formula was removed).
        mat_node
            .params
            .insert("color_a".to_string(), float(effective_alpha));
        mat_node.params.insert("metallic".to_string(), float(m.metallic));
        mat_node
            .params
            .insert("roughness".to_string(), float(m.roughness.max(0.01)));
        // 0.0 ambient: no flat fill floor, so the shadow side of a matte model
        // goes to true black under the default single-key rig — the hard,
        // dramatic "lit only by scene lights" look. The shared Ambient card
        // (below) raises it across every material to restore fill.
        mat_node.params.insert("ambient".to_string(), float(0.0));
        mat_node
            .params
            .insert("emission_r".to_string(), float(m.emissive[0]));
        mat_node
            .params
            .insert("emission_g".to_string(), float(m.emissive[1]));
        mat_node
            .params
            .insert("emission_b".to_string(), float(m.emissive[2]));
        // `emission_intensity` is the existing wired multiplier on
        // `node.pbr_material` — `KHR_materials_emissive_strength` folds
        // into it directly rather than growing a new param (D5: the
        // strength extension IS a multiplier on the same quantity this
        // param already controls). No extension present → factor 1.0, so
        // an emissive material still needs SOME emissive factor to glow
        // (matches the pre-F-P4 "any factor channel > 0" gate).
        let emissive_lit = m.emissive.iter().any(|&c| c > 0.0);
        mat_node.params.insert(
            "emission_intensity".to_string(),
            float(if emissive_lit { m.emissive_strength } else { 0.0 }),
        );
        mat_node.params.insert(
            "alpha_mode".to_string(),
            enum_val(if is_glass {
                2 // Blend
            } else if m.alpha_mask {
                1 // Mask
            } else {
                0 // Opaque
            }),
        );
        mat_node
            .params
            .insert("alpha_cutoff".to_string(), float(m.alpha_cutoff));
        // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_materials_specular + ior
        // → F0 scale (`fs_pbr`); KHR_texture_transform → base-color UV
        // affine (`resolve_albedo`). Every field defaults to the neutral
        // value verified in `gltf_load.rs` (ior=1.5, specular_factor=1.0,
        // specular_color_factor=[1,1,1], identity uv transform), so a
        // material without these extensions wires byte-identical params.
        mat_node.params.insert("ior".to_string(), float(m.ior));
        mat_node
            .params
            .insert("specular".to_string(), float(m.specular_factor));
        mat_node
            .params
            .insert("specular_tint_r".to_string(), float(m.specular_color_factor[0]));
        mat_node
            .params
            .insert("specular_tint_g".to_string(), float(m.specular_color_factor[1]));
        mat_node
            .params
            .insert("specular_tint_b".to_string(), float(m.specular_color_factor[2]));
        // GLB_CONFORMANCE_DESIGN.md G-P5/D5: KHR_materials_clearcoat
        // factors → the second GGX lobe (`fs_pbr`). Defaults (0.0/0.0)
        // reproduce byte-identical pre-G-P5 output — see `gltf_load.rs`.
        mat_node
            .params
            .insert("clearcoat".to_string(), float(m.clearcoat_factor));
        mat_node.params.insert(
            "clearcoat_roughness".to_string(),
            float(m.clearcoat_roughness_factor),
        );
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1: sheen, iridescence,
        // anisotropy, dispersion, transmission+volume factors → uniform
        // slots on `node.pbr_material` (`render_scene.wgsl` declares the
        // matching struct fields but reads none of them yet — E2-E6 wire
        // the shading math). Every default reproduces glTF's own implicit
        // default, so a material without these extensions wires
        // byte-identical params to pre-E1.
        mat_node
            .params
            .insert("sheen_color_r".to_string(), float(m.sheen_color_factor[0]));
        mat_node
            .params
            .insert("sheen_color_g".to_string(), float(m.sheen_color_factor[1]));
        mat_node
            .params
            .insert("sheen_color_b".to_string(), float(m.sheen_color_factor[2]));
        mat_node
            .params
            .insert("sheen_roughness".to_string(), float(m.sheen_roughness_factor));
        mat_node
            .params
            .insert("iridescence".to_string(), float(m.iridescence_factor));
        mat_node
            .params
            .insert("iridescence_ior".to_string(), float(m.iridescence_ior));
        mat_node.params.insert(
            "iridescence_thickness_min".to_string(),
            float(m.iridescence_thickness_minimum),
        );
        mat_node.params.insert(
            "iridescence_thickness_max".to_string(),
            float(m.iridescence_thickness_maximum),
        );
        mat_node
            .params
            .insert("anisotropy_strength".to_string(), float(m.anisotropy_strength));
        mat_node
            .params
            .insert("anisotropy_rotation".to_string(), float(m.anisotropy_rotation));
        mat_node
            .params
            .insert("dispersion".to_string(), float(m.dispersion));
        mat_node
            .params
            .insert("transmission".to_string(), float(m.transmission_factor));
        mat_node
            .params
            .insert("volume_thickness".to_string(), float(m.volume_thickness_factor));
        mat_node.params.insert(
            "volume_attenuation_distance".to_string(),
            float(m.volume_attenuation_distance),
        );
        mat_node.params.insert(
            "volume_attenuation_color_r".to_string(),
            float(m.volume_attenuation_color[0]),
        );
        mat_node.params.insert(
            "volume_attenuation_color_g".to_string(),
            float(m.volume_attenuation_color[1]),
        );
        mat_node.params.insert(
            "volume_attenuation_color_b".to_string(),
            float(m.volume_attenuation_color[2]),
        );
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E3/E4/E5/E6 (D1 revised):
        // sheen, iridescence, anisotropy, and now (E6) clearcoat/specular/
        // transmission/volume-thickness textures are all sampled (see the
        // wiring below) — no report-only warnings remain for any family's
        // texture in this doc.
        // Per-map KHR_texture_transform affines (G-P4) — one 6-param set
        // per map family, identity when the extension is absent.
        let parts = ["m00", "m01", "m10", "m11", "tx", "ty"];
        for (prefix, xf) in [
            ("uv_", &m.base_color_uv_transform),
            ("nrm_uv_", &m.normal_uv_transform),
            ("mr_uv_", &m.mr_uv_transform),
            ("occ_uv_", &m.occlusion_uv_transform),
            ("em_uv_", &m.emissive_uv_transform),
        ] {
            for (part, value) in parts.iter().zip(xf.iter()) {
                mat_node
                    .params
                    .insert(format!("{prefix}{part}"), float(*value));
            }
        }
        // GLB_XFAIL_BURNDOWN_DESIGN.md D3 (BUG-164): per-map-family sampler
        // settings → `node.pbr_material`'s `{prefix}wrap_u/wrap_v/mag_filter/
        // min_filter` enum params. Index order matches that primitive's
        // `WRAP_MODES`/`FILTER_MODES` arrays (0 = Repeat / Linear, the
        // default both sides agree on).
        let wrap_idx = |w: gltf_load::GltfWrapMode| -> u32 {
            match w {
                gltf_load::GltfWrapMode::Repeat => 0,
                gltf_load::GltfWrapMode::ClampToEdge => 1,
                gltf_load::GltfWrapMode::MirrorRepeat => 2,
            }
        };
        let filter_idx = |f: gltf_load::GltfFilterMode| -> u32 {
            match f {
                gltf_load::GltfFilterMode::Linear => 0,
                gltf_load::GltfFilterMode::Nearest => 1,
            }
        };
        for (prefix, s) in [
            ("", &m.base_color_sampler),
            ("nrm_", &m.normal_sampler),
            ("mr_", &m.mr_sampler),
            ("occ_", &m.occlusion_sampler),
            ("em_", &m.emissive_sampler),
        ] {
            mat_node
                .params
                .insert(format!("{prefix}wrap_u"), enum_val(wrap_idx(s.wrap_u)));
            mat_node
                .params
                .insert(format!("{prefix}wrap_v"), enum_val(wrap_idx(s.wrap_v)));
            mat_node.params.insert(
                format!("{prefix}mag_filter"),
                enum_val(filter_idx(s.mag_filter)),
            );
            mat_node.params.insert(
                format!("{prefix}min_filter"),
                enum_val(filter_idx(s.min_filter)),
            );
        }
        group_nodes.push(mat_node);
        // P1 scene-panel exposure convergence: expose ALL params of scene-
        // vocabulary atoms. The old metallic/roughness curation above is
        // replaced by the primitive's own ParamDef manifest.
        stamp_scene_node_exposures_into(
            &mut card_params,
            &mut card_bindings,
            mat_id,
            &NodeId::new(&mat_node_id),
            &format!("{group_name} — Material"),
            &metadata_for_node_type("node.pbr_material"),
        );

        // No per-object Metallic/Roughness card sliders (Peter, 2026-07-15:
        // "no need to modify them and they explode the card" — with one pair
        // per object, a multi-object import's card grew unusably long). The
        // material node above still carries the glTF's own metallic/
        // roughness values; only the card exposure is gone.
        // One shared "Ambient" fill knob fans out to every material's ambient
        // (a single source_id across all mat_k bindings — the preset_runtime
        // fan-out). Default 0.0 = the lights-only look; raise it for flat fill.
        // The card param itself is pushed once after the loop.
        // (a single source_id across all mat_k bindings — the preset_runtime
        // fan-out). Default 0.0 = the lights-only look; raise it for flat fill.
        // The card param itself is pushed once after the loop.
        card_bindings.push(card_binding(
            "scene_ambient", "Ambient", 0.0, &mat_node_id, "ambient", 1.0,
        ));

        // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D1/D3/P3: the group's outward
        // interface is a single `object: Object` port. Internally, the mesh
        // geometry, the material, this object's transform, and every
        // present map/instances wire into a `node.scene_object` node (NOT
        // directly to render_scene's legacy per-object ports, which no
        // longer exist post-P2); that node's single `object` output is what
        // crosses the group boundary via `system.group_output`.
        let scene_object_id = fresh_id();
        let out_id = fresh_id();
        let outputs = vec![InterfacePortDef { name: "object".to_string(), port_type: "Object".to_string() }];
        // A2: a skinned object's vertices come from node.skin_mesh's `out`,
        // not directly from the mesh source (which for a skinned object
        // outputs UNDEFORMED bind-pose geometry). A3: likewise a morphed
        // object's vertices come from node.morph_targets_blend's `out`, not
        // the base mesh source directly (which outputs the UNBLENDED
        // bind/rest geometry) — a morphed object's base geometry alone is
        // never what should render once targets are non-static.
        match (skinned_vertices_source, morphed_vertices_source) {
            (Some(skinmesh_id), _) => group_wires.push(wire(skinmesh_id, "out", scene_object_id, "vertices")),
            (None, Some(blend_id)) => group_wires.push(wire(blend_id, "out", scene_object_id, "vertices")),
            (None, None) => group_wires.push(wire(mesh_id, "vertices", scene_object_id, "vertices")),
        }
        group_wires.push(wire(mat_id, "out", scene_object_id, "material"));

        // Recenter this object at the origin so the fixed-target orbit
        // camera frames the (not-recentered) gltf_mesh_source output — same
        // convention `gltf_mesh_source_renders_azalea_to_png` proves. Lives
        // on this object's OWN `node.transform_3d` now (D9 — the recenter
        // moved off the shared render node onto the per-object atom), no
        // transform sliders on the card by default: transforms are
        // performed via expose-what-you-need, gizmos (P6), or the group
        // face (D6), not a scene-editor slider wall.
        //
        // BUG-221: for a non-skinned object, the mesh source above was
        // already shifted by -m.own_center (local (0,0,0) = this object's
        // own visual center). This node's `pos` must place that recentered
        // pivot at the SAME world position it sat at before the fix —
        // `own_center - center` (own_center's location within the
        // whole-scene recentered space) — so net placement at import time
        // is unchanged and only the rotation pivot moves. A rotating
        // `rot_*` on THIS node then spins the mesh about local (0,0,0),
        // which is now the object's own visual center by construction.
        // Skinned objects are excluded (no mesh-side shift was applied
        // above — BUG-205's doctrine already excludes them from rigid
        // transform_3d positioning; their world placement comes entirely
        // from the joint palette), so they keep the pre-fix whole-scene
        // `-center` recenter, unchanged.
        let transform_node_id = format!("transform_{k}");
        let transform_id = fresh_id();
        let mut transform_node = plain_node(
            transform_id,
            &transform_node_id,
            "node.transform_3d",
            &transform_node_id,
        );
        let transform_pos = if skinned_vertices_source.is_none() {
            [
                m.own_center[0] - center[0],
                m.own_center[1] - center[1],
                m.own_center[2] - center[2],
            ]
        } else {
            [-center[0], -center[1], -center[2]]
        };
        transform_node.params.insert("pos_x".to_string(), float(transform_pos[0]));
        transform_node.params.insert("pos_y".to_string(), float(transform_pos[1]));
        transform_node.params.insert("pos_z".to_string(), float(transform_pos[2]));
        group_nodes.push(transform_node);
        stamp_scene_node_exposures_into(
            &mut card_params,
            &mut card_bindings,
            transform_id,
            &NodeId::new(&transform_node_id),
            &format!("{group_name} — Transform"),
            &metadata_for_node_type("node.transform_3d"),
        );
        group_wires.push(wire(transform_id, "transform", scene_object_id, "transform"));

        // GLTF_ANIMATION_DESIGN.md A1/A4 (D1): "animating a rigid node is
        // animating params" — when this object's animation resolved in AT
        // LEAST ONE clip (see gltf_load::resolve_object_animation, run per
        // clip), insert one node.gltf_animation_source per object and wire
        // its nine scalar outputs into this SAME transform_3d's nine
        // port-shadowed inputs, additive to the static recenter above
        // (the recenter stays as transform_3d's own pos_x/y/z param
        // default; the animation source's wired output overrides it at
        // runtime, same as any port-shadow).
        //
        // BUG-205: SKINNED objects are excluded. A skinned mesh's
        // positioning comes ENTIRELY from its joint palette (the A2
        // doctrine above) — the joint worlds already include the static
        // ancestor chain via joint_root_world. resolve_object_animation
        // walks the same ancestor chain, so a rig with an animated
        // ancestor above the joint tree (Sketchfab FBX exports animate
        // `Bip01`, whose static scale is ALSO in that chain) would apply
        // that ancestor's transform a SECOND time through transform_3d —
        // skeleton_animated.glb rendered at 0.0254² of its authored size
        // (a ~12px speck). An ANIMATED ancestor prefix is still sampled
        // statically by the pose path (the documented joint_root_world
        // approximation); wiring it here rigidly was never the fix for
        // that — it double-transforms instead.
        if m.animations.iter().any(Option::is_some) && skinned_vertices_source.is_none() {
            let anim_node_id = format!("anim_{k}");
            let anim_id = fresh_id();
            let mut anim_node =
                plain_node(anim_id, &anim_node_id, "node.gltf_animation_source", &anim_node_id);
            // The wire into transform_3d's pos_x/y/z port-shadows WINS
            // outright over that node's own static recenter param — see
            // node.gltf_animation_source's `recenter_x` doc comment. BUG-221:
            // this branch only runs when `skinned_vertices_source.is_none()`
            // (the guard above), so the mesh source was already shifted by
            // -m.own_center — fold the SAME `own_center - center` offset
            // `transform_node` above uses (not the old whole-scene
            // `-center`), so the animated object lands at the identical net
            // world position, pivoting about its own visual center exactly
            // like a static object of this same shape does.
            anim_node.params.insert("recenter_x".to_string(), float(m.own_center[0] - center[0]));
            anim_node.params.insert("recenter_y".to_string(), float(m.own_center[1] - center[1]));
            anim_node.params.insert("recenter_z".to_string(), float(m.own_center[2] - center[2]));

            // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: keyframe payload no
            // longer lives in Table params — `path` plus THREE per-channel
            // node selectors (`translation_node`/`rotation_node`/
            // `scale_node` — see node.gltf_animation_source's module doc
            // comment for why one shared selector is wrong, confirmed by
            // BoxAnimated.glb itself) select this object's channels from
            // the shared `gltf_anim_cache`. `clip_durations` (tiny, D1) is
            // still stamped per clip. The first `Some` clip's per-channel
            // node indices are stamped as the file-wide selectors — every
            // clip for one object resolves against the SAME scene-node
            // structure, so this is consistent across clips, not just a
            // clip-0 special case.
            let mut clip_durations_rows = Vec::with_capacity(m.animations.len());
            let mut fallback_duration_s = 1e-6;
            let mut translation_node: Option<i32> = None;
            let mut rotation_node: Option<i32> = None;
            let mut scale_node: Option<i32> = None;
            for (c, clip_anim) in m.animations.iter().enumerate() {
                let Some(anim) = clip_anim else {
                    clip_durations_rows.push(vec![c as f32, 1e-6]);
                    continue;
                };
                if translation_node.is_none() {
                    translation_node = Some(anim.translation_node.map(|n| n as i32).unwrap_or(-1));
                }
                if rotation_node.is_none() {
                    rotation_node = Some(anim.rotation_node.map(|n| n as i32).unwrap_or(-1));
                }
                if scale_node.is_none() {
                    scale_node = Some(anim.scale_node.map(|n| n as i32).unwrap_or(-1));
                }
                let duration_s = anim.duration_s.max(1e-6);
                clip_durations_rows.push(vec![c as f32, duration_s]);
                if c == 0 {
                    fallback_duration_s = duration_s;
                }
            }
            anim_node.params.insert("duration_s".to_string(), float(fallback_duration_s));
            anim_node.params.insert("clip_durations".to_string(), table(clip_durations_rows));
            anim_node
                .params
                .insert("translation_node".to_string(), int(translation_node.unwrap_or(-1)));
            anim_node.params.insert("rotation_node".to_string(), int(rotation_node.unwrap_or(-1)));
            anim_node.params.insert("scale_node".to_string(), int(scale_node.unwrap_or(-1)));
            group_nodes.push(anim_node);
            string_bindings.push(StringBindingDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                label: "Model File".to_string(),
                default_value: path_str.to_string(),
                target: BindingTarget::Node {
                    node_id: NodeId::new(&anim_node_id),
                    param: "path".to_string(),
                },
            });
            animation_card_bindings(&mut card_bindings, anim_prefix, &anim_node_id);
            animated = true;
            for port in
                ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z"]
            {
                group_wires.push(wire(anim_id, port, transform_id, port));
            }
        }

        string_bindings.push(StringBindingDef {
            id: MODEL_FILE_PARAM_ID.to_string(),
            label: "Model File".to_string(),
            default_value: path_str.to_string(),
            target: BindingTarget::Node {
                node_id: NodeId::new(&mesh_node_id),
                param: "path".to_string(),
            },
        });

        if let Some(tex_index) = m.base_color_texture {
            let tex_node_id = format!("tex_{k}");
            let tex_id = fresh_id();
            let mut tex_node =
                plain_node(tex_id, &tex_node_id, "node.gltf_texture_source", &tex_node_id);
            tex_node
                .params
                .insert("texture_index".to_string(), int(tex_index as i32));
            tex_node.params.insert("color_space".to_string(), enum_val(0)); // sRGB — correct for albedo
            // v1 default — the summary doesn't carry per-texture pixel
            // dimensions yet, so every base-color map resamples to 1024².
            // TODO: thread the source image's actual width/height through
            // `GltfImportSummary`/`GltfMaterialInfo` so a non-1024 texture
            // doesn't resample.
            tex_node.params.insert("width".to_string(), int(1024));
            tex_node.params.insert("height".to_string(), int(1024));
            group_nodes.push(tex_node);

            group_wires.push(wire(tex_id, "out", scene_object_id, "base_color_map"));

            string_bindings.push(StringBindingDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                label: "Model File".to_string(),
                default_value: path_str.to_string(),
                target: BindingTarget::Node {
                    node_id: NodeId::new(&tex_node_id),
                    param: "path".to_string(),
                },
            });

            textures_wired += 1;
        }

        // D3/D5/D6 — normal / metallic-roughness / occlusion / emissive
        // maps. `map_tex_cache` is scoped to THIS object and keyed by glTF
        // `texture_index`: ORM-packed files (occlusion index == mr index,
        // a common glTF convention) reuse the same `node.gltf_texture_source`
        // for both ports instead of decoding the same physical image
        // twice. Colour space per D6: normal/MR/occlusion decode linear
        // (data maps — the raw bytes ARE the value), emissive decodes sRGB
        // (a colour map, same as base-colour).
        let mut map_tex_cache: std::collections::HashMap<(u32, u32, u32), (u32, String)> =
            std::collections::HashMap::new();
        if let Some(tex_index) = m.normal_texture {
            wire_map_texture(
                tex_index,
                1, // Linear
                "normal_tex",
                "normal_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.mr_texture {
            wire_map_texture(
                tex_index,
                1, // Linear
                "mr_tex",
                "mr_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                // GLB_XFAIL_BURNDOWN_DESIGN.md D2: this is a spec-gloss
                // specularGlossinessTexture standing in for mrMap — repack
                // its alpha (gloss) into G=roughness/B=metallic at blit
                // time so render_scene's mr_map read stays untouched.
                if m.mr_texture_is_gloss_alpha { 1 } else { 0 },
            );
        }
        if let Some(tex_index) = m.occlusion_texture {
            wire_map_texture(
                tex_index,
                1, // Linear
                "occlusion_tex",
                "occlusion_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.emissive_texture {
            wire_map_texture(
                tex_index,
                0, // sRGB — a colour map, same convention as base-colour
                "emissive_tex",
                "emissive_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E3/E4/E5 (D1 revised — full
        // spec surface per family): sheen/iridescence/anisotropy extension
        // textures, same `wire_map_texture` doctrine as the base five maps
        // above. sheenColorTexture is a colour map (sRGB); every other
        // extension texture here is a data map (linear) per its own spec
        // section.
        if let Some(tex_index) = m.sheen_color_texture {
            wire_map_texture(
                tex_index,
                0, // sRGB — sheenColorTexture is a colour map
                "sheen_color_tex",
                "sheen_color_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.sheen_roughness_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (alpha channel)
                "sheen_roughness_tex",
                "sheen_roughness_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.iridescence_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (R channel = factor scale)
                "iridescence_tex",
                "iridescence_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.iridescence_thickness_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (G channel = thickness lerp)
                "iridescence_thickness_tex",
                "iridescence_thickness_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.anisotropy_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (RG = rotation, B = strength)
                "anisotropy_tex",
                "anisotropy_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1 revised — full spec
        // surface): the texture-completion sweep. Same `wire_map_texture`
        // doctrine as the seven maps above — clearcoatTexture/
        // clearcoatRoughnessTexture/clearcoatNormalTexture are data maps
        // (R/G/RGB channels respectively, none are colour); specularTexture
        // is a data map (alpha channel); specularColorTexture is a colour
        // map (sRGB, tints an RGB factor); transmissionTexture and
        // thicknessTexture are data maps (R/G channels).
        if let Some(tex_index) = m.clearcoat_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (R channel = clearcoatFactor scale)
                "clearcoat_tex",
                "clearcoat_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.clearcoat_roughness_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (G channel = clearcoatRoughnessFactor scale)
                "clearcoat_roughness_tex",
                "clearcoat_roughness_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.clearcoat_normal_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — tangent-space normal map, same convention as normalMap
                "clearcoat_normal_tex",
                "clearcoat_normal_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.specular_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (ALPHA channel = specularFactor scale)
                "specular_tex",
                "specular_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.specular_color_texture {
            wire_map_texture(
                tex_index,
                0, // sRGB — specularColorTexture is a colour map
                "specular_color_tex",
                "specular_color_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.transmission_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (R channel = transmissionFactor scale)
                "transmission_tex",
                "transmission_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }
        if let Some(tex_index) = m.volume_thickness_texture {
            wire_map_texture(
                tex_index,
                1, // Linear — data map (G channel = thicknessFactor scale)
                "volume_thickness_tex",
                "volume_thickness_map",
                k,
                path_str,
                fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut string_bindings,
                &mut map_tex_cache,
                scene_object_id,
                0,
            );
        }

        // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D1/D3: the `node.scene_object`
        // node binding this object's mesh/transform/material/maps/instances
        // (wired above) into a single `Object` value — handle-stamped with
        // this object's group name (D6: "the object IS its `scene_object`
        // node; the name is its `handle`"). Its `object` output is what
        // crosses the group boundary.
        let scene_object_node = plain_node(
            scene_object_id,
            &format!("object_{k}_bind"),
            "node.scene_object",
            &group_name,
        );
        group_nodes.push(scene_object_node);
        stamp_scene_node_exposures_into(
            &mut card_params,
            &mut card_bindings,
            scene_object_id,
            &NodeId::new(format!("object_{k}_bind")),
            &group_name,
            &metadata_for_node_type("node.scene_object"),
        );

        // `system.group_output` closes the body; its single `object` port is
        // the interface output name the scene_object's wire above targets. A
        // boundary node carries no params and no title.
        group_nodes.push(plain_node(
            out_id,
            &format!("object_{k}_out"),
            GROUP_OUTPUT_TYPE_ID,
            "output",
        ));
        group_wires.push(wire(scene_object_id, "object", out_id, "object"));

        // The group box itself, named for the material so the top level reads as
        // labeled boxes a performer can navigate. Folded away at load; only its
        // outputs cross to the top level.
        let group_id = fresh_id();
        let mut group_node =
            plain_node(group_id, &format!("object_{k}"), GROUP_TYPE_ID, &group_name);
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface { inputs: Vec::new(), outputs, params: Vec::new() },
            nodes: group_nodes,
            wires: group_wires,
            // A distinct high-saturation header tint per object so a multi-mesh
            // import reads as a few colour-coded boxes at a glance.
            tint: Some(group_tint(k)),
        }));
    let mut wires_to_render: Vec<EffectGraphWire> = Vec::new();
        // Top-level wire: the group's single `object` output feeds
        // render_scene's `object_{port_index}` port (D4 — render_scene's v2
        // per-object surface is `object_{i}` only). After flattening this
        // becomes the exact `object_k_bind.object → render.object_k` wire
        // an ungrouped hand-built scene would use directly.
        wires_to_render.push(wire(group_id, "object", render_id, &format!("object_{port_index}")));

    ObjectGroupOutput {
        group_node,
        wires_to_render,
        card_params,
        card_bindings,
        string_bindings,
        report_lines,
        textures_wired,
        animated,
    }
}

