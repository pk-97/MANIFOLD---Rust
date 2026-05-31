//! Generate the node catalog from the live registry.
//!
//! The code is the single source of truth; the docs are derived. This
//! module walks the [`PrimitiveFactory`] inventory (for `type_id` +
//! picker label), joins each entry with its [`NodeDescriptor`] (purpose /
//! summary / category / role / examples) and a freshly-`create()`d
//! instance (for live port + param shapes), and renders two artifacts:
//!
//! - [`node_catalog_json`] — `docs/node_catalog.json`, the machine-readable
//!   descriptor the AI composition surface consumes.
//! - [`generated_block`] — the marker-delimited "Registered node index"
//!   block injected into `docs/NODE_CATALOG.md`.
//!
//! [`regenerates_in_sync`](tests::regenerates_in_sync) re-renders both in
//! memory and asserts they byte-match what's on disk, so a registry change
//! that isn't reflected in the docs fails CI — the permanent fix for the
//! hand-reconciliation drift that let a fused monolith (`node.glitch`) sit
//! in the registry but vanish from the catalog.
//!
//! Deterministic by construction: rows sort by `type_id`, no timestamps,
//! so same registry → byte-identical output.

use std::fmt::Write as _;

use manifold_core::EffectTypeId;
use manifold_core::effect_graph_def::PresetMetadata;

use crate::generators::bundled_generator_presets::loaded_generator_presets_from_bundled;
use crate::node_graph::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
use crate::node_graph::descriptor::{Category, NodeDescriptor, Role, descriptor_for};
use crate::node_graph::palette::PaletteCategory;
use crate::node_graph::param_doc::tooltip_for;
use crate::node_graph::parameters::{ParamType, ParamValue};
use crate::node_graph::persistence::PrimitiveFactory;
use crate::node_graph::ports::{PortType, ScalarType};

/// Opening marker of the generated block in `docs/NODE_CATALOG.md`.
pub const BEGIN_MARKER: &str =
    "<!-- BEGIN GENERATED: registered-node-index — do not edit; run `cargo run -p manifold-renderer --bin gen_node_catalog` -->";
/// Closing marker of the generated block.
pub const END_MARKER: &str = "<!-- END GENERATED: registered-node-index -->";

/// Which editor stratum a registered node lands in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stratum {
    Atom,
    Driver,
    /// Registered + loadable from JSON, but not surfaced in the palette
    /// (boundary nodes, composites, whole-effect wrappers).
    Unlisted,
}

impl Stratum {
    fn from_picker(picker: Option<PaletteCategory>) -> Self {
        match picker {
            Some(PaletteCategory::Atom) => Self::Atom,
            Some(PaletteCategory::Driver) => Self::Driver,
            None => Self::Unlisted,
        }
    }
}

/// One node, joined from registry + descriptor + a live instance.
struct NodeRow {
    type_id: &'static str,
    label: Option<&'static str>,
    stratum: Stratum,
    purpose: &'static str,
    summary: &'static str,
    category: Category,
    role: Role,
    aliases: &'static [&'static str],
    examples: &'static [&'static str],
    inputs: Vec<PortRow>,
    outputs: Vec<PortRow>,
    params: Vec<ParamRow>,
}

struct PortRow {
    name: String,
    ty: String,
    required: bool,
}

struct ParamRow {
    name: &'static str,
    label: &'static str,
    ty: ParamType,
    default: String,
    range: Option<(f32, f32)>,
    enum_values: &'static [&'static str],
}

/// Test-only fixture primitives (`node.__smoke_test*`) register in the
/// inventory under `cfg(test)` but not in a normal build. Excluding them
/// keeps the generated artifact identical whether produced by the bin
/// (non-test) or the drift-guard test (cfg(test)).
fn is_test_fixture(type_id: &str) -> bool {
    type_id.starts_with("node.__")
}

