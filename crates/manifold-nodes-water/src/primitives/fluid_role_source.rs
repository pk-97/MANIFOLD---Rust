//! `node.fluid_role_source` — prepare one immutable geometry source and emit a
//! CPU `FluidRole` wire.  The node has no solver state, native handles, timing,
//! or rendering responsibilities.

manifold_core::testkit_visible! {
pub(crate) mod geometry;
}

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use manifold_node_engine::mesh::PLATONIC_SHAPES;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use crate::fluid_role::{FluidRole, FluidRoleKind, PreparedFluidGeometry};
use manifold_node_engine::scene::mesh_source::MeshSource;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::scene::mesh_selection::MeshSelection;
use crate::physics_mesh::{PART_PORTS, parse_compound_materials};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::transform::Transform;
use geometry::{GeometryMode, prepare_geometry, prepare_wired_geometry};

const GEOMETRY_MODES: &[&str] = &["Collision Proxy", "Closed Mesh"];
const FLUID_ROLE_KINDS: &[&str] = &["Initial Fill", "Inflow", "Outflow", "Collider"];

manifold_core::testkit_visible! {
    testkit {
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompoundPreparation {
    pub materials: [Option<i32>; 64],
    pub part_transforms: [Transform; 64],
}
    }
    production {
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CompoundPreparation {
    pub(crate) materials: [Option<i32>; 64],
    pub(crate) part_transforms: [Transform; 64],
}
    }
}

pub(crate) const MESH_PORTS: [&str; 64] = [
    "mesh_0", "mesh_1", "mesh_2", "mesh_3", "mesh_4", "mesh_5", "mesh_6", "mesh_7", "mesh_8",
    "mesh_9", "mesh_10", "mesh_11", "mesh_12", "mesh_13", "mesh_14", "mesh_15", "mesh_16",
    "mesh_17", "mesh_18", "mesh_19", "mesh_20", "mesh_21", "mesh_22", "mesh_23", "mesh_24",
    "mesh_25", "mesh_26", "mesh_27", "mesh_28", "mesh_29", "mesh_30", "mesh_31", "mesh_32",
    "mesh_33", "mesh_34", "mesh_35", "mesh_36", "mesh_37", "mesh_38", "mesh_39", "mesh_40",
    "mesh_41", "mesh_42", "mesh_43", "mesh_44", "mesh_45", "mesh_46", "mesh_47", "mesh_48",
    "mesh_49", "mesh_50", "mesh_51", "mesh_52", "mesh_53", "mesh_54", "mesh_55", "mesh_56",
    "mesh_57", "mesh_58", "mesh_59", "mesh_60", "mesh_61", "mesh_62", "mesh_63",
];

manifold_core::testkit_visible! {
    testkit {
#[derive(Clone, Debug, PartialEq)]
pub struct WiredPreparation {
    pub sources: [Option<MeshSource>; 64],
    pub part_transforms: [Transform; 64],
}
    }
    production {
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WiredPreparation {
    pub(crate) sources: [Option<MeshSource>; 64],
    pub(crate) part_transforms: [Transform; 64],
}
    }
}

manifold_core::testkit_visible! {
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparationKey {
    path: String,
    selection: MeshSelection,
    shape: u32,
    radius: f32,
    source_transform: Transform,
    mode: GeometryMode,
    compound: Option<CompoundPreparation>,
    wired: Option<Box<WiredPreparation>>,
}
}

impl PreparationKey {
    fn matches(
        &self,
        path: &str,
        selection: MeshSelection,
        builtin: (u32, f32),
        source_transform: Transform,
        mode: GeometryMode,
        compound: Option<&CompoundPreparation>,
        wired: Option<&WiredPreparation>,
    ) -> bool {
        self.path == path
            && self.selection == selection
            && self.shape == builtin.0
            && self.radius == builtin.1
            && self.source_transform == source_transform
            && self.mode == mode
            && self.compound.as_ref() == compound
            && self.wired.as_deref() == wired
    }
}

