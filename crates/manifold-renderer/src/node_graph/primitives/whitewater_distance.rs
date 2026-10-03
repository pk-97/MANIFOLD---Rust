//! Engine whitewater surface distance orchestration. The stencil is the
//! registered codegen atom UpwindDistance; reduction is stage-local.
//! FLIP Fluids particlelevelset.cpp / levelsetsolver.cpp, MIT; see notices.
use super::{standalone_pipeline::standalone_pipeline, upwind_distance::UpwindDistance};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

pub(crate) fn scratch_bytes(cells: [u32; 3]) -> u64 {
    let n = cells.into_iter().map(u64::from).product::<u64>();
    let blocks = cells
        .into_iter()
        .map(|n| u64::from(n.div_ceil(6)))
        .product::<u64>();
    12 * n + 4 * blocks + 16
}
#[derive(Default)]
pub(crate) struct SurfaceDistance {
    pipelines: Vec<GpuComputePipeline>,
    sweep: Option<GpuComputePipeline>,
    buffers: Option<[GpuBuffer; 5]>,
    cells: [u32; 3],
}
impl SurfaceDistance {
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
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
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
        ]);
        self.cells = cells;
        Ok(())
    }
    pub fn encode(&self, enc: &mut GpuEncoder, source: &GpuBuffer, h: f32) -> &GpuBuffer {
        let [current, candidate, valid, blocks, state] =
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
                ],
                [work, 1, 1],
                "whitewater.distance.control",
            );
            enc.compute_memory_barrier_buffers();
        };
        pass(
            enc,
            0,
            0,
            (nx.div_ceil(6) * ny.div_ceil(6) * nz.div_ceil(6)).div_ceil(256),
        );
        pass(enc, 1, 0, count.div_ceil(256));
        for iteration in 0..6 {
            pass(enc, 2, iteration, 1);
            let words = [
                (nx as f32).to_bits(),
                (ny as f32).to_bits(),
                (nz as f32).to_bits(),
                h.to_bits(),
                count,
                0,
                0,
                0,
            ];
            enc.dispatch_compute(
                self.sweep.as_ref().expect("sweep prepared"),
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&words),
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
                [count.div_ceil(256), 1, 1],
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
