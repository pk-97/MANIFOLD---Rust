//! glTF import ASSEMBLER (P1c stage 2) — a pure function that turns a
//! parsed `.glb`'s [`gltf_load::GltfImportSummary`] (stage 1: the CPU-only
//! parse) into a generator [`EffectGraphDef`] that renders the model
//! faithfully: one `node.render_scene` object PER DISTINCT MATERIAL, each
//! fed its material-filtered geometry (`node.gltf_mesh_source`), that
//! material's base-color texture (`node.gltf_texture_source`, when present),
//! and a `node.pbr_material` atom carrying the glTF's PBR factors — plus a
//! shared synthesized framing camera (`node.orbit_camera`), a sun light
//! (`node.light`), and an IBL envmap (`node.bake_environment`).
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
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, ParamSpecDef,
    PresetMetadata, SerializedParamValue, SkipModeDef, StringBindingDef, StringParamSpecDef,
};

use super::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID};
use super::gltf_load;

/// Hard cap mirrored from `node.render_scene`'s own `MAX_OBJECTS` — the
/// assembler cannot emit more objects than the renderer can host. Materials
/// beyond this are dropped (smallest by vertex count first), not silently
/// merged — see [`ImportReport::dropped_over_cap`].
const MAX_RENDER_SCENE_OBJECTS: usize = 8;

/// Stable identity for the one outer-card text config every imported
/// preset carries: the source `.glb`/`.gltf` path.
const MODEL_FILE_PARAM_ID: &str = "model_file";

/// What the assembler did, for the caller (importer UI, tests) to report
/// or warn on. Not part of the graph itself.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// Distinct materials with geometry, as parsed (before the
    /// [`MAX_RENDER_SCENE_OBJECTS`] cap).
    pub material_count: usize,
    /// Objects actually wired into `node.render_scene` — `min(material_count, 8)`.
    pub object_count: usize,
    /// How many objects got a `node.gltf_texture_source` → `base_color_map_N` wire.
    pub textures_wired: usize,
    /// Materials dropped because the glb has more than 8 (the smallest by
    /// vertex count, so the most visually significant objects survive).
    pub dropped_over_cap: usize,
    /// Triangle-list vertices belonging to glTF's unassigned default
    /// material — v1 does not import these (mirrors
    /// [`gltf_load::GltfImportSummary::default_material_vertex_count`]).
    pub default_material_vertex_count: u32,
    /// Always `true` today — the assembler always synthesizes a framing
    /// camera (the glb's own embedded cameras, if any, are not yet
    /// consumed). Kept as a field so a future embedded-camera path has
    /// somewhere to report `false`.
    pub camera_synthesized: bool,
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

fn float(v: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value: v }
}
fn int(v: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value: v }
}
fn enum_val(v: u32) -> SerializedParamValue {
    SerializedParamValue::Enum { value: v }
}

/// Degrees→radians factor for the camera card sliders. The camera node's
/// `orbit`/`tilt`/`fov_y` params are radians (matching `node.orbit_camera`),
/// but a performer wants degrees on the card, so each angle slider carries a
/// [`BindingDef::scale`] of this value — the `param_binding` write boundary
/// applies `value * scale` (no `deg_to_rad` helper node needed, unlike the
/// hand-authored MetallicGlass preset).
const DEG2RAD: f32 = std::f32::consts::PI / 180.0;