manifold_node_engine::primitive! {
    name: FluidRoleSource,
    type_id: "node.fluid_role_source",
    purpose: "Prepare one built-in or imported closed volume as a CPU FluidRole wire. Collision Proxy cooks reusable Box3D hulls; Closed Mesh preserves the exact indexed surface after exact-coordinate welding and validates it as a closed volume. Live transform, enabled, velocity, inheritance, and friction controls do not recook geometry.",
    inputs: {
        transform: Transform required,
        source_transform: Transform optional,
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
        ParamDef { name: Cow::Borrowed("compound_materials"), label: "Compound Materials", ty: ParamType::Table, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
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
        source_pending: bool = false,
    },
}

impl Primitive for FluidRoleSource {
    fn source_asset_paths(&self) -> &'static [&'static str] {
        &["path"]
    }

    fn source_asset_identity(
        &self,
        _: &manifold_node_engine::exec::effect_node::ParamValues,
    ) -> manifold_node_engine::scene::source_asset::SourceAssetIdentity<'_> {
        // Native take preflight compares the complete accepted role mesh.
        manifold_node_engine::scene::source_asset::SourceAssetIdentity::PreparedGeometry
    }

    fn warmup_pending(&self) -> bool {
        self.source_pending || self.pending_geometry.is_some()
    }

    // Drops its prepared geometry while a wired mesh source reloads, and
    // historical samples publish against the last prepared source.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Wired mesh sources and their part transforms are handled below.
        let wired_source_port = |port: &str| {
            MESH_PORTS.contains(&port)
                || PART_PORTS
                    .iter()
                    .position(|part| *part == port)
                    .is_some_and(|index| ctx.inputs.slot(MESH_PORTS[index]).is_some())
        };
        if ctx.inputs.any_pending_except(wired_source_port) {
            ctx.mark_outputs_pending();
            return;
        }
        let Some(transform) = ctx.inputs.transform("transform") else {
            return self.fail(ctx, "Fluid Role Source needs a transform".into());
        };
        if let Err(error) = validate_transform(transform, "body transform") {
            return self.fail(ctx, error);
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

        // Historical samples update live controls against the last prepared
        // source. Setup edits are discrete and must not recook past geometry.
        if ctx.sim_step.authored_sample_only {
            self.publish_role(
                ctx,
                role,
                transform,
                enabled,
                velocity,
                inherit_motion,
                friction,
            );
            return;
        }

        let source_transform = ctx.inputs.transform("source_transform").unwrap_or_default();
        if let Err(error) = validate_transform(source_transform, "source transform") {
            return self.fail_setup(ctx, error);
        }
        self.source_pending = false;
        let wired = match resolve_wired_sources(ctx) {
            Ok((wired, false)) => wired,
            Ok((_, true)) => {
                self.source_pending = true;
                // Old prepared geometry cannot become ready while the visible
                // source is loading a different selection.
                self.last_key = None;
                self.pending_geometry = None;
                self.geometry = None;
                self.preparation_error = None;
                ctx.mark_outputs_pending();
                return;
            }
            Err(error) => return self.fail_setup(ctx, error),
        };
        let compound = if wired.is_none() {
            match resolve_compound(ctx) {
                Ok(compound) => compound,
                Err(error) => return self.fail_setup(ctx, error),
            }
        } else {
            None
        };

        let path = if wired.is_some() {
            ""
        } else {
            match ctx.params.get("path") {
                None | Some(ParamValue::Float(0.0)) => "",
                Some(ParamValue::String(path)) => path.as_str(),
                Some(_) => return self.fail_setup(ctx, "Fluid role path must be a string".into()),
            }
        };
        if compound.is_some() && path.is_empty() {
            return self.fail_setup(
                ctx,
                "Fluid role compound_materials requires an imported mesh path".into(),
            );
        }
        let mode = match resolve_index(ctx, "geometry", 0, GEOMETRY_MODES.len() as u32) {
            Ok(0) => GeometryMode::CollisionProxy,
            Ok(1) => GeometryMode::ClosedMesh,
            Ok(_) => unreachable!(),
            Err(error) => return self.fail_setup(ctx, error),
        };
        let shape = if wired.is_some() {
            1
        } else {
            match resolve_index(ctx, "shape", 1, PLATONIC_SHAPES.len() as u32) {
                Ok(value) => value,
                Err(error) => return self.fail_setup(ctx, error),
            }
        };
        let radius = if wired.is_some() {
            1.0
        } else {
            match resolve_f32(ctx, "radius", 1.0) {
                Ok(value) if value.is_finite() && value > 0.0 => value,
                Ok(_) => {
                    return self
                        .fail_setup(ctx, "Fluid role radius must be finite and positive".into());
                }
                Err(error) => return self.fail_setup(ctx, error),
            }
        };
        let selection_result = if wired.is_some() {
            resolve_collider_parts(ctx).map(default_selection)
        } else {
            resolve_selection(ctx)
        };
        let selection = match selection_result {
            Ok(selection) => selection,
            Err(error) => return self.fail_setup(ctx, error),
        };
        let setup_changed = self.last_key.as_ref().is_none_or(|last| {
            !last.matches(
                path,
                selection,
                (shape, radius),
                source_transform,
                mode,
                compound.as_ref(),
                wired.as_ref(),
            )
        });
        if setup_changed {
            self.last_key = Some(PreparationKey {
                path: path.to_owned(),
                selection,
                shape,
                radius,
                source_transform,
                mode,
                compound,
                wired: wired.as_ref().map(|wired| Box::new(wired.clone())),
            });
            self.geometry = None;
            self.preparation_error = None;
            self.pending_geometry = None;
            let (tx, rx) = mpsc::channel();
            let path_for_worker = PathBuf::from(path);
            match std::thread::Builder::new()
                .name("fluid-role-prepare".into())
                .spawn(move || {
                    let result = if let Some(wired) = wired {
                        prepare_wired_geometry(
                            &wired,
                            source_transform,
                            mode,
                            selection.collider_parts,
                        )
                    } else {
                        prepare_geometry(
                            &path_for_worker,
                            selection,
                            shape,
                            radius,
                            source_transform,
                            mode,
                            compound.as_ref(),
                        )
                    }
                    .map(|meshes| Arc::new(PreparedFluidGeometry::new(meshes)));
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
            // Offline waits for the preparation, so the liquid's first tick
            // never depends on how fast the worker ran.
            let received = if ctx.sim_step.offline() {
                rx.recv().map_err(|_| mpsc::TryRecvError::Disconnected)
            } else {
                rx.try_recv()
            };
            match received {
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

        self.publish_role(
            ctx,
            role,
            transform,
            enabled,
            velocity,
            inherit_motion,
            friction,
        );
    }
}

impl FluidRoleSource {
    fn publish_role(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        role: FluidRoleKind,
        transform: Transform,
        enabled: bool,
        velocity: [f32; 3],
        inherit_motion: f32,
        friction: f32,
    ) {
        if self.source_pending
            || self.pending_geometry.is_some()
            || self.geometry.is_none()
            || self.preparation_error.is_some()
        {
            if let Some(error) = &self.preparation_error {
                return self.fail(ctx, error.clone());
            }
            ctx.mark_outputs_pending();
            return;
        }
        ctx.outputs.set_cpu_value(
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

    fn fail(&mut self, ctx: &mut EffectNodeContext<'_, '_>, error: String) {
        ctx.error(error);
        ctx.mark_outputs_pending();
        if let Some(gpu) = ctx.gpu.as_deref_mut() {
            gpu.merge_frame_status(manifold_node_engine::runtime::frame_status::FrameRenderStatus::Failed(
                manifold_node_engine::runtime::frame_status::FrameRenderFailure::InvalidGeometry,
            ));
        }
    }

    fn fail_setup(&mut self, ctx: &mut EffectNodeContext<'_, '_>, error: String) {
        self.source_pending = false;
        self.last_key = None;
        self.pending_geometry = None;
        self.geometry = None;
        self.preparation_error = Some(error.clone());
        self.fail(ctx, error);
    }
}

fn resolve_wired_sources(
    ctx: &EffectNodeContext<'_, '_>,
) -> Result<(Option<WiredPreparation>, bool), String> {
    let mut wired = WiredPreparation {
        sources: std::array::from_fn(|_| None),
        part_transforms: [Transform::default(); 64],
    };
    let mut any = false;
    for (index, port) in MESH_PORTS.iter().enumerate() {
        let Some(slot) = ctx.inputs.slot(port) else {
            continue;
        };
        any = true;
        let source = ctx.inputs.mesh_source_slot(slot);
        if !ctx.inputs.slot_content_ready(slot) || source.is_none() {
            return Ok((None, true));
        }
        wired.sources[index] = source;
        if let Some(slot) = ctx.inputs.slot(PART_PORTS[index]) {
            let transform = ctx.inputs.transform(PART_PORTS[index]);
            if !ctx.inputs.slot_content_ready(slot) || transform.is_none() {
                return Ok((None, true));
            }
            wired.part_transforms[index] = transform.expect("checked above");
        }
        validate_transform(wired.part_transforms[index], "mesh part transform")?;
    }
    Ok((any.then_some(wired), false))
}

fn resolve_compound(
    ctx: &EffectNodeContext<'_, '_>,
) -> Result<Option<CompoundPreparation>, String> {
    if let Some(value) = ctx.params.get("compound_materials")
        && !matches!(value, ParamValue::Table(_) | ParamValue::Float(0.0))
    {
        return Err("Fluid role compound_materials must be a [slot, material_index] table".into());
    }
    let (materials, has_compound) = parse_compound_materials(ctx)?;
    let Some(mut compound) = has_compound.then_some(CompoundPreparation {
        materials,
        part_transforms: [Transform::default(); 64],
    }) else {
        return Ok(None);
    };
    for (slot, port) in PART_PORTS.iter().enumerate() {
        if compound.materials[slot].is_some_and(|material| material < -2) {
            return Err(format!(
                "compound part {slot}: material index must be >= -2"
            ));
        }
        compound.part_transforms[slot] = ctx.inputs.transform(port).unwrap_or_default();
        if compound.materials[slot].is_some() {
            validate_transform(compound.part_transforms[slot], "compound part")
                .map_err(|error| format!("compound part {slot}: {error}"))?;
        }
    }
    Ok(Some(compound))
}

manifold_core::testkit_visible! {
pub(crate) fn default_selection(collider_parts: u32) -> MeshSelection {
    MeshSelection {
        mesh: -1,
        primitive: -1,
        material: -1,
        fit: false,
        recenter: true,
        translate: [0.0; 3],
        fragment_count: 1,
        fragment_index: 0,
        collider_parts,
    }
}
}

fn resolve_collider_parts(ctx: &EffectNodeContext<'_, '_>) -> Result<u32, String> {
    let parts = resolve_integer(ctx, "collider_parts", 32)?;
    if !(1..=64).contains(&parts) {
        return Err("Fluid role collider_parts must be in 1..=64".into());
    }
    Ok(parts as u32)
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
    use manifold_node_engine::exec::backend::Backend;
    use manifold_node_engine::bindings::Slot;
    use manifold_node_engine::exec::effect_node::ParamValues;
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::parameters::TableData;
    use manifold_node_engine::ports::PortType;
    use manifold_node_engine::primitive::PrimitiveSpec;
    use manifold_node_engine::exec::backend::MockBackend;
    use crate::testkit::fluid_role_source::{run_inputs, test_slots, settle_inputs};

    fn run_once(
        primitive: &mut FluidRoleSource,
        backend: &mut MockBackend,
        transform_slot: Slot,
        output_slot: Slot,
        params: &ParamValues,
    ) -> bool {
        let input_bindings: &[(&'static str, Slot)] = &[("transform", transform_slot)];
        run_inputs(primitive, backend, input_bindings, output_slot, params)
    }

    fn settle(
        primitive: &mut FluidRoleSource,
        backend: &mut MockBackend,
        transform_slot: Slot,
        output_slot: Slot,
        params: &ParamValues,
    ) -> FluidRole {
        settle_inputs(
            primitive,
            backend,
            &[("transform", transform_slot)],
            output_slot,
            params,
        )
    }

    #[test]
    fn scene_physics_wired_mesh_edits_reprepare_and_history_holds_geometry() {
        let mut backend = MockBackend::new();
        let (transform, output) = test_slots(&mut backend);
        let mesh = backend.acquire(ResourceId(2), PortType::MeshSource, None, (0, 0));
        let missing = backend.acquire(ResourceId(3), PortType::MeshSource, None, (0, 0));
        backend.set_mesh_source(mesh, MeshSource::Cube { size: 1.0 });
        let inputs = [("transform", transform), ("mesh_0", mesh)];
        let mut primitive = FluidRoleSource::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("geometry"), ParamValue::Enum(1));
        // Connected geometry shadows legacy source fields completely.
        params.insert(Cow::Borrowed("path"), ParamValue::Float(42.0));
        params.insert(Cow::Borrowed("radius"), ParamValue::Float(-1.0));
        params.insert(Cow::Borrowed("compound_materials"), ParamValue::Bool(false));
        let first = settle_inputs(&mut primitive, &mut backend, &inputs, output, &params);
        let extent = |role: &FluidRole| {
            role.geometry.meshes[0]
                .vertices
                .iter()
                .map(|point| point[0].abs())
                .fold(0.0_f32, f32::max)
        };
        assert!((extent(&first) - 0.5).abs() < 1.0e-6);

        backend.set_mesh_source(mesh, MeshSource::Cube { size: 2.0 });
        let larger = settle_inputs(&mut primitive, &mut backend, &inputs, output, &params);
        assert!(!Arc::ptr_eq(&first.geometry, &larger.geometry));
        assert!((extent(&larger) - 1.0).abs() < 1.0e-6);

        params.insert(Cow::Borrowed("velocity_y"), ParamValue::Float(3.0));
        backend.set_transform(
            transform,
            Transform {
                pos: [2.0, 0.0, 0.0],
                ..Transform::default()
            },
        );
        backend.set_mesh_source(mesh, MeshSource::Cube { size: 3.0 });
        {
            // Source slots may be unbound in the CPU-only historical pass.
            let historical = [("transform", transform), ("mesh_0", missing)];
            assert!(!crate::testkit::fluid_role_source::run_inputs_under(
                &mut primitive,
                &mut backend,
                &historical,
                output,
                &params,
                crate::physics::SimStep::default().authored_sample(),
            ));
            let sampled = backend.cpu_values().get::<FluidRole>(output).unwrap();
            assert!(Arc::ptr_eq(&sampled.geometry, &larger.geometry));
            assert_eq!(sampled.velocity[1], 3.0);
            assert_eq!(sampled.transform.pos[0], 2.0);
        }
        let newest = settle_inputs(&mut primitive, &mut backend, &inputs, output, &params);
        assert!((extent(&newest) - 1.5).abs() < 1.0e-6);
        assert!(!Arc::ptr_eq(&newest.geometry, &larger.geometry));

        let missing_inputs = [("transform", transform), ("mesh_0", missing)];
        assert!(run_inputs(
            &mut primitive,
            &mut backend,
            &missing_inputs,
            output,
            &params
        ));
        assert!(Primitive::warmup_pending(&primitive));
        assert!(primitive.geometry.is_none());
        let restored = settle_inputs(&mut primitive, &mut backend, &inputs, output, &params);
        assert!((extent(&restored) - 1.5).abs() < 1.0e-6);
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
            .cpu_values()
            .get::<FluidRole>(output_slot)
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
        let after_revision = backend.cpu_values().get::<FluidRole>(output_slot);
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

    #[test]
    fn scene_physics_fluid_role_source_rejects_wrong_or_duplicate_compound_materials() {
        let mut backend = MockBackend::new();
        let (transform_slot, output_slot) = test_slots(&mut backend);
        let mut primitive = FluidRoleSource::new();

        let mut wrong_type = ParamValues::default();
        wrong_type.insert(Cow::Borrowed("compound_materials"), ParamValue::Bool(true));
        assert!(run_once(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &wrong_type
        ));
        assert!(primitive.last_key.is_none());

        let table = TableData::new(vec![vec![0.0, 0.0], vec![0.0, 1.0]]).unwrap();
        let mut duplicate = ParamValues::default();
        duplicate.insert(
            Cow::Borrowed("compound_materials"),
            ParamValue::Table(Arc::new(table)),
        );
        assert!(run_once(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &duplicate
        ));
        assert!(primitive.last_key.is_none());

        let table = TableData::new(vec![vec![0.0, 0.0]]).unwrap();
        let mut missing_path = ParamValues::default();
        missing_path.insert(
            Cow::Borrowed("compound_materials"),
            ParamValue::Table(Arc::new(table)),
        );
        assert!(run_once(
            &mut primitive,
            &mut backend,
            transform_slot,
            output_slot,
            &missing_path
        ));
        assert!(primitive.last_key.is_none());
    }

}
