//! `node.camera_switch` — N-way camera selector.
//!
//! `num_inputs` sets how many `Camera` inputs the node exposes (`a`, `b`,
//! `c`, …) and the `select` enum picks one for `out`. A scene's live camera
//! cut is one of these with every camera's lens wired in: a cut is a param
//! write on `select`, never a rebuild. Camera-path scene modifiers mint the
//! two-input form (previous camera on `a`, the modifier's on `b`), so the
//! default of two inputs keeps their graphs as they were.
//!
//! An unwired pick falls back to the first wired input, so a lone producer
//! still reaches `out`. With `select` set as a param, the inputs it does not
//! pick are pruned from the frame.
//!
//! CPU-only: `Camera` is a value type drained per wire per frame. Hand-written
//! because the port list follows `num_inputs` (the `node.switch_texture`
//! pattern).

use std::borrow::Cow;
use std::sync::OnceLock;

use manifold_node_engine::exec::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamValues, intern_name,
};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use manifold_node_engine::scene::camera::Camera;

pub const CAMERA_SWITCH_TYPE_ID: &str = "node.camera_switch";

/// Input `index`'s port name, in pick order: `a` to `z`, then `aa`, `ab`
/// and on, as spreadsheet columns are named. There is no last one.
pub fn camera_switch_input(index: usize) -> String {
    let mut name = Vec::new();
    let mut k = index + 1;
    while k > 0 {
        k -= 1;
        name.push(b'a' + (k % 26) as u8);
        k /= 26;
    }
    name.reverse();
    String::from_utf8(name).expect("ascii letters")
}

/// The `select` labels the graph editor shows for the first inputs; a pick
/// past them is still a valid index.
const SELECT_LABELS: [&str; 26] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z",
];

const DEFAULT_INPUTS: usize = 2;

const OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Camera,
    kind: PortKind::Output,
    required: false,
}];

const PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: Cow::Borrowed("select"),
        label: "Select",
        ty: ParamType::Enum,
        default: ParamValue::Enum(0),
        range: None,
        enum_values: &SELECT_LABELS,
    },
    ParamDef {
        name: Cow::Borrowed("num_inputs"),
        label: "Input Count",
        ty: ParamType::Int,
        default: ParamValue::Float(DEFAULT_INPUTS as f32),
        range: None,
        enum_values: &[],
    },
];

pub struct CameraSwitch {
    inputs: Vec<NodeInput>,
    /// The input names, interned when the ports are built, so the per-frame
    /// pick never formats or locks.
    names: Vec<&'static str>,
}

impl CameraSwitch {
    pub fn new() -> Self {
        let mut node = Self {
            inputs: Vec::new(),
            names: Vec::new(),
        };
        node.rebuild_ports(DEFAULT_INPUTS);
        node
    }

    fn rebuild_ports(&mut self, n: usize) {
        self.names = (0..n.max(1))
            .map(|k| intern_name(&Cow::Owned(camera_switch_input(k))))
            .collect();
        self.inputs = self
            .names
            .iter()
            .map(|&name| NodePort {
                name: Cow::Borrowed(name),
                ty: PortType::Camera,
                kind: PortKind::Input,
                required: false,
            })
            .collect();
    }

    fn picked(&self, params: &ParamValues) -> &'static str {
        let index = match params.get("select") {
            Some(ParamValue::Enum(n)) => *n as usize,
            _ => 0,
        };
        self.names[index.min(self.names.len().saturating_sub(1))]
    }
}

impl Default for CameraSwitch {
    fn default() -> Self {
        Self::new()
    }
}

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(CAMERA_SWITCH_TYPE_ID))
}

impl EffectNode for CameraSwitch {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }

    fn boundary_reason(&self) -> Option<manifold_node_engine::freeze::classify::BoundaryReason> {
        Some(manifold_node_engine::freeze::classify::BoundaryReason::NonGpu)
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
        let n = params
            .get("num_inputs")
            .and_then(|v| v.as_scalar())
            .map_or(DEFAULT_INPUTS, |f| f.round().max(1.0) as usize);
        if n != self.inputs.len() {
            self.rebuild_ports(n);
        }
    }

    /// The picked input's producers run; the others are skipped. An unwired
    /// pick keeps every input live, since the fallback may need any of them.
    fn selected_input_branch(
        &self,
        params: &ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        let picked = self.picked(params);
        wired_inputs.contains(&picked).then_some(picked)
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let picked = self.picked(ctx.params);
        let used = ctx
            .inputs
            .camera(picked)
            .map(|camera| (picked, camera))
            .or_else(|| {
                self.inputs.iter().find_map(|port| {
                    ctx.inputs
                        .camera(&port.name)
                        .map(|camera| (&*port.name, camera))
                })
            });
        let camera = match used {
            Some((port, mut camera)) => {
                // A different input is a different shot: the renderer resets
                // its temporal history on the frame it changes.
                let index = self
                    .inputs
                    .iter()
                    .position(|input| input.name == port)
                    .unwrap_or(0) as u64;
                camera.shot = camera.shot.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (index + 1);
                camera
            }
            None => Camera::default_perspective(),
        };
        ctx.outputs.set_camera("out", camera);
    }
}

#[cfg(test)]
#[path = "camera_switch_shot_tests.rs"]
mod shot_tests;

inventory::submit! {
    manifold_node_engine::persistence::PrimitiveFactory {
        type_id: CAMERA_SWITCH_TYPE_ID,
        create: || Box::new(CameraSwitch::new()),
        picker: Some(manifold_node_engine::palette::PickerInfo {
            label: "Camera Switch",
            category: manifold_node_engine::palette::PaletteCategory::Driver,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_have_no_cap_and_keep_their_names() {
        assert_eq!(
            [0, 25, 26, 31, 701, 702].map(camera_switch_input),
            ["a", "z", "aa", "af", "zz", "aaa"]
        );
        let mut node = CameraSwitch::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("num_inputs"), ParamValue::Float(40.0));
        params.insert(Cow::Borrowed("select"), ParamValue::Enum(39));
        node.reconfigure(&params);
        assert_eq!(node.inputs().len(), 40);
        assert_eq!(node.selected_input_branch(&params, &["an"]), Some("an"));
    }
}
