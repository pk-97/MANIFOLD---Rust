//! `node.matter_water_push` — the water lifts and drags the Matter once a
//! tick (`shaders/matter_water_push.wgsl`), from the liquid step's water as
//! its last tick left it. In the World's tick loop the Matter steps each tick
//! before the water (the water reads the grains of the same tick), so the
//! water arrives through state-capture inputs: what the step held when its
//! last tick ended.

use std::borrow::Cow;
use std::sync::LazyLock;

use manifold_core::liquid_domain::WATER_PUSH_STEP;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::wet_map::DRY_SECONDS;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, table_words};
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::grid::LIQUID_FACES;
use manifold_water_liquid::whitewater::WHITEWATER_COMMON;

use super::matter_common::MATTER_MATERIAL;
use crate::matter::MatterPoint;

static SHADER: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{LIQUID_FACES}\n{WHITEWATER_COMMON}\n{MATTER_MATERIAL}\n{}",
        include_str!("shaders/matter_water_push.wgsl")
    )
});
const THREADS: u32 = 256;

/// Field order identical to the shader's `Params`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct WaterPushUniforms {
    count: u32,
    pool_tiles: u32,
    cell: f32,
    dt: f32,
    gravity: [f32; 3],
    dry: f32,
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

manifold_node_engine::primitive! {
    name: MatterWaterPush,
    type_id: "node.matter_water_push",
    purpose: "Once a tick, at its first substep, the water lifts and drags each live point in place: the water's velocity (FLIP's MAC trilinear on the step's pooled faces) and the point's share under water (the step's distance trilinear at cell centres, 1 half a cell inside the surface, 0 half a cell out) at the point. Under water the point loses its material row's lift share of its gravity and moves toward the water's velocity at the row's drag rate, exact over the tick; it keeps how wet it is and its share under water now. A point whose cell the water's tiles do not hold is under no water. The step's faces, distance, tiles and pool_tiles are state-capture inputs: the step as its last tick left it.",
    inputs: {
        points: Array(MatterPoint) required,
        active_count: ScalarF32 optional,
        tick_start: ScalarF32 optional,
        interval_duration: ScalarF32 optional,
        materials: Array(f32) optional,
        gravity_x: ScalarF32 optional,
        gravity: ScalarF32 optional,
        gravity_z: ScalarF32 optional,
        faces: Array(FaceSample) optional,
        distance: Array(f32) optional,
        tiles: Array(u32) optional,
        pool_tiles: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        points_out: Array(MatterPoint),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 0.0, 0.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, between node.matter_state's out and node.matter_to_grid's points; in place, points_out is points. tick_start from node.matter_state, interval_duration (the tick's seconds) and gravity from node.matter_domain, active_count from node.matter_fill, materials from node.matter_material. faces, distance, tiles and pool_tiles from the World's water step (node.gpu_flip_step), which steps after the Matter in each tick, and cell_size from the step's own cell size.",
    examples: [],
    picker: { label: "Matter Water Push", category: Atom },
    summary: "Lets the water lift and drag the sand, once a tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["water on sand", "sand drag", "sand push", "water push"],
    extra_fields: {
        kernel: Option<GpuComputePipeline> = None,
    },
}

impl MatterWaterPush {
    fn kernel(&mut self, device: &GpuDevice) -> &GpuComputePipeline {
        self.kernel.get_or_insert_with(|| {
            device.create_compute_pipeline(SHADER.as_str(), "water_push", "node.matter_water_push")
        })
    }
}

impl Primitive for MatterWaterPush {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.kernel(device);
    }

    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &WATER_PUSH_STEP
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("points", "points_out")]
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "points_out")
            .then(|| input_capacities.iter().find(|(p, _)| *p == "points"))
            .flatten()
            .map(|&(_, n)| n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let tick_start = ctx.scalar_or_param("tick_start", 0.0) > 0.5;
        let count = ctx.scalar_or_param("active_count", 0.0).round().max(0.0) as u32;
        let pool_tiles = ctx.scalar_or_param("pool_tiles", 0.0).round();
        let cell = ctx.scalar_or_param("cell_size", 0.0625);
        let dt = ctx.scalar_or_param("interval_duration", 0.0);
        let gravity = [("gravity_x", 0.0), ("gravity", -9.81), ("gravity_z", 0.0)]
            .map(|(name, default)| ctx.scalar_or_param(name, default));
        let (Some(points), Some(materials), Some(faces), Some(distance), Some(tiles)) = (
            ctx.inputs.array("points"),
            ctx.inputs.array("materials"),
            ctx.inputs.array("faces"),
            ctx.inputs.array("distance"),
            ctx.inputs.array("tiles"),
        ) else {
            return;
        };
        let gpu = ctx.gpu_encoder();
        let positive = |v: f32| v > 0.0 && v.is_finite();
        if !tick_start || count == 0 || !positive(dt) || !positive(cell) {
            return;
        }
        // Before the water's first tick its storage holds no pool: nothing
        // pushes yet.
        if !(1.0..=16_777_216.0).contains(&pool_tiles) {
            return;
        }
        let pool_tiles = pool_tiles as u32;
        let elements = pool_len(pool_tiles);
        if faces.size < elements * size_of::<FaceSample>() as u64
            || distance.size < elements * 4
            || tiles.size < table_words(pool_tiles) * 4
        {
            return;
        }
        let count = count.min((points.size / size_of::<MatterPoint>() as u64) as u32);
        let uniforms = WaterPushUniforms {
            count,
            pool_tiles,
            cell,
            dt,
            gravity,
            dry: DRY_SECONDS,
        };
        let kernel = self.kernel(gpu.device);
        gpu.native_enc.dispatch_compute(
            kernel,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                buffer(1, points),
                buffer(2, faces),
                buffer(3, distance),
                buffer(4, tiles),
                buffer(5, materials),
            ],
            [count.div_ceil(THREADS), 1, 1],
            "node.matter_water_push",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("water push shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("water push shader validates");
        assert_eq!(size_of::<WaterPushUniforms>(), 32);
    }
}
