//! Engine whitewater surface distance orchestration over pooled cell arrays on
//! the liquid step's world tiles. The stencil is the registered codegen atom
//! UpwindDistance; reduction is stage-local.
//! FLIP Fluids particlelevelset.cpp / levelsetsolver.cpp, MIT; see notices.
use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};
use manifold_node_engine::scene::world_tiles::{READ_SIDE, pool_len, table_entries};
use {
    super::upwind_distance::UpwindDistance, super::upwind_distance::UpwindUniforms,
    manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline,
};

/// gpu_flip_step/common.wgsl `ClockPlan`.
const PLAN_BYTES: u64 = 48;

/// The stage's scratch for a pool of `pool_tiles` tiles.
pub fn scratch_bytes(pool_tiles: u32) -> u64 {
    // Fields, slot flags, convergence state, sweep grid, zero clock plan.
    12 * pool_len(pool_tiles) + 4 * u64::from(pool_tiles) + 16 + 16 + PLAN_BYTES
}

/// The stage's kernels: the world tiles' read side, then the passes.
fn distance_source() -> String {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/whitewater_distance.wgsl")
    )
}

/// The surface distance kernels, for consumers that validate or embed them.
pub fn whitewater_distance_shader() -> String {
    distance_source()
}

#[derive(Default)]
pub struct SurfaceDistance {
    pipelines: Vec<GpuComputePipeline>,
    sweep: Option<GpuComputePipeline>,
    buffers: Option<[GpuBuffer; 7]>,
    pool_tiles: u32,
}
impl SurfaceDistance {
    /// current, candidate, valid, slot flags, state, sweep grid, zero plan.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn scratch(&self) -> &[GpuBuffer; 7] {
        self.buffers.as_ref().expect("distance reserved")
    }
    pub fn prepare(&mut self, device: &GpuDevice) {
        standalone_pipeline::<UpwindDistance>(&mut self.sweep, device);
        if self.pipelines.is_empty() {
            let source = distance_source();
            for entry in [
                "mark_slots",
                "initialize",
                "reset",
                "reduce",
                "decide",
                "accept",
                "finish",
                "gate",
            ] {
                self.pipelines.push(device.create_compute_pipeline(
                    &source,
                    entry,
                    "whitewater.distance",
                ));
            }
        }
    }
    /// Room for the distance of a pool of `pool_tiles` tiles.
    pub fn reserve(&mut self, device: &GpuDevice, pool_tiles: u32) -> Result<(), String> {
        if self.buffers.is_some() && self.pool_tiles == pool_tiles {
            return Ok(());
        }
        manifold_node_engine::load::expand::admit_world_bytes(
            device.modifier_memory_snapshot(),
            scratch_bytes(pool_tiles),
        )
        .map_err(|e| e.to_string())?;
        let bytes = pool_len(pool_tiles) * 4;
        let flags = u64::from(pool_tiles) * 4;
        self.buffers = Some([
            device.try_create_buffer(bytes, BufferUse::Gpu)?,
            device.try_create_buffer(bytes, BufferUse::Gpu)?,
            device.try_create_buffer(bytes, BufferUse::Gpu)?,
            device.try_create_buffer(flags, BufferUse::Gpu)?,
            device.try_create_buffer(16, BufferUse::Gpu)?,
            device.try_create_buffer(16, BufferUse::Gpu)?,
            {
                let zeros = device.try_create_buffer(PLAN_BYTES, BufferUse::Upload)?;
                zeros.zero_fill();
                zeros
            },
        ]);
        self.pool_tiles = pool_tiles;
        Ok(())
    }
    /// Always active: a zero clock plan. `source` is a pooled cell array of
    /// the pool's `pool_len(pool_tiles)` elements under the table `tiles`; the
    /// output is a pooled array on the same table, 5h where a cell is out of
    /// the band and in the canonical element.
    pub fn encode(
        &self,
        enc: &mut GpuEncoder,
        source: &GpuBuffer,
        tiles: &GpuBuffer,
        pool_tiles: u32,
        h: f32,
    ) -> &GpuBuffer {
        let zeros = &self.buffers.as_ref().expect("distance reserved")[6];
        self.encode_gated(enc, source, tiles, pool_tiles, h, zeros)
    }

    /// Gated by the FLIP clock plan, so it can sit in a substep slot: an
    /// inactive slot (live clock, zero step) leaves the output and every
    /// field and state word as they were, and only rewrites the sweep grid
    /// to zero groups.
    pub fn encode_gated(
        &self,
        enc: &mut GpuEncoder,
        source: &GpuBuffer,
        tiles: &GpuBuffer,
        pool_tiles: u32,
        h: f32,
        plan: &GpuBuffer,
    ) -> &GpuBuffer {
        assert_eq!(
            pool_tiles, self.pool_tiles,
            "distance reserved for this pool"
        );
        let [current, candidate, valid, slot_flags, state, args, _] =
            self.buffers.as_ref().expect("distance reserved");
        let count = pool_len(pool_tiles) as u32;
        let entries = table_entries(pool_tiles);
        let pass = |enc: &mut GpuEncoder, entry: usize, iteration: u32, work: u32| {
            let words = [count, pool_tiles, entries, iteration, h.to_bits(), 0, 0, 0];
            enc.dispatch_compute(
                &self.pipelines[entry],
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&words),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: source,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: current,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: candidate,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: valid,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 5,
                        buffer: slot_flags,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 6,
                        buffer: state,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 7,
                        buffer: plan,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 8,
                        buffer: args,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 9,
                        buffer: tiles,
                        offset: 0,
                    },
                ],
                [work, 1, 1],
                "whitewater.distance.control",
            );
            enc.compute_memory_barrier_buffers();
        };
        // The generated sweep cannot read the plan, so it runs over the
        // grid `gate` writes: count/256 groups when active, none otherwise.
        pass(enc, 7, 0, 1);
        pass(enc, 0, 0, pool_tiles.div_ceil(256));
        pass(enc, 1, 0, count.div_ceil(256));
        for iteration in 0..6 {
            pass(enc, 2, iteration, 1);
            let uniforms = UpwindUniforms::new(h, pool_tiles, count);
            enc.dispatch_compute_indirect(
                self.sweep.as_ref().expect("sweep prepared"),
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: current,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: valid,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: tiles,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: candidate,
                        offset: 0,
                    },
                ],
                args,
                0,
                "whitewater.distance.upwind",
            );
            enc.compute_memory_barrier_buffers();
            pass(enc, 3, iteration, count.div_ceil(256));
            // Commit the sweep before testing convergence, like the engine
            // swapping tempPtr into outputPtr before its stopping test.
            pass(enc, 5, iteration, count.div_ceil(256));
            pass(enc, 4, iteration, 1);
        }
        pass(enc, 6, 0, count.div_ceil(256));
        current
    }
}
