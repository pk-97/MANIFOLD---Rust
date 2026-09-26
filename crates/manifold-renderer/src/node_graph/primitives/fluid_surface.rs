//! Native FLIP/APIC simulation is one FFI boundary; its mesh remains a
//! composable input to the existing material and scene rendering graph.
use std::borrow::Cow;

use crate::frame_status::{FrameRenderFailure, FrameRenderStatus};
use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::{FluidControls, FluidRuntime, FluidSettings};
use crate::node_graph::fluid_cache::CacheMode;
use crate::node_graph::fluid_mesh_upload::FluidMeshUpload;
use crate::node_graph::instance_upload::InstanceSnapshotUpload;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use manifold_fluids::{LiquidOptions, SurfaceOptions, WhitewaterOptions};

crate::primitive! {
    name: FluidSurface,
    type_id: "node.fluid_surface",
    purpose: "Simulate a cubic liquid domain with the native FLIP Fluids CPU engine and output its reconstructed surface. Translate emitter and obstacle boxes through Transform inputs; use the accepted obstacle_pose to render the collider at the same simulation time as the liquid.",
    inputs: {
        emitter: Transform optional, obstacle: Transform optional, initial_volume: Transform optional,
        resolution: ScalarF32 optional, domain_size: ScalarF32 optional, fill_height: ScalarF32 optional,
        viscosity: ScalarF32 optional, surface_tension: ScalarF32 optional,
        gravity: ScalarF32 optional, emission: ScalarF32 optional, inflow_speed: ScalarF32 optional,
        speed: ScalarF32 optional, reset: ScalarF32 optional, surface_subdivisions: ScalarF32 optional,
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
    },
    params: [
        ParamDef { name: Cow::Borrowed("resolution"), label: "Resolution", ty: ParamType::Int, default: ParamValue::Float(24.0), range: Some((8.0, 96.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Initial Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("viscosity"), label: "Viscosity", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("surface_tension"), label: "Surface Tension", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
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
        ParamDef { name: Cow::Borrowed("max_capacity"), label: "Mesh Capacity", ty: ParamType::Int, default: ParamValue::Float(786432.0), range: Some((3.0, 3145728.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cache_mode"), label: "Cache Mode", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 2.0)), enum_values: &["Live", "Record", "Playback"] },
        ParamDef { name: Cow::Borrowed("cache_path"), label: "Cache Path", ty: ParamType::String, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "CPU reference engine, not a real-time guarantee. Domain is a cube centered in X/Z, with its floor at Y=0, in metres. Emitter/obstacle/initial_volume transforms describe axis-aligned boxes using full dimensions; rotations and billboards are rejected, and initial_volume must be fully contained in the domain. The optional initial_volume seeds a localized zero-velocity column in addition to the fill_height pool. Domain size, resolution, fill, initial volume, transfer and surface detail changes restart the simulation. Native state lives on a background worker. Preview retains time debt and displays the latest complete mesh; export drains the same fixed 60 Hz ticks. Historical controls use the existing 240 Hz stateless physics ancestry sampler. Reset and backwards transport start a fresh simulation. Wire obstacle_pose to the visible unit-cube collider to avoid showing it ahead of the fluid. Overflow is a visible error, never a truncated mesh. Native whitewater is optional and defaults off. Its foam, bubbles and spray outputs are instance transforms at the same accepted tick as the mesh; wire each matching count to scene_object.instance_count and author particle meshes/materials separately. Particle scale and smoothing affect surface reconstruction, not solver dynamics. Liquid, surface and whitewater settings restart the world. Viscosity and surface tension use scale-dependent native coefficients, not calibrated material units. Surface-tension validation includes the 64-cubed dam-break regression; the honey reference uses zero tension. Whitewater capacity bounds native emission; the three output arrays each reserve that capacity. Particle instances shrink during their last 0.2 seconds. Two-way Box3D coupling is not part of this integration. Mesh output uses the engine mesher; material and rendering stay separate graph nodes. cache_mode is Live, Record or Playback and cache_path names a compressed fixed-60-Hz geometry snapshot stream. Record publishes atomically; Playback uses baked geometry, whitewater, obstacle pose and stats exactly and does not run the solver. Playback requires every requested tick and never silently falls back to Live. The physical settings and fixed tick are part of the cache manifest.",
    examples: ["WaterBasin", "WaterDamBreak", "HoneyDamBreak"],
    picker: { label: "Liquid Surface", category: Atom },
    summary: "Simulate liquid, a pouring source and a moving box, and generate a surface for scene rendering.",
    category: Geometry3D, role: Source,
    aliases: ["water", "liquid", "fluid", "FLIP", "APIC"],
    boundary_reason: IoBridge,
    extra_fields: {
        runtime: FluidRuntime = FluidRuntime::default(),
        upload: FluidMeshUpload = FluidMeshUpload::default(),
        foam_upload: InstanceSnapshotUpload = InstanceSnapshotUpload::default(),
        bubble_upload: InstanceSnapshotUpload = InstanceSnapshotUpload::default(),
        spray_upload: InstanceSnapshotUpload = InstanceSnapshotUpload::default(),
        last_version: u64 = u64::MAX,
        last_lag: u32 = u32::MAX,
    },
}

impl FluidSurface {
    fn report_failure(ctx: &mut EffectNodeContext<'_, '_>, error: String) {
        ctx.error(error);
        ctx.mark_outputs_pending();
        if let Some(gpu) = ctx.gpu.as_deref_mut() {
            gpu.merge_frame_status(FrameRenderStatus::Failed(FrameRenderFailure::Simulation));
        }
    }
}

impl Primitive for FluidSurface {
    fn clear_state(&mut self) {
        self.runtime.clear();
    }
    fn warmup_pending(&self) -> bool {
        self.runtime.warmup_pending()
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
        if port != "vertices" {
            return None;
        }
        let value = match params.get("max_capacity") {
            Some(ParamValue::Float(n)) => *n,
            _ => 786432.0,
        };
        Some((value.clamp(3.0, 3145728.0) as u32 / 3) * 3)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        for (name, fallback) in [
            ("resolution", 24.0),
            ("viscosity", 0.0),
            ("surface_tension", 0.0),
            ("surface_subdivisions", 0.0),
            ("emission", 1.0),
            ("surface_smoothing_iterations", 2.0),
            ("whitewater", 0.0),
            ("whitewater_capacity", 100000.0),
        ] {
            if !ctx.scalar_or_param(name, fallback).is_finite() {
                Self::report_failure(ctx, format!("Water: {name} must be finite"));
                return;
            }
        }
        if !(0.0..=2.0).contains(&ctx.scalar_or_param("surface_subdivisions", 0.0)) {
            Self::report_failure(ctx, "Water: surface detail must be between 0 and 2".into());
            return;
        }
        if !(0.0..=10.0).contains(&ctx.scalar_or_param("surface_smoothing_iterations", 2.0))
            || !(1.0..=250000.0).contains(&ctx.param_f32("whitewater_capacity", 100000.0))
        {
            Self::report_failure(
                ctx,
                "Water: invalid surface smoothing iterations or whitewater capacity".into(),
            );
            return;
        }
        let settings = FluidSettings {
            resolution: ctx.scalar_or_param("resolution", 24.0).round() as u32,
            domain_size: ctx.scalar_or_param("domain_size", 4.0),
            fill_height: ctx.scalar_or_param("fill_height", 0.4),
            initial_volume: ctx.inputs.transform("initial_volume"),
            surface_subdivisions: ctx.scalar_or_param("surface_subdivisions", 0.0).round() as u32,
            liquid: LiquidOptions {
                viscosity: f64::from(ctx.scalar_or_param("viscosity", 0.0)),
                surface_tension: f64::from(ctx.scalar_or_param("surface_tension", 0.0)),
            },
            surface: SurfaceOptions {
                particle_scale: f64::from(ctx.scalar_or_param("surface_particle_scale", 3.0)),
                smoothing: f64::from(ctx.scalar_or_param("surface_smoothing", 0.5)),
                smoothing_iterations: ctx
                    .scalar_or_param("surface_smoothing_iterations", 2.0)
                    .round() as u32,
            },
            whitewater: WhitewaterOptions {
                enabled: ctx.scalar_or_param("whitewater", 0.0) > 0.5,
                max_particles: ctx.param_f32("whitewater_capacity", 100000.0).round() as u32,
                wavecrest_rate: f64::from(ctx.scalar_or_param("whitewater_wavecrest_rate", 175.0)),
                turbulence_rate: f64::from(
                    ctx.scalar_or_param("whitewater_turbulence_rate", 175.0),
                ),
                min_energy: f64::from(ctx.scalar_or_param("whitewater_min_energy", 0.1)),
                max_energy: f64::from(ctx.scalar_or_param("whitewater_max_energy", 60.0)),
            },
            apic: matches!(ctx.params.get("transfer"), Some(ParamValue::Enum(1))),
            max_vertices: (ctx
                .param_f32("max_capacity", 786432.0)
                .clamp(3.0, 3145728.0) as usize
                / 3)
                * 3,
        };
        let cache_mode = match ctx.params.get("cache_mode") {
            Some(ParamValue::Enum(value)) => CacheMode::from_enum(*value),
            None => Some(CacheMode::Live),
            _ => None,
        };
        let Some(cache_mode) = cache_mode else {
            Self::report_failure(
                ctx,
                "Water: cache mode must be Live, Record or Playback".into(),
            );
            return;
        };
        let cache_path = match ctx.params.get("cache_path") {
            Some(ParamValue::String(path)) => path.as_str(),
            Some(ParamValue::Float(_)) | None => "",
            _ => {
                Self::report_failure(ctx, "Water: cache path must be a String".into());
                return;
            }
        };
        if let Err(error) = self.runtime.set_cache(cache_mode, cache_path) {
            Self::report_failure(ctx, error);
            return;
        }
        let defaults = FluidControls::default();
        let controls = FluidControls {
            emitter: ctx.inputs.transform("emitter").unwrap_or(defaults.emitter),
            obstacle: ctx
                .inputs
                .transform("obstacle")
                .unwrap_or(defaults.obstacle),
            gravity: ctx.scalar_or_param("gravity", -9.81),
            emission: ctx.scalar_or_param("emission", 1.0) > 0.5,
            inflow_speed: ctx.scalar_or_param("inflow_speed", 1.5),
        };
        if let Err(error) = self.runtime.observe(
            settings,
            controls,
            ctx.time.seconds,
            ctx.scalar_or_param("speed", 1.0),
            ctx.scalar_or_param("reset", 0.0),
        ) {
            Self::report_failure(ctx, error);
            return;
        }
        if crate::node_graph::physics::authored_sample_only() {
            return;
        }
        if let Err(error) = self
            .runtime
            .advance(crate::node_graph::physics::offline_simulation())
        {
            Self::report_failure(ctx, error);
            return;
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
        let retained = ctx.outputs_retained();
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return;
        };
        let mut uploaded = false;
        if let Some(dst) = ctx.outputs.array("vertices") {
            match self.upload.upload(
                gpu,
                dst,
                &self.runtime.vertices,
                self.runtime.version,
                retained,
            ) {
                Ok(changed) => uploaded |= changed,
                Err(error) => {
                    Self::report_failure(ctx, error.into());
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
                        Self::report_failure(ctx, error.into());
                        return;
                    }
                }
            }
        }
        if retained
            && !uploaded
            && self.last_version == self.runtime.version
            && self.last_lag == lag.to_bits()
        {
            ctx.mark_outputs_unchanged();
        }
        self.last_version = self.runtime.version;
        self.last_lag = lag.to_bits();
    }
}
