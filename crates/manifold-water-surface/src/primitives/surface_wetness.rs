//! `node.surface_wetness` — per display node, how wet the surface is, 0 to 1.
//! Everything the water touches is wet, and dries at the rate a thin film
//! evaporates. The node's buffer is the state, stored per slot of the
//! display's world tiles: it persists between frames, and starts over when
//! the display time runs backwards or a slot takes another tile.

use std::borrow::Cow;
use std::sync::LazyLock;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, slot_row_word, table_entries};
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::{TilePool, tile_pool};

// The read side, then the kernel.
static SHADER: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/surface_wetness.wgsl")
    )
});
const THREADS: u32 = 64;

/// The film a drained smooth surface holds, about 0.1 mm, in metres.
const WET_FILM_METRES: f32 = 1.0e-4;
/// Reference evaporation in a mild climate, 5 mm a day (FAO-56), in metres per second.
const EVAPORATION_METRES_PER_SECOND: f32 = 5.8e-8;
/// The time the film takes to evaporate at still-air rate, about 29 minutes.
const DRY_SECONDS: f32 = WET_FILM_METRES / EVAPORATION_METRES_PER_SECOND;

/// Field order identical to the shader's `Params`; one 16-byte row.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct WetnessUniforms {
    pool_tiles: u32,
    elements: u32,
    h: f32,
    decay: f32,
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

/// The fraction of the film still on after `elapsed` seconds at `drying` times
/// still-air evaporation; 0 when the node starts over (`None`), so nothing
/// carries and only the current contact is wet.
fn film_left(elapsed: Option<f32>, drying: f32) -> f32 {
    elapsed.map_or(0.0, |dt| {
        (-f64::from(dt) * f64::from(drying) / f64::from(DRY_SECONDS)).exp() as f32
    })
}

manifold_node_engine::primitive! {
    name: SurfaceWetness,
    type_id: "node.surface_wetness",
    purpose: "Per node of the display's world tiles, how wet the surface is, 0 to 1, kept between frames. Where the water's level set at the node is below one display cell (water at the node or within a cell of it) the node is wet, 1. Elsewhere it dries as a thin film evaporates: wetness times exp(-dt * drying / 1724 s) each run, 1724 s being a 0.1 mm film over 5 mm a day of evaporation (FAO-56). It is stored per slot of the tiles, so a slot that holds another tile than last frame, or was free, starts dry. The node starts over (only the current contact is wet) on its first run, when epoch changes, when the pool or cell size changes, and when time goes backwards.",
    inputs: {
        field: Array(f32) required,
        tiles: Array(u32) required,
        time: ScalarF32 required,
        epoch: ScalarF32 optional,
        drying: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        wetness: Array(f32),
    },
    params: [
        // Times still-air evaporation: 1 dries a soaked floor in about half an hour, 100 in about 17 s, 0 never.
        float_param!("drying", "Drying", 1.0, 0.0, 1000.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire field from the display's level set, tiles, pool_tiles and cell_size from node.surface_tiles, time from the domain's display_time and epoch from its epoch; its wetness feeds the water object's field_wetness. The wetness persists between frames, so keep the node running while the water is shown. Wet tiles stay in the table for node.surface_tiles' Keep Seconds, which should cover how long the film takes to dry.",
    examples: [],
    picker: { label: "Surface Wetness", category: Atom },
    summary: "Remembers how wet the surface is on the display's tiles: soaked where the water touches, drying over time.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface wetness", "wet surfaces", "wetness"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        wet_pipeline: Option<GpuComputePipeline> = None,
        wetness: Option<GpuBuffer> = None,
        slot_rows: Option<GpuBuffer> = None,
        last_time: Option<f32> = None,
        last_epoch: Option<f32> = None,
        last_pool: Option<TilePool> = None,
    },
}

impl SurfaceWetness {
    /// The wetness and last frame's slot rows for `pool`: new zeroed storage
    /// when the pool's size changes, never otherwise. A zeroed wetness holds
    /// nothing to carry, whatever its slot rows say.
    fn ensure_state(&mut self, device: &GpuDevice, pool: TilePool) -> Result<(), String> {
        let wetness_bytes = pool.elements() * 4;
        if self
            .wetness
            .as_ref()
            .is_some_and(|buffer| buffer.size == wetness_bytes)
        {
            return Ok(());
        }
        let rows_bytes = 16 * u64::from(pool.pool_tiles);
        self.wetness = None;
        self.slot_rows = None;
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            wetness_bytes + rows_bytes,
        )
        .map_err(|error| error.to_string())?;
        let wetness = device.try_create_buffer_shared(wetness_bytes)?;
        wetness.zero_fill();
        let rows = device.try_create_buffer_shared(rows_bytes)?;
        rows.zero_fill();
        self.wetness = Some(wetness);
        self.slot_rows = Some(rows);
        Ok(())
    }
}

