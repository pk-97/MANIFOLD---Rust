use crate::generators::mesh_common::PLATONIC_SHAPES;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::{ColliderGeometry, RigidBody};
use crate::node_graph::physics_mesh::{
    PART_PORTS, MeshSelection, load_compound_materials, parse_compound_materials,
    prepare_colliders, transform_vertices, validate_transform,
};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::transform::Transform;
use std::borrow::Cow;
use std::sync::{Arc, mpsc};

const IDENTITY_TRANSFORM: Transform = Transform {
    pos: [0.0; 3],
    rot_euler: [0.0; 3],
    scale: [1.0; 3],
    billboard: false,
};

crate::primitive! {
 name: RigidBodyNode,
 type_id: "node.rigid_body",
 purpose: "Describe a rigid body's shape, starting transform, motion type, mass and contact properties. Wire body into a shared Physics World. An optional imported mesh source is prepared once as fitted convex hulls using standard Box3D.",
 inputs: {
  transform: Transform required,
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
  mass: ScalarF32 optional,
  friction: ScalarF32 optional,
  bounce: ScalarF32 optional,
  release_count: ScalarF32 optional,
 },
 outputs: { body: RigidBody, shape: ScalarF32, },
params: [
ParamDef { name: Cow::Borrowed("enabled"), label: "Physics", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
ParamDef { name: Cow::Borrowed("shape"), label: "Shape", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, 4.0)), enum_values: PLATONIC_SHAPES },
ParamDef { name: Cow::Borrowed("motion"), label: "Motion", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, 2.0)), enum_values: &["Fixed", "Moving", "Animated"] },
ParamDef { name: Cow::Borrowed("mass"), label: "Mass (kg)", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.01, 100.0)), enum_values: &[] },
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
 summary: "Give an object mass, friction and bounce, then connect it to a Physics World.",
 category: Geometry3D, role: Source,
 aliases: ["physics body", "rigid body", "collider"],
 boundary_reason: NonGpu,
 extra_fields: {
    collider_path: String = String::new(),
    collider_selection: Option<MeshSelection> = None,
    collider: Option<Arc<ColliderGeometry>> = None,
    pending_collider: Option<mpsc::Receiver<Result<ColliderGeometry, String>>> = None,
    collider_error: Option<String> = None,
    collider_source_transform: Transform = Transform::default(),
    collider_materials: [Option<i32>; 64] = [None; 64],
    collider_part_transforms: [Transform; 64] = [Transform { pos: [0.0; 3], rot_euler: [0.0; 3], scale: [1.0; 3], billboard: false }; 64],
 },
}
impl Primitive for RigidBodyNode {
    fn warmup_pending(&self) -> bool {
        self.pending_collider.is_some()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(transform) = ctx.inputs.transform("transform") else {
            ctx.error("Rigid Body needs a starting transform");
            return;
        };
        let source_transform = ctx
            .inputs
            .transform("source_transform")
            .unwrap_or(IDENTITY_TRANSFORM);
        let (compound_materials, compound) = match parse_compound_materials(ctx) {
            Ok(value) => value,
            Err(error) => {
                ctx.error(error);
                ctx.mark_outputs_pending();
                return;
            }
        };
        let source_transform = if compound {
            IDENTITY_TRANSFORM
        } else {
            source_transform
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
            self.pending_collider = None;
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
                mass: ctx.scalar_or_param("mass", 1.0),
                friction: ctx.scalar_or_param("friction", 0.5),
                bounce: ctx.scalar_or_param("bounce", 0.15),
                collider: self.collider.clone(),
            };
            let shape = body.shape;
            ctx.outputs.set_rigid_body("body", body);
            ctx.outputs
                .set_scalar("shape", ParamValue::Float(shape as f32));
            return;
        }
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(path)) => path.as_str(),
            _ => "",
        };
        let selection = MeshSelection::from_context(ctx);
        let changed = self.collider_path != path
            || self.collider_selection != Some(selection)
            || self.collider_source_transform != source_transform
            || self.collider_materials != compound_materials
            || self.collider_part_transforms != part_transforms;
        if changed && self.pending_collider.is_none() {
            self.collider_path.clear();
            self.collider_path.push_str(path);
            self.collider_selection = Some(selection);
            self.collider = None;
            self.collider_error = None;
            self.collider_source_transform = source_transform;
            self.collider_materials = compound_materials;
            self.collider_part_transforms = part_transforms;
            if !path.is_empty() {
                let path = std::path::PathBuf::from(path);
                let (tx, rx) = mpsc::channel();
                self.pending_collider = Some(rx);
                std::thread::spawn(move || {
                    let result: Result<ColliderGeometry, String> = (|| {
                        if compound {
                            let vertices = load_compound_materials(
                                &path,
                                selection,
                                compound_materials,
                                part_transforms,
                            )?;
                            prepare_colliders(&vertices, selection.collider_parts)
                        } else {
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
                    if self.collider_selection == Some(selection) && self.collider_path == path {
                        self.collider = Some(Arc::new(collider));
                    }
                    self.pending_collider = None;
                }
                Ok(Err(error)) => {
                    self.collider_error = Some(error);
                    self.pending_collider = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.collider_error = Some("Physics collider preparation disconnected".into());
                    self.pending_collider = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.pending_collider.is_some()
            || self.collider_error.is_some()
            || self.collider_selection != Some(selection)
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
            mass: ctx.scalar_or_param("mass", 1.0),
            friction: ctx.scalar_or_param("friction", 0.5),
            bounce: ctx.scalar_or_param("bounce", 0.15),
            collider: self.collider.clone(),
        };
        let shape = body.shape;
        ctx.outputs.set_rigid_body("body", body);
        ctx.outputs
            .set_scalar("shape", ParamValue::Float(shape as f32));
    }
}
