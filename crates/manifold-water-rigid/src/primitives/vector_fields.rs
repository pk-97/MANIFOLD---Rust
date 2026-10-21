//! CPU vector-field sources and composition nodes for native physics inputs.

use std::borrow::Cow;

use manifold_physics::{FalloffCurve, FieldValue, PhysicsError, TurbulenceDrift, TurbulenceField};

/// The radial and vortex fields' Falloff Curve choices, by index.
const FALLOFF_CURVES: &[&str] = &["Edge", "Inverse"];

fn falloff_curve(ctx: &EffectNodeContext<'_, '_>) -> FalloffCurve {
    let index = match ctx.inputs.scalar("curve") {
        Some(ParamValue::Float(index)) => index,
        _ => match ctx.params.get("curve") {
            Some(ParamValue::Enum(index)) => *index as f32,
            Some(ParamValue::Float(index)) => *index,
            _ => 0.0,
        },
    };
    FalloffCurve::from_index(index)
}

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

fn required_field(ctx: &mut EffectNodeContext<'_, '_>, port: &str) -> Option<FieldValue> {
    let Some(slot) = ctx.inputs.slot(port) else {
        ctx.error(format!("vector field input `{port}` is required"));
        ctx.mark_outputs_pending();
        return None;
    };
    let Some(value) = ctx.inputs.cpu_value_slot::<FieldValue>(slot) else {
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
        Ok(value) => ctx.outputs.set_cpu_value("out", value),
        Err(error) => {
            ctx.error(format!("{operation}: {error}"));
            ctx.mark_outputs_pending();
        }
    }
}

manifold_node_engine::primitive! {
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
        ParamDef { name: Cow::Borrowed("normalize"), label: "Normalize", ty: ParamType::Bool, default: ParamValue::Bool(false), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use as a constant world-space source. Wire x, y, or z to scalar controls when the vector should change over time; unwired components use their parameters. Normalize keeps only the direction (length 1, zero stays zero), so a scale after it sets the strength exactly: a wind's speed in m/s whatever direction is dialled.",
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
        let mut vector = [
            ctx.scalar_or_param("x", 0.0),
            ctx.scalar_or_param("y", 1.0),
            ctx.scalar_or_param("z", 0.0),
        ];
        let length = vector.iter().map(|c| c * c).sum::<f32>().sqrt();
        if matches!(ctx.params.get("normalize"), Some(ParamValue::Bool(true))) && length > 0.0 {
            vector = vector.map(|c| c / length);
        }
        write_result(ctx, FieldValue::uniform(vector), "uniform vector field");
    }
}

manifold_node_engine::primitive! {
    name: RadialVectorField,
    type_id: "node.radial_vector_field",
    purpose: "Emit a dimensionless radial vector field around a world-space center with validated radius and falloff.",
    inputs: {
        center_x: ScalarF32 optional,
        center_y: ScalarF32 optional,
        center_z: ScalarF32 optional,
        radius: ScalarF32 optional,
        falloff: ScalarF32 optional,
        curve: ScalarF32 optional,
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
        ParamDef { name: Cow::Borrowed("curve"), label: "Falloff Curve", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: FALLOFF_CURVES },
    ],
    depth_rule: Terminal,
    composition_notes: "Use as a radial source centered in world coordinates; at negative strength it is an attractor. Falloff Curve picks how strength falls with distance d for radius R and power p: Edge (1 − d/R)^p, nothing past the radius; Inverse full inside the radius and (R/d)^p beyond it, unbounded (power 2 is inverse square). Wire any center, radius, falloff or curve scalar to modulate the source; invalid values are reported and leave the output pending.",
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
            FieldValue::radial_curve(center, radius, falloff, falloff_curve(ctx)),
            "radial vector field",
        );
    }
}

manifold_node_engine::primitive! {
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
        curve: ScalarF32 optional,
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
        ParamDef { name: Cow::Borrowed("curve"), label: "Falloff Curve", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: FALLOFF_CURVES },
    ],
    depth_rule: Terminal,
    composition_notes: "Use as a tangential source around a world-space axis. Falloff Curve is as on the radial field, by distance from the axis. Wire the center, axis, radius, falloff or curve components to scalar controls; the shared evaluator validates finite values and a non-zero axis.",
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
            FieldValue::vortex_curve(center, axis, radius, falloff, falloff_curve(ctx)),
            "vortex vector field",
        );
    }
}

