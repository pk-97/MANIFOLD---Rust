//! glTF import ASSEMBLER (P1c stage 2) — a pure function that turns a
//! parsed `.glb`'s [`gltf_load::GltfImportSummary`] (stage 1: the CPU-only
//! parse) into a generator [`EffectGraphDef`] that renders the model
//! faithfully: one `node.render_scene` object PER DISTINCT MATERIAL, each
//! fed its material-filtered geometry (`node.gltf_mesh_source`), that
//! material's base-color texture (`node.gltf_texture_source`, when present),
//! a `node.pbr_material` atom carrying the glTF's PBR factors, and a
//! `node.transform_3d` atom (seeded to recenter the object at the origin) —
//! plus a shared synthesized framing camera (`node.orbit_camera`), a sun
//! light (`node.light`), and an IBL envmap (`node.bake_environment`).
//!
//! Each object's producers are wrapped in one named, tinted node **group**
//! so a multi-mesh import reads as a few labelled boxes in the graph editor
//! rather than a wall of loose nodes; the group flattens away at load to the
//! exact same flat graph, so nothing the runtime sees changes (see
//! [`build_import_graph`] and `docs/GROUPING_GRAPHS.md`).
//!
//! No GPU, no file I/O beyond the one [`gltf_load::gltf_import_summary`]
//! parse this module drives — everything here is graph-shape assembly.
//! The glb path itself never becomes a node `param` (there is no `String`
//! variant in [`SerializedParamValue`] — see its doc comment); it flows
//! through `presetMetadata.stringParams` + `stringBindings`, the same
//! outer-card text-config convention `node.image_folder`-based presets use
//! (see `assets/generator-presets/MriVolume.json`'s `axial_folder`).
//!
//! Production caller: `manifold-app`'s `.glb`/`.gltf` file-drop handler
//! (`Application::import_model_file`) calls [`assemble_import_graph`], then
//! installs the result on a new generator layer via
//! `manifold_editing::commands::layer::ImportModelLayerCommand`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use manifold_core::NodeId;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID,
    GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef, ParamSpecDef,
    PresetMetadata, SerializedParamValue, SkipModeDef, StringBindingDef, StringParamSpecDef,
};

use super::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID};
use super::gltf_load;
use super::gltf_load::GltfImportSummary;

use crate::node_graph::primitives::DEFAULT_NEAR as CAMERA_NEAR_DEFAULT;
use crate::node_graph::primitives::gltf_anim_shared::LOOP_MODES;
use crate::node_graph::primitives::render_scene::OBJECT_SAFETY_MAX;

/// How many of the largest (by vertex count) objects get a per-object card
/// slider (currently just glass's Opacity knob). GLB_CONFORMANCE_DESIGN.md
/// D4: the graph wires EVERY material 1:1 — this cap is pure UI curation on
/// top of a fully-imported graph, never a reason to drop geometry. Distinct
/// from [`OBJECT_SAFETY_MAX`], which bounds the graph itself.
const CARD_CURATION_MAX: usize = 16;

/// Stable identity for the one outer-card text config every imported
/// preset carries: the source `.glb`/`.gltf` path.
const MODEL_FILE_PARAM_ID: &str = "model_file";
/// GLB_CONFORMANCE_DESIGN.md D6 — the HDRI environment's own Browse field,
/// a distinct string param from [`MODEL_FILE_PARAM_ID`] (the imported
/// .glb's path). Empty by default; `node.hdri_source` reads an empty path
/// as "nothing decoded" and clears its output to black.
const HDRI_FILE_PARAM_ID: &str = "hdri_file";

/// What the assembler did, for the caller (importer UI, tests) to report
/// or warn on. Not part of the graph itself.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// Distinct materials with geometry, as parsed. Import is 1:1
    /// (GLB_CONFORMANCE_DESIGN.md D4) — always equal to `object_count`.
    pub material_count: usize,
    /// Objects wired into `node.render_scene` — always equal to
    /// `material_count`; nothing is ever dropped for exceeding a count
    /// (`assemble_import_graph` errors instead, see [`OBJECT_SAFETY_MAX`]).
    pub object_count: usize,
    /// How many objects got a `node.gltf_texture_source` → `base_color_map_N` wire.
    pub textures_wired: usize,
    /// Triangle-list vertices belonging to glTF's unassigned default
    /// material — imported as a real object since BUG-171 (mirrors
    /// [`gltf_load::GltfImportSummary::default_material_vertex_count`]).
    pub default_material_vertex_count: u32,
    /// Always `true` today — the assembler always synthesizes a framing
    /// camera (the glb's own embedded cameras, if any, are not yet
    /// consumed). Kept as a field so a future embedded-camera path has
    /// somewhere to report `false`.
    pub camera_synthesized: bool,
    /// D9 doctrine ("every import produces a report") applied to the
    /// per-material features F-P4 parses but cannot yet map: clearcoat
    /// (Deferred #1), transmission (report-only until F-P5), and BLEND
    /// materials downgraded to Mask cutout (the F-P5 stopgap). One line per
    /// occurrence, naming the material. Never silently dropped.
    pub report_lines: Vec<String>,
}

/// Build an [`EffectGraphNode`] with the given identity and every other
/// field at its "ordinary node" default. `EffectGraphNode` doesn't derive
/// `Default` (several fields are meaningful `Option`s used by grouping /
/// the graph editor), so this centralises the shape once rather than
/// repeating all eleven fields at every call site.
fn plain_node(id: u32, node_id: &str, type_id: &str, handle: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(node_id),
        type_id: type_id.to_string(),
        handle: Some(handle.to_string()),
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

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
fn wire_map_texture(
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

fn float(v: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value: v }
}
fn int(v: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value: v }
}
fn bool_val(v: bool) -> SerializedParamValue {
    SerializedParamValue::Bool { value: v }
}
fn enum_val(v: u32) -> SerializedParamValue {
    SerializedParamValue::Enum { value: v }
}
fn table(rows: Vec<Vec<f32>>) -> SerializedParamValue {
    SerializedParamValue::Table { rows }
}

/// GLTF_ANIMATION_DESIGN.md A2: `[joint_index, m0..m15]` row (column-major
/// 4x4) — the row shape `node.gltf_skeleton_pose`'s `joint_root_world_table`
/// / `inverse_bind_table` Tables use.
fn mat4_row(joint: usize, m: &gltf_load::Mat4) -> Vec<f32> {
    let mut row = Vec::with_capacity(17);
    row.push(joint as f32);
    for col in m.iter() {
        row.extend_from_slice(col);
    }
    row
}

/// Push one Vec3 track row in the widened schema
/// `[..prefix, time, val_x,val_y,val_z, mode, in_x,in_y,in_z, out_x,out_y,out_z]`
/// (IMPORT_ANYTHING_WAVE_DESIGN.md W2) — `prefix` is whatever leading key
/// columns the caller groups rows by (`[clip]` for the A1 rigid path,
/// `[clip, joint]` for the A2 skeleton path); `gltf_anim_shared`'s samplers
/// read the mode/tangent columns relative to the value column, so every
/// caller gets STEP/CUBICSPLINE support for free by routing through this.
fn push_vec3_row(
    rows: &mut Vec<Vec<f32>>,
    prefix: &[f32],
    time: f32,
    value: [f32; 3],
    mode: gltf_load::GltfInterp,
    in_tangent: [f32; 3],
    out_tangent: [f32; 3],
) {
    let mut row = prefix.to_vec();
    row.push(time);
    row.extend_from_slice(&value);
    row.push(mode.to_f32());
    row.extend_from_slice(&in_tangent);
    row.extend_from_slice(&out_tangent);
    rows.push(row);
}

/// Same as [`push_vec3_row`] for a `[x, y, z, w]` quaternion track.
fn push_quat_row(
    rows: &mut Vec<Vec<f32>>,
    prefix: &[f32],
    time: f32,
    value: [f32; 4],
    mode: gltf_load::GltfInterp,
    in_tangent: [f32; 4],
    out_tangent: [f32; 4],
) {
    let mut row = prefix.to_vec();
    row.push(time);
    row.extend_from_slice(&value);
    row.push(mode.to_f32());
    row.extend_from_slice(&in_tangent);
    row.extend_from_slice(&out_tangent);
    rows.push(row);
}

/// Build the six flat Tables `node.gltf_skeleton_pose` needs from one
/// object's resolved skin topology (`GltfSkinInfo`) plus EVERY parsed
/// clip's per-node animation map (A4/D4: one row-group per `(clip_index,
/// joint_index)` pair, not just clip 0). Topology rows (parent/root-world/
/// inverse-bind) are per-joint only — a skin's topology doesn't vary by
/// clip. Track rows are grouped ascending by `(clip_index, joint_index)`,
/// ascending time within a block (the primitive's row-grouping contract). A
/// joint with no animated channel in a given clip gets a single static row
/// from its BIND pose (`joint_bind_translation`/`_rotation`/`_scale`) —
/// never the identity A1's rigid-object sampler falls back to, because an
/// unrigged joint's bind pose is frequently non-identity. `node_anims_by_clip`
/// empty (a skin with zero animation clips in the whole asset) is treated as
/// one implicit static clip 0. Returns the six Tables, the `clip_durations`
/// rows (`[clip_index, duration_s]`), and a fallback `duration_s` (clip 0's,
/// or `1e-3` if even that has no animated joints — the primitive's own
/// zero-guard floor).
#[allow(clippy::type_complexity)]
fn build_skeleton_pose_tables(
    skin: &gltf_load::GltfSkinInfo,
    node_anims_by_clip: &[std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>],
) -> (Vec<Vec<f32>>, Vec<Vec<f32>>, Vec<Vec<f32>>, Vec<Vec<f32>>, Vec<Vec<f32>>, Vec<Vec<f32>>, Vec<Vec<f32>>, f32) {
    let n = skin.joint_node_indices.len();
    let mut parent_rows = Vec::with_capacity(n);
    let mut root_world_rows = Vec::new();
    let mut inverse_bind_rows = Vec::with_capacity(n);
    for j in 0..n {
        parent_rows.push(vec![j as f32, skin.joint_parent[j] as f32]);
        inverse_bind_rows.push(mat4_row(j, &skin.inverse_bind_matrices[j]));
        if skin.joint_parent[j] < 0 {
            root_world_rows.push(mat4_row(j, &skin.joint_root_world[j]));
        }
    }

    let empty_anims = std::collections::BTreeMap::new();
    let clip_count = node_anims_by_clip.len().max(1);
    let mut translation_rows = Vec::new();
    let mut rotation_rows = Vec::new();
    let mut scale_rows = Vec::new();
    let mut clip_durations_rows = Vec::with_capacity(clip_count);
    let mut fallback_duration_s = 1e-3;

    for c in 0..clip_count {
        let node_anims = node_anims_by_clip.get(c).unwrap_or(&empty_anims);
        let mut duration_s: f32 = 0.0;
        for j in 0..n {
            let anim = node_anims.get(&skin.joint_node_indices[j]);
            let prefix = [c as f32, j as f32];
            match anim.and_then(|a| a.translation.as_ref()) {
                Some(t) if !t.times.is_empty() => {
                    for (i, (time, v)) in t.times.iter().zip(t.values.iter()).enumerate() {
                        let (in_t, out_t) = t.tangents_at(i);
                        push_vec3_row(&mut translation_rows, &prefix, *time, *v, t.mode, in_t, out_t);
                        duration_s = duration_s.max(*time);
                    }
                }
                _ => {
                    let b = skin.joint_bind_translation[j];
                    push_vec3_row(
                        &mut translation_rows,
                        &prefix,
                        0.0,
                        b,
                        gltf_load::GltfInterp::Linear,
                        [0.0; 3],
                        [0.0; 3],
                    );
                }
            }
            match anim.and_then(|a| a.rotation.as_ref()) {
                Some(r) if !r.times.is_empty() => {
                    for (i, (time, v)) in r.times.iter().zip(r.values.iter()).enumerate() {
                        let (in_t, out_t) = r.tangents_at(i);
                        push_quat_row(&mut rotation_rows, &prefix, *time, *v, r.mode, in_t, out_t);
                        duration_s = duration_s.max(*time);
                    }
                }
                _ => {
                    let b = skin.joint_bind_rotation[j];
                    push_quat_row(
                        &mut rotation_rows,
                        &prefix,
                        0.0,
                        b,
                        gltf_load::GltfInterp::Linear,
                        [0.0; 4],
                        [0.0; 4],
                    );
                }
            }
            match anim.and_then(|a| a.scale.as_ref()) {
                Some(s) if !s.times.is_empty() => {
                    for (i, (time, v)) in s.times.iter().zip(s.values.iter()).enumerate() {
                        let (in_t, out_t) = s.tangents_at(i);
                        push_vec3_row(&mut scale_rows, &prefix, *time, *v, s.mode, in_t, out_t);
                        duration_s = duration_s.max(*time);
                    }
                }
                _ => {
                    let b = skin.joint_bind_scale[j];
                    push_vec3_row(
                        &mut scale_rows,
                        &prefix,
                        0.0,
                        b,
                        gltf_load::GltfInterp::Linear,
                        [0.0; 3],
                        [0.0; 3],
                    );
                }
            }
        }
        let duration_s = duration_s.max(1e-3);
        clip_durations_rows.push(vec![c as f32, duration_s]);
        if c == 0 {
            fallback_duration_s = duration_s;
        }
    }

    (
        parent_rows,
        root_world_rows,
        inverse_bind_rows,
        translation_rows,
        rotation_rows,
        scale_rows,
        clip_durations_rows,
        fallback_duration_s,
    )
}

/// GLTF_ANIMATION_DESIGN.md A3/A4: build `node.gltf_morph_weights`'
/// `weight_tracks` Table rows from `morph`'s static topology plus EVERY
/// parsed clip's per-node animation map — a `weights` channel targets the
/// mesh-owning node directly (no ancestor-chain composition, unlike TRS),
/// so this is a single lookup by `morph.mesh_node_index` per clip, not a
/// chain walk. An unanimated target in a given clip (the node carries no
/// `weights` channel, or the channel exists but doesn't cover the whole
/// target range) falls back to its authored `static_weights[i]` — never a
/// silent 0.0 (`MorphPrimitivesTest.glb`'s `mesh.weights = [0.5]` is the
/// documented case this guards). `node_anims_by_clip` empty is treated as
/// one implicit static clip 0 (mirrors `build_skeleton_pose_tables`).
/// Returns `(weight_track_rows, clip_durations_rows, fallback_duration_s)`.
fn build_morph_weight_table(
    morph: &gltf_load::GltfObjectMorph,
    node_anims_by_clip: &[std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>],
) -> (Vec<Vec<f32>>, Vec<Vec<f32>>, f32) {
    let n = morph.target_count as usize;
    let empty_anims = std::collections::BTreeMap::new();
    let clip_count = node_anims_by_clip.len().max(1);
    let mut rows = Vec::new();
    let mut clip_durations_rows = Vec::with_capacity(clip_count);
    let mut fallback_duration_s = 1e-3;

    for c in 0..clip_count {
        let node_anims = node_anims_by_clip.get(c).unwrap_or(&empty_anims);
        // Rows must be grouped ascending by (clip_index, target_index),
        // ascending time WITHIN a (clip, target) block — the SAME emission
        // contract `build_skeleton_pose_tables` uses. glTF's keyframe
        // `input` accessor is spec-required to be non-decreasing, so
        // iterating `t.times` in accessor order is already time-ascending.
        let track = node_anims.get(&morph.mesh_node_index).and_then(|a| a.weights.as_ref());
        let duration_s = match track {
            Some(t) if !t.times.is_empty() && t.values.iter().all(|v| v.len() == n) => {
                for i in 0..n {
                    for (time, values) in t.times.iter().zip(t.values.iter()) {
                        rows.push(vec![c as f32, i as f32, *time, values[i]]);
                    }
                }
                t.times.last().copied().unwrap_or(0.0)
            }
            _ => {
                for i in 0..n {
                    let w = morph.static_weights.get(i).copied().unwrap_or(0.0);
                    rows.push(vec![c as f32, i as f32, 0.0, w]);
                }
                0.0
            }
        }
        .max(1e-3);
        clip_durations_rows.push(vec![c as f32, duration_s]);
        if c == 0 {
            fallback_duration_s = duration_s;
        }
    }

    (rows, clip_durations_rows, fallback_duration_s)
}

/// Degrees→radians factor for the camera card sliders. The camera node's
/// `orbit`/`tilt`/`fov_y` params are radians (matching `node.orbit_camera`),
/// but a performer wants degrees on the card, so each angle slider carries a
/// [`BindingDef::scale`] of this value — the `param_binding` write boundary
/// applies `value * scale` (no `deg_to_rad` helper node needed, unlike the
/// hand-authored MetallicGlass preset).
const DEG2RAD: f32 = std::f32::consts::PI / 180.0;

/// Default softbox dome-fill radiance stamped on imported rigs (F-P7) —
/// node param and Fill Light card default stay in sync through this one
/// constant. Tuned against the DamagedHelmet/AMG probe renders: enough
/// broad radiance that metallic surfaces read their albedo, low enough
/// that the black-void product look survives.
const IMPORT_FILL_DEFAULT: f32 = 0.6;

/// Default softbox strip intensity stamped on imported rigs (F-P7): half
/// the primitive's own 6.0 default. With the fill dome supplying the broad
/// radiance, full-strength strips dominate every curved reflection (the
/// banded-visor look); 3.0 keeps the chrome-streak character as an accent.
/// The Strip Lights card fader dials it live.
const IMPORT_STRIPS_DEFAULT: f32 = 3.0;

/// One outer-card slider definition. Curated performance surface for an
/// imported model (camera framing, light, material) — see
/// [`assemble_import_graph`]'s metadata block. `default_value` must match
/// the wired node param's value (through `scale`) so the card reproduces the
/// assembler's look on first frame with no drift. `section` bundles this
/// knob under a collapsible card header (D5/D9,
/// SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): per-object knobs get the
/// object's group name, shared knobs get `"Camera"`/`"Sun"`/`"Environment"`.
fn card_param(
    id: &str,
    name: &str,
    min: f32,
    max: f32,
    default: f32,
    is_angle: bool,
    section: &str,
) -> ParamSpecDef {
    ParamSpecDef {
        id: id.to_string(),
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: manifold_core::macro_bank::MacroCurve::default(),
        invert: false,
        is_angle,
        is_trigger_gate: false,
        wraps: false,
        section: Some(section.to_string()),
    }
}

/// Route one card slider (`id`) to one inner node param. `scale` folds a
/// unit conversion (e.g. [`DEG2RAD`]) into the write boundary; pass `1.0`
/// for a pass-through. `default_value` mirrors the matching [`card_param`]'s
/// so the slider's fallback (when a project carries no `param_values` slot)
/// still reproduces the authored look.
fn card_binding(
    id: &str,
    name: &str,
    default: f32,
    node_id: &str,
    param: &str,
    scale: f32,
) -> BindingDef {
    BindingDef {
        id: id.to_string(),
        label: name.to_string(),
        default_value: default,
        target: BindingTarget::Node {
            node_id: NodeId::new(node_id),
            param: param.to_string(),
        },
        convert: manifold_core::effects::ParamConvert::Float,
        user_added: false,
        scale,
        offset: 0.0,
    }
}

/// GLTF_ANIMATION_DESIGN.md A4: perform-UI exposure for an animated
/// object's clip clock — Rate (the scrub/speed gesture; `progress` stays
/// wire-only, see the phase brief's Deviation note), Clip (D4 selector),
/// Loop Mode, Retrigger. Pushed into the SAME `card_params`/`card_bindings`
/// vectors every other outer-card knob uses; `node_id` is whichever of
/// `node.gltf_animation_source` / `node.gltf_skeleton_pose` /
/// `node.gltf_morph_weights` owns this clip. `clip_count` sizes the Clip
/// knob's range (0 when unknown treated as 1, so a single-clip object still
/// gets a valid, if inert, 0..0 range rather than an empty one).
fn animation_card_controls(
    card_params: &mut Vec<ParamSpecDef>,
    card_bindings: &mut Vec<BindingDef>,
    id_prefix: &str,
    node_id: &str,
    section: &str,
    clip_count: u32,
) {
    let base = ParamSpecDef {
        id: String::new(),
        name: String::new(),
        min: 0.0,
        max: 1.0,
        default_value: 0.0,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: manifold_core::macro_bank::MacroCurve::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: Some(section.to_string()),
    };

    card_params.push(ParamSpecDef {
        id: format!("{id_prefix}_rate"),
        name: "Rate".to_string(),
        min: 0.0625,
        max: 16.0,
        default_value: 1.0,
        ..base.clone()
    });
    card_bindings.push(card_binding(&format!("{id_prefix}_rate"), "Rate", 1.0, node_id, "rate", 1.0));

    card_params.push(ParamSpecDef {
        id: format!("{id_prefix}_clip"),
        name: "Clip".to_string(),
        min: 0.0,
        max: (clip_count.max(1) - 1) as f32,
        default_value: 0.0,
        whole_numbers: true,
        ..base.clone()
    });
    card_bindings.push(BindingDef {
        id: format!("{id_prefix}_clip"),
        label: "Clip".to_string(),
        default_value: 0.0,
        target: BindingTarget::Node { node_id: NodeId::new(node_id), param: "clip_index".to_string() },
        convert: manifold_core::effects::ParamConvert::IntRound,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
    });

    card_params.push(ParamSpecDef {
        id: format!("{id_prefix}_loop_mode"),
        name: "Loop Mode".to_string(),
        min: 0.0,
        max: (LOOP_MODES.len() - 1) as f32,
        default_value: 0.0,
        whole_numbers: true,
        value_labels: LOOP_MODES.iter().map(|s| s.to_string()).collect(),
        ..base.clone()
    });
    card_bindings.push(BindingDef {
        id: format!("{id_prefix}_loop_mode"),
        label: "Loop Mode".to_string(),
        default_value: 0.0,
        target: BindingTarget::Node { node_id: NodeId::new(node_id), param: "loop_mode".to_string() },
        convert: manifold_core::effects::ParamConvert::EnumRound,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
    });

    card_params.push(ParamSpecDef {
        id: format!("{id_prefix}_retrigger"),
        name: "Retrigger".to_string(),
        min: 0.0,
        max: 1_000_000.0,
        default_value: 0.0,
        whole_numbers: true,
        is_trigger: true,
        ..base
    });
    card_bindings.push(BindingDef {
        id: format!("{id_prefix}_retrigger"),
        label: "Retrigger".to_string(),
        default_value: 0.0,
        target: BindingTarget::Node { node_id: NodeId::new(node_id), param: "trigger_count".to_string() },
        convert: manifold_core::effects::ParamConvert::Trigger,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
    });
}

/// Replace every run of non-alphanumeric characters with a single `_` and
/// trim leading/trailing underscores, so a filename stem like
/// `"cc0__oomurasaki_azalea_r._x_pulchrum"` becomes a clean identifier
/// safe for a [`PresetTypeId`] / OSC path segment. Falls back to
/// `"ImportedModel"` for a stem that sanitizes to nothing, and prefixes a
/// leading digit (OSC/identifier convention) with `Model_`.
fn sanitize_identifier(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut prev_underscore = false;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "ImportedModel".to_string()
    } else if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("Model_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// True when `name` matches one of `build_object_group`'s own deterministic
/// inner-node handles for object `k` (`mesh_{k}`, `mat_{k}`, `pose_{k}`,
/// `skinmesh_{k}`, `morphweights_{k}`, `morphdeltas_{k}`, `morphblend_{k}`,
/// `transform_{k}`, `anim_{k}`, `tex_{k}`) or the group's own literal
/// `"output"` boundary handle. Regression guard for the duplicate-handle
/// panic a glTF material authored with a name like `"mat_0"` produces
/// (`MetalRoughSpheresNoTextures.glb`, 98 materials named `mat_0..mat_97`):
/// SCENE_OBJECT_AND_PANEL_V2's P3 stamps both the group and its inner
/// `node.scene_object` with `group_name` (D6), so when `group_name` equals
/// this object's own `mat_{k}` handle, the group's `node.pbr_material`
/// (handle `mat_{k}`) and the `node.scene_object`/group-output boundary
/// (handle `group_name`) collide once flattened under the group's own
/// handle — `graph.rs`'s `add_node_named` rejects the duplicate.
fn collides_with_object_group_inner_handle(name: &str, k: usize) -> bool {
    name == "output"
        || [
            "mesh", "mat", "pose", "skinmesh", "morphweights", "morphdeltas", "morphblend",
            "transform", "anim", "tex",
        ]
        .iter()
        .any(|prefix| name == format!("{prefix}_{k}"))
}

/// Display / namespace name for one object's group: the glTF material name when
/// present (with the reserved `/` swapped for a space — the flattener uses `/`
/// as the handle namespace separator), else `"Object N"`. Deduped against
/// siblings — two same-named materials would otherwise collide in the flattened
/// handle map — by appending an index until unique. Also deduped against this
/// object's OWN inner-node handle vocabulary (`collides_with_object_group_inner_handle`)
/// — a material literally named e.g. `"mat_0"` would otherwise stamp both the
/// group/scene_object handle AND the sibling `node.pbr_material` handle with
/// the same string, colliding once flattened (see that function's doc comment).
fn unique_group_name(
    material_name: Option<&str>,
    index: usize,
    used: &mut std::collections::HashSet<String>,
) -> String {
    let base = material_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.replace('/', " "))
        .unwrap_or_else(|| format!("Object {}", index + 1));
    let mut name = base.clone();
    let mut n = index + 1;
    while used.contains(&name) || collides_with_object_group_inner_handle(&name, index) {
        name = format!("{base} {n}");
        n += 1;
    }
    used.insert(name.clone());
    name
}

/// A distinct RGBA header tint for object `index`, spread around the hue wheel by
/// the golden ratio (the same scheme `Layer::generate_layer_color` uses for
/// timeline layers) at high saturation — so a multi-mesh import reads as a few
/// colour-coded boxes, never a wall of same-coloured groups.
fn group_tint(index: usize) -> [f32; 4] {
    let hue = (index as f32 * 0.618_034) % 1.0;
    let c = manifold_core::color::Color::hsv_to_rgb(hue, 0.7, 0.85);
    [c.r, c.g, c.b, 1.0]
}

/// Parse `path` and assemble a generator [`EffectGraphDef`] that renders it
/// faithfully: one `node.render_scene` object per distinct material — 1:1,
/// no truncation (GLB_CONFORMANCE_DESIGN.md D4) — each fed its
/// material-filtered geometry + base-color texture (if any) + a PBR
/// material, framed by a synthesized orbit camera sized to the glb's
/// bounding box, lit by one sun light, under a baked IBL envmap (required —
/// `node.pbr_material` is degenerate without one). Pure function: one CPU
/// parse via [`gltf_load::gltf_import_summary`], no GPU, no other I/O.
///
/// Errors when the glb has no materials with geometry (nothing to import),
/// or when it has more than [`OBJECT_SAFETY_MAX`] materials with geometry
/// (a real GPU/port-list safety bound, D4 — never silently truncated) —
/// propagated from [`gltf_load::gltf_import_summary`] or raised here.
pub fn assemble_import_graph(path: &Path) -> Result<(EffectGraphDef, ImportReport), String> {
    let summary = gltf_load::gltf_import_summary(path)?;
    build_import_graph(&summary, path)
}


/// Output of building one object's node group + its wiring into
/// `render_scene` — the reusable core of the per-object loop, factored out
/// of [`build_import_graph`] so [`merge_import_into_graph`] (D5) can build
/// the SAME per-object shape against an EXISTING scene's `render_scene`
/// node and object-index range, without re-running the whole assembler —
/// no camera/envmap/lights/lens, chrome the merge path never touches.
struct ObjectGroupOutput {
    /// The named, tinted group node (`GROUP_TYPE_ID`) — push directly onto
    /// the target level's `nodes`.
    group_node: EffectGraphNode,
    /// Top-level wires from this group's outputs to `render_scene`'s
    /// `mesh_{port_index}` / `material_{port_index}` / … ports — push
    /// directly onto the target level's `wires`.
    wires_to_render: Vec<EffectGraphWire>,
    card_params: Vec<ParamSpecDef>,
    card_bindings: Vec<BindingDef>,
    string_bindings: Vec<StringBindingDef>,
    report_lines: Vec<String>,
    textures_wired: usize,
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
fn build_object_group(
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
) -> ObjectGroupOutput {
    let k = local_k;
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
            let (
                parent_rows,
                root_world_rows,
                inverse_bind_rows,
                translation_rows,
                rotation_rows,
                scale_rows,
                clip_durations_rows,
                duration_s,
            ) = build_skeleton_pose_tables(&obj_skin.info, node_anims_by_clip);
            let mut pose_node =
                plain_node(pose_id, &pose_node_id, "node.gltf_skeleton_pose", &pose_node_id);
            pose_node.params.insert("joint_count".to_string(), int(joint_count as i32));
            pose_node.params.insert("duration_s".to_string(), float(duration_s));
            pose_node.params.insert("joint_parent_table".to_string(), table(parent_rows));
            if !root_world_rows.is_empty() {
                pose_node
                    .params
                    .insert("joint_root_world_table".to_string(), table(root_world_rows));
            }
            pose_node
                .params
                .insert("inverse_bind_table".to_string(), table(inverse_bind_rows));
            pose_node
                .params
                .insert("translation_tracks".to_string(), table(translation_rows));
            pose_node.params.insert("rotation_tracks".to_string(), table(rotation_rows));
            pose_node.params.insert("scale_tracks".to_string(), table(scale_rows));
            pose_node.params.insert("clip_durations".to_string(), table(clip_durations_rows));
            group_nodes.push(pose_node);
            animation_card_controls(
                &mut card_params,
                &mut card_bindings,
                &pose_node_id,
                &pose_node_id,
                &group_name,
                node_anims_by_clip.len().max(1) as u32,
            );

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
            let (weight_rows, weights_clip_durations_rows, weights_duration_s) =
                build_morph_weight_table(morph, node_anims_by_clip);
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
                .insert("weight_tracks".to_string(), table(weight_rows));
            weights_node
                .params
                .insert("clip_durations".to_string(), table(weights_clip_durations_rows));
            group_nodes.push(weights_node);
            animation_card_controls(
                &mut card_params,
                &mut card_bindings,
                &weights_node_id,
                &weights_node_id,
                &group_name,
                node_anims_by_clip.len().max(1) as u32,
            );

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

        // No per-object Metallic/Roughness card sliders (Peter, 2026-07-15:
        // "no need to modify them and they explode the card" — with one pair
        // per object, a multi-object import's card grew unusably long). The
        // material node above still carries the glTF's own metallic/
        // roughness values; only the card exposure is gone.
        // IMPORT_FIDELITY_DESIGN.md D8/F-P5 performer gesture: glass objects
        // ONLY get an Opacity knob (solid → ghost mid-set on the material's
        // alpha) — opaque/mask objects don't expose it, matching the
        // "inline mux option table params" discipline of not over-exposing
        // every knob to every object. GLB_CONFORMANCE_DESIGN.md D4: card
        // curation is UI-only and caps at CARD_CURATION_MAX — `materials`
        // is sorted largest-vertex-count-first above, so `k < CARD_CURATION_MAX`
        // IS "the largest 16"; every object still gets full graph wiring
        // regardless of `k`, this gate only withholds the card slider.
        if is_glass && k < CARD_CURATION_MAX {
            let opacity_id = format!("opacity_{k}");
            card_params.push(card_param(&opacity_id, "Opacity", 0.0, 1.0, effective_alpha, false, &group_name));
            card_bindings.push(card_binding(
                &opacity_id, "Opacity", effective_alpha, &mat_node_id, "color_a", 1.0,
            ));
        }
        // One shared "Ambient" fill knob fans out to every material's ambient
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

            let mut translation_rows = Vec::new();
            let mut rotation_rows = Vec::new();
            let mut scale_rows = Vec::new();
            let mut clip_durations_rows = Vec::with_capacity(m.animations.len());
            let mut fallback_duration_s = 1e-6;
            for (c, clip_anim) in m.animations.iter().enumerate() {
                let Some(anim) = clip_anim else {
                    clip_durations_rows.push(vec![c as f32, 1e-6]);
                    continue;
                };
                let prefix = [c as f32];
                if let Some(t) = &anim.translation {
                    for (i, (time, v)) in t.times.iter().zip(t.values.iter()).enumerate() {
                        let (in_t, out_t) = t.tangents_at(i);
                        push_vec3_row(&mut translation_rows, &prefix, *time, *v, t.mode, in_t, out_t);
                    }
                }
                if let Some(r) = &anim.rotation {
                    for (i, (time, v)) in r.times.iter().zip(r.values.iter()).enumerate() {
                        let (in_t, out_t) = r.tangents_at(i);
                        push_quat_row(&mut rotation_rows, &prefix, *time, *v, r.mode, in_t, out_t);
                    }
                }
                if let Some(s) = &anim.scale {
                    for (i, (time, v)) in s.times.iter().zip(s.values.iter()).enumerate() {
                        let (in_t, out_t) = s.tangents_at(i);
                        push_vec3_row(&mut scale_rows, &prefix, *time, *v, s.mode, in_t, out_t);
                    }
                }
                let duration_s = anim.duration_s.max(1e-6);
                clip_durations_rows.push(vec![c as f32, duration_s]);
                if c == 0 {
                    fallback_duration_s = duration_s;
                }
            }
            anim_node.params.insert("duration_s".to_string(), float(fallback_duration_s));
            anim_node.params.insert("clip_durations".to_string(), table(clip_durations_rows));
            if !translation_rows.is_empty() {
                anim_node.params.insert("translation_track".to_string(), table(translation_rows));
            }
            if !rotation_rows.is_empty() {
                anim_node.params.insert("rotation_track".to_string(), table(rotation_rows));
            }
            if !scale_rows.is_empty() {
                anim_node.params.insert("scale_track".to_string(), table(scale_rows));
            }
            group_nodes.push(anim_node);
            animation_card_controls(
                &mut card_params,
                &mut card_bindings,
                &anim_node_id,
                &anim_node_id,
                &group_name,
                m.animations.len().max(1) as u32,
            );
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
    }
}

