//! Shared graph-op constructors (`plain_node`, `wire`, param value ctors)
//! and naming helpers used across the importer's assembly modules.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire, SerializedParamValue};

/// Build an [`EffectGraphNode`] with the given identity and every other
/// field at its "ordinary node" default. `EffectGraphNode` doesn't derive
/// `Default` (several fields are meaningful `Option`s used by grouping /
/// the graph editor), so this centralises the shape once rather than
/// repeating all eleven fields at every call site.
pub(super) fn plain_node(id: u32, node_id: &str, type_id: &str, handle: &str) -> EffectGraphNode {
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

pub(super) fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

pub(super) fn float(v: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value: v }
}
pub(super) fn int(v: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value: v }
}
pub(super) fn bool_val(v: bool) -> SerializedParamValue {
    SerializedParamValue::Bool { value: v }
}
pub(super) fn enum_val(v: u32) -> SerializedParamValue {
    SerializedParamValue::Enum { value: v }
}
pub(super) fn table(rows: Vec<Vec<f32>>) -> SerializedParamValue {
    SerializedParamValue::Table { rows }
}

/// Replace every run of non-alphanumeric characters with a single `_` and
/// trim leading/trailing underscores, so a filename stem like
/// `"cc0__oomurasaki_azalea_r._x_pulchrum"` becomes a clean identifier
/// safe for a [`PresetTypeId`] / OSC path segment. Falls back to
/// `"ImportedModel"` for a stem that sanitizes to nothing, and prefixes a
/// leading digit (OSC/identifier convention) with `Model_`.
pub(super) fn sanitize_identifier(stem: &str) -> String {
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
pub(super) fn unique_group_name(
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
pub(super) fn group_tint(index: usize) -> [f32; 4] {
    let hue = (index as f32 * 0.618_034) % 1.0;
    let c = manifold_core::color::Color::hsv_to_rgb(hue, 0.7, 0.85);
    [c.r, c.g, c.b, 1.0]
}
