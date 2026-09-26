//! CPU vector-field sources and composition nodes for native physics inputs.

use std::borrow::Cow;

use manifold_physics::{FieldValue, PhysicsError};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

fn required_field(ctx: &mut EffectNodeContext<'_, '_>, port: &str) -> Option<FieldValue> {
    let Some(slot) = ctx.inputs.slot(port) else {
        ctx.error(format!("vector field input `{port}` is required"));
        ctx.mark_outputs_pending();
        return None;
    };
    if !ctx.inputs.slot_content_ready(slot) {
        ctx.mark_outputs_pending();
        return None;
    }
    let Some(value) = ctx.inputs.vector_field_slot(slot) else {
        ctx.error(format!("vector field input `{port}` has no value"));
        ctx.mark_outputs_pending();
        return None;
    };
    Some(value)
}

fn write_result(
    ctx: &mut EffectNodeContext<'_, '_>,
    result: Result<FieldValue, PhysicsError>,
    operation: &str,
) {
    match result {
        Ok(value) => ctx.outputs.set_vector_field("out", value),
        Err(error) => {
            ctx.error(format!("{operation}: {error}"));
            ctx.mark_outputs_pending();
        }
    }
}

crate::primitive! {
    name: UniformVectorField,
    type_id: "node.uniform_vector_field",
    purpose: "Emit a constant dimensionless vector field in world coordinates from three scalar components.",
    inputs: {
        x: ScalarF32 optional,
        y: ScalarF32 optional,
        z: ScalarF32 optional,
    },
    outputs: {
        out: VectorField,
    },
    params: [
        ParamDef { name: Cow::Borrowed("x"), label: "X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("y"), label: "Y", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("z"), label: "Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use as a constant world-space source. Wire x, y, or z to scalar controls when the vector should change over time; unwired components use their parameters.",
    examples: [],
    picker: { label: "Uniform Vector Field", category: Atom },
    summary: "Outputs the same vector at every world-space sample.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["uniform field", "constant vector field", "vector source"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for UniformVectorField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let vector = [
            ctx.scalar_or_param("x", 0.0),
            ctx.scalar_or_param("y", 1.0),
            ctx.scalar_or_param("z", 0.0),
        ];
        write_result(ctx, FieldValue::uniform(vector), "uniform vector field");
    }
}

crate::primitive! {
    name: RadialVectorField,
    type_id: "node.radial_vector_field",
    purpose: "Emit a dimensionless radial vector field around a world-space center with validated radius and falloff.",
    inputs: {
        center_x: ScalarF32 optional,
        center_y: ScalarF32 optional,
        center_z: ScalarF32 optional,
        radius: ScalarF32 optional,
        falloff: ScalarF32 optional,
    },
    outputs: {
        out: VectorField,
    },
    params: [
        ParamDef { name: Cow::Borrowed("center_x"), label: "Center X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("center_y"), label: "Center Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("center_z"), label: "Center Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("radius"), label: "Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.01, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("falloff"), label: "Falloff", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 8.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use as a radial source centered in world coordinates. Wire any center, radius, or falloff scalar to modulate the source; invalid values are reported and leave the output pending.",
    examples: [],
    picker: { label: "Radial Vector Field", category: Atom },
    summary: "Outputs vectors pointing away from a world-space center within a radius.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["radial field", "outward field", "point field"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for RadialVectorField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let center = [
            ctx.scalar_or_param("center_x", 0.0),
            ctx.scalar_or_param("center_y", 0.0),
            ctx.scalar_or_param("center_z", 0.0),
        ];
        let radius = ctx.scalar_or_param("radius", 1.0);
        let falloff = ctx.scalar_or_param("falloff", 1.0);
        write_result(
            ctx,
            FieldValue::radial(center, radius, falloff),
            "radial vector field",
        );
    }
}

