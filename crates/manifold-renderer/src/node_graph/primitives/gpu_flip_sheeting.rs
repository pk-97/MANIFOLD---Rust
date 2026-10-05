//! Ported from FLIP Fluids particlesheeter.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP sheet detection and seed selection, a stage internal of
//! `node.gpu_flip_step` (DECOMPOSING_GENERATORS.md section 1.2 (Specialised
//! solvers are stage nodes); ADDING_PRIMITIVES.md exclusion 6). Not wired into
//! the step yet. The contract is `manifold_fluids::sheeter`, the CPU port
//! proven against the native sheeter; this stage is proven against that port.
//!
//! Output: the births the sheeter would seed before its fill-rate draw, each a
//! grid-local position and the rank of the candidate it came from, and their
//! true count (the list holds at most `capacity`).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

const SHADER: &str = include_str!("shaders/gpu_flip_sheeting.wgsl");
const ENTRIES: [&str; 7] = ["clear", "detect", "feather", "feather_border", "select_markers", "candidates", "resolve"];
/// gpu_flip_step.wgsl `ClockPlan`.
const PLAN_BYTES: u64 = 48;
/// The engine's `sheetFillThreshold` default; no user control yet.
pub(crate) const FILL_THRESHOLD: f32 = -0.95;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    n: [u32; 3],
    count: u32,
    b: [u32; 3],
    capacity: u32,
    origin: [f32; 3],
    h: f32,
    inv_h: f32,
    inv_sub: f32,
    half_h: f32,
    sub_dx: f32,
    max_depth: f32,
    step_distance: f32,
    steps: u32,
    max_seed_depth: f32,
    max_radius: f32,
    threshold: f32,
    pad: [u32; 2],
}

/// Bytes per cell: two sheet flags, the mask word, the selected count, the
/// candidate flags, four selected markers, and eight half-cell claims.
const BYTES_PER_CELL: u64 = 5 * 4 + 4 * 16 + 8 * 4;

/// Everything the stage holds for `cells` and a birth list of `capacity`.
pub(crate) fn scratch_bytes(cells: [u32; 3], capacity: u32) -> u64 {
    let n = cells.into_iter().map(u64::from).product::<u64>();
    BYTES_PER_CELL * n + 16 * u64::from(capacity.max(1)) + 16 + PLAN_BYTES
}

/// What one sheeting pass reads.
pub(crate) struct SheetInputs<'a> {
    /// Liquid particles sorted by cell, stable within a cell.
    pub particles: &'a GpuBuffer,
    /// Per sorted slot, the particle's input index (the engine's marker order).
    pub order: &'a GpuBuffer,
    /// One range per cell of the sort, its bins being the solver cells.
    pub ranges: &'a GpuBuffer,
    pub count: u32,
    /// The surface level set at the cell centres, x fastest.
    pub phi: &'a GpuBuffer,
    /// The grid's minimum corner; positions are made grid-local by it.
    pub origin: [f32; 3],
    pub h: f32,
    pub threshold: f32,
}

#[derive(Default)]
pub(crate) struct GpuSheeting {
    pipelines: Vec<GpuComputePipeline>,
    /// sheet a, sheet b, mask, selected count, selected, claims, candidate
    /// flags, births, birth count, zero plan.
    buffers: Option<[GpuBuffer; 10]>,
    cells: [u32; 3],
    capacity: u32,
}

impl GpuSheeting {
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_empty() {
            for entry in ENTRIES {
                self.pipelines.push(device.create_compute_pipeline(SHADER, entry, "gpu_flip.sheeting"));
            }
        }
    }

    pub(crate) fn reserve(&mut self, device: &GpuDevice, cells: [u32; 3], capacity: u32) -> Result<(), String> {
        if self.buffers.is_some() && self.cells == cells && self.capacity == capacity {
            return Ok(());
        }
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            scratch_bytes(cells, capacity),
        )
        .map_err(|e| e.to_string())?;
        let n = cells.into_iter().map(u64::from).product::<u64>();
        let plan = device.try_create_buffer_shared(PLAN_BYTES)?;
        plan.zero_fill();
        self.buffers = Some([
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(4 * 16 * n)?,
            device.try_create_buffer(8 * 4 * n)?,
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(16 * u64::from(capacity.max(1)))?,
            device.try_create_buffer(16)?,
            plan,
        ]);
        self.cells = cells;
        self.capacity = capacity;
        Ok(())
    }

    /// The birth list and its count buffer (word 0 is the true count).
    pub(crate) fn births(&self) -> (&GpuBuffer, &GpuBuffer) {
        let b = self.buffers.as_ref().expect("sheeting reserved");
        (&b[7], &b[8])
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn scratch(&self) -> &[GpuBuffer; 10] {
        self.buffers.as_ref().expect("sheeting reserved")
    }

    /// Always active: the zero clock plan.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>) {
        let zeros = &self.buffers.as_ref().expect("sheeting reserved")[9];
        self.encode_gated(enc, inputs, zeros);
    }

    /// Gated by the FLIP clock plan: in an inactive slot every pass returns
    /// before writing.
    pub(crate) fn encode_gated(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>, plan: &GpuBuffer) {
        let [sheet_a, sheet_b, mask, selected_count, selected, claims, flags, births, birth_count, _] =
            self.buffers.as_ref().expect("sheeting reserved");
        let n = self.cells;
        let h = inputs.h;
        let dx = f64::from(h);
        // The sheeter's constants, rounded as manifold_fluids::sheeter rounds them.
        let test_distance = (3.0 * dx) as f32;
        let step_distance = (0.5 * dx) as f32;
        let params = Params {
            n,
            count: inputs.count,
            b: n.map(|c| c.div_ceil(2)),
            capacity: self.capacity,
            origin: inputs.origin,
            h,
            inv_h: (1.0 / dx) as f32,
            inv_sub: (1.0 / (0.5 * dx)) as f32,
            half_h: (0.5 * dx) as f32,
            sub_dx: (0.5 * dx) as f32,
            max_depth: (2.0 * dx) as f32,
            step_distance,
            steps: (test_distance / step_distance).ceil() as u32,
            max_seed_depth: dx as f32,
            max_radius: (2.0 * dx) as f32,
            threshold: inputs.threshold,
            pad: [0; 2],
        };
        fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
            GpuBinding::Buffer { binding, buffer, offset: 0 }
        }
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
            buffer(1, inputs.particles),
            buffer(2, inputs.ranges),
            buffer(3, inputs.order),
            buffer(4, inputs.phi),
            buffer(5, sheet_a),
            buffer(6, sheet_b),
            buffer(7, mask),
            buffer(8, selected),
            buffer(9, selected_count),
            buffer(10, claims),
            buffer(11, flags),
            buffer(12, births),
            buffer(13, birth_count),
            buffer(14, plan),
        ];
        let cells = n[0] * n[1] * n[2];
        let groups = |work: u32| [work.div_ceil(256).max(1), 1, 1];
        let work = [cells, inputs.count, cells, cells, cells, 8 * cells, 8 * cells];
        let labels = [
            "gpu_flip.sheeting.clear",
            "gpu_flip.sheeting.detect",
            "gpu_flip.sheeting.feather",
            "gpu_flip.sheeting.feather_border",
            "gpu_flip.sheeting.select",
            "gpu_flip.sheeting.candidates",
            "gpu_flip.sheeting.resolve",
        ];
        for ((pipeline, work), label) in self.pipelines.iter().zip(work).zip(labels) {
            enc.dispatch_compute(pipeline, &bindings, groups(work), label);
            enc.compute_memory_barrier_buffers();
        }
    }
}
