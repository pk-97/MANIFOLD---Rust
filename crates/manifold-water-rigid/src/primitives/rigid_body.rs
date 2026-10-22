use manifold_node_engine::mesh::{InstanceTransform, MeshVertex};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::scene::mesh_source::MeshSource;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use crate::physics::{CopySpawn, MAX_COPIES, RigidBody, Solid};
use manifold_node_engine::scene::mesh_selection::MeshSelection;
use crate::physics_mesh::{MESH_PORTS, PART_PORTS, transform_vertices, validate_transform};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::transform::{IDENTITY_MAT4, Transform, rotation_to_render_scene_euler};
use manifold_physics::collision_shape::{CollisionShape, SCALE_STEPS_PER_OCTAVE, ShapeKey};
use std::borrow::Cow;
use std::sync::{Arc, mpsc};

const IDENTITY_TRANSFORM: Transform = Transform {
    pos: [0.0; 3],
    rot_euler: [0.0; 3],
    scale: [1.0; 3],
    billboard: false,
    parent: IDENTITY_MAT4,
};

/// The box spawner a Physics World reads when this body is its `copies`
/// prototype. Arrangement accepts a wired scalar or the enum param.
fn read_spawn(ctx: &EffectNodeContext<'_, '_>) -> CopySpawn {
    let defaults = CopySpawn::default();
    let wired_layout = ctx.inputs.scalar("copy_layout").and_then(|value| match value {
        ParamValue::Float(value) => Some(value),
        ParamValue::Enum(value) => Some(value as f32),
        _ => None,
    });
    let layout = wired_layout.unwrap_or_else(|| match ctx.params.get("copy_layout") {
        Some(ParamValue::Enum(value)) => *value as f32,
        Some(ParamValue::Float(value)) => *value,
        _ => defaults.layout,
    });
    CopySpawn {
        count: ctx.scalar_or_param("copy_count", defaults.count),
        spacing: ctx.scalar_or_param("copy_size_spacing", defaults.spacing),
        columns: ctx.scalar_or_param("copy_columns", defaults.columns),
        layout,
        turn: [
            ctx.scalar_or_param("copy_turn_x", 0.0),
            ctx.scalar_or_param("copy_turn_y", 0.0),
            ctx.scalar_or_param("copy_turn_z", 0.0),
        ],
    }
}

/// `transform` with its parent frame folded in: the world pose and scale
/// the solver takes. A shear the parent chain makes is dropped.
pub(crate) fn flatten(transform: Transform) -> Transform {
    if transform.parent == IDENTITY_MAT4 {
        return transform;
    }
    let m = transform.world();
    let scale: [f32; 3] = std::array::from_fn(|col| (0..3).map(|row| m[col][row] * m[col][row]).sum::<f32>().sqrt());
    let rotation: [[f32; 3]; 3] =
        std::array::from_fn(|row| std::array::from_fn(|col| if scale[col] > 0.0 { m[col][row] / scale[col] } else { 0.0 }));
    Transform {
        pos: [m[3][0], m[3][1], m[3][2]],
        rot_euler: rotation_to_render_scene_euler(rotation),
        scale,
        billboard: transform.billboard,
        parent: IDENTITY_MAT4,
    }
}

/// The form the solver takes a body in: a Fixed body is a static collider,
/// the exact mesh; anything that moves is convex hulls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Hulls,
    Mesh,
}

/// What a body's collision shape is made from. Equal wants give equal
/// shapes, so a want that has not changed never rebuilds one.
#[derive(Clone, Debug)]
struct ShapeWant {
    /// Wired meshes, each placed by its part transform: `mesh_k` with
    /// `part_k`, or the single `source` with `source_transform` in slot 0.
    parts: Box<[Option<(MeshSource, Transform)>; 64]>,
    /// An unwired body's mesh file and selection (a shatter piece).
    path: String,
    selection: Option<MeshSelection>,
    form: Form,
    /// The scale in eighth-octave steps: hull detail follows the body's size.
    scale_steps: [i32; 3],
}