crate::primitive! {
    name: VortexVectorField,
    type_id: "node.vortex_vector_field",
    purpose: "Emit a dimensionless tangential vector field around a world-space axis with validated radius and falloff.",
    inputs: {
        center_x: ScalarF32 optional,
        center_y: ScalarF32 optional,
        center_z: ScalarF32 optional,
        axis_x: ScalarF32 optional,
        axis_y: ScalarF32 optional,
        axis_z: ScalarF32 optional,
        radius: ScalarF32 optional,
        falloff: ScalarF32 optional,
    },
    outputs: {
        out: VectorField,
    },
    params: [
        ParamDef { name: Cow::Borrowed("center_x"), label: "Center X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("center_y"), label: "Center Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("center_z"), label: "Center Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("axis_x"), label: "Axis X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("axis_y"), label: "Axis Y", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("axis_z"), label: "Axis Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("radius"), label: "Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.01, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("falloff"), label: "Falloff", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 8.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use as a tangential source around a world-space axis. Wire the center, axis, radius, or falloff components to scalar controls; the shared evaluator validates finite values and a non-zero axis.",
    examples: [],
    picker: { label: "Vortex Vector Field", category: Atom },
    summary: "Outputs tangential vectors circling a world-space axis within a radius.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["vortex field", "swirl field", "tangential field"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for VortexVectorField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let center = [
            ctx.scalar_or_param("center_x", 0.0),
            ctx.scalar_or_param("center_y", 0.0),
            ctx.scalar_or_param("center_z", 0.0),
        ];
        let axis = [
            ctx.scalar_or_param("axis_x", 0.0),
            ctx.scalar_or_param("axis_y", 1.0),
            ctx.scalar_or_param("axis_z", 0.0),
        ];
        let radius = ctx.scalar_or_param("radius", 1.0);
        let falloff = ctx.scalar_or_param("falloff", 1.0);
        write_result(
            ctx,
            FieldValue::vortex(center, axis, radius, falloff),
            "vortex vector field",
        );
    }
}

crate::primitive! {
    name: AddVectorFields,
    type_id: "node.add_vector_fields",
    purpose: "Sum two dimensionless vector fields componentwise at the same world-space sample.",
    inputs: {
        a: VectorField required,
        b: VectorField required,
    },
    outputs: {
        out: VectorField,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Wire two vector-field sources to add their vectors before a native solver consumes the result. Both inputs must be ready; no default field is substituted.",
    examples: [],
    picker: { label: "Add Vector Fields", category: Atom },
    summary: "Adds two vector fields component by component.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["add fields", "sum vector fields", "field sum"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for AddVectorFields {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(a), Some(b)) = (required_field(ctx, "a"), required_field(ctx, "b")) else {
            return;
        };
        write_result(ctx, a.sum(&b), "add vector fields");
    }
}

crate::primitive! {
    name: MultiplyVectorFields,
    type_id: "node.multiply_vector_fields",
    purpose: "Multiply two dimensionless vector fields componentwise at the same world-space sample.",
    inputs: {
        a: VectorField required,
        b: VectorField required,
    },
    outputs: {
        out: VectorField,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Wire a vector field and a componentwise mask field to multiply them. A field with equal components acts as a spatial mask; both inputs must be ready.",
    examples: [],
    picker: { label: "Multiply Vector Fields", category: Atom },
    summary: "Multiplies two vector fields component by component.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["multiply fields", "field mask", "field product"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for MultiplyVectorFields {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(a), Some(b)) = (required_field(ctx, "a"), required_field(ctx, "b")) else {
            return;
        };
        write_result(ctx, a.multiply(&b), "multiply vector fields");
    }
}

