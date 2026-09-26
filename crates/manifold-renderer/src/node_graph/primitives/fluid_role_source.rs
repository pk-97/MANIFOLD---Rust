//! `node.fluid_role_source` — prepare one immutable geometry source and emit a
//! CPU `FluidRole` wire.  The node has no solver state, native handles, timing,
//! or rendering responsibilities.

mod geometry;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use crate::generators::mesh_common::PLATONIC_SHAPES;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_role::{FluidRole, FluidRoleKind, PreparedFluidGeometry};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics_mesh::MeshSelection;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::transform::Transform;
use geometry::{GeometryMode, prepare_geometry};

const GEOMETRY_MODES: &[&str] = &["Collision Proxy", "Closed Mesh"];
const FLUID_ROLE_KINDS: &[&str] = &["Initial Fill", "Inflow", "Outflow", "Collider"];

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PreparationKey {
    path: String,
    selection: MeshSelection,
    shape: u32,
    radius: f32,
    source_transform: Transform,
    mode: GeometryMode,
}

impl PreparationKey {
    fn matches(
        &self,
        path: &str,
        selection: MeshSelection,
        shape: u32,
        radius: f32,
        source_transform: Transform,
        mode: GeometryMode,
    ) -> bool {
        self.path == path
            && self.selection == selection
            && self.shape == shape
            && self.radius == radius
            && self.source_transform == source_transform
            && self.mode == mode
    }
}

