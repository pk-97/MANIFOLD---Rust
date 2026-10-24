//! `node.loop_cell` — how long one Scene Loop tile is: the rest boxes of
//! what the loop tiles, measured along the corridor, plus a gap.
//!
//! `boxes` sets how many `box_k` / `within_k` pairs the node exposes, with
//! no cap. A box is a transform that maps the unit cube onto it (its
//! position the box's centre, its scale the box's size), inside the frame
//! `within_k` (the world when unwired): an object's rest box under its
//! transform, a copy group's `rest_box`, a swarm's lattice box in its
//! emitter's frame. The cell is the length of their union along `axis` plus
//! `gap`, so the tiles follow the live Transform, Copies and swarm rows as
//! Blender's Array follows its object's size, with Spacing the constant gap
//! on top. A box of no size at all measures nothing (a flat one still has
//! a place); with nothing measured the cell is the gap.
//!
//! The boxes are rest poses: the compiler wires where things start, never
//! where physics moves them, so a falling body never moves the tiles.
//!
//! CPU-only. Hand-written because the port list follows `boxes`
//! (`node.merge_meshes` pattern).

use std::borrow::Cow;
use std::sync::OnceLock;

use manifold_core::scene_doc::transform::mat4_mul;
use manifold_node_engine::exec::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::{
    NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
};

pub const LOOP_CELL_TYPE_ID: &str = "node.loop_cell";

/// The shortest cell: the corridor divides by the cell to find the
/// camera's place in it, so a cell of no length has no place.
pub const MIN_CELL: f32 = 0.01;

const DEFAULT_BOXES: usize = 1;
/// Ports before the box pairs.
const FIXED_INPUTS: usize = 1;
/// Ports per box: the box, the frame it sits in.
const BOX_PORTS: usize = 2;

const OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("cell_size"),
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Output,
    required: false,
}];

const PARAMS: [ParamDef; 3] = [
    ParamDef {
        name: Cow::Borrowed("boxes"),
        label: "Boxes",
        ty: ParamType::Int,
        default: ParamValue::Float(DEFAULT_BOXES as f32),
        // The slider's reach; typed values go past it.
        range: Some((1.0, 64.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("axis"),
        label: "Axis",
        ty: ParamType::Enum,
        default: ParamValue::Enum(4), // +Z, as node.scene_array
        range: None,
        enum_values: super::scene_array::AXIS_LABELS,
    },
    ParamDef {
        name: Cow::Borrowed("gap"),
        label: "Gap",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 1000.0)),
        enum_values: &[],
    },
];

pub struct LoopCell {
    inputs: Vec<NodeInput>,
}

impl LoopCell {
    pub fn new() -> Self {
        let mut node = Self { inputs: Vec::new() };
        node.rebuild_ports(DEFAULT_BOXES);
        node
    }

    fn rebuild_ports(&mut self, boxes: usize) {
        let port = |name: Cow<'static, str>, ty| NodePort {
            name,
            ty,
            kind: PortKind::Input,
            required: false,
        };
        self.inputs = vec![port(
            Cow::Borrowed("gap"),
            PortType::Scalar(ScalarType::F32),
        )];
        for k in 0..boxes.max(1) {
            self.inputs
                .push(port(Cow::Owned(format!("box_{k}")), PortType::Transform));
            self.inputs
                .push(port(Cow::Owned(format!("within_{k}")), PortType::Transform));
        }
    }

    fn boxes(&self) -> usize {
        (self.inputs.len() - FIXED_INPUTS) / BOX_PORTS
    }
}

impl Default for LoopCell {
    fn default() -> Self {
        Self::new()
    }
}

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(LOOP_CELL_TYPE_ID))
}

/// The cell for boxes whose world matrices are `boxes`: the length of their
/// union along world axis `axis` (0, 1, 2), plus `gap`, at least
/// [`MIN_CELL`].
pub fn cell_size(boxes: impl IntoIterator<Item = [[f32; 4]; 4]>, axis: usize, gap: f32) -> f32 {
    let mut span: Option<(f32, f32)> = None;
    for m in boxes {
        // A box of no size at all is nothing; a flat one still has a place.
        let size: f32 = m[..3]
            .iter()
            .flat_map(|column| column[..3].iter())
            .map(|v| v.abs())
            .sum();
        // The unit cube's half extent along the axis, through the matrix.
        let reach = 0.5 * (m[0][axis].abs() + m[1][axis].abs() + m[2][axis].abs());
        let centre = m[3][axis];
        if !(size > 0.0 && size.is_finite() && centre.is_finite()) {
            continue;
        }
        let (lo, hi) = (centre - reach, centre + reach);
        span = Some(span.map_or((lo, hi), |(min, max)| (min.min(lo), max.max(hi))));
    }
    let depth = span.map_or(0.0, |(lo, hi)| hi - lo);
    (depth + gap).max(MIN_CELL)
}

