//! Engine whitewater surface distance orchestration. The stencil is the
//! registered codegen atom UpwindDistance; reduction is stage-local.
//! FLIP Fluids particlelevelset.cpp / levelsetsolver.cpp, MIT; see notices.
use {manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline, super::upwind_distance::UpwindDistance, super::upwind_distance::UpwindUniforms};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

/// gpu_flip_step.wgsl `ClockPlan`.
const PLAN_BYTES: u64 = 48;

pub fn scratch_bytes(cells: [u32; 3]) -> u64 {
    let n = cells.into_iter().map(u64::from).product::<u64>();
    let blocks = cells
        .into_iter()
        .map(|n| u64::from(n.div_ceil(6)))
        .product::<u64>();
    // Fields, block flags, convergence state, sweep grid, zero clock plan.
    12 * n + 4 * blocks + 16 + 16 + PLAN_BYTES
}
#[derive(Default)]
pub struct SurfaceDistance {
    pipelines: Vec<GpuComputePipeline>,
    sweep: Option<GpuComputePipeline>,
    buffers: Option<[GpuBuffer; 7]>,
    cells: [u32; 3],
}
impl SurfaceDistance {
    /// current, candidate, valid, blocks, state, sweep grid, zero plan.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn scratch(&self) -> &[GpuBuffer; 7] {
        self.buffers.as_ref().expect("distance reserved")
    }
    pub fn prepare(&mut self, device: &GpuDevice) {
        standalone_pipeline::<UpwindDistance>(&mut self.sweep, device);
        if self.pipelines.is_empty() {
            for entry in [
                "mark_blocks",
                "initialize",
                "reset",
                "reduce",
                "decide",
                "accept",
                "finish",
                "gate",
            ] {
                self.pipelines.push(device.create_compute_pipeline(
                    include_str!("shaders/whitewater_distance.wgsl"),
                    entry,
                    "whitewater.distance",
                ));
            }
        }
    }
    pub fn reserve(&mut self, device: &GpuDevice, cells: [u32; 3]) -> Result<(), String> {
        if self.buffers.is_some() && self.cells == cells {
            return Ok(());
        }
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            scratch_bytes(cells),
        )
        .map_err(|e| e.to_string())?;
        let bytes = cells.into_iter().map(u64::from).product::<u64>() * 4;
        let blocks = cells
            .into_iter()
            .map(|n| u64::from(n.div_ceil(6)))
            .product::<u64>()
            * 4;
        self.buffers = Some([
            device.try_create_buffer(bytes)?,
            device.try_create_buffer(bytes)?,
            device.try_create_buffer(bytes)?,
            device.try_create_buffer(blocks)?,
            device.try_create_buffer(16)?,
            device.try_create_buffer(16)?,
            {
                let zeros = device.try_create_buffer_shared(PLAN_BYTES)?;
                zeros.zero_fill();
                zeros
            },
        ]);
        self.cells = cells;
        Ok(())
    }
    /// Always active: a zero clock plan.
    pub fn encode(&self, enc: &mut GpuEncoder, source: &GpuBuffer, h: f32) -> &GpuBuffer {
        let zeros = &self.buffers.as_ref().expect("distance reserved")[6];
        self.encode_gated(enc, source, h, zeros)
    }

    /// Gated by the FLIP clock plan, so it can sit in a substep slot: an
    /// inactive slot (live clock, zero step) leaves the output and every
    /// field and state word as they were, and only rewrites the sweep grid
    /// to zero groups.
    pub fn encode_gated(&self, enc: &mut GpuEncoder, source: &GpuBuffer, h: f32, plan: &GpuBuffer) -> &GpuBuffer {
        let [current, candidate, valid, blocks, state, args, _] =
            self.buffers.as_ref().expect("distance reserved");
        let [nx, ny, nz] = self.cells;
        let count = nx * ny * nz;
        let pass = |enc: &mut GpuEncoder, entry: usize, iteration: u32, work: u32| {
            let words = [
                nx,
                ny,
                nz,
                count,
                h.to_bits(),
                iteration,
                nx.div_ceil(6),
                ny.div_ceil(6),
            ];
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
                        buffer: blocks,
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
                ],
                [work, 1, 1],
                "whitewater.distance.control",
            );
            enc.compute_memory_barrier_buffers();
        };
        // The generated sweep cannot read the plan, so it runs over the
        // grid `gate` writes: count/256 groups when active, none otherwise.
        pass(enc, 7, 0, 1);
        pass(
            enc,
            0,
            0,
            (nx.div_ceil(6) * ny.div_ceil(6) * nz.div_ceil(6)).div_ceil(256),
        );
        pass(enc, 1, 0, count.div_ceil(256));
        for iteration in 0..6 {
            pass(enc, 2, iteration, 1);
            let uniforms = UpwindUniforms::new([nx, ny, nz].map(|n| n as f32), h, count);
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

/// The surface distance kernel, for consumers that validate or embed it.
pub const WHITEWATER_DISTANCE_SHADER: &str = include_str!("shaders/whitewater_distance.wgsl");