impl Default for ShapeWant {
    fn default() -> Self {
        Self {
            parts: Box::new(std::array::from_fn(|_| None)),
            path: String::new(),
            selection: None,
            form: Form::Hulls,
            scale_steps: [0; 3],
        }
    }
}

/// Wants compare bit for bit, so a NaN input stays one unchanged want and is
/// refused once instead of rebuilding every frame.
impl PartialEq for ShapeWant {
    fn eq(&self, other: &Self) -> bool {
        let same_part = |a: &Option<(MeshSource, Transform)>, b: &Option<(MeshSource, Transform)>| match (a, b) {
            (None, None) => true,
            (Some((source_a, transform_a)), Some((source_b, transform_b))) => {
                same_source(source_a, source_b) && transform_bits(transform_a) == transform_bits(transform_b)
            }
            _ => false,
        };
        let same_selection = match (&self.selection, &other.selection) {
            (None, None) => true,
            (Some(a), Some(b)) => same_selection(a, b),
            _ => false,
        };
        self.form == other.form
            && self.scale_steps == other.scale_steps
            && self.path == other.path
            && same_selection
            && self.parts.iter().zip(other.parts.iter()).all(|(a, b)| same_part(a, b))
    }
}

fn same_source(a: &MeshSource, b: &MeshSource) -> bool {
    match (a, b) {
        (MeshSource::Cube { size: a }, MeshSource::Cube { size: b }) => a.to_bits() == b.to_bits(),
        (MeshSource::Platonic { shape: shape_a, radius: a }, MeshSource::Platonic { shape: shape_b, radius: b }) => {
            shape_a == shape_b && a.to_bits() == b.to_bits()
        }
        (MeshSource::Plane { width: wa, height: ha }, MeshSource::Plane { width: wb, height: hb }) => {
            wa.to_bits() == wb.to_bits() && ha.to_bits() == hb.to_bits()
        }
        (MeshSource::Gltf { path: path_a, node_path: node_a, selection: a }, MeshSource::Gltf { path: path_b, node_path: node_b, selection: b }) => {
            path_a == path_b && node_a == node_b && same_selection(a, b)
        }
        _ => a == b,
    }
}

fn same_selection(a: &MeshSelection, b: &MeshSelection) -> bool {
    let unplaced = |selection: &MeshSelection| MeshSelection { translate: [0.0; 3], ..*selection };
    a.translate.map(f32::to_bits) == b.translate.map(f32::to_bits) && unplaced(a) == unplaced(b)
}

type TransformBits = ([[u32; 3]; 3], bool, [[u32; 4]; 4]);

fn transform_bits(transform: &Transform) -> TransformBits {
    (
        [transform.pos, transform.rot_euler, transform.scale].map(|v| v.map(f32::to_bits)),
        transform.billboard,
        transform.parent.map(|column| column.map(f32::to_bits)),
    )
}

impl ShapeWant {
    fn is_empty(&self) -> bool {
        self.parts.iter().all(Option::is_none) && self.path.is_empty()
    }

    /// The scale the steps stand for.
    fn scale(&self) -> [f32; 3] {
        self.scale_steps.map(|step| (step as f32 / SCALE_STEPS_PER_OCTAVE).exp2())
    }

    /// Load every part, place it, and make the solid this want asks for.
    /// Runs on a worker thread.
    fn build(&self) -> Result<(ShapeKey, Solid), String> {
        let mut lists: Vec<Vec<[f32; 3]>> = Vec::new();
        for (source, transform) in self.parts.iter().flatten() {
            let mut vertices = source.load_vertices()?;
            transform_vertices(&mut vertices, *transform)?;
            lists.push(positions(&vertices));
        }
        if let Some(selection) = self.selection.filter(|_| !self.path.is_empty()) {
            let vertices = selection.load(std::path::Path::new(&self.path))?;
            lists.push(positions(&vertices));
        }
        let shape = CollisionShape::from_triangle_lists(lists.iter().map(Vec::as_slice)).map_err(|error| error.to_string())?;
        let scale = self.scale();
        let key = ShapeKey::new(&shape, scale, None).map_err(|error| error.to_string())?;
        let solid = match self.form {
            Form::Hulls => Solid::Hulls(shape.body_hulls(scale).map_err(|error| error.to_string())?),
            Form::Mesh => Solid::Mesh(shape.exact_mesh()),
        };
        Ok((key, solid))
    }
}