impl EffectNode for LoopCell {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }

    fn boundary_reason(&self) -> Option<manifold_node_engine::freeze::classify::BoundaryReason> {
        Some(manifold_node_engine::freeze::classify::BoundaryReason::NonGpu)
    }

    fn param_contract(&self, param_name: &str) -> Option<manifold_core::effects::RangeContract> {
        (param_name == "boxes").then_some(manifold_core::effects::RangeContract {
            min: Some(1.0),
            max: None,
            reason: manifold_core::effects::RangeReason::Count,
        })
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        &OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &PARAMS
    }

    fn reconfigure(&mut self, params: &ParamValues) {
        let boxes = params
            .get("boxes")
            .and_then(|v| v.as_scalar())
            .map_or(DEFAULT_BOXES, |f| f.round().max(1.0) as usize);
        if boxes != self.boxes() {
            self.rebuild_ports(boxes);
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => (*n as usize / 2).min(2),
            _ => 2,
        };
        let gap = ctx.scalar_or_param("gap", 0.0);
        let inputs = &ctx.inputs;
        let boxes = self.inputs[FIXED_INPUTS..]
            .chunks_exact(BOX_PORTS)
            .filter_map(|ports| {
                let placed = inputs.transform(&ports[0].name)?.world();
                Some(match inputs.transform(&ports[1].name) {
                    Some(within) => mat4_mul(within.world(), placed),
                    None => placed,
                })
            });
        let cell = cell_size(boxes, axis, gap);
        ctx.outputs.set_scalar("cell_size", ParamValue::Float(cell));
    }
}

inventory::submit! {
    manifold_node_engine::persistence::PrimitiveFactory {
        type_id: LOOP_CELL_TYPE_ID,
        create: || Box::new(LoopCell::new()),
        picker: Some(manifold_node_engine::palette::PickerInfo {
            label: "Loop Cell",
            category: manifold_node_engine::palette::PaletteCategory::Driver,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::scene_doc::transform::trs_matrix;

    #[test]
    fn boxes_size_the_port_list_with_no_cap() {
        let mut node = LoopCell::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("boxes"), ParamValue::Float(100.0));
        node.reconfigure(&params);
        assert_eq!(node.boxes(), 100);
        assert_eq!(
            node.inputs.last().map(|port| port.name.as_ref()),
            Some("within_99")
        );
    }

    /// The cell is the union's length along the axis plus the gap: a turned
    /// box reaches as far as its corners do, a box of no size or a lost one
    /// measures nothing, and nothing measured leaves the gap.
    #[test]
    fn the_cell_is_the_union_along_the_axis_plus_the_gap() {
        let cube = trs_matrix([0.0; 3], [0.0; 3], [1.0; 3]);
        let far = trs_matrix([0.0, 0.0, 4.0], [0.0; 3], [2.0; 3]);
        assert_eq!(
            cell_size([cube, far], 2, 0.5),
            5.5 + 0.5,
            "from -0.5 to 5 along Z, then the gap"
        );
        assert_eq!(
            cell_size([cube, far], 0, 0.0),
            2.0,
            "across X the two overlap"
        );
        let turned = trs_matrix([0.0; 3], [0.0, std::f32::consts::FRAC_PI_4, 0.0], [1.0; 3]);
        assert!(
            (cell_size([turned], 2, 0.0) - std::f32::consts::SQRT_2).abs() < 1e-6,
            "a turned cube reaches its corners"
        );
        let flat = trs_matrix([0.0, 0.0, 9.0], [0.0; 3], [0.0; 3]);
        let lost = trs_matrix([f32::NAN; 3], [0.0; 3], [1.0; 3]);
        assert_eq!(
            cell_size([cube, flat, lost], 2, 0.0),
            1.0,
            "a box of no size or no place measures nothing"
        );
        let plane = trs_matrix([0.0, 0.0, 3.0], [0.0; 3], [1.0, 1.0, 0.0]);
        assert_eq!(
            cell_size([cube, plane], 2, 0.0),
            3.5,
            "a flat box still has a place"
        );
        assert_eq!(
            cell_size([], 2, 3.0),
            3.0,
            "nothing measured leaves the gap"
        );
        assert_eq!(cell_size([], 2, 0.0), MIN_CELL, "a cell always has length");
    }
}
