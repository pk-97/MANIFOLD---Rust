//! One event clock per Math View modifier, shared across all presentation modes.
use super::*;
use crate::node_graph::primitives::{BeatEnvelopeDurations, BeatEnvelopeState};
use crate::node_graph::scene_modifier_expand::math_resource_node_id;
use manifold_core::scene_modifier_preset::SceneModifierInstanceDef;

pub(super) struct MathEvents {
    controls: Vec<(&'static str, NodeInstanceId, f32)>,
    masks: Vec<NodeInstanceId>,
    variant_masks: Vec<Vec<NodeInstanceId>>,
    diagrams: Vec<Vec<NodeInstanceId>>,
    pulse: BeatEnvelopeState,
    scan: BeatEnvelopeState,
}
impl MathEvents {
    pub fn prepare(
        modifier: &SceneModifierInstanceDef,
        parent: &PresetRuntime,
        variants: &[PresetRuntime],
        controls: Vec<(&'static str, NodeInstanceId, f32)>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let resolve = |runtime: &PresetRuntime,
                       role: &str|
         -> Result<Vec<NodeInstanceId>, JsonGeneratorLoadError> {
            modifier
                .mesh_frames
                .iter()
                .map(|frame| {
                    runtime
                        .graph
                        .instance_by_node_id(&math_resource_node_id(
                            &modifier.id,
                            &frame.target,
                            role,
                        ))
                        .ok_or_else(|| {
                            super::math_view::invalid(format!(
                                "missing Math View {role} for {:?}",
                                frame.target
                            ))
                        })
                })
                .collect()
        };
        let mut variant_masks = Vec::with_capacity(variants.len());
        let mut diagrams = Vec::with_capacity(variants.len());
        for variant in variants {
            variant_masks.push(resolve(variant, "weights")?);
            let mut variant_diagrams = resolve(variant, "diagram")?;
            variant_diagrams.extend(resolve(variant, "surface")?);
            diagrams.push(variant_diagrams);
        }
        Ok(Self {
            controls,
            masks: resolve(parent, "weights")?,
            variant_masks,
            diagrams,
            pulse: Default::default(),
            scan: Default::default(),
        })
    }
    pub fn clear(&mut self) {
        self.pulse = Default::default();
        self.scan = Default::default();
    }
    pub fn carry_from(&mut self, previous: &Self) {
        self.pulse = previous.pulse.clone();
        self.scan = previous.scan.clone();
    }
    pub fn tick(
        &mut self,
        graph: &mut Graph,
        variants: &mut [PresetRuntime],
        count: f32,
        baseline: f32,
        beat: Beats,
        enabled: bool,
    ) {
        let value = |name: &str| {
            let (_, node, default) = self
                .controls
                .iter()
                .find(|c| c.0 == name)
                .expect("prepared Math View control");
            graph
                .get_node(*node)
                .and_then(|n| n.params.get("value"))
                .and_then(ParamValue::as_scalar)
                .filter(|v| v.is_finite())
                .unwrap_or(*default)
        };
        if !enabled {
            // A bypassed Math View is fully neutral on the scene and does not
            // advance its event clocks.
            for &mask in &self.masks {
                write_mask(
                    graph,
                    mask,
                    MaskSettings {
                        gain: 1.0,
                        amount: 0.0,
                        progress: 0.0,
                        width: 0.2,
                        direction: 2,
                        reveal: false,
                        enabled: false,
                    },
                );
            }
            return;
        }
        let pulse_window = if value("pulse_trigger") > 0.5 {
            value("pulse_beats").max(0.0)
        } else {
            0.0
        };
        let scan_window = if value("scan_trigger") > 0.5 {
            value("scan_beats").max(0.0)
        } else {
            0.0
        };
        let pulse = self
            .pulse
            .step(
                count,
                Some(baseline),
                beat,
                BeatEnvelopeDurations {
                    window: pulse_window,
                    ..Default::default()
                },
            )
            .expect("finite event stream")
            .0;
        let elapsed = self
            .scan
            .step(
                count,
                Some(baseline),
                beat,
                BeatEnvelopeDurations {
                    window: scan_window,
                    ..Default::default()
                },
            )
            .expect("finite event stream")
            .1;
        let gain =
            (1.0 + value("pulse_strength") * (value("pulse") + pulse).clamp(0.0, 1.0)).max(0.0);
        let scan_amount =
            value("scan_amount")
                .clamp(0.0, 1.0)
                .max(if elapsed >= 0.0 { 1.0 } else { 0.0 });
        let progress = (value("scan_progress")
            + if elapsed >= 0.0 && scan_window > 0.0 {
                elapsed / scan_window
            } else {
                0.0
            })
        .clamp(0.0, 1.0);
        let width = value("scan_width").max(0.000001);
        let direction = value("scan_direction").round().clamp(0.0, 5.0) as u32;
        let reveal = value("scan_mode") >= 0.5;
        let connected = value("connect_mesh") >= 0.5;
        let pulse_mesh = value("pulse_target").round() != 1.0;
        let scan_mesh = value("scan_target").round() != 1.0;
        let scene_gain = if pulse_mesh { gain } else { 1.0 };
        let scene_amount = if scan_mesh { scan_amount } else { 0.0 };
        for &mask in &self.masks {
            write_mask(
                graph,
                mask,
                MaskSettings {
                    gain: scene_gain,
                    amount: scene_amount,
                    progress,
                    width,
                    direction,
                    reveal,
                    enabled: connected,
                },
            );
        }
        for (variant, (masks, diagrams)) in variants
            .iter_mut()
            .zip(self.variant_masks.iter().zip(&self.diagrams))
        {
            for &mask in masks {
                write_mask(
                    &mut variant.graph,
                    mask,
                    MaskSettings {
                        gain: 1.0,
                        amount: scan_amount,
                        progress,
                        width,
                        direction,
                        reveal,
                        enabled: true,
                    },
                );
            }
            for &diagram in diagrams {
                for (name, value) in [
                    ("pulse_gain", gain),
                    ("scan_amount", scan_amount),
                    ("scan_progress", progress),
                    ("scan_width", width),
                    ("scan_direction", direction as f32),
                    ("scan_mode", if reveal { 1.0 } else { 0.0 }),
                ] {
                    variant
                        .graph
                        .set_param_unchecked(diagram, name, ParamValue::Float(value));
                }
            }
        }
    }
}

struct MaskSettings {
    gain: f32,
    amount: f32,
    progress: f32,
    width: f32,
    direction: u32,
    reveal: bool,
    enabled: bool,
}

fn write_mask(graph: &mut Graph, node: NodeInstanceId, settings: MaskSettings) {
    let MaskSettings {
        gain,
        amount,
        progress,
        width,
        direction,
        reveal,
        enabled,
    } = settings;
    let (yaw, pitch) = match direction {
        0 => (std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2),
        1 => (-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2),
        2 => (0.0, 0.0),
        3 => (0.0, std::f32::consts::PI),
        4 => (0.0, std::f32::consts::FRAC_PI_2),
        _ => (0.0, -std::f32::consts::FRAC_PI_2),
    };
    let position = (2.0 * progress - 1.0) * (1.0 + width);
    let sign = if direction % 2 == 0 { 1.0 } else { -1.0 };
    let center = match direction / 2 {
        0 => [position * sign, 0.0, 0.0],
        1 => [0.0, position * sign, 0.0],
        _ => [0.0, 0.0, position * sign],
    };
    let (mut low, mut high) = if reveal {
        (gain * (1.0 - amount), gain)
    } else {
        (gain, gain * (1.0 + amount))
    };
    // Exact reveal endpoints include cells at the calibrated boundary.
    if reveal && progress <= 0.0 {
        high = low;
    }
    if reveal && progress >= 1.0 {
        low = high;
    }
    graph.set_param_unchecked(node, "shape", ParamValue::Enum(if reveal { 2 } else { 0 }));
    for (name, value) in [
        ("amount", if enabled { 1.0 } else { 0.0 }),
        ("low", low),
        ("high", high),
        ("width", if reveal { 0.0 } else { width }),
        ("center_x", center[0]),
        ("center_y", center[1]),
        ("center_z", center[2]),
        ("yaw", yaw),
        ("pitch", pitch),
    ] {
        graph.set_param_unchecked(node, name, ParamValue::Float(value));
    }
}