fn positions(vertices: &[MeshVertex]) -> Vec<[f32; 3]> {
    vertices.iter().map(|vertex| vertex.position).collect()
}

/// Whether reading this frame's geometry found every wired mesh ready.
enum Geometry {
    Ready,
    /// A wired mesh is still loading: the body keeps the shape it has.
    Loading,
}

manifold_node_engine::primitive! {
 name: RigidBodyNode,
 type_id: "node.rigid_body",
 purpose: "Describe a rigid body's starting transform, motion type, mass in kg and contact properties; the mass is the card value whatever the body's size. Its collision shape comes from its mesh automatically (manifold_physics::collision_shape): a Fixed body is the exact mesh, a moving body convex hulls with detail chosen from its size and triangles. Wire body into a shared Physics World; the body's pose (or instances and active_count for a copy group) carries what the world solved to the object that draws it, and acceleration takes a force field that pushes this body only.",
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
  mesh_0: MeshSource optional,
  mesh_1: MeshSource optional,
  mesh_2: MeshSource optional,
  mesh_3: MeshSource optional,
  mesh_4: MeshSource optional,
  mesh_5: MeshSource optional,
  mesh_6: MeshSource optional,
  mesh_7: MeshSource optional,
  mesh_8: MeshSource optional,
  mesh_9: MeshSource optional,
  mesh_10: MeshSource optional,
  mesh_11: MeshSource optional,
  mesh_12: MeshSource optional,
  mesh_13: MeshSource optional,
  mesh_14: MeshSource optional,
  mesh_15: MeshSource optional,
  mesh_16: MeshSource optional,
  mesh_17: MeshSource optional,
  mesh_18: MeshSource optional,
  mesh_19: MeshSource optional,
  mesh_20: MeshSource optional,
  mesh_21: MeshSource optional,
  mesh_22: MeshSource optional,
  mesh_23: MeshSource optional,
  mesh_24: MeshSource optional,
  mesh_25: MeshSource optional,
  mesh_26: MeshSource optional,
  mesh_27: MeshSource optional,
  mesh_28: MeshSource optional,
  mesh_29: MeshSource optional,
  mesh_30: MeshSource optional,
  mesh_31: MeshSource optional,
  mesh_32: MeshSource optional,
  mesh_33: MeshSource optional,
  mesh_34: MeshSource optional,
  mesh_35: MeshSource optional,
  mesh_36: MeshSource optional,
  mesh_37: MeshSource optional,
  mesh_38: MeshSource optional,
  mesh_39: MeshSource optional,
  mesh_40: MeshSource optional,
  mesh_41: MeshSource optional,
  mesh_42: MeshSource optional,
  mesh_43: MeshSource optional,
  mesh_44: MeshSource optional,
  mesh_45: MeshSource optional,
  mesh_46: MeshSource optional,
  mesh_47: MeshSource optional,
  mesh_48: MeshSource optional,
  mesh_49: MeshSource optional,
  mesh_50: MeshSource optional,
  mesh_51: MeshSource optional,
  mesh_52: MeshSource optional,
  mesh_53: MeshSource optional,
  mesh_54: MeshSource optional,
  mesh_55: MeshSource optional,
  mesh_56: MeshSource optional,
  mesh_57: MeshSource optional,
  mesh_58: MeshSource optional,
  mesh_59: MeshSource optional,
  mesh_60: MeshSource optional,
  mesh_61: MeshSource optional,
  mesh_62: MeshSource optional,
  mesh_63: MeshSource optional,
  mass: ScalarF32 optional,
  friction: ScalarF32 optional,
  bounce: ScalarF32 optional,
  release_count: ScalarF32 optional,
  copy_count: ScalarF32 optional,
  copy_size_spacing: ScalarF32 optional,
  copy_columns: ScalarF32 optional,
  copy_layout: ScalarF32 optional,
  copy_turn_x: ScalarF32 optional,
  copy_turn_y: ScalarF32 optional,
  copy_turn_z: ScalarF32 optional,
  acceleration: VectorField optional,
 },
 outputs: { body: RigidBody, pose: Transform, instances: Array(InstanceTransform), active_count: ScalarF32, },
