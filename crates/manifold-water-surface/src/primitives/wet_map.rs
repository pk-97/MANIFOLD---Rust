//! `node.wet_map` — where and when the water touched a surface, as one
//! top-down map over the water's object-space XZ (the layout is
//! `manifold_node_engine::scene::wet_map`). Each frame every liquid particle
//! near a solid stamps its texel with the display time and the height it
//! touched; the mesh shaders read the map and dry it by the age of the last
//! touch, so the map costs one pass over the particles a frame and nothing
//! while the water is gone.

use std::borrow::Cow;
use std::sync::LazyLock;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::wet_map::{LAYOUT, SIDE, map_bytes};
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;

// The read side and the map's layout, then the kernels.
static SHADER: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{LAYOUT}\n{}",
        include_str!("shaders/wet_map.wgsl")
    )
});
const MARK_THREADS: u32 = 64;

/// The texel a simulation cell asks for, metres: a quarter cell, so a splash
/// edge is finer than the surface drawn over it, kept between 5 mm and 50 mm.
/// The placed map may take a coarser texel to hold a wide spread.
fn base_texel(sim_cell_size: f32) -> f32 {
    (sim_cell_size / 4.0).clamp(0.005, 0.05)
}

/// Field order identical to the shader's `Params`; two 16-byte rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct WetMapUniforms {
    now: f32,
    drying: f32,
    base_texel: f32,
    restart: u32,
    side: u32,
    capacity: u32,
    pool_tiles: u32,
    cell: f32,
}

/// What the last run saw: the map carries on from it only at the same epoch
/// and texel, with time standing still or moving on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LastRun {
    time: f32,
    epoch: Option<f32>,
    base_texel: f32,
}