crate::primitive! {
    name: ScaleVectorField,
    type_id: "node.scale_vector_field",
    purpose: "Scale a dimensionless vector field by a scalar strength without changing its world-coordinate domain.",
    inputs: {
        field: VectorField required,
        strength: ScalarF32 optional,
    },
    outputs: {
        out: VectorField,
    },
    params: [
        ParamDef { name: Cow::Borrowed("strength"), label: "Strength", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-100.0, 100.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire a vector field and use strength as a live scalar control. The field remains dimensionless and in world coordinates; invalid strength values are reported and leave the output pending.",
    examples: [],
    picker: { label: "Scale Vector Field", category: Atom },
    summary: "Scales a vector field by an independent strength control.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["scale field", "field strength", "vector gain"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for ScaleVectorField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(field) = required_field(ctx, "field") else {
            return;
        };
        let strength = ctx.scalar_or_param("strength", 1.0);
        write_result(ctx, field.scaled(strength), "scale vector field");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs};
    use crate::node_graph::effect_node::{EffectNodeContext, FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::{MockBackend, Slot};
    use manifold_core::{Beats, Seconds};
    use manifold_physics::VectorField;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn run_node<P: Primitive>(
        node: &mut P,
        params: ParamValues,
        fields: &[(&'static str, FieldValue)],
        scalars: &[(&'static str, f32)],
        pending_ports: &[&str],
    ) -> (Option<FieldValue>, bool, Vec<String>) {
        let mut backend = MockBackend::new();
        let output_slot = backend.acquire(ResourceId(0), PortType::VectorField, None, (0, 0));
        let mut input_bindings: Vec<(&'static str, Slot)> = Vec::new();
        for (index, &(port, ref value)) in fields.iter().enumerate() {
            let slot = backend.acquire(
                ResourceId(index as u32 + 1),
                PortType::VectorField,
                None,
                (0, 0),
            );
            backend.set_vector_field(slot, value.clone());
            input_bindings.push((port, slot));
        }
        for (index, &(port, value)) in scalars.iter().enumerate() {
            let slot = backend.acquire(
                ResourceId(fields.len() as u32 + index as u32 + 1),
                PortType::Scalar(ScalarType::F32),
                None,
                (0, 0),
            );
            backend.set_scalar(slot, ParamValue::Float(value));
            input_bindings.push((port, slot));
        }
        let mut pending = vec![false; backend.slot_count() as usize];
        for port in pending_ports {
            if let Some((_, slot)) = input_bindings.iter().find(|(name, _)| name == port) {
                pending[slot.0 as usize] = true;
            }
        }

        let output_bindings: &[(&'static str, Slot)] = &[("out", output_slot)];
        let inputs = NodeInputs::new(&input_bindings, &backend, &[]).with_pending(&pending);
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let mut vector_field_scratch = Vec::new();
        let outputs = NodeOutputs::new(
            output_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut render_mode_scratch,
            &mut object_scratch,
        )
        .with_vector_field_writes(&mut vector_field_scratch);
        let mut errors = Vec::new();
        let outputs_pending;
        {
            let mut context = EffectNodeContext::new(frame_time(), &params, inputs, outputs, None)
                .with_errors(&mut errors);
            node.run(&mut context);
            outputs_pending = context.outputs_pending;
        }
        for (slot, value) in vector_field_scratch.drain(..) {
            backend.set_vector_field(slot, value);
        }
        (backend.vector_field(output_slot), outputs_pending, errors)
    }

    #[test]
    fn uniform_vector_field_scalar_wires_override_parameters() {
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("x"), ParamValue::Float(9.0));
        params.insert(Cow::Borrowed("y"), ParamValue::Float(8.0));
        params.insert(Cow::Borrowed("z"), ParamValue::Float(7.0));
        let mut node = UniformVectorField::new();
        let (field, pending, errors) =
            run_node(&mut node, params, &[], &[("x", 1.25), ("z", -2.5)], &[]);
        assert!(!pending, "valid source should be ready: {errors:?}");
        assert!(errors.is_empty());
        assert_eq!(field.unwrap().sample([12.0, -3.0, 4.0]), [1.25, 8.0, -2.5]);
    }

    #[test]
    fn vector_field_composition_preserves_sum_product_and_scale() {
        let first = FieldValue::uniform([2.0, 3.0, 4.0]).unwrap();
        let second = FieldValue::uniform([1.0, 2.0, 3.0]).unwrap();
        let mask = FieldValue::uniform([2.0, 1.0, 0.5]).unwrap();

        let mut add = AddVectorFields::new();
        let (sum, add_pending, add_errors) = run_node(
            &mut add,
            ParamValues::default(),
            &[("a", first), ("b", second)],
            &[],
            &[],
        );
        assert!(!add_pending, "add should be ready: {add_errors:?}");
        let mut multiply = MultiplyVectorFields::new();
        let (product, multiply_pending, multiply_errors) = run_node(
            &mut multiply,
            ParamValues::default(),
            &[("a", sum.unwrap()), ("b", mask)],
            &[],
            &[],
        );
        assert!(
            !multiply_pending,
            "multiply should be ready: {multiply_errors:?}"
        );
        let mut scale = ScaleVectorField::new();
        let (scaled, scale_pending, scale_errors) = run_node(
            &mut scale,
            ParamValues::default(),
            &[("field", product.unwrap())],
            &[("strength", 2.0)],
            &[],
        );
        assert!(!scale_pending, "scale should be ready: {scale_errors:?}");
        assert_eq!(scaled.unwrap().sample([0.0, 0.0, 0.0]), [12.0, 10.0, 7.0]);
    }

    #[test]
    fn invalid_leaf_and_pending_required_input_hold_output_pending() {
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("radius"), ParamValue::Float(f32::NAN));
        let mut radial = RadialVectorField::new();
        let (field, pending, errors) = run_node(&mut radial, params, &[], &[], &[]);
        assert!(field.is_none());
        assert!(pending);
        assert_eq!(errors.len(), 1);

        let mut add = AddVectorFields::new();
        let (_, pending, errors) = run_node(
            &mut add,
            ParamValues::default(),
            &[
                ("a", FieldValue::uniform([1.0; 3]).unwrap()),
                ("b", FieldValue::uniform([2.0; 3]).unwrap()),
            ],
            &[],
            &["a"],
        );
        assert!(pending);
        assert!(
            errors.is_empty(),
            "pending input is not an evaluation error"
        );
    }
}