/// One outer-card slider definition. Curated performance surface for an
/// imported model (camera framing, light, material) — see
/// [`assemble_import_graph`]'s metadata block. `default_value` must match
/// the wired node param's value (through `scale`) so the card reproduces the
/// assembler's look on first frame with no drift.
fn card_param(id: &str, name: &str, min: f32, max: f32, default: f32, is_angle: bool) -> ParamSpecDef {
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

/// Parse `path` and assemble a generator [`EffectGraphDef`] that renders it
/// faithfully: one `node.render_scene` object per distinct material
/// (capped at [`MAX_RENDER_SCENE_OBJECTS`], largest-by-vertex-count first),
/// each fed its material-filtered geometry + base-color texture (if any) +
/// a PBR material, framed by a synthesized orbit camera sized to the glb's
/// bounding box, lit by one sun light, under a baked IBL envmap (required —
/// `node.pbr_material` is degenerate without one). Pure function: one CPU
/// parse via [`gltf_load::gltf_import_summary`], no GPU, no other I/O.
///
/// Errors when the glb has no materials with geometry (nothing to import) —
/// propagated from [`gltf_load::gltf_import_summary`] or raised here.
pub fn assemble_import_graph(path: &Path) -> Result<(EffectGraphDef, ImportReport), String> {
    let summary = gltf_load::gltf_import_summary(path)?;
    if summary.materials.is_empty() {
        return Err(format!(
            "{}: no materials with geometry — nothing to import",
            path.display()
        ));
    }

    // Largest-by-vertex-count first, so a >8-material glb keeps its most
    // visually significant objects when capped, not an arbitrary prefix.
    let mut materials = summary.materials.clone();
    materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
    let dropped_over_cap = materials.len().saturating_sub(MAX_RENDER_SCENE_OBJECTS);
    materials.truncate(MAX_RENDER_SCENE_OBJECTS);
    let n = materials.len();
    if dropped_over_cap > 0 {
        log::warn!(
            "gltf_import::assemble_import_graph({}): {} materials with geometry, \
             node.render_scene caps at {MAX_RENDER_SCENE_OBJECTS} objects — dropping the \
             {dropped_over_cap} smallest by vertex count",
            path.display(),
            summary.materials.len(),
        );
    }
    if summary.default_material_vertex_count > 0 {
        log::warn!(
            "gltf_import::assemble_import_graph({}): {} vertices belong to glTF's unassigned \
             default material — v1 does not import these",
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
    let distance = 2.2 * radius;

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

    let envmap_id = fresh_id();
    nodes.push(plain_node(envmap_id, "envmap", "node.bake_environment", "envmap"));

    let camera_id = fresh_id();
    let mut cam_node = plain_node(camera_id, "camera", "node.orbit_camera", "camera");
    cam_node.params.insert("orbit".to_string(), float(0.7));
    cam_node.params.insert("tilt".to_string(), float(0.3));
    cam_node.params.insert("distance".to_string(), float(distance));
    cam_node.params.insert("fov_y".to_string(), float(0.9));
    cam_node.params.insert("look_y".to_string(), float(0.0));
    nodes.push(cam_node);

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
    let multi = n > 1;

    for (k, m) in materials.iter().enumerate() {
        let mesh_node_id = format!("mesh_{k}");
        let mat_node_id = format!("mat_{k}");

        let mesh_id = fresh_id();
        let mut mesh_node =
            plain_node(mesh_id, &mesh_node_id, "node.gltf_mesh_source", &mesh_node_id);
        mesh_node
            .params
            .insert("material_index".to_string(), int(m.material_index as i32));
        mesh_node
            .params
            .insert("max_capacity".to_string(), int(m.vertex_count.max(1) as i32));
        nodes.push(mesh_node);

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
        mat_node
            .params
            .insert("color_a".to_string(), float(m.base_color_factor[3]));
        mat_node.params.insert("metallic".to_string(), float(m.metallic));
        mat_node
            .params
            .insert("roughness".to_string(), float(m.roughness.max(0.01)));
        // 0.18, not the atom's 0.05 default: a single key light leaves the
        // shadow side of a matte model near-black, so it reads as a silhouette
        // rather than a form. A modest ambient lift restores the far side
        // enough to see shape under the default rig.
        mat_node.params.insert("ambient".to_string(), float(0.18));
        mat_node
            .params
            .insert("emission_r".to_string(), float(m.emissive[0]));
        mat_node
            .params
            .insert("emission_g".to_string(), float(m.emissive[1]));
        mat_node
            .params
            .insert("emission_b".to_string(), float(m.emissive[2]));
        let emissive_lit = m.emissive.iter().any(|&c| c > 0.0);
        mat_node.params.insert(
            "emission_intensity".to_string(),
            float(if emissive_lit { 1.0 } else { 0.0 }),
        );
        mat_node
            .params
            .insert("alpha_mode".to_string(), enum_val(if m.alpha_mask { 1 } else { 0 }));
        mat_node
            .params
            .insert("alpha_cutoff".to_string(), float(m.alpha_cutoff));
        nodes.push(mat_node);

        // Per-object material knobs on the card: metallic + roughness, the
        // two live PBR controls. Suffixed with the object number only when
        // there's more than one, so a single-material model reads
        // "Metallic" / "Roughness" not "Metallic 1". Defaults mirror the
        // node params set just above, so the card reproduces the glTF's own
        // material on first frame.
        let suffix = if multi { format!(" {}", k + 1) } else { String::new() };
        let metal_default = m.metallic;
        let rough_default = m.roughness.max(0.01);
        let metal_id = format!("metal_{k}");
        let rough_id = format!("rough_{k}");
        let metal_name = format!("Metallic{suffix}");
        let rough_name = format!("Roughness{suffix}");
        card_params.push(card_param(&metal_id, &metal_name, 0.0, 1.0, metal_default, false));
        card_bindings.push(card_binding(
            &metal_id, &metal_name, metal_default, &mat_node_id, "metallic", 1.0,
        ));
        card_params.push(card_param(&rough_id, &rough_name, 0.01, 1.0, rough_default, false));
        card_bindings.push(card_binding(
            &rough_id, &rough_name, rough_default, &mat_node_id, "roughness", 1.0,
        ));

        wires.push(wire(mesh_id, "vertices", render_id, &format!("mesh_{k}")));
        wires.push(wire(mat_id, "out", render_id, &format!("material_{k}")));

        // Recenter this object at the origin so the fixed-target orbit
        // camera frames the (not-recentered) gltf_mesh_source output —
        // same convention `gltf_mesh_source_renders_azalea_to_png` proves.
        render_node
            .params
            .insert(format!("pos_x_{k}"), float(-center[0]));
        render_node
            .params
            .insert(format!("pos_y_{k}"), float(-center[1]));
        render_node
            .params
            .insert(format!("pos_z_{k}"), float(-center[2]));

        string_bindings.push(StringBindingDef {
            id: MODEL_FILE_PARAM_ID.to_string(),
            label: "Model File".to_string(),
            default_value: path_str.clone(),
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
            nodes.push(tex_node);

            wires.push(wire(tex_id, "out", render_id, &format!("base_color_map_{k}")));

            string_bindings.push(StringBindingDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                label: "Model File".to_string(),
                default_value: path_str.clone(),
                target: BindingTarget::Node {
                    node_id: NodeId::new(&tex_node_id),
                    param: "path".to_string(),
                },
            });

            textures_wired += 1;
        }
    }

    nodes.push(render_node);

    let final_id = fresh_id();
    nodes.push(plain_node(final_id, "final", FINAL_OUTPUT_TYPE_ID, "final"));

    wires.push(wire(camera_id, "out", render_id, "camera"));
    wires.push(wire(envmap_id, "envmap", render_id, "envmap"));
    wires.push(wire(sun_id, "out", render_id, "light_0"));
    wires.push(wire(render_id, "color", final_id, "in"));

    // Shared framing / light / environment card knobs. These come LAST in
    // `card_params` (after the per-object material knobs) but read first on
    // the card as the primary performance controls. Angle sliders are in
    // degrees (scale = DEG2RAD folds the conversion into the write boundary);
    // defaults mirror the `camera`/`sun`/`envmap` node params set above so
    // the card is a faithful mirror of the assembled look.
    card_params.push(card_param("cam_orbit", "Camera Orbit", -180.0, 180.0, 0.7 / DEG2RAD, true)); // angle
    card_bindings.push(card_binding(
        "cam_orbit", "Camera Orbit", 0.7 / DEG2RAD, "camera", "orbit", DEG2RAD,
    ));
    card_params.push(card_param("cam_tilt", "Camera Tilt", -89.0, 89.0, 0.3 / DEG2RAD, true)); // angle
    card_bindings.push(card_binding(
        "cam_tilt", "Camera Tilt", 0.3 / DEG2RAD, "camera", "tilt", DEG2RAD,
    ));
    card_params.push(card_param(
        "cam_dist",
        "Camera Distance",
        0.1,
        (distance * 4.0).max(1.0),
        distance,
        false,
    ));
    card_bindings.push(card_binding(
        "cam_dist", "Camera Distance", distance, "camera", "distance", 1.0,
    ));
    card_params.push(card_param("cam_fov", "Camera FOV", 20.0, 120.0, 0.9 / DEG2RAD, true)); // angle
    card_bindings.push(card_binding(
        "cam_fov", "Camera FOV", 0.9 / DEG2RAD, "camera", "fov_y", DEG2RAD,
    ));

    card_params.push(card_param("sun_int", "Sun Intensity", 0.0, 10.0, 3.5, false));
    card_bindings.push(card_binding("sun_int", "Sun Intensity", 3.5, "sun", "intensity", 1.0));
    card_params.push(card_param("sun_x", "Sun X", -15.0, 15.0, 5.0, false));
    card_bindings.push(card_binding("sun_x", "Sun X", 5.0, "sun", "pos_x", 1.0));
    card_params.push(card_param("sun_y", "Sun Y", -15.0, 15.0, 2.0, false));
    card_bindings.push(card_binding("sun_y", "Sun Y", 2.0, "sun", "pos_y", 1.0));
    card_params.push(card_param("sun_z", "Sun Z", -15.0, 15.0, 3.0, false));
    card_bindings.push(card_binding("sun_z", "Sun Z", 3.0, "sun", "pos_z", 1.0));

    // `node.bake_environment`'s `horizon_strength` is its brightness knob
    // (default 1.0, range 0..4) — the closest thing to "envmap intensity"
    // and what drives IBL reflection strength on the PBR materials.
    card_params.push(card_param("env_bright", "Reflections", 0.0, 4.0, 1.0, false));
    card_bindings.push(card_binding(
        "env_bright", "Reflections", 1.0, "envmap", "horizon_strength", 1.0,
    ));

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
        string_params: vec![StringParamSpecDef {
            id: MODEL_FILE_PARAM_ID.to_string(),
            name: "Model File".to_string(),
            default_value: path_str,
            is_file_picker: true,
            use_dropdown: false,
        }],
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

    let report = ImportReport {
        material_count: summary.materials.len(),
        object_count: n,
        textures_wired,
        dropped_over_cap,
        default_material_vertex_count: summary.default_material_vertex_count,
        camera_synthesized: true,
    };

    Ok((def, report))
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
        assert_eq!(report.dropped_over_cap, 0);
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
        assert_eq!(meta.string_params.len(), 1, "exactly one model_file string param");
        assert_eq!(meta.string_params[0].id, "model_file");
        assert!(meta.string_params[0].is_file_picker);

        assert_eq!(meta.string_bindings.len(), 4, "2 mesh + 2 texture path bindings");
        for b in &meta.string_bindings {
            assert_eq!(b.id, "model_file");
            match &b.target {
                BindingTarget::Node { param, .. } => assert_eq!(param, "path"),
                other => panic!("expected a Node binding target, got {other:?}"),
            }
        }

        // Curated performance surface (D9 / P0): the card must carry real
        // params + bindings, not the empty vecs the pre-P0 assembler emitted.
        // Azalea has 2 objects → 4 camera + 4 sun + 1 envmap + 2×(metallic +
        // roughness) = 13 sliders, each with exactly one binding.
        assert_eq!(meta.params.len(), 13, "4 camera + 4 sun + 1 envmap + 2×2 material");
        assert_eq!(
            meta.bindings.len(),
            meta.params.len(),
            "every card param routes to exactly one node param"
        );
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
        // Spot-check the shared framing knobs and the per-object material
        // knobs (suffixed because azalea has >1 object).
        for id in ["cam_orbit", "cam_dist", "sun_int", "env_bright", "metal_0", "rough_1"] {
            assert!(meta.params.iter().any(|p| p.id == id), "missing card param `{id}`");
        }
        // Camera angle sliders convert degrees→radians at the write boundary.
        let orbit = meta.bindings.iter().find(|b| b.id == "cam_orbit").unwrap();
        assert!(
            (orbit.scale - std::f32::consts::PI / 180.0).abs() < 1e-6,
            "camera angle bindings fold DEG2RAD into `scale`"
        );
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
        PresetRuntime::from_def(def, &registry)
            .expect("assembled azalea graph must compile through PresetRuntime::from_def");
    }

    /// Regression for the glTF-import "unknown parameter 'pos_x_N'" load
    /// failure (Peter, 2026-07-05): a model with >2 distinct materials
    /// assembles a `node.render_scene` with `objects >= 3`, whose per-object
    /// transform params (`pos_x_2`, `pos_y_2`, …) only exist after the node
    /// reconfigures to that object count. The loader used to snapshot the
    /// param surface at the default 2-object count and reject `pos_x_2` as
    /// unknown, so the whole generator failed to load and rendered black. The
    /// azalea fixture has exactly 2 objects, so it never exercised this — the
    /// coverage gap that let it ship. This synthetic 3-object def reproduces
    /// it with no large fixture and must load clean.
    #[test]
    fn render_scene_with_three_objects_loads_per_object_transform_params() {
        use crate::node_graph::parameters::ParamValue;
        use crate::node_graph::persistence::EffectGraphDefExt;
        use manifold_core::effect_graph_def::EffectGraphNode;

        let mut params = std::collections::BTreeMap::new();
        params.insert("objects".to_string(), ParamValue::Float(3.0).into());
        params.insert("lights".to_string(), ParamValue::Float(1.0).into());
        // The param that was rejected before the fix: object index 2's X/Y pos,
        // which only exists once render_scene reconfigures to objects >= 3.
        params.insert("pos_x_2".to_string(), ParamValue::Float(-1.5).into());
        params.insert("pos_y_2".to_string(), ParamValue::Float(0.25).into());

        let render = EffectGraphNode {
            id: 0,
            node_id: manifold_core::NodeId::new("render"),
            type_id: "node.render_scene".to_string(),
            handle: Some("render".to_string()),
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        };
        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render],
            wires: Vec::new(),
        };

        // Validate at the `into_graph` layer — the exact place the
        // "unknown parameter 'pos_x_2'" error was raised. (A full
        // `from_def` additionally enforces generator-boundary wiring, which
        // this minimal single-node def deliberately omits — out of scope for
        // the param-surface regression.)
        let registry = PrimitiveRegistry::with_builtin();
        def.into_graph(&registry).expect(
            "render_scene with objects=3 must accept per-object param pos_x_2 at load \
             (reconfigure runs before param validation)",
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
        PresetRuntime::from_def(def, &registry).unwrap_or_else(|e| {
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

        let path = azalea_fixture_path();
        if !path.exists() {
            eprintln!(
                "imported_azalea_renders_faithfully_to_png: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble azalea");
        println!("imported azalea report: {report:?}");

        let (w, h) = (768u32, 768u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();

        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, &device, w, h, format)
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

        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut fraction = 0.0f64;
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
            if fraction > 0.02 {
                println!(
                    "imported_azalea_renders_faithfully_to_png: converged on attempt {attempt} \
                     (non-black fraction {fraction:.4})"
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
        assert!(
            fraction > 0.02,
            "expected >2% non-black pixels after polling for both background parses, got \
             {fraction:.4} — likely a broken importer graph, a parse that never landed, or empty geometry"
        );

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
            .create_with_override(&device, &preset_id, Some(&def), w, h, false)
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

        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut fraction = 0.0f64;
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
            if fraction > 0.02 {
                println!(
                    "create_with_override proof: converged on attempt {attempt} (non-black {fraction:.4})"
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
}