manifold_node_engine::primitive! {
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

manifold_node_engine::primitive! {
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

manifold_node_engine::primitive! {
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

manifold_node_engine::primitive! {
    name: TurbulenceVectorField,
    type_id: "node.turbulence_vector_field",
    purpose: "Emit a smooth swirling dimensionless vector field (curl noise, divergence free) whose swirls are about Size metres across and drift through space along the drift direction, travelling Drift Speed integrated over transport time (an automated speed bends the drift, never jumps it). A backward seek or loop starts the drift again.",
    inputs: {
        size: ScalarF32 optional,
        drift_x: ScalarF32 optional,
        drift_y: ScalarF32 optional,
        drift_z: ScalarF32 optional,
        drift_speed: ScalarF32 optional,
        seed: ScalarF32 optional,
    },
    outputs: {
        out: VectorField,
    },
    params: [
        ParamDef { name: Cow::Borrowed("size"), label: "Size", ty: ParamType::Float, default: ParamValue::Float(2.0), range: Some((0.05, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("drift_x"), label: "Drift X", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("drift_y"), label: "Drift Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("drift_z"), label: "Drift Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("drift_speed"), label: "Drift Speed", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-50.0, 50.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("seed"), label: "Seed", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 1000.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Gusts and stirring for a wind or a force. Its vectors are about 1 long, so scale it to the strength you want (m/s before node.wind_field, m/s² as a push). Drift moves the swirls through space at Drift Speed m/s along the drift direction (normalized; zero holds them still); give a wind's turbulence the wind's direction and speed so the gusts travel with the air. Swirls smaller than the force lattice's spacing (a quarter of the solver grid, or Force Detail on a swarm) smooth out.",
    examples: [],
    picker: { label: "Turbulence Vector Field", category: Atom },
    summary: "Outputs smooth swirling vectors that drift through space over time.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["turbulence", "noise field", "curl noise", "gusts"],
    integrates_time: true,
    boundary_reason: NonGpu,
    extra_fields: {
        drift: TurbulenceDrift = TurbulenceDrift::default(),
    },
}

impl Primitive for TurbulenceVectorField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let size = ctx.scalar_or_param("size", 2.0);
        let drift = [ctx.scalar_or_param("drift_x", 1.0), ctx.scalar_or_param("drift_y", 0.0), ctx.scalar_or_param("drift_z", 0.0)];
        let speed = ctx.scalar_or_param("drift_speed", 0.0);
        let seed = ctx.scalar_or_param("seed", 0.0).round().max(0.0) as u32;
        let length = drift.iter().map(|c| c * c).sum::<f32>().sqrt();
        // The drift travelled so far, in f64 and wrapped to the noise period,
        // so the gusts keep their precision through a long set.
        let velocity = if length > 0.0 { drift.map(|c| f64::from(c / length) * f64::from(speed)) } else { [0.0; 3] };
        let offset = self.drift.advance(ctx.played(), velocity);
        // A size that is not positive is refused before the offset is read.
        let field = FieldValue::turbulence(size, TurbulenceField::wrap_offset(size, offset), seed);
        write_result(ctx, field, "turbulence vector field");
    }

    fn clear_state(&mut self) {
        self.drift.restart();
    }
}

manifold_node_engine::primitive! {
    name: WindField,
    type_id: "node.wind_field",
    purpose: "Turn a vector field read as air velocity in m/s into wind: anything moving through it at velocity v is pushed by Drag Rate · (wind − v), so it relaxes toward the wind at Drag Rate per second (Houdini POP Wind air resistance). Air already on the input is kept.",
    inputs: {
        velocity: VectorField required,
        drag_rate: ScalarF32 optional,
    },
    outputs: {
        out: VectorField,
    },
    params: [
        ParamDef { name: Cow::Borrowed("drag_rate"), label: "Drag Rate", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 20.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire a velocity field (node.uniform_vector_field scaled to m/s, plus node.turbulence_vector_field for gusts) and add the result to any force list. A still velocity (zero) is plain air resistance. Each consumer (water, swarm, physics world) scales the drag rate by its own Air Drag. Mask the velocity before it becomes wind: node.multiply_vector_fields refuses a field carrying air.",
    examples: [],
    picker: { label: "Wind Field", category: Atom },
    summary: "Makes a velocity field into wind that drags things toward its velocity.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["wind", "drag", "air", "air resistance"],
    pure: true,
    boundary_reason: NonGpu,
}

impl Primitive for WindField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(velocity) = required_field(ctx, "velocity") else {
            return;
        };
        let drag_rate = ctx.scalar_or_param("drag_rate", 1.0);
        write_result(ctx, velocity.air(drag_rate), "wind field");
    }
}

#[cfg(test)]
mod tests {
    use manifold_node_engine::exec::cpu_values::CpuWireWrites;
    use super::*;

    use manifold_node_engine::exec::backend::Backend;
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs};
    use manifold_node_engine::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::ports::{PortType, ScalarType};
    use manifold_node_engine::{exec::backend::MockBackend, bindings::Slot};
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
            backend.cpu_values_mut().set(slot, value.clone());
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
        let output_bindings: &[(&'static str, Slot)] = &[("out", output_slot)];
        let inputs = NodeInputs::new(&input_bindings, &backend, &[]);
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let mut vector_field_scratch = CpuWireWrites::default();
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
        .with_cpu_value_writes(&mut vector_field_scratch);
        let mut errors = Vec::new();
        let outputs_pending;
        {
            let mut context = EffectNodeContext::new(frame_time(), &params, inputs, outputs, None)
                .with_errors(&mut errors);
            node.run(&mut context);
            outputs_pending = context.outputs_pending;
        }
        vector_field_scratch.commit(backend.cpu_values_mut());
        (backend.cpu_values().get::<FieldValue>(output_slot), outputs_pending, errors)
    }

    #[test]
    fn uniform_vector_field_scalar_wires_override_parameters() {
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("x"), ParamValue::Float(9.0));
        params.insert(Cow::Borrowed("y"), ParamValue::Float(8.0));
        params.insert(Cow::Borrowed("z"), ParamValue::Float(7.0));
        let mut node = UniformVectorField::new();
        let (field, pending, errors) =
            run_node(&mut node, params, &[], &[("x", 1.25), ("z", -2.5)]);
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
        );
        assert!(!add_pending, "add should be ready: {add_errors:?}");
        let mut multiply = MultiplyVectorFields::new();
        let (product, multiply_pending, multiply_errors) = run_node(
            &mut multiply,
            ParamValues::default(),
            &[("a", sum.unwrap()), ("b", mask)],
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
        );
        assert!(!scale_pending, "scale should be ready: {scale_errors:?}");
        assert_eq!(scaled.unwrap().sample([0.0, 0.0, 0.0]), [12.0, 10.0, 7.0]);
    }

    #[test]
    fn invalid_leaf_holds_output_pending() {
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("radius"), ParamValue::Float(f32::NAN));
        let mut radial = RadialVectorField::new();
        let (field, pending, errors) = run_node(&mut radial, params, &[], &[]);
        assert!(field.is_none());
        assert!(pending);
        assert_eq!(errors.len(), 1);
    }

    /// The card's curve choice reaches the field: Inverse keeps pulling past
    /// the radius, Edge does not.
    #[test]
    fn radial_field_takes_the_falloff_curve_choice() {
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("radius"), ParamValue::Float(1.0));
        params.insert(Cow::Borrowed("curve"), ParamValue::Enum(1));
        let (inverse, _, errors) = run_node(&mut RadialVectorField::new(), params.clone(), &[], &[]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(inverse.unwrap().sample([2.0, 0.0, 0.0]), [0.5, 0.0, 0.0]);
        let (edge, _, _) = run_node(&mut RadialVectorField::new(), params, &[], &[("curve", 0.0)]);
        assert_eq!(edge.unwrap().sample([2.0, 0.0, 0.0]), [0.0; 3]);
    }

    /// A wind node turns a velocity into air at its drag rate; a negative
    /// rate is refused.
    #[test]
    fn wind_field_carries_air_at_its_drag_rate() {
        let velocity = FieldValue::uniform([3.0, 0.0, 0.0]).unwrap();
        let (wind, pending, errors) =
            run_node(&mut WindField::new(), ParamValues::default(), &[("velocity", velocity.clone())], &[("drag_rate", 2.0)]);
        assert!(!pending, "{errors:?}");
        let sample = wind.unwrap().sample_force([0.0; 3]);
        assert_eq!((sample.push, sample.drag, sample.drag_wind), ([0.0; 3], 2.0, [6.0, 0.0, 0.0]));
        let (_, pending, errors) =
            run_node(&mut WindField::new(), ParamValues::default(), &[("velocity", velocity)], &[("drag_rate", -1.0)]);
        assert!(pending && errors.len() == 1);
    }

    #[test]
    fn turbulence_field_is_a_unit_scale_swirl() {
        let (field, pending, errors) =
            run_node(&mut TurbulenceVectorField::new(), ParamValues::default(), &[], &[("size", 3.0), ("seed", 2.0)]);
        assert!(!pending, "{errors:?}");
        let expected = manifold_physics::TurbulenceField::new(3.0, [0.0; 3], 2).unwrap();
        assert_eq!(field.unwrap().sample([0.4, 1.1, -0.7]), expected.sample([0.4, 1.1, -0.7]));
    }
}