impl Primitive for SurfaceWetness {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "wetness"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "wetness" => self.wetness.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        if port != "wetness" {
            return None;
        }
        inputs
            .iter()
            .find(|(name, _)| *name == "field")
            .map(|&(_, n)| n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Surface Wetness") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(field), Some(tiles)) = (ctx.inputs.array("field"), ctx.inputs.array("tiles"))
        else {
            ctx.error("Surface Wetness: field and tiles must be wired");
            return;
        };
        let Some(ParamValue::Float(time)) = ctx.inputs.scalar("time") else {
            ctx.error("Surface Wetness: time must be wired");
            return;
        };
        if !time.is_finite() {
            ctx.error("Surface Wetness: time must be finite");
            return;
        }
        let drying = ctx.scalar_or_param("drying", 1.0);
        if !(drying.is_finite() && drying >= 0.0) {
            ctx.error("Surface Wetness: drying must be finite and at least 0");
            return;
        }
        let epoch = match ctx.inputs.scalar("epoch") {
            Some(ParamValue::Float(epoch)) => Some(epoch),
            _ => None,
        };
        let elements = pool.elements();
        let Ok(count) = u32::try_from(elements) else {
            ctx.error(format!(
                "Surface Wetness: a pool of {} tiles has more elements than an index can address",
                pool.pool_tiles
            ));
            return;
        };
        if field.size < elements * 4 {
            ctx.error(format!(
                "Surface Wetness: field holds {} values; a pool of {} tiles needs {elements}",
                field.size / 4,
                pool.pool_tiles
            ));
            return;
        }
        // The wetness carries on only from a run of the same pool and cell
        // size, the same epoch and an earlier or equal time; anything else
        // starts over.
        let same_pool = self.last_pool == Some(pool);
        let elapsed = self
            .last_time
            .filter(|&last| same_pool && epoch == self.last_epoch && time >= last)
            .map(|last| time - last);
        let uniforms = WetnessUniforms {
            pool_tiles: pool.pool_tiles,
            elements: count,
            h: pool.cell_size,
            decay: film_left(elapsed, drying),
        };
        let prepared = {
            let gpu = ctx.gpu_encoder();
            if self.wet_pipeline.is_none() {
                self.wet_pipeline = Some(gpu.device.create_compute_pipeline(
                    SHADER.as_str(),
                    "cs_wetness",
                    "node.surface_wetness",
                ));
            }
            self.ensure_state(gpu.device, pool)
        };
        if let Err(error) = prepared {
            ctx.error(format!(
                "Surface Wetness: a pool of {} tiles needs storage the device cannot give: {error}",
                pool.pool_tiles
            ));
            return;
        }
        let (Some(pipeline), Some(wetness), Some(rows)) = (
            self.wet_pipeline.as_ref(),
            self.wetness.as_ref(),
            self.slot_rows.as_ref(),
        ) else {
            return;
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                buffer(1, field),
                buffer(2, tiles),
                buffer(3, rows),
                buffer(4, wetness),
            ],
            [count.div_ceil(THREADS), 1, 1],
            "node.surface_wetness",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        // Next frame's slots are compared with this frame's tiles.
        gpu.native_enc.copy_buffer_range(
            tiles,
            slot_row_word(table_entries(pool.pool_tiles), 0) * 4,
            rows,
            0,
            rows.size,
        );
        self.last_time = Some(time);
        self.last_epoch = epoch;
        self.last_pool = Some(pool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("surface wetness shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("surface wetness shader validates");
        assert!(
            module
                .entry_points
                .iter()
                .any(|point| point.name == "cs_wetness")
        );
        assert_eq!(std::mem::size_of::<WetnessUniforms>(), 16);
    }

    #[test]
    fn drying_time_is_the_film_over_the_evaporation_rate() {
        assert!((DRY_SECONDS - 1724.1).abs() < 0.1, "{DRY_SECONDS}");
    }

    #[test]
    fn the_film_left_follows_the_drying_rate_and_a_restart_leaves_none() {
        let want = (-30.0_f64 * 4.0 / f64::from(DRY_SECONDS)).exp() as f32;
        assert_eq!(film_left(Some(30.0), 4.0), want);
        assert_eq!(film_left(Some(0.0), 4.0), 1.0);
        // No drying never dries; a restart carries nothing whatever the rate.
        assert_eq!(film_left(Some(600.0), 0.0), 1.0);
        assert_eq!(film_left(None, 4.0), 0.0);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
