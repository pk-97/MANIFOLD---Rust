//! Native FLIP/APIC simulation is one FFI boundary; its mesh remains a
//! composable input to the existing material and scene rendering graph.
use std::borrow::Cow;

use crate::runtime::frame_status::{FrameRenderFailure, FrameRenderStatus};
use crate::mesh::{InstanceTransform, MeshVertex};
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::scene::fluid_domain::{FluidDomainSnapshot, FluidDomainState};
use crate::water::fluid::{CoupledRigidInputs, FluidControls, FluidDomainNative, FluidRuntime, FluidSettings};
use crate::water::fluid_cache::CacheMode;
use crate::water::fluid_mesh_upload::FluidMeshUpload;
use crate::water::fluid::particle_ring::SlotFrame;
use crate::particles::FluidParticle;
use crate::scene::fluid_domain::MAX_FLUID_ROLES;
use crate::water::fluid_role::{FluidRole};
use crate::exec::instance_upload::InstanceSnapshotUpload;
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::scene::impulse::RigidImpulseTargets;
use crate::water::physics::RigidSceneObservation;
use crate::water::physics_events::ResolvedNodeImpulse;
use crate::water::node::{PhysicsNode, PhysicsNodeRegistration};
use crate::primitive::Primitive;
use manifold_fluids::{LiquidOptions, SurfaceOptions, WhitewaterOptions};
use manifold_physics::FieldValue;

/// Particle-frame outputs (GPU_FLUID_SURFACE_DESIGN.md section 3.2). Any of
/// them wired switches the node into publishing particle frames.
const PARTICLE_ARRAY_PORTS: [&str; 4] = ["particles_a", "particles_b", "solid_a", "solid_b"];

/// Valid empty storage before the native solver accepts its first tick. Kept
/// outside the worker ring: it is immutable while a display frame reads it.
pub struct EmptyParticleFrame {
    particles: manifold_gpu::GpuBuffer,
    solid: manifold_gpu::GpuBuffer,
    nodes: [u32; 3],
}

impl EmptyParticleFrame {
    fn new(device: &manifold_gpu::GpuDevice, nodes: [u32; 3]) -> Result<Self, String> {
        let particle_bytes = std::mem::size_of::<FluidParticle>() as u64;
        let solid_bytes = nodes.into_iter().map(u64::from).product::<u64>() * 4;
        crate::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(), particle_bytes + solid_bytes,
        ).map_err(|error| error.to_string())?;
        let particles = device.try_create_buffer_shared(particle_bytes).map_err(|error| error.to_string())?;
        let solid = device.try_create_buffer_shared(solid_bytes).map_err(|error| error.to_string())?;
        // Fresh storage, not yet published. Zero live particles imply an
        // exterior level set everywhere, regardless of the zero solid field.
        particles.zero_fill();
        solid.zero_fill();
        Ok(Self { particles, solid, nodes })
    }
}

const ROLE_PORTS: [&str; MAX_FLUID_ROLES] = [
    "role_0", "role_1", "role_2", "role_3", "role_4", "role_5", "role_6", "role_7", "role_8",
    "role_9", "role_10", "role_11", "role_12", "role_13", "role_14", "role_15", "role_16",
    "role_17", "role_18", "role_19", "role_20", "role_21", "role_22", "role_23", "role_24",
    "role_25", "role_26", "role_27", "role_28", "role_29", "role_30", "role_31", "role_32",
    "role_33", "role_34", "role_35", "role_36", "role_37", "role_38", "role_39", "role_40",
    "role_41", "role_42", "role_43", "role_44", "role_45", "role_46", "role_47", "role_48",
    "role_49", "role_50", "role_51", "role_52", "role_53", "role_54", "role_55", "role_56",
    "role_57", "role_58", "role_59", "role_60", "role_61", "role_62", "role_63",
];