/// Assemble the generator graph from an already-parsed [`GltfImportSummary`].
/// Split from [`assemble_import_graph`] (which owns the single file parse) so the
/// graph shape — including the per-object node **grouping** — is testable against
/// a synthetic summary with no `.glb` fixture on disk.
///
/// Each distinct material becomes one node **group** (`GROUP_TYPE_ID`) named for
/// the material: its `node.gltf_mesh_source` + `node.pbr_material` +
/// `node.transform_3d` (+ optional `node.gltf_texture_source`) live inside,
/// bound by an inner `node.scene_object` and exposed through a
/// `system.group_output` as a single `object: Object` port, wired to the
/// shared `node.render_scene`'s `object_{k}` port (SCENE_OBJECT_AND_PANEL_V2_DESIGN
/// D1/D3/D4). Grouping is a pure presentation layer: it flattens
/// away at load (`manifold_core::flatten::flatten_groups`, run inside
/// `instantiate_def`) to the exact same flat graph, and every inner node keeps its
/// stable `node_id`, so the card/string bindings that target `mesh_k`/`mat_k`/
/// `tex_k`/`transform_k` by id resolve unchanged (see `docs/GROUPING_GRAPHS.md` §2).
fn build_import_graph(
    summary: &GltfImportSummary,
    path: &Path,
) -> Result<(EffectGraphDef, ImportReport), String> {
    if summary.materials.is_empty() {
        return Err(format!(
            "{}: no materials with geometry — nothing to import",
            path.display()
        ));
    }

    // GLB_CONFORMANCE_DESIGN.md D4: import is 1:1 — every material with
    // geometry gets its own render_scene object, never a truncated prefix.
    // `OBJECT_SAFETY_MAX` (1024) is a real GPU/port-list safety bound, not a
    // curation cap: an asset beyond it errors loudly instead of silently
    // dropping geometry (the AMG GT3's black body, BUG-163, was exactly
    // this — 14 of 78 materials, including the livery, dropped over the old
    // 64-object cap).
    if summary.materials.len() > OBJECT_SAFETY_MAX as usize {
        return Err(format!(
            "{}: {} materials with geometry exceeds the {}-object safety bound — \
             this asset cannot be imported 1:1 without risking a runaway port-list \
             (raise OBJECT_SAFETY_MAX in render_scene.rs if a real asset legitimately \
             needs more; never silently truncate)",
            path.display(),
            summary.materials.len(),
            OBJECT_SAFETY_MAX,
        ));
    }
    // Largest-by-vertex-count first: not a truncation boundary anymore
    // (every material is wired), but it IS the ordering the card curation
    // below relies on — the largest CARD_CURATION_MAX objects by vertex
    // count get per-object sliders, everything after them still gets full
    // geometry/material/texture wiring, just no card exposure.
    let mut materials = summary.materials.clone();
    materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
    let n = materials.len();
    if summary.default_material_vertex_count > 0 {
        // BUG-171 made this geometry import as a real object (a synthetic
        // default-material entry); BUG-207 made that entry resolve its own
        // skin/morph/animation too. This log line is informational, not a
        // warning about dropped data — kept at `warn!` so it's still easy
        // to spot in a log dump when triaging an import.
        log::warn!(
            "gltf_import::assemble_import_graph({}): {} vertices belong to glTF's unassigned \
             default material — imported as a normal object (glTF spec §3.9.2 implicit default)",
            path.display(),
            summary.default_material_vertex_count,
        );
    }
    if summary.camera_count > 0 {
        log::info!(
            "gltf_import::assemble_import_graph({}): glb carries {} embedded camera(s) — v1 \
             ignores them and synthesizes its own bbox-framed orbit camera",
            path.display(),
            summary.camera_count,
        );
    }

    let center = [
        (summary.bbox_min[0] + summary.bbox_max[0]) * 0.5,
        (summary.bbox_min[1] + summary.bbox_max[1]) * 0.5,
        (summary.bbox_min[2] + summary.bbox_max[2]) * 0.5,
    ];
    let dims = [
        summary.bbox_max[0] - summary.bbox_min[0],
        summary.bbox_max[1] - summary.bbox_min[1],
        summary.bbox_max[2] - summary.bbox_min[2],
    ];
    let radius =
        ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(1e-3);
    // Camera vertical FOV — hoisted here so both the framing-distance fit
    // below and the camera node's own `fov_y` param (set further down) read
    // the SAME value; a duplicated literal is how BUG-206-style drift
    // happens in the first place.
    let fov_y = 0.9_f32;
    // BUG-206 fix: `2.2 * radius` alone frames by the bbox half-DIAGONAL,
    // which for an elongated (tall/thin) object barely exceeds its dominant
    // axis — the frame's vertical span contains the object with almost no
    // margin, and camera tilt + perspective push it past the top/bottom
    // edges. Frame by PER-AXIS fit instead: for each axis, the distance
    // required so that half the axis's extent subtends no more than the
    // half-FOV, with a 1.15 safety margin. The render aspect isn't known at
    // import time, so the horizontal half-angle is conservatively treated
    // as equal to the vertical one (square-aspect assumption — never
    // UNDER-frames a wider-than-tall render).
    let half_fov_tan = (fov_y * 0.5).tan();
    let per_axis_fit = dims
        .iter()
        .map(|&extent| (extent * 0.5) / half_fov_tan * 1.15)
        .fold(0.0f32, f32::max);
    // The `2.2 * radius` floor keeps every COMPACT asset's framing IDENTICAL
    // to before this fix (the golden-stability guarantee: per_axis_fit is
    // only ever larger than the floor for objects dominated by one axis,
    // where the diagonal-based distance genuinely under-frames).
    let distance = (2.2 * radius).max(per_axis_fit);
    // BUG-165/BUG-169 root cause (diagnosed via GLB_XFAIL_BURNDOWN_DESIGN.md
    // P1's `--trace` instrument): `node.orbit_camera`'s `near` clip plane
    // defaults to a fixed 0.05 (camera_orbit.rs), which was never scaled to
    // the framed object's own size. `distance = 2.2 * radius` already
    // scales with the object, so the object's front face sits at
    // `distance - radius == 1.2 * radius` from the camera — for any object
    // with `radius` below ~0.042 (BoomBox: 0.0172, MetalRoughSpheresNoTextures:
    // 0.0056 — both real-world-scale Khronos assets authored in meters),
    // the fixed near plane sits IN FRONT of the object and the whole frame
    // clips to black every frame (confirmed via `--trace`: io_pending goes
    // false almost immediately and the frame stays byte-stable-black from
    // frame 0/1 — not a decode race, ruling out the BUG-165 (a) hypothesis;
    // BUG-169's "lighting/material" hypothesis was also wrong — same
    // mechanism, not a texture-less-material bug).
    //
    // Fix: `near` tracks the object's own front-face distance (with a 2x
    // safety margin so the surface never grazes the plane), capped at the
    // pre-existing 0.05 default so every currently-passing asset whose
    // front face already clears 0.05 gets the IDENTICAL near value as
    // before (front_margin = 1.2 * radius stays >= 0.05 whenever radius >=
    // ~0.0417 — true for every other passing Khronos asset checked:
    // WaterBottle radius 0.151, DamagedHelmet 1.64, MetalRoughSpheres 6.99,
    // TextureSettingsTest 7.21, Duck 1.27, Box 0.87). Only genuinely
    // sub-threshold objects get a smaller near plane.
    let front_margin = (distance - radius).max(1e-4);
    let near_clip = CAMERA_NEAR_DEFAULT.min(front_margin * 0.5);

    let path_str = path.to_string_lossy().into_owned();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ImportedModel".to_string());
    let sanitized = sanitize_identifier(&stem);
    let osc_prefix = sanitized.to_lowercase();

    let mut nodes = Vec::new();
    let mut wires = Vec::new();
    let mut next_id = 0u32;
    let mut fresh_id = move || {
        let v = next_id;
        next_id += 1;
        v
    };

    let input_id = fresh_id();
    nodes.push(plain_node(input_id, "input", GENERATOR_INPUT_TYPE_ID, "input"));

    // IMPORT_FIDELITY_DESIGN.md D7 (F-P4) — the default import look is the
    // "black-void studio": `mode = softbox` (exact-zero black base + bright
    // emitter strips, never the legacy gradient studio) at `intensity = 1.0`
    // (the F-P1/F-P3 default already matches `mode`'s own primitive
    // defaults — emitter_count/intensity/elevation/width — so nothing else
    // needs stamping here). Superseded the old "intensity 0, lights-only"
    // default; the Environment card below (`env_intensity`) now starts at
    // 1.0 to match.
    let envmap_id = fresh_id();
    let mut envmap_node = plain_node(envmap_id, "envmap", "node.bake_environment", "envmap");
    envmap_node.params.insert("intensity".to_string(), float(1.0));
    envmap_node.params.insert("mode".to_string(), enum_val(1)); // Softbox
    // F-P7 dome fill: metals are lit exclusively by the environment, so
    // against D7's pure-black void every metallic import read as dark
    // chrome regardless of its albedo (the 2026-07-15 helmet/AMG failure).
    // A modest neutral dome gives metals a world to reflect while the
    // background stays black (the envmap is never drawn as a backdrop).
    // The Fill Light card slider below dials it, 0 = the original void.
    envmap_node.params.insert("fill".to_string(), float(IMPORT_FILL_DEFAULT));
    envmap_node
        .params
        .insert("emitter_intensity".to_string(), float(IMPORT_STRIPS_DEFAULT));
    nodes.push(envmap_node);

    // GLB_CONFORMANCE_DESIGN.md D6 — `node.hdri_source` decodes a real-world
    // linear-HDR .exr (Browse-wired via the `hdri_file` string binding
    // below) and `node.switch_texture` picks between it and the softbox
    // bake above by the `env_mode` card enum (default 0 = Softbox, so the
    // black-void aesthetic stays the import default — Peter, 2026-07-15:
    // "I quite like the pure void and sunlight only look"). `render_scene`'s
    // `envmap` input now wires from the switch's `out`, not the bake
    // directly.
    let hdri_id = fresh_id();
    nodes.push(plain_node(hdri_id, "hdri", "node.hdri_source", "hdri"));

    // HDRI exposure stage: `node.bake_environment` has its own `intensity`
    // master (the Environment card fader's original target), but a decoded
    // EXR arrives at the file's true radiance — a real daytime pure-sky
    // HDRI averages ~0.2–0.4 linear (measured on kloppenheim_07: mean 0.24,
    // sky half 0.34), roughly 4× dimmer than the softbox default. Without
    // an exposure stage the Environment fader would be dead in HDRI mode
    // and the performer would have no way to bring a real sky up to stage
    // brightness. `node.gain` on the HDRI branch (range 0–4, matching the
    // card fader) restores symmetry: env_intensity fans out to BOTH
    // envmap.intensity and this gain, so one fader is the environment
    // master in either mode — same fan-out pattern as the sun_x/y/z macros.
    // (`node.exposure` is the gain atom's type id; its param is `gain`.)
    let hdri_gain_id = fresh_id();
    nodes.push(plain_node(hdri_gain_id, "hdri_gain", "node.exposure", "hdri_gain"));

    let env_select_id = fresh_id();
    let mut env_select_node =
        plain_node(env_select_id, "env_select", "node.switch_texture", "env_select");
    env_select_node.params.insert("num_inputs".to_string(), int(2));
    env_select_node.params.insert("selector".to_string(), float(0.0)); // 0 = Softbox
    nodes.push(env_select_node);

    let camera_id = fresh_id();
    let mut cam_node = plain_node(camera_id, "camera", "node.orbit_camera", "camera");
    cam_node.params.insert("orbit".to_string(), float(0.7));
    cam_node.params.insert("tilt".to_string(), float(0.3));
    cam_node.params.insert("distance".to_string(), float(distance));
    cam_node.params.insert("fov_y".to_string(), float(fov_y));
    cam_node.params.insert("look_y".to_string(), float(0.0));
    // BUG-165/BUG-169 fix — see `near_clip` computation above.
    cam_node.params.insert("near".to_string(), float(near_clip));
    nodes.push(cam_node);

    // Physical lens (CINEMATIC_POST D6): sits between the raw orbit camera
    // and render_scene/ao. No depth-of-field consumer wired anymore (the
    // "dof" group was removed 2026-07-15 for buggy visuals) and no
    // motion_blur consumer either (see the motion_blur removal note below),
    // so `shutter_angle`/`focus_distance`/`f_stop` are along for the ride
    // only insofar as `node.camera_lens` requires them — nothing downstream
    // reads them today.
    let lens_id = fresh_id();
    let mut lens_node = plain_node(lens_id, "lens", "node.camera_lens", "lens");
    lens_node.params.insert("focus_distance".to_string(), float(distance));
    lens_node.params.insert("f_stop".to_string(), float(32.0));
    nodes.push(lens_node);

    let sun_id = fresh_id();
    let mut sun_node = plain_node(sun_id, "sun", "node.light", "sun");
    sun_node.params.insert("mode".to_string(), enum_val(0)); // Sun
    sun_node.params.insert("pos_x".to_string(), float(5.0));
    sun_node.params.insert("pos_y".to_string(), float(2.0));
    sun_node.params.insert("pos_z".to_string(), float(3.0));
    sun_node.params.insert("aim_x".to_string(), float(0.0));
    sun_node.params.insert("aim_y".to_string(), float(0.0));
    sun_node.params.insert("aim_z".to_string(), float(0.0));
    // 3.5, not the primitive's 1.5 default: `node.pbr_material` divides diffuse
    // by π (energy conservation), so a unit-intensity sun lands a fully-facing
    // matte surface at ~0.32 — a dark subject then reads near-black. ~3.5
    // offsets the /π so an imported model is legible under the default rig
    // without the user having to touch the light. Aesthetic default; the light
    // node is a normal graph node the user can dial down.
    sun_node.params.insert("intensity".to_string(), float(3.5));
    // Hard shadow softness: crisp, defined shadows suit the dramatic
    // single-key "model on black" look. The Shadow Type card lets the user
    // soften to Soft/Very Soft/Contact. `light_size` is inert for Hard (no
    // penumbra), kept at 1.0 so a switch to a softer tier gives a sensible
    // penumbra width.
    sun_node.params.insert("cast_shadows".to_string(), float(1.0));
    sun_node.params.insert("shadow_softness".to_string(), enum_val(0)); // Hard
    sun_node.params.insert("light_size".to_string(), float(1.0));
    // Shadow quality: the Sun's `range` is the shadow's orthographic
    // half-extent, and the shadow map's texels spread across it. The default
    // 30 wraps a huge area, so on a recentered hero mesh (spanning ±radius
    // about the origin) almost none of the map's texels land on the subject —
    // that's the blocky, texel-stepping look as the sun moves. Wrap the extent
    // tightly to the model (radius × 1.5 for margin) and quadruple the map to
    // 4096², so the shadow texels are fine enough that edges read crisp and
    // per-frame motion stops stepping. Both are normal light-node params the
    // user can still dial.
    sun_node.params.insert("range".to_string(), float((radius * 1.5).max(0.01)));
    sun_node.params.insert("shadow_resolution".to_string(), float(4096.0));
    nodes.push(sun_node);

    let render_id = fresh_id();
    let mut render_node = plain_node(render_id, "render", "node.render_scene", "render");
    render_node.params.insert("objects".to_string(), int(n as i32));
    render_node.params.insert("lights".to_string(), int(1));

    let mut string_bindings = Vec::new();
    // Curated outer-card performance surface (D9 / P0). Camera + light +
    // envmap are added once after the loop; per-object material knobs are
    // pushed here so they interleave with the `mat_k` nodes they target.
    let mut card_params: Vec<ParamSpecDef> = Vec::new();
    let mut card_bindings: Vec<BindingDef> = Vec::new();
    let mut textures_wired = 0usize;
    // D9 doctrine — see `ImportReport::report_lines`.
    let mut report_lines: Vec<String> = Vec::new();

    // Group names, deduped so two identically-named glTF materials don't collide
    // in the flattened handle map (the flattener prefixes every inner handle with
    // the group name).
    let mut used_group_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // GLTF_ANIMATION_DESIGN.md A2: clip `[0]`'s per-node TRS tracks, keyed
    // by node index — the same lookup `gltf_load::gltf_import_summary`
    // builds internally for A1's rigid-object resolution, rebuilt here
    // because `node.gltf_skeleton_pose`'s Tables need it per-JOINT (every
    // joint in a skin, not just the one mesh-owning node A1 resolves
    // against). A4: ALL clips, not just clip 0 (D4 multi-clip selection).
    let node_anims_by_clip: Vec<std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>> =
        summary.animations.iter().map(|a| a.nodes.iter().map(|n| (n.node_index, n.clone())).collect()).collect();

    for (k, m) in materials.iter().enumerate() {
        let mut out = build_object_group(
            k,
            k,
            render_id,
            m,
            &path_str,
            center,
            &node_anims_by_clip,
            &mut used_group_names,
            &mut fresh_id,
            radius,
        );
        nodes.push(out.group_node);
        wires.append(&mut out.wires_to_render);
        card_params.append(&mut out.card_params);
        card_bindings.append(&mut out.card_bindings);
        string_bindings.append(&mut out.string_bindings);
        report_lines.append(&mut out.report_lines);
        textures_wired += out.textures_wired;
    }

    nodes.push(render_node);

    // No atmosphere node (fog + god rays removed, Peter 2026-07-15): the
    // BUG-149 scene-scaled fog and shaft knobs never produced the look he
    // wanted on imports — the cinematic void-haze treatment is a pending
    // design of its own (`project_void_haze_design_pending`), not two
    // faders on this card. render_scene's `atmosphere` input is lazy, so
    // leaving it unwired is byte-identical to the old wired-at-defaults
    // node with both sliders at 0.

    // SSAO contact-occlusion arm, packaged as the same "ao" node group
    // CinematicScene ships (CINEMATIC_POST D9): ssao_gtao → bilateral_blur
    // (H then V) denoise → Multiply onto render_scene's color. Grouped so
    // the graph reads as one labeled box, same as every object group above;
    // flattens to the identical atom wiring at load
    // (`manifold_core::flatten::flatten_groups`, docs/NODE_GROUPS_DESIGN.md).
    //
    // SSAO radius is a WORLD-space distance and the importer never rescales
    // the model, so a hero mesh can be fractions of a unit or hundreds across.
    // Scale the default to the model's own bounding radius (kept inside the
    // atom's declared 0.01..5.0 envelope) so contact shadows read at any size.
    let ssao_radius_default = (radius * 0.5).clamp(0.01, 5.0);

    let mut ao_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut ao_wires: Vec<EffectGraphWire> = Vec::new();
    let ao_in_id = fresh_id();
    ao_nodes.push(plain_node(ao_in_id, "ao_in", GROUP_INPUT_TYPE_ID, "input"));
    let ssao_id = fresh_id();
    let mut ssao_node = plain_node(ssao_id, "ssao", "node.ssao_gtao", "ssao");
    ssao_node.params.insert("radius".to_string(), float(ssao_radius_default));
    ssao_node.params.insert("intensity".to_string(), float(1.0));
    ao_nodes.push(ssao_node);
    let ssao_mix_id = fresh_id();
    let mut ssao_mix_node = plain_node(ssao_mix_id, "ssao_mix", "node.mix", "ssao_mix");
    ssao_mix_node.params.insert("amount".to_string(), float(1.0));
    ssao_mix_node.params.insert("mode".to_string(), enum_val(4)); // Multiply
    ao_nodes.push(ssao_mix_node);
    let bilat_h_id = fresh_id();
    let mut bilat_h_node = plain_node(bilat_h_id, "bilat_h", "node.bilateral_blur", "bilat_h");
    bilat_h_node.params.insert("axis".to_string(), enum_val(0));
    bilat_h_node.params.insert("depth_sigma".to_string(), float(0.1));
    ao_nodes.push(bilat_h_node);
    let bilat_v_id = fresh_id();
    let mut bilat_v_node = plain_node(bilat_v_id, "bilat_v", "node.bilateral_blur", "bilat_v");
    bilat_v_node.params.insert("axis".to_string(), enum_val(1));
    bilat_v_node.params.insert("depth_sigma".to_string(), float(0.1));
    ao_nodes.push(bilat_v_node);
    let ao_out_id = fresh_id();
    ao_nodes.push(plain_node(ao_out_id, "ao_out", GROUP_OUTPUT_TYPE_ID, "output"));
    ao_wires.push(wire(ao_in_id, "depth", ssao_id, "depth"));
    ao_wires.push(wire(ao_in_id, "camera", ssao_id, "camera"));
    ao_wires.push(wire(ao_in_id, "color", ssao_mix_id, "a"));
    ao_wires.push(wire(ssao_id, "out", bilat_h_id, "in"));
    ao_wires.push(wire(ao_in_id, "depth", bilat_h_id, "depth"));
    ao_wires.push(wire(ao_in_id, "camera", bilat_h_id, "camera"));
    ao_wires.push(wire(bilat_h_id, "out", bilat_v_id, "in"));
    ao_wires.push(wire(ao_in_id, "depth", bilat_v_id, "depth"));
    ao_wires.push(wire(ao_in_id, "camera", bilat_v_id, "camera"));
    ao_wires.push(wire(bilat_v_id, "out", ssao_mix_id, "b"));
    ao_wires.push(wire(ssao_mix_id, "out", ao_out_id, "out"));

    let ao_group_id = fresh_id();
    let mut ao_group_node = plain_node(ao_group_id, "ao", GROUP_TYPE_ID, "ao");
    ao_group_node.title = Some("Ambient Occlusion".to_string());
    ao_group_node.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: vec![
                InterfacePortDef { name: "depth".to_string(), port_type: "Texture2D".to_string() },
                InterfacePortDef { name: "camera".to_string(), port_type: "Camera".to_string() },
                InterfacePortDef { name: "color".to_string(), port_type: "Texture2D".to_string() },
            ],
            outputs: vec![InterfacePortDef { name: "out".to_string(), port_type: "Texture2D".to_string() }],
            params: Vec::new(),
        },
        nodes: ao_nodes,
        wires: ao_wires,
        tint: None,
    }));
    nodes.push(ao_group_node);

    // Depth of Field group removed (Peter, 2026-07-15): the coc_dilate/
    // bokeh_gather chain read as buggy in practice and made imported scenes
    // hard to look at. `render_scene → ao → final` now, no dof stage; the
    // "lens" node stays — it still feeds render_scene's/ao's `camera` input
    // and the FOV card knob.

    // No `node.motion_blur` tail: it's live-tracked as BUG-136 (MED-HIGH,
    // `docs/BUG_BACKLOG.md`) — orbiting the camera in the running app
    // produces no visible blur despite the wiring, shader math, and
    // velocity buffer all having been runtime-confirmed correct through
    // exhaustive headless probing. The unresolved suspects live in the
    // live app's UI-thread param propagation / render-loop scheduling,
    // outside what this assembler controls. It's also disproportionately
    // expensive for a no-op: `graph_tool fusion` on the assembled import
    // graph shows the whole GTAO/DoF chain (and this node too, had it
    // stayed) failing to fuse into anything but one 2-node region — every
    // atom pays its own dispatch (BUG-141, also open) — so a broken,
    // always-on 32px gather was pure cost with no visual payoff. Retry
    // once BUG-136 lands a root cause.
    let final_id = fresh_id();
    nodes.push(plain_node(final_id, "final", FINAL_OUTPUT_TYPE_ID, "final"));

    // The lens sits between the raw orbit camera and every downstream
    // consumer (render_scene, the ao group) — the node whose fov_y the
    // Camera FOV card slider reads.
    wires.push(wire(camera_id, "out", lens_id, "camera"));
    wires.push(wire(lens_id, "out", render_id, "camera"));
    // D6 env_mode switch: envmap(bake) -> in_0 (Softbox), hdri -> gain ->
    // in_1 (HDRI, with the exposure stage — see the hdri_gain node above),
    // the switch's `out` feeds render_scene same as the direct wire used to.
    wires.push(wire(envmap_id, "envmap", env_select_id, "in_0"));
    wires.push(wire(hdri_id, "out", hdri_gain_id, "in"));
    wires.push(wire(hdri_gain_id, "out", env_select_id, "in_1"));
    wires.push(wire(env_select_id, "out", render_id, "envmap"));
    wires.push(wire(sun_id, "out", render_id, "light_0"));

    // render_scene → ao (contact AO) → final.
    wires.push(wire(render_id, "depth", ao_group_id, "depth"));
    wires.push(wire(lens_id, "out", ao_group_id, "camera"));
    wires.push(wire(render_id, "color", ao_group_id, "color"));
    wires.push(wire(ao_group_id, "out", final_id, "in"));

    // Shared framing / light / environment card knobs. These come LAST in
    // `card_params` (after the per-object material knobs) but read first on
    // the card as the primary performance controls. Angle sliders store
    // RADIANS and carry `is_angle` (the app-wide convention — the slider
    // shows/edits degrees, storage is radians), so the binding is a
    // pass-through (`scale = 1.0`); mixing a degrees store with an `is_angle`
    // formatter double-converts (40° → 2298°). Defaults mirror the
    // `camera`/`sun`/`envmap` node params set above so the card is a faithful
    // mirror of the assembled look.
    // Both angle sliders `wraps: true` (Peter, 2026-07-15: "properly wrap 360
    // degrees") — a drag/modulation that crosses ±180° loops back round
    // instead of sticking at the edge (`constrain_to_range`'s wrap arm,
    // `manifold-core/src/params.rs`). Tilt widens from its old ±89° clamp to
    // the same full ±180° span as orbit: `camera_orbit`'s
    // `distance*sin(tilt)`/`cos(tilt)` spherical math is continuous through
    // the poles, so a full loop is a smooth over-the-top orbit, not a
    // singularity.
    let mut cam_orbit_param = card_param(
        "cam_orbit", "Camera Orbit", -180.0 * DEG2RAD, 180.0 * DEG2RAD, 0.7, true, "Camera",
    );
    cam_orbit_param.wraps = true;
    card_params.push(cam_orbit_param);
    card_bindings.push(card_binding(
        "cam_orbit", "Camera Orbit", 0.7, "camera", "orbit", 1.0,
    ));
    let mut cam_tilt_param = card_param(
        "cam_tilt", "Camera Tilt", -180.0 * DEG2RAD, 180.0 * DEG2RAD, 0.3, true, "Camera",
    );
    cam_tilt_param.wraps = true;
    card_params.push(cam_tilt_param);
    card_bindings.push(card_binding(
        "cam_tilt", "Camera Tilt", 0.3, "camera", "tilt", 1.0,
    ));
    card_params.push(card_param(
        "cam_dist",
        "Camera Distance",
        0.1,
        (distance * 4.0).max(1.0),
        distance,
        false,
        "Camera",
    ));
    card_bindings.push(card_binding(
        "cam_dist", "Camera Distance", distance, "camera", "distance", 1.0,
    ));
    card_params.push(card_param(
        "cam_fov", "Camera FOV", 20.0 * DEG2RAD, 120.0 * DEG2RAD, 0.9, true, "Camera",
    )); // angle (radians)
    card_bindings.push(card_binding(
        "cam_fov", "Camera FOV", 0.9, "camera", "fov_y", 1.0,
    ));

    card_params.push(card_param("sun_int", "Sun Intensity", 0.0, 10.0, 3.5, false, "Sun"));
    card_bindings.push(card_binding("sun_int", "Sun Intensity", 3.5, "sun", "intensity", 1.0));
    // D7 sun coherence (2026-07-15, Peter: "place these fake strips and
    // lights in the same positions as the real scene lights so it looks
    // coherent") — each sun_x/y/z macro fans out to TWO targets: the sun
    // `node.light`'s position (unchanged, pre-existing) AND the envmap's
    // new `sun_x/sun_y/sun_z` disc-direction params (F-P2/F-P3). Direction
    // params bind 1:1 (no conversion math) — `node.bake_environment`
    // normalizes the raw vector internally, and `aim` stays fixed at the
    // origin, so this object's `pos_*` IS already the sun's direction. One
    // fader now moves illumination, shadow, AND envmap reflection together.
    card_params.push(card_param("sun_x", "Sun X", -15.0, 15.0, 5.0, false, "Sun"));
    card_bindings.push(card_binding("sun_x", "Sun X", 5.0, "sun", "pos_x", 1.0));
    card_bindings.push(card_binding("sun_x", "Sun X", 5.0, "envmap", "sun_x", 1.0));
    card_params.push(card_param("sun_y", "Sun Y", -15.0, 15.0, 2.0, false, "Sun"));
    card_bindings.push(card_binding("sun_y", "Sun Y", 2.0, "sun", "pos_y", 1.0));
    card_bindings.push(card_binding("sun_y", "Sun Y", 2.0, "envmap", "sun_y", 1.0));
    card_params.push(card_param("sun_z", "Sun Z", -15.0, 15.0, 3.0, false, "Sun"));
    card_bindings.push(card_binding("sun_z", "Sun Z", 3.0, "sun", "pos_z", 1.0));
    card_bindings.push(card_binding("sun_z", "Sun Z", 3.0, "envmap", "sun_z", 1.0));
    // Shadow tier on the outer card: crisp-vs-soft is a look decision, so it
    // rides next to the sun sliders instead of requiring a trip into the
    // graph. Labels must stay in `ShadowSoftness` variant order — the
    // EnumRound binding writes the label's index straight into the light's
    // `shadow_softness` enum. Default 0 (Hard) mirrors the crisp-shadow
    // default the assembler stamps on the sun node above.
    let mut shadow_param = card_param("sun_shadow", "Shadow Type", 0.0, 3.0, 0.0, false, "Sun");
    shadow_param.whole_numbers = true;
    shadow_param.value_labels = vec![
        "Hard".to_string(),
        "Soft".to_string(),
        "Very Soft".to_string(),
        "Contact".to_string(),
    ];
    card_params.push(shadow_param);
    let mut shadow_binding =
        card_binding("sun_shadow", "Shadow Type", 0.0, "sun", "shadow_softness", 1.0);
    shadow_binding.convert = manifold_core::effects::ParamConvert::EnumRound;
    card_bindings.push(shadow_binding);

    // `node.bake_environment`'s `intensity` master scales the WHOLE baked map
    // (every studio term), so 0 = a black environment and no image-based
    // lighting at all — unlike `horizon_strength`, which only dims the horizon
    // band and leaves the bright zenith/sun terms lighting the scene. D7
    // (F-P4): default flips to 1.0 — the softbox black-void studio is now
    // the import default (envmap node param above), so the card's default
    // must mirror it (range stays 0–4, unchanged).
    card_params.push(card_param("env_intensity", "Environment", 0.0, 4.0, 1.0, false, "Environment"));
    card_bindings.push(card_binding(
        "env_intensity", "Environment", 1.0, "envmap", "intensity", 1.0,
    ));
    // G-P6: the Environment master also drives the HDRI branch's exposure
    // gain (see the hdri_gain node above), so the fader is live in BOTH
    // env_mode positions — softbox scales inside the bake, HDRI scales the
    // decoded map. Each mode passes through exactly one of the two targets,
    // so there is no double-scaling. Same fan-out pattern as sun_x/y/z.
    card_bindings.push(card_binding(
        "env_intensity", "Environment", 1.0, "hdri_gain", "gain", 1.0,
    ));
    // F-P7 — the softbox dome fill (see the envmap node above). Separate
    // from `env_intensity` (which scales strips + disc + fill together):
    // this one moves ONLY the broad dome radiance, i.e. how much "world"
    // the metals reflect, without touching the strip highlights.
    card_params.push(card_param(
        "env_fill", "Fill Light", 0.0, 2.0, IMPORT_FILL_DEFAULT, false, "Environment",
    ));
    card_bindings.push(card_binding(
        "env_fill", "Fill Light", IMPORT_FILL_DEFAULT, "envmap", "fill", 1.0,
    ));
    // The strip emitters' own fader (F-P7): strips are the chrome-streak
    // accent, the fill is the world — independent faders, deliberately not
    // folded into `env_intensity` (which scales the whole bake).
    card_params.push(card_param(
        "env_strips", "Strip Lights", 0.0, 12.0, IMPORT_STRIPS_DEFAULT, false, "Environment",
    ));
    card_bindings.push(card_binding(
        "env_strips", "Strip Lights", IMPORT_STRIPS_DEFAULT, "envmap", "emitter_intensity", 1.0,
    ));
    // GLB_CONFORMANCE_DESIGN.md D6 — HDRI environment mode. env_mode picks
    // between the softbox bake (default, index 0) and the decoded HDRI file
    // (index 1) via `env_select`'s `selector` param (a plain Float — the
    // node.switch_texture family, not an Enum-typed node param — so the
    // binding is a Float pass-through like the mux-option-table pattern,
    // not EnumRound). `env_mode = 1` with an empty `hdri_file` reads as
    // black (node.hdri_source clears `out` to black until a file decodes),
    // same "nothing wired yet" convention as every other unwired texture
    // source in this graph.
    let mut env_mode_param = card_param("env_mode", "Environment Mode", 0.0, 1.0, 0.0, false, "Environment");
    env_mode_param.whole_numbers = true;
    env_mode_param.value_labels = vec!["Softbox".to_string(), "HDRI".to_string()];
    card_params.push(env_mode_param);
    card_bindings.push(card_binding(
        "env_mode", "Environment Mode", 0.0, "env_select", "selector", 1.0,
    ));
    // The HDRI file itself is a SEPARATE Browse field/string param from
    // "model_file" (the imported .glb's own path) — a different file, a
    // different picker, never defaulted to the glb path. Empty until the
    // performer picks one; `node.hdri_source` reads that as "nothing
    // decoded yet" and clears `out` to black (step 6 of its `run()`),
    // which env_mode=0 (Softbox, the default) never reaches anyway.
    string_bindings.push(StringBindingDef {
        id: HDRI_FILE_PARAM_ID.to_string(),
        label: "HDRI File".to_string(),
        default_value: String::new(),
        target: BindingTarget::Node {
            node_id: NodeId::new("hdri"),
            param: "path".to_string(),
        },
    });

    // The shared Ambient fill knob (its per-material bindings were fanned out
    // in the object loop above). 0.0 = no flat fill (lights-only); raise it to
    // lift the shadow side of every material at once.
    card_params.push(card_param("scene_ambient", "Ambient", 0.0, 1.0, 0.0, false, "Environment"));

    // No SSAO card sliders (Peter, 2026-07-15: "the defaults look good") —
    // the `ao` node group stays wired at its defaults
    // (`ssao_radius_default`/1.0 intensity, set on the ssao node above);
    // it's just no longer exposed on the outer card.

    // No Atmosphere section: fog + god rays removed with the atmosphere
    // node (Peter 2026-07-15) — see the removal comment in
    // `build_import_graph`.

    // Category "Geometry" matches the existing 3D-geometry generator
    // convention (Tesseract / DigitalPlants / NestedCubes / Duocylinder /
    // Wireframe) — closer to this preset's actual content than any entry
    // in `preset_type_registry::ALL_CATEGORIES`, which is an EFFECT-picker
    // bucket list (generators carry no category validation; see that
    // module's doc comment — "Text & Media" on `MriVolume`, a generator,
    // already isn't in that list either).
    let metadata = PresetMetadata {
        id: PresetTypeId::from_string(sanitized),
        display_name: stem.clone(),
        category: "Geometry".to_string(),
        osc_prefix,
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        params: card_params,
        bindings: card_bindings,
        skip_mode: SkipModeDef::default(),
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: vec![
            StringParamSpecDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                name: "Model File".to_string(),
                default_value: path_str,
                is_file_picker: true,
                use_dropdown: false,
            },
            StringParamSpecDef {
                id: HDRI_FILE_PARAM_ID.to_string(),
                name: "HDRI File".to_string(),
                default_value: String::new(),
                is_file_picker: true,
                use_dropdown: false,
            },
        ],
        string_bindings,
    };

    let def = EffectGraphDef {
        version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        nodes,
        wires,
    }
    .with_name(stem)
    .with_description(format!("Imported from {}", path.display()))
    .with_preset_metadata(metadata);

    // GLTF_ANIMATION_DESIGN.md A1: fold the parse-time animation findings
    // (non-LINEAR channels, morph-weight channels, multi-node-per-material
    // conflicts) into the same never-silent report doctrine as every
    // other D9 line above.
    report_lines.extend(summary.animation_report_lines.iter().cloned());

    let report = ImportReport {
        material_count: summary.materials.len(),
        object_count: n,
        textures_wired,
        default_material_vertex_count: summary.default_material_vertex_count,
        camera_synthesized: true,
        report_lines,
    };

    Ok((def, report))
}

