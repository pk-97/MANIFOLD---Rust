use manifold_node_engine::mesh::PLATONIC_SHAPES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::scene::mesh_source::MeshSource;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use crate::physics::{ColliderGeometry, RigidBody};
use manifold_node_engine::scene::physics_mesh::{MeshSelection, PART_PORTS, load_compound_materials, parse_compound_materials, transform_vertices, validate_transform};
use crate::physics_mesh::prepare_colliders;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::transform::Transform;
use std::borrow::Cow;
use std::sync::{Arc, mpsc};

const IDENTITY_TRANSFORM: Transform = Transform {
    pos: [0.0; 3],
    rot_euler: [0.0; 3],
    scale: [1.0; 3],
    billboard: false,
};

fn same_mesh_source(left: Option<&MeshSource>, right: Option<&MeshSource>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(MeshSource::Cube { size: left }), Some(MeshSource::Cube { size: right })) => {
            left.to_bits() == right.to_bits()
        }
        (
            Some(MeshSource::Platonic {
                shape: left_shape,
                radius: left_radius,
            }),
            Some(MeshSource::Platonic {
                shape: right_shape,
                radius: right_radius,
            }),
        ) => left_shape == right_shape && left_radius.to_bits() == right_radius.to_bits(),
        (
            Some(MeshSource::Gltf {
                path: left_path,
                selection: left_selection,
            }),
            Some(MeshSource::Gltf {
                path: right_path,
                selection: right_selection,
            }),
        ) => left_path == right_path && same_selection(left_selection, right_selection),
        _ => false,
    }
}

fn same_selection(left: &MeshSelection, right: &MeshSelection) -> bool {
    left.mesh == right.mesh
        && left.primitive == right.primitive
        && left.material == right.material
        && left.fit == right.fit
        && left.recenter == right.recenter
        && left
            .translate
            .iter()
            .zip(right.translate.iter())
            .all(|(left, right)| left.to_bits() == right.to_bits())
        && left.fragment_count == right.fragment_count
        && left.fragment_index == right.fragment_index
        && left.collider_parts == right.collider_parts
}

fn same_optional_selection(left: Option<MeshSelection>, right: Option<MeshSelection>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => same_selection(&left, &right),
        _ => false,
    }
}

