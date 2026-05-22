//! `node.scalar_array_accumulator` — add `increment` to every element
//! of an internal accumulator on each clip trigger, emit the
//! accumulator as `Array<f32>`.
//!
//! The envelope-mode driver counterpart to `node.cycle_table_row`:
//! both produce `Array<f32>` from a `trigger_count` source, but where
//! the cycler snaps to discrete preset rows, the accumulator advances
//! continuously. NestedCubes' envelope mode wires the accumulator into
//! `nested_cubes_geometry.target_angles` to get the "+90° per trigger"
//! behaviour without snapping to a preset.
//!
//! Per-instance offsets aren't carried — the accumulator starts at
//! all-zeros and every element advances by the same `increment` on
//! each trigger. This is a deliberate simplification of the legacy
//! NestedCubes generator which started at `(0, 90, 180, 270, 360)`
//! to look mid-animation at frame 0; the decomposed primitive starts
//! at all-zeros (perfectly aligned cubes) and spreads on the first
//! trigger. Functionally identical from frame 1 onwards.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: ScalarArrayAccumulator,
    type_id: "node.scalar_array_accumulator",
    purpose: "Add `increment` to every element of an internal Array<f32> accumulator on each clip trigger; emit the accumulator. Generic envelope-mode driver — pair with a trigger_count source to advance N parallel scalars synchronously. NestedCubes envelope mode is the first user.",
    inputs: {
        trigger_count: ScalarF32 optional,
        increment: ScalarF32 optional,
    },
    outputs: {
        accumulated: Array(f32),
    },
    params: [
        ParamDef {
            name: "increment",
            label: "Increment",
            ty: ParamType::Float,
            default: ParamValue::Float(90.0),
            range: Some((-360.0, 360.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "capacity",
            label: "Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(5.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Trigger detection mirrors legacy NestedCubes: a fresh `trigger_count > last_seen` event adds `increment` to every accumulator element. `increment` is port-shadows-param so it can be modulated. Output capacity comes from the `capacity` param at chain-build time. Accumulator state is fresh on rebuild (per the graph-editor-is-authoring-not-perform rule); same trigger_count across rebuild won't double-add.",
    examples: [],
    picker: { label: "Scalar Array Accumulator", category: Driver },
    extra_fields: {
        accumulator: Vec<f32> = Vec::new(),
        last_trigger_count: Option<u32> = None,
    },
}

impl Primitive for ScalarArrayAccumulator {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "accumulated" {
            return None;
        }
        params
            .get("capacity")
            .and_then(|v| v.as_u32_clamped(1))
            .or(Some(5))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(dst) = ctx.outputs.array("accumulated") else {
            log::warn!(
                "node.scalar_array_accumulator: no GpuBuffer bound to output \
                 port `accumulated` — the chain build did not pre-allocate \
                 the Array<f32> output.",
            );
            return;
        };
        let f32_size = std::mem::size_of::<f32>() as u64;
        let capacity = (dst.size / f32_size) as usize;
        if capacity == 0 {
            return;
        }
        // Lazy init / resize the accumulator to match output capacity.
        if self.accumulator.len() != capacity {
            self.accumulator.resize(capacity, 0.0);
        }

        let increment = ctx
            .inputs
            .scalar("increment")
            .and_then(|v| v.as_scalar())
            .or_else(|| ctx.params.get("increment").and_then(|v| v.as_scalar()))
            .unwrap_or(90.0);
        let raw_count = ctx
            .inputs
            .scalar("trigger_count")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        let count = raw_count.round().max(0.0) as u32;

        // Detect new-trigger edge. First frame establishes the baseline
        // without advancing — matches legacy NestedCubes' "should_trigger
        // only when last_trigger_count >= 0" gate, so loading the preset
        // doesn't immediately consume a trigger.
        let should_advance = match self.last_trigger_count {
            None => false,
            Some(last) => count != last,
        };
        self.last_trigger_count = Some(count);
        if should_advance {
            for value in self.accumulator.iter_mut() {
                *value += increment;
            }
        }

        unsafe { dst.write(0, bytemuck::cast_slice(&self.accumulator)) };
    }

    fn clear_state(&mut self) {
        self.accumulator.clear();
        self.last_trigger_count = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_trigger_and_increment_inputs_plus_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let inputs = ScalarArrayAccumulator::INPUTS;
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].name, "trigger_count");
        assert!(!inputs[0].required);
        assert_eq!(inputs[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(inputs[1].name, "increment");
        assert!(!inputs[1].required);

        assert_eq!(ScalarArrayAccumulator::OUTPUTS.len(), 1);
        assert_eq!(ScalarArrayAccumulator::OUTPUTS[0].name, "accumulated");
        assert_eq!(
            ScalarArrayAccumulator::OUTPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<f32>())
        );
    }

    #[test]
    fn declares_increment_and_capacity_params() {
        let params = ScalarArrayAccumulator::PARAMS;
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "increment");
        assert_eq!(params[1].name, "capacity");
    }

    #[test]
    fn array_output_capacity_reads_capacity_param() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ScalarArrayAccumulator::new();
        let mut params = ParamValues::default();
        params.insert("capacity", ParamValue::Float(7.0));
        let cap = Primitive::array_output_capacity(&prim, "accumulated", &params, &[]);
        assert_eq!(cap, Some(7));
    }

    #[test]
    fn primitive_registers_as_palette_driver() {
        use crate::node_graph::palette::{PaletteCategory, palette_atoms};
        let atoms = palette_atoms();
        let entry = atoms
            .iter()
            .find(|e| e.type_id == ScalarArrayAccumulator::TYPE_ID)
            .expect("scalar_array_accumulator should be registered as a palette atom");
        assert_eq!(entry.label, "Scalar Array Accumulator");
        assert!(matches!(entry.category, PaletteCategory::Driver));
    }
}
