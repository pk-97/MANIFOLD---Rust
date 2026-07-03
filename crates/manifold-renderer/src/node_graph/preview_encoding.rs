//! Semantic preview encoding — how a node's output texture is shown in the
//! editor's node-output preview.
//!
//! One tone curve can't serve every output: a colour image wants raw, a
//! density wants its blacks kept black and lifted, a force field wants its
//! direction and magnitude shown, a normal map wants its xyz decoded, a depth
//! buffer wants a near-far ramp — the same way Unreal's buffer visualization
//! decodes each G-buffer channel its own way rather than with a single
//! brightness slider.
//!
//! The signal isn't in the pixels (nearly every graph texture is allocated
//! `rgba16float` regardless of what it carries), it's in the node and, crucially,
//! in the data flowing *into* it. A Gaussian Blur's own descriptor can't know
//! whether it's blurring a colour image or a force field — but the node feeding
//! it does. So the encoding follows the **data**, not the node:
//!
//! - [`PreviewEncoding::declared_kind`] asks whether a node *asserts* an output
//!   kind from its own identity (a field generator declares a vector field, a
//!   mask node declares a scalar, a colour grade declares colour). A pure
//!   *filter* — blur, warp, stylize — asserts nothing and returns `None`.
//! - [`PreviewEncoding::propagate`] walks the flattened graph: declaring nodes
//!   seed their kind, filters inherit their primary input's kind. After one
//!   pass a blurred force field is *known* to be a force field everywhere, so
//!   selecting the inner blur previews it as a flow wheel with zero guessing.
//! - [`PreviewEncoding::derive`] is the single-node fallback (and the
//!   group-interface port-name path), used when no propagated kind is on hand.

use crate::node_graph::descriptor::{Category, Role, descriptor_for};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;

/// Live scalar I/O of a previewed node: `(inputs, outputs)`, each entry
/// `(port_name, value)`. Captured for the editor's value inspector when a
/// previewed node has no image output.
pub type PreviewScalarIo = (Vec<(String, f32)>, Vec<(String, f32)>);