manifold_node_engine::primitive! {
 name: RigidBodyNode,
 type_id: "node.rigid_body",
 purpose: "Describe a rigid body's shape, starting transform, motion type, density and contact properties; Box3D gives it density times its installed hull volume as mass. Wire body into a shared Physics World. An optional builtin or imported MeshSource is prepared once as convex hulls using standard Box3D.",
 inputs: {
  transform: Transform required,
  source: MeshSource optional,
  source_transform: Transform optional,
  part_0: Transform optional,
  part_1: Transform optional,
  part_2: Transform optional,
  part_3: Transform optional,
  part_4: Transform optional,
  part_5: Transform optional,
  part_6: Transform optional,
  part_7: Transform optional,
  part_8: Transform optional,
  part_9: Transform optional,
  part_10: Transform optional,
  part_11: Transform optional,
  part_12: Transform optional,
  part_13: Transform optional,
  part_14: Transform optional,
  part_15: Transform optional,
  part_16: Transform optional,
  part_17: Transform optional,
  part_18: Transform optional,
  part_19: Transform optional,
  part_20: Transform optional,
  part_21: Transform optional,
  part_22: Transform optional,
  part_23: Transform optional,
  part_24: Transform optional,
  part_25: Transform optional,
  part_26: Transform optional,
  part_27: Transform optional,
  part_28: Transform optional,
  part_29: Transform optional,
  part_30: Transform optional,
  part_31: Transform optional,
  part_32: Transform optional,
  part_33: Transform optional,
  part_34: Transform optional,
  part_35: Transform optional,
  part_36: Transform optional,
  part_37: Transform optional,
  part_38: Transform optional,
  part_39: Transform optional,
  part_40: Transform optional,
  part_41: Transform optional,
  part_42: Transform optional,
  part_43: Transform optional,
  part_44: Transform optional,
  part_45: Transform optional,
  part_46: Transform optional,
  part_47: Transform optional,
  part_48: Transform optional,
  part_49: Transform optional,
  part_50: Transform optional,
  part_51: Transform optional,
  part_52: Transform optional,
  part_53: Transform optional,
  part_54: Transform optional,
  part_55: Transform optional,
  part_56: Transform optional,
  part_57: Transform optional,
  part_58: Transform optional,
  part_59: Transform optional,
  part_60: Transform optional,
  part_61: Transform optional,
  part_62: Transform optional,
  part_63: Transform optional,
  density: ScalarF32 optional,
  friction: ScalarF32 optional,
  bounce: ScalarF32 optional,
  release_count: ScalarF32 optional,
 },
 outputs: { body: RigidBody, shape: ScalarF32, },
params: [
ParamDef { name: Cow::Borrowed("enabled"), label: "Physics", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
ParamDef { name: Cow::Borrowed("shape"), label: "Shape", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, 4.0)), enum_values: PLATONIC_SHAPES },
ParamDef { name: Cow::Borrowed("motion"), label: "Motion", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, 2.0)), enum_values: &["Fixed", "Moving", "Animated"] },
ParamDef { name: Cow::Borrowed("density"), label: "Density (kg/m³)", ty: ParamType::Float, default: ParamValue::Float(crate::physics::DEFAULT_DENSITY), range: Some((1.0, 25_000.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("friction"), label: "Friction", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.0, 1.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("bounce"), label: "Bounce", ty: ParamType::Float, default: ParamValue::Float(0.15), range: Some((0.0, 1.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("release_count"), label: "Release", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1_000_000.0)), enum_values: &[] },

ParamDef { name: Cow::Borrowed("path"), label: "Mesh File", ty: ParamType::String, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
ParamDef { name: Cow::Borrowed("mesh_index"), label: "Mesh Index", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0,1024.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("primitive_index"), label: "Primitive Index", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0,1024.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("material_index"), label: "Material Index", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-2.0,1024.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("fit"), label: "Fit", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0,1.0)), enum_values: &["none", "unit_box"] },
ParamDef { name: Cow::Borrowed("recenter"), label: "Recenter", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
ParamDef { name: Cow::Borrowed("translate_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
ParamDef { name: Cow::Borrowed("translate_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
ParamDef { name: Cow::Borrowed("translate_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
ParamDef { name: Cow::Borrowed("fragment_count"), label: "Pieces", ty: ParamType::Int, default: ParamValue::Float(1.0), range: Some((1.0,64.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("fragment_index"), label: "Piece", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0,63.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("fragment_parent"), label: "Fragment Parent", ty: ParamType::Int, default: ParamValue::Float(-1.0), range: Some((-1.0,63.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("collider_parts"), label: "Collider Detail", ty: ParamType::Int, default: ParamValue::Float(32.0), range: Some((1.0,64.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("compound_materials"), label: "Compound Materials", ty: ParamType::Table, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
 ],
 depth_rule: Terminal,
 composition_notes: "One description per body. All bodies that should collide feed the same Physics World. Dynamic bodies respond to gravity. Fixed bodies are static: changing their authored transform teleports them and does not sweep through contacts. Choose Animated for any driven moving or spinning collider; its target moves through the solver with velocity. Scale or shape changes rebuild the world. Primitive mesh radius must be 1. Imported sources copy the mesh selection/fit/offset and prepare convex parts once during warmup, with a 0.1% thickness for open surfaces. Rendering remains unchanged. Transform scale applies equally to mesh and collider.",
 examples: ["PhysicsSolids"],
 picker: { label: "Rigid Body", category: Atom },
 summary: "Give an object density, friction and bounce, then connect it to a Physics World.",
 category: Geometry3D, role: Source,
 aliases: ["physics body", "rigid body", "collider"],
 boundary_reason: NonGpu,
 extra_fields: {
    collider_source: Option<MeshSource> = None,
    collider_path: String = String::new(),
    collider_selection: Option<MeshSelection> = None,
    collider: Option<Arc<ColliderGeometry>> = None,
    pending_collider: Option<mpsc::Receiver<Result<ColliderGeometry, String>>> = None,
    collider_error: Option<String> = None,
    collider_source_transform: Transform = Transform::default(),
    collider_materials: [Option<i32>; 64] = [None; 64],
    collider_part_transforms: [Transform; 64] = [Transform { pos: [0.0; 3], rot_euler: [0.0; 3], scale: [1.0; 3], billboard: false }; 64],
    source_pending: bool = false,
 },
}
impl Primitive for RigidBodyNode {
    fn source_asset_paths(&self) -> &'static [&'static str] {
        &["path"]
    }

    fn source_asset_identity(
        &self,
        _: &manifold_node_engine::exec::effect_node::ParamValues,
    ) -> manifold_node_engine::scene::source_asset::SourceAssetIdentity<'_> {
        // The paired take compares the installed native hulls, including all
        // compound members, before publishing any cached frame.
        manifold_node_engine::scene::source_asset::SourceAssetIdentity::PreparedGeometry
    }

    fn warmup_pending(&self) -> bool {
        self.source_pending || self.pending_collider.is_some()
    }

    // A disabled body ignores its pending source, and an enabled one drops
    // its collider while the source reloads.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if ctx.inputs.any_pending_except(|port| port == "source") {
            ctx.mark_outputs_pending();
            return;
        }
        let Some(transform) = ctx.inputs.transform("transform") else {
            ctx.error("Rigid Body needs a starting transform");
            return;
        };
        let requested_source_transform = ctx
            .inputs
            .transform("source_transform")
            .unwrap_or(IDENTITY_TRANSFORM);
        let enabled = !matches!(ctx.params.get("enabled"), Some(ParamValue::Bool(false)));
        let release_count = ctx.scalar_or_param("release_count", 0.0);
        let fragment_parent = match ctx.params.get("fragment_parent") {
            Some(ParamValue::Float(value)) if value.is_finite() && *value >= 0.0 => {
                Some(value.round() as usize)
            }
            _ => None,
        };
        let selector = |name: &str| match ctx.params.get(name) {
            Some(ParamValue::Enum(v)) => *v,
            Some(ParamValue::Float(v)) => v.round() as u32,
            _ => 1,
        };
        // Disabled bodies emit immediately, so imported collider preparation
        // cannot hold the physics world pending.
        if !enabled {
            self.source_pending = false;
            self.pending_collider = None;
            self.collider_source = None;
            self.collider_path.clear();
            self.collider_selection = None;
            self.collider = None;
            self.collider_error = None;
            self.collider_source_transform = IDENTITY_TRANSFORM;
            self.collider_materials = [None; 64];
            self.collider_part_transforms = [IDENTITY_TRANSFORM; 64];
            let body = RigidBody {
                transform,
                enabled,
                release_count,
                fragment_parent,
                shape: selector("shape"),
                kind: selector("motion"),
                density: ctx.scalar_or_param("density", crate::physics::DEFAULT_DENSITY),
                friction: ctx.scalar_or_param("friction", 0.5),
                bounce: ctx.scalar_or_param("bounce", 0.15),
                collider: self.collider.clone(),
                wall: false,
            };
            let shape = body.shape;
            ctx.outputs.set_cpu_value("body", body);
            ctx.outputs
                .set_scalar("shape", ParamValue::Float(shape as f32));
            return;
        }
        let source_slot = ctx.inputs.slot("source");
        let wired_source = match source_slot {
            None => None,
            Some(slot) if !ctx.inputs.slot_content_ready(slot) => {
                self.source_pending = true;
                self.pending_collider = None;
                self.collider_source = None;
                self.collider_path.clear();
                self.collider_selection = None;
                self.collider = None;
                self.collider_error = None;
                self.collider_source_transform = IDENTITY_TRANSFORM;
                self.collider_materials = [None; 64];
                self.collider_part_transforms = [IDENTITY_TRANSFORM; 64];
                ctx.mark_outputs_pending();
                return;
            }
            Some(slot) => match ctx.inputs.mesh_source_slot(slot) {
                Some(source) => Some(source),
                None => {
                    self.source_pending = true;
                    self.pending_collider = None;
                    self.collider_source = None;
                    self.collider_path.clear();
                    self.collider_selection = None;
                    self.collider = None;
                    self.collider_error = None;
                    self.collider_source_transform = IDENTITY_TRANSFORM;
                    self.collider_materials = [None; 64];
                    self.collider_part_transforms = [IDENTITY_TRANSFORM; 64];
                    ctx.mark_outputs_pending();
                    return;
                }
            },
        };
        self.source_pending = false;
        // A typed source owns the complete geometry description.  Legacy
        // compound/path parameters are only resolved when this port is
        // unwired, so changing them cannot recook a wired source.
        let (compound_materials, compound) = if wired_source.is_some() {
            ([None; 64], false)
        } else {
            match parse_compound_materials(ctx) {
                Ok(value) => value,
                Err(error) => {
                    ctx.error(error);
                    ctx.mark_outputs_pending();
                    return;
                }
            }
        };
        let source_transform = if compound {
            IDENTITY_TRANSFORM
        } else {
            requested_source_transform
        };
        let mut part_transforms = [IDENTITY_TRANSFORM; 64];
        if compound {
            for (slot, port) in PART_PORTS.iter().enumerate() {
                part_transforms[slot] = ctx.inputs.transform(port).unwrap_or(IDENTITY_TRANSFORM);
                if compound_materials[slot].is_some()
                    && let Err(error) = validate_transform(part_transforms[slot])
                {
                    ctx.error(format!("compound part {slot}: {error}"));
                    ctx.mark_outputs_pending();
                    return;
                }
            }
        }
        if let Err(error) = validate_transform(source_transform) {
            ctx.error(error);
            ctx.mark_outputs_pending();
            return;
        }
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(path)) => path.as_str(),
            _ => "",
        };
        let (path, selection) = if wired_source.is_some() {
            ("", None)
        } else {
            (path, Some(MeshSelection::from_context(ctx)))
        };
        let changed = !same_mesh_source(self.collider_source.as_ref(), wired_source.as_ref())
            || self.collider_path != path
            || !same_optional_selection(self.collider_selection, selection)
            || self.collider_source_transform != source_transform
            || self.collider_materials != compound_materials
            || self.collider_part_transforms != part_transforms;
        if changed && self.pending_collider.is_none() {
            self.collider_source = wired_source.clone();
            self.collider_path.clear();
            self.collider_path.push_str(path);
            self.collider_selection = selection;
            self.collider = None;
            self.collider_error = None;
            self.collider_source_transform = source_transform;
            self.collider_materials = compound_materials;
            self.collider_part_transforms = part_transforms;
            if wired_source.is_some() || !path.is_empty() {
                let path = std::path::PathBuf::from(path);
                let source = wired_source.clone();
                let (tx, rx) = mpsc::channel();
                self.pending_collider = Some(rx);
                std::thread::spawn(move || {
                    let result: Result<ColliderGeometry, String> = (|| {
                        if let Some(source) = source {
                            let mut vertices = source.load_vertices()?;
                            transform_vertices(&mut vertices, source_transform)?;
                            match source {
                                MeshSource::Gltf { selection, .. } => {
                                    prepare_colliders(&vertices, selection.collider_parts)
                                }
                                MeshSource::Cube { .. } | MeshSource::Platonic { .. } => {
                                    let points: Vec<_> = vertices
                                        .into_iter()
                                        .map(|vertex| vertex.position)
                                        .collect();
                                    let hull = manifold_physics::cook_hull(&points)
                                        .map_err(|error| error.to_string())?;
                                    Ok(ColliderGeometry { hulls: vec![hull] })
                                }
                            }
                        } else if compound {
                            let selection = selection.expect("legacy compound selection");
                            let vertices = load_compound_materials(
                                &path,
                                selection,
                                compound_materials,
                                part_transforms,
                            )?;
                            prepare_colliders(&vertices, selection.collider_parts)
                        } else {
                            let selection = selection.expect("legacy imported selection");
                            let mut vertices = selection.load(&path)?;
                            transform_vertices(&mut vertices, source_transform)?;
                            prepare_colliders(&vertices, selection.collider_parts)
                        }
                    })();
                    let _ = tx.send(result);
                });
            }
        }
        if let Some(rx) = &self.pending_collider {
            match rx.try_recv() {
                Ok(Ok(collider)) => {
                    if same_mesh_source(self.collider_source.as_ref(), wired_source.as_ref())
                        && same_optional_selection(self.collider_selection, selection)
                        && self.collider_path == path
                        && self.collider_source_transform == source_transform
                        && self.collider_materials == compound_materials
                        && self.collider_part_transforms == part_transforms
                    {
                        self.collider = Some(Arc::new(collider));
                    }
                    self.pending_collider = None;
                }
                Ok(Err(error)) => {
                    if same_mesh_source(self.collider_source.as_ref(), wired_source.as_ref())
                        && same_optional_selection(self.collider_selection, selection)
                        && self.collider_path == path
                        && self.collider_source_transform == source_transform
                        && self.collider_materials == compound_materials
                        && self.collider_part_transforms == part_transforms
                    {
                        self.collider_error = Some(error);
                    }
                    self.pending_collider = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    if same_mesh_source(self.collider_source.as_ref(), wired_source.as_ref())
                        && same_optional_selection(self.collider_selection, selection)
                        && self.collider_path == path
                        && self.collider_source_transform == source_transform
                        && self.collider_materials == compound_materials
                        && self.collider_part_transforms == part_transforms
                    {
                        self.collider_error =
                            Some("Physics collider preparation disconnected".into());
                    }
                    self.pending_collider = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.pending_collider.is_some()
            || self.collider_error.is_some()
            || !same_mesh_source(self.collider_source.as_ref(), wired_source.as_ref())
            || !same_optional_selection(self.collider_selection, selection)
            || self.collider_path != path
            || self.collider_source_transform != source_transform
            || self.collider_materials != compound_materials
            || self.collider_part_transforms != part_transforms
        {
            ctx.mark_outputs_pending();
            if let Some(error) = &self.collider_error {
                ctx.error(error.clone());
            }
            return;
        }
        let body = RigidBody {
            transform,
            enabled,
            release_count,
            fragment_parent,
            shape: selector("shape"),
            kind: selector("motion"),
            density: ctx.scalar_or_param("density", crate::physics::DEFAULT_DENSITY),
            friction: ctx.scalar_or_param("friction", 0.5),
            bounce: ctx.scalar_or_param("bounce", 0.15),
            collider: self.collider.clone(),
            wall: false,
        };
        let shape = body.shape;
        ctx.outputs.set_cpu_value("body", body);
        ctx.outputs
            .set_scalar("shape", ParamValue::Float(shape as f32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::exec::backend::Backend;
    use manifold_node_engine::exec::cpu_values::CpuWireWrites;
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs, Slot};
    use manifold_node_engine::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::ports::{PortType, ScalarType};
    use manifold_node_engine::exec::backend::MockBackend;
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
        primitive: &mut RigidBodyNode,
        backend: &mut MockBackend,
        transform_slot: Slot,
        source_slot: Slot,
        body_slot: Slot,
        shape_slot: Slot,
        params: &ParamValues,
    ) -> bool {
        let input_bindings: &[(&'static str, Slot)] =
            &[("transform", transform_slot), ("source", source_slot)];
        let output_bindings: &[(&'static str, Slot)] =
            &[("body", body_slot), ("shape", shape_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let mut body_scratch = CpuWireWrites::default();
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
        .with_cpu_value_writes(&mut body_scratch);
        let pending = {
            let mut ctx = EffectNodeContext::new(frame_time(), params, inputs, outputs, None);
            primitive.run(&mut ctx);
            ctx.outputs_pending
        };
        body_scratch.commit(backend.cpu_values_mut());
        pending
    }

    fn settle(
        primitive: &mut RigidBodyNode,
        backend: &mut MockBackend,
        transform_slot: Slot,
        source_slot: Slot,
        body_slot: Slot,
        shape_slot: Slot,
        params: &ParamValues,
    ) -> RigidBody {
        for _ in 0..200 {
            let pending = run_once(
                primitive,
                backend,
                transform_slot,
                source_slot,
                body_slot,
                shape_slot,
                params,
            );
            if !pending && let Some(body) = backend.cpu_values().get::<RigidBody>(body_slot) {
                return body;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("rigid body source preparation did not settle");
    }

    #[test]
    fn rigid_body_source_pending_does_not_fallback_and_same_source_rebuilds() {
        let mut backend = MockBackend::new();
        let transform_slot = backend.acquire(ResourceId(0), PortType::Transform, None, (0, 0));
        let source_slot = backend.acquire(ResourceId(1), PortType::MeshSource, None, (0, 0));
        let body_slot = backend.acquire(ResourceId(2), PortType::RigidBody, None, (0, 0));
        let shape_slot =
            backend.acquire(ResourceId(3), PortType::Scalar(ScalarType::F32), None, (0, 0));
        backend.set_transform(transform_slot, Transform::default());
        backend.set_mesh_source(source_slot, MeshSource::Cube { size: 2.0 });
        let mut primitive = RigidBodyNode::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("shape"), ParamValue::Enum(0));
        let first = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            source_slot,
            body_slot,
            shape_slot,
            &params,
        );
        let first_collider = first.collider.clone().expect("source hull");
        assert_eq!(first_collider.hulls.len(), 1);
        let repeated = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            source_slot,
            body_slot,
            shape_slot,
            &params,
        );
        assert!(
            Arc::ptr_eq(
                repeated.collider.as_ref().expect("repeated source hull"),
                &first_collider
            ),
            "stable source evaluation must retain the prepared collider"
        );

        backend.release(ResourceId(1), PortType::MeshSource, None, (0, 0));
        assert!(run_once(
            &mut primitive,
            &mut backend,
            transform_slot,
            source_slot,
            body_slot,
            shape_slot,
            &params
        ));
        assert!(primitive.collider.is_none());
        backend.set_mesh_source(source_slot, MeshSource::Cube { size: 2.0 });
        let restored = settle(
            &mut primitive,
            &mut backend,
            transform_slot,
            source_slot,
            body_slot,
            shape_slot,
            &params,
        );
        let restored_collider = restored.collider.expect("restored source hull");
        assert_eq!(restored_collider.hulls, first_collider.hulls);
        assert_eq!(restored_collider.hulls[0].len(), 8);

        backend.set_mesh_source(source_slot, MeshSource::Cube { size: 0.6 });
        let resized = settle(
            &mut primitive, &mut backend, transform_slot, source_slot,
            body_slot, shape_slot, &params,
        ).collider.expect("edited source hull");
        assert!(!Arc::ptr_eq(&resized, &restored_collider));
        let extent = resized.hulls[0].iter()
            .map(|point| point[0].abs()).fold(0.0_f32, f32::max);
        assert!((extent - 0.3).abs() < 1e-6, "source edits change collision size");

        backend.release(ResourceId(1), PortType::MeshSource, None, (0, 0));
        params.insert(Cow::Borrowed("enabled"), ParamValue::Bool(false));
        assert!(!run_once(
            &mut primitive, &mut backend, transform_slot, source_slot,
            body_slot, shape_slot, &params,
        ), "disabled bodies do not wait for missing geometry");
        assert!(!backend.cpu_values().get::<RigidBody>(body_slot).unwrap().enabled);
    }

    #[test]
    fn rigid_body_invalid_mesh_source_reports_error_after_async_preparation() {
        let mut backend = MockBackend::new();
        let transform_slot = backend.acquire(ResourceId(0), PortType::Transform, None, (0, 0));
        let source_slot = backend.acquire(ResourceId(1), PortType::MeshSource, None, (0, 0));
        let body_slot = backend.acquire(ResourceId(2), PortType::RigidBody, None, (0, 0));
        let shape_slot =
            backend.acquire(ResourceId(3), PortType::Scalar(ScalarType::F32), None, (0, 0));
        backend.set_transform(transform_slot, Transform::default());
        backend.set_mesh_source(source_slot, MeshSource::Cube { size: f32::NAN });
        let mut primitive = RigidBodyNode::new();
        let params = ParamValues::default();
        for _ in 0..200 {
            let pending = run_once(
                &mut primitive,
                &mut backend,
                transform_slot,
                source_slot,
                body_slot,
                shape_slot,
                &params,
            );
            assert!(pending, "invalid geometry must never emit a fallback body");
            if primitive.collider_error.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(primitive.collider_error.is_some());
        assert!(primitive.pending_collider.is_none());
        assert!(run_once(
            &mut primitive, &mut backend, transform_slot, source_slot,
            body_slot, shape_slot, &params,
        ));
        assert!(primitive.collider_error.is_some());
        assert!(primitive.pending_collider.is_none(), "unchanged invalid source retains its failure");
        assert!(backend.cpu_values().get::<RigidBody>(body_slot).is_none());
    }
}
