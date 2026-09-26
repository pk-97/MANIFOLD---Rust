//! Native FLIP/APIC simulation is one FFI boundary; its mesh remains a
//! composable input to the existing material and scene rendering graph.
use std::borrow::Cow;

use crate::frame_status::{FrameRenderFailure, FrameRenderStatus};
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::{FluidControls, FluidRuntime, FluidSettings};
use crate::node_graph::fluid_mesh_upload::FluidMeshUpload;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: FluidSurface,
    type_id: "node.fluid_surface",
    purpose: "Simulate a cubic liquid domain with the native FLIP Fluids CPU engine and output its reconstructed surface. Translate emitter and obstacle boxes through Transform inputs; use the accepted obstacle_pose to render the collider at the same simulation time as the liquid.",
    inputs: {
        emitter: Transform optional, obstacle: Transform optional,
        resolution: ScalarF32 optional, domain_size: ScalarF32 optional, fill_height: ScalarF32 optional,
        gravity: ScalarF32 optional, emission: ScalarF32 optional, inflow_speed: ScalarF32 optional,
        speed: ScalarF32 optional, reset: ScalarF32 optional, surface_subdivisions: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex), obstacle_pose: Transform,
        simulation_time: ScalarF32, lag_seconds: ScalarF32, simulation_ms: ScalarF32,
        meshing_ms: ScalarF32, particle_count: ScalarF32, vertex_count: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("resolution"), label: "Resolution", ty: ParamType::Int, default: ParamValue::Float(24.0), range: Some((8.0, 96.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Initial Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("emission"), label: "Pour", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &["Off", "On"] },
        ParamDef { name: Cow::Borrowed("inflow_speed"), label: "Flow Speed", ty: ParamType::Float, default: ParamValue::Float(1.5), range: Some((0.0, 5.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("speed"), label: "Simulation Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("surface_subdivisions"), label: "Surface Detail", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("transfer"), label: "Transfer", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: &["FLIP", "APIC"] },
        ParamDef { name: Cow::Borrowed("max_capacity"), label: "Mesh Capacity", ty: ParamType::Int, default: ParamValue::Float(786432.0), range: Some((3.0, 3145728.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "CPU reference engine, not a real-time guarantee. Domain is a cube centered in X/Z, with its floor at Y=0, in metres. Emitter/obstacle transforms describe axis-aligned boxes using full dimensions; rotations and billboards are rejected. Domain size, resolution, fill, transfer and surface detail changes restart the simulation. Native state lives on a background worker. Preview retains time debt and displays the latest complete mesh; export drains the same fixed 60 Hz ticks. Historical controls use the existing 240 Hz stateless physics ancestry sampler. Reset and backwards transport start a fresh simulation. Wire obstacle_pose to the visible unit-cube collider to avoid showing it ahead of the fluid. Overflow is a visible error, never a truncated mesh. Whitewater and two-way Box3D coupling are not part of this initial integration. Mesh output uses the engine mesher; material and rendering stay separate graph nodes.",
    examples: ["WaterBasin"],
    picker: { label: "Liquid Surface", category: Atom },
    summary: "Simulate liquid, a pouring source and a moving box, and generate a surface for scene rendering.",
    category: Geometry3D, role: Source,
    aliases: ["water", "liquid", "fluid", "FLIP", "APIC"],
    boundary_reason: IoBridge,
    extra_fields: {
        runtime: FluidRuntime = FluidRuntime::default(),
        upload: FluidMeshUpload = FluidMeshUpload::default(),
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
            ("surface_subdivisions", 0.0),
            ("emission", 1.0),
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
        let settings = FluidSettings {
            resolution: ctx.scalar_or_param("resolution", 24.0).round() as u32,
            domain_size: ctx.scalar_or_param("domain_size", 4.0),
            fill_height: ctx.scalar_or_param("fill_height", 0.4),
            surface_subdivisions: ctx.scalar_or_param("surface_subdivisions", 0.0).round() as u32,
            apic: matches!(ctx.params.get("transfer"), Some(ParamValue::Enum(1))),
            max_vertices: (ctx
                .param_f32("max_capacity", 786432.0)
                .clamp(3.0, 3145728.0) as usize
                / 3)
                * 3,
        };
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
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        let retained = ctx.outputs_retained();
        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return;
        };
        match self.upload.upload(
            gpu,
            dst,
            &self.runtime.vertices,
            self.runtime.version,
            retained,
        ) {
            Ok(false) if self.last_lag == lag.to_bits() => ctx.mark_outputs_unchanged(),
            Ok(_) => {}
            Err(error) => Self::report_failure(ctx, error.into()),
        }
        self.last_lag = lag.to_bits();
    }
}