/// Collect one [`NodeRow`] per registered primitive, sorted by `type_id`.
fn collect_rows() -> Vec<NodeRow> {
    let mut rows: Vec<NodeRow> = inventory::iter::<PrimitiveFactory>
        .into_iter()
        .filter(|f| !is_test_fixture(f.type_id))
        .map(|f| {
            let node = (f.create)();
            let desc: Option<&NodeDescriptor> = descriptor_for(f.type_id);
            NodeRow {
                type_id: f.type_id,
                label: f.picker.map(|p| p.label),
                stratum: Stratum::from_picker(f.picker.map(|p| p.category)),
                purpose: desc.map(|d| d.purpose).unwrap_or(""),
                summary: desc.map(|d| d.summary).unwrap_or(""),
                category: desc.map(|d| d.category).unwrap_or(Category::Uncategorized),
                role: desc.map(|d| d.role).unwrap_or(Role::Unknown),
                aliases: desc.map(|d| d.aliases).unwrap_or(&[]),
                examples: desc.map(|d| d.examples).unwrap_or(&[]),
                inputs: node.inputs().iter().map(port_row).collect(),
                outputs: node.outputs().iter().map(port_row).collect(),
                params: node.parameters().iter().map(param_row).collect(),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.type_id.cmp(b.type_id));
    rows
}

fn port_row(p: &crate::node_graph::ports::NodePort) -> PortRow {
    PortRow {
        name: p.name.to_string(),
        ty: port_type_str(&p.ty),
        required: p.required,
    }
}

fn param_row(p: &crate::node_graph::parameters::ParamDef) -> ParamRow {
    ParamRow {
        name: p.name,
        label: p.label,
        ty: p.ty,
        default: param_default_str(&p.default),
        range: p.range,
        enum_values: p.enum_values,
    }
}

/// Short, stable type tag for a port — what an author/AI needs to know
/// what wires where, without the full channel signature.
fn port_type_str(ty: &PortType) -> String {
    match ty {
        PortType::Texture2D => "Texture2D".into(),
        PortType::Texture2DTyped(_) => "Texture2D (typed)".into(),
        PortType::Texture3D => "Texture3D".into(),
        PortType::Scalar(s) => match s {
            ScalarType::F32 => "f32".into(),
            ScalarType::Vec2 => "vec2".into(),
            ScalarType::Vec3 => "vec3".into(),
            ScalarType::Vec4 => "vec4".into(),
            ScalarType::Color => "color".into(),
        },
        PortType::Array(_) => "Array".into(),
        PortType::Camera => "Camera".into(),
        PortType::Light => "Light".into(),
        PortType::Material => "Material".into(),
    }
}

fn param_type_str(t: ParamType) -> &'static str {
    match t {
        ParamType::Float => "float",
        ParamType::Int => "int",
        ParamType::Bool => "bool",
        ParamType::Vec2 => "vec2",
        ParamType::Vec3 => "vec3",
        ParamType::Vec4 => "vec4",
        ParamType::Color => "color",
        ParamType::Enum => "enum",
        ParamType::Table => "table",
        ParamType::String => "string",
        ParamType::Trigger => "trigger",
    }
}

fn fmt_f32(v: f32) -> String {
    // `{}` already trims trailing zeros (1.0 → "1", 0.25 → "0.25") and is
    // stable across runs — what the drift guard needs.
    format!("{v}")
}

fn param_default_str(v: &ParamValue) -> String {
    match v {
        ParamValue::Float(f) => fmt_f32(*f),
        ParamValue::Bool(b) => b.to_string(),
        ParamValue::Vec2(a) => format!("[{}, {}]", fmt_f32(a[0]), fmt_f32(a[1])),
        ParamValue::Vec3(a) => {
            format!("[{}, {}, {}]", fmt_f32(a[0]), fmt_f32(a[1]), fmt_f32(a[2]))
        }
        ParamValue::Vec4(a) | ParamValue::Color(a) => format!(
            "[{}, {}, {}, {}]",
            fmt_f32(a[0]),
            fmt_f32(a[1]),
            fmt_f32(a[2]),
            fmt_f32(a[3])
        ),
        ParamValue::Enum(i) => i.to_string(),
        ParamValue::Table(_) => "table".into(),
        ParamValue::String(s) => format!("{s:?}"),
    }
}

// ─── JSON artifact ───────────────────────────────────────────────────

/// Render `docs/node_catalog.json` — the authoritative, machine-readable
/// descriptor for the whole node vocabulary plus the effect/generator
/// presets. Deterministic (sorted, no timestamps).
pub fn node_catalog_json() -> String {
    let rows = collect_rows();

    let nodes: Vec<serde_json::Value> = rows.iter().map(node_json).collect();
    let presets: Vec<serde_json::Value> = preset_json_rows();

    let doc = serde_json::json!({
        "$comment": "GENERATED by `cargo run -p manifold-renderer --bin gen_node_catalog`. \
                     Source of truth is the node registry; edit nodes in code, not this file.",
        "node_count": nodes.len(),
        "preset_count": presets.len(),
        "nodes": nodes,
        "presets": presets,
    });

    // Pretty-print with a trailing newline (POSIX text-file convention).
    let mut s = serde_json::to_string_pretty(&doc).expect("catalog json serializes");
    s.push('\n');
    s
}

fn node_json(r: &NodeRow) -> serde_json::Value {
    let inputs: Vec<serde_json::Value> = r
        .inputs
        .iter()
        .map(|p| {
            serde_json::json!({ "name": p.name, "type": p.ty, "required": p.required })
        })
        .collect();
    let outputs: Vec<serde_json::Value> = r
        .outputs
        .iter()
        .map(|p| serde_json::json!({ "name": p.name, "type": p.ty }))
        .collect();
    let params: Vec<serde_json::Value> = r
        .params
        .iter()
        .map(|p| {
            let mut o = serde_json::json!({
                "name": p.name,
                "label": p.label,
                "type": param_type_str(p.ty),
                "default": p.default,
            });
            if let Some((lo, hi)) = p.range {
                o["range"] = serde_json::json!([fmt_f32(lo), fmt_f32(hi)]);
            }
            if !p.enum_values.is_empty() {
                o["enum_values"] = serde_json::json!(p.enum_values);
            }
            if let Some(tip) = tooltip_for(r.type_id, p.name) {
                o["tooltip"] = serde_json::json!(tip);
            }
            o
        })
        .collect();

    serde_json::json!({
        "type_id": r.type_id,
        "label": r.label,
        "stratum": match r.stratum {
            Stratum::Atom => "atom",
            Stratum::Driver => "driver",
            Stratum::Unlisted => "unlisted",
        },
        "category": r.category.label(),
        "role": r.role.label(),
        "summary": r.summary,
        "purpose": r.purpose,
        "aliases": r.aliases,
        "examples": r.examples,
        "inputs": inputs,
        "outputs": outputs,
        "params": params,
    })
}

/// One effect or generator preset, normalized from its `PresetMetadata`.
struct PresetRow {
    id: String,
    name: String,
    category: String,
    /// `"effect"` or `"generator"` — they ship in two registries but are
    /// one user-facing concept (a card composed of atoms).
    kind: &'static str,
    is_line_based: bool,
    params: Vec<PresetParam>,
}

struct PresetParam {
    id: String,
    name: String,
    min: f32,
    max: f32,
    default: f32,
}

fn preset_row_from_meta(meta: &PresetMetadata, kind: &'static str) -> PresetRow {
    PresetRow {
        id: meta.id.as_str().to_string(),
        name: meta.display_name.clone(),
        category: meta.category.clone(),
        kind,
        is_line_based: meta.is_line_based,
        params: meta
            .params
            .iter()
            .map(|p| PresetParam {
                id: p.id.clone(),
                name: p.name.clone(),
                min: p.min,
                max: p.max,
                default: p.default_value,
            })
            .collect(),
    }
}

/// Collect every bundled preset — effects *and* generators — sorted by id.
fn collect_presets() -> Vec<PresetRow> {
    let mut rows: Vec<PresetRow> = Vec::new();

    let mut effect_ids: Vec<EffectTypeId> = bundled_preset_type_ids().collect();
    effect_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for id in &effect_ids {
        if let Some(meta) = bundled_preset_def(id).and_then(|d| d.preset_metadata.as_ref()) {
            rows.push(preset_row_from_meta(meta, "effect"));
        }
    }

    for meta in loaded_generator_presets_from_bundled() {
        rows.push(preset_row_from_meta(&meta, "generator"));
    }

    rows.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.kind.cmp(b.kind)));
    rows
}