/// Live (post-binding-apply, post-modulation) scalar param values for every
/// node of the watched effect/generator this frame, keyed by stable
/// [`NodeId`]. Each node carries `(param_name, value)` pairs read straight off
/// the running graph, so the editor canvas can show what a driver / Ableton /
/// envelope / card slider is *actually* doing to each knob this frame instead
/// of the frozen authoring default. Param names are `&'static` (they come from
/// the primitive's `ParamDef`), so the per-frame tap allocates only the small
/// outer/inner `Vec`s, never the strings. Empty when nothing is watched.
pub type LiveNodeParams = Vec<(NodeId, Vec<(&'static str, f32)>)>;

/// How the node-output preview should render a captured texture. Six fixed
/// encodings cover the data the graph actually carries; anything unclassified
/// falls to [`PreviewEncoding::ScalarLift`], which keeps blacks black and lifts
/// the dark — strictly safer than a raw blit on a near-black field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewEncoding {
    /// Show the texture as-is. Colour images, composites, final outputs —
    /// already meant to be looked at.
    #[default]
    Color,
    /// Unsigned scalar / data field: 0 stays black, dark values lifted by a
    /// fixed asinh curve. Density, masks, luminance, occlusion, and the safe
    /// default for anything not yet classified.
    ScalarLift,
    /// Signed scalar centred at 0: diverging blue → black → red ramp. Signed
    /// distance fields, divergence, signed height, anything that swings either
    /// side of zero where the sign carries meaning.
    ScalarSigned,
    /// 2D vector field: direction → hue, magnitude → brightness (the standard
    /// optical-flow colour wheel). Force fields, flow, gradients,
    /// displacement, UV / coordinate maps.
    VectorField,
    /// Tangent-space normal map: xyz in `[-1,1]` decoded to the familiar
    /// blue-dominant RGB. Surface normals, bump output.
    Normal,
    /// Depth / disparity buffer: a perceptual near-far colour ramp (turbo-like)
    /// so subtle distance gradients read at a glance instead of washing to a
    /// near-flat grey.
    Depth,
}

impl PreviewEncoding {
    /// Derive an encoding for a single previewed output from its node `type_id`
    /// and the name of the port being shown — for a group that's the interface
    /// output name (`forceField`), otherwise the producing node's output port.
    /// The single-node fallback when no propagated kind is available; unknown
    /// nodes fall to the safe `ScalarLift`.
    pub fn derive(type_id: &str, port_name: &str) -> Self {
        Self::declared_kind(type_id, port_name).unwrap_or(Self::ScalarLift)
    }

    /// The encoding a *port name* alone forces, if it is semantically loaded
    /// (`forceField`, `depth`, `normalMap`, `signedDist`, `density`, `colour`).
    /// `None` for a generic name (`out`, `result`). This is the strongest
    /// signal — a blur feeding a group's `forceField` output is a force field
    /// regardless of the blur's own descriptor — so the group path consults it
    /// before the propagated producer kind.
    pub fn from_port_name(port_name: &str) -> Option<Self> {
        let p = port_name.to_ascii_lowercase();
        if name_is_normal(&p) {
            return Some(Self::Normal);
        }
        if name_is_depth(&p) {
            return Some(Self::Depth);
        }
        if name_is_vector(&p) {
            return Some(Self::VectorField);
        }
        if name_is_signed(&p) {
            return Some(Self::ScalarSigned);
        }
        if name_is_scalar(&p) {
            return Some(Self::ScalarLift);
        }
        if name_is_color(&p) {
            return Some(Self::Color);
        }
        None
    }

    /// The kind a node *asserts* from its own identity, or `None` if it's a
    /// pure filter that should inherit its input's kind. Priority: a loaded
    /// output-port name, then `type_id` keywords, then the descriptor
    /// category / role. Filter categories (blur, warp, stylize) and the
    /// passthrough categories (math, routing, noise…) return `None` so
    /// propagation carries the upstream kind through them.
    pub fn declared_kind(type_id: &str, port_name: &str) -> Option<Self> {
        if let Some(by_port) = Self::from_port_name(port_name) {
            return Some(by_port);
        }

        let tid = type_id.to_ascii_lowercase();
        if name_is_normal(&tid) {
            return Some(Self::Normal);
        }
        if name_is_depth(&tid) {
            return Some(Self::Depth);
        }
        if name_is_vector(&tid) {
            return Some(Self::VectorField);
        }
        if name_is_signed(&tid) {
            return Some(Self::ScalarSigned);
        }
        if name_is_scalar(&tid) {
            return Some(Self::ScalarLift);
        }
        if name_is_color(&tid) {
            return Some(Self::Color);
        }

        if let Some(d) = descriptor_for(type_id) {
            match d.category {
                Category::FieldsAndCoordinates => return Some(Self::VectorField),
                Category::Mask => return Some(Self::ScalarLift),
                Category::DetectionAndSampling => return Some(Self::ScalarLift),
                Category::ColorAndTone
                | Category::Composite
                | Category::Generate
                | Category::MaterialsAndLighting
                | Category::Geometry3D => return Some(Self::Color),
                // Filters and passthroughs assert nothing — they take the kind
                // of whatever flows in. Inheriting is the whole point of
                // propagation: a blurred / warped / stylised field is still
                // that field.
                Category::BlurAndSharpen
                | Category::DistortAndWarp
                | Category::Stylize
                | Category::Noise
                | Category::MathAndConvert
                | Category::Routing
                | Category::Control
                | Category::Particles2D
                | Category::Particles3D
                | Category::Uncategorized => {}
            }
            if d.role == Role::Sink {
                return Some(Self::Color);
            }
        }

        None
    }

    /// Propagate a data-kind to every node in a **flattened** graph, keyed by
    /// stable [`NodeId`]. Declaring nodes (sources, field generators, masks,
    /// colour ops) seed their kind; filter / passthrough nodes inherit their
    /// primary texture input's kind. Nodes that still resolve to nothing (an
    /// undeclared node with no determined input) fall to `ScalarLift`.
    ///
    /// Built once at chain / generator build, next to
    /// [`group_output_producer_map`](manifold_core::flatten::group_output_producer_map),
    /// and consulted by `set_preview_target` so the previewed node's encoding
    /// follows the data through any number of intervening filters.
    pub fn propagate(def: &EffectGraphDef) -> ahash::AHashMap<NodeId, Self> {
        // `None` = transparent / not-yet-resolved. Seed each node with what it
        // declares from its own identity (output-port name unknown here, so the
        // `type_id` + descriptor signal carries declaration).
        let mut kind: ahash::AHashMap<u32, Option<Self>> = def
            .nodes
            .iter()
            .map(|n| (n.id, Self::declared_kind(&n.type_id, "")))
            .collect();

        // Primary input source per node: the upstream node feeding its main
        // texture port. Choose by a port-name preference so a `src`/`in` wins
        // over an auxiliary `mask`/`amount` input; ties keep the first wire.
        let mut primary_src: ahash::AHashMap<u32, (i32, u32)> = ahash::AHashMap::new();
        for w in &def.wires {
            let score = input_port_rank(&w.to_port);
            match primary_src.get(&w.to_node) {
                Some((best, _)) if *best <= score => {}
                _ => {
                    primary_src.insert(w.to_node, (score, w.from_node));
                }
            }
        }

        // Fixed-point inheritance. One hop per pass; `nodes.len()` passes
        // converges any DAG, and a transparent cycle with no external seed
        // simply stays `None` (→ fallback) rather than looping.
        for _ in 0..=def.nodes.len() {
            let mut changed = false;
            for n in &def.nodes {
                if kind.get(&n.id).copied().flatten().is_some() {
                    continue; // declared or already inherited
                }
                if let Some((_, src)) = primary_src.get(&n.id)
                    && let Some(Some(upstream)) = kind.get(src).copied()
                {
                    kind.insert(n.id, Some(upstream));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        def.nodes
            .iter()
            .map(|n| {
                (
                    n.node_id.clone(),
                    kind.get(&n.id).copied().flatten().unwrap_or(Self::ScalarLift),
                )
            })
            .collect()
    }
}

/// Lower rank = stronger candidate for a node's primary (main image) input.
fn input_port_rank(port: &str) -> i32 {
    match port.to_ascii_lowercase().as_str() {
        "src" | "source" | "in" | "input" => 0,
        "tex" | "texture" | "image" | "img" => 1,
        "color" | "colour" | "a" | "base" => 2,
        _ => 10,
    }
}

fn name_is_vector(s: &str) -> bool {
    // NB: keep keywords specific — a loose `grad` would false-match `color_grade`.
    [
        "force", "flow", "velocity", "gradient", "vector", "displace", "curl", "vortic", "uv",
        "coord",
    ]
    .iter()
    .any(|k| s.contains(k))
}

fn name_is_scalar(s: &str) -> bool {
    ["density", "mask", "lumin", "occlus", "falloff"]
        .iter()
        .any(|k| s.contains(k))
}

fn name_is_signed(s: &str) -> bool {
    ["sdf", "signed", "divergence", "diverg", "distance"]
        .iter()
        .any(|k| s.contains(k))
}

fn name_is_normal(s: &str) -> bool {
    ["normal", "tangent", "bump"].iter().any(|k| s.contains(k))
}

fn name_is_depth(s: &str) -> bool {
    ["depth", "disparity", "zbuffer", "z_buffer"]
        .iter()
        .any(|k| s.contains(k))
}

fn name_is_color(s: &str) -> bool {
    [
        "color", "colour", "albedo", "image", "composite", "display", "final",
    ]
    .iter()
    .any(|k| s.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{
        EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn group_force_field_port_picks_vector() {
        // The headline case: a group's `forceField` output, even though the
        // producer behind it is a blur node.
        assert_eq!(
            PreviewEncoding::derive("node.gaussian_blur", "forceField"),
            PreviewEncoding::VectorField
        );
    }

    #[test]
    fn field_node_descriptor_picks_vector() {
        // gradient_central_diff / rotate_vec2_by_angle are FieldsAndCoordinates.
        assert_eq!(
            PreviewEncoding::derive("node.edge_slope", "out"),
            PreviewEncoding::VectorField
        );
        assert_eq!(
            PreviewEncoding::derive("node.rotate_vector", "out"),
            PreviewEncoding::VectorField
        );
    }

    #[test]
    fn density_port_picks_scalar() {
        assert_eq!(
            PreviewEncoding::derive("node.gaussian_blur", "blurredDensity"),
            PreviewEncoding::ScalarLift
        );
    }

    #[test]
    fn new_encoding_port_names_resolve() {
        assert_eq!(
            PreviewEncoding::derive("node.whatever", "normalMap"),
            PreviewEncoding::Normal
        );
        assert_eq!(
            PreviewEncoding::derive("node.whatever", "sceneDepth"),
            PreviewEncoding::Depth
        );
        assert_eq!(
            PreviewEncoding::derive("node.whatever", "signedDist"),
            PreviewEncoding::ScalarSigned
        );
    }

    #[test]
    fn filter_declares_nothing_unknown_falls_to_scalar() {
        // A blur asserts no kind of its own (it's transparent to its input).
        assert_eq!(PreviewEncoding::declared_kind("node.gaussian_blur", "out"), None);
        // …so the single-node fallback for a generic port is the safe lift.
        assert_eq!(
            PreviewEncoding::derive("node.gaussian_blur", "out"),
            PreviewEncoding::ScalarLift
        );
        // A type with no descriptor and a generic port → also safe ScalarLift.
        assert_eq!(
            PreviewEncoding::derive("node.totally_unknown_xyz", "out"),
            PreviewEncoding::ScalarLift
        );
    }

    // ── propagation ──

    fn node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(node_id),
            type_id: type_id.to_string(),
            handle: None,
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

    fn def(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes,
            wires,
        }
    }

    #[test]
    fn blur_inherits_field_kind_through_propagation() {
        // field_gen (vector) -> blur (transparent) -> blur2 (transparent).
        // Selecting either blur should preview as a vector field.
        let d = def(
            vec![
                node(0, "field", "node.edge_slope"),
                node(1, "blur", "node.gaussian_blur"),
                node(2, "blur2", "node.gaussian_blur"),
            ],
            vec![wire(0, "out", 1, "src"), wire(1, "out", 2, "src")],
        );
        let kinds = PreviewEncoding::propagate(&d);
        assert_eq!(kinds[&NodeId::new("field")], PreviewEncoding::VectorField);
        assert_eq!(
            kinds[&NodeId::new("blur")],
            PreviewEncoding::VectorField,
            "blur should inherit the field kind, not assert Color"
        );
        assert_eq!(
            kinds[&NodeId::new("blur2")],
            PreviewEncoding::VectorField,
            "kind propagates through a chain of filters"
        );
    }

    #[test]
    fn color_source_propagates_through_blur() {
        let d = def(
            vec![
                node(0, "grade", "node.color_grade"),
                node(1, "blur", "node.gaussian_blur"),
            ],
            vec![wire(0, "out", 1, "src")],
        );
        let kinds = PreviewEncoding::propagate(&d);
        assert_eq!(kinds[&NodeId::new("blur")], PreviewEncoding::Color);
    }

    #[test]
    fn primary_input_wins_over_auxiliary_mask() {
        // A blur fed a colour image on `src` and a scalar on `mask` should
        // inherit the colour, not the mask.
        let d = def(
            vec![
                node(0, "grade", "node.color_grade"),
                node(1, "m", "node.luminance_mask"),
                node(2, "blur", "node.gaussian_blur"),
            ],
            vec![wire(0, "out", 2, "src"), wire(1, "out", 2, "mask")],
        );
        let kinds = PreviewEncoding::propagate(&d);
        assert_eq!(kinds[&NodeId::new("blur")], PreviewEncoding::Color);
    }

    #[test]
    fn undetermined_node_falls_to_scalar_lift() {
        let d = def(vec![node(0, "lone", "node.gaussian_blur")], vec![]);
        let kinds = PreviewEncoding::propagate(&d);
        assert_eq!(kinds[&NodeId::new("lone")], PreviewEncoding::ScalarLift);
    }
}