crate::primitive! {
    name: FluidRoleSource,
    type_id: "node.fluid_role_source",
    purpose: "Prepare one built-in or imported closed volume as a CPU FluidRole wire. Collision Proxy cooks reusable Box3D hulls; Closed Mesh preserves the exact indexed surface after exact-coordinate welding and validates it as a closed volume. Live transform, enabled, velocity, inheritance, and friction controls do not recook geometry.",
    inputs: {
        transform: Transform required,
        source_transform: Transform optional,
        role: ScalarF32 optional,
        enabled: ScalarF32 optional,
        velocity_x: ScalarF32 optional,
        velocity_y: ScalarF32 optional,
        velocity_z: ScalarF32 optional,
        inherit_motion: ScalarF32 optional,
        friction: ScalarF32 optional,
        geometry: ScalarF32 optional,
        shape: ScalarF32 optional,
        radius: ScalarF32 optional,
        mesh_index: ScalarF32 optional,
        primitive_index: ScalarF32 optional,
        material_index: ScalarF32 optional,
        fit: ScalarF32 optional,
        recenter: ScalarF32 optional,
        translate_x: ScalarF32 optional,
        translate_y: ScalarF32 optional,
        translate_z: ScalarF32 optional,
        fragment_count: ScalarF32 optional,
        fragment_index: ScalarF32 optional,
        collider_parts: ScalarF32 optional,
    },
    outputs: {
        role: FluidRole,
    },
    params: [
        ParamDef { name: Cow::Borrowed("role"), label: "Role", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, 3.0)), enum_values: FLUID_ROLE_KINDS },
        ParamDef { name: Cow::Borrowed("enabled"), label: "Enabled", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("velocity_x"), label: "Velocity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("velocity_y"), label: "Velocity Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("velocity_z"), label: "Velocity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("inherit_motion"), label: "Inherit Motion", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("friction"), label: "Friction", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("geometry"), label: "Geometry", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: GEOMETRY_MODES },
        ParamDef { name: Cow::Borrowed("shape"), label: "Builtin Shape", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, (PLATONIC_SHAPES.len() - 1) as f32)), enum_values: PLATONIC_SHAPES },
        ParamDef { name: Cow::Borrowed("radius"), label: "Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("path"), label: "Mesh File", ty: ParamType::String, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("mesh_index"), label: "Mesh Index", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0, 1024.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("primitive_index"), label: "Primitive Index", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0, 1024.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("material_index"), label: "Material Index", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-2.0, 1024.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fit"), label: "Fit", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: &["none", "unit_box"] },
        ParamDef { name: Cow::Borrowed("recenter"), label: "Recenter", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("translate_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("translate_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("translate_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fragment_count"), label: "Pieces", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fragment_index"), label: "Piece", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 63.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("collider_parts"), label: "Collider Detail", ty: ParamType::Int, default: ParamValue::Float(32.0), range: Some((1.0, 64.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use one source for each fluid role and wire all roles into the fluid world. Choose Initial Fill for starting volume, Inflow/Outflow for boundary sources, or Collider for obstacles. Builtin shapes use the shared Platonic geometry; imported paths reuse the existing MeshSelection loader. Collision Proxy is explicit approximate geometry for arbitrary/open source surfaces; Closed Mesh keeps a nonconvex container's cavity and rejects open or invalid surfaces instead of silently substituting a hull.",
    examples: [],
    picker: { label: "Fluid Role Source", category: Atom },
    summary: "Prepares one reusable fluid source or collider geometry and emits its typed CPU role wire.",
    category: Geometry3D,
    role: Source,
    aliases: ["fluid source", "fluid role", "fluid collider", "inflow", "outflow", "initial fill"],
    boundary_reason: NonGpu,
    extra_fields: {
        last_key: Option<PreparationKey> = None,
        geometry: Option<Arc<PreparedFluidGeometry>> = None,
        pending_geometry: Option<mpsc::Receiver<Result<Arc<PreparedFluidGeometry>, String>>> = None,
        preparation_error: Option<String> = None,
    },
}

impl Primitive for FluidRoleSource {
    fn warmup_pending(&self) -> bool {
        self.pending_geometry.is_some()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(transform) = ctx.inputs.transform("transform") else {
            ctx.error("Fluid Role Source needs a transform");
            ctx.mark_outputs_pending();
            return;
        };
        if let Err(error) = validate_transform(transform, "body transform") {
            ctx.error(error);
            ctx.mark_outputs_pending();
            return;
        }
        let source_transform = ctx.inputs.transform("source_transform").unwrap_or_default();
        if let Err(error) = validate_transform(source_transform, "source transform") {
            return self.fail_setup(ctx, error);
        }

        let role = match resolve_index(ctx, "role", 1, FLUID_ROLE_KINDS.len() as u32) {
            Ok(value) => match value {
                0 => FluidRoleKind::InitialFill,
                1 => FluidRoleKind::Inflow,
                2 => FluidRoleKind::Outflow,
                3 => FluidRoleKind::Collider,
                _ => unreachable!(),
            },
            Err(error) => return self.fail(ctx, error),
        };
        let enabled = match resolve_bool(ctx, "enabled", true) {
            Ok(value) => value,
            Err(error) => return self.fail(ctx, error),
        };
        let velocity = match resolve_velocity(ctx) {
            Ok(values) => values,
            Err(error) => return self.fail(ctx, error),
        };
        if velocity.iter().any(|value| !value.is_finite()) {
            return self.fail(ctx, "Fluid role velocity must be finite".into());
        }
        let inherit_motion = match resolve_f32(ctx, "inherit_motion", 0.0) {
            Ok(value) if value.is_finite() && value >= 0.0 => value,
            Ok(_) => {
                return self.fail(
                    ctx,
                    "Fluid role inherit_motion must be finite and non-negative".into(),
                );
            }
            Err(error) => return self.fail(ctx, error),
        };
        let friction = match resolve_f32(ctx, "friction", 0.0) {
            Ok(value) if value.is_finite() && (0.0..=1.0).contains(&value) => value,
            Ok(_) => {
                return self.fail(
                    ctx,
                    "Fluid role friction must be finite and in 0..=1".into(),
                );
            }
            Err(error) => return self.fail(ctx, error),
        };

        let path = match ctx.params.get("path") {
            None | Some(ParamValue::Float(0.0)) => "",
            Some(ParamValue::String(path)) => path.as_str(),
            Some(_) => return self.fail_setup(ctx, "Fluid role path must be a string".into()),
        };
        let mode = match resolve_index(ctx, "geometry", 0, GEOMETRY_MODES.len() as u32) {
            Ok(0) => GeometryMode::CollisionProxy,
            Ok(1) => GeometryMode::ClosedMesh,
            Ok(_) => unreachable!(),
            Err(error) => return self.fail_setup(ctx, error),
        };
        let shape = match resolve_index(ctx, "shape", 1, PLATONIC_SHAPES.len() as u32) {
            Ok(value) => value,
            Err(error) => return self.fail_setup(ctx, error),
        };
        let radius = match resolve_f32(ctx, "radius", 1.0) {
            Ok(value) if value.is_finite() && value > 0.0 => value,
            Ok(_) => {
                return self
                    .fail_setup(ctx, "Fluid role radius must be finite and positive".into());
            }
            Err(error) => return self.fail_setup(ctx, error),
        };
        let selection = match resolve_selection(ctx) {
            Ok(selection) => selection,
            Err(error) => return self.fail_setup(ctx, error),
        };
        let setup_changed = self.last_key.as_ref().is_none_or(|last| {
            !last.matches(path, selection, shape, radius, source_transform, mode)
        });
        if setup_changed {
            self.last_key = Some(PreparationKey {
                path: path.to_owned(),
                selection,
                shape,
                radius,
                source_transform,
                mode,
            });
            self.geometry = None;
            self.preparation_error = None;
            self.pending_geometry = None;
            let (tx, rx) = mpsc::channel();
            let path_for_worker = PathBuf::from(path);
            match std::thread::Builder::new()
                .name("fluid-role-prepare".into())
                .spawn(move || {
                    let result = prepare_geometry(
                        &path_for_worker,
                        selection,
                        shape,
                        radius,
                        source_transform,
                        mode,
                    )
                    .map(|meshes| Arc::new(PreparedFluidGeometry { meshes }));
                    let _ = tx.send(result);
                }) {
                Ok(_) => self.pending_geometry = Some(rx),
                Err(error) => {
                    self.preparation_error =
                        Some(format!("Fluid role preparation could not start: {error}"))
                }
            }
        }

        if let Some(rx) = &self.pending_geometry {
            match rx.try_recv() {
                Ok(Ok(geometry)) => {
                    self.geometry = Some(geometry);
                    self.pending_geometry = None;
                    self.preparation_error = None;
                }
                Ok(Err(error)) => {
                    self.pending_geometry = None;
                    self.geometry = None;
                    self.preparation_error = Some(error);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending_geometry = None;
                    self.geometry = None;
                    self.preparation_error = Some("Fluid role preparation disconnected".into());
                }
            }
        }

        if self.pending_geometry.is_some()
            || self.geometry.is_none()
            || self.preparation_error.is_some()
        {
            if let Some(error) = &self.preparation_error {
                ctx.error(error.clone());
            }
            ctx.mark_outputs_pending();
            return;
        }
        ctx.outputs.set_fluid_role(
            "role",
            FluidRole {
                geometry: self.geometry.as_ref().expect("checked above").clone(),
                kind: role,
                transform,
                enabled,
                velocity,
                inherit_motion,
                friction,
            },
        );
    }
}

impl FluidRoleSource {
    fn fail(&mut self, ctx: &mut EffectNodeContext<'_, '_>, error: String) {
        ctx.error(error);
        ctx.mark_outputs_pending();
    }

    fn fail_setup(&mut self, ctx: &mut EffectNodeContext<'_, '_>, error: String) {
        self.last_key = None;
        self.pending_geometry = None;
        self.geometry = None;
        self.preparation_error = Some(error.clone());
        ctx.error(error);
        ctx.mark_outputs_pending();
    }
}

fn validate_transform(transform: Transform, label: &str) -> Result<(), String> {
    if transform.billboard {
        return Err(format!("{label} cannot use billboard mode"));
    }
    if transform
        .pos
        .into_iter()
        .chain(transform.rot_euler)
        .chain(transform.scale)
        .any(|value| !value.is_finite())
    {
        return Err(format!("{label} must be finite"));
    }
    if transform.scale.iter().any(|value| *value <= 0.0) {
        return Err(format!("{label} scale must be strictly positive"));
    }
    Ok(())
}

fn wired_f32(ctx: &EffectNodeContext<'_, '_>, name: &str) -> Result<Option<f32>, String> {
    match ctx.inputs.scalar(name) {
        None => Ok(None),
        Some(ParamValue::Float(value)) => Ok(Some(value)),
        Some(_) => Err(format!("Fluid role input `{name}` must be a scalar float")),
    }
}

fn resolve_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> Result<f32, String> {
    if let Some(value) = wired_f32(ctx, name)? {
        return Ok(value);
    }
    match ctx.params.get(name) {
        None => Ok(default),
        Some(ParamValue::Float(value)) => Ok(*value),
        Some(_) => Err(format!("Fluid role parameter `{name}` must be a float")),
    }
}

fn resolve_velocity(ctx: &EffectNodeContext<'_, '_>) -> Result<[f32; 3], String> {
    Ok([
        resolve_f32(ctx, "velocity_x", 0.0)?,
        resolve_f32(ctx, "velocity_y", 0.0)?,
        resolve_f32(ctx, "velocity_z", 0.0)?,
    ])
}

fn resolve_index(
    ctx: &EffectNodeContext<'_, '_>,
    name: &str,
    default: u32,
    count: u32,
) -> Result<u32, String> {
    let value = if let Some(value) = wired_f32(ctx, name)? {
        value
    } else {
        match ctx.params.get(name) {
            Some(ParamValue::Enum(value)) => *value as f32,
            Some(ParamValue::Float(value)) => *value,
            None => default as f32,
            Some(_) => {
                return Err(format!(
                    "Fluid role parameter `{name}` must be an enum or integer"
                ));
            }
        }
    };
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value >= count as f32 {
        return Err(format!(
            "Fluid role `{name}` must be an integer in 0..{}",
            count - 1
        ));
    }
    Ok(value as u32)
}

fn resolve_bool(
    ctx: &EffectNodeContext<'_, '_>,
    name: &str,
    default: bool,
) -> Result<bool, String> {
    if let Some(value) = wired_f32(ctx, name)? {
        if value.is_finite() {
            return Ok(value > 0.5);
        }
        return Err(format!("Fluid role `{name}` must be finite"));
    }
    match ctx.params.get(name) {
        Some(ParamValue::Bool(value)) => Ok(*value),
        Some(ParamValue::Float(value)) if value.is_finite() => Ok(*value > 0.5),
        None => Ok(default),
        Some(_) => Err(format!("Fluid role parameter `{name}` must be a bool")),
    }
}

fn resolve_integer(
    ctx: &EffectNodeContext<'_, '_>,
    name: &str,
    default: i32,
) -> Result<i32, String> {
    let value = resolve_f32(ctx, name, default as f32)?;
    if !value.is_finite()
        || value.fract() != 0.0
        || value < i32::MIN as f32
        || value > i32::MAX as f32
    {
        return Err(format!("Fluid role `{name}` must be a finite integer"));
    }
    Ok(value as i32)
}

fn resolve_selection(ctx: &EffectNodeContext<'_, '_>) -> Result<MeshSelection, String> {
    let mesh = resolve_integer(ctx, "mesh_index", -1)?;
    let primitive = resolve_integer(ctx, "primitive_index", -1)?;
    let material = resolve_integer(ctx, "material_index", -1)?;
    if mesh < -1 || primitive < -1 || material < -2 {
        return Err("Fluid role mesh selectors must be >= -1 (material >= -2)".into());
    }
    let fit = resolve_index(ctx, "fit", 0, 2)? == 1;
    let recenter = resolve_bool(ctx, "recenter", true)?;
    let translate = [
        resolve_f32(ctx, "translate_x", 0.0)?,
        resolve_f32(ctx, "translate_y", 0.0)?,
        resolve_f32(ctx, "translate_z", 0.0)?,
    ];
    if translate.iter().any(|value| !value.is_finite()) {
        return Err("Fluid role source offsets must be finite".into());
    }
    let fragment_count = resolve_integer(ctx, "fragment_count", 1)?;
    let fragment_index = resolve_integer(ctx, "fragment_index", 0)?;
    let collider_parts = resolve_integer(ctx, "collider_parts", 32)?;
    if !(1..=64).contains(&fragment_count) || fragment_index < 0 || fragment_index >= fragment_count
    {
        return Err("Fluid role fragment_index must be within fragment_count 1..=64".into());
    }
    if !(1..=64).contains(&collider_parts) {
        return Err("Fluid role collider_parts must be in 1..=64".into());
    }
    Ok(MeshSelection {
        mesh,
        primitive,
        material,
        fit,
        recenter,
        translate: [translate[0], translate[1], translate[2]],
        fragment_count: fragment_count as u32,
        fragment_index: fragment_index as u32,
        collider_parts: collider_parts as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::{EffectNodeContext, FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::PortType;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::{EffectNode, MockBackend};
    use manifold_core::{Beats, Seconds};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn run_once(
        primitive: &mut FluidRoleSource,
        backend: &mut MockBackend,
        transform_slot: Slot,
        output_slot: Slot,
        params: &ParamValues,
    ) -> bool {
        let input_bindings: &[(&'static str, Slot)] = &[("transform", transform_slot)];
        let output_bindings: &[(&'static str, Slot)] = &[("role", output_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let mut role_scratch = Vec::new();
        let inputs = NodeInputs::new(input_bindings, backend, &[]);
        let outputs = NodeOutputs::new(
            output_bindings,
            backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut render_mode_scratch,
            &mut object_scratch,
        )
        .with_fluid_role_writes(&mut role_scratch);
        let pending = {
            let mut ctx = EffectNodeContext::new(frame_time(), params, inputs, outputs, None);
            primitive.run(&mut ctx);
            ctx.outputs_pending
        };
        for (slot, value) in role_scratch.drain(..) {
            backend.set_fluid_role(slot, value);
        }
        pending
    }

    fn test_slots(backend: &mut MockBackend) -> (Slot, Slot) {
        let transform_slot = backend.acquire(ResourceId(0), PortType::Transform, None, (0, 0));
        let output_slot = backend.acquire(ResourceId(1), PortType::FluidRole, None, (0, 0));
        backend.set_transform(transform_slot, Transform::default());
        (transform_slot, output_slot)
    }

    fn settle(
        primitive: &mut FluidRoleSource,
        backend: &mut MockBackend,
        transform_slot: Slot,
        output_slot: Slot,
        params: &ParamValues,
    ) -> FluidRole {
        for _ in 0..200 {
            let pending = run_once(primitive, backend, transform_slot, output_slot, params);
            if !pending && let Some(role) = backend.fluid_role(output_slot) {
                return role;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("fluid role source preparation did not settle");
    }

    #[test]
    fn scene_physics_fluid_role_source_declares_typed_role_output_and_required_transform() {
        assert_eq!(FluidRoleSource::TYPE_ID, "node.fluid_role_source");
        assert_eq!(FluidRoleSource::OUTPUTS.len(), 1);
        assert_eq!(FluidRoleSource::OUTPUTS[0].name, "role");
        assert_eq!(FluidRoleSource::OUTPUTS[0].ty, PortType::FluidRole);
        let transform = FluidRoleSource::INPUTS
            .iter()
            .find(|port| port.name == "transform")
            .unwrap();
        assert!(transform.required);
        assert_eq!(transform.ty, PortType::Transform);
    }

    #[test]
    fn scene_physics_fluid_role_source_exposes_all_four_roles_and_two_geometry_modes() {
        assert_eq!(
            FLUID_ROLE_KINDS,
            &["Initial Fill", "Inflow", "Outflow", "Collider"]
        );
        assert_eq!(GEOMETRY_MODES, &["Collision Proxy", "Closed Mesh"]);
        assert_eq!(
            FluidRoleSource::PARAMS
                .iter()
                .find(|param| param.name == "role")
                .unwrap()
                .default,
            ParamValue::Enum(1)
        );
        assert_eq!(
            FluidRoleSource::PARAMS
                .iter()
                .find(|param| param.name == "geometry")
                .unwrap()
                .default,
            ParamValue::Enum(0)
        );
    }

    #[test]
    fn scene_physics_fluid_role_source_registers_as_non_gpu_source() {
        let prim = FluidRoleSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fluid_role_source");
    }

    #[test]
    fn scene_physics_fluid_role_source_builtin_default_runs_without_path_param() {
        let mut backend = MockBackend::new();
        let (transform_slot, output_slot) = test_slots(&mut backend);
        let mut primitive = FluidRoleSource::new();
        let params = ParamValues::default();
        let role = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &params,
        );
        assert_eq!(role.kind, FluidRoleKind::Inflow);
        assert_eq!(role.geometry.meshes.len(), 1);
        assert!(!role.geometry.meshes[0].triangles.is_empty());
    }

    #[test]
    fn scene_physics_fluid_role_source_recovers_after_invalid_setup_is_undone() {
        let mut backend = MockBackend::new();
        let (transform_slot, output_slot) = test_slots(&mut backend);
        let mut primitive = FluidRoleSource::new();
        let params = ParamValues::default();
        let first = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &params,
        );
        let mut invalid = params.clone();
        invalid.insert(Cow::Borrowed("radius"), ParamValue::Float(-1.0));
        assert!(run_once(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &invalid
        ));
        assert!(primitive.geometry.is_none());
        let restored = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &params,
        );
        assert!(!Arc::ptr_eq(&first.geometry, &restored.geometry));
        assert_eq!(first.geometry.meshes, restored.geometry.meshes);
    }

    #[test]
    fn scene_physics_fluid_role_source_live_controls_reuse_geometry_arc() {
        let mut backend = MockBackend::new();
        let (transform_slot, output_slot) = test_slots(&mut backend);
        let mut primitive = FluidRoleSource::new();
        let mut params: ParamValues = FluidRoleSource::PARAMS
            .iter()
            .map(|param| (param.name.clone(), param.default.clone()))
            .collect();
        let first = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &params,
        );
        params.insert(Cow::Borrowed("velocity_x"), ParamValue::Float(4.0));
        params.insert(Cow::Borrowed("friction"), ParamValue::Float(0.75));
        run_once(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &params,
        );
        let second = backend
            .fluid_role(output_slot)
            .expect("live-control update should publish");
        assert!(Arc::ptr_eq(&first.geometry, &second.geometry));
        assert_eq!(second.velocity, [4.0, 0.0, 0.0]);
        assert_eq!(second.friction, 0.75);
    }

    #[test]
    fn scene_physics_fluid_role_source_setup_revision_discards_old_arc_before_republish() {
        let mut backend = MockBackend::new();
        let (transform_slot, output_slot) = test_slots(&mut backend);
        let mut primitive = FluidRoleSource::new();
        let params = ParamValues::default();
        let first = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &params,
        );
        let mut changed = params.clone();
        changed.insert(Cow::Borrowed("radius"), ParamValue::Float(2.0));
        run_once(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &changed,
        );
        let after_revision = backend.fluid_role(output_slot);
        assert!(
            Primitive::warmup_pending(&primitive)
                || after_revision
                    .as_ref()
                    .is_some_and(|role| !Arc::ptr_eq(&first.geometry, &role.geometry))
        );
        let second = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &changed,
        );
        assert!(!Arc::ptr_eq(&first.geometry, &second.geometry));
        assert_eq!(
            second.geometry.meshes[0].vertices.len(),
            first.geometry.meshes[0].vertices.len()
        );
    }
}