/// Recursively find the largest node `id` anywhere in `nodes`, including
/// inside group bodies. Node ids only need to be unique WITHIN the level
/// (`Vec<EffectGraphNode>`) that holds them — `descend_level` looks a group
/// id up in its own sibling list, and the flattener assigns every node a
/// brand-new global id at load time (`manifold_core::flatten::flatten_groups`
/// — `clone.id = new_id`) — so a merge only strictly needs to avoid
/// colliding with the TOP-LEVEL ids `render_scene`'s siblings use. Walking
/// every nesting level anyway costs nothing and is the simplest thing that
/// is obviously correct for every level at once.
fn max_node_id_recursive(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|n| {
            let inner = n.group.as_ref().map(|g| max_node_id_recursive(&g.nodes)).unwrap_or(0);
            n.id.max(inner)
        })
        .max()
        .unwrap_or(0)
}

/// BUG-195's real fix: the largest KNOWN `source_bbox_radius` (BUG-194's
/// import-time provenance param, stamped on every `node.gltf_mesh_source`/
/// `node.gltf_skinned_mesh_source` this session's importer creates) among
/// every mesh-source node already in the target def, searched recursively
/// (mesh-source nodes live inside each object's group, same nesting
/// `max_node_id_recursive` walks). `-1.0` (the "unknown" sentinel — a
/// hand-built node the importer never touched) is excluded, never treated
/// as a real radius of zero. `None` when the def has no mesh-source node
/// with a known radius at all (a hand-built scene with no glTF import in
/// its history) — the caller falls back to the orbit-camera proxy.
fn max_known_source_bbox_radius(nodes: &[EffectGraphNode]) -> Option<f32> {
    nodes
        .iter()
        .filter_map(|n| {
            let own = matches!(
                n.type_id.as_str(),
                "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source"
            )
            .then(|| match n.params.get("source_bbox_radius") {
                Some(SerializedParamValue::Float { value }) if *value >= 0.0 => Some(*value),
                _ => None,
            })
            .flatten();
            let inner = n.group.as_ref().and_then(|g| max_known_source_bbox_radius(&g.nodes));
            match (own, inner) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            }
        })
        .fold(None, |acc, v| Some(acc.map_or(v, |a: f32| a.max(v))))
}

/// D5's merge plan: everything `ImportModelIntoSceneCommand`
/// (`manifold-editing`) needs to splice a SECOND (third, nth) glTF's object
/// groups into an EXISTING scene's `render_scene`, without touching that
/// scene's own chrome (camera/envmap/lights/lens — the target scene keeps
/// its own). Every field is a plain `manifold_core` type so the editing
/// crate — which cannot depend on `manifold-renderer` (see
/// `AddSceneObjectCommand`'s own doc comment for the same constraint) —
/// can consume it without a dependency-direction violation: the caller
/// (`manifold-app`, which depends on both) builds this plan here, then
/// hands its fields to `ImportModelIntoSceneCommand::new`.
#[derive(Debug, Clone)]
pub struct MergePlan {
    /// The target scene's `node.render_scene` node id — informational, so
    /// the command doesn't have to re-search `def` for it.
    pub render_scene_node_id: u32,
    /// New top-level nodes: one `GROUP_TYPE_ID` group per incoming
    /// material, same shape [`build_import_graph`] emits per object. NO
    /// camera, NO envmap, NO lights, NO lens — the target scene's chrome is
    /// never touched or duplicated.
    pub new_nodes: Vec<EffectGraphNode>,
    /// New top-level wires: each new group's single `object` output into
    /// `render_scene`'s `object_{k}` port, `k` continuing from the target's
    /// existing `objects` count.
    pub new_wires: Vec<EffectGraphWire>,
    /// `render_scene`'s new `objects` param value (existing + incoming).
    pub new_objects_count: u32,
    /// Card-spec additions (per-object knobs, sectioned by object/group
    /// name — same shape the importer's own outer card carries).
    pub new_card_params: Vec<ParamSpecDef>,
    pub new_card_bindings: Vec<BindingDef>,
    pub new_string_bindings: Vec<StringBindingDef>,
    /// D9 doctrine ("every import produces a report") applied to the merge:
    /// unmapped per-material features (same as [`ImportReport::report_lines`])
    /// plus, when the D5 scale-sanity rule fired, one line naming the
    /// normalize factor applied. Never a silent adjustment.
    pub report_lines: Vec<String>,
}

/// D5 — merge a second (third, nth) glTF's objects into `def`'s EXISTING
/// `node.render_scene`, reusing [`build_object_group`] (the SAME per-object
/// shape [`build_import_graph`] emits) for every incoming material. Never
/// calls [`assemble_import_graph`] / [`build_import_graph`] — this function
/// builds ONLY object groups, no chrome, so there is nothing to filter back
/// out and no chrome-duplication risk (the rejected "splice the whole
/// assembled def" alternative, D5).
///
/// New node ids are allocated above `def`'s current max id (see
/// [`max_node_id_recursive`]) — a merge twin of [`build_import_graph`]'s own
/// `fresh_id`. Group names that collide with an existing top-level handle
/// get suffixed by [`unique_group_name`] (the importer's own dedup helper,
/// reused verbatim) — `used_group_names` is seeded with every existing
/// top-level handle, not just names from this merge, so a merged object
/// can never silently share a namespace root with the scene's own chrome
/// or another object.
///
/// **Scale sanity (D5):** the incoming asset keeps its native units; each
/// new object's `transform_3d` is seeded with `pos = -center` (the
/// importer's own recenter convention). A uniform `scale` is ALSO seeded,
/// but only when the incoming bbox radius differs from a "scene reference
/// radius" by more than 10× in either direction. **BUG-195, fixed:** the
/// reference radius is [`max_known_source_bbox_radius`] — the largest
/// `source_bbox_radius` (BUG-194's import-time provenance param) among the
/// target def's own existing mesh-source nodes, a real per-object size fact
/// rather than an inversion of a camera value the user may have hand-retuned.
/// The old proxy — the target's own synthesized `node.orbit_camera`'s
/// `distance` param, inverted through the EXACT formula [`build_import_graph`]
/// used to seed it (`distance = 2.2 * radius`) — is kept as the fallback for
/// a scene with no known-radius mesh-source node at all (a hand-built scene
/// with no glTF import in its history). No top-level `node.orbit_camera`
/// either → normalization is skipped entirely (native units), never guessed.
fn merge_import_into_graph(
    def: &EffectGraphDef,
    summary: &GltfImportSummary,
    path: &Path,
) -> Result<MergePlan, String> {
    if summary.materials.is_empty() {
        return Err(format!(
            "{}: no materials with geometry — nothing to import",
            path.display()
        ));
    }

    let Some(render_scene_node) =
        def.nodes.iter().find(|n| n.type_id == super::scene_vm::RENDER_SCENE_TYPE_ID)
    else {
        return Err(
            "target scene graph has no top-level node.render_scene — cannot merge an import \
             into it"
                .to_string(),
        );
    };
    let render_scene_node_id = render_scene_node.id;
    let existing_objects: u32 = match render_scene_node.params.get("objects") {
        Some(SerializedParamValue::Float { value }) => value.round().max(0.0) as u32,
        Some(SerializedParamValue::Int { value }) => (*value).max(0) as u32,
        _ => 0,
    };

    let mut materials = summary.materials.clone();
    materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
    let incoming = materials.len();

    // GLB_CONFORMANCE_DESIGN.md D4 / OBJECT_SAFETY_MAX — enforced on the
    // POST-MERGE total (P4), same loud-error posture as the importer's own
    // over-bound reject. Never a silent partial merge.
    if incoming > OBJECT_SAFETY_MAX as usize {
        return Err(format!(
            "{}: {incoming} materials with geometry exceeds the {OBJECT_SAFETY_MAX}-object \
             safety bound on its own — this asset cannot be imported 1:1 without risking a \
             runaway port-list (raise OBJECT_SAFETY_MAX in render_scene.rs if a real asset \
             legitimately needs more; never silently truncate)",
            path.display(),
        ));
    }
    let post_merge_total = existing_objects as usize + incoming;
    if post_merge_total > OBJECT_SAFETY_MAX as usize {
        return Err(format!(
            "{}: merging {incoming} object(s) into a scene that already has {existing_objects} \
             would total {post_merge_total}, exceeding the {OBJECT_SAFETY_MAX}-object safety \
             bound — this merge cannot proceed without risking a runaway port-list on \
             render_scene (raise OBJECT_SAFETY_MAX in render_scene.rs if a real scene \
             legitimately needs more; never silently drop objects)",
            path.display(),
        ));
    }

    let center = [
        (summary.bbox_min[0] + summary.bbox_max[0]) * 0.5,
        (summary.bbox_min[1] + summary.bbox_max[1]) * 0.5,
        (summary.bbox_min[2] + summary.bbox_max[2]) * 0.5,
    ];
    let dims = [
        summary.bbox_max[0] - summary.bbox_min[0],
        summary.bbox_max[1] - summary.bbox_min[1],
        summary.bbox_max[2] - summary.bbox_min[2],
    ];
    let incoming_radius =
        ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(1e-3);

    // BUG-195 real fix: prefer a STORED per-object radius (BUG-194's
    // `source_bbox_radius` provenance param) over the orbit-camera proxy
    // below — the largest known radius among the target's own existing
    // mesh-source nodes is a real fact about the scene's geometry, not an
    // inversion of a camera framing value the user may have hand-retuned.
    // Only when no mesh-source node in the def has a known radius (e.g. a
    // scene built entirely by hand, never touched by this importer) does
    // the proxy apply — kept unchanged as the fallback, never removed.
    let scene_reference_radius: Option<f32> =
        max_known_source_bbox_radius(&def.nodes).filter(|r| *r > 1e-6).or_else(|| {
            def.nodes
                .iter()
                .find(|n| n.type_id == "node.orbit_camera")
                .and_then(|n| match n.params.get("distance") {
                    Some(SerializedParamValue::Float { value }) => Some(*value / 2.2),
                    _ => None,
                })
                .filter(|r| *r > 1e-6)
        });

    let normalize_scale: Option<f32> = scene_reference_radius.and_then(|ref_radius| {
        let ratio = incoming_radius / ref_radius;
        if (0.1..=10.0).contains(&ratio) { None } else { Some(ref_radius / incoming_radius) }
    });

    let mut next_id = max_node_id_recursive(&def.nodes) + 1;
    let mut fresh_id = move || {
        let v = next_id;
        next_id += 1;
        v
    };

    // Seeded with every existing top-level handle (not just group names) —
    // conservative, and cheap, so a merged object can never silently share
    // a namespace root with existing scene chrome or another object.
    let mut used_group_names: std::collections::HashSet<String> =
        def.nodes.iter().filter_map(|n| n.handle.clone()).collect();

    let node_anims_by_clip: Vec<std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>> =
        summary
            .animations
            .iter()
            .map(|a| a.nodes.iter().map(|n| (n.node_index, n.clone())).collect())
            .collect();

    let path_str = path.to_string_lossy().into_owned();

    let mut new_nodes = Vec::new();
    let mut new_wires = Vec::new();
    let mut new_card_params = Vec::new();
    let mut new_card_bindings = Vec::new();
    let mut new_string_bindings = Vec::new();
    let mut report_lines = Vec::new();

    for (local_k, m) in materials.iter().enumerate() {
        let port_index = existing_objects as usize + local_k;
        let mut out = build_object_group(
            local_k,
            port_index,
            render_scene_node_id,
            m,
            &path_str,
            center,
            &node_anims_by_clip,
            &mut used_group_names,
            &mut fresh_id,
            incoming_radius,
        );
        // D5 scale sanity: seeded on THIS object's own transform_3d — an
        // ordinary, visible, undoable value, never hidden state. Every
        // object in one incoming asset shares the same normalize factor
        // (it's the whole asset's scale, not a per-material one).
        //
        // Confessed shortcut: an object whose glTF animation ALSO drives
        // scale (`node.gltf_animation_source`'s `scale_x/y/z` wired as
        // port-shadows onto this SAME transform_3d, unconditionally, by
        // `build_object_group`) has this static seed overridden at runtime
        // by that wire — a port-shadow always wins over the static param
        // regardless of value. Normalizing a >10x-mismatched asset whose
        // objects are ALSO scale-animated is therefore a known gap, not
        // silently wrong: logged in BUG_BACKLOG (BUG-195 addendum) rather
        // than fixed here.
        if let Some(scale) = normalize_scale
            && let Some(transform_node) = out
                .group_node
                .group
                .as_mut()
                .and_then(|g| g.nodes.iter_mut().find(|n| n.type_id == "node.transform_3d"))
        {
            transform_node.params.insert("scale_x".to_string(), float(scale));
            transform_node.params.insert("scale_y".to_string(), float(scale));
            transform_node.params.insert("scale_z".to_string(), float(scale));
        }
        new_nodes.push(out.group_node);
        new_wires.append(&mut out.wires_to_render);
        new_card_params.append(&mut out.card_params);
        new_card_bindings.append(&mut out.card_bindings);
        new_string_bindings.append(&mut out.string_bindings);
        report_lines.append(&mut out.report_lines);
    }

    if let Some(scale) = normalize_scale {
        report_lines.push(format!(
            "merged import scaled ×{scale:.4} to match the scene (incoming radius \
             {incoming_radius:.4} vs scene reference {:.4})",
            scene_reference_radius.unwrap_or(0.0),
        ));
    }

    Ok(MergePlan {
        render_scene_node_id,
        new_nodes,
        new_wires,
        new_objects_count: (existing_objects as usize + incoming) as u32,
        new_card_params,
        new_card_bindings,
        new_string_bindings,
        report_lines,
    })
}