fn restarts(last: Option<LastRun>, now: LastRun) -> bool {
    last.is_none_or(|last| {
        last.epoch != now.epoch || last.base_texel != now.base_texel || now.time < last.time
    })
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

manifold_node_engine::primitive! {
    name: WetMap,
    type_id: "node.wet_map",
    purpose: "Where and when the water touched a surface, as one top-down map of 1024 x 1024 texels over the water's object-space XZ, kept between frames. Each run every liquid particle within two radii of the solid (its trilinear distance on the display tiles) stamps the texel of the surface point it touches (back along the solid's normal by that distance) with the display time, widens the texel's lowest and highest touched heights and sets the bit of that height's bin (bins two texels tall, wrapping every 32). The map is read by world position: a texel is wet only within two texels of a height it was touched at, so the underside of an overhang the water passed under, and a wall between a splash at its foot and one at its top, stay dry; its wetness is exp(-age * drying / 15 s) since the last touch, so it dries without any pass over the map. The texel is a quarter simulation cell (5 mm to 50 mm), or coarser when the water spreads wider than the map holds at that size. The map is placed centred on the water's extent at twice its size, and placed again, cleared, when the water leaves it. It starts over, cleared, on its first run, when epoch changes, when the texel changes and when time goes backwards.",
    inputs: {
        particles: Array(FluidParticle) required,
        solid: Array(f32) required,
        tiles: Array(u32) required,
        extent: Array(f32) required,
        time: ScalarF32 required,
        epoch: ScalarF32 optional,
        drying: ScalarF32 optional,
        sim_cell_size: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        map: Array(u32),
    },
    params: [
        // Times the stage drying rate: 1 dries a splash in about half a minute, 0 never.
        float_param!("drying", "Drying", 1.0, 0.0, 1000.0),
        float_param!("sim_cell_size", "Water Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire particles from the drawn liquid particles, solid from the display solid, tiles, pool_tiles and cell_size from node.surface_tiles with extent from its extent, sim_cell_size from the water's cell size, time from the domain's display_time and epoch from its epoch; its map feeds the water object's wet_map. The map persists between frames, so keep the node running while the water is shown.",
    examples: [],
    picker: { label: "Wet Map", category: Atom },
    summary: "Remembers where the water touched the ground and walls, so they look wet and dry off over time.",
    category: Particles3D,
    role: Filter,
    aliases: ["wet map", "wetness", "wet surfaces"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        kernels: Option<Kernels> = None,
        map: Option<GpuBuffer> = None,
        clear_groups: Option<GpuBuffer> = None,
        last: Option<LastRun> = None,
    },
}

/// The node's three passes, one pipeline each.
struct Kernels {
    place: GpuComputePipeline,
    clear: GpuComputePipeline,
    mark: GpuComputePipeline,
}

impl Kernels {
    fn new(device: &GpuDevice) -> Self {
        let make =
            |entry: &str| device.create_compute_pipeline(SHADER.as_str(), entry, "node.wet_map");
        Self {
            place: make("cs_place"),
            clear: make("cs_clear"),
            mark: make("cs_mark"),
        }
    }
}

impl WetMap {
    /// The map and the clear's group count, made once and zeroed: a zeroed
    /// map is unplaced and dry.
    fn ensure_state(&mut self, device: &GpuDevice) -> Result<(), String> {
        if self.map.is_some() {
            return Ok(());
        }
        let bytes = map_bytes(SIDE);
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            bytes + 16,
        )
        .map_err(|error| error.to_string())?;
        let map = device.try_create_buffer_shared(bytes)?;
        map.zero_fill();
        let groups = device.try_create_buffer_shared(16)?;
        groups.zero_fill();
        self.map = Some(map);
        self.clear_groups = Some(groups);
        Ok(())
    }
}

impl Primitive for WetMap {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.kernels.get_or_insert_with(|| Kernels::new(device));
    }

    fn provides_array_output(&self, port: &str) -> bool {
        port == "map"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "map" => self.map.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, made at the first run.
        (port == "map").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Wet Map") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(particles), Some(solid), Some(tiles), Some(extent)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.inputs.array("extent"),
        ) else {
            ctx.error("Wet Map: particles, solid, tiles and extent must be wired");
            return;
        };
        let Some(ParamValue::Float(time)) = ctx.inputs.scalar("time") else {
            ctx.error("Wet Map: time must be wired");
            return;
        };
        if !time.is_finite() {
            ctx.error("Wet Map: time must be finite");
            return;
        }
        let drying = ctx.scalar_or_param("drying", 1.0);
        if !(drying.is_finite() && drying >= 0.0) {
            ctx.error("Wet Map: drying must be finite and at least 0");
            return;
        }
        let sim_cell = ctx.scalar_or_param("sim_cell_size", 0.0625);
        if !(sim_cell.is_finite() && sim_cell > 0.0) {
            ctx.error(format!(
                "Wet Map: the water's cell size {sim_cell} is not a length"
            ));
            return;
        }
        if solid.size < pool.elements() * 4 || extent.size < 32 {
            ctx.error(format!(
                "Wet Map: solid holds {} values and extent {} for a pool of {} tiles; it needs {} and 8",
                solid.size / 4,
                extent.size / 4,
                pool.pool_tiles,
                pool.elements()
            ));
            return;
        }
        let epoch = match ctx.inputs.scalar("epoch") {
            Some(ParamValue::Float(epoch)) => Some(epoch),
            _ => None,
        };
        let run = LastRun {
            time,
            epoch,
            base_texel: base_texel(sim_cell),
        };
        let Ok(capacity) = u32::try_from(particles.size / size_of::<FluidParticle>() as u64) else {
            ctx.error("Wet Map: particles holds more records than a pass indexes");
            return;
        };
        let uniforms = WetMapUniforms {
            now: time,
            drying,
            base_texel: run.base_texel,
            restart: u32::from(restarts(self.last, run)),
            side: SIDE,
            capacity,
            pool_tiles: pool.pool_tiles,
            cell: pool.cell_size,
        };
        let prepared = {
            let gpu = ctx.gpu_encoder();
            self.kernels.get_or_insert_with(|| Kernels::new(gpu.device));
            self.ensure_state(gpu.device)
        };
        if let Err(error) = prepared {
            ctx.error(format!(
                "Wet Map: the map needs storage the device cannot give: {error}"
            ));
            return;
        }
        let (Some(kernels), Some(map), Some(groups)) = (
            self.kernels.as_ref(),
            self.map.as_ref(),
            self.clear_groups.as_ref(),
        ) else {
            return;
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            buffer(1, particles),
            buffer(2, solid),
            buffer(3, tiles),
            buffer(4, extent),
            buffer(5, map),
            buffer(6, groups),
        ];
        let enc = &mut *ctx.gpu_encoder().native_enc;
        enc.dispatch_compute(&kernels.place, &bindings, [1, 1, 1], "node.wet_map.place");
        enc.compute_memory_barrier_buffers();
        enc.dispatch_compute_indirect(&kernels.clear, &bindings, groups, 0, "node.wet_map.clear");
        enc.compute_memory_barrier_buffers();
        if capacity > 0 {
            enc.dispatch_compute(
                &kernels.mark,
                &bindings,
                [capacity.div_ceil(MARK_THREADS), 1, 1],
                "node.wet_map.mark",
            );
        }
        self.last = Some(run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("wet map shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("wet map shader validates");
        for entry in ["cs_place", "cs_clear", "cs_mark"] {
            assert!(
                module.entry_points.iter().any(|point| point.name == entry),
                "missing {entry}"
            );
        }
        assert_eq!(std::mem::size_of::<WetMapUniforms>(), 32);
    }

    #[test]
    fn the_texel_is_a_quarter_cell_within_its_bounds() {
        assert_eq!(base_texel(0.0625), 0.015625);
        assert_eq!(base_texel(0.004), 0.005);
        assert_eq!(base_texel(0.9), 0.05);
    }

    #[test]
    fn the_map_starts_over_on_a_new_epoch_texel_or_time_running_back() {
        let at = |time, epoch, base_texel| LastRun {
            time,
            epoch,
            base_texel,
        };
        let last = Some(at(4.0, Some(1.0), 0.01));
        assert!(restarts(None, at(0.0, None, 0.01)));
        assert!(!restarts(last, at(4.0, Some(1.0), 0.01)));
        assert!(!restarts(last, at(9.0, Some(1.0), 0.01)));
        assert!(restarts(last, at(3.9, Some(1.0), 0.01)));
        assert!(restarts(last, at(9.0, Some(2.0), 0.01)));
        assert!(restarts(last, at(9.0, Some(1.0), 0.02)));
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