fn preset_json_rows() -> Vec<serde_json::Value> {
    collect_presets()
        .iter()
        .map(|r| {
            let params: Vec<serde_json::Value> = r
                .params
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "id": p.id,
                        "name": p.name,
                        "min": fmt_f32(p.min),
                        "max": fmt_f32(p.max),
                        "default": fmt_f32(p.default),
                    })
                })
                .collect();
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "kind": r.kind,
                "category": r.category,
                "is_line_based": r.is_line_based,
                "params": params,
            })
        })
        .collect()
}

// ─── Markdown block ──────────────────────────────────────────────────

/// Render the marker-delimited block injected into `docs/NODE_CATALOG.md`:
/// the complete registered-node index (grouped by stratum) plus the
/// effect/generator preset list. Includes the open/close markers.
pub fn generated_block() -> String {
    let rows = collect_rows();
    let mut out = String::new();

    let _ = writeln!(out, "{BEGIN_MARKER}");
    out.push('\n');
    let _ = writeln!(
        out,
        "_Generated from the node registry. Do not hand-edit. \
         {} nodes registered, grouped by category. Full ports, params, \
         tooltips and search aliases live in [node_catalog.json](node_catalog.json)._",
        rows.len()
    );

    for &cat in Category::ALL {
        let group: Vec<&NodeRow> = rows.iter().filter(|r| r.category == cat).collect();
        if group.is_empty() {
            continue;
        }
        out.push('\n');
        let _ = writeln!(out, "### {} ({})", cat.label(), group.len());
        out.push('\n');
        let _ = writeln!(out, "| Node | type_id | role | summary |");
        let _ = writeln!(out, "|---|---|---|---|");
        for r in group {
            // Prefer the friendly summary when one is filled, falling back to
            // the first sentence of the technical purpose.
            let blurb = if r.summary.is_empty() {
                first_sentence(r.purpose)
            } else {
                r.summary
            };
            let _ = writeln!(
                out,
                "| {} | `{}` | {} | {} |",
                opt_cell(r.label),
                r.type_id,
                role_cell(r.role),
                md_cell(blurb),
            );
        }
    }

    // Effect + generator presets.
    let presets = collect_presets();
    out.push('\n');
    let _ = writeln!(out, "### Effect & generator presets ({})", presets.len());
    out.push('\n');
    let _ = writeln!(out, "| id | name | kind | category | params |");
    let _ = writeln!(out, "|---|---|---|---|---|");
    for r in &presets {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} |",
            r.id,
            md_cell(&r.name),
            r.kind,
            md_cell(&r.category),
            r.params.len(),
        );
    }

    out.push('\n');
    let _ = write!(out, "{END_MARKER}");
    out
}