/// Public entry point for the "Import Model…" merge gesture
/// (`manifold-app`'s dispatch calls this — never [`merge_import_into_graph`]
/// directly, since that function takes a [`GltfImportSummary`], which is
/// `pub(crate)` to `manifold-renderer` and so cannot appear in a public
/// signature; the exact same constraint [`assemble_import_graph`] resolves
/// for [`build_import_graph`]). One CPU parse via
/// [`gltf_load::gltf_import_summary`], then the pure merge.
pub fn assemble_merge_plan(def: &EffectGraphDef, path: &Path) -> Result<MergePlan, String> {
    let summary = gltf_load::gltf_import_summary(path)?;
    merge_import_into_graph(def, &summary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;
    use crate::preset_runtime::PresetRuntime;
    use manifold_core::effect_graph_def::BindingTarget;

    fn azalea_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb")
    }

    /// Build a minimal, valid `.glb` with `n` distinct materials, each owning
    /// exactly one triangle (so every material has geometry and therefore
    /// counts toward `ImportReport::material_count`) — hand-rolled binary
    /// container (12-byte header + JSON chunk + BIN chunk, no external
    /// `.bin`/textures, no `uri` on the buffer so it resolves to the BIN
    /// chunk per spec §Binary glTF). GLB_CONFORMANCE_DESIGN.md G-P2: proves
    /// the FULL production parse path (`gltf::import` → `gltf_import_summary`
    /// → `build_import_graph`) imports every material 1:1, not just the
    /// graph-assembly half a synthetic [`GltfImportSummary`] would exercise.
    /// Written to the OS temp dir, not committed — a builder fn, not a
    /// binary asset (the phase brief's explicit call).
    fn write_synthetic_multimaterial_glb(n: usize) -> std::path::PathBuf {
        let mut accessors = Vec::with_capacity(n);
        let mut buffer_views = Vec::with_capacity(n);
        let mut materials = Vec::with_capacity(n);
        let mut primitives = Vec::with_capacity(n);
        let mut bin = Vec::with_capacity(n * 36);

        for i in 0..n {
            // One triangle per material, spread along X so no two overlap —
            // cosmetic, but keeps bbox/normal math non-degenerate.
            let ox = i as f32 * 2.0;
            let tri: [[f32; 3]; 3] = [[ox, 0.0, 0.0], [ox + 1.0, 0.0, 0.0], [ox, 1.0, 0.0]];
            for v in &tri {
                for c in v {
                    bin.extend_from_slice(&c.to_le_bytes());
                }
            }
            let byte_offset = i * 36;
            buffer_views.push(serde_json::json!({
                "buffer": 0,
                "byteOffset": byte_offset,
                "byteLength": 36,
            }));
            accessors.push(serde_json::json!({
                "bufferView": i,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [ox, 0.0, 0.0],
                "max": [ox + 1.0, 1.0, 0.0],
            }));
            materials.push(serde_json::json!({
                "name": format!("Mat{i}"),
                "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] },
            }));
            // Mode omitted — glTF's default primitive mode is 4 (TRIANGLES).
            primitives.push(serde_json::json!({
                "attributes": { "POSITION": i },
                "material": i,
            }));
        }

        let doc = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            "meshes": [{ "primitives": primitives }],
            "accessors": accessors,
            "bufferViews": buffer_views,
            "materials": materials,
            "buffers": [{ "byteLength": bin.len() }],
        });
        let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");

        // GLB container: header + JSON chunk (space-padded to 4 bytes) + BIN
        // chunk (zero-padded to 4 bytes). Chunk type magics per the Binary
        // glTF spec: 0x4E4F534A = "JSON", 0x004E4942 = "BIN\0".
        let mut json_padded = json_bytes;
        while !json_padded.len().is_multiple_of(4) {
            json_padded.push(b' ');
        }
        let mut bin_padded = bin;
        while !bin_padded.len().is_multiple_of(4) {
            bin_padded.push(0);
        }
        let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

        let mut glb = Vec::with_capacity(total_len);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total_len as u32).to_le_bytes());
        glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_padded);
        glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin_padded);

        let path = std::env::temp_dir().join(format!(
            "manifold_synthetic_{n}mat_{}_{}.glb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
        path
    }

    /// GLB_CONFORMANCE_DESIGN.md D4 / G-P2's named gate test: a 100-material
    /// synthetic asset — well past the old (dead) 64-object cap, well under
    /// `OBJECT_SAFETY_MAX` (1024) — imports every single material 1:1, no
    /// truncation. Also proves the card-curation split: only the largest
    /// `CARD_CURATION_MAX` (16) objects would get a per-object slider (none
    /// of these materials are glass, so no Opacity sliders exist either way
    /// — the object/wire count is what this test pins).
    #[test]
    fn over_cap_asset_imports_one_to_one() {
        let path = write_synthetic_multimaterial_glb(100);
        let (def, report) = assemble_import_graph(&path).expect("assemble 100-material synthetic glb");
        std::fs::remove_file(&path).ok();

        assert_eq!(report.material_count, 100, "all 100 materials have geometry");
        assert_eq!(
            report.object_count, 100,
            "import is 1:1 — object_count must equal material_count, no truncation (D4)"
        );

        // Every material got its own render_scene wire — object_0..object_99
        // all present, nothing dropped past the old 64-object boundary
        // (SCENE_OBJECT_AND_PANEL_V2_DESIGN D4: render_scene's per-object
        // surface is `object_{i}` only, post-P2).
        for k in 0..100 {
            assert!(
                def.wires.iter().any(|w| w.to_port == format!("object_{k}")),
                "material {k} (past the old 64-object cap) must still wire object_{k}"
            );
        }
        // render_scene's own `objects` param must reflect the true count,
        // not a clamped one.
        let render_node = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .expect("assembled graph has a render_scene node");
        assert_eq!(
            render_node.params.get("objects"),
            Some(&int(100)),
            "render_scene.objects must be the true unclamped count"
        );

        // Structural gate: the assembled graph — 100 objects, well past the
        // dead 64-object UI cap — must still compile through the real
        // registry (catches a bad port/wire before any GPU proof).
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None)
            .expect("100-object import graph must compile through PresetRuntime::from_def");
    }

    /// D4's other half: a glb whose material count exceeds `OBJECT_SAFETY_MAX`
    /// (1024) must error loudly at import time — never silently truncate.
    /// `object_cap_exceeded_glb_errors_loudly_never_truncates` deliberately
    /// builds only ONE material past the limit (1025) rather than a much
    /// larger number — the synthetic-glb builder is O(n) JSON, and this test
    /// only needs to cross the boundary, not stress it.
    #[test]
    fn object_cap_exceeded_glb_errors_loudly_never_truncates() {
        let n = OBJECT_SAFETY_MAX as usize + 1;
        let path = write_synthetic_multimaterial_glb(n);
        let result = assemble_import_graph(&path);
        std::fs::remove_file(&path).ok();

        let err = result.expect_err("a glb past OBJECT_SAFETY_MAX must error, not truncate");
        assert!(
            err.contains(&n.to_string()) && err.contains(&OBJECT_SAFETY_MAX.to_string()),
            "error must name both the actual count and the safety bound, got: {err}"
        );
    }

    /// Build a minimal, valid `.glb` with ONE triangle primitive that has NO
    /// `material` key at all — glTF's implicit default material
    /// (GLB_XFAIL_BURNDOWN_DESIGN.md D4, BUG-171). No `materials` array in
    /// the document at all, matching a real asset like `BoxVertexColors.glb`
    /// that carries geometry but declares zero materials. Same hand-rolled
    /// binary-container shape as `write_synthetic_multimaterial_glb`.
    fn write_synthetic_default_material_glb() -> std::path::PathBuf {
        let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let mut bin = Vec::with_capacity(36);
        for v in &tri {
            for c in v {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }

        let doc = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            // No "material" key on the primitive — glTF's implicit default
            // material. No "materials" array at all, matching a real asset
            // that declares zero materials.
            "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
            "accessors": [{
                "bufferView": 0,
                "componentType": 5126,
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            }],
            "bufferViews": [{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }],
            "buffers": [{ "byteLength": bin.len() }],
        });
        let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");

        let mut json_padded = json_bytes;
        while !json_padded.len().is_multiple_of(4) {
            json_padded.push(b' ');
        }
        let mut bin_padded = bin;
        while !bin_padded.len().is_multiple_of(4) {
            bin_padded.push(0);
        }
        let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

        let mut glb = Vec::with_capacity(total_len);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total_len as u32).to_le_bytes());
        glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_padded);
        glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin_padded);

        let path = std::env::temp_dir().join(format!(
            "manifold_synthetic_defaultmat_{}_{}.glb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
        path
    }

    /// GLB_XFAIL_BURNDOWN_DESIGN.md D4 (BUG-171) / §4 invariant ("no silent
    /// geometry drop"): a hand-rolled glb whose ONLY primitive has no
    /// material must import as exactly ONE object — the synthetic
    /// default-material entry — through the FULL production parse path
    /// (`gltf_import_summary` → `assemble_import_graph`), not just the
    /// graph-assembly half a synthetic `GltfImportSummary` would exercise.
    /// Before D4 this asset errored "no materials with geometry — nothing
    /// to import" (the geometry was silently uncounted).
    #[test]
    fn default_material_primitive_imports_as_one_object() {
        let path = write_synthetic_default_material_glb();
        let (def, report) = assemble_import_graph(&path).expect(
            "a materialless-primitive glb must import via the D4 synthetic default material",
        );
        std::fs::remove_file(&path).ok();

        assert_eq!(
            report.object_count, 1,
            "one materialless primitive must yield exactly one render_scene object (D4)"
        );
        assert_eq!(report.default_material_vertex_count, 3, "the one triangle's 3 vertices");

        // The synthetic object's mesh source must carry the D4 sentinel
        // param, not a real (or the colliding "unset") material_index.
        // gltf_mesh_source lives inside the object's group box until load
        // time flattening — same pattern every other nested-node assertion
        // in this test module uses.
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten import graph");
        let mesh_node = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.gltf_mesh_source")
            .expect("flattened graph has a gltf_mesh_source node");
        assert_eq!(
            mesh_node.params.get("material_index"),
            Some(&int(super::gltf_load::DEFAULT_MATERIAL_MESH_PARAM)),
            "the synthetic object's mesh source must select via the D4 sentinel, not a real \
             material index or the -1 'unset' value"
        );

        // Structural gate: compiles through the real registry.
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None)
            .expect("materialless-primitive import graph must compile through PresetRuntime::from_def");
    }

    /// GLB_CONFORMANCE_DESIGN.md D4's card-curation half, plus the standard
    /// §5 round-trip rule (imports serialize — same pattern as
    /// `round_trip_preserves_map_wires_and_sun_coherence_bindings`): 20
    /// glass objects, largest-vertex-count-first — the first `CARD_CURATION_MAX`
    /// (16) get an Opacity card slider, the remaining 4 don't, but ALL 20
    /// still get full graph wiring. Every bit of that — object count, the
    /// curated/uncurated split, and the wiring — must survive a save/reload
    /// (JSON round trip of `EffectGraphDef`, the actual persisted artifact
    /// for an imported generator layer).
    #[test]
    fn card_curation_caps_at_16_but_wiring_and_round_trip_stay_1_to_1() {
        let n = 20;
        let materials: Vec<_> = (0..n)
            .map(|k| {
                let mut m = full_material(k as u32, &format!("Glass{k}"), (n - k) as u32 * 100);
                // All glass, so every object is a curation candidate — makes
                // the 16/4 split unambiguous.
                m.was_blend = true;
                m.transmission_factor = 0.5;
                m
            })
            .collect();
        let summary = GltfImportSummary {
            materials,
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_curation_round_trip.glb");
        let (def, report) = build_import_graph(&summary, path).expect("build 20-object graph");
        assert_eq!(report.object_count, 20, "1:1 — every glass object gets full wiring");

        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        for (def, label) in [(&def, "pre-reload"), (&reloaded, "post-reload")] {
            let meta = def.preset_metadata.as_ref().unwrap_or_else(|| panic!("{label}: v2 metadata"));
            let opacity_count = meta.params.iter().filter(|p| p.name == "Opacity").count();
            assert_eq!(
                opacity_count, CARD_CURATION_MAX,
                "{label}: exactly the largest {CARD_CURATION_MAX} objects get an Opacity slider"
            );
            for k in 0..CARD_CURATION_MAX {
                assert!(
                    meta.params.iter().any(|p| p.id == format!("opacity_{k}")),
                    "{label}: object {k} (top {CARD_CURATION_MAX}) must have a card slider"
                );
            }
            for k in CARD_CURATION_MAX..n {
                assert!(
                    !meta.params.iter().any(|p| p.id == format!("opacity_{k}")),
                    "{label}: object {k} (past the curation cap) must NOT have a card slider"
                );
            }

            // Curation is UI-only — full graph wiring survives for every
            // object, curated or not (the forbidden-move: "per-object slider
            // explosion" never becomes "per-object geometry drop").
            let flat = manifold_core::flatten::flatten_groups(def)
                .unwrap_or_else(|e| panic!("{label}: flatten failed: {e}"));
            let render = flat.nodes.iter().find(|n| n.type_id == "node.render_scene").unwrap();
            for k in 0..n {
                assert!(
                    flat.wires.iter().any(|w| w.to_node == render.id && w.to_port == format!("object_{k}")),
                    "{label}: object {k} must still wire object_{k} regardless of card curation"
                );
            }
        }

        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(reloaded, &registry, None)
            .expect("reloaded 20-object import graph must build through PresetRuntime::from_def");
    }

    /// CPU-only, fast — not gated behind `#[ignore]`. Guards the
    /// file-missing case (no fixture in a checkout without
    /// `tests/fixtures/gltf/`) so CI without the large fixture still
    /// passes; when the fixture IS present, asserts the assembled graph's
    /// known azalea shape.
    #[test]
    fn assembles_azalea_into_two_object_render_scene_graph() {
        let path = azalea_fixture_path();
        if !path.exists() {
            println!(
                "assembles_azalea_into_two_object_render_scene_graph: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble azalea");
        println!("azalea import report: {report:?}");

        assert_eq!(report.material_count, 2, "azalea has 2 materials with geometry");
        assert_eq!(report.object_count, 2);
        assert_eq!(report.textures_wired, 2, "both azalea materials carry a base-color texture");
        assert_eq!(report.default_material_vertex_count, 0);
        assert!(report.camera_synthesized);

        assert!(
            def.nodes.iter().any(|n| n.type_id == GENERATOR_INPUT_TYPE_ID),
            "assembled graph must carry a system.generator_input node"
        );
        assert!(
            def.nodes.iter().any(|n| n.type_id == FINAL_OUTPUT_TYPE_ID),
            "assembled graph must carry a system.final_output node"
        );

        let meta = def.preset_metadata.as_ref().expect("v2 metadata");
        // GLB_CONFORMANCE_DESIGN.md D6: a second string param, `hdri_file`,
        // holds the HDRI environment's own Browse field — a separate file
        // from `model_file` (the imported .glb itself).
        assert_eq!(meta.string_params.len(), 2, "model_file + hdri_file string params");
        assert_eq!(meta.string_params[0].id, "model_file");
        assert!(meta.string_params[0].is_file_picker);
        assert_eq!(meta.string_params[1].id, "hdri_file");
        assert!(meta.string_params[1].is_file_picker);

        assert_eq!(
            meta.string_bindings.len(),
            5,
            "2 mesh + 2 texture path bindings (model_file) + 1 HDRI path binding (hdri_file)"
        );
        for b in &meta.string_bindings {
            assert!(b.id == "model_file" || b.id == "hdri_file", "unexpected string binding id {}", b.id);
            match &b.target {
                BindingTarget::Node { param, .. } => assert_eq!(param, "path"),
                other => panic!("expected a Node binding target, got {other:?}"),
            }
        }
        assert_eq!(
            meta.string_bindings.iter().filter(|b| b.id == "hdri_file").count(),
            1,
            "exactly one hdri_file binding, targeting the hdri node"
        );

        // Curated performance surface. Azalea has 2 objects → 4 camera + 5 sun
        // + 1 Environment + 1 Environment Mode (D6) + 1 Fill Light + 1 Strip
        // Lights (F-P7) + 1 Ambient = 14 framing/material sliders. No
        // Atmosphere section (fog + god rays removed with the atmosphere
        // node, Peter 2026-07-15), no Motion Blur (BUG-136), no per-object
        // Metallic/Roughness and no SSAO/DoF card sliders (Peter,
        // 2026-07-15: DoF removed for buggy visuals, AO/metallic/roughness
        // hidden — defaults still apply, just not on the card).
        assert_eq!(meta.params.len(), 14, "14 framing/material sliders");
        // Every param routes one-to-one except: the shared Ambient, which
        // fans out to every material's ambient (2 for azalea); D7's sun
        // coherence, where each of sun_x/sun_y/sun_z fans out to TWO targets
        // (the sun light AND the envmap's disc direction) — 3 extra
        // bindings; and G-P6's Environment master, which fans out to the
        // softbox bake's intensity AND the HDRI branch's exposure gain — 1
        // extra. 14 + 1 (ambient) + 3 (sun coherence) + 1 (env fan-out) = 19.
        assert_eq!(
            meta.bindings.len(),
            19,
            "14 params, Ambient fanned to 2 materials, sun_x/y/z each fanned to 2 targets, env_intensity fanned to bake + hdri gain"
        );
        // Every card param routes to at least one node param.
        for p in &meta.params {
            assert!(
                meta.bindings.iter().any(|b| b.id == p.id),
                "card param `{}` has no binding",
                p.id
            );
        }
        // Every binding must reference a param that actually exists (address
        // by id, never by position — the fan-out rule).
        for b in &meta.bindings {
            assert!(
                meta.params.iter().any(|p| p.id == b.id),
                "binding `{}` has no matching param",
                b.id
            );
            assert!(
                matches!(b.target, BindingTarget::Node { .. }),
                "import card bindings route to inner nodes"
            );
        }
        // Spot-check the shared framing knobs.
        for id in ["cam_orbit", "cam_dist", "sun_int", "env_intensity", "scene_ambient"] {
            assert!(meta.params.iter().any(|p| p.id == id), "missing card param `{id}`");
        }
        // Shared framing/light/environment knobs carry the fixed section names.
        let cam_orbit = meta.params.iter().find(|p| p.id == "cam_orbit").unwrap();
        assert_eq!(cam_orbit.section.as_deref(), Some("Camera"));
        let sun_int = meta.params.iter().find(|p| p.id == "sun_int").unwrap();
        assert_eq!(sun_int.section.as_deref(), Some("Sun"));
        let env_intensity = meta.params.iter().find(|p| p.id == "env_intensity").unwrap();
        assert_eq!(env_intensity.section.as_deref(), Some("Environment"));
        // D7 (F-P4): the black-void softbox studio is now the import
        // default, so Environment starts at 1.0 (range unchanged); the
        // shared Ambient fill still starts at 0 — softbox lighting comes
        // from the envmap + sun, not a flat fill floor.
        assert_eq!(env_intensity.default_value, 1.0, "environment bakes at softbox intensity 1.0 by default (D7)");
        let scene_ambient = meta.params.iter().find(|p| p.id == "scene_ambient").unwrap();
        assert_eq!(scene_ambient.default_value, 0.0, "no ambient fill by default");
        // The Ambient card fans out to every material's `ambient` param.
        let ambient_targets: std::collections::HashSet<String> = meta
            .bindings
            .iter()
            .filter(|b| b.id == "scene_ambient")
            .filter_map(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    assert_eq!(param, "ambient");
                    Some(node_id.as_str().to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            ambient_targets,
            ["mat_0", "mat_1"].into_iter().map(String::from).collect(),
            "shared Ambient drives both azalea materials"
        );
        // The environment master routes to the envmap's new `intensity` param
        // (not the old horizon_strength).
        let env_binding = meta.bindings.iter().find(|b| b.id == "env_intensity").unwrap();
        match &env_binding.target {
            BindingTarget::Node { node_id, param } => {
                assert_eq!(node_id.as_str(), "envmap");
                assert_eq!(param, "intensity");
            }
            other => panic!("expected envmap node target, got {other:?}"),
        }

        // Camera angle sliders store radians (app-wide `is_angle` convention),
        // so the binding is a pass-through — no unit fold at the write boundary.
        assert!(cam_orbit.is_angle, "camera orbit slider is an angle param");
        assert!(
            (cam_orbit.default_value - 0.7).abs() < 1e-6,
            "camera orbit default is stored in radians"
        );
        let orbit = meta.bindings.iter().find(|b| b.id == "cam_orbit").unwrap();
        assert!(
            (orbit.scale - 1.0).abs() < 1e-6,
            "camera angle bindings pass radians straight through"
        );
        // Orbit and tilt both wrap a full 360° instead of clamping at their
        // edges (Peter, 2026-07-15).
        assert!(cam_orbit.wraps, "camera orbit must wrap 360°");
        let cam_tilt = meta.params.iter().find(|p| p.id == "cam_tilt").unwrap();
        assert!(cam_tilt.wraps, "camera tilt must wrap 360°");
        assert!(
            (cam_tilt.min - (-std::f32::consts::PI)).abs() < 1e-4
                && (cam_tilt.max - std::f32::consts::PI).abs() < 1e-4,
            "camera tilt spans the full ±180° range to match orbit's wrap"
        );

        // GTAO and the lens are wired into the spine. No motion blur
        // (BUG-136 + fusion cost, see the removal comment in
        // `build_import_graph`), no depth-of-field group (Peter, 2026-07-15:
        // buggy visuals), no atmosphere node (fog + god rays removed,
        // Peter 2026-07-15).
        for present in ["node.ssao_gtao", "node.bilateral_blur", "node.camera_lens"] {
            assert!(
                def.nodes.iter().any(|n| n.type_id == present)
                    || def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                        g.nodes.iter().any(|inner| inner.type_id == present)
                    }),
                "imported graph must carry `{present}`"
            );
        }
        // `node.motion_blur` was removed (BUG-136: no visible effect live,
        // despite correct wiring — see the removal comment above);
        // `node.variable_blur` was P4's superseded DoF blur stage; and the
        // DoF chain itself (`coc_from_depth`/`coc_dilate`/`bokeh_gather`)
        // was removed 2026-07-15 for buggy visuals. None is reintroduced.
        for absent in [
            "node.motion_blur",
            "node.variable_blur",
            "node.coc_from_depth",
            "node.coc_dilate",
            "node.bokeh_gather",
            "node.atmosphere",
        ] {
            assert!(
                !def.nodes.iter().any(|n| n.type_id == absent)
                    && !def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                        g.nodes.iter().any(|inner| inner.type_id == absent)
                    }),
                "`{absent}` should not be in the imported graph"
            );
        }
        // No SSAO/metallic/roughness/DoF card sliders — the underlying nodes
        // keep their defaults, they're just no longer exposed on the card.
        for gone_prefix in ["ssao_", "metal_", "rough_", "dof_"] {
            assert!(
                !meta.params.iter().any(|p| p.id.starts_with(gone_prefix)),
                "no card param should start with `{gone_prefix}`"
            );
        }
        for gone in [
            "lens_focus", "lens_fstop", "lens_shutter", "lens_ev", "dof_radius",
            "motion_blur_px", "mb_shutter", "ssao_bias", "fog_density", "god_rays",
        ] {
            assert!(
                !meta.params.iter().any(|p| p.id == gone),
                "unused card param id `{gone}` should not exist"
            );
        }
        // Shadow type defaults to Hard (enum 0) for the crisp dramatic look.
        let sun_shadow = meta.params.iter().find(|p| p.id == "sun_shadow").unwrap();
        assert_eq!(sun_shadow.default_value, 0.0, "shadow type defaults to Hard");
    }

    /// Structural gate (fast, no GPU): the assembled azalea graph must
    /// compile through the real `PrimitiveRegistry` — every node type_id
    /// resolves, every wire's ports exist and type-check, both boundary
    /// nodes are present and wired correctly. Catches a bad port/param
    /// name or a missing wire before the (slow, `#[ignore]`d) GPU proof.
    #[test]
    fn assembled_azalea_graph_passes_from_def_structural_check() {
        let path = azalea_fixture_path();
        if !path.exists() {
            println!(
                "assembled_azalea_graph_passes_from_def_structural_check: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, _report) = assemble_import_graph(&path).expect("assemble azalea");
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None)
            .expect("assembled azalea graph must compile through PresetRuntime::from_def");
    }

    /// Grouping proof, fixture-free: each object's producers must live inside one
    /// named, tinted group, and the grouped graph must flatten to the SAME flat
    /// wiring the ungrouped assembler produced (compared in node_id space) with
    /// every card/string binding still resolving. Uses a synthetic two-material
    /// summary — one textured, one not — so it needs no `.glb` on disk.
    #[test]
    fn build_import_graph_groups_each_object_and_flattens_to_flat_wiring() {
        use super::gltf_load::GltfMaterialInfo;
        use manifold_core::effect_graph_def::GROUP_TYPE_ID;
        use manifold_core::flatten::flatten_groups;

        let mat = |material_index: u32, name: &str, verts: u32, tex: Option<u32>| GltfMaterialInfo {
            material_index,
            name: Some(name.to_string()),
            base_color_factor: [0.5, 0.5, 0.5, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            emissive: [0.0, 0.0, 0.0],
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: tex,
            normal_texture: None,
            normal_scale: 1.0,
            mr_texture: None,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive_texture: None,
            emissive_strength: 1.0,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0, 1.0, 1.0],
            specular_texture: None,
            specular_color_texture: None,
            base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            uv_tex_coord_override: false,
            mr_texture_is_gloss_alpha: false,
            transmission_factor: 0.0,
            transmission_texture: None,
            clearcoat_factor: 0.0,
            clearcoat_roughness_factor: 0.0,
            clearcoat_texture: None,
            clearcoat_roughness_texture: None,
            clearcoat_normal_texture: None,
            sheen_color_factor: [0.0, 0.0, 0.0],
            sheen_roughness_factor: 0.0,
            sheen_color_texture: None,
            sheen_roughness_texture: None,
            iridescence_factor: 0.0,
            iridescence_ior: 1.3,
            iridescence_thickness_minimum: 100.0,
            iridescence_thickness_maximum: 400.0,
            iridescence_texture: None,
            iridescence_thickness_texture: None,
            anisotropy_strength: 0.0,
            anisotropy_rotation: 0.0,
            anisotropy_texture: None,
            dispersion: 0.0,
            volume_thickness_factor: 0.0,
            volume_attenuation_distance: super::gltf_load::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
            volume_attenuation_color: [1.0, 1.0, 1.0],
            volume_thickness_texture: None,
            was_blend: false,
            vertex_count: verts,
            base_color_sampler: super::gltf_load::GltfSamplerInfo::default(),
            normal_sampler: super::gltf_load::GltfSamplerInfo::default(),
            mr_sampler: super::gltf_load::GltfSamplerInfo::default(),
            occlusion_sampler: super::gltf_load::GltfSamplerInfo::default(),
            emissive_sampler: super::gltf_load::GltfSamplerInfo::default(),
            animations: Vec::new(),
            skin: None,
            morph: None,
            own_center: [0.0, 0.0, 0.0],
        };
        let summary = GltfImportSummary {
            // Largest-vertex-first sort makes object 0 = Leaf (textured), 1 = Bark.
            materials: vec![mat(0, "Leaf", 1200, Some(0)), mat(1, "Bark", 800, None)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_model.glb");
        let (def, report) = build_import_graph(&summary, path).expect("build grouped graph");
        assert_eq!(report.object_count, 2);
        assert_eq!(report.textures_wired, 1);

        // Top level: two per-object group boxes PLUS the "ao" presentation
        // group (CINEMATIC_POST; "dof" removed 2026-07-15), no bare producer
        // nodes.
        let groups: Vec<_> = def.nodes.iter().filter(|n| n.type_id == GROUP_TYPE_ID).collect();
        assert_eq!(groups.len(), 3, "2 object groups + ao");
        assert!(groups.iter().all(|g| g.group.is_some()));
        for bare in [
            "node.gltf_mesh_source",
            "node.pbr_material",
            "node.gltf_texture_source",
            "node.transform_3d",
        ] {
            assert!(
                !def.nodes.iter().any(|n| n.type_id == bare),
                "producer `{bare}` must live inside a group, not at the top level"
            );
        }
        // Only the per-object groups carry a tint (CINEMATIC_POST's ao
        // group is an untinted presentation box, not per-object identity).
        let object_groups: Vec<_> = groups
            .iter()
            .filter(|g| g.group.as_ref().unwrap().tint.is_some())
            .copied()
            .collect();
        assert_eq!(object_groups.len(), 2, "one tinted group per object");
        // Every object group's interface declares a single `object` output
        // (SCENE_OBJECT_AND_PANEL_V2_DESIGN D1/D3 — the transform/material/
        // mesh triplet is bound INSIDE the group by `node.scene_object` now,
        // not exposed as separate interface ports).
        for g in &object_groups {
            let outputs = &g.group.as_ref().unwrap().interface.outputs;
            assert!(
                outputs.iter().any(|o| o.name == "object" && o.port_type == "Object"),
                "every object group exposes a single object output"
            );
        }
        // Distinct tints per object group (legibility).
        let tints: Vec<_> = object_groups.iter().filter_map(|g| g.group.as_ref().unwrap().tint).collect();
        assert_eq!(tints.len(), 2, "every object group gets a tint");
        assert_ne!(tints[0], tints[1], "each object group gets its own tint");

        // Flatten and prove the runtime sees the same flat wiring the ungrouped
        // assembler produced — in node_id space (survives id renumbering + handle
        // prefixing).
        let flat = flatten_groups(&def).expect("grouped import graph flattens");
        let id_of = |doc_id: u32| -> String {
            flat.nodes
                .iter()
                .find(|n| n.id == doc_id)
                .map(|n| n.node_id.as_str().to_string())
                .unwrap_or_default()
        };
        let conn: std::collections::HashSet<(String, String, String, String)> = flat
            .wires
            .iter()
            .map(|w| (id_of(w.from_node), w.from_port.clone(), id_of(w.to_node), w.to_port.clone()))
            .collect();
        // Internal wires into each object's `node.scene_object` bind node —
        // survive flattening in-scope (SCENE_OBJECT_AND_PANEL_V2_DESIGN
        // D1/D3: the mesh/material/transform/map triplet binds to
        // scene_object now, not directly to render_scene).
        for (from_id, from_port, to_id, to_port) in [
            ("mesh_0", "vertices", "object_0_bind", "vertices"),
            ("mat_0", "out", "object_0_bind", "material"),
            ("tex_0", "out", "object_0_bind", "base_color_map"),
            ("mesh_1", "vertices", "object_1_bind", "vertices"),
            ("mat_1", "out", "object_1_bind", "material"),
            ("transform_0", "transform", "object_0_bind", "transform"),
            ("transform_1", "transform", "object_1_bind", "transform"),
        ] {
            assert!(
                conn.contains(&(
                    from_id.to_string(),
                    from_port.to_string(),
                    to_id.to_string(),
                    to_port.to_string(),
                )),
                "flattened graph missing wire {from_id}.{from_port} -> {to_id}.{to_port}"
            );
        }
        // Each object's single `object` output reaches render_scene's
        // `object_{k}` port (D4 — render_scene's v2 per-object surface).
        for (from_id, to_port) in [("object_0_bind", "object_0"), ("object_1_bind", "object_1")] {
            assert!(
                conn.contains(&(
                    from_id.to_string(),
                    "object".to_string(),
                    "render".to_string(),
                    to_port.to_string(),
                )),
                "flattened graph missing wire {from_id}.object -> render.{to_port}"
            );
        }
        // Bark has no texture — no base_color_map wire into its scene_object.
        assert!(
            !conn.iter().any(|(_, _, to, tp)| to == "object_1_bind" && tp == "base_color_map"),
            "untextured object must not wire a base-color map"
        );
        // No group / boundary nodes survive flattening.
        assert!(
            !flat.nodes.iter().any(|n| {
                n.type_id == GROUP_TYPE_ID || n.type_id.contains("group_output") || n.type_id.contains("group_input")
            }),
            "flattened graph must contain no group or boundary nodes"
        );

        // Every card + string binding still targets a node_id that exists post-flatten.
        let meta = def.preset_metadata.as_ref().expect("v2 metadata");
        let flat_ids: std::collections::HashSet<&str> =
            flat.nodes.iter().map(|n| n.node_id.as_str()).collect();
        for b in &meta.bindings {
            if let BindingTarget::Node { node_id, .. } = &b.target {
                assert!(
                    flat_ids.contains(node_id.as_str()),
                    "card binding `{}` targets `{}`, gone after flatten",
                    b.id,
                    node_id.as_str()
                );
            }
        }
        for b in &meta.string_bindings {
            if let BindingTarget::Node { node_id, .. } = &b.target {
                assert!(
                    flat_ids.contains(node_id.as_str()),
                    "string binding targets `{}`, gone after flatten",
                    node_id.as_str()
                );
            }
        }

        // The editor's own data path (`GraphSnapshot::from_def`, which routes a
        // grouped def through the group-preserving structural snapshot) must show
        // all four groups as navigable boxes — each carrying its inner producers —
        // not a flat wall of nodes. This is the legibility payoff, verified at the
        // snapshot layer (the pixels still want Peter's eyes on a real model).
        let snap = crate::node_graph::GraphSnapshot::from_def(&def)
            .expect("editor snapshot builds from the grouped def");
        let snap_groups: Vec<_> =
            snap.nodes.iter().filter(|n| n.group.is_some()).collect();
        assert_eq!(snap_groups.len(), 3, "editor snapshot shows 2 object + ao group boxes");
        let snap_object_groups: Vec<_> = snap_groups
            .iter()
            .filter(|g| g.group.as_ref().unwrap().nodes.iter().any(|inner| inner.type_id == "node.pbr_material"))
            .collect();
        assert_eq!(snap_object_groups.len(), 2, "exactly the two object groups carry a material node");

        // Finally, it must build through the production loader (which flattens).
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None)
            .expect("grouped import graph must build through PresetRuntime::from_def");
    }

    /// A synthetic [`GltfMaterialInfo`] carrying every texture kind F-P4
    /// wires, with independent test-controlled fields for the three
    /// report-only features (clearcoat/transmission/BLEND). Defaults mirror
    /// a "fully-mapped, nothing extra" material — callers override only
    /// what a specific test cares about (Rust has no field-update syntax
    /// across `..` for `pub(crate)` structs outside the defining module, so
    /// this is a plain builder-by-closure, not `..Default::default()`).
    fn full_material(material_index: u32, name: &str, verts: u32) -> super::gltf_load::GltfMaterialInfo {
        use super::gltf_load::GltfMaterialInfo;
        GltfMaterialInfo {
            material_index,
            name: Some(name.to_string()),
            base_color_factor: [0.8, 0.8, 0.8, 1.0],
            metallic: 1.0,
            roughness: 0.4,
            emissive: [1.0, 0.5, 0.2],
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: Some(0),
            normal_texture: Some(1),
            normal_scale: 1.0,
            mr_texture: Some(2),
            occlusion_texture: Some(3),
            occlusion_strength: 1.0,
            emissive_texture: Some(4),
            emissive_strength: 2.5,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0, 1.0, 1.0],
            specular_texture: None,
            specular_color_texture: None,
            base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            uv_tex_coord_override: false,
            mr_texture_is_gloss_alpha: false,
            transmission_factor: 0.0,
            transmission_texture: None,
            clearcoat_factor: 0.0,
            clearcoat_roughness_factor: 0.0,
            clearcoat_texture: None,
            clearcoat_roughness_texture: None,
            clearcoat_normal_texture: None,
            sheen_color_factor: [0.0, 0.0, 0.0],
            sheen_roughness_factor: 0.0,
            sheen_color_texture: None,
            sheen_roughness_texture: None,
            iridescence_factor: 0.0,
            iridescence_ior: 1.3,
            iridescence_thickness_minimum: 100.0,
            iridescence_thickness_maximum: 400.0,
            iridescence_texture: None,
            iridescence_thickness_texture: None,
            anisotropy_strength: 0.0,
            anisotropy_rotation: 0.0,
            anisotropy_texture: None,
            dispersion: 0.0,
            volume_thickness_factor: 0.0,
            volume_attenuation_distance: super::gltf_load::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
            volume_attenuation_color: [1.0, 1.0, 1.0],
            volume_thickness_texture: None,
            was_blend: false,
            vertex_count: verts,
            base_color_sampler: super::gltf_load::GltfSamplerInfo::default(),
            normal_sampler: super::gltf_load::GltfSamplerInfo::default(),
            mr_sampler: super::gltf_load::GltfSamplerInfo::default(),
            occlusion_sampler: super::gltf_load::GltfSamplerInfo::default(),
            emissive_sampler: super::gltf_load::GltfSamplerInfo::default(),
            animations: Vec::new(),
            skin: None,
            morph: None,
            own_center: [0.0, 0.0, 0.0],
        }
    }

    /// BUG-194/BUG-195: `build_import_graph` stamps `source_vertex_count`
    /// (exactly `GltfMaterialInfo::vertex_count`) and `source_bbox_radius`
    /// (the whole-import bbox radius, the same value the synthesized
    /// orbit camera's `distance` is derived from) onto every mesh-source
    /// node it creates — read back by `SceneVm`'s header and by a future
    /// merge's scale-sanity rule.
    #[test]
    fn build_import_graph_seeds_source_vertex_count_and_bbox_radius() {
        let half_extent = 3.0_f32;
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "Leaf", 250), full_material(1, "Bark", 900)],
            bbox_min: [-half_extent, -half_extent, -half_extent],
            bbox_max: [half_extent, half_extent, half_extent],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_seed_test.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build import graph");

        // Same radius formula `build_import_graph` uses for the synthesized
        // orbit camera (dims are the full cube, diag/2).
        let dims = 2.0 * half_extent;
        let expected_radius = ((3.0 * dims * dims).sqrt() * 0.5).max(1e-3);

        let mesh_sources: Vec<&EffectGraphNode> = def
            .nodes
            .iter()
            .filter_map(|n| n.group.as_ref())
            .flat_map(|g| g.nodes.iter())
            .filter(|n| n.type_id == "node.gltf_mesh_source")
            .collect();
        assert_eq!(mesh_sources.len(), 2, "one node.gltf_mesh_source per material");

        // Largest-by-vertex-count-first ordering (build_import_graph sorts
        // materials that way) — Bark (900) is object 0, Leaf (250) is
        // object 1.
        let mut seen_counts: Vec<i32> = Vec::new();
        for mesh in &mesh_sources {
            let vcount = match mesh.params.get("source_vertex_count") {
                Some(SerializedParamValue::Int { value }) => *value,
                other => panic!("expected an Int source_vertex_count, got {other:?}"),
            };
            seen_counts.push(vcount);
            let radius = match mesh.params.get("source_bbox_radius") {
                Some(SerializedParamValue::Float { value }) => *value,
                other => panic!("expected a Float source_bbox_radius, got {other:?}"),
            };
            assert!(
                (radius - expected_radius).abs() < 1e-4,
                "expected {expected_radius}, got {radius}"
            );
        }
        seen_counts.sort_unstable();
        assert_eq!(seen_counts, vec![250, 900]);
    }

    /// BUG-221: composed per-object recenter. The mesh source is shifted
    /// by `-own_center` (so local `(0,0,0)` becomes THIS object's own
    /// visual center, not wherever the source file authored its local
    /// origin) and the user-facing `node.transform_3d`'s `pos` is
    /// repositioned to `own_center - scene_center`, so the two compose
    /// back to the OLD whole-scene-only `-scene_center` recenter (net
    /// world placement at import time is unchanged — only the rotation
    /// pivot moves). A synthetic two-material summary with hand-picked
    /// `own_center` values (a "minimal fixture constructed
    /// programmatically" — no `.glb` on disk needed for this half of the
    /// gate) makes every expected number computable by hand; the
    /// companion `gpu-proofs` tests below prove the same claim against a
    /// real multi-object asset by rendering it.
    #[test]
    fn bug221_object_transform_recenters_about_own_bbox_center_not_scene_center() {
        let mut big = full_material(0, "Big", 999); // k=0 after the largest-vertex-count-first sort
        big.own_center = [5.0, 1.0, -0.5];
        let mut small = full_material(1, "Small", 1); // k=1
        small.own_center = [-3.0, 0.0, 0.0];

        let summary = GltfImportSummary {
            materials: vec![big, small],
            bbox_min: [-4.0, -1.0, -2.0],
            bbox_max: [8.0, 3.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let center = [
            (summary.bbox_min[0] + summary.bbox_max[0]) * 0.5,
            (summary.bbox_min[1] + summary.bbox_max[1]) * 0.5,
            (summary.bbox_min[2] + summary.bbox_max[2]) * 0.5,
        ];
        assert_eq!(center, [2.0, 1.0, -0.5], "sanity: scene-wide bbox center");

        let path = std::path::Path::new("/tmp/synthetic_bug221_test.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build import graph");

        let group = def.nodes.iter().find(|n| n.type_id == GROUP_TYPE_ID).expect("object 0's group");
        let body = group.group.as_ref().expect("group has a body");
        let mesh0 = body
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("mesh_0"))
            .expect("mesh_0 node inside object 0's group");
        let transform0 = body
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("transform_0"))
            .expect("transform_0 node inside object 0's group");

        fn float_param(node: &EffectGraphNode, name: &str) -> f32 {
            match node.params.get(name) {
                Some(SerializedParamValue::Float { value }) => *value,
                other => panic!("expected a Float {name} param, got {other:?}"),
            }
        }

        let translate = [
            float_param(mesh0, "translate_x"),
            float_param(mesh0, "translate_y"),
            float_param(mesh0, "translate_z"),
        ];
        let pos = [
            float_param(transform0, "pos_x"),
            float_param(transform0, "pos_y"),
            float_param(transform0, "pos_z"),
        ];
        let own_center = [5.0_f32, 1.0, -0.5];

        for i in 0..3 {
            assert!(
                (translate[i] - (-own_center[i])).abs() < 1e-5,
                "mesh_0.translate_{i} should be -own_center[{i}]: got {translate:?}"
            );
            assert!(
                (pos[i] - (own_center[i] - center[i])).abs() < 1e-5,
                "transform_0.pos_{i} should be own_center[{i}] - center[{i}]: got {pos:?}"
            );
            // The composed net offset must equal the OLD whole-scene-only
            // recenter (-center) — this is the "layout preservation"
            // claim at value level: mesh-side translate and outer pos
            // cancel own_center out entirely, so net world placement is
            // byte-identical to the pre-fix formula regardless of what
            // own_center is.
            assert!(
                (translate[i] + pos[i] - (-center[i])).abs() < 1e-5,
                "mesh_0.translate_{i} + transform_0.pos_{i} must equal -center[{i}] \
                 (net world placement unchanged): translate={translate:?} pos={pos:?} center={center:?}"
            );
        }
        // "Position ≈ the object's bounds center", expressed in the SAME
        // recentered-scene coordinate space Position is shown in — a
        // concrete, non-tautological check on top of the formula asserts
        // above.
        assert!((pos[0] - 3.0).abs() < 1e-5 && (pos[1] - 0.0).abs() < 1e-5 && (pos[2] - 0.0).abs() < 1e-5);
    }

    /// Regression for a duplicate-handle panic found via the IMPORT_ANYTHING_WAVE
    /// Lane W5 conformance sweep on `MetalRoughSpheresNoTextures.glb` (98
    /// materials authored `"mat_0".."mat_97"`): SCENE_OBJECT_AND_PANEL_V2's P3
    /// stamps both the object's group node AND its inner `node.scene_object`
    /// with `unique_group_name`'s output (D6), which previously took the
    /// glTF material's name verbatim — so a material named `"mat_0"` collided
    /// with that SAME object's own `node.pbr_material` handle (`format!("mat_{k}")`),
    /// both flattening to `"mat_0/mat_0"` and panicking in `graph.rs`'s
    /// `add_node_named`. `collides_with_object_group_inner_handle` now vetoes
    /// this — build must succeed and the group must NOT be literally named
    /// "mat_0".
    #[test]
    fn material_named_like_its_own_inner_handle_does_not_collide() {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "mat_0", 100)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_mat_0_collision.glb");
        let (def, _report) =
            build_import_graph(&summary, path).expect("build import graph for mat_0-named material");

        let group = def
            .nodes
            .iter()
            .find(|n| n.type_id == GROUP_TYPE_ID)
            .expect("one object group");
        assert_ne!(
            group.handle.as_deref(),
            Some("mat_0"),
            "the group handle must be deduped away from this object's own mat_{{k}} inner handle"
        );

        // `flatten_groups` doesn't itself assert handle uniqueness (only
        // `Graph::add_node_named`, at load time, does — graph.rs:137) — so
        // reproduce that check directly on the flattened output, which is
        // exactly what the panic message ("duplicate handle 'mat_0/mat_0'")
        // was catching.
        let flattened = manifold_core::flatten::flatten_groups(&def).expect("flatten must succeed");
        let mut seen = std::collections::HashSet::new();
        for n in &flattened.nodes {
            if let Some(h) = &n.handle {
                assert!(seen.insert(h.clone()), "duplicate flattened handle: '{h}'");
            }
        }
    }

    // -----------------------------------------------------------------
    // SCENE_SETUP_PANEL_DESIGN.md P4 — merge_import_into_graph (D5)
    // -----------------------------------------------------------------

    /// Build a target scene `EffectGraphDef` (as if produced by a PRIOR
    /// import) whose bbox is a cube of half-extent `half_extent` centered
    /// at the origin, and whose `objects` count on `render_scene` is
    /// whatever a single-material summary produces (1). Its synthesized
    /// `node.orbit_camera`'s `distance` param is `2.2 * radius` — the exact
    /// value [`merge_import_into_graph`]'s scene-reference-radius proxy
    /// inverts back out.
    fn scene_def_with_bbox_half_extent(half_extent: f32) -> EffectGraphDef {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "Existing", 500)],
            bbox_min: [-half_extent, -half_extent, -half_extent],
            bbox_max: [half_extent, half_extent, half_extent],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_target_scene.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build target scene");
        def
    }

    fn merge_summary(materials: Vec<super::gltf_load::GltfMaterialInfo>, half_extent: f32) -> GltfImportSummary {
        GltfImportSummary {
            materials,
            bbox_min: [-half_extent, -half_extent, -half_extent],
            bbox_max: [half_extent, half_extent, half_extent],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        }
    }

    /// The scene's own render_scene node id + its `objects` count, read the
    /// same way a caller would before building the merge summary's expected
    /// port range.
    fn render_scene_objects(def: &EffectGraphDef) -> (u32, u32) {
        let node = def.nodes.iter().find(|n| n.type_id == "node.render_scene").unwrap();
        let objects = match node.params.get("objects") {
            Some(SerializedParamValue::Int { value }) => *value as u32,
            Some(SerializedParamValue::Float { value }) => *value as u32,
            _ => 0,
        };
        (node.id, objects)
    }

    /// Id offsetting: new nodes must be allocated ABOVE the target def's
    /// current max id (recursively, including inside existing object
    /// groups) — never colliding with an existing node anywhere in the def.
    #[test]
    fn merge_allocates_ids_above_the_targets_current_max() {
        let def = scene_def_with_bbox_half_extent(1.0);
        let existing_max = max_node_id_recursive(&def.nodes);

        let summary = merge_summary(vec![full_material(0, "Incoming", 300)], 1.0);
        let path = std::path::Path::new("/tmp/synthetic_merge_incoming.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge one object");

        assert!(!plan.new_nodes.is_empty(), "merge must produce at least the one incoming object group");
        for n in &plan.new_nodes {
            assert!(
                n.id > existing_max,
                "new top-level node id {} must be above the target's existing max id {existing_max}",
                n.id
            );
            if let Some(group) = &n.group {
                assert!(max_node_id_recursive(&group.nodes) > existing_max || group.nodes.is_empty());
            }
        }
    }

    /// Chrome skipped: a `MergePlan`'s `new_nodes` must NEVER contain a
    /// camera / envmap / hdri / light / lens node — the target scene keeps
    /// its own chrome untouched, D5's core rejection ("splice the whole
    /// assembled def" duplicates chrome).
    #[test]
    fn merge_plan_never_contains_chrome_nodes() {
        let def = scene_def_with_bbox_half_extent(1.0);
        let summary = merge_summary(
            vec![full_material(0, "A", 100), full_material(1, "B", 200)],
            1.0,
        );
        let path = std::path::Path::new("/tmp/synthetic_merge_chrome_check.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge two objects");

        const CHROME_TYPE_IDS: &[&str] = &[
            "node.orbit_camera",
            "node.free_camera",
            "node.look_at_camera",
            "node.camera_lens",
            "node.bake_environment",
            "node.hdri_source",
            "node.exposure",
            "node.switch_texture",
            "node.light",
            "node.render_scene",
            "node.ssao_gtao",
            "node.bilateral_blur",
            "node.mix",
        ];
        fn assert_no_chrome(nodes: &[EffectGraphNode]) {
            for n in nodes {
                assert!(
                    !CHROME_TYPE_IDS.contains(&n.type_id.as_str()),
                    "merge plan must never contain chrome node type_id `{}` — the target scene \
                     keeps its own chrome",
                    n.type_id
                );
                if let Some(group) = &n.group {
                    assert_no_chrome(&group.nodes);
                }
            }
        }
        assert_no_chrome(&plan.new_nodes);
        // Every new node is a top-level GROUP_TYPE_ID box (one per object) —
        // never a bare chrome-shaped node at the top level either.
        for n in &plan.new_nodes {
            assert_eq!(n.type_id, GROUP_TYPE_ID, "merge only ever adds object groups at the top level");
        }
    }

    /// Name-collision suffixing: an incoming material named the same as an
    /// existing top-level handle gets suffixed by `unique_group_name` (the
    /// importer's own dedup helper, reused verbatim — not reimplemented),
    /// never a silent duplicate name. `used_group_names` is seeded with the
    /// target's existing handles, so the very first colliding local object
    /// (whose own local index restarts at 0 for a merge) gets "Existing 1"
    /// — the same helper a single import would produce "Name 2" from ONLY
    /// when "Name 1" was already taken too; the exact numeral isn't the
    /// contract, uniqueness is.
    #[test]
    fn merge_suffixes_a_colliding_group_name() {
        let def = scene_def_with_bbox_half_extent(1.0);
        // The target scene's one object group is named "Existing" (full_material's name).
        assert!(def.nodes.iter().any(|n| n.handle.as_deref() == Some("Existing")));

        let summary = merge_summary(vec![full_material(0, "Existing", 300)], 1.0);
        let path = std::path::Path::new("/tmp/synthetic_merge_name_collision.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge colliding name");

        let new_group = plan.new_nodes.first().expect("one merged object group");
        assert_ne!(
            new_group.handle.as_deref(),
            Some("Existing"),
            "a colliding incoming group name must never collide with an existing top-level handle"
        );
        assert_eq!(
            new_group.handle.as_deref(),
            Some("Existing 1"),
            "unique_group_name's own dedup convention, reused verbatim"
        );
    }

    /// Objects count bumps correctly: merging N materials into a scene that
    /// already has M objects produces `new_objects_count == M + N`, and the
    /// new wires target ports `object_M..object_{M+N-1}` (continuing, never
    /// restarting at 0).
    #[test]
    fn merge_bumps_objects_count_and_continues_port_indices() {
        let def = scene_def_with_bbox_half_extent(1.0);
        let (render_id, existing_objects) = render_scene_objects(&def);
        assert_eq!(existing_objects, 1, "scene_def_with_bbox_half_extent seeds exactly one object");

        let summary = merge_summary(
            vec![full_material(0, "A", 100), full_material(1, "B", 200), full_material(2, "C", 50)],
            1.0,
        );
        let path = std::path::Path::new("/tmp/synthetic_merge_objects_count.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge three objects");

        assert_eq!(plan.render_scene_node_id, render_id);
        assert_eq!(plan.new_objects_count, existing_objects + 3);
        assert_eq!(plan.new_nodes.len(), 3, "one group per incoming material");

        for k in existing_objects..(existing_objects + 3) {
            assert!(
                plan.new_wires.iter().any(|w| w.to_node == render_id && w.to_port == format!("object_{k}")),
                "new wires must target object_{k} (continuing from the existing {existing_objects} objects), not restart at object_0"
            );
        }
        // Never re-targets an already-occupied port.
        assert!(
            !plan.new_wires.iter().any(|w| w.to_node == render_id && w.to_port == "object_0"),
            "merge must not re-wire the scene's EXISTING object_0 port"
        );
    }

    /// Card-spec sections extend: a glass incoming material gets an Opacity
    /// card slider sectioned under its OWN group name (same as a fresh
    /// import), appended to the plan's card additions — never dropped,
    /// never colliding with the target's existing card params.
    #[test]
    fn merge_extends_card_spec_sections_for_new_objects() {
        let def = scene_def_with_bbox_half_extent(1.0);
        let mut glass = full_material(0, "GlassPane", 400);
        glass.was_blend = true;
        glass.transmission_factor = 0.0;
        let summary = merge_summary(vec![glass], 1.0);
        let path = std::path::Path::new("/tmp/synthetic_merge_card_spec.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge one glass object");

        assert!(
            plan.new_card_params.iter().any(|p| p.name == "Opacity" && p.section.as_deref() == Some("GlassPane")),
            "the merged glass object must get its own Opacity slider sectioned under its group name"
        );
        assert!(
            plan.new_card_bindings.iter().any(|b| b.id.starts_with("opacity_")),
            "the merged Opacity slider must carry a binding"
        );
        // The shared Ambient binding still fans out for the new material too.
        assert!(
            plan.new_card_bindings.iter().any(|b| b.id == "scene_ambient"),
            "the merged object's material still gets the shared Ambient binding"
        );
    }

    /// D5 scale sanity, no-op case: an incoming asset within 10x of the
    /// scene's reference radius gets NO seeded scale (native units).
    #[test]
    fn merge_within_10x_never_normalizes() {
        let def = scene_def_with_bbox_half_extent(1.0); // scene reference radius ~= sqrt(3)
        let summary = merge_summary(vec![full_material(0, "Same", 100)], 1.0); // identical bbox, ratio 1.0
        let path = std::path::Path::new("/tmp/synthetic_merge_no_normalize.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge same-scale object");

        let group = plan.new_nodes.first().unwrap();
        let transform = group
            .group
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.transform_3d")
            .expect("object group has a transform_3d");
        assert!(
            !transform.params.contains_key("scale_x"),
            "within 10x, no scale should be seeded at all — native units"
        );
        assert!(
            !plan.report_lines.iter().any(|l| l.contains("scaled ×")),
            "no normalize report line when the ratio is within bounds"
        );
    }

    /// BUG-195 real fix: when the target's own mesh-source node carries a
    /// KNOWN `source_bbox_radius` that disagrees with what the
    /// orbit-camera-distance proxy would derive (e.g. the user hand-retuned
    /// Camera Distance on the card after import, per the confessed BUG-195
    /// blind spot), the stored radius wins — never the proxy.
    #[test]
    fn merge_scale_sanity_prefers_stored_radius_over_camera_proxy() {
        let mut def = scene_def_with_bbox_half_extent(1.0);
        // The proxy (unmutated orbit_camera.distance / 2.2) is ~= sqrt(3) ~=
        // 1.732 — same as the stored radius build_import_graph seeded, by
        // construction. Mutate ONLY the stored radius to something wildly
        // different, simulating a scene whose stored provenance no longer
        // agrees with the (user-editable) camera distance.
        let group = def
            .nodes
            .iter_mut()
            .find(|n| n.type_id == manifold_core::effect_graph_def::GROUP_TYPE_ID)
            .expect("one object group");
        let mesh = group
            .group
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|n| n.type_id == "node.gltf_mesh_source")
            .expect("object group has a mesh source");
        mesh.params
            .insert("source_bbox_radius".to_string(), SerializedParamValue::Float { value: 100.0 });

        // Incoming asset has the SAME bbox as the (unmutated) target scene —
        // against the proxy (~1.732) the ratio is 1.0 (no normalize); against
        // the mutated stored radius (100.0) the ratio is ~0.017 (normalizes).
        let summary = merge_summary(vec![full_material(0, "Same", 100)], 1.0);
        let path = std::path::Path::new("/tmp/synthetic_merge_prefers_stored_radius.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge");

        let new_group = plan.new_nodes.first().unwrap();
        let transform = new_group
            .group
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.transform_3d")
            .expect("object group has a transform_3d");
        let scale = match transform.params.get("scale_x") {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!(
                "expected a seeded scale_x — the stored radius (100.0), not the camera proxy \
                 (~1.732), must have driven this decision, got {other:?}"
            ),
        };
        assert!(
            scale > 10.0,
            "stored radius (100.0) vs incoming (~1.732) should normalize UP by ~57x, got {scale}"
        );
    }

    /// D5 scale sanity, too-big boundary: an incoming asset >10x LARGER
    /// than the scene's reference radius gets a seeded scale < 1.0 that
    /// brings it back down to the reference size.
    #[test]
    fn merge_over_10x_too_big_normalizes_down() {
        let def = scene_def_with_bbox_half_extent(1.0);
        // 20x the scene's half-extent -> incoming radius ~20x the scene's.
        let summary = merge_summary(vec![full_material(0, "Giant", 100)], 20.0);
        let path = std::path::Path::new("/tmp/synthetic_merge_too_big.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge oversized object");

        let group = plan.new_nodes.first().unwrap();
        let transform = group
            .group
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.transform_3d")
            .unwrap();
        let scale = match transform.params.get("scale_x") {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("expected a seeded scale_x float param, got {other:?}"),
        };
        assert!(scale < 1.0, "an oversized incoming asset must be scaled DOWN, got {scale}");
        assert!(
            plan.report_lines.iter().any(|l| l.contains("scaled ×")),
            "a normalize report line must be present"
        );
    }

    /// D5 scale sanity, too-small boundary: an incoming asset >10x SMALLER
    /// than the scene's reference radius gets a seeded scale > 1.0 that
    /// brings it back up to the reference size.
    #[test]
    fn merge_over_10x_too_small_normalizes_up() {
        let def = scene_def_with_bbox_half_extent(1.0);
        // 1/20th the scene's half-extent -> incoming radius ~1/20th the scene's.
        let summary = merge_summary(vec![full_material(0, "Tiny", 100)], 0.05);
        let path = std::path::Path::new("/tmp/synthetic_merge_too_small.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge undersized object");

        let group = plan.new_nodes.first().unwrap();
        let transform = group
            .group
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.transform_3d")
            .unwrap();
        let scale = match transform.params.get("scale_x") {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("expected a seeded scale_x float param, got {other:?}"),
        };
        assert!(scale > 1.0, "an undersized incoming asset must be scaled UP, got {scale}");
        assert!(
            plan.report_lines.iter().any(|l| l.contains("scaled ×")),
            "a normalize report line must be present"
        );
    }

    /// Negative gate: OBJECT_SAFETY_MAX is enforced on the POST-MERGE total
    /// (existing + incoming), never silently truncated.
    #[test]
    fn merge_over_object_safety_max_post_merge_errors_loudly() {
        let def = scene_def_with_bbox_half_extent(1.0); // 1 existing object
        let n = OBJECT_SAFETY_MAX as usize; // exactly at the max on its own; +1 existing pushes it over
        let materials: Vec<_> = (0..n).map(|k| full_material(k as u32, &format!("M{k}"), 10)).collect();
        let summary = merge_summary(materials, 1.0);
        let path = std::path::Path::new("/tmp/synthetic_merge_over_cap.glb");
        let err = merge_import_into_graph(&def, &summary, path)
            .expect_err("existing (1) + incoming (OBJECT_SAFETY_MAX) must exceed the bound");
        assert!(err.contains(&OBJECT_SAFETY_MAX.to_string()), "error must name the safety bound: {err}");
    }

    /// `graph_tool`-equivalent structural gate: a merged def (target scene +
    /// the plan's new nodes/wires spliced onto the target's own nodes/
    /// wires, `objects` bumped) flattens cleanly and compiles through the
    /// real registry — the same proof every import graph gets.
    #[test]
    fn merged_def_flattens_and_compiles_through_registry() {
        let def = scene_def_with_bbox_half_extent(1.0);
        let (render_id, existing_objects) = render_scene_objects(&def);
        let summary = merge_summary(
            vec![full_material(0, "Merged1", 150), full_material(1, "Merged2", 250)],
            1.0,
        );
        let path = std::path::Path::new("/tmp/synthetic_merge_flatten_compile.glb");
        let plan = merge_import_into_graph(&def, &summary, path).expect("merge two objects");

        let mut merged = def.clone();
        merged.nodes.extend(plan.new_nodes.clone());
        merged.wires.extend(plan.new_wires.clone());
        if let Some(node) = merged.nodes.iter_mut().find(|n| n.id == render_id) {
            node.params.insert(
                "objects".to_string(),
                SerializedParamValue::Int { value: plan.new_objects_count as i32 },
            );
        }
        if let Some(meta) = merged.preset_metadata.as_mut() {
            meta.params.extend(plan.new_card_params.clone());
            meta.bindings.extend(plan.new_card_bindings.clone());
            meta.string_bindings.extend(plan.new_string_bindings.clone());
        }

        let flat = manifold_core::flatten::flatten_groups(&merged)
            .unwrap_or_else(|e| panic!("merged def must flatten cleanly: {e}"));
        // The flattener reassigns EVERY ordinary node (including top-level
        // ones like render_scene) a fresh id (`flatten.rs`'s `clone.id =
        // new_id`) — the pre-flatten `render_id` no longer resolves, so
        // re-find render_scene by type_id in the flattened output.
        let flat_render_id = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .expect("flattened def keeps its render_scene node")
            .id;
        for k in existing_objects..(existing_objects + 2) {
            assert!(
                flat.wires.iter().any(|w| w.to_node == flat_render_id && w.to_port == format!("object_{k}")),
                "flattened merged def must wire object_{k}"
            );
        }

        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(merged, &registry, None)
            .expect("merged import graph must compile through PresetRuntime::from_def");
    }

    /// Real-asset merge, using two SMALL fixtures already in this worktree
    /// (not the held-out warehouse/skull/rosetta trio, which only exist in
    /// the main checkout) — `cc0__oomurasaki_azalea_r._x_pulchrum.glb` as
    /// the target scene, Khronos's tiny `Box.glb` merged into it. Writes
    /// the merged def to a JSON file so `graph_tool validate`/`fusion` can
    /// run against it as a real file, per the phase gate, and doubles as a
    /// regression test against real (not hand-built) glTF data.
    #[test]
    fn merges_a_real_asset_and_writes_merged_def_for_graph_tool() {
        let target_path = azalea_fixture_path();
        let box_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/Box.glb");
        if !target_path.exists() || !box_path.exists() {
            println!(
                "merges_a_real_asset_and_writes_merged_def_for_graph_tool: fixture(s) missing, skipping"
            );
            return;
        }

        let (target_def, target_report) =
            assemble_import_graph(&target_path).expect("assemble azalea target scene");
        assert_eq!(target_report.object_count, 2, "azalea has 2 materials with geometry");

        let box_summary =
            gltf_load::gltf_import_summary(&box_path).expect("parse Box.glb summary");
        let plan = merge_import_into_graph(&target_def, &box_summary, &box_path)
            .expect("merge Box.glb into the azalea scene");
        assert_eq!(plan.new_objects_count, 3, "2 azalea objects + 1 Box object");
        assert_eq!(plan.new_nodes.len(), 1, "Box.glb has exactly one material with geometry");

        let mut merged = target_def.clone();
        merged.nodes.extend(plan.new_nodes.clone());
        merged.wires.extend(plan.new_wires.clone());
        if let Some(node) =
            merged.nodes.iter_mut().find(|n| n.id == plan.render_scene_node_id)
        {
            node.params.insert(
                "objects".to_string(),
                SerializedParamValue::Int { value: plan.new_objects_count as i32 },
            );
        }
        if let Some(meta) = merged.preset_metadata.as_mut() {
            meta.params.extend(plan.new_card_params.clone());
            meta.bindings.extend(plan.new_card_bindings.clone());
            meta.string_bindings.extend(plan.new_string_bindings.clone());
        }

        // Structural proof, same as the synthetic test above.
        let flat = manifold_core::flatten::flatten_groups(&merged)
            .unwrap_or_else(|e| panic!("real-asset merged def must flatten cleanly: {e}"));
        let flat_render_id = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .expect("flattened def keeps its render_scene node")
            .id;
        assert!(
            flat.wires.iter().any(|w| w.to_node == flat_render_id && w.to_port == "object_2"),
            "flattened merged def must wire the new Box object at object_2"
        );
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(merged.clone(), &registry, None)
            .expect("real-asset merged import graph must compile through PresetRuntime::from_def");

        let json = serde_json::to_string_pretty(&merged).expect("serialize merged def");
        let out_path = std::env::temp_dir().join("scene_setup_p4_merged_azalea_box.json");
        std::fs::write(&out_path, json).expect("write merged def JSON for graph_tool");
        println!(
            "merges_a_real_asset_and_writes_merged_def_for_graph_tool: wrote {}",
            out_path.display()
        );
    }

    /// A target `def` with no top-level `node.render_scene` at all is a
    /// named escalation, not a guess — merging into a graph the panel would
    /// never show as a scene must error loudly.
    #[test]
    fn merge_into_a_def_without_render_scene_errors() {
        let def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        let summary = merge_summary(vec![full_material(0, "Orphan", 100)], 1.0);
        let path = std::path::Path::new("/tmp/synthetic_merge_no_render_scene.glb");
        let err = merge_import_into_graph(&def, &summary, path)
            .expect_err("a def with no render_scene must error, never silently no-op");
        assert!(err.contains("render_scene"));
    }

    /// D6 colour-space pinning + D3 port-wiring: a synthetic material
    /// carrying all five texture kinds (base-colour, normal, MR, occlusion,
    /// emissive) must wire all four NEW ports (`normal_map`, `mr_map`,
    /// `occlusion_map`, `emissive_map`) into `node.scene_object`, each
    /// fed by a `node.gltf_texture_source` whose `color_space` matches D6:
    /// base-colour and emissive decode sRGB (0), normal/MR/occlusion decode
    /// Linear (1) — the data-map convention (raw bytes ARE the value).
    #[test]
    fn imports_all_map_kinds_with_correct_color_spaces() {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "Helmet", 1000)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_all_maps.glb");
        let (def, report) = build_import_graph(&summary, path).expect("build graph");
        assert_eq!(report.textures_wired, 1, "base-colour wired");

        // Flatten so the group-internal texture-source nodes and the
        // top-level render_scene wires are both queryable in one flat
        // node/wire list (same recipe the grouping-equivalence test above
        // uses).
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

        // SCENE_OBJECT_AND_PANEL_V2_DESIGN D1/D3: the maps wire into this
        // object's `node.scene_object` bind node (`object_0_bind`), not
        // directly into render_scene — render_scene only ever sees the
        // single `object_0` port.
        let scene_object = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.scene_object")
            .expect("scene_object bind node");
        for port in ["normal_map", "mr_map", "occlusion_map", "emissive_map"] {
            assert!(
                flat.wires.iter().any(|w| w.to_node == scene_object.id && w.to_port == port),
                "expected a wire into scene_object port `{port}`"
            );
        }

        // Each new map's own source node carries the D6-correct color_space.
        let expect_color_space = |prefix: &str, expected: u32| {
            let node = flat
                .nodes
                .iter()
                .find(|n| n.node_id.starts_with(prefix) && n.type_id == "node.gltf_texture_source")
                .unwrap_or_else(|| panic!("expected a `{prefix}*` gltf_texture_source node"));
            let cs = node.params.get("color_space").expect("color_space param set");
            assert_eq!(
                *cs,
                enum_val(expected),
                "`{prefix}*` color_space must be {expected} ({})",
                if expected == 0 { "sRGB" } else { "Linear" }
            );
        };
        expect_color_space("tex_", 0); // base-colour: sRGB
        expect_color_space("normal_tex_", 1); // normal: Linear
        expect_color_space("mr_tex_", 1); // metallic-roughness: Linear
        expect_color_space("occlusion_tex_", 1); // occlusion: Linear
        expect_color_space("emissive_tex_", 0); // emissive: sRGB

        // KHR_materials_emissive_strength folds into the existing
        // emission_intensity param rather than growing a new one (D5).
        let mat = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.pbr_material")
            .expect("pbr_material node");
        assert_eq!(
            mat.params.get("emission_intensity"),
            Some(&float(2.5)),
            "emissive_strength (2.5) must land on emission_intensity"
        );

        // Fully-mapped, nothing report-worthy: no clearcoat/transmission/BLEND lines.
        assert!(
            report.report_lines.is_empty(),
            "a fully-mapped material with no clearcoat/transmission/BLEND should report nothing, got {:?}",
            report.report_lines
        );
    }

    /// D5 ORM-packing: when `occlusion_texture` and `mr_texture` share the
    /// same glTF texture index (the common "one packed ORM image" case),
    /// the importer must wire ONE `node.gltf_texture_source` into BOTH
    /// `occlusion_map_0` and `mr_map_0` — never decode the same physical
    /// image twice.
    #[test]
    fn orm_packed_occlusion_and_mr_share_one_texture_source_node() {
        let mut m = full_material(0, "ORM", 500);
        m.occlusion_texture = Some(7);
        m.mr_texture = Some(7); // same physical image as occlusion
        let summary = GltfImportSummary {
            materials: vec![m],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_orm.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

        let orm_sources: Vec<_> = flat
            .nodes
            .iter()
            .filter(|n| {
                n.type_id == "node.gltf_texture_source"
                    && n.params.get("texture_index") == Some(&int(7))
            })
            .collect();
        assert_eq!(
            orm_sources.len(),
            1,
            "occlusion_texture == mr_texture must decode through exactly ONE source node, found {}",
            orm_sources.len()
        );
        let scene_object = flat.nodes.iter().find(|n| n.type_id == "node.scene_object").unwrap();
        let source_id = orm_sources[0].id;
        for port in ["occlusion_map", "mr_map"] {
            assert!(
                flat.wires
                    .iter()
                    .any(|w| w.to_node == scene_object.id && w.to_port == port && w.from_node == source_id),
                "expected `{port}` wired directly from the shared ORM source node"
            );
        }
    }

    /// D9 doctrine ("every import produces a report") applied to G-P5's
    /// clearcoat feature set. GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1
    /// revised — full spec surface): a TEXTURED coat is now a real mapping
    /// too (no more report-only gap) — an over-featured synthetic material
    /// carrying a textured clearcoat together with transmission and a
    /// BLEND alphaMode must produce ZERO report lines, and must build a
    /// real `Blend` material with the transmission-folded alpha, the
    /// clearcoat factor, AND the clearcoatMap wire onto `node.pbr_material`
    /// / its group's output.
    #[test]
    fn over_featured_material_wires_clearcoat_texture_and_maps_transmission_to_blend() {
        let mut m = full_material(0, "Kitchen Sink", 300);
        m.clearcoat_factor = 1.0;
        m.clearcoat_roughness_factor = 0.1;
        m.clearcoat_texture = Some(0);
        m.transmission_factor = 0.9;
        m.was_blend = true;
        m.alpha_mask = false; // a real glTF BLEND material never sets MASK too
        m.base_color_factor = [0.9, 0.95, 1.0, 1.0];
        let summary = GltfImportSummary {
            materials: vec![m],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_over_featured.glb");
        let (def, report) = build_import_graph(&summary, path).expect("build graph");
        println!("over-featured report: {:#?}", report.report_lines);
        assert!(
            !report.report_lines.iter().any(|l| l.contains("clearcoat")),
            "clearcoat texture must no longer be report-only: {:?}",
            report.report_lines
        );
        assert!(
            !report.report_lines.iter().any(|l| l.contains("transmission") || l.contains("BLEND")),
            "transmission/BLEND must no longer produce report lines: {:?}",
            report.report_lines
        );

        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");
        let mat = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.pbr_material")
            .expect("pbr_material node");
        assert_eq!(
            mat.params.get("alpha_mode"),
            Some(&enum_val(2)),
            "transmission/BLEND material must map to alpha_mode Blend (2)"
        );
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2b: color_a is base_color.a
        // alone now (1.0) — transmission's see-through is carried by
        // fs_pbr's shader-side diffuse substitution, not by darkened alpha
        // (the old D8/F-P5 approximation this phase removes).
        let color_a = mat.params.get("color_a").expect("color_a set");
        match color_a {
            SerializedParamValue::Float { value } => assert!(
                (value - 1.0).abs() < 1e-4,
                "color_a must equal base_color.a unchanged, got {value}"
            ),
            other => panic!("expected Float color_a, got {other:?}"),
        }
        assert_eq!(mat.params.get("clearcoat"), Some(&float(1.0)));
        assert_eq!(mat.params.get("clearcoat_roughness"), Some(&float(0.1)));
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6: the textured coat wires
        // `clearcoat_map` from this object's group into its
        // `node.scene_object` bind node through the flattener, same as
        // sheen/iridescence/anisotropy (SCENE_OBJECT_AND_PANEL_V2_DESIGN
        // D1/D3 — render_scene itself only ever sees `object_0`).
        let scene_object = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.scene_object")
            .expect("scene_object bind node");
        assert!(
            flat.wires
                .iter()
                .any(|w| w.to_node == scene_object.id && w.to_port == "clearcoat_map"),
            "expected clearcoat_map wired on scene_object"
        );
    }

    /// D7 sun coherence: each of the Sun X/Y/Z card macros must carry TWO
    /// binding targets — the sun `node.light`'s position (unchanged,
    /// pre-existing) AND the envmap's new `sun_x`/`sun_y`/`sun_z` disc-
    /// direction params — so performing the sun macro moves illumination,
    /// shadow, AND the envmap's reflected sun disc together (Peter,
    /// 2026-07-15: "place these fake strips and lights in the same
    /// positions as the real scene lights").
    #[test]
    fn sun_macros_bind_both_the_light_and_the_envmap_disc_direction() {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "Object", 100)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_sun.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");
        let meta = def.preset_metadata.as_ref().expect("v2 metadata");

        for (macro_id, sun_param, env_param) in
            [("sun_x", "pos_x", "sun_x"), ("sun_y", "pos_y", "sun_y"), ("sun_z", "pos_z", "sun_z")]
        {
            let bindings: Vec<_> = meta.bindings.iter().filter(|b| b.id == macro_id).collect();
            assert_eq!(
                bindings.len(),
                2,
                "`{macro_id}` must carry exactly 2 binding targets (sun light + envmap disc), got {}",
                bindings.len()
            );
            let targets_sun = bindings.iter().any(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    node_id.as_str() == "sun" && param == sun_param
                }
                _ => false,
            });
            let targets_envmap = bindings.iter().any(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    node_id.as_str() == "envmap" && param == env_param
                }
                _ => false,
            });
            assert!(targets_sun, "`{macro_id}` must bind the sun light's `{sun_param}`");
            assert!(
                targets_envmap,
                "`{macro_id}` must ALSO bind the envmap's `{env_param}` disc-direction param"
            );
            // Both bindings must carry the same default (scale 1.0, "no
            // conversion math in a binding" per D7) so the card reproduces
            // the assembled look with no drift on first frame.
            let defaults: std::collections::HashSet<_> =
                bindings.iter().map(|b| b.default_value.to_bits()).collect();
            assert_eq!(defaults.len(), 1, "`{macro_id}`'s two bindings must share one default value");
        }

        // D7 import defaults: softbox @ 1.0, not the legacy gradient @ 0.
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");
        let envmap = flat.nodes.iter().find(|n| n.type_id == "node.bake_environment").unwrap();
        assert_eq!(envmap.params.get("mode"), Some(&enum_val(1)), "import default mode = Softbox");
        assert_eq!(envmap.params.get("intensity"), Some(&float(1.0)), "import default intensity = 1.0");
        let env_intensity_param = meta.params.iter().find(|p| p.id == "env_intensity").unwrap();
        assert_eq!(env_intensity_param.default_value, 1.0, "Environment card default = 1.0");
        assert_eq!(env_intensity_param.min, 0.0);
        assert_eq!(env_intensity_param.max, 4.0, "range stays 0-4 (D7: only the default flips)");

        // F-P7 import defaults: dome fill on, strips at half the primitive
        // default — node params and card faders stay in sync through the
        // shared constants.
        assert_eq!(
            envmap.params.get("fill"),
            Some(&float(IMPORT_FILL_DEFAULT)),
            "import default fill = IMPORT_FILL_DEFAULT"
        );
        assert_eq!(
            envmap.params.get("emitter_intensity"),
            Some(&float(IMPORT_STRIPS_DEFAULT)),
            "import default strips = IMPORT_STRIPS_DEFAULT"
        );
        let fill_param = meta.params.iter().find(|p| p.id == "env_fill").unwrap();
        assert_eq!(fill_param.default_value, IMPORT_FILL_DEFAULT);
        let strips_param = meta.params.iter().find(|p| p.id == "env_strips").unwrap();
        assert_eq!(strips_param.default_value, IMPORT_STRIPS_DEFAULT);
    }

    /// BUG-036 round-trip gate: the assembled import graph — including the
    /// new map-port wires and the D7 sun-coherence dual bindings — must
    /// survive a save/reload cycle. `EffectGraphDef` (this function's
    /// return type) IS the persisted artifact for a generator layer's
    /// override (`ImportModelLayerCommand` stores it verbatim), so a JSON
    /// round trip through its own `Serialize`/`Deserialize` impl is the
    /// real save/reload path, not a stand-in for it. Asserts the wires and
    /// bindings survive AND that the reloaded def still builds through the
    /// production loader (`PresetRuntime::from_def`) — proving the ports
    /// resolve after reload, not only right after assembly.
    #[test]
    fn round_trip_preserves_map_wires_and_sun_coherence_bindings() {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "Helmet", 1000)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_round_trip.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");

        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        let flat = manifold_core::flatten::flatten_groups(&reloaded).expect("flatten reloaded def");
        let scene_object = flat.nodes.iter().find(|n| n.type_id == "node.scene_object").unwrap();
        for port in ["normal_map", "mr_map", "occlusion_map", "emissive_map"] {
            assert!(
                flat.wires.iter().any(|w| w.to_node == scene_object.id && w.to_port == port),
                "reloaded def must still wire `{port}` — maps must stay bound after reload"
            );
        }
        let meta = reloaded.preset_metadata.as_ref().expect("reloaded v2 metadata");
        for macro_id in ["sun_x", "sun_y", "sun_z"] {
            let count = meta.bindings.iter().filter(|b| b.id == macro_id).count();
            assert_eq!(count, 2, "`{macro_id}`'s dual binding must survive reload");
        }

        // Modulation live after reload, not just structurally present: the
        // reloaded def must still build through the production loader.
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(reloaded, &registry, None)
            .expect("reloaded import graph must build through PresetRuntime::from_def");
    }

    /// GLTF_ANIMATION_DESIGN.md A1 deliverable 3: a material whose
    /// `GltfMaterialInfo::animation` resolved gets one
    /// `node.gltf_animation_source` inserted into its group, wired into
    /// its OWN `node.transform_3d`'s nine port-shadowed inputs — additive
    /// to the static recenter (which stays on `transform_3d`'s own
    /// pos_x/y/z param default). A material with no resolved animation
    /// gets no such node (never fabricated).
    #[test]
    fn animated_material_wires_animation_source_into_its_own_transform_3d() {
        use super::gltf_load::{GltfObjectAnimation, QuatTrack, Vec3Track};

        let mut animated = full_material(0, "Inner", 1000);
        animated.animations = vec![Some(GltfObjectAnimation {
            duration_s: 2.0,
            translation: Some(Vec3Track {
                times: vec![0.0, 1.0],
                values: vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0]],
                ..Default::default()
            }),
            rotation: Some(QuatTrack {
                times: vec![0.0, 1.0],
                values: vec![
                    [0.0, 0.0, 0.0, 1.0],
                    [0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2],
                ],
                ..Default::default()
            }),
            scale: None,
        })];
        let mut static_obj = full_material(1, "Outer", 500);
        static_obj.animations = Vec::new();

        let summary = GltfImportSummary {
            materials: vec![animated, static_obj],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_animation_wiring.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

        let anim_nodes: Vec<_> =
            flat.nodes.iter().filter(|n| n.type_id == "node.gltf_animation_source").collect();
        assert_eq!(anim_nodes.len(), 1, "only the animated object gets a source node");
        let anim = anim_nodes[0];

        // Table params carry the parsed tracks (translation/rotation
        // present, scale absent per the synthetic summary above).
        assert!(matches!(
            anim.params.get("translation_track"),
            Some(SerializedParamValue::Table { rows }) if rows.len() == 2
        ));
        assert!(matches!(
            anim.params.get("rotation_track"),
            Some(SerializedParamValue::Table { rows }) if rows.len() == 2
        ));
        assert!(
            !anim.params.contains_key("scale_track"),
            "absent scale channel must not be fabricated as a Table"
        );
        assert_eq!(anim.params.get("duration_s"), Some(&float(2.0)));

        let transform =
            flat.nodes.iter().find(|n| n.type_id == "node.transform_3d" && n.node_id.as_str().contains("transform_0")).expect("transform_0 present");
        for port in
            ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z"]
        {
            assert!(
                flat.wires
                    .iter()
                    .any(|w| w.from_node == anim.id && w.from_port == port && w.to_node == transform.id && w.to_port == port),
                "animation source must wire `{port}` into transform_0"
            );
        }

        // The registry-facing build path must accept the Table params
        // (proves node.gltf_animation_source is actually registered and
        // its Table param declarations match SerializedParamValue's
        // conversion, not just that JSON round-trips syntactically).
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None)
            .expect("import graph with an animated object must build through PresetRuntime::from_def");
    }

    /// BUG-204 regression: the A4 Retrigger card param is `is_trigger`
    /// and binds to the animation nodes' `trigger_count` — which must be
    /// `ParamType::Trigger`, or validate.rs card lint (d) rejects the
    /// assembled graph and EVERY animated or rigged glb fails at import
    /// (skeleton_animated.glb, 2026-07-17: A4 shipped `trigger_count` as
    /// Int four days after the lint landed). Runs the real fixture through
    /// the same lint the import path uses.
    #[test]
    fn animated_and_rigged_import_passes_card_lints() {
        use crate::node_graph::persistence::EffectGraphDefExt;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/skeleton_animated.glb");
        let (def, _report) =
            super::assemble_import_graph(&path).expect("assemble skeleton_animated.glb");
        let registry = PrimitiveRegistry::with_builtin();
        let graph = def.clone().into_graph(&registry).expect("import graph must build");
        let (errors, _warnings) =
            crate::node_graph::validate::check_card_lints(&def, Some(&graph));
        assert!(
            errors.is_empty(),
            "card lints must accept the assembled animated+rigged import: {errors:?}"
        );
    }

    /// BUG-205 regression (double-transform half): a SKINNED object must
    /// NOT get a `node.gltf_animation_source` wired into its transform_3d.
    /// skeleton_animated.glb animates `Bip01` — an ancestor ABOVE the
    /// joint tree whose static 0.0254 scale is already inside the joint
    /// palette via `joint_root_world` — so the rigid path re-applying that
    /// chain shrank the render to 0.0254² of its authored size (a ~12px
    /// speck at the framing distance).
    #[test]
    fn skinned_import_gets_no_rigid_animation_source() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/skeleton_animated.glb");
        let (def, _report) =
            super::assemble_import_graph(&path).expect("assemble skeleton_animated.glb");
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten import def");
        assert!(
            flat.nodes.iter().any(|n| n.type_id == "node.gltf_skeleton_pose"),
            "rigged import must drive its mesh through node.gltf_skeleton_pose"
        );
        assert!(
            !flat.nodes.iter().any(|n| n.type_id == "node.gltf_animation_source"),
            "a skinned object's positioning comes entirely from its joint palette — \
             a rigid node.gltf_animation_source on the same object re-applies the \
             ancestor chain a second time (BUG-205)"
        );
    }

    /// BUG-208: an object with BOTH a skin and morph targets must import
    /// with its morph animation COMPOSED, not silently dropped —
    /// `node.morph_targets_blend` chained between
    /// `node.gltf_skinned_mesh_source` and `node.skin_mesh`'s `in` (glTF
    /// applies morph then skin, §3.7.2), and the deltas source's
    /// `skinned` param set so its loaded deltas share the skinned
    /// source's untransformed bind-pose space. `skin_morph.glb`: a
    /// Blender-authored armature-skinned cylinder with a keyframed
    /// "Bulge" shape key, carrying both a skin AND morph targets plus
    /// animation channels for each.
    #[test]
    fn skin_and_morph_combination_composes_instead_of_dropping() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/hostile/skin_morph.glb");
        let (def, report) = super::assemble_import_graph(&path).expect("assemble skin_morph.glb");

        assert!(
            report.report_lines.iter().any(|l| l.contains("BUG-208")),
            "import report must call out the skin+morph composition explicitly: {:?}",
            report.report_lines
        );

        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten import def");
        assert!(
            flat.nodes.iter().any(|n| n.type_id == "node.gltf_skinned_mesh_source"),
            "skin+morph object must still be driven by node.gltf_skinned_mesh_source"
        );
        let blend = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.morph_targets_blend")
            .expect("skin+morph object must carry a node.morph_targets_blend — morph animation \
                     dropped silently (BUG-208)");
        let skinmesh = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.skin_mesh")
            .expect("skinned object must carry node.skin_mesh");
        assert!(
            flat.wires
                .iter()
                .any(|w| w.from_node == blend.id && w.from_port == "out"
                    && w.to_node == skinmesh.id && w.to_port == "in"),
            "node.morph_targets_blend's `out` must feed node.skin_mesh's `in` directly — \
             glTF applies morph before skin (§3.7.2)"
        );
        let deltas = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.gltf_morph_deltas_source")
            .expect("skin+morph object must carry node.gltf_morph_deltas_source");
        assert_eq!(
            deltas.params.get("skinned"),
            Some(&SerializedParamValue::Bool { value: true }),
            "the deltas source must be told this object is skinned — otherwise its loader \
             world-transforms the deltas while the skinned base vertices stay untransformed \
             (a coordinate-space mismatch, BUG-208)"
        );
    }

    /// The hostile fixture shelf: real-world-shaped assets (Sketchfab FBX
    /// conversions, Mixamo rigs, Blender exports) whose traits the Khronos
    /// suite doesn't exercise — transform-bearing ancestors above joint
    /// trees, unit-conversion scales, animated prefixes. BUG-204 and
    /// BUG-205 both shipped through green gates because no oracle ever fed
    /// the import pipeline this input class; every glb under
    /// `tests/fixtures/gltf/hostile/` runs the full CPU chain here
    /// (assemble → graph build → card lints → PresetRuntime build), and
    /// the gpu-proofs sibling below renders each one and checks framing
    /// invariants. Add assets by dropping a glb in the directory —
    /// `scripts/blender/fbx2glb.py` converts FBX-only sources.
    fn hostile_fixture_paths() -> Vec<std::path::PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/hostile");
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read hostile fixture dir {}: {e}", dir.display()))
            .filter_map(|entry| {
                let p = entry.ok()?.path();
                (p.extension().and_then(|e| e.to_str()) == Some("glb")).then_some(p)
            })
            .collect();
        paths.sort();
        assert!(!paths.is_empty(), "hostile fixture shelf is empty — the sweep is vacuous");
        paths
    }

    #[test]
    fn hostile_fixtures_assemble_validate_and_build() {
        use crate::node_graph::persistence::EffectGraphDefExt;
        for path in hostile_fixture_paths() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let (def, _report) = super::assemble_import_graph(&path)
                .unwrap_or_else(|e| panic!("{name}: assemble_import_graph failed: {e}"));
            let registry = PrimitiveRegistry::with_builtin();
            let graph = def
                .clone()
                .into_graph(&registry)
                .unwrap_or_else(|e| panic!("{name}: import graph failed to build: {e:?}"));
            let (errors, _warnings) =
                crate::node_graph::validate::check_card_lints(&def, Some(&graph));
            assert!(errors.is_empty(), "{name}: card lints rejected the import: {errors:?}");
            PresetRuntime::from_def(def, &registry, None)
                .unwrap_or_else(|e| panic!("{name}: PresetRuntime::from_def failed: {e:?}"));
        }
    }

    /// Merge every hostile fixture into a real existing scene (the azalea
    /// import) and run the merged def through the same CPU chain — merge
    /// reuses `build_object_group`, but that sharing is exactly the kind
    /// of claim this shelf exists to prove rather than assume (BUG-204's
    /// class: two features correct alone, never composed). Also pins
    /// BUG-205 through the merge path: a merged skinned object must get
    /// its skeleton pose and must NOT get a rigid animation source.
    #[test]
    fn hostile_fixtures_merge_into_existing_scene() {
        use crate::node_graph::persistence::EffectGraphDefExt;
        let (target, _report) = super::assemble_import_graph(&azalea_fixture_path())
            .expect("assemble azalea target scene");
        let (render_id, existing_objects) = render_scene_objects(&target);
        for path in hostile_fixture_paths() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let plan = super::assemble_merge_plan(&target, &path)
                .unwrap_or_else(|e| panic!("{name}: assemble_merge_plan failed: {e}"));

            let had_skin =
                plan.new_nodes.iter().any(|n| contains_type(n, "node.gltf_skeleton_pose"));
            let has_rigid_anim =
                plan.new_nodes.iter().any(|n| contains_type(n, "node.gltf_animation_source"));
            if had_skin {
                assert!(
                    !has_rigid_anim,
                    "{name}: merged skinned object carries a rigid gltf_animation_source — \
                     BUG-205's double-transform through the merge path"
                );
            }

            let mut merged = target.clone();
            merged.nodes.extend(plan.new_nodes.clone());
            merged.wires.extend(plan.new_wires.clone());
            if let Some(node) = merged.nodes.iter_mut().find(|n| n.id == render_id) {
                node.params.insert(
                    "objects".to_string(),
                    SerializedParamValue::Int { value: plan.new_objects_count as i32 },
                );
            }
            if let Some(meta) = merged.preset_metadata.as_mut() {
                meta.params.extend(plan.new_card_params.clone());
                meta.bindings.extend(plan.new_card_bindings.clone());
                meta.string_bindings.extend(plan.new_string_bindings.clone());
            }

            let registry = PrimitiveRegistry::with_builtin();
            let graph = merged
                .clone()
                .into_graph(&registry)
                .unwrap_or_else(|e| panic!("{name}: merged graph failed to build: {e:?}"));
            let (errors, _warnings) =
                crate::node_graph::validate::check_card_lints(&merged, Some(&graph));
            assert!(errors.is_empty(), "{name}: card lints rejected the merged def: {errors:?}");
            PresetRuntime::from_def(merged, &registry, None)
                .unwrap_or_else(|e| panic!("{name}: merged PresetRuntime build failed: {e:?}"));
            let _ = existing_objects;
        }
    }

    /// Group-aware type search: merge plans emit one GROUP node per object
    /// with the real producers in its `group.body`.
    fn contains_type(node: &EffectGraphNode, type_id: &str) -> bool {
        if node.type_id == type_id {
            return true;
        }
        node.group
            .as_ref()
            .is_some_and(|g| g.nodes.iter().any(|inner| contains_type(inner, type_id)))
    }

    /// Render every hostile fixture and check framing invariants — the
    /// automated form of "does it look plausibly right": enough lit pixels
    /// to be a real render (BUG-205's speck fails), not a full-frame
    /// blowout, and the lit centroid near frame center (wrong-space
    /// framing fails). Edge-contact (object cropped at opposite frame
    /// edges) is checked at TWO phases — 0.0 (straight/rest pose, the worst
    /// case for an elongated skinned rig) and 0.25 (the original single
    /// phase) — against an xfail list. BUG-206 fixed the framing distance
    /// (per-axis fit, not bbox-diagonal), so the list is empty; a fixture
    /// only goes back on it after investigation confirms the crop is a
    /// distinct, unrelated bug (see BUG-206 backlog entry).
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn hostile_fixtures_render_within_framing_invariants() {
        let (w, h) = (256u32, 256u32);
        const EDGE_XFAIL: &[&str] = &[];
        const PHASES: &[f32] = &[0.0, 0.25];
        for path in hostile_fixture_paths() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            for &phase in PHASES {
                let (def, _report) = super::assemble_import_graph(&path)
                    .unwrap_or_else(|e| panic!("{name}: assemble failed: {e}"));
                let duration_s = skeleton_pose_duration_s_or_static(&def);
                let rgba = render_skinned_import_at_progress(def, w, h, phase, duration_s);

                let mut lit = 0u64;
                let (mut cx, mut cy) = (0.0f64, 0.0f64);
                let (mut top, mut bottom, mut left, mut right) = (false, false, false, false);
                for y in 0..h {
                    for x in 0..w {
                        let i = ((y * w + x) * 4) as usize;
                        if rgba[i].max(rgba[i + 1]).max(rgba[i + 2]) > 8 {
                            lit += 1;
                            cx += x as f64;
                            cy += y as f64;
                            top |= y == 0;
                            bottom |= y == h - 1;
                            left |= x == 0;
                            right |= x == w - 1;
                        }
                    }
                }
                let fraction = lit as f64 / (w as u64 * h as u64) as f64;
                assert!(
                    (0.005..=0.95).contains(&fraction),
                    "{name}@phase{phase}: lit fraction {fraction:.4} outside [0.005, 0.95] — \
                     speck (BUG-205 class), black frame, or full-frame blowout"
                );
                let (cx, cy) = (cx / lit as f64 / w as f64, cy / lit as f64 / h as f64);
                assert!(
                    (0.2..=0.8).contains(&cx) && (0.2..=0.8).contains(&cy),
                    "{name}@phase{phase}: lit centroid ({cx:.2}, {cy:.2}) outside the center \
                     region — wrong-space framing/recenter"
                );
                let cropped = (top && bottom) || (left && right);
                if !EDGE_XFAIL.contains(&name.as_str()) {
                    assert!(
                        !cropped,
                        "{name}@phase{phase}: object touches opposite frame edges — default \
                         framing crops it (BUG-206 class)"
                    );
                }
            }
        }
    }

    /// BUG-205 regression (bbox-space half): the import summary's bbox for
    /// a skinned mesh must live in bind-pose SKINNED space (what
    /// `node.skin_mesh` renders), not the mesh node's world space (which
    /// glTF skinning ignores). skeleton_animated.glb's two spaces disagree
    /// visibly: mesh-node-world y spans 0.36..2.22, bind-skinned y spans
    /// -0.57..1.20 — the old bbox recentered/framed a box the skeleton
    /// never occupies (feet cropped below frame).
    #[test]
    fn skinned_import_summary_bbox_is_in_bind_skinned_space() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/skeleton_animated.glb");
        let summary =
            super::gltf_load::gltf_import_summary(&path).expect("parse skeleton_animated.glb");
        assert!(
            summary.bbox_min[1] < 0.0 && summary.bbox_max[1] < 1.5,
            "bbox must be the bind-pose skinned one (y ≈ -0.57..1.20), got y {:.3}..{:.3} — \
             the mesh-node-world bbox (y ≈ 0.36..2.22) means the summary regressed to \
             treating the skinned mesh as static (BUG-205)",
            summary.bbox_min[1],
            summary.bbox_max[1]
        );
    }

    /// GLTF_ANIMATION_DESIGN.md A1 deliverable 4 (Table params + the new
    /// node type survive V1 JSON save→reload — the STANDARD §5 gate must
    /// PROVE this, not assume it, per the phase brief).
    #[test]
    fn animation_tables_survive_json_round_trip() {
        use super::gltf_load::{GltfObjectAnimation, QuatTrack, Vec3Track};

        let mut animated = full_material(0, "Inner", 1000);
        animated.animations = vec![Some(GltfObjectAnimation {
            duration_s: 3.708_33,
            translation: Some(Vec3Track {
                times: vec![0.0, 1.25, 2.5, 3.708_33],
                values: vec![[0.0, 0.0, 0.0], [0.0, 2.52, 0.0], [0.0, 2.52, 0.0], [0.0, 0.0, 0.0]],
                ..Default::default()
            }),
            rotation: Some(QuatTrack {
                times: vec![1.25, 2.5],
                values: vec![[0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 0.0]],
                ..Default::default()
            }),
            scale: None,
        })];
        let summary = GltfImportSummary {
            materials: vec![animated],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_animation_round_trip.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");

        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        let flat = manifold_core::flatten::flatten_groups(&reloaded).expect("flatten reloaded def");
        let anim = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.gltf_animation_source")
            .expect("animation source survives reload");
        assert!(matches!(
            anim.params.get("translation_track"),
            Some(SerializedParamValue::Table { rows }) if rows.len() == 4
        ));
        assert!(matches!(
            anim.params.get("rotation_track"),
            Some(SerializedParamValue::Table { rows }) if rows.len() == 2
        ));

        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(reloaded, &registry, None)
            .expect("reloaded import graph with animation Tables must build through PresetRuntime::from_def");
    }

    /// IMPORT_FIDELITY_DESIGN.md D8/F-P5 round-trip gate: a `Blend` alpha_mode
    /// (from a transmission material) and its performer-facing Opacity card
    /// binding must survive save → reload, and stay live (modulatable) after
    /// reload — the BUG-036 rule (create-path green is half a gate for
    /// stateful features).
    #[test]
    fn round_trip_preserves_blend_alpha_mode_and_opacity_binding() {
        let mut m = full_material(0, "Windshield", 500);
        m.was_blend = true;
        m.transmission_factor = 0.9;
        let summary = GltfImportSummary {
            materials: vec![m],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_glass_round_trip.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");

        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        let flat = manifold_core::flatten::flatten_groups(&reloaded).expect("flatten reloaded def");
        let mat = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.pbr_material")
            .expect("pbr_material node");
        assert_eq!(
            mat.params.get("alpha_mode"),
            Some(&enum_val(2)),
            "reloaded def must still carry alpha_mode Blend"
        );

        let meta = reloaded.preset_metadata.as_ref().expect("reloaded v2 metadata");
        assert!(
            meta.bindings.iter().any(|b| b.label == "Opacity"),
            "reloaded def must still carry the glass object's Opacity card binding"
        );

        // Modulation live after reload — not just structurally present.
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(reloaded, &registry, None)
            .expect("reloaded glass import graph must build through PresetRuntime::from_def");
    }

    /// GRAPH_TOOLING_DESIGN D6: `assemble_import_graph`'s output must be
    /// validated through `validate_def` before it reaches the project — the
    /// assembler is code and has bugs. This proves the mechanism the
    /// `manifold-app` importer hook relies on: a deliberately corrupted
    /// assembler-style def (one node's `type_id` rewritten to a type the
    /// registry doesn't know) fails `validate_def` with an issue naming that
    /// node, never silently. Fixture-free — reuses the synthetic two-material
    /// summary from `build_import_graph_groups_each_object_and_flattens_to_flat_wiring`.
    #[test]
    fn corrupted_assembler_output_fails_validation_naming_the_node() {
        use super::gltf_load::GltfMaterialInfo;
        use crate::node_graph::{ValidateKind, validate_def};
        use manifold_gpu::GpuDevice;

        let mat = |material_index: u32, name: &str, verts: u32, tex: Option<u32>| GltfMaterialInfo {
            material_index,
            name: Some(name.to_string()),
            base_color_factor: [0.5, 0.5, 0.5, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            emissive: [0.0, 0.0, 0.0],
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: tex,
            normal_texture: None,
            normal_scale: 1.0,
            mr_texture: None,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive_texture: None,
            emissive_strength: 1.0,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0, 1.0, 1.0],
            specular_texture: None,
            specular_color_texture: None,
            base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            uv_tex_coord_override: false,
            mr_texture_is_gloss_alpha: false,
            transmission_factor: 0.0,
            transmission_texture: None,
            clearcoat_factor: 0.0,
            clearcoat_roughness_factor: 0.0,
            clearcoat_texture: None,
            clearcoat_roughness_texture: None,
            clearcoat_normal_texture: None,
            sheen_color_factor: [0.0, 0.0, 0.0],
            sheen_roughness_factor: 0.0,
            sheen_color_texture: None,
            sheen_roughness_texture: None,
            iridescence_factor: 0.0,
            iridescence_ior: 1.3,
            iridescence_thickness_minimum: 100.0,
            iridescence_thickness_maximum: 400.0,
            iridescence_texture: None,
            iridescence_thickness_texture: None,
            anisotropy_strength: 0.0,
            anisotropy_rotation: 0.0,
            anisotropy_texture: None,
            dispersion: 0.0,
            volume_thickness_factor: 0.0,
            volume_attenuation_distance: super::gltf_load::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
            volume_attenuation_color: [1.0, 1.0, 1.0],
            volume_thickness_texture: None,
            was_blend: false,
            vertex_count: verts,
            base_color_sampler: super::gltf_load::GltfSamplerInfo::default(),
            normal_sampler: super::gltf_load::GltfSamplerInfo::default(),
            mr_sampler: super::gltf_load::GltfSamplerInfo::default(),
            occlusion_sampler: super::gltf_load::GltfSamplerInfo::default(),
            emissive_sampler: super::gltf_load::GltfSamplerInfo::default(),
            animations: Vec::new(),
            skin: None,
            morph: None,
            own_center: [0.0, 0.0, 0.0],
        };
        let summary = GltfImportSummary {
            materials: vec![mat(0, "Leaf", 1200, Some(0)), mat(1, "Bark", 800, None)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
        };
        let path = std::path::Path::new("/tmp/synthetic_model.glb");
        let (mut def, _report) = build_import_graph(&summary, path).expect("build graph");

        // Corrupt exactly one node's type_id to something the registry has
        // never heard of — the "assembler wrote a typo" failure class.
        // The producer nodes live inside a group's body (see
        // `build_import_graph_groups_each_object_and_flattens_to_flat_wiring`
        // above), so search recursively rather than only the top level.
        fn find_pbr_material_mut(
            nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
        ) -> Option<&mut manifold_core::effect_graph_def::EffectGraphNode> {
            for n in nodes {
                if n.type_id == "node.pbr_material" {
                    return Some(n);
                }
                if let Some(group) = n.group.as_mut()
                    && let Some(found) = find_pbr_material_mut(&mut group.nodes)
                {
                    return Some(found);
                }
            }
            None
        }
        const CORRUPT_TYPE_ID: &str = "node.definitely_not_a_real_type";
        {
            let target = find_pbr_material_mut(&mut def.nodes)
                .expect("assembled graph has a pbr_material node to corrupt");
            target.type_id = CORRUPT_TYPE_ID.to_string();
        }

        let registry = PrimitiveRegistry::with_builtin();
        let device = std::sync::Arc::new(GpuDevice::new());
        let report = validate_def(&def, &registry, ValidateKind::Generator, &device);

        assert!(
            !report.is_valid(),
            "a def with an unknown type_id must fail validate_def, not pass silently"
        );
        // Match by type_id, not doc_id: `validate_def` flattens groups before
        // classifying (this def's producers live inside a group), and
        // flattening renumbers doc ids via a fresh `IdAlloc` — the corrupted
        // node's ORIGINAL id doesn't survive to the error, but its (equally
        // corrupted) type_id does, and the reported node_id still names a
        // real node in the flattened def the error is about.
        assert!(
            report
                .errors
                .iter()
                .any(|issue| issue.type_id.as_deref() == Some(CORRUPT_TYPE_ID) && issue.node_id.is_some()),
            "expected an error naming a node with the corrupted type_id; got: {:?}",
            report.errors
        );
    }

    /// Regression for the glTF-import "unknown parameter 'pos_x_N'" load
    /// failure (Peter, 2026-07-05), REWRITTEN for
    /// SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D3/P2 — the original
    /// subject (a per-object param that only existed once `render_scene`
    /// reconfigured to a higher object count) no longer exists: per-object
    /// TRS is a `transform_n: Transform` PORT now, not a param. The
    /// analogous regression is a PORT that only exists once `render_scene`
    /// reconfigures — `transform_2`, which a naive loader could reject as
    /// "unknown port" if it validated wires against the default 2-object
    /// port surface instead of the reconfigured one. A model with >2
    /// distinct materials (`objects >= 3`) is exactly the shape that used to
    /// trip this; the azalea fixture has only 2 objects, so it never
    /// exercised it — the coverage gap that let the original bug ship. This
    /// synthetic 3-object def reproduces the shape with no large fixture and
    /// must load + wire clean, proving reconfigure runs before wire/port
    /// validation for the new port-based surface too.
    #[test]
    fn render_scene_with_three_objects_loads_object_port() {
        // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D4/P2: `transform_2` (the
        // subject of the original "unknown parameter 'pos_x_2'" regression
        // this test was written for) no longer exists as a render_scene
        // port at all — `node.scene_object` owns `transform` now, and
        // render_scene's per-object surface is `object_k` only. The
        // analogous regression under the new shape: `object_2`, a port
        // that only exists once render_scene reconfigures to objects >= 3,
        // must load clean (reconfigure runs before port validation) —
        // same proof, new port.
        use crate::node_graph::persistence::EffectGraphDefExt;

        let mut render = plain_node(0, "render", "node.render_scene", "render");
        render.params.insert("objects".to_string(), int(3));
        render.params.insert("lights".to_string(), int(1));

        let scene_object_2 = plain_node(1, "object_2", "node.scene_object", "object_2");

        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, scene_object_2],
            wires: vec![wire(1, "object", 0, "object_2")],
        };

        // Validate at the `into_graph` layer — the exact place the
        // "unknown parameter 'pos_x_2'" error was raised for the old shape.
        // (A full `from_def` additionally enforces generator-boundary
        // wiring, which this minimal two-node def deliberately omits — out
        // of scope for the port-surface regression.)
        let registry = PrimitiveRegistry::with_builtin();
        let graph = def.into_graph(&registry).expect(
            "render_scene with objects=3 must accept an object_2 wire at load \
             (reconfigure runs before port validation)",
        );
        assert!(
            graph.wires().iter().any(|w| w.to.1 == "object_2"),
            "the object_2 wire survives into the built graph"
        );
    }

    /// Real held-out proof of Peter's exact case: point `MESH_SNAP_GLB` at a
    /// `.glb` with >2 distinct materials (e.g. the japanese-apricot fixture)
    /// and confirm the assembled generator loads through the production
    /// `PresetRuntime::from_def` without the `unknown parameter 'pos_x_2'`
    /// failure. `#[ignore]` + env-gated: the large held-out fixtures aren't in
    /// the tree, and this is a targeted manual gate, not CI.
    #[test]
    #[ignore = "env-gated held-out glTF; set MESH_SNAP_GLB to a >2-material .glb"]
    fn held_out_gltf_generator_loads_through_from_def() {
        let Ok(glb) = std::env::var("MESH_SNAP_GLB") else {
            println!("MESH_SNAP_GLB unset — skipping");
            return;
        };
        let (def, report) = assemble_import_graph(std::path::Path::new(&glb))
            .unwrap_or_else(|e| panic!("assemble {glb}: {e}"));
        println!("held-out import report: {report:?}");
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None).unwrap_or_else(|e| {
            panic!("held-out glTF generator failed to load through from_def: {e}")
        });
        println!("held-out glTF generator loaded clean ({} objects)", report.object_count);
    }

    // ========================================================================
    // Visual proof — render the assembled azalea graph through the real
    // production path (PresetRuntime::from_def_with_device + render()) and
    // confirm it's actually lit/textured, not just structurally valid.
    // ========================================================================

    /// Decode one IEEE-754 binary16 value to f32 (no `half` dependency).
    /// Copied from `mesh_snapshot.rs` (test-only, small, not worth a shared
    /// module for two call sites).
    #[cfg(feature = "gpu-proofs")]
    fn half_to_f32(h: u16) -> f32 {
        let sign = if (h >> 15) & 1 == 1 { -1.0f32 } else { 1.0f32 };
        let exp = (h >> 10) & 0x1f;
        let mant = h & 0x3ff;
        let mag = if exp == 0 {
            (mant as f32) * 2f32.powi(-24)
        } else if exp == 0x1f {
            if mant == 0 { f32::INFINITY } else { f32::NAN }
        } else {
            (1.0 + (mant as f32) / 1024.0) * 2f32.powi(exp as i32 - 15)
        };
        sign * mag
    }

    #[cfg(feature = "gpu-proofs")]
    /// Reinhard-tonemap an HDR channel to 8-bit: `out = (v/(1+v)).clamp(0,1)*255`.
    fn tonemap_channel(v: f32) -> u8 {
        let ldr = (v / (1.0 + v)).clamp(0.0, 1.0);
        (ldr * 255.0).round() as u8
    }

    #[cfg(feature = "gpu-proofs")]
    /// Render the assembled azalea graph through the PRODUCTION preset
    /// runtime (`PresetRuntime::from_def_with_device` + `render()`), the
    /// same path a real imported `.manifold` project's generator card
    /// takes — not the raw `Graph`/`Executor` harness `mesh_snapshot.rs`
    /// uses for its own lower-level proofs. Both `node.gltf_mesh_source`
    /// (2 objects) and `node.gltf_texture_source` (2 textures) parse on
    /// background threads, so the render is polled (bounded, with a short
    /// sleep between attempts) until the frame is actually non-black,
    /// mirroring `gltf_textured_azalea_renders_through_render_scene_to_png`'s
    /// convergence loop. Ignored by default: needs a GPU, the large
    /// fixture, and writes a file. Point `MESH_SNAP_OUT` at an absolute
    /// path to control the output location.
    #[test]
    #[ignore]
    fn imported_azalea_renders_faithfully_to_png() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        // MESH_SNAP_GLB overrides the fixture — the P2 held-out-input gate
        // (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §5 P2) points this at a
        // >1-object model NOT the azalea the transform-port swap was
        // developed against, to prove the group-placement wiring generalizes.
        let path = std::env::var("MESH_SNAP_GLB")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| azalea_fixture_path());
        if !path.exists() {
            eprintln!(
                "imported_azalea_renders_faithfully_to_png: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble import graph");
        println!("imported model report: {report:?}");

        let (w, h) = (768u32, 768u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();

        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("assembled azalea graph must build through PresetRuntime::from_def_with_device");

        let target = RenderTarget::new(&device, w, h, format, "imported-azalea");
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        // BUG-100 (see docs/BUG_BACKLOG.md): `fraction > 0.02` alone is NOT
        // a texture-decode-completion signal — the material's own `ambient`
        // floor (`node.pbr_material`'s 0.18) lights the WHOLE silhouette
        // from frame 1, regardless of whether `node.gltf_texture_source`'s
        // background-thread decode has landed yet, so the old check broke
        // out of this loop on the very first or second attempt for models
        // whose base-color texture takes longer to decode than azalea's —
        // capturing (and, before this fix, permanently asserting on) an
        // under-textured, falsely-"near-black"-looking frame. Confirmed by
        // rendering: forcing extra attempts on the exact same unmodified
        // graph turns a near-black `cc0__japanese_apricot_prunus_mume.glb`/
        // `lowe.glb` capture into a fully lit, richly textured one — the
        // geometry/lighting/material rig was never the problem.
        //
        // Real completion signal: once the texture (and any other async
        // parse) has actually landed, the render is a pure function of a
        // static camera + static geometry + static light, so consecutive
        // frames stop changing. Poll until the readback is byte-identical
        // across `STABLE_STREAK` consecutive frames (not just non-black),
        // so a still-loading frame's mid-decode churn can't look "done".
        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut fraction = 0.0f64;
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        for attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("imported-azalea-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }

            let bytes_per_row = w * 8;
            let total_bytes = u64::from(h * bytes_per_row);
            let readback_buf = device.create_buffer_shared(total_bytes);
            let mut readback_enc = device.create_encoder("imported-azalea-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();

            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            let total = (w * h) as usize;
            fraction = non_black as f64 / total as f64;

            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());

            if stable_count >= STABLE_STREAK {
                println!(
                    "imported_azalea_renders_faithfully_to_png: converged on attempt {attempt} \
                     (non-black fraction {fraction:.4}, stable for {STABLE_STREAK} frames)"
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let total = (w * h) as usize;
        println!(
            "imported_azalea_renders_faithfully_to_png: non-black pixel fraction = {fraction:.4} \
             ({max_attempts} attempts budget, total {total})"
        );
        // PNG is written BEFORE the coverage assert so a failing run still
        // leaves the frame on disk — the assert message alone can't show
        // WHERE the pixels went (tiny vs offset vs black, BUG-205's triage).
        let out_path = std::env::var("MESH_SNAP_OUT")
            .unwrap_or_else(|_| "target/mesh-snap/imported_azalea.png".to_string());
        if let Some(parent) = std::path::Path::new(&out_path).parent() {
            std::fs::create_dir_all(parent).expect("create output dir");
        }
        image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
        println!(
            "imported_azalea_renders_faithfully_to_png: wrote {out_path} (fraction {fraction:.4}, report {report:?})"
        );
        assert!(
            fraction > 0.02,
            "expected >2% non-black pixels after polling for both background parses, got \
             {fraction:.4} — likely a broken importer graph, a parse that never landed, or empty geometry"
        );
    }

    #[cfg(feature = "gpu-proofs")]
    /// Stage-4 production-path proof: render the assembled azalea graph the way
    /// a real dropped-in generator LAYER renders it — through
    /// [`GeneratorRegistry::create_with_override`] with the import graph as the
    /// per-layer override and the imported preset id as `gen_type`. This is the
    /// exact per-layer call `GeneratorRenderer::render_all` makes for a layer
    /// carrying a `generator_graph`, and it differs from
    /// `imported_azalea_renders_faithfully_to_png` (which drives
    /// `PresetRuntime::from_def_with_device` directly) in one load-bearing way:
    /// `is_watched = false` routes through the **on-demand fusion** attempt
    /// (`fused_generator_def_for`) that the raw-def path skips. So this closes
    /// the last gap — proving the imported graph survives the fuser and renders
    /// through the same code an installed timeline layer hits.
    ///
    /// It also proves the registry-gate is a non-issue: the sanitized preset id
    /// (`cc0_oomurasaki_...`) is NOT in the bundled catalog, yet the override
    /// def renders anyway because `create_with_override` builds from the def.
    #[test]
    #[ignore]
    fn imported_azalea_renders_through_create_with_override_to_png() {
        use crate::generators::registry::GeneratorRegistry;
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let path = azalea_fixture_path();
        if !path.exists() {
            eprintln!(
                "imported_azalea_renders_through_create_with_override_to_png: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble azalea");
        let preset_id = def
            .preset_metadata
            .as_ref()
            .expect("assembled def carries v2 metadata")
            .id
            .clone();
        println!(
            "create_with_override proof: preset id = {preset_id:?} (NOT in bundled catalog), report {report:?}"
        );

        let (w, h) = (768u32, 768u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = GeneratorRegistry::new(format);

        // is_watched = false -> production render path, INCLUDING the on-demand
        // fusion attempt the raw from_def proof never exercised.
        let mut generator = registry
            .create_with_override(device.arc(), &preset_id, Some(&def), w, h, false, None, None)
            .expect(
                "create_with_override must build the imported generator from its override def, \
                 even though the preset id is not in the bundled catalog",
            );

        let target = RenderTarget::new(&device, w, h, format, "imported-azalea-layer");
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        // BUG-100: same premature-convergence fix as
        // `imported_azalea_renders_faithfully_to_png` — `fraction > 0.02`
        // alone is satisfied by the material's ambient floor before the
        // texture decode lands, so require the readback to be stable
        // across `STABLE_STREAK` consecutive frames too.
        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut fraction = 0.0f64;
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        for attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("imported-azalea-layer-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }

            let bytes_per_row = w * 8;
            let total_bytes = u64::from(h * bytes_per_row);
            let readback_buf = device.create_buffer_shared(total_bytes);
            let mut readback_enc = device.create_encoder("imported-azalea-layer-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();

            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            fraction = non_black as f64 / (w * h) as f64;

            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());

            if stable_count >= STABLE_STREAK {
                println!(
                    "create_with_override proof: converged on attempt {attempt} (non-black {fraction:.4}, stable for {STABLE_STREAK} frames)"
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert!(
            fraction > 0.02,
            "expected the imported layer to render non-black through create_with_override, got \
             {fraction:.4} — a broken fusion pass, a parse that never landed, or a registry-gate regression"
        );

        let out_path = std::env::var("MESH_SNAP_OUT")
            .unwrap_or_else(|_| "target/mesh-snap/imported_azalea_layer.png".to_string());
        if let Some(parent) = std::path::Path::new(&out_path).parent() {
            std::fs::create_dir_all(parent).expect("create output dir");
        }
        image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
        println!("create_with_override proof: wrote {out_path} (fraction {fraction:.4})");
    }

    #[cfg(feature = "gpu-proofs")]
    fn box_animated_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/BoxAnimated.glb")
    }

    /// GLTF_ANIMATION_DESIGN.md A1's `duration_s`, read straight off the
    /// assembled graph's animated `node.gltf_animation_source` — so the
    /// render tests below can pick exact `(beats, seconds)` pairs that
    /// land on a specific `progress` through the DEFAULT (unwired) beat
    /// drive, without wiring a scrub source into the graph.
    #[cfg(feature = "gpu-proofs")]
    fn box_animated_duration_s() -> f32 {
        let summary = gltf_load::gltf_import_summary(&box_animated_fixture_path())
            .expect("parse BoxAnimated.glb");
        summary
            .materials
            .iter()
            .find_map(|m| m.animations.first().and_then(|a| a.as_ref()))
            .expect("BoxAnimated.glb must resolve an animation on one of its materials")
            .duration_s
    }

    /// Render-and-look finding (this session, verified with a throwaway
    /// probe test before writing this): `BoxAnimated.glb`'s "inner_box"
    /// (the only animated object) sits almost entirely INSIDE the
    /// stationary "outer_box" shell — under the importer's DEFAULT
    /// synthesized camera (a ~17-degree-above-horizon orbit shot tuned
    /// for typical hero objects) the inner box is visible only as a
    /// sliver of color peeking through a gap at the very top rim. Once
    /// its translation lifts it more than ~0.4 world units (well before
    /// progress=0.25 in this clip), it moves entirely out of that sliver
    /// and the rendered frame goes back to "no visible inner box" —
    /// IDENTICAL to every other progress where it's equally absent from
    /// the sliver. A default-camera four-phase test would (and, before
    /// this fix, DID) come back pixel-identical for 3 of the 4 phases —
    /// not a wiring bug, a camera-framing fact about this specific asset
    /// discovered by rendering and looking, not assumed. This override
    /// re-points the SAME synthesized camera near-vertical (looking down
    /// through the shell's open top — confirmed with the probe render:
    /// the inner box's full motion and rotation are clearly visible from
    /// here) via its own card bindings (`cam_tilt`/`cam_dist` — NOT the
    /// raw node params, which the binding evaluation overwrites every
    /// frame with its own default_value regardless of what the node
    /// param says). Test-only instrumentation — never a change to the
    /// production import default.
    #[cfg(feature = "gpu-proofs")]
    fn point_camera_down_to_see_inner_box(def: &mut manifold_core::effect_graph_def::EffectGraphDef) {
        let meta = def.preset_metadata.as_mut().expect("import graph carries v2 metadata");
        for b in meta.bindings.iter_mut() {
            match b.id.as_str() {
                "cam_tilt" => b.default_value = 1.4,
                "cam_dist" => b.default_value = 6.0,
                _ => {}
            }
        }
    }

    /// Render the assembled `def` at a chosen `progress` (via the
    /// `node.gltf_animation_source` default beat-drive: `progress =
    /// wrap(beats * rate / (duration_s * beats_per_second))` with
    /// `rate=1.0` and the `beats_per_second=2.0` fallback this test picks
    /// by setting `seconds = beats * 0.5` — see
    /// `gltf_animation_source::default_progress`), polling for the
    /// background mesh-parse to converge (same BUG-100 double-condition
    /// convergence loop `imported_azalea_renders_faithfully_to_png` uses).
    /// Returns the tonemapped RGBA8 buffer.
    #[cfg(feature = "gpu-proofs")]
    #[allow(clippy::too_many_arguments)]
    fn render_box_animated_at_progress(
        def: manifold_core::effect_graph_def::EffectGraphDef,
        w: u32,
        h: u32,
        progress: f32,
        duration_s: f32,
    ) -> Vec<u8> {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let beats = progress * duration_s * 2.0;
        let seconds = (beats * 0.5) as f64;

        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("BoxAnimated graph must build through PresetRuntime::from_def_with_device");
        let target = RenderTarget::new(&device, w, h, format, "box-animated");
        let ctx = PresetContext {
            time: seconds,
            beat: beats as f64,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        for _attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("box-animated-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let bytes_per_row = w * 8;
            let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut readback_enc = device.create_encoder("box-animated-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();
            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            let fraction = non_black as f64 / (w * h) as f64;
            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());
            if stable_count >= STABLE_STREAK {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        rgba
    }

    /// A1 gate item 1 (four-phase PNG goldens): `BoxAnimated.glb`
    /// imported and rendered headless at progress 0 / 0.25 / 0.5 / 0.75
    /// must produce four visibly distinct frames — the box's translation
    /// (and, past progress ~0.34, its rotation too) actually moves it
    /// across frame. Written to `tests/fixtures/gltf/goldens/` following
    /// the suite's existing `box_animated.png` naming (that file is the
    /// STATIC single-frame conformance golden from GLB_CONFORMANCE —
    /// this test's four phase-suffixed files are new, additive, and
    /// never overwrite it).
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn box_animated_four_phase_pngs_are_visibly_distinct() {
        let path = box_animated_fixture_path();
        if !path.exists() {
            eprintln!(
                "box_animated_four_phase_pngs_are_visibly_distinct: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }
        let duration_s = box_animated_duration_s();
        let (w, h) = (256u32, 256u32);
        let phases = [0.0f32, 0.25, 0.5, 0.75];
        let mut frames = Vec::new();
        for &p in &phases {
            let (mut def, _report) = assemble_import_graph(&path).expect("assemble BoxAnimated");
            point_camera_down_to_see_inner_box(&mut def);
            frames.push(render_box_animated_at_progress(def, w, h, p, duration_s));
        }

        let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/goldens");
        std::fs::create_dir_all(&out_dir).expect("create goldens dir");
        for (p, rgba) in phases.iter().zip(frames.iter()) {
            let out_path = out_dir.join(format!("box_animated_p{:03}.png", (p * 100.0).round() as u32));
            image::save_buffer(&out_path, rgba, w, h, image::ExtendedColorType::Rgba8)
                .unwrap_or_else(|e| panic!("save {}: {e}", out_path.display()));
        }

        for i in 0..frames.len() {
            for j in (i + 1)..frames.len() {
                assert_ne!(
                    frames[i], frames[j],
                    "progress {} and progress {} rendered byte-identical frames — the clip isn't animating",
                    phases[i], phases[j]
                );
            }
        }
    }

    /// A1 gate item 2 (round-trip): build the import graph, serialize it
    /// through the V1 JSON path, reload, re-render at progress 0.5, and
    /// confirm a pixel match against the pre-reload progress-0.5 render —
    /// proves `Table` params (the keyframe tracks) and the new
    /// `node.gltf_animation_source` node type survive save→reload AND
    /// stay live (not just structurally present — STANDARD §5's
    /// "modulation live after reload" gate, same doctrine as
    /// `round_trip_preserves_map_wires_and_sun_coherence_bindings`).
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn box_animated_round_trip_preserves_animation_and_renders_identically() {
        let path = box_animated_fixture_path();
        if !path.exists() {
            eprintln!(
                "box_animated_round_trip_preserves_animation_and_renders_identically: \
                 fixture not found at {}, skipping",
                path.display()
            );
            return;
        }
        let duration_s = box_animated_duration_s();
        let (w, h) = (256u32, 256u32);

        let (mut def, _report) = assemble_import_graph(&path).expect("assemble BoxAnimated");
        point_camera_down_to_see_inner_box(&mut def);
        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef =
            serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        let before = render_box_animated_at_progress(def, w, h, 0.5, duration_s);
        let after = render_box_animated_at_progress(reloaded, w, h, 0.5, duration_s);
        assert_eq!(
            before, after,
            "progress-0.5 render must pixel-match before and after a save/reload round trip"
        );
    }

    // ─── GLTF_ANIMATION_DESIGN.md A2 gate (CesiumMan/Fox skin deformation) ─

    #[cfg(feature = "gpu-proofs")]
    fn khronos_fixture_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos")
            .join(name)
    }

    /// Read `duration_s` straight off the assembled graph's
    /// `node.gltf_skeleton_pose` node (rather than recomputing it) — the
    /// same "read the built graph's own param" convention
    /// `box_animated_duration_s` uses for A1's animation source. Every
    /// object's producer nodes live INSIDE its group box (`EffectGraphNode::group`),
    /// not at the top level, so this recurses. Panics if the asset didn't
    /// resolve a skin onto any object (a real bug this test wants to
    /// catch loudly, not skip past).
    #[cfg(feature = "gpu-proofs")]
    fn skeleton_pose_duration_s(def: &manifold_core::effect_graph_def::EffectGraphDef) -> f32 {
        fn find(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> Option<f32> {
            for node in nodes {
                if node.type_id.as_str() == "node.gltf_skeleton_pose"
                    && let Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) =
                        node.params.get("duration_s")
                {
                    return Some(*value);
                }
                if let Some(group) = &node.group
                    && let Some(v) = find(&group.nodes)
                {
                    return Some(v);
                }
            }
            None
        }
        find(&def.nodes).expect("assembled graph has no node.gltf_skeleton_pose with a duration_s param")
    }

    /// Like [`skeleton_pose_duration_s`] but for the general hostile shelf
    /// (IMPORT_ANYTHING_WAVE_DESIGN.md W1 added a plain unrigged fixture —
    /// `webp_texture.glb` — alongside the skinned Mixamo-shaped ones):
    /// `0.0` when the asset has no skeleton pose at all, which
    /// `render_skinned_import_at_progress` renders as a static rest pose
    /// regardless of `progress`. The strict panicking variant stays for
    /// call sites that know their fixture is always skinned.
    #[cfg(feature = "gpu-proofs")]
    fn skeleton_pose_duration_s_or_static(
        def: &manifold_core::effect_graph_def::EffectGraphDef,
    ) -> f32 {
        fn find(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> Option<f32> {
            for node in nodes {
                if node.type_id.as_str() == "node.gltf_skeleton_pose"
                    && let Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) =
                        node.params.get("duration_s")
                {
                    return Some(*value);
                }
                if let Some(group) = &node.group
                    && let Some(v) = find(&group.nodes)
                {
                    return Some(v);
                }
            }
            None
        }
        find(&def.nodes).unwrap_or(0.0)
    }

    /// Render an assembled skinned-import `def` at a chosen `progress`
    /// (via `node.gltf_skeleton_pose`'s default beat-drive — identical
    /// formula/convergence-polling as `render_box_animated_at_progress`,
    /// generalized past the BoxAnimated-specific camera override since
    /// CesiumMan/Fox frame fine under the importer's default camera).
    #[cfg(feature = "gpu-proofs")]
    fn render_skinned_import_at_progress(
        def: manifold_core::effect_graph_def::EffectGraphDef,
        w: u32,
        h: u32,
        progress: f32,
        duration_s: f32,
    ) -> Vec<u8> {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let beats = progress * duration_s * 2.0;
        let seconds = (beats * 0.5) as f64;

        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("skinned import graph must build through PresetRuntime::from_def_with_device");
        let target = RenderTarget::new(&device, w, h, format, "skinned-import");
        let ctx = PresetContext {
            time: seconds,
            beat: beats as f64,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        for _attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("skinned-import-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let bytes_per_row = w * 8;
            let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut readback_enc = device.create_encoder("skinned-import-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();
            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            let fraction = non_black as f64 / (w * h) as f64;
            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());
            if stable_count >= STABLE_STREAK {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        rgba
    }

    /// A2 gate: `CesiumMan.glb` and `Fox.glb` — a real rigged, skinned,
    /// animated character each — must render four VISIBLY DISTINCT frames
    /// across the clip, proving the skin actually deforms (not just a
    /// rigid object moving, which A1 already proved for `BoxAnimated`).
    /// Written to `tests/fixtures/gltf/goldens/` alongside the A1 goldens.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn skinned_characters_render_four_visibly_distinct_deformed_poses() {
        let (w, h) = (256u32, 256u32);
        let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/goldens");
        std::fs::create_dir_all(&out_dir).expect("create goldens dir");

        for asset in ["CesiumMan.glb", "Fox.glb"] {
            let path = khronos_fixture_path(asset);
            if !path.exists() {
                eprintln!(
                    "skinned_characters_render_four_visibly_distinct_deformed_poses: \
                     fixture not found at {}, skipping {asset}",
                    path.display()
                );
                continue;
            }
            let (def, _report) = assemble_import_graph(&path).expect("assemble skinned import");
            let duration_s = skeleton_pose_duration_s(&def);

            let phases = [0.0f32, 0.25, 0.5, 0.75];
            let mut frames = Vec::new();
            for &p in &phases {
                let (def, _report) = assemble_import_graph(&path).expect("assemble skinned import");
                frames.push(render_skinned_import_at_progress(def, w, h, p, duration_s));
            }

            let stem = asset.trim_end_matches(".glb").to_lowercase();
            for (p, rgba) in phases.iter().zip(frames.iter()) {
                let out_path =
                    out_dir.join(format!("{stem}_skin_p{:03}.png", (p * 100.0).round() as u32));
                image::save_buffer(&out_path, rgba, w, h, image::ExtendedColorType::Rgba8)
                    .unwrap_or_else(|e| panic!("save {}: {e}", out_path.display()));
            }

            for i in 0..frames.len() {
                for j in (i + 1)..frames.len() {
                    assert_ne!(
                        frames[i], frames[j],
                        "{asset}: progress {} and progress {} rendered byte-identical frames — \
                         the skin isn't deforming",
                        phases[i], phases[j]
                    );
                }
            }
        }
    }

    /// A2 gate: hot-path check (CLAUDE.md content-thread discipline;
    /// STANDARD §5's content-thread gate) on the design doc's two NAMED
    /// gate fixtures, `CesiumMan.glb` and `Fox.glb`. Substitute for the
    /// `MANIFOLD_RENDER_TRACE`-driven `manifold-app` journey-proof harness
    /// (`bug035_verify.rs`/`bug037_verify.rs`'s pattern) — wiring a full
    /// content-thread project/layer/generator around an imported glTF
    /// asset is real additional infrastructure this phase doesn't build;
    /// this measures the actual GPU encode+submit wall-clock cost of the
    /// exact render path the gate cares about (per-frame CPU skeleton-pose
    /// sampling + the skin_mesh dispatch + render_scene), on a warm
    /// `PresetRuntime` built once, looped. `CesiumMan.glb` (14016
    /// vertices, one skin, 19 joints) is the largest single skinned mesh
    /// among the gate fixtures. Asserts no frame exceeds 20ms across a
    /// 30-frame warm loop for either asset.
    ///
    /// Re-derived finding (this session, NOT part of this gate):
    /// `BrainStem.glb` — the design doc's named joint-count stress case —
    /// is actually a MANY-SMALL-SKINS stress case (24 separate skinned
    /// objects, each only 18 joints, not one large palette) that measured
    /// a flat ~370ms/frame from frame 0 (not a one-time parse cost — see
    /// BUG-190). `CesiumMan`'s own skin_mesh dispatch alone measures
    /// ~5-6ms, so this reads as a pre-existing many-object `render_scene`
    /// scaling cost (shadow/SSAO passes × object count) rather than a
    /// skinning-specific regression, but that's not proven — logged as
    /// BUG-190 for a dedicated investigation rather than asserted here
    /// against a fixture the doc never named as a mandatory gate.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn skinned_import_hot_path_stays_under_20ms_per_frame() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        for asset in ["CesiumMan.glb", "Fox.glb"] {
            let path = khronos_fixture_path(asset);
            if !path.exists() {
                eprintln!("skinned_import_hot_path_stays_under_20ms_per_frame: fixture not found at {}, skipping {asset}", path.display());
                continue;
            }
            let (def, _report) = assemble_import_graph(&path).expect("assemble skinned import");

            let (w, h) = (512u32, 512u32);
            let device = crate::test_device();
            let format = GpuTextureFormat::Rgba16Float;
            let registry = PrimitiveRegistry::with_builtin();
            let mut generator =
                PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                    .expect("skinned import graph must build");
            let target = RenderTarget::new(&device, w, h, format, "skinned-hot-path");

            const WARMUP: u32 = 10;
            const MEASURED: u32 = 30;
            let mut max_ms = 0.0f64;
            let mut total_ms = 0.0f64;
            for frame in 0..(WARMUP + MEASURED) {
                let beats = frame as f64 * 0.1;
                let ctx = PresetContext {
                    time: beats * 0.5,
                    beat: beats,
                    dt: 1.0 / 60.0,
                    width: w,
                    height: h,
                    output_width: w,
                    output_height: h,
                    aspect: 1.0,
                    owner_key: 0,
                    is_clip_level: false,
                    frame_count: frame as i64,
                    anim_progress: 0.0,
                    trigger_count: 0,
                };
                let start = std::time::Instant::now();
                {
                    let mut enc = device.create_encoder("skinned-hot-path");
                    {
                        let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                        generator.render(
                            &mut gpu,
                            &target.texture,
                            &ctx,
                            &manifold_core::params::ParamManifest::default(),
                        );
                    }
                    enc.commit_and_wait_completed();
                }
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                if frame >= WARMUP {
                    max_ms = max_ms.max(elapsed_ms);
                    total_ms += elapsed_ms;
                }
            }
            let avg_ms = total_ms / MEASURED as f64;
            println!("{asset}: skinned-import hot path — avg {avg_ms:.2}ms, max {max_ms:.2}ms over {MEASURED} frames");
            assert!(
                max_ms < 20.0,
                "{asset}: a frame took {max_ms:.2}ms (> 20ms budget) — skinning dropped a frame"
            );
        }
    }

    // ─── GLTF_ANIMATION_DESIGN.md A3 gate (AnimatedMorphCube/MorphStressTest) ─

    /// Read `duration_s` straight off the assembled graph's
    /// `node.gltf_morph_weights` node — same "read the built graph's own
    /// param" convention `skeleton_pose_duration_s` uses for A2. Panics if
    /// the asset didn't resolve morph targets onto any object (a real bug
    /// this test wants to catch loudly, not skip past).
    #[cfg(feature = "gpu-proofs")]
    fn morph_weights_duration_s(def: &manifold_core::effect_graph_def::EffectGraphDef) -> f32 {
        fn find(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> Option<f32> {
            for node in nodes {
                if node.type_id.as_str() == "node.gltf_morph_weights"
                    && let Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) =
                        node.params.get("duration_s")
                {
                    return Some(*value);
                }
                if let Some(group) = &node.group
                    && let Some(v) = find(&group.nodes)
                {
                    return Some(v);
                }
            }
            None
        }
        find(&def.nodes).expect("assembled graph has no node.gltf_morph_weights with a duration_s param")
    }

    /// Render an assembled morphed-import `def` at a chosen `progress` (via
    /// `node.gltf_morph_weights`' default beat-drive) — identical
    /// formula/convergence-polling as `render_skinned_import_at_progress`,
    /// just against the morph weights node's own `duration_s` instead of
    /// the skeleton pose's.
    #[cfg(feature = "gpu-proofs")]
    fn render_morph_import_at_progress(
        def: manifold_core::effect_graph_def::EffectGraphDef,
        w: u32,
        h: u32,
        progress: f32,
        duration_s: f32,
    ) -> Vec<u8> {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let beats = progress * duration_s * 2.0;
        let seconds = (beats * 0.5) as f64;

        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("morphed import graph must build through PresetRuntime::from_def_with_device");
        let target = RenderTarget::new(&device, w, h, format, "morph-import");
        let ctx = PresetContext {
            time: seconds,
            beat: beats as f64,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        for _attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("morph-import-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let bytes_per_row = w * 8;
            let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut readback_enc = device.create_encoder("morph-import-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();
            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            let fraction = non_black as f64 / (w * h) as f64;
            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());
            if stable_count >= STABLE_STREAK {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        rgba
    }

    /// A3 gate (positive): `AnimatedMorphCube.glb` and `MorphStressTest.glb`
    /// — imported and rendered headless at four chosen progress values —
    /// must each produce four visibly distinct frames, proving the morph
    /// targets actually blend (the cube's face genuinely bulges/deforms)
    /// rather than producing noise or a frozen base mesh. Written to
    /// `tests/fixtures/gltf/goldens/` alongside the A1/A2 goldens.
    ///
    /// Deviation from the phase brief's literal "progress 0/0.25/0.5/0.75"
    /// (same "re-derive against the real asset" doctrine the brief's own
    /// morph_mesh-shape deviation used): re-derived this session by
    /// decoding `MorphStressTest.glb`'s `weight_tracks` output accessor —
    /// its "Individuals" clip (`animations[0]`, the only clip A3 samples)
    /// is eight SEQUENTIAL narrow pulses, one per target, each ramping
    /// 0→1→0 within roughly one keyframe interval and sitting in a
    /// near-zero valley the rest of the ~9.37s clip (peaks measured at
    /// progress ≈0.057/0.181/0.306/0.431/0.555/0.680/0.804/0.929). The
    /// evenly-spaced 0/0.25/0.5/0.75 sample points all land in valleys
    /// between pulses — a real content property, not a bug (confirmed:
    /// `node.gltf_morph_weights` correctly samples ~0 there). `AnimatedMorphCube`
    /// keeps the brief's literal four phases (its 2-target animation is a
    /// continuous ramp, not a pulse train, so they land on genuinely
    /// different weights). For `MorphStressTest`, four phases are chosen at
    /// alternating pulse PEAKS (targets 0/2/4/6) instead, preserving the
    /// gate's actual intent — proving the blend genuinely differs across
    /// the clip — rather than the literal fraction values, which would
    /// prove nothing about this asset's blending.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn morph_targets_render_four_visibly_distinct_poses() {
        let (w, h) = (256u32, 256u32);
        let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/goldens");
        std::fs::create_dir_all(&out_dir).expect("create goldens dir");

        let assets: [(&str, [f32; 4]); 2] = [
            ("AnimatedMorphCube.glb", [0.0, 0.25, 0.5, 0.75]),
            // Target 0/2/4/6 peak weight-1.0 progress values (see doc
            // comment above) — spreads across the clip landing ON pulses
            // instead of between them.
            ("MorphStressTest.glb", [0.057, 0.306, 0.555, 0.804]),
        ];

        for (asset, phases) in assets {
            let path = khronos_fixture_path(asset);
            if !path.exists() {
                eprintln!(
                    "morph_targets_render_four_visibly_distinct_poses: fixture not found at {}, \
                     skipping {asset}",
                    path.display()
                );
                continue;
            }
            let (def, _report) = assemble_import_graph(&path).expect("assemble morphed import");
            let duration_s = morph_weights_duration_s(&def);

            let mut frames = Vec::new();
            for &p in &phases {
                let (def, _report) = assemble_import_graph(&path).expect("assemble morphed import");
                frames.push(render_morph_import_at_progress(def, w, h, p, duration_s));
            }

            let stem = asset.trim_end_matches(".glb").to_lowercase();
            for (p, rgba) in phases.iter().zip(frames.iter()) {
                let out_path =
                    out_dir.join(format!("{stem}_morph_p{:03}.png", (p * 100.0).round() as u32));
                image::save_buffer(&out_path, rgba, w, h, image::ExtendedColorType::Rgba8)
                    .unwrap_or_else(|e| panic!("save {}: {e}", out_path.display()));
            }

            for i in 0..frames.len() {
                for j in (i + 1)..frames.len() {
                    assert_ne!(
                        frames[i], frames[j],
                        "{asset}: progress {} and progress {} rendered byte-identical frames — \
                         the morph targets aren't blending",
                        phases[i], phases[j]
                    );
                }
            }
        }
    }

    /// A3 gate (round-trip): build the morphed import graph, serialize it
    /// through the V1 JSON path, reload, re-render at progress 0.5, and
    /// confirm a pixel match against the pre-reload progress-0.5 render —
    /// proves the `weight_tracks` Table plus the three new node types
    /// (`node.gltf_morph_weights`, `node.gltf_morph_deltas_source`,
    /// `node.morph_targets_blend`) survive save→reload AND stay live, same
    /// doctrine as `box_animated_round_trip_preserves_animation_and_renders_identically`.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn morph_targets_round_trip_preserves_weights_and_renders_identically() {
        let path = khronos_fixture_path("AnimatedMorphCube.glb");
        if !path.exists() {
            eprintln!(
                "morph_targets_round_trip_preserves_weights_and_renders_identically: fixture not \
                 found at {}, skipping",
                path.display()
            );
            return;
        }
        let (w, h) = (256u32, 256u32);

        let (def, _report) = assemble_import_graph(&path).expect("assemble morphed import");
        let duration_s = morph_weights_duration_s(&def);
        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef =
            serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        let before = render_morph_import_at_progress(def, w, h, 0.5, duration_s);
        let after = render_morph_import_at_progress(reloaded, w, h, 0.5, duration_s);
        assert_eq!(
            before, after,
            "progress-0.5 render must pixel-match before and after a save/reload round trip"
        );
    }

    // Only used by the `#[cfg(feature = "gpu-proofs")]` render gates below —
    // gated the same way to avoid a dead-code warning on the default
    // (GPU-free) test sweep.
    #[cfg(feature = "gpu-proofs")]
    fn damaged_helmet_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/DamagedHelmet.glb")
    }

    /// Build a minimal in-memory glb: one XZ quad whose UVs live ENTIRELY
    /// outside [0,1] (V in [1.25, 1.75]) textured by an embedded 2×2 PNG —
    /// top row BLUE, bottom row RED. Under the glTF default sampler
    /// (REPEAT) V wraps to [0.25, 0.75] and samples the TOP (blue) row;
    /// under ClampToEdge every sample pins to the BOTTOM (red) row. The
    /// out-of-range-UV regression fixture for
    /// `material_maps_repeat_out_of_range_uvs`.
    #[cfg(feature = "gpu-proofs")]
    fn build_out_of_range_uv_glb() -> Vec<u8> {
        let positions: [[f32; 3]; 4] =
            [[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0]];
        let normals: [[f32; 3]; 4] = [[0.0, 1.0, 0.0]; 4];
        // V spans [1.05, 1.45]: REPEAT wraps to [0.05, 0.45] — entirely
        // inside the TOP (blue) row of the 2×2 texture. Clamp pins to
        // V = 1.0, the BOTTOM (red) edge row. (First cut used [1.25, 1.75],
        // which wraps across BOTH rows and reads mixed — a fixture bug,
        // not a sampler bug.)
        let uvs: [[f32; 2]; 4] = [[0.25, 1.05], [0.75, 1.05], [0.75, 1.45], [0.25, 1.45]];
        let indices: [u16; 6] = [0, 2, 1, 0, 3, 2];

        // 2×2 PNG: row 0 (top) blue, row 1 red.
        let mut png = Vec::new();
        {
            use image::ImageEncoder;
            let pixels: [u8; 16] =
                [0, 0, 255, 255, 0, 0, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255];
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(&pixels, 2, 2, image::ExtendedColorType::Rgba8)
                .expect("encode fixture png");
        }

        let mut bin: Vec<u8> = Vec::new();
        let pos_off = bin.len();
        bin.extend(positions.iter().flatten().flat_map(|f| f.to_le_bytes()));
        let norm_off = bin.len();
        bin.extend(normals.iter().flatten().flat_map(|f| f.to_le_bytes()));
        let uv_off = bin.len();
        bin.extend(uvs.iter().flatten().flat_map(|f| f.to_le_bytes()));
        let idx_off = bin.len();
        bin.extend(indices.iter().flat_map(|i| i.to_le_bytes()));
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let png_off = bin.len();
        bin.extend_from_slice(&png);
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }

        let json = serde_json::json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0}],
            "meshes": [{"primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
                "indices": 3,
                "material": 0
            }]}],
            "materials": [{"pbrMetallicRoughness": {
                "baseColorTexture": {"index": 0},
                "metallicFactor": 0.0,
                "roughnessFactor": 1.0
            }}],
            "textures": [{"source": 0, "sampler": 0}],
            "samplers": [{}],
            "images": [{"bufferView": 4, "mimeType": "image/png"}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3",
                 "min": [-1.0, 0.0, -1.0], "max": [1.0, 0.0, 1.0]},
                {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC2"},
                {"bufferView": 3, "componentType": 5123, "count": 6, "type": "SCALAR"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteOffset": pos_off, "byteLength": 48},
                {"buffer": 0, "byteOffset": norm_off, "byteLength": 48},
                {"buffer": 0, "byteOffset": uv_off, "byteLength": 32},
                {"buffer": 0, "byteOffset": idx_off, "byteLength": 12},
                {"buffer": 0, "byteOffset": png_off, "byteLength": png.len()}
            ],
            "buffers": [{"byteLength": bin.len()}]
        });
        let mut json_bytes = serde_json::to_vec(&json).expect("fixture json");
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }

        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin);
        glb
    }

    /// Regression gate for the 2026-07-15 striped-helmet bug: material maps
    /// must sample with REPEAT wrapping (the glTF default sampler), not the
    /// envmap's clamp-V. The fixture quad's V coords are entirely in
    /// [1.25, 1.75]: REPEAT reads the texture's blue top row; the broken
    /// clamp pinned every sample to the red bottom edge row. Asserts the
    /// rendered quad is blue-dominant. Run deliberately (gpu-proofs).
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn material_maps_repeat_out_of_range_uvs() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let glb = build_out_of_range_uv_glb();
        let path = std::env::temp_dir().join("manifold_uv_wrap_regression.glb");
        std::fs::write(&path, &glb).expect("write temp fixture");

        let (def, _report) = assemble_import_graph(&path).expect("assemble uv-wrap fixture");

        let (w, h) = (128u32, 128u32);
        let device = crate::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator = PresetRuntime::from_def_with_device(
            def,
            &registry,
            device.arc(),
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("uv-wrap fixture builds through PresetRuntime");
        let target = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "uv-wrap");
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        // Poll until the background mesh+texture decodes land (byte-stable,
        // non-black — the BUG-100 double condition).
        let mut rgb_sum = [0.0f32; 3];
        let mut prev: Option<Vec<u8>> = None;
        let mut stable = 0u32;
        let mut converged = false;
        for _ in 0..200 {
            {
                let mut enc = device.create_encoder("uv-wrap-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let bytes_per_row = w * 8;
            let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut renc = device.create_encoder("uv-wrap-readback");
            renc.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
            renc.commit_and_wait_completed();
            let ptr = buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            rgb_sum = [0.0; 3];
            let mut raw = Vec::with_capacity(halves.len() * 2);
            for px in halves.chunks_exact(4) {
                rgb_sum[0] += half_to_f32(px[0]).max(0.0);
                rgb_sum[1] += half_to_f32(px[1]).max(0.0);
                rgb_sum[2] += half_to_f32(px[2]).max(0.0);
                raw.extend(px.iter().flat_map(|v| v.to_le_bytes()));
            }
            let non_black = rgb_sum.iter().sum::<f32>() > 1.0;
            if non_black && prev.as_deref() == Some(raw.as_slice()) {
                stable += 1;
            } else {
                stable = 0;
            }
            prev = Some(raw);
            if stable >= 3 {
                converged = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(converged, "uv-wrap fixture render never stabilized non-black");
        assert!(
            rgb_sum[2] > rgb_sum[0] * 2.0,
            "out-of-range V must WRAP to the blue top row, not clamp to the red \
             bottom edge: sum RGB = {rgb_sum:?}"
        );
    }

    #[cfg(feature = "gpu-proofs")]
    fn amg_gt3_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb")
    }

    /// F-P4 held-out-input gate (DESIGN_DOC_STANDARD.md §5): Khronos
    /// `DamagedHelmet.glb` (CC-BY 4.0 — attribution in
    /// `tests/fixtures/gltf/README.md`) carries all five glTF PBR map types
    /// and was never used to develop this importer — the fixture-overfitting
    /// check. Must import with every one of F-P2's four new map ports wired
    /// (asserted by port name), render headless through the real
    /// `PresetRuntime::from_def_with_device` + `render()` path without error,
    /// and produce a non-degenerate frame: mean luminance strictly between
    /// 0.02 and 0.98 (catches both an all-black failure — e.g. a stuck
    /// background decode — and a blown-out one — e.g. a light-leak from a
    /// broken IBL term — without judging the LOOK, which is Peter's L4 call).
    /// Needs a GPU device: run deliberately with `--features gpu-proofs`.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_core::flatten::flatten_groups;
        use manifold_gpu::GpuTextureFormat;

        let path = damaged_helmet_fixture_path();
        if !path.exists() {
            eprintln!(
                "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: fixture not \
                 found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble DamagedHelmet");
        println!("DamagedHelmet import report: {report:?}");
        assert_eq!(report.object_count, 1, "DamagedHelmet is a single-material model");

        // Every one of F-P2's four new map ports must be wired, by name —
        // not just "the import didn't error". SCENE_OBJECT_AND_PANEL_V2_DESIGN
        // D1/D3: these wire into the object's `node.scene_object` bind node
        // now, not directly into `render` — `render` itself only ever sees
        // the single `object_0` port.
        let flat = flatten_groups(&def).expect("DamagedHelmet import graph flattens");
        let scene_object_id =
            flat.nodes.iter().find(|n| n.type_id == "node.scene_object").expect("scene_object bind node").id;
        let scene_object_ports: std::collections::HashSet<String> = flat
            .wires
            .iter()
            .filter(|w| w.to_node == scene_object_id)
            .map(|w| w.to_port.clone())
            .collect();
        for port in ["base_color_map", "normal_map", "mr_map", "occlusion_map", "emissive_map"] {
            assert!(
                scene_object_ports.contains(port),
                "DamagedHelmet must wire `{port}` — carries all five glTF PBR maps; got {scene_object_ports:?}"
            );
        }

        let (w, h) = (512u32, 512u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("DamagedHelmet import graph must build through PresetRuntime::from_def_with_device");
        let target = RenderTarget::new(&device, w, h, format, "damaged-helmet");
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        // Same convergence-polling loop as `imported_azalea_renders_faithfully_to_png`
        // — background texture/mesh decodes need to land before the readback
        // means anything. Byte-identical across STABLE_STREAK consecutive
        // frames is the completion signal, but (BUG-100) byte-identical
        // alone isn't enough: DamagedHelmet wires FIVE background texture
        // decodes (base-color/normal/mr/occlusion/emissive, each its own
        // `node.gltf_texture_source` background thread), and
        // `node.gltf_texture_source` emits solid black on every frame until
        // its own decode lands (see that primitive's `run()` step 6) — so a
        // frame where every wired source is STILL mid-decode is *also*
        // byte-stable (three identical black frames) and would falsely read
        // as "converged" before any decode actually finished. Require
        // `fraction > 0.02` (measured, non-black) alongside byte-stability,
        // exactly like the azalea proof's own convergence check.
        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        let mut converged = false;
        let mut fraction = 0.0f64;
        for attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("damaged-helmet-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }

            let bytes_per_row = w * 8;
            let total_bytes = u64::from(h * bytes_per_row);
            let readback_buf = device.create_buffer_shared(total_bytes);
            let mut readback_enc = device.create_encoder("damaged-helmet-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();

            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            fraction = non_black as f64 / (w * h) as f64;

            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());

            if stable_count >= STABLE_STREAK {
                println!(
                    "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: converged \
                     on attempt {attempt} (non-black fraction {fraction:.4})"
                );
                converged = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            converged,
            "DamagedHelmet render never stabilized non-black after {max_attempts} attempts \
             (last non-black fraction {fraction:.4}) — a background texture decode may be stuck"
        );

        // Mean luminance (Rec. 601 luma over the tonemapped LDR frame),
        // strictly between 0.02 and 0.98 — non-degenerate without judging
        // the look.
        let mut sum_luma = 0.0f64;
        let pixel_count = (w * h) as usize;
        for px in rgba.chunks_exact(4) {
            let (r, g, b) = (px[0] as f64 / 255.0, px[1] as f64 / 255.0, px[2] as f64 / 255.0);
            sum_luma += 0.299 * r + 0.587 * g + 0.114 * b;
        }
        let mean_luminance = sum_luma / pixel_count as f64;
        println!(
            "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: mean luminance = {mean_luminance:.4}"
        );

        let out_path = std::env::var("MESH_SNAP_OUT")
            .unwrap_or_else(|_| "target/mesh-snap/damaged_helmet.png".to_string());
        if let Some(parent) = std::path::Path::new(&out_path).parent() {
            std::fs::create_dir_all(parent).expect("create output dir");
        }
        image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {out_path}: {e}"));

        assert!(
            mean_luminance.is_finite() && mean_luminance > 0.02 && mean_luminance < 0.98,
            "expected non-degenerate mean luminance in (0.02, 0.98), got {mean_luminance:.4}"
        );
    }

    /// Peter-facing look-check sanity, NOT a machine gate (the AMG fixture
    /// is untracked, licensing-unverified — vecarz — and absent in a fresh
    /// checkout, per the design's explicit "stays untracked" call). Skips
    /// cleanly when the file is absent so it never fails CI; when present,
    /// only proves the import + render pipeline doesn't error — the actual
    /// look (chrome + void + glow) is Peter's in-app L4 check.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn amg_gt3_glb_imports_and_renders_without_error_if_present() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let path = amg_gt3_fixture_path();
        if !path.exists() {
            println!(
                "amg_gt3_glb_imports_and_renders_without_error_if_present: fixture not tracked, \
                 skipping (expected in a fresh checkout — vecarz licensing unverified)"
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble AMG GT3");
        println!("AMG GT3 import report: {report:?}");
        // GLB_CONFORMANCE_DESIGN.md G-P2 conformance gate: the AMG GT3 has
        // 78 materials with geometry; with the cap dead, ALL of them must be
        // wired (pre-G-P2 this was 64, dropping the livery — BUG-163).
        assert_eq!(
            report.object_count, 78,
            "AMG GT3 import must be 1:1 — 78 materials, 78 objects, no cap-drop (BUG-163)"
        );
        assert_eq!(report.material_count, 78);

        let (w, h) = (512u32, 512u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("AMG GT3 import graph must build through PresetRuntime::from_def_with_device");
        let target = RenderTarget::new(&device, w, h, format, "amg-gt3-sanity");
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = device.create_encoder("amg-gt3-sanity-render");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            generator.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
        println!("amg_gt3_glb_imports_and_renders_without_error_if_present: rendered without error");
    }

    // ── SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P3 held-out gate: the_rosetta_stone.glb ──
    // A fixture v1's briefs never used for emission tests (per the P3 phase
    // brief) — proves the importer's NEW scene_object-shaped emission on an
    // asset none of the earlier scene_object work was tuned against.

    fn rosetta_stone_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/the_rosetta_stone.glb")
    }

    /// CPU-only, in the default sweep: every imported object row is
    /// scene_object-shaped (an `object_k` wire whose producer resolves,
    /// through at most one group hop, to a `node.scene_object`), and a
    /// fresh import needs no migration —
    /// `migrate_scene_object_wires` must return `false` on the assembled
    /// def, proving the importer emits the target shape natively rather
    /// than relying on the load-time migration to paper over legacy wires.
    #[test]
    fn rosetta_stone_imports_scene_object_shaped_with_no_migration_needed() {
        let path = rosetta_stone_fixture_path();
        if !path.exists() {
            println!(
                "rosetta_stone_imports_scene_object_shaped_with_no_migration_needed: fixture not \
                 found at {}, skipping",
                path.display()
            );
            return;
        }

        let (mut def, report) = assemble_import_graph(&path).expect("assemble the_rosetta_stone");
        println!("the_rosetta_stone import report: object_count={}", report.object_count);
        assert!(report.object_count >= 1, "the_rosetta_stone must import at least one object");

        let render = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .expect("render_scene node present");
        let render_id = render.id;
        let objects = report.object_count;
        for k in 0..objects {
            let object_port = format!("object_{k}");
            let producer_id = def
                .wires
                .iter()
                .find(|w| w.to_node == render_id && w.to_port == object_port)
                .unwrap_or_else(|| panic!("object {k}: no wire into render_scene's `{object_port}`"))
                .from_node;
            let producer = def.nodes.iter().find(|n| n.id == producer_id).expect("producer node exists");
            let is_scene_object_shaped = producer.type_id == "node.scene_object"
                || (producer.type_id == GROUP_TYPE_ID
                    && producer
                        .group
                        .as_ref()
                        .is_some_and(|g| g.nodes.iter().any(|n| n.type_id == "node.scene_object")));
            assert!(
                is_scene_object_shaped,
                "object {k}: producer (type_id={}) must be node.scene_object or a group containing one",
                producer.type_id
            );
        }

        // Negative: zero legacy per-object port wires anywhere on render_scene.
        for prefix in [
            "mesh_", "material_", "transform_", "base_color_map_", "normal_map_", "mr_map_",
            "occlusion_map_", "emissive_map_", "instances_",
        ] {
            assert!(
                !def.wires.iter().any(|w| w.to_node == render_id && w.to_port.starts_with(prefix)),
                "the_rosetta_stone must wire zero legacy `{prefix}*` ports into render_scene"
            );
        }

        assert!(
            !manifold_core::scene_object_migration::migrate_scene_object_wires(&mut def),
            "a fresh the_rosetta_stone import must already be scene_object-shaped — \
             migrate_scene_object_wires should be a no-op"
        );
    }

    /// GPU render proof: the_rosetta_stone import must actually draw non-
    /// degenerate content and be saved to disk for visual inspection.
    /// Needs a GPU device: run deliberately with `--features gpu-proofs`.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn rosetta_stone_import_renders_gpu_proof() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let path = rosetta_stone_fixture_path();
        if !path.exists() {
            eprintln!(
                "rosetta_stone_import_renders_gpu_proof: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble the_rosetta_stone");
        println!("the_rosetta_stone import report: object_count={}", report.object_count);

        let (w, h) = (512u32, 512u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("the_rosetta_stone import graph must build through PresetRuntime::from_def_with_device");
        let target = RenderTarget::new(&device, w, h, format, "rosetta-stone");
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        // Same convergence-polling loop as the DamagedHelmet/AMG proofs
        // above — background texture decodes need to land before the
        // readback means anything.
        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        let mut converged = false;
        let mut fraction = 0.0f64;
        for attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("rosetta-stone-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }

            let bytes_per_row = w * 8;
            let total_bytes = u64::from(h * bytes_per_row);
            let readback_buf = device.create_buffer_shared(total_bytes);
            let mut readback_enc = device.create_encoder("rosetta-stone-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();

            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            fraction = non_black as f64 / (w * h) as f64;

            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());

            if stable_count >= STABLE_STREAK {
                println!(
                    "rosetta_stone_import_renders_gpu_proof: converged on attempt {attempt} \
                     (non-black fraction {fraction:.4})"
                );
                converged = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            converged,
            "the_rosetta_stone render never stabilized non-black after {max_attempts} attempts \
             (last non-black fraction {fraction:.4})"
        );

        let out_path = std::env::var("MESH_SNAP_OUT")
            .unwrap_or_else(|_| "target/mesh-snap/the_rosetta_stone.png".to_string());
        if let Some(parent) = std::path::Path::new(&out_path).parent() {
            std::fs::create_dir_all(parent).expect("create output dir");
        }
        image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
        println!("rosetta_stone_import_renders_gpu_proof: wrote {out_path}");
    }

    /// One frame, no convergence loop (this is a look-check demo, not a
    /// numeric gate) — renders `def` at time 0 into a fresh RGBA buffer.
    #[cfg(feature = "gpu-proofs")]
    fn render_once(def: EffectGraphDef, w: u32, h: u32, label: &str) -> Vec<u8> {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .unwrap_or_else(|e| panic!("{label}: import graph must build: {e:?}"));
        let target = RenderTarget::new(&device, w, h, format, label);
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut rgba = Vec::new();
        // A few frames for background texture decodes to land (no
        // stability-polling needed for a demo, just enough headroom).
        for _ in 0..30 {
            let mut enc = device.create_encoder(label);
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                generator.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();

            let bytes_per_row = w * 8;
            let readback_buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut readback_enc = device.create_encoder(label);
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();
            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            rgba = Vec::with_capacity((w * h * 4) as usize);
            for px in halves.chunks_exact(4) {
                rgba.push(tonemap_channel(half_to_f32(px[0])));
                rgba.push(tonemap_channel(half_to_f32(px[1])));
                rgba.push(tonemap_channel(half_to_f32(px[2])));
                rgba.push((half_to_f32(px[3]).clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        rgba
    }

    /// Demo pair for the P3 gate's L2 artifact: the_rosetta_stone alone,
    /// then the_rosetta_stone plus a duplicate of its (sole) object offset
    /// by D11's +0.5 on `pos_x` — mirroring `DuplicateSceneObjectCommand`'s
    /// shape structurally (deep-clone the producer subtree with fresh doc
    /// ids, rewire internal wires, wire the clone's `object` output to the
    /// next free `object_k`, bump `objects`) without depending on
    /// `manifold-editing` from this crate — the actual command's
    /// correctness (fresh-id freshness, undo, D6 handle sync) is proven by
    /// its own inverse-pair unit tests in `manifold-editing`; this test
    /// proves the RENDER characteristics of the shape it produces.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn duplicate_demo_pair_renders_original_then_original_plus_offset_copy() {
        let path = rosetta_stone_fixture_path();
        if !path.exists() {
            eprintln!(
                "duplicate_demo_pair_renders_original_then_original_plus_offset_copy: fixture not \
                 found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, _report) = assemble_import_graph(&path).expect("assemble the_rosetta_stone");
        let (w, h) = (512u32, 512u32);

        let original_rgba = render_once(def.clone(), w, h, "rosetta-original");
        let out_dir = std::env::var("MESH_SNAP_OUT_DIR").unwrap_or_else(|_| "target/mesh-snap".to_string());
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let original_path = format!("{out_dir}/rosetta_duplicate_demo_original.png");
        image::save_buffer(&original_path, &original_rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {original_path}: {e}"));

        // Build the "plus a duplicate" def: clone the sole object's producer
        // subtree with fresh doc ids (mirrors
        // manifold_editing::commands::graph::deep_clone_with_fresh_ids /
        // DuplicateSceneObjectCommand), offset transform_3d.pos_x by +0.5,
        // wire to the next object_k slot, bump `objects`.
        let mut plus_def = def.clone();
        let render_id = plus_def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .expect("render_scene node")
            .id;
        let producer_id = plus_def
            .wires
            .iter()
            .find(|w| w.to_node == render_id && w.to_port == "object_0")
            .expect("object_0 wire present")
            .from_node;
        let source_index = plus_def.nodes.iter().position(|n| n.id == producer_id).expect("producer node");

        fn max_id(nodes: &[EffectGraphNode]) -> u32 {
            nodes
                .iter()
                .map(|n| n.id.max(n.group.as_ref().map(|g| max_id(&g.nodes)).unwrap_or(0)))
                .max()
                .unwrap_or(0)
        }
        fn collect_all_handles(nodes: &[EffectGraphNode], out: &mut std::collections::HashSet<String>) {
            for n in nodes {
                if let Some(h) = &n.handle {
                    out.insert(h.clone());
                }
                if let Some(body) = n.group.as_deref() {
                    collect_all_handles(&body.nodes, out);
                }
            }
        }
        // Mirrors `dedup_handle` in
        // `manifold_editing::commands::graph` — `base`, else `base_2`,
        // `base_3`, … (this crate has no dependency on manifold-editing,
        // so the tiny helper is duplicated rather than shared).
        fn dedup_handle(base: &str, taken: &mut std::collections::HashSet<String>) -> String {
            if !taken.contains(base) {
                taken.insert(base.to_string());
                return base.to_string();
            }
            let mut i = 2u32;
            loop {
                let cand = format!("{base}_{i}");
                if !taken.contains(&cand) {
                    taken.insert(cand.clone());
                    return cand;
                }
                i += 1;
            }
        }
        // Mirrors `deep_clone_with_fresh_ids` in
        // `manifold_editing::commands::graph::DuplicateSceneObjectCommand` —
        // fresh doc id + fresh NodeId + deduped handle on every node,
        // recursively. `Graph::add_node_named` rejects a duplicate handle
        // anywhere in the whole graph, so a clone whose inner nodes kept
        // their source's exact handles (`mesh_0`, `mat_0`, …) fails to
        // build even with fresh ids everywhere else. `node_id_map` (BUG-212)
        // mirrors the production fix: collects every (old, new) stable
        // NodeId pair across the whole subtree so the caller can re-target
        // `string_bindings` entries onto the clone's fresh ids.
        fn clone_fresh(
            src: &EffectGraphNode,
            next_id: &mut u32,
            taken: &mut std::collections::HashSet<String>,
            node_id_map: &mut Vec<(manifold_core::NodeId, manifold_core::NodeId)>,
        ) -> EffectGraphNode {
            let mut node = src.clone();
            node.id = *next_id;
            *next_id += 1;
            let old_node_id = node.node_id.clone();
            node.node_id = manifold_core::NodeId::new(manifold_core::short_id());
            node_id_map.push((old_node_id, node.node_id.clone()));
            node.handle = node.handle.as_deref().map(|h| dedup_handle(h, taken));
            if let Some(group) = node.group.as_deref_mut() {
                let mut id_map: Vec<(u32, u32)> = Vec::new();
                let mut new_nodes = Vec::new();
                for n in &group.nodes {
                    let old = n.id;
                    let cloned = clone_fresh(n, next_id, taken, node_id_map);
                    id_map.push((old, cloned.id));
                    new_nodes.push(cloned);
                }
                let remap = |id: u32| id_map.iter().find(|(o, _)| *o == id).map(|(_, n)| *n).unwrap_or(id);
                group.wires = group
                    .wires
                    .iter()
                    .map(|w| EffectGraphWire {
                        from_node: remap(w.from_node),
                        from_port: w.from_port.clone(),
                        to_node: remap(w.to_node),
                        to_port: w.to_port.clone(),
                    })
                    .collect();
                group.nodes = new_nodes;
            }
            node
        }

        let mut next_id = max_id(&plus_def.nodes) + 1;
        let source_node = plus_def.nodes[source_index].clone();
        let mut taken = std::collections::HashSet::new();
        collect_all_handles(&plus_def.nodes, &mut taken);
        let mut node_id_map: Vec<(manifold_core::NodeId, manifold_core::NodeId)> = Vec::new();
        let mut clone = clone_fresh(&source_node, &mut next_id, &mut taken, &mut node_id_map);
        // D11's exact top-level convention (handle + " 2"), derived from the
        // SOURCE's own handle (not the post-dedup one — see the identical
        // comment on `DuplicateSceneObjectCommand::execute`).
        let cloned_handle = source_node.handle.as_ref().map(|h| format!("{h} 2"));
        clone.handle = cloned_handle.clone();
        if let Some(body) = clone.group.as_deref_mut()
            && let Some(inner_object) = body.nodes.iter_mut().find(|n| n.type_id == "node.scene_object")
        {
            inner_object.handle = cloned_handle;
        }
        clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));

        // BUG-212 fix (mirrors `DuplicateSceneObjectCommand`'s shipped fix,
        // not a demo-only workaround): `string_bindings` (the "Model File" →
        // mesh-source `path` binding every importer object carries)
        // addresses its target by stable NodeId, which `clone_fresh` just
        // minted fresh for every cloned node (D11). Clone every
        // `string_bindings` entry whose target falls inside the duplicated
        // subtree (per `node_id_map`, collected above), re-targeted at the
        // clone's fresh NodeId, same `id`/`label`/`default_value` — D11's
        // "fresh NodeIds make cloned bindings dangle" is a deliberate
        // tradeoff for CARD exposes (`bindings`/`exposed_params`), not for
        // this non-performer-facing importer plumbing.
        if let Some(meta) = plus_def.preset_metadata.as_mut() {
            let new_entries: Vec<manifold_core::effect_graph_def::StringBindingDef> = meta
                .string_bindings
                .iter()
                .filter_map(|b| match &b.target {
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => node_id_map
                        .iter()
                        .find(|(old, _)| old == node_id)
                        .map(|(_, new_id)| manifold_core::effect_graph_def::StringBindingDef {
                            id: b.id.clone(),
                            label: b.label.clone(),
                            default_value: b.default_value.clone(),
                            target: manifold_core::effect_graph_def::BindingTarget::Node {
                                node_id: new_id.clone(),
                                param: param.clone(),
                            },
                        }),
                    manifold_core::effect_graph_def::BindingTarget::Composite { .. } => None,
                })
                .collect();
            meta.string_bindings.extend(new_entries);
        }

        if let Some(body) = clone.group.as_deref_mut()
            && let Some(transform_node) = body.nodes.iter_mut().find(|n| n.type_id == "node.transform_3d")
        {
            let cur = match transform_node.params.get("pos_x") {
                Some(SerializedParamValue::Float { value }) => *value,
                _ => 0.0,
            };
            transform_node
                .params
                .insert("pos_x".to_string(), SerializedParamValue::Float { value: cur + 0.5 });
        }
        let clone_id = clone.id;
        plus_def.nodes.push(clone);
        plus_def
            .wires
            .push(wire(clone_id, "object", render_id, "object_1"));
        plus_def
            .nodes
            .iter_mut()
            .find(|n| n.id == render_id)
            .unwrap()
            .params
            .insert("objects".to_string(), SerializedParamValue::Float { value: 2.0 });

        let plus_rgba = render_once(plus_def, w, h, "rosetta-plus-duplicate");
        let plus_path = format!("{out_dir}/rosetta_duplicate_demo_plus_copy.png");
        image::save_buffer(&plus_path, &plus_rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {plus_path}: {e}"));

        assert_ne!(
            original_rgba, plus_rgba,
            "the duplicate-plus-offset render must differ from the original (a second, offset copy is visible)"
        );
        println!(
            "duplicate_demo_pair_renders_original_then_original_plus_offset_copy: wrote \
             {original_path} and {plus_path}"
        );
    }

    // ── BUG-221 render proofs: composed per-object recenter ──
    //
    // Both tests below reconstruct the PRE-fix graph from the CURRENT
    // (post-fix) `assemble_import_graph` output, rather than duplicating
    // the old formula by hand: per-object recenter now splits into
    // `mesh_k.translate_* = -own_center` and `transform_k.pos_* =
    // own_center - center`, and BUG-221's own fix guarantees
    // `translate + pos == -center` (the old whole-scene-only recenter) —
    // proven independently, at value level, by
    // `bug221_object_transform_recenters_about_own_bbox_center_not_scene_center`
    // above. Reconstructing pre-fix behavior as `translate=0, pos =
    // translate_old + pos_old` is therefore a faithful mechanical inverse
    // of the fix, not a hand-typed duplicate of old code that could drift.

    #[cfg(feature = "gpu-proofs")]
    fn emissive_strength_test_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/EmissiveStrengthTest.glb")
    }

    /// Rebuild every object group's `mesh_k`/`transform_k` pair back to the
    /// PRE-BUG-221 shape: `mesh_k.translate_* = 0`, `transform_k.pos_* =
    /// (old mesh translate) + (old transform pos)`. See the module comment
    /// above this function for why this reconstruction is faithful.
    #[cfg(feature = "gpu-proofs")]
    fn reconstruct_pre_bug221_fix(def: &EffectGraphDef) -> EffectGraphDef {
        let mut out = def.clone();
        for node in &mut out.nodes {
            let Some(body) = node.group.as_mut() else { continue };
            let mesh_handles: Vec<String> = body
                .nodes
                .iter()
                .filter_map(|n| n.handle.clone())
                .filter(|h| h.starts_with("mesh_"))
                .collect();
            for mesh_handle in mesh_handles {
                let k = &mesh_handle["mesh_".len()..];
                let transform_handle = format!("transform_{k}");
                let Some(mi) = body.nodes.iter().position(|n| n.handle.as_deref() == Some(mesh_handle.as_str()))
                else {
                    continue;
                };
                let Some(ti) =
                    body.nodes.iter().position(|n| n.handle.as_deref() == Some(transform_handle.as_str()))
                else {
                    continue;
                };
                for (mesh_param, transform_param) in
                    [("translate_x", "pos_x"), ("translate_y", "pos_y"), ("translate_z", "pos_z")]
                {
                    let t = match body.nodes[mi].params.get(mesh_param) {
                        Some(SerializedParamValue::Float { value }) => *value,
                        _ => 0.0,
                    };
                    let p = match body.nodes[ti].params.get(transform_param) {
                        Some(SerializedParamValue::Float { value }) => *value,
                        _ => 0.0,
                    };
                    body.nodes[mi].params.insert(mesh_param.to_string(), float(0.0));
                    body.nodes[ti].params.insert(transform_param.to_string(), float(t + p));
                }
            }
        }
        out
    }

    /// Mean absolute per-channel difference between two same-sized,
    /// already-tonemapped RGBA8 buffers (`render_once`'s output format) —
    /// same metric shape as `scene_object_migration_round_trip.rs`'s.
    #[cfg(feature = "gpu-proofs")]
    fn mean_abs_diff_u8(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let sum: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs() / 255.0).sum();
        sum / a.len() as f64
    }

    /// BUG-221 layout-preservation gate: `EmissiveStrengthTest.glb` (6
    /// distinct objects laid out in a world-space row, `translation.x`
    /// from -6 to +6 — a real multi-object asset whose per-object
    /// `own_center` differs substantially from the whole-scene center,
    /// confirmed by direct measurement during triage) rendered at default
    /// params (no rotation) must look the SAME whether every object's
    /// pivot lives at its own visual center (post-fix, current code) or at
    /// the shared whole-scene origin (pre-fix, reconstructed) — the fix is
    /// a pivot-only change, never a placement change.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bug221_layout_preserved_before_and_after_fix() {
        let path = emissive_strength_test_fixture_path();
        if !path.exists() {
            eprintln!("bug221_layout_preserved_before_and_after_fix: fixture not found at {}, skipping", path.display());
            return;
        }
        let (post_def, _report) = assemble_import_graph(&path).expect("assemble EmissiveStrengthTest");
        let pre_def = reconstruct_pre_bug221_fix(&post_def);

        let (w, h) = (512u32, 512u32);
        let post_rgba = render_once(post_def, w, h, "bug221-post-fix-no-rotation");
        let pre_rgba = render_once(pre_def, w, h, "bug221-pre-fix-no-rotation");

        let out_dir = std::env::var("MESH_SNAP_OUT_DIR").unwrap_or_else(|_| "target/mesh-snap".to_string());
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let post_path = format!("{out_dir}/bug221_layout_post_fix.png");
        let pre_path = format!("{out_dir}/bug221_layout_pre_fix.png");
        image::save_buffer(&post_path, &post_rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {post_path}: {e}"));
        image::save_buffer(&pre_path, &pre_rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {pre_path}: {e}"));

        let diff = mean_abs_diff_u8(&post_rgba, &pre_rgba);
        eprintln!("bug221_layout_preserved_before_and_after_fix: mean_abs_diff = {diff:.6}");
        assert!(
            diff < 0.01,
            "BUG-221's fix must not change net world placement — pre-fix and post-fix renders \
             at default (no-rotation) params should be pixel-comparable, got mean_abs_diff={diff:.6} \
             (wrote {pre_path}, {post_path})"
        );
    }

    /// BUG-221 pivot-behavior gate: a 45° `rot_y` applied to ONLY the
    /// single object whose `own_center` sits farthest from the whole-scene
    /// center (found the same way the summary-scan triage did —
    /// `EmissiveStrengthTest.glb`'s cubes sit at world X from -6 to +6,
    /// each translated away from the origin by its own glTF node, so one
    /// of the five cube objects has a large own_center/scene-center
    /// offset). Rotating only that one object (not the shared backdrop
    /// mesh, which spans the whole scene and would confound the
    /// comparison by visibly swinging in BOTH renders regardless of the
    /// fix) isolates BUG-221's exact claim: pre-fix, that object's pivot
    /// is the shared whole-scene origin, so a 45° spin swings it well out
    /// of its original footprint; post-fix, its pivot is its own visual
    /// center, so the same 45° spin rotates it in place.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bug221_pivot_spins_in_place_after_fix_but_not_before() {
        let path = emissive_strength_test_fixture_path();
        if !path.exists() {
            eprintln!("bug221_pivot_spins_in_place_after_fix_but_not_before: fixture not found at {}, skipping", path.display());
            return;
        }
        let summary = gltf_load::gltf_import_summary(&path).expect("parse EmissiveStrengthTest for offset scan");
        let center = [
            (summary.bbox_min[0] + summary.bbox_max[0]) * 0.5,
            (summary.bbox_min[1] + summary.bbox_max[1]) * 0.5,
            (summary.bbox_min[2] + summary.bbox_max[2]) * 0.5,
        ];
        // Same largest-vertex-count-first sort `build_import_graph` uses,
        // so this index lines up with the `transform_{k}`/`mesh_{k}`
        // handles in the built def.
        let mut materials = summary.materials.clone();
        materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
        let (target_k, target) = materials
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                let mag = |m: &super::gltf_load::GltfMaterialInfo| {
                    let d = [m.own_center[0] - center[0], m.own_center[1] - center[1], m.own_center[2] - center[2]];
                    d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
                };
                mag(a).partial_cmp(&mag(b)).unwrap()
            })
            .expect("EmissiveStrengthTest has at least one material");
        let offset = [
            target.own_center[0] - center[0],
            target.own_center[1] - center[1],
            target.own_center[2] - center[2],
        ];
        let offset_mag = (offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2]).sqrt();
        eprintln!(
            "bug221_pivot_spins_in_place_after_fix_but_not_before: rotating object {target_k} \
             (own_center={:?}, scene center={center:?}, offset magnitude={offset_mag:.3})",
            target.own_center
        );
        assert!(
            offset_mag > 1.0,
            "fixture must actually exercise a far-from-scene-center object for this gate to mean \
             anything, got offset magnitude {offset_mag:.3}"
        );

        let (post_def, _report) = assemble_import_graph(&path).expect("assemble EmissiveStrengthTest");
        let pre_def = reconstruct_pre_bug221_fix(&post_def);

        fn rotate_one_object_y(mut def: EffectGraphDef, k: usize, radians: f32) -> EffectGraphDef {
            let target_handle = format!("transform_{k}");
            for node in &mut def.nodes {
                let Some(body) = node.group.as_mut() else { continue };
                for n in &mut body.nodes {
                    if n.type_id == "node.transform_3d" && n.handle.as_deref() == Some(target_handle.as_str()) {
                        n.params.insert("rot_y".to_string(), float(radians));
                    }
                }
            }
            def
        }

        let rot = std::f32::consts::FRAC_PI_4; // 45 degrees
        let post_rot_def = rotate_one_object_y(post_def, target_k, rot);
        let pre_rot_def = rotate_one_object_y(pre_def, target_k, rot);

        let (w, h) = (512u32, 512u32);
        let post_rot_rgba = render_once(post_rot_def, w, h, "bug221-post-fix-45deg");
        let pre_rot_rgba = render_once(pre_rot_def, w, h, "bug221-pre-fix-45deg");

        let out_dir = std::env::var("MESH_SNAP_OUT_DIR").unwrap_or_else(|_| "target/mesh-snap".to_string());
        std::fs::create_dir_all(&out_dir).expect("create output dir");
        let post_path = format!("{out_dir}/bug221_pivot_post_fix_45deg.png");
        let pre_path = format!("{out_dir}/bug221_pivot_pre_fix_45deg.png");
        image::save_buffer(&post_path, &post_rot_rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {post_path}: {e}"));
        image::save_buffer(&pre_path, &pre_rot_rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {pre_path}: {e}"));

        assert_ne!(
            post_rot_rgba, pre_rot_rgba,
            "pre-fix and post-fix 45deg-rotated renders must differ — different pivots must produce \
             visibly different results, or the fix changed nothing"
        );
        println!(
            "bug221_pivot_spins_in_place_after_fix_but_not_before: wrote {pre_path} and {post_path} \
             — look at both: pre-fix should show cube(s) swung away from their unrotated footprint, \
             post-fix should show every cube still occupying roughly its unrotated footprint, just \
             with rotated faces"
        );
    }
}


