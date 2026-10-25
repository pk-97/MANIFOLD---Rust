//! Render the registered node index from the live registry: every node with its ports and params. `gen_node_catalog` prints it.

use std::fmt::Write as _;

use manifold_node_engine::descriptor::{Category, NodeDescriptor, Role, descriptor_for};
use manifold_node_engine::palette::PaletteCategory;
use manifold_node_engine::parameters::{ParamType, ParamValue};
use manifold_node_engine::persistence::PrimitiveFactory;
use manifold_node_engine::ports::{PortType, ScalarType};

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
    ty: ParamType,
    default: String,
    range: Option<(f32, f32)>,
    enum_values: &'static [&'static str],
}

/// Test-only fixture primitives (`node.__smoke_test*`) register in the
/// inventory under `cfg(test)` but not in a normal build. They are skipped
/// so the index lists the same nodes in both builds.
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
                inputs: node.inputs().iter().map(port_row).collect(),
                outputs: node.outputs().iter().map(port_row).collect(),
                params: node.parameters().iter().map(param_row).collect(),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.type_id.cmp(b.type_id));
    rows
}

fn port_row(p: &manifold_node_engine::ports::NodePort) -> PortRow {
    PortRow {
        name: p.name.to_string(),
        ty: port_type_str(&p.ty),
        required: p.required,
    }
}

fn param_row(p: &manifold_node_engine::parameters::ParamDef) -> ParamRow {
    ParamRow {
        name: manifold_node_engine::exec::effect_node::intern_name(&p.name),
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
        PortType::Transform => "Transform".into(),
        PortType::Atmosphere => "Atmosphere".into(),
        PortType::RenderMode => "RenderMode".into(),
        PortType::RigidBody => "RigidBody".into(),
        PortType::FluidRole => "FluidRole".into(),
        PortType::MeshSource => "MeshSource".into(),
        PortType::VectorField => "VectorField".into(),
        PortType::Object => "Object".into(),
    }
}

fn param_type_str(t: ParamType) -> &'static str {
    match t {
        ParamType::Float => "float",
        ParamType::Angle => "angle",
        ParamType::Frequency => "frequency",
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
    // `{}` already trims trailing zeros (1.0 → "1", 0.25 → "0.25").
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

// ─── node index ──────────────────────────────────────────────────────

/// Render the registered node index as plain text, grouped by category.
/// Each node shows its picker label, type_id, role, summary, aliases, ports
/// and params. `gen_node_catalog` prints this.
pub fn node_index() -> String {
    let rows = collect_rows();
    let mut out = String::new();
    let _ = writeln!(out, "{} nodes registered, grouped by category.", rows.len());

    for &cat in Category::ALL {
        let group: Vec<&NodeRow> = rows.iter().filter(|r| r.category == cat).collect();
        if group.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\n## {} ({})", cat.label(), group.len());
        for r in group {
            out.push('\n');
            let _ = write!(
                out,
                "{}  {}",
                r.label.unwrap_or("(no picker label)"),
                r.type_id
            );
            if !matches!(r.role, Role::Unknown) {
                let _ = write!(out, "  [{}]", r.role.label());
            }
            if r.stratum == Stratum::Unlisted {
                out.push_str("  (not in palette)");
            }
            out.push('\n');
            // Prefer the friendly summary when one is filled, falling back to
            // the first sentence of the technical purpose.
            let blurb = if r.summary.is_empty() {
                first_sentence(r.purpose)
            } else {
                r.summary
            };
            if !blurb.is_empty() {
                let _ = writeln!(out, "  {blurb}");
            }
            if !r.aliases.is_empty() {
                let _ = writeln!(out, "  aliases: {}", r.aliases.join(", "));
            }
            let _ = writeln!(out, "  in:  {}", port_list(&r.inputs, true));
            let _ = writeln!(out, "  out: {}", port_list(&r.outputs, false));
            for p in &r.params {
                let _ = writeln!(out, "  param {}", param_line(p));
            }
        }
    }
    out
}

/// `name: type` per port, comma separated. Input ports that are not required
/// get an `(optional)` mark.
fn port_list(ports: &[PortRow], mark_optional: bool) -> String {
    if ports.is_empty() {
        return "none".into();
    }
    ports
        .iter()
        .map(|p| {
            if mark_optional && !p.required {
                format!("{}: {} (optional)", p.name, p.ty)
            } else {
                format!("{}: {}", p.name, p.ty)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn param_line(p: &ParamRow) -> String {
    let mut s = format!("{} ({}) = {}", p.name, param_type_str(p.ty), p.default);
    if !p.enum_values.is_empty() {
        let _ = write!(s, " [{}]", p.enum_values.join("|"));
    }
    if let Some((lo, hi)) = p.range {
        let _ = write!(s, " range {}..{}", fmt_f32(lo), fmt_f32(hi));
    }
    s
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Completeness gate: every
    /// palette-visible node (Atom or Driver stratum — hidden/legacy/
    /// `system.*` nodes are `Unlisted`, from having no `picker:`, and are
    /// exempt) must ship a non-empty label, summary, category, and alias
    /// list. Vocabulary becomes a merge requirement paid by the node's
    /// author, when context is richest.
    #[test]
    fn palette_visible_nodes_have_complete_descriptors() {
        for row in collect_rows() {
            if row.stratum == Stratum::Unlisted {
                continue;
            }
            assert!(
                row.label.is_some_and(|l| !l.is_empty()),
                "palette node `{}` has an empty picker label",
                row.type_id
            );
            assert!(
                !row.summary.is_empty(),
                "palette node `{}` has an empty summary",
                row.type_id
            );
            assert!(
                row.category != Category::Uncategorized,
                "palette node `{}` has no category",
                row.type_id
            );
            assert!(
                !row.aliases.is_empty(),
                "palette node `{}` has no aliases",
                row.type_id
            );
        }
    }
}