crate::primitive! {
    name: FluidSurface,
    type_id: "node.fluid_surface",
    purpose: "Simulate a rectangular liquid domain with the native FLIP Fluids CPU engine and output its reconstructed surface. Connect FluidRole inputs for mesh fills, inflows, drains and colliders, with retained live motion controls. Legacy box Transform inputs remain supported.",
    inputs: {
        role_0: FluidRole optional,
        role_1: FluidRole optional,
        role_2: FluidRole optional,
        role_3: FluidRole optional,
        role_4: FluidRole optional,
        role_5: FluidRole optional,
        role_6: FluidRole optional,
        role_7: FluidRole optional,
        role_8: FluidRole optional,
        role_9: FluidRole optional,
        role_10: FluidRole optional,
        role_11: FluidRole optional,
        role_12: FluidRole optional,
        role_13: FluidRole optional,
        role_14: FluidRole optional,
        role_15: FluidRole optional,
        role_16: FluidRole optional,
        role_17: FluidRole optional,
        role_18: FluidRole optional,
        role_19: FluidRole optional,
        role_20: FluidRole optional,
        role_21: FluidRole optional,
        role_22: FluidRole optional,
        role_23: FluidRole optional,
        role_24: FluidRole optional,
        role_25: FluidRole optional,
        role_26: FluidRole optional,
        role_27: FluidRole optional,
        role_28: FluidRole optional,
        role_29: FluidRole optional,
        role_30: FluidRole optional,
        role_31: FluidRole optional,
        role_32: FluidRole optional,
        role_33: FluidRole optional,
        role_34: FluidRole optional,
        role_35: FluidRole optional,
        role_36: FluidRole optional,
        role_37: FluidRole optional,
        role_38: FluidRole optional,
        role_39: FluidRole optional,
        role_40: FluidRole optional,
        role_41: FluidRole optional,
        role_42: FluidRole optional,
        role_43: FluidRole optional,
        role_44: FluidRole optional,
        role_45: FluidRole optional,
        role_46: FluidRole optional,
        role_47: FluidRole optional,
        role_48: FluidRole optional,
        role_49: FluidRole optional,
        role_50: FluidRole optional,
        role_51: FluidRole optional,
        role_52: FluidRole optional,
        role_53: FluidRole optional,
        role_54: FluidRole optional,
        role_55: FluidRole optional,
        role_56: FluidRole optional,
        role_57: FluidRole optional,
        role_58: FluidRole optional,
        role_59: FluidRole optional,
        role_60: FluidRole optional,
        role_61: FluidRole optional,
        role_62: FluidRole optional,
        role_63: FluidRole optional,
        acceleration_field: VectorField optional,
        domain: Transform optional, emitter: Transform optional, obstacle: Transform optional, initial_volume: Transform optional,
        resolution: ScalarF32 optional, domain_size: ScalarF32 optional, fill_height: ScalarF32 optional,
        viscosity: ScalarF32 optional, surface_tension: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        emission: ScalarF32 optional, inflow_speed: ScalarF32 optional,
        speed: ScalarF32 optional, reset: ScalarF32 optional, surface_subdivisions: ScalarF32 optional,
        liquid_density: ScalarF32 optional,
        surface_particle_scale: ScalarF32 optional, surface_smoothing: ScalarF32 optional,
        surface_smoothing_iterations: ScalarF32 optional, whitewater: ScalarF32 optional,
        whitewater_wavecrest_rate: ScalarF32 optional, whitewater_turbulence_rate: ScalarF32 optional,
        whitewater_min_energy: ScalarF32 optional, whitewater_max_energy: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex), obstacle_pose: Transform,
        foam: Array(InstanceTransform), bubbles: Array(InstanceTransform), spray: Array(InstanceTransform),
        foam_count: ScalarF32, bubble_count: ScalarF32, spray_count: ScalarF32,
        simulation_time: ScalarF32, lag_seconds: ScalarF32, simulation_ms: ScalarF32,
        meshing_ms: ScalarF32, particle_count: ScalarF32, vertex_count: ScalarF32,
        upload_ms: ScalarF32,
        particles_a: Array(FluidParticle), particles_b: Array(FluidParticle),
        count_a: ScalarF32, count_b: ScalarF32, identity_a: ScalarF32, identity_b: ScalarF32,
        solid_a: Array(f32), solid_b: Array(f32), grid_bounds: Transform,
        grid_nodes_x: ScalarF32, grid_nodes_y: ScalarF32, grid_nodes_z: ScalarF32,
        blend: ScalarF32, span: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("seed"), label: "Seed", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16777215.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("resolution"), label: "Resolution", ty: ParamType::Int, default: ParamValue::Float(24.0), range: Some((8.0, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("grid_budget_mcells"), label: "Grid Budget (M cells)", ty: ParamType::Float, default: ParamValue::Float(8.0), range: Some((0.01, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_x"), label: "Closed −X", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_x"), label: "Closed +X", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_y"), label: "Closed Bottom", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_y"), label: "Closed Top", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_z"), label: "Closed −Z", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_z"), label: "Closed +Z", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Initial Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("liquid_density"), label: "Liquid Density", ty: ParamType::Float, default: ParamValue::Float(1000.0), range: Some((1.0, 5000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("viscosity"), label: "Viscosity", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("surface_tension"), label: "Surface Tension", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("emission"), label: "Pour", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &["Off", "On"] },
        ParamDef { name: Cow::Borrowed("inflow_speed"), label: "Flow Speed", ty: ParamType::Float, default: ParamValue::Float(1.5), range: Some((0.0, 5.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("speed"), label: "Simulation Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("surface_subdivisions"), label: "Surface Detail", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("surface_particle_scale"), label: "Surface Particle Scale", ty: ParamType::Float, default: ParamValue::Float(3.0), range: Some((0.25, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("surface_smoothing"), label: "Surface Smoothing", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("surface_smoothing_iterations"), label: "Smoothing Iterations", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((0.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("whitewater"), label: "Whitewater", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &["Off", "On"] },
        ParamDef { name: Cow::Borrowed("whitewater_wavecrest_rate"), label: "Wavecrest Emission", ty: ParamType::Float, default: ParamValue::Float(175.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("whitewater_turbulence_rate"), label: "Turbulence Emission", ty: ParamType::Float, default: ParamValue::Float(175.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("whitewater_min_energy"), label: "Whitewater Min Energy", ty: ParamType::Float, default: ParamValue::Float(0.1), range: Some((0.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("whitewater_max_energy"), label: "Whitewater Max Energy", ty: ParamType::Float, default: ParamValue::Float(60.0), range: Some((0.01, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("whitewater_capacity"), label: "Whitewater Capacity", ty: ParamType::Int, default: ParamValue::Float(100000.0), range: Some((1.0, 250000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("transfer"), label: "Transfer", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: &["FLIP", "APIC"] },
        ParamDef { name: Cow::Borrowed("max_capacity"), label: "Initial Mesh Allocation", ty: ParamType::Int, default: ParamValue::Float(786432.0), range: Some((3.0, 3145728.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cache_mode"), label: "Cache Mode", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 2.0)), enum_values: &["Live", "Record", "Playback"] },
        ParamDef { name: Cow::Borrowed("cache_path"), label: "Cache Path", ty: ParamType::String, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "CPU reference engine, not a real-time guarantee. The optional domain Transform sets the axis-aligned container centre and full XYZ dimensions in metres. Bounds round outward around that centre to uniform cells. Domain rotation and billboarding are rejected. Without that input, Domain Size preserves the cube centred in X/Z with floor Y=0. Closed face toggles control all six boundaries; domain and boundary edits restart the simulation. FluidRole inputs accept prepared closed meshes or explicit collision proxies with live translation/rotation; geometry, role and scale edits restart the world. The optional acceleration_field is a scene-space vector field in metres per second squared, shared with Physics World. It is retained and sampled at fixed ticks; live changes do not rebuild the simulation. Mesh-role and vector-field graphs currently require Live mode pending complete cache and input-take identity support. Legacy emitter/obstacle/initial_volume transforms describe axis-aligned boxes using full dimensions; rotations and billboards are rejected, and initial_volume must be fully contained in the domain. The optional initial_volume seeds a localized zero-velocity column in addition to the fill_height pool. Domain size, resolution, fill, initial volume, transfer, density and surface detail changes restart the simulation. In a coupled Physics World pair, the rigid world owns playback speed and both reset controls restart the same native pair; the liquid density is physical kg/m3 and is passed to the shared worker. Coupled Physics World support uses the shared Live worker; non-Live cache modes reject coupled scenes until paired input takes are supported. Native state lives on a background worker. Preview retains time debt and displays the latest complete mesh; export drains the same fixed 60 Hz ticks. Historical controls use the existing 240 Hz stateless physics ancestry sampler. Reset and backwards transport start a fresh simulation. Wire obstacle_pose to the visible unit-cube collider to avoid showing it ahead of the fluid. Overflow is a visible error, never a truncated mesh. Native whitewater is optional and defaults off. Its foam, bubbles and spray outputs are instance transforms at the same accepted tick as the mesh; wire each matching count to scene_object.instance_count and author particle meshes/materials separately. Particle scale and smoothing affect surface reconstruction, not solver dynamics. Liquid, surface and whitewater settings restart the world. Viscosity and surface tension use scale-dependent native coefficients, not calibrated material units. Surface-tension validation includes the 64-cubed dam-break regression; the honey reference uses zero tension. Whitewater capacity bounds native emission; the three output arrays each reserve that capacity. Particle instances shrink during their last 0.2 seconds. Mesh output uses the engine mesher; material and rendering stay separate graph nodes. cache_mode is Live, Record or Playback and cache_path names a compressed fixed-60-Hz geometry snapshot stream. Record publishes atomically; Playback uses baked geometry, whitewater, obstacle pose and stats exactly and does not run the solver. Playback requires every requested tick and never silently falls back to Live. The physical settings and fixed tick are part of the cache manifest.",
    examples: ["WaterBasin", "WaterDamBreak", "HoneyDamBreak"],
    summary: "Simulate liquid and generate its surface. Connect optional sources and colliders to control its motion.",
    category: Geometry3D, role: Source,
    aliases: ["water", "liquid", "fluid", "FLIP", "APIC"],
    boundary_reason: IoBridge,
    extra_fields: {
        surface_buffer: Option<manifold_gpu::GpuBuffer> = None,
        empty_particle_frame: Option<EmptyParticleFrame> = None,
        runtime: FluidRuntime = FluidRuntime::default(),
        upload: FluidMeshUpload = FluidMeshUpload::default(),
        foam_upload: InstanceSnapshotUpload = InstanceSnapshotUpload::default(),
        bubble_upload: InstanceSnapshotUpload = InstanceSnapshotUpload::default(),
        spray_upload: InstanceSnapshotUpload = InstanceSnapshotUpload::default(),
        last_version: u64 = u64::MAX,
        last_lag: u32 = u32::MAX,
        last_particle_version: u64 = u64::MAX,
        last_blend: u32 = u32::MAX,
        role_pending: bool = false,
        domain_failure: bool = false,
        coupled_mode: bool = false,
        coupled_observation: Option<RigidSceneObservation> = None,
        coupled_colliders: RigidImpulseTargets = RigidImpulseTargets::default(),
        coupled_error: Option<String> = None,
        coupled_previous_reset: Option<f32> = None,
    },
}

impl PhysicsNode for FluidSurface {
    fn set_physics_source_identity(&mut self, identity: Result<[u8; 32], String>) {
        self.runtime.set_source_identity(identity);
    }

    fn set_physics_project_tempo(&mut self, tempo: Option<&crate::runtime::preset_context::ProjectTempo>) {
        self.runtime.set_project_tempo(tempo);
    }

    fn set_coupled_physics(&mut self, enabled: bool) {
        if self.coupled_mode == enabled {
            return;
        }
        self.coupled_mode = enabled;
        self.runtime.clear();
        self.empty_particle_frame = None;
        self.coupled_observation = None;
        self.coupled_error = None;
        self.coupled_previous_reset = None;
    }

    fn set_coupled_rigid_inputs(
        &mut self,
        observation: Option<&RigidSceneObservation>,
        colliders: RigidImpulseTargets,
        error: Option<&str>,
    ) {
        self.set_coupled_physics(true);
        self.coupled_observation = observation.cloned();
        self.coupled_colliders = colliders;
        self.coupled_error = error.map(str::to_owned);
    }

    fn coupled_rigid_frame(&self) -> Option<&crate::water::fluid::CoupledRigidFrame> {
        if !self.coupled_mode || self.role_pending || self.domain_failure {
            return None;
        }
        if self.coupled_observation.is_none() || self.coupled_error.is_some() {
            return None;
        }
        self.runtime.coupled_rigid_frame()
    }

    fn physics_impulse_epoch(&self) -> Option<u64> {
        self.runtime.impulse_epoch()
    }

    fn physics_impulse_stamp(
        &self,
        transport: manifold_core::Seconds,
        sequence: u64,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        if self.role_pending {
            return Err("Liquid Surface: cannot capture an impulse while inputs are pending".into());
        }
        if self.domain_failure {
            return Err("Liquid Surface: cannot capture an impulse after a domain failure".into());
        }
        self.runtime.impulse_stamp(transport, sequence)
    }

    fn enqueue_physics_impulse(
        &mut self,
        stamp: manifold_physics::input::EventStamp,
        impulse: ResolvedNodeImpulse,
    ) -> Result<manifold_physics::TickStamp, String> {
        self.runtime.enqueue_scene_impulse(stamp, impulse)
    }

    fn drain_physics_impulses(
        &mut self,
        consume: &mut dyn FnMut(
            manifold_physics::input::AppliedEvent<ResolvedNodeImpulse>,
        ),
    ) {
        for event in self.runtime.drain_scene_impulses() {
            consume(event);
        }
    }

    fn fluid_domain_snapshot(&self) -> Option<FluidDomainSnapshot> {
        let mut snapshot = self.runtime.domain_snapshot();
        if self.role_pending {
            snapshot.state = FluidDomainState::PendingInputs;
            snapshot.accepted_layout = None;
        } else if self.domain_failure {
            snapshot.state = FluidDomainState::Failed;
            snapshot.accepted_layout = None;
        }
        Some(snapshot)
    }
}

inventory::submit! { PhysicsNodeRegistration::new::<FluidSurface>() }

impl FluidSurface {
    fn report_failure(
        domain_failure: &mut bool,
        ctx: &mut EffectNodeContext<'_, '_>,
        error: String,
    ) {
        *domain_failure = true;
        ctx.error(error);
        ctx.mark_outputs_pending();
        if let Some(gpu) = ctx.gpu.as_deref_mut() {
            gpu.merge_frame_status(FrameRenderStatus::Failed(FrameRenderFailure::Simulation));
        }
    }
}

impl Primitive for FluidSurface {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "vertices" || PARTICLE_ARRAY_PORTS.contains(&port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&manifold_gpu::GpuBuffer> {
        if port == "vertices" {
            return self.surface_buffer.as_ref();
        }
        // The worker writes ring slots directly; A/B are published as is.
        let Some((a, b)) = self.runtime.particles.pair() else {
            let empty = self.empty_particle_frame.as_ref()?;
            return match port {
                "particles_a" | "particles_b" => Some(&empty.particles),
                "solid_a" | "solid_b" => Some(&empty.solid),
                _ => None,
            };
        };
        match port {
            "particles_a" => Some(a.particles()),
            "particles_b" => Some(b.particles()),
            "solid_a" => Some(a.solid()),
            "solid_b" => Some(b.solid()),
            _ => None,
        }
    }

    fn clear_state(&mut self) {
        self.runtime.clear();
        self.empty_particle_frame = None;
        self.role_pending = false;
        self.domain_failure = false;
        self.coupled_observation = None;
        self.coupled_error = None;
        self.coupled_previous_reset = None;
    }
    fn warmup_pending(&self) -> bool {
        !self.domain_failure && (self.role_pending || self.runtime.warmup_pending())
    }
    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        if matches!(port, "foam" | "bubbles" | "spray") {
            let value = match params.get("whitewater_capacity") {
                Some(ParamValue::Float(n)) => *n,
                _ => 100000.0,
            };
            return Some(value.clamp(1.0, 250000.0).round() as u32);
        }
        if PARTICLE_ARRAY_PORTS.contains(&port) {
            // The ring provides storage sized by the solver; downstream
            // arrays re-derive capacity when a provided array grows.
            return Some(1);
        }
        if port != "vertices" {
            return None;
        }
        let value = match params.get("max_capacity") {
            Some(ParamValue::Float(n)) => *n,
            _ => 786432.0,
        };
        Some((value.clamp(3.0, 3145728.0) as u32 / 3) * 3)
    }
    // While an input is pending the simulation clock holds instead of
    // jumping when the input lands.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.domain_failure = false;
        self.role_pending = false;
        let coupled_observation = if self.coupled_mode {
            if let Some(error) = self.coupled_error.clone() {
                Self::report_failure(&mut self.domain_failure, ctx, error);
                return;
            }
            let Some(observation) = self.coupled_observation.as_ref() else {
                self.role_pending = true;
                self.runtime.hold_pending(ctx.time.seconds);
                ctx.mark_outputs_pending();
                return;
            };
            Some(observation)
        } else {
            None
        };
        self.role_pending |= ctx.inputs.any_pending();
        let mut roles = std::array::from_fn::<_, MAX_FLUID_ROLES, _>(|_| None);
        for (index, port) in ROLE_PORTS.iter().enumerate() {
            if ctx.inputs.slot(port).is_some() {
                roles[index] = ctx.inputs.cpu_value::<FluidRole>(port);
                self.role_pending |= roles[index].is_none();
            }
        }
        if ctx.inputs.slot("domain").is_some() {
            self.role_pending |= ctx.inputs.transform("domain").is_none();
        }
        let acceleration_field = ctx.inputs.cpu_value::<FieldValue>("acceleration_field");
        if ctx.inputs.slot("acceleration_field").is_some() {
            self.role_pending |= acceleration_field.is_none();
        }
        if self.role_pending {
            self.runtime.hold_pending(ctx.time.seconds);
            ctx.mark_outputs_pending();
            return;
        }
        for (name, fallback) in [
            ("resolution", 24.0),
            ("viscosity", 0.0),
            ("surface_tension", 0.0),
            ("surface_subdivisions", 0.0),
            ("emission", 1.0),
            ("surface_smoothing_iterations", 2.0),
            ("whitewater", 0.0),
            ("whitewater_capacity", 100000.0),
            ("liquid_density", 1000.0),
        ] {
            if !ctx.scalar_or_param(name, fallback).is_finite() {
                Self::report_failure(
                    &mut self.domain_failure,
                    ctx,
                    format!("Water: {name} must be finite"),
                );
                return;
            }
        }
        if !(0.0..=2.0).contains(&ctx.scalar_or_param("surface_subdivisions", 0.0)) {
            Self::report_failure(
                &mut self.domain_failure,
                ctx,
                "Water: surface detail must be between 0 and 2".into(),
            );
            return;
        }
        let liquid_density = ctx.scalar_or_param("liquid_density", 1000.0);
        if !liquid_density.is_finite() || liquid_density <= 0.0 {
            Self::report_failure(
                &mut self.domain_failure,
                ctx,
                "Water: liquid density must be finite and positive".into(),
            );
            return;
        }
        if !(0.0..=10.0).contains(&ctx.scalar_or_param("surface_smoothing_iterations", 2.0))
            || !(1.0..=250000.0).contains(&ctx.param_f32("whitewater_capacity", 100000.0))
        {
            Self::report_failure(
                &mut self.domain_failure,
                ctx,
                "Water: invalid surface smoothing iterations or whitewater capacity".into(),
            );
            return;
        }
        let boundary_collisions = match boundary_collisions(ctx.params) {
            Ok(faces) => faces,
            Err(error) => {
                Self::report_failure(&mut self.domain_failure, ctx, error);
                return;
            }
        };
        let seed = ctx.param_f32("seed", 0.0);
        if !seed.is_finite() || !(0.0..=16_777_215.0).contains(&seed) || seed.fract() != 0.0 {
            Self::report_failure(
                &mut self.domain_failure,
                ctx,
                "Water: seed must be an integer between 0 and 16777215".into(),
            );
            return;
        }
        let settings = fluid_settings(
            |name, default| ctx.scalar_or_param(name, default),
            ctx.params,
            ctx.inputs.transform("domain"),
            ctx.inputs.transform("initial_volume"),
            boundary_collisions,
            seed as u64,
        );
        if let Ok(layout) = settings.domain_layout()
            && let Err(error) = layout.admit_flip_grid(ctx.param_f32("grid_budget_mcells", 8.0))
        {
            Self::report_failure(&mut self.domain_failure, ctx, error);
            return;
        }
        let cache_mode = match ctx.params.get("cache_mode") {
            Some(ParamValue::Enum(value)) => CacheMode::from_enum(*value),
            None => Some(CacheMode::Live),
            _ => None,
        };
        let Some(cache_mode) = cache_mode else {
            Self::report_failure(
                &mut self.domain_failure,
                ctx,
                "Water: cache mode must be Live, Record or Playback".into(),
            );
            return;
        };
        let cache_path = match ctx.params.get("cache_path") {
            Some(ParamValue::String(path)) => path.as_str(),
            Some(ParamValue::Float(_)) | None => "",
            _ => {
                Self::report_failure(
                    &mut self.domain_failure,
                    ctx,
                    "Water: cache path must be a String".into(),
                );
                return;
            }
        };
        if let Err(error) = self.runtime.set_cache(cache_mode, cache_path) {
            Self::report_failure(&mut self.domain_failure, ctx, error);
            return;
        }
        let particle_outputs = PARTICLE_ARRAY_PORTS
            .iter()
            .any(|port| ctx.outputs.slot(port).is_some());
        if particle_outputs && cache_mode != CacheMode::Live {
            Self::report_failure(
                &mut self.domain_failure,
                ctx,
                "Fluid particle outputs require Live mode until particle frames are recorded in the cache manifest".into(),
            );
            return;
        }
        self.runtime
            .set_outputs(particle_outputs, ctx.outputs.slot("vertices").is_some());
        let defaults = FluidControls::default();
        let emitter = ctx.inputs.transform("emitter");
        let obstacle = ctx.inputs.transform("obstacle");
        let controls = FluidControls {
            emitter: emitter.unwrap_or(defaults.emitter),
            obstacle: obstacle.unwrap_or(defaults.obstacle),
            obstacle_enabled: obstacle.is_some(),
            gravity: [
                ctx.scalar_or_param("gravity_x", 0.0),
                ctx.scalar_or_param("gravity", -9.81),
                ctx.scalar_or_param("gravity_z", 0.0),
            ],
            emission: emitter.is_some() && ctx.scalar_or_param("emission", 1.0) > 0.5,
            inflow_speed: ctx.scalar_or_param("inflow_speed", 1.5),
        };
        let authored_only = crate::water::physics::authored_sample_only();
        let fluid_speed = ctx.scalar_or_param("speed", 1.0);
        let fluid_reset = ctx.scalar_or_param("reset", 0.0);
        let (transport, speed, rigid_reset_edge) = if let Some(observation) = coupled_observation {
            if !fluid_speed.is_finite() || fluid_speed != observation.speed {
                Self::report_failure(
                    &mut self.domain_failure,
                    ctx,
                    format!(
                        "Fluid coupling: liquid and rigid bodies require the same Simulation Speed and shared World control (liquid {fluid_speed}, rigid {})",
                        observation.speed
                    ),
                );
                return;
            }
            if (observation.transport.0 - ctx.time.seconds.0).abs() > 1e-9 {
                Self::report_failure(
                    &mut self.domain_failure,
                    ctx,
                    "Fluid coupling: rigid observation transport does not match liquid transport"
                        .into(),
                );
                return;
            }
            let reset_changed = self
                .coupled_previous_reset
                .is_some_and(|previous| previous != observation.reset);
            if authored_only && reset_changed {
                self.role_pending = true;
                self.runtime.hold_pending(ctx.time.seconds);
                ctx.mark_outputs_pending();
                return;
            }
            (observation.transport, observation.speed, reset_changed)
        } else {
            (ctx.time.seconds, fluid_speed, false)
        };
        if rigid_reset_edge && !authored_only {
            self.runtime.request_reset();
        }
        let result = self.runtime.observe_coupled_frame(
            settings,
            controls,
            &roles,
            acceleration_field,
            coupled_observation.map(|observation| CoupledRigidInputs {
                scene: &observation.inputs,
                colliders: self.coupled_colliders,
                density: f64::from(liquid_density),
            }),
            crate::exec::effect_node::FrameTime {
                seconds: transport,
                ..ctx.time
            },
            speed,
            fluid_reset,
        );
        if let Err(error) = result {
            Self::report_failure(&mut self.domain_failure, ctx, error);
            return;
        }
        let history_drain = crate::water::physics::history_drain_requested();
        if crate::water::physics::authored_sample_only() && !history_drain {
            return;
        }
        let blocking = crate::water::physics::offline_simulation();
        // Offline, an owed particle capture (slot growth) is served in the
        // same frame: prepare storage, then let the worker capture the tick.
        for _ in 0..3 {
            if particle_outputs
                && let Some(gpu) = ctx.gpu.as_deref()
                && let Err(error) = self.runtime.prepare_particles(gpu.device)
            {
                Self::report_failure(&mut self.domain_failure, ctx, error);
                return;
            }
            if let Err(error) = self.runtime.advance(blocking) {
                Self::report_failure(&mut self.domain_failure, ctx, error);
                return;
            }
            if !(blocking && ctx.gpu.is_some() && self.runtime.particle_capture_pending()) {
                break;
            }
        }
        if crate::water::physics::authored_sample_only() {
            return;
        }
        if let Some(observation) = coupled_observation {
            self.coupled_previous_reset = Some(observation.reset);
        }
        let lag = self.runtime.lag_seconds() as f32;
        ctx.outputs
            .set_transform("obstacle_pose", self.runtime.obstacle);
        for (name, value) in [
            ("simulation_time", self.runtime.simulation_time() as f32),
            ("lag_seconds", lag),
            ("simulation_ms", self.runtime.stats.simulation_ms as f32),
            ("meshing_ms", self.runtime.stats.meshing_ms as f32),
            ("particle_count", self.runtime.stats.particles as f32),
            ("vertex_count", self.runtime.vertices.len() as f32),
            ("foam_count", self.runtime.whitewater.foam.len() as f32),
            ("bubble_count", self.runtime.whitewater.bubbles.len() as f32),
            ("spray_count", self.runtime.whitewater.spray.len() as f32),
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        let (blend, span) = self.runtime.particle_blend();
        if particle_outputs {
            self.publish_particle_frame(ctx, blend, span);
        }
        let retained = ctx.outputs_retained();
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return;
        };
        // CPU encode cost only; the GPU side of the copy lands in the frame's GPU time.
        let upload_started = std::time::Instant::now();
        let mut uploaded = false;
        if let Some(dst) = ctx.outputs.array("vertices") {
            let required = self.runtime.vertices.len() as u64 * std::mem::size_of::<MeshVertex>() as u64;
            if required > dst.size {
                // Grow only when needed. The current snapshot and solver epoch
                // survive resource replacement, including during offline export.
                let stride = std::mem::size_of::<MeshVertex>() as u64 * 3;
                let bytes = required.max(dst.size.saturating_mul(3) / 2).div_ceil(stride) * stride;
                let candidate = crate::load::expand::admit_candidate_bytes(
                    gpu.device.modifier_memory_snapshot(), bytes,
                ).map_err(|error| error.to_string()).and_then(|()| {
                    gpu.device.try_create_buffer_shared(bytes)
                });
                match candidate {
                    Ok(buffer) => self.surface_buffer = Some(buffer),
                    Err(error) => {
                        Self::report_failure(&mut self.domain_failure, ctx,
                            format!("Fluid surface needs {bytes} bytes of GPU storage: {error}"));
                        return;
                    }
                }
            } else {
                self.surface_buffer = Some(dst.clone());
            }
            let dst = self.surface_buffer.as_ref().expect("surface storage prepared");
            match self.upload.upload(
                gpu,
                dst,
                &self.runtime.vertices,
                self.runtime.version,
                retained,
            ) {
                Ok(changed) => uploaded |= changed,
                Err(error) => {
                    Self::report_failure(&mut self.domain_failure, ctx, error.into());
                    return;
                }
            }
        }
        for (name, values, upload) in [
            ("foam", &self.runtime.whitewater.foam, &mut self.foam_upload),
            (
                "bubbles",
                &self.runtime.whitewater.bubbles,
                &mut self.bubble_upload,
            ),
            (
                "spray",
                &self.runtime.whitewater.spray,
                &mut self.spray_upload,
            ),
        ] {
            if let Some(dst) = ctx.outputs.array(name) {
                match upload.upload(gpu, dst, values, self.runtime.version, retained) {
                    Ok(changed) => uploaded |= changed,
                    Err(error) => {
                        Self::report_failure(&mut self.domain_failure, ctx, error.into());
                        return;
                    }
                }
            }
        }
        let upload_ms = upload_started.elapsed().as_secs_f64() * 1000.0;
        ctx.outputs
            .set_scalar("upload_ms", ParamValue::Float(upload_ms as f32));
        if retained
            && !uploaded
            && self.last_version == self.runtime.version
            && self.last_lag == lag.to_bits()
            && self.last_particle_version == self.runtime.particles.version
            && self.last_blend == blend.to_bits()
        {
            ctx.mark_outputs_unchanged();
        }
        self.last_version = self.runtime.version;
        self.last_lag = lag.to_bits();
        self.last_particle_version = self.runtime.particles.version;
        self.last_blend = blend.to_bits();
    }
}

impl FluidSurface {
    /// Section 3.2's scalar and lattice outputs for the published A/B pair.
    /// The arrays themselves are provided storage (`provided_array_output`).
    fn publish_particle_frame(&mut self, ctx: &mut EffectNodeContext<'_, '_>, blend: f32, span: f32) {
        let (a, b) = self
            .runtime
            .particles
            .pair()
            .map_or((None, None), |(a, b)| (a.frame(), b.frame()));
        let count = |frame: Option<SlotFrame>| frame.map_or(0.0, |frame| frame.info.count as f32);
        let epoch = |frame: Option<SlotFrame>| {
            frame.map_or(0.0, |frame| frame.info.identity_epoch as f32)
        };
        for (name, value) in [
            ("count_a", count(a)),
            ("count_b", count(b)),
            ("identity_a", epoch(a)),
            ("identity_b", epoch(b)),
            ("blend", blend),
            ("span", span),
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        // Geometry is CPU-owned and exists before the first particle capture.
        // Never publish a zero lattice as a ready surface input at tick zero.
        if let Some((bounds, nodes)) = self.runtime.particle_lattice() {
            if b.is_none()
                && let Some(gpu) = ctx.gpu.as_deref()
                && self.empty_particle_frame.as_ref().is_none_or(|empty| empty.nodes != nodes)
            {
                match EmptyParticleFrame::new(gpu.device, nodes) {
                    Ok(empty) => self.empty_particle_frame = Some(empty),
                    Err(error) => {
                        Self::report_failure(&mut self.domain_failure, ctx,
                            format!("Fluid empty particle frame: {error}"));
                        return;
                    }
                }
            }
            ctx.outputs.set_transform("grid_bounds", bounds);
            for (name, value) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].into_iter().zip(nodes) {
                ctx.outputs.set_scalar(name, ParamValue::Float(value as f32));
            }
        }
        if b.is_some() {
            // Buffer drops are fence-retired; neither the worker nor this
            // publisher ever overwrites the empty frame while it is read.
            self.empty_particle_frame = None;
        }
        if ctx.gpu.is_some() {
            // This frame's GPU work reads the pair; reuse waits for it.
            self.runtime.particles.mark_read();
        }
    }
}

/// The FLIP domain's settings from its params and wires (`read` is
/// `scalar_or_param`): the node and the extent checker build them with the
/// same function.
pub(crate) fn fluid_settings(
    read: impl Fn(&str, f32) -> f32,
    params: &ParamValues,
    domain: Option<crate::scene::transform::Transform>,
    initial_volume: Option<crate::scene::transform::Transform>,
    boundary_collisions: [bool; 6],
    seed: u64,
) -> FluidSettings {
    let param = |name: &str, default: f32| match params.get(name) {
        Some(ParamValue::Float(value)) => *value,
        _ => default,
    };
    FluidSettings {
        seed,
        resolution: read("resolution", 24.0).round() as u32,
        domain_size: read("domain_size", 4.0),
        domain,
        boundary_collisions,
        fill_height: read("fill_height", 0.4),
        initial_volume,
        surface_subdivisions: read("surface_subdivisions", 0.0).round() as u32,
        liquid: LiquidOptions {
            viscosity: f64::from(read("viscosity", 0.0)),
            surface_tension: f64::from(read("surface_tension", 0.0)),
        },
        time_steps: Default::default(),
        surface: SurfaceOptions {
            particle_scale: f64::from(read("surface_particle_scale", 3.0)),
            smoothing: f64::from(read("surface_smoothing", 0.5)),
            smoothing_iterations: read("surface_smoothing_iterations", 2.0).round() as u32,
        },
        whitewater: WhitewaterOptions {
            enabled: read("whitewater", 0.0) > 0.5,
            max_particles: param("whitewater_capacity", 100000.0).round() as u32,
            wavecrest_rate: f64::from(read("whitewater_wavecrest_rate", 175.0)),
            turbulence_rate: f64::from(read("whitewater_turbulence_rate", 175.0)),
            min_energy: f64::from(read("whitewater_min_energy", 0.1)),
            max_energy: f64::from(read("whitewater_max_energy", 60.0)),
        },
        apic: matches!(params.get("transfer"), Some(ParamValue::Enum(1))),
        max_vertices: (param("max_capacity", 786432.0).clamp(3.0, 3145728.0) as usize / 3) * 3,
    }
}

pub(crate) fn boundary_collisions(params: &ParamValues) -> Result<[bool; 6], String> {
    let mut faces = [true; 6];
    for (index, name) in [
        "closed_neg_x",
        "closed_pos_x",
        "closed_neg_y",
        "closed_pos_y",
        "closed_neg_z",
        "closed_pos_z",
    ]
    .into_iter()
    .enumerate()
    {
        faces[index] = match params.get(name) {
            None => true,
            Some(ParamValue::Bool(value)) => *value,
            Some(ParamValue::Float(value)) if value.is_finite() && (0.0..=1.0).contains(value) => {
                *value > 0.5
            }
            _ => return Err(format!("Fluid: {name} must be a boolean")),
        };
    }
    Ok(faces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::backend::MockBackend;
    use crate::bindings::{NodeInputs, NodeOutputs};
    use crate::exec::effect_node::FrameTime;
    use crate::scene::impulse::{ImpulseTarget, RigidImpulseTargets};
    use crate::water::physics::{PhysicsAuthoredSampleScope, PhysicsStepScope, RigidSceneInputs, RigidSceneObservation};
    use crate::water::physics_events::ResolvedNodeImpulse;
    use manifold_core::{Beats, Seconds};
    use manifold_physics::FieldValue;
    use manifold_physics::input::EventStamp;

    fn coupled_observation(transport: f64, speed: f32, reset: f32) -> RigidSceneObservation {
        RigidSceneObservation {
            inputs: RigidSceneInputs::default(),
            transport: Seconds(transport),
            speed,
            reset,
        }
    }

    fn run_mock(
        fluid: &mut FluidSurface,
        params: &ParamValues,
        transport: f64,
        errors: &mut Vec<String>,
    ) {
        run_mock_with_scalars(fluid, params, &[], transport, errors);
    }

    fn run_mock_with_scalars(
        fluid: &mut FluidSurface,
        params: &ParamValues,
        scalar_inputs: &[(&'static str, f32)],
        transport: f64,
        errors: &mut Vec<String>,
    ) {
        use crate::{exec::backend::Backend, ports::PortType, exec::execution_plan::ResourceId, ports::ScalarType};

        let mut backend = MockBackend::new();
        let bindings: Vec<_> = scalar_inputs.iter().enumerate().map(|(index, &(name, value))| {
            let slot = backend.acquire(ResourceId(index as u32), PortType::Scalar(ScalarType::F32), None, (0, 0));
            backend.set_scalar(slot, ParamValue::Float(value));
            (name, slot)
        }).collect();
        let inputs = NodeInputs::new(&bindings, &backend, &[]);
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let outputs = NodeOutputs::new(
            &[],
            &backend,
            &mut scalar,
            &mut camera,
            &mut light,
            &mut material,
            &mut transform,
            &mut atmosphere,
            &mut render_mode,
            &mut object,
        );
        let time = FrameTime {
            beats: Beats(transport),
            seconds: Seconds(transport),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx =
            EffectNodeContext::new(time, params, inputs, outputs, None).with_errors(errors);
        Primitive::run(fluid, &mut ctx);
    }

    #[test]
    fn fluid_gravity_axes_accept_graph_wires_and_reject_nonfinite_values() {
        let _offline = PhysicsStepScope::for_render(true);
        for name in ["gravity_x", "gravity", "gravity_z"] {
            let mut params = coupled_params();
            params.insert(Cow::Borrowed(name), ParamValue::Float(f32::NAN));
            let mut errors = Vec::new();
            let mut fluid = FluidSurface::new();
            run_mock(&mut fluid, &params, 0.0, &mut errors);
            assert!(errors.iter().any(|error| error.contains("gravity")), "{name}: {errors:?}");

            errors.clear();
            let mut fluid = FluidSurface::new();
            run_mock_with_scalars(&mut fluid, &params, &[(name, 2.0)], 0.0, &mut errors);
            assert!(errors.is_empty(), "wired {name} must override its local parameter: {errors:?}");
            assert_eq!(fluid.runtime.domain_snapshot().state, FluidDomainState::Ready);

            errors.clear();
            params.insert(Cow::Borrowed(name), ParamValue::Float(0.0));
            let mut fluid = FluidSurface::new();
            run_mock_with_scalars(&mut fluid, &params, &[(name, f32::INFINITY)], 0.0, &mut errors);
            assert!(errors.iter().any(|error| error.contains("gravity")), "wired {name}: {errors:?}");
            assert_eq!(PhysicsNode::fluid_domain_snapshot(&fluid).unwrap().state, FluidDomainState::Failed);
        }
    }

    fn coupled_params() -> ParamValues {
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("resolution"), ParamValue::Float(8.0));
        params.insert(Cow::Borrowed("fill_height"), ParamValue::Float(0.0));
        params.insert(Cow::Borrowed("gravity"), ParamValue::Float(0.0));
        params
    }

    #[test]
    fn fluid_seed_is_saved_and_restarts_the_simulation() {
        let _offline = PhysicsStepScope::for_render(true);
        let mut fluid = FluidSurface::new();
        let mut params = coupled_params();
        let mut errors = Vec::new();
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let original_epoch = fluid.runtime.domain_snapshot().epoch;
        params.insert(Cow::Borrowed("seed"), ParamValue::Float(16_777_215.0));
        use crate::persistence::SerializedParamValue;
        let authored: std::collections::BTreeMap<String, SerializedParamValue> = params
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone().into()))
            .collect();
        let saved = serde_json::to_vec(&authored).unwrap();
        let restored: std::collections::BTreeMap<String, SerializedParamValue> =
            serde_json::from_slice(&saved).unwrap();
        let restored: ParamValues = restored
            .into_iter()
            .map(|(key, value)| (Cow::Owned(key), value.into()))
            .collect();
        run_mock(&mut fluid, &restored, 0.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let new_epoch = fluid.runtime.domain_snapshot().epoch;
        assert_ne!(new_epoch, original_epoch);
        run_mock(&mut fluid, &restored, 0.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(fluid.runtime.domain_snapshot().epoch, new_epoch);
    }

    #[test]
    fn fluid_seed_rejects_fractional_nonfinite_and_out_of_range_values() {
        for value in [-1.0, 0.5, 16_777_216.0, f32::NAN, f32::INFINITY] {
            let mut params = coupled_params();
            params.insert(Cow::Borrowed("seed"), ParamValue::Float(value));
            let mut fluid = FluidSurface::new();
            let mut errors = Vec::new();
            run_mock(&mut fluid, &params, 0.0, &mut errors);
            assert!(
                errors.iter().any(|error| error.contains("seed must be an integer")),
                "{value}: {errors:?}"
            );
            assert_eq!(
                PhysicsNode::fluid_domain_snapshot(&fluid).unwrap().state,
                FluidDomainState::Failed
            );
        }
    }

    #[test]
    fn coupled_fluid_uses_paired_runtime_speed_and_publishes_frame() {
        let _offline = PhysicsStepScope::for_render(true);
        let mut fluid = FluidSurface::new();
        let mut errors = Vec::new();
        let params = coupled_params();
        let first = coupled_observation(0.0, 1.0, 0.0);
        PhysicsNode::set_coupled_physics(&mut fluid, true);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&first),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            fluid.runtime.domain_snapshot().state,
            FluidDomainState::Ready
        );
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_some());

        let second = coupled_observation(1.0 / 60.0, 2.0, 0.0);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&second),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock_with_scalars(
            &mut fluid,
            &params,
            &[("speed", 2.0)],
            1.0 / 60.0,
            &mut errors,
        );
        assert!(errors.is_empty(), "{errors:?}");
        // A speed edit starts at its observation; the preceding interval
        // retains speed 1, then the next interval advances at shared speed 2.
        assert!((fluid.runtime.simulation_time() - 1.0 / 60.0).abs() < 1e-8);
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_some());
        let third = coupled_observation(2.0 / 60.0, 2.0, 0.0);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&third),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock_with_scalars(&mut fluid, &params, &[("speed", 2.0)], 2.0 / 60.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert!((fluid.runtime.simulation_time() - 3.0 / 60.0).abs() < 1e-8);
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_some());
    }

    #[test]
    fn coupled_fluid_rejects_speed_mismatch_until_shared_speed_recovers() {
        let _offline = PhysicsStepScope::for_render(true);
        let mut fluid = FluidSurface::new();
        let params = coupled_params();
        let mut errors = Vec::new();
        let first = coupled_observation(0.0, 1.0, 0.0);
        PhysicsNode::set_coupled_physics(&mut fluid, true);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&first),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock_with_scalars(&mut fluid, &params, &[("speed", 1.0)], 0.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let initial_time = fluid.runtime.simulation_time();
        let initial_epoch = fluid.runtime.domain_snapshot().epoch;

        let second = coupled_observation(1.0 / 60.0, 2.0, 0.0);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&second),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock_with_scalars(&mut fluid, &params, &[("speed", 1.0)], 1.0 / 60.0, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("same Simulation Speed")),
            "{errors:?}"
        );
        assert_eq!(fluid.runtime.simulation_time(), initial_time);
        assert_eq!(fluid.runtime.domain_snapshot().epoch, initial_epoch);
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_none());

        errors.clear();
        run_mock_with_scalars(
            &mut fluid,
            &params,
            &[("speed", f32::NAN)],
            1.0 / 60.0,
            &mut errors,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("same Simulation Speed")),
            "{errors:?}"
        );
        assert_eq!(fluid.runtime.simulation_time(), initial_time);
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_none());

        errors.clear();
        run_mock_with_scalars(&mut fluid, &params, &[("speed", 2.0)], 1.0 / 60.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        // A speed edit starts at its observation; the preceding interval
        // retains speed 1, then the next interval advances at shared speed 2.
        assert!((fluid.runtime.simulation_time() - 1.0 / 60.0).abs() < 1e-8);
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_some());
        let third = coupled_observation(2.0 / 60.0, 2.0, 0.0);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&third),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock_with_scalars(&mut fluid, &params, &[("speed", 2.0)], 2.0 / 60.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert!((fluid.runtime.simulation_time() - 3.0 / 60.0).abs() < 1e-8);
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_some());
    }

    #[test]
    fn coupled_fluid_pending_and_error_never_fall_back_to_standalone() {
        let _offline = PhysicsStepScope::for_render(true);
        let mut fluid = FluidSurface::new();
        let params = coupled_params();
        let mut errors = Vec::new();
        PhysicsNode::set_coupled_physics(&mut fluid, true);
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert_ne!(
            fluid.runtime.domain_snapshot().state,
            FluidDomainState::Ready
        );
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_none());
        assert_eq!(
            PhysicsNode::fluid_domain_snapshot(&fluid).unwrap().state,
            FluidDomainState::PendingInputs
        );

        let observation = coupled_observation(0.0, 1.0, 0.0);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&observation),
            RigidImpulseTargets::default(),
            Some("rigid capture failed"),
        );
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert_eq!(
            PhysicsNode::fluid_domain_snapshot(&fluid).unwrap().state,
            FluidDomainState::Failed
        );
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_none());
        assert!(
            errors
                .iter()
                .any(|error| error.contains("rigid capture failed"))
        );
    }

    #[test]
    fn coupled_fluid_reset_edges_are_independent_and_historical_world_reset_is_pending() {
        let _offline = PhysicsStepScope::for_render(true);
        let mut fluid = FluidSurface::new();
        let mut params = coupled_params();
        let mut errors = Vec::new();
        let first = coupled_observation(0.0, 1.0, 0.0);
        PhysicsNode::set_coupled_physics(&mut fluid, true);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&first),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let initial_epoch = fluid.runtime.domain_snapshot().epoch;

        let world_reset = coupled_observation(1.0 / 60.0, 1.0, 1.0);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&world_reset),
            RigidImpulseTargets::default(),
            None,
        );
        {
            let _historical = PhysicsAuthoredSampleScope::new();
            run_mock(&mut fluid, &params, 1.0 / 60.0, &mut errors);
        }
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(fluid.runtime.domain_snapshot().epoch, initial_epoch);
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_none());

        run_mock(&mut fluid, &params, 1.0 / 60.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let after_world_reset = fluid.runtime.domain_snapshot().epoch;
        assert_eq!(after_world_reset, initial_epoch + 1);

        params.insert(Cow::Borrowed("reset"), ParamValue::Float(1.0));
        let same_world_reset = coupled_observation(2.0 / 60.0, 1.0, 1.0);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&same_world_reset),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock(&mut fluid, &params, 2.0 / 60.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(fluid.runtime.domain_snapshot().epoch, after_world_reset + 1);
    }

    #[test]
    fn coupled_fluid_rejects_transport_and_density_mismatch() {
        let _offline = PhysicsStepScope::for_render(true);
        let mut fluid = FluidSurface::new();
        let mut params = coupled_params();
        let mut errors = Vec::new();
        let observation = coupled_observation(0.25, 1.0, 0.0);
        PhysicsNode::set_coupled_physics(&mut fluid, true);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&observation),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert!(errors.iter().any(|error| error.contains("transport")));
        errors.clear();

        params.insert(Cow::Borrowed("liquid_density"), ParamValue::Float(-1.0));
        let valid = coupled_observation(0.0, 1.0, 0.0);
        PhysicsNode::set_coupled_rigid_inputs(
            &mut fluid,
            Some(&valid),
            RigidImpulseTargets::default(),
            None,
        );
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert!(errors.iter().any(|error| error.contains("density")));
        assert!(PhysicsNode::coupled_rigid_frame(&fluid).is_none());
    }

    #[test]
    fn scene_physics_unconnected_fluid_source_does_not_emit_demo_liquid() {
        let backend = MockBackend::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("resolution"), ParamValue::Float(8.0));
        params.insert(Cow::Borrowed("fill_height"), ParamValue::Float(0.0));
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let mut errors = Vec::new();
        let mut fluid = FluidSurface::new();
        let _offline = PhysicsStepScope::for_render(true);
        for frame in 0..=3 {
            let inputs = NodeInputs::new(&[], &backend, &[]);
            let outputs = NodeOutputs::new(
                &[],
                &backend,
                &mut scalar,
                &mut camera,
                &mut light,
                &mut material,
                &mut transform,
                &mut atmosphere,
                &mut render_mode,
                &mut object,
            );
            let time = FrameTime {
                beats: Beats(f64::from(frame) / 60.0),
                seconds: Seconds(f64::from(frame) / 60.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: frame.into(),
            };
            let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None)
                .with_errors(&mut errors);
            Primitive::run(&mut fluid, &mut ctx);
        }
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(fluid.runtime.stats.particles, 0);
        assert!(fluid.runtime.vertices.is_empty());
        assert!((fluid.runtime.simulation_time() - 3.0 / 60.0).abs() < 1e-8);
    }

    #[test]
    fn fluid_surface_effect_node_impulse_dispatch_preserves_receipt_and_retry_sequence() {
        use crate::exec::effect_node::EffectNode;
        let mut node = FluidSurface::new();
        let settings = FluidSettings {
            resolution: 8,
            fill_height: 0.0,
            ..FluidSettings::default()
        };
        let controls = FluidControls {
            gravity: [0.0; 3],
            emission: false,
            obstacle_enabled: false,
            ..FluidControls::default()
        };
        node.runtime
            .observe(settings, controls, Seconds::ZERO, 1.0, 0.0)
            .expect("native fluid initialization");
        let epoch = PhysicsNode::physics_impulse_epoch(&node).expect("native impulse epoch");
        let stamp = EventStamp {
            epoch,
            time: Seconds::ZERO,
            sequence: 23,
        };
        let field = FieldValue::uniform([-1.0, 2.0, 0.5]).expect("finite impulse field");
        let wrong = ResolvedNodeImpulse {
            field: field.clone(),
            target: ImpulseTarget::Rigid(Default::default()),
        };
        let valid = ResolvedNodeImpulse {
            field: field.clone(),
            target: ImpulseTarget::Fluid,
        };

        {
            let graph_node: &mut dyn EffectNode = &mut node;
            let native = crate::water::node::get_mut(graph_node)
                .expect("FluidSurface has a native PhysicsNode registration");
            assert!(native
                .enqueue_physics_impulse(stamp, wrong)
                .expect_err("wrong target must be rejected before queue admission")
                .contains("rigid"));
            assert_eq!(
                native
                    .enqueue_physics_impulse(stamp, valid)
                    .expect("same producer sequence must remain valid")
                    .tick,
                0
            );
        }

        node.runtime
            .observe(settings, controls, Seconds(crate::water::fluid::TICK), 1.0, 0.0)
            .expect("native fluid observation");
        node.runtime.advance(true).expect("native fluid tick");

        let mut receipts = Vec::new();
        let graph_node: &mut dyn EffectNode = &mut node;
        crate::water::node::get_mut(graph_node)
            .expect("FluidSurface has a native PhysicsNode registration")
            .drain_physics_impulses(&mut |event| receipts.push(event));
        assert_eq!(receipts.len(), 1);
        let receipt = receipts.pop().expect("one fluid receipt");
        assert_eq!(receipt.source, stamp);
        assert_eq!(receipt.applied, manifold_physics::TickStamp { epoch, tick: 0 });
        assert_eq!(receipt.lateness, Seconds::ZERO);
        assert_eq!(receipt.value.field, field);
        assert_eq!(receipt.value.target, ImpulseTarget::Fluid);
        crate::water::node::get_mut(graph_node)
            .expect("FluidSurface has a native PhysicsNode registration")
            .drain_physics_impulses(&mut |_| panic!("receipt drained twice"));
    }

    #[test]
    fn fluid_grid_budget_is_explicit_and_can_be_increased() {
        let mut fluid = FluidSurface::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("resolution"), ParamValue::Float(8.0));
        params.insert(Cow::Borrowed("fill_height"), ParamValue::Float(0.0));
        params.insert(Cow::Borrowed("grid_budget_mcells"), ParamValue::Float(0.001));
        let mut errors = Vec::new();
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert!(errors.iter().any(|error| error.contains("Grid Budget")));
        params.insert(Cow::Borrowed("grid_budget_mcells"), ParamValue::Float(0.01));
        errors.clear();
        run_mock(&mut fluid, &params, 0.0, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn fluid_surface_domain_snapshot_recovers_after_validation_error() {
        let backend = MockBackend::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("fill_height"), ParamValue::Float(0.0));
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let mut errors = Vec::new();
        let mut fluid = FluidSurface::new();
        let _offline = PhysicsStepScope::for_render(true);
        for resolution in [8.0, f32::NAN, 8.0] {
            params.insert(Cow::Borrowed("resolution"), ParamValue::Float(resolution));
            let inputs = NodeInputs::new(&[], &backend, &[]);
            let outputs = NodeOutputs::new(
                &[],
                &backend,
                &mut scalar,
                &mut camera,
                &mut light,
                &mut material,
                &mut transform,
                &mut atmosphere,
                &mut render_mode,
                &mut object,
            );
            let time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(0.0),
                delta: Seconds(0.0),
                frame_count: 0,
            };
            let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None)
                .with_errors(&mut errors);
            Primitive::run(&mut fluid, &mut ctx);
            let snapshot = PhysicsNode::fluid_domain_snapshot(&fluid).unwrap();
            if resolution.is_finite() {
                assert!(errors.is_empty(), "{errors:?}");
                assert_eq!(snapshot.state, FluidDomainState::Ready);
                assert!(snapshot.accepted_layout.is_some());
            } else {
                assert!(!errors.is_empty());
                assert_eq!(snapshot.state, FluidDomainState::Failed);
                assert!(snapshot.accepted_layout.is_none());
            }
            errors.clear();
        }
    }

    #[test]
    fn fluid_surface_domain_snapshot_hides_accepted_bounds_for_pending_or_failed_state() {
        let mut fluid = FluidSurface::new();
        fluid
            .runtime
            .observe(
                FluidSettings {
                    resolution: 8,
                    ..FluidSettings::default()
                },
                FluidControls::default(),
                Seconds(0.0),
                1.0,
                0.0,
            )
            .unwrap();
        fluid.runtime.advance(false).unwrap();
        assert!(fluid.warmup_pending());
        fluid.domain_failure = true;
        assert!(
            !fluid.warmup_pending(),
            "failed inputs must not keep pumping an old worker"
        );
        fluid.domain_failure = false;
        fluid.runtime.advance(true).unwrap();
        assert_eq!(
            PhysicsNode::fluid_domain_snapshot(&fluid).unwrap().state,
            FluidDomainState::Ready
        );

        fluid.role_pending = true;
        let pending = PhysicsNode::fluid_domain_snapshot(&fluid).unwrap();
        assert_eq!(pending.state, FluidDomainState::PendingInputs);
        assert!(pending.accepted_layout.is_none());

        fluid.role_pending = false;
        fluid.domain_failure = true;
        let failed = PhysicsNode::fluid_domain_snapshot(&fluid).unwrap();
        assert_eq!(failed.state, FluidDomainState::Failed);
        assert!(failed.accepted_layout.is_none());

        fluid.clear_state();
        let reset = PhysicsNode::fluid_domain_snapshot(&fluid).unwrap();
        assert_eq!(reset.state, FluidDomainState::Initializing);
        assert!(reset.accepted_layout.is_none());
    }

    /// `run_mock` with the named array outputs bound, as a plan binds wired
    /// outputs.
    fn run_mock_with_outputs(
        fluid: &mut FluidSurface,
        params: &ParamValues,
        outputs: &[&'static str],
        errors: &mut Vec<String>,
    ) {
        use crate::ports::ArrayType;
        use crate::{exec::backend::Backend, ports::PortType, exec::execution_plan::ResourceId};

        let mut backend = MockBackend::new();
        let bindings: Vec<_> = outputs
            .iter()
            .enumerate()
            .map(|(index, &name)| {
                let ty = if name.starts_with("solid") {
                    ArrayType::of_known::<f32>()
                } else {
                    ArrayType::of_known::<FluidParticle>()
                };
                (name, backend.acquire(ResourceId(index as u32), PortType::Array(ty), None, (0, 0)))
            })
            .collect();
        let inputs = NodeInputs::new(&[], &backend, &[]);
        let (mut scalar, mut camera, mut light, mut material) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut transform, mut atmosphere, mut render_mode, mut object) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let outputs = NodeOutputs::new(
            &bindings,
            &backend,
            &mut scalar,
            &mut camera,
            &mut light,
            &mut material,
            &mut transform,
            &mut atmosphere,
            &mut render_mode,
            &mut object,
        );
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(time, params, inputs, outputs, None).with_errors(errors);
        Primitive::run(fluid, &mut ctx);
    }

    #[test]
    fn first_frame_particle_lattice_is_valid_before_gpu_publish() {
        use crate::{exec::backend::Backend, ports::PortType, exec::execution_plan::ResourceId, ports::ScalarType};

        let mut fluid = FluidSurface::new();
        let settings = FluidSettings { resolution: 8, ..FluidSettings::default() };
        fluid.runtime.set_outputs(true, false);
        fluid.runtime.observe(settings, FluidControls::default(), Seconds(0.0), 1.0, 0.0).unwrap();
        fluid.runtime.advance(true).unwrap();
        assert_eq!(fluid.runtime.simulation_time(), 0.0);
        assert!(!fluid.runtime.particle_capture_pending(), "native capture starts after the first completed tick");
        assert!(fluid.runtime.particles.pair().is_none());
        let expected = settings.domain_layout().unwrap().solid_lattice();
        let mut backend = MockBackend::new();
        let names = ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"];
        let mut bindings: Vec<_> = names.iter().enumerate().map(|(i, &name)| {
            (name, backend.acquire(ResourceId(i as u32), PortType::Scalar(ScalarType::F32), None, (0, 0)))
        }).collect();
        bindings.push(("grid_bounds", backend.acquire(ResourceId(3), PortType::Transform, None, (0, 0))));
        let inputs = NodeInputs::new(&[], &backend, &[]);
        let (mut scalar, mut camera, mut light, mut material) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut transform, mut atmosphere, mut render_mode, mut object) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let outputs = NodeOutputs::new(&bindings, &backend, &mut scalar, &mut camera, &mut light,
            &mut material, &mut transform, &mut atmosphere, &mut render_mode, &mut object);
        let params = ParamValues::default();
        let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(0.0), frame_count: 1 };
        let pending = {
            let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
            fluid.publish_particle_frame(&mut ctx, 1.0, 0.0);
            ctx.outputs_pending
        };
        assert_eq!(scalar.len(), 3);
        for ((_, value), expected) in scalar.iter().zip(expected.1) {
            assert_eq!(value.as_scalar(), Some(expected as f32));
        }
        assert_eq!(transform[0].1, expected.0);
        let cell = transform[0].1.scale[0] / (scalar[0].1.as_scalar().unwrap() - 1.0);
        assert!(cell.is_finite() && cell > 0.0);
        assert!(!pending, "the initial empty frame has valid CPU geometry");
    }

    #[test]
    #[cfg(feature = "gpu-proofs")]
    fn fluid_empty_particle_frame_has_complete_zeroed_storage() {
        let device = manifold_gpu::testkit::test_device();
        let nodes = [12; 3];
        let empty = EmptyParticleFrame::new(&device, nodes).unwrap();
        assert_eq!(empty.particles.size, std::mem::size_of::<FluidParticle>() as u64);
        assert_eq!(empty.solid.size, 12 * 12 * 12 * 4);
        for buffer in [&empty.particles, &empty.solid] {
            // SAFETY: freshly allocated shared storage, never submitted to GPU.
            let bytes = unsafe {
                std::slice::from_raw_parts(buffer.mapped_ptr().unwrap(), buffer.size as usize)
            };
            assert!(bytes.iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn fluid_particle_outputs_reject_record_and_playback() {
        let _offline = PhysicsStepScope::for_render(true);
        for (mode, rejected) in [(0, false), (1, true), (2, true)] {
            for outputs in [&["particles_b"][..], &["solid_a"], &["vertices"]] {
                let mut params = coupled_params();
                params.insert(Cow::Borrowed("cache_mode"), ParamValue::Enum(mode));
                params.insert(
                    Cow::Borrowed("cache_path"),
                    ParamValue::String(std::sync::Arc::new("/tmp/manifold-unused-particle-cache".into())),
                );
                let mut errors = Vec::new();
                let mut fluid = FluidSurface::new();
                run_mock_with_outputs(&mut fluid, &params, outputs, &mut errors);
                let named = errors
                    .iter()
                    .any(|error| error.contains("particle outputs require Live mode"));
                let particle_port = outputs[0] != "vertices";
                assert_eq!(named, rejected && particle_port, "mode {mode} {outputs:?}: {errors:?}");
                if named {
                    assert_eq!(
                        PhysicsNode::fluid_domain_snapshot(&fluid).unwrap().state,
                        FluidDomainState::Failed
                    );
                }
            }
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