params: [
ParamDef { name: Cow::Borrowed("enabled"), label: "Enabled", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
ParamDef { name: Cow::Borrowed("motion"), label: "Body Type", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, 2.0)), enum_values: &["Fixed", "Moving", "Animated"] },
ParamDef { name: Cow::Borrowed("mass"), label: "Mass", ty: ParamType::Float, default: ParamValue::Float(crate::physics::DEFAULT_MASS_KG), range: Some((0.1, 100_000.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("friction"), label: "Friction", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.0, 1.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("bounce"), label: "Bounce", ty: ParamType::Float, default: ParamValue::Float(0.15), range: Some((0.0, 1.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("release_count"), label: "Break Apart", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1_000_000.0)), enum_values: &[] },

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
ParamDef { name: Cow::Borrowed("copy_count"), label: "Count", ty: ParamType::Int, default: ParamValue::Float(100.0), range: Some((0.0, MAX_COPIES as f32)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("copy_size_spacing"), label: "Spacing", ty: ParamType::Float, default: ParamValue::Float(manifold_core::copies_body::SIZE_SPACING_DEFAULT), range: Some(manifold_core::copies_body::SIZE_SPACING_RANGE), enum_values: &[] },
ParamDef { name: Cow::Borrowed("copy_columns"), label: "Columns", ty: ParamType::Int, default: ParamValue::Float(16.0), range: Some((1.0, 64.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("copy_layout"), label: "Layout", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 3.0)), enum_values: &manifold_core::copies_body::COPY_LAYOUT_LABELS },
ParamDef { name: Cow::Borrowed("copy_turn_x"), label: "Copy Turn X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("copy_turn_y"), label: "Copy Turn Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("copy_turn_z"), label: "Copy Turn Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)), enum_values: &[] },
 ],
 depth_rule: Terminal,
 composition_notes: "One description per body. All bodies that should collide feed the same Physics World. The shape comes from the mesh: wire the object's mesh source into source (one mesh, placed by source_transform) or mesh_k with part_k (a compound body of several meshes, each placed by its part transform, parent frames included); an unwired body reads its mesh file (path and selection), as a shatter piece does. A Fixed body is static and collides as the exact mesh; changing its authored transform teleports it. Moving and Animated bodies collide as convex hulls, detail chosen from size and triangle count, open surfaces given a 0.1% shell. While a new shape is prepared the body keeps its old one; a body with no shape yet stays out of the world until it has one, and the world keeps stepping. The transform's parent frame is folded into the body's world pose. Wired into a Physics World copy-group input copies_k, the body is the prototype of a copy group: copy_count, copy_layout (Grid, Scatter, Line or Ring, centred on the body), copy_size_spacing (a multiple of the copy's box width, 1.0 touching), copy_columns and copy_turn_x/y/z (each copy's own turn) set its copies, which the body's transform places as one group with uniform scale. An edit lays the copies out at rest at the next tick start; Reset returns them to the layout; Enabled off draws them at rest with no physics. Port-shadowed; any other body ignores them. The pose, instances, active_count and acceleration ports are aliases: at graph build they become the world slot the body's membership wire names, and a body with no world passes its authored transform through pose.",
 examples: ["PhysicsSolids", "PhysicsBoxes"],
 picker: { label: "Rigid Body", category: Atom },
 summary: "Give an object a mass in kg, friction and bounce, then connect it to a Physics World.",
 category: Geometry3D, role: Source,
 aliases: ["physics body", "rigid body", "collider"],
 boundary_reason: NonGpu,
 extra_fields: {
    // What the current shape is for, and what this frame asks for.
    want: ShapeWant = ShapeWant::default(),
    asked: ShapeWant = ShapeWant::default(),
    // The last shape made: the body's shape, and its stand-in while a new
    // one is built.
    shape: Option<Arc<Solid>> = None,
    shape_key: Option<(ShapeKey, Form)> = None,
    building: Option<(ShapeWant, mpsc::Receiver<Result<(ShapeKey, Solid), String>>)> = None,
    // Why the shape for `want` could not be made.
    refused: Option<Arc<str>> = None,
    // A wired mesh is still loading.
    loading: bool = false,
    // This node's body, the same for the node's life: a recompile that
    // carries the node keeps it, so the world follows the body to its new
    // port (`RigidBody::identity`).
    identity: u64 = next_identity(),
    // A mesh has reached this node since it was built. Until one has, an
    // empty ask means the wired mesh has not arrived (the first replay after
    // a structural edit runs before the mesh's producer), not that the body
    // has none.
    mesh_seen: bool = false,
 },
}

/// A fresh body identity: never 0, never repeated in this run.
fn next_identity() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl RigidBodyNode {
    /// Fill `self.asked` with what this frame's inputs ask for.
    fn read_geometry(&mut self, ctx: &EffectNodeContext<'_, '_>, form: Form, scale: [f32; 3]) -> Result<Geometry, String> {
        let asked = &mut self.asked;
        asked.form = form;
        // An exact mesh is installed at the exact scale; only hull detail
        // follows the size.
        if form == Form::Mesh {
            asked.scale_steps = [0; 3];
        } else if scale.iter().all(|s| s.is_finite() && *s > 0.0) {
            asked.scale_steps = scale.map(|s| (s.log2() * SCALE_STEPS_PER_OCTAVE).round() as i32);
        }
        asked.path.clear();
        asked.selection = None;
        let wired_parts = MESH_PORTS.iter().any(|port| ctx.inputs.slot(port).is_some());
        let mut loading = false;
        for (index, part) in asked.parts.iter_mut().enumerate() {
            let (port, transform_port) = if wired_parts {
                (MESH_PORTS[index], PART_PORTS[index])
            } else if index == 0 {
                ("source", "source_transform")
            } else {
                *part = None;
                continue;
            };
            let Some(slot) = ctx.inputs.slot(port) else {
                *part = None;
                continue;
            };
            let source = ctx.inputs.slot_content_ready(slot).then(|| ctx.inputs.mesh_source_slot(slot)).flatten();
            let Some(source) = source else {
                loading = true;
                continue;
            };
            let transform = ctx.inputs.transform(transform_port).unwrap_or(IDENTITY_TRANSFORM);
            validate_transform(transform).map_err(|error| format!("mesh part {index}: {error}"))?;
            *part = Some((source, transform));
        }
        if !wired_parts && ctx.inputs.slot("source").is_none()
            && let Some(ParamValue::String(path)) = ctx.params.get("path")
        {
            asked.path.push_str(path);
            asked.selection = Some(MeshSelection::from_context(ctx));
        }
        self.mesh_seen |= !asked.is_empty();
        Ok(if loading || !self.mesh_seen { Geometry::Loading } else { Geometry::Ready })
    }

    /// Start building the asked-for shape when it differs from the current
    /// one, and take a finished build.
    fn prepare(&mut self) {
        if let Some((built_for, receiver)) = &self.building {
            match receiver.try_recv() {
                Err(mpsc::TryRecvError::Empty) => return,
                result => {
                    let result = result.unwrap_or_else(|_| Err("the collision shape build stopped".into()));
                    if *built_for == self.want {
                        match result {
                            Ok((key, solid)) => {
                                if self.shape_key != Some((key, built_for.form)) {
                                    self.shape = Some(Arc::new(solid));
                                    self.shape_key = Some((key, built_for.form));
                                }
                                self.refused = None;
                            }
                            Err(error) => self.refused = Some(Arc::from(error)),
                        }
                    }
                    self.building = None;
                }
            }
        }
        if self.asked == self.want && (self.shape_key.is_some() || self.refused.is_some()) {
            return;
        }
        self.want.clone_from(&self.asked);
        self.refused = None;
        if self.want.is_empty() {
            return;
        }
        let want = self.want.clone();
        let (sender, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("collision-shape".into()).spawn(move || {
            // Always settle, so a build that panicked is a refusal, not a wait.
            let built = std::panic::catch_unwind(|| want.build())
                .unwrap_or_else(|_| Err("the collision shape build panicked".into()));
            let _ = sender.send(built);
        });
        match spawned {
            Ok(_) => self.building = Some((self.want.clone(), receiver)),
            Err(error) => self.refused = Some(Arc::from(format!("the collision shape build could not start: {error}"))),
        }
    }
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
        self.building.is_some() || self.loading
    }

    // A body whose mesh is still loading keeps its last shape.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if ctx.inputs.any_pending_except(|port| port == "source" || MESH_PORTS.contains(&port)) {
            ctx.mark_outputs_pending();
            return;
        }
        let Some(transform) = ctx.inputs.transform("transform") else {
            ctx.error("Rigid Body needs a starting transform");
            return;
        };
        let transform = flatten(transform);
        let enabled = !matches!(ctx.params.get("enabled"), Some(ParamValue::Bool(false)));
        let release_count = ctx.scalar_or_param("release_count", 0.0);
        let fragment_parent = match ctx.params.get("fragment_parent") {
            Some(ParamValue::Float(value)) if value.is_finite() && *value >= 0.0 => {
                Some(value.round() as usize)
            }
            _ => None,
        };
        let kind = match ctx.params.get("motion") {
            Some(ParamValue::Enum(v)) => *v,
            Some(ParamValue::Float(v)) => v.round() as u32,
            _ => 1,
        };
        let form = if kind == 0 { Form::Mesh } else { Form::Hulls };
        let geometry = self.read_geometry(ctx, form, transform.scale);
        self.loading = matches!(geometry, Ok(Geometry::Loading));
        match geometry {
            Ok(Geometry::Ready) => self.prepare(),
            // Nothing new to build until every wired mesh has loaded.
            Ok(Geometry::Loading) => {}
            Err(error) => {
                self.want.clone_from(&self.asked);
                self.refused = Some(Arc::from(error));
            }
        }
        let collider = if self.refused.is_some() { None } else { self.shape.clone() };
        let body = RigidBody {
            transform,
            enabled,
            release_count,
            fragment_parent,
            kind,
            mass: ctx.scalar_or_param("mass", crate::physics::DEFAULT_MASS_KG),
            friction: ctx.scalar_or_param("friction", 0.5),
            bounce: ctx.scalar_or_param("bounce", 0.15),
            collider,
            refused: self.refused.clone(),
            wall: false,
            identity: self.identity,
            spawn: read_spawn(ctx),
        };
        ctx.outputs.set_cpu_value("body", body);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::exec::backend::Backend;
    use manifold_node_engine::exec::cpu_values::CpuWireWrites;
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs, Slot};
    use manifold_node_engine::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::ports::PortType;
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

    struct Rig {
        node: RigidBodyNode,
        backend: MockBackend,
        transform: Slot,
        source: Slot,
        body: Slot,
        params: ParamValues,
    }

    impl Rig {
        fn new(source: MeshSource) -> Self {
            let mut backend = MockBackend::new();
            let transform = backend.acquire(ResourceId(0), PortType::Transform, None, (0, 0));
            let source_slot = backend.acquire(ResourceId(1), PortType::MeshSource, None, (0, 0));
            let body = backend.acquire(ResourceId(2), PortType::RigidBody, None, (0, 0));
            backend.set_transform(transform, Transform::default());
            backend.set_mesh_source(source_slot, source);
            Self { node: RigidBodyNode::new(), backend, transform, source: source_slot, body, params: ParamValues::default() }
        }

        fn run(&mut self) -> RigidBody {
            let input_bindings: &[(&'static str, Slot)] = &[("transform", self.transform), ("source", self.source)];
            let output_bindings: &[(&'static str, Slot)] = &[("body", self.body)];
            let mut scalar_scratch = Vec::new();
            let mut camera_scratch = Vec::new();
            let mut light_scratch = Vec::new();
            let mut material_scratch = Vec::new();
            let mut transform_scratch = Vec::new();
            let mut atmosphere_scratch = Vec::new();
            let mut render_mode_scratch = Vec::new();
            let mut object_scratch = Vec::new();
            let mut body_scratch = CpuWireWrites::default();
            let inputs = NodeInputs::new(input_bindings, &self.backend, &[]);
            let outputs = NodeOutputs::new(
                output_bindings,
                &self.backend,
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
            {
                let mut ctx = EffectNodeContext::new(frame_time(), &self.params, inputs, outputs, None);
                self.node.run(&mut ctx);
                assert!(!ctx.outputs_pending, "a body never holds its world");
            }
            body_scratch.commit(self.backend.cpu_values_mut());
            self.backend.cpu_values().get::<RigidBody>(self.body).expect("a body every frame")
        }

        /// Run until the shape build settles.
        fn settle(&mut self) -> RigidBody {
            for _ in 0..400 {
                let body = self.run();
                if !self.node.warmup_pending() {
                    return body;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("the collision shape build did not settle");
        }
    }

    #[test]
    fn a_body_keeps_its_shape_while_a_new_one_is_built() {
        let mut rig = Rig::new(MeshSource::Cube { size: 2.0 });
        let first = rig.settle().collider.expect("cube shape");
        let hulls = first.hulls().expect("a moving body is hulls");
        assert_eq!(hulls.len(), 1, "a cube is convex: one hull");
        assert_eq!(hulls[0].len(), 8);
        assert!(Arc::ptr_eq(&rig.settle().collider.unwrap(), &first), "an unchanged mesh keeps its shape");

        rig.backend.release(ResourceId(1), PortType::MeshSource, None, (0, 0));
        let reloading = rig.run();
        assert!(Arc::ptr_eq(reloading.collider.as_ref().unwrap(), &first), "a reloading mesh keeps the old shape");

        rig.backend.set_mesh_source(rig.source, MeshSource::Cube { size: 0.6 });
        let resized = rig.settle().collider.expect("edited shape");
        assert!(!Arc::ptr_eq(&resized, &first));
        let extent = resized.hulls().unwrap()[0].iter().map(|point| point[0].abs()).fold(0.0_f32, f32::max);
        assert!((extent - 0.3).abs() < 1e-6, "source edits change collision size");

        rig.params.insert(Cow::Borrowed("motion"), ParamValue::Enum(0));
        let fixed = rig.settle().collider.expect("static shape");
        assert!(matches!(*fixed, Solid::Mesh(_)), "a Fixed body is the exact mesh");
    }

    #[test]
    fn a_bad_mesh_is_refused_with_a_reason() {
        let mut rig = Rig::new(MeshSource::Cube { size: f32::NAN });
        let body = rig.settle();
        assert!(body.collider.is_none(), "invalid geometry never becomes a shape");
        assert!(body.refused.is_some());
        let again = rig.run();
        assert!(again.refused.is_some() && rig.node.building.is_none(), "an unchanged bad mesh is not rebuilt");
    }
}