/// Replace the content between the markers in `existing` with a freshly
/// generated block. Returns `None` if either marker is missing.
pub fn inject(existing: &str) -> Option<String> {
    let begin = existing.find(BEGIN_MARKER)?;
    let end_marker_start = existing.find(END_MARKER)?;
    let end = end_marker_start + END_MARKER.len();
    let mut out = String::with_capacity(existing.len());
    out.push_str(&existing[..begin]);
    out.push_str(&generated_block());
    out.push_str(&existing[end..]);
    Some(out)
}

// ─── markdown cell sanitizers ────────────────────────────────────────

fn first_sentence(s: &str) -> &str {
    if s.is_empty() {
        return s;
    }
    // First sentence = up to the first ". " (keeping the period). Falls
    // back to the whole string if there's no sentence break.
    match s.find(". ") {
        Some(i) => &s[..=i],
        None => s,
    }
}

fn md_cell(s: &str) -> String {
    if s.is_empty() {
        return "—".into();
    }
    let cleaned = s.replace('\n', " ").replace('|', "\\|");
    // Keep table rows readable.
    const MAX: usize = 160;
    if cleaned.chars().count() > MAX {
        let truncated: String = cleaned.chars().take(MAX - 1).collect();
        format!("{truncated}…")
    } else {
        cleaned
    }
}

fn opt_cell(s: Option<&str>) -> String {
    match s {
        Some(s) => md_cell(s),
        None => "—".into(),
    }
}

fn role_cell(r: Role) -> &'static str {
    match r {
        Role::Unknown => "—",
        other => other.label(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn docs_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs")
    }

    #[test]
    fn block_is_deterministic() {
        assert_eq!(generated_block(), generated_block());
        assert_eq!(node_catalog_json(), node_catalog_json());
    }

    #[test]
    fn block_carries_markers_and_every_registered_node() {
        let block = generated_block();
        assert!(block.starts_with(BEGIN_MARKER));
        assert!(block.trim_end().ends_with(END_MARKER));
        // Completeness: every registered (non-fixture) primitive's
        // type_id appears in the block — the `node.glitch`-class drift
        // can't recur silently.
        for f in inventory::iter::<PrimitiveFactory> {
            if is_test_fixture(f.type_id) {
                continue;
            }
            assert!(
                block.contains(f.type_id),
                "registered node `{}` missing from generated index",
                f.type_id
            );
        }
    }

    #[test]
    fn regenerates_in_sync() {
        // The on-disk artifacts must match a fresh regeneration. If this
        // fails, a node changed without regenerating the docs — run
        // `cargo run -p manifold-renderer --bin gen_node_catalog`.
        let docs = docs_dir();

        let json_path = docs.join("node_catalog.json");
        let on_disk_json = std::fs::read_to_string(&json_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", json_path.display()));
        assert_eq!(
            on_disk_json,
            node_catalog_json(),
            "docs/node_catalog.json is stale — run `cargo run -p manifold-renderer --bin gen_node_catalog`"
        );

        let md_path = docs.join("NODE_CATALOG.md");
        let on_disk_md = std::fs::read_to_string(&md_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", md_path.display()));
        let regenerated = inject(&on_disk_md)
            .expect("NODE_CATALOG.md is missing the generated-block markers");
        assert_eq!(
            on_disk_md, regenerated,
            "docs/NODE_CATALOG.md generated block is stale — run `cargo run -p manifold-renderer --bin gen_node_catalog`"
        );
    }
}
