//! Ported from FLIP Fluids particlesheeter.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP sheet detection and seed selection, a stage internal of
//! `node.gpu_flip_step` (DECOMPOSING_GENERATORS.md section 1.2 (Specialised
//! solvers are stage nodes); ADDING_PRIMITIVES.md exclusion 6). Not wired into
//! the step yet. The contract is `manifold_fluids::sheeter`, the CPU port
//! proven against the native sheeter; this stage is proven against that port.
//!
//! Output: the births the sheeter would seed before its fill-rate draw, in
//! the engine's order (ascending candidate rank), each a grid-local position
//! and its rank, and their true count. A list of `capacity` keeps the engine's
//! first `capacity` births.
//!
//! Parity contract. Exact: cell and half-cell indexing (the f64 floor, see
//! the shader's `exact_floor`), candidate centres, visiting order, the claim
//! resolution. Narrowed: interpolation and vector arithmetic run in f32 under
//! Metal fast math where the engine widens weights to f64 and contracts its
//! own way, so a decision within rounding of its threshold (band, depth walk,
//! radius, nearest-three, plane guard, mindot) can differ; and a marker's
//! cell for the density cap and first-four selection is the shared
//! ParticleSorter's f32 assignment, which can differ from the engine's f64
//! floor for a marker within an f32 rounding of a cell face.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

const SHADER: &str = include_str!("shaders/gpu_flip_sheeting.wgsl");
const ENTRIES: [&str; 8] = ["clear", "detect", "feather", "feather_border", "select_markers", "candidates", "resolve", "place"];
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
    /// The f64 reciprocals' bits, low word first, for the exact indexing.
    inv_h_bits: [u32; 2],
    inv_sub_bits: [u32; 2],
    pad: [u32; 2],
}

/// Bytes per cell: two sheet flags, the mask word, the selected count, the
/// candidate flags, four selected markers, and eight half-cell claims.
const BYTES_PER_CELL: u64 = 5 * 4 + 4 * 16 + 8 * 4;

/// Everything the stage holds for `cells` and a birth list of `capacity`,
/// the rank scan included.
pub(crate) fn scratch_bytes(cells: [u32; 3], capacity: u32) -> u64 {
    let n = cells.into_iter().map(u64::from).product::<u64>();
    let scan = 4 * super::prefix_scan::storage_words(ranks(cells) as usize) as u64;
    BYTES_PER_CELL * n + 16 * u64::from(capacity.max(1)) + 16 + PLAN_BYTES + scan
}

/// Candidate ranks: eight offsets of eight cells of every 2-cell bucket.
fn ranks(cells: [u32; 3]) -> u64 {
    64 * cells.into_iter().map(|c| u64::from(c.div_ceil(2))).product::<u64>()
}

fn words(x: f64) -> [u32; 2] {
    let bits = x.to_bits();
    [bits as u32, (bits >> 32) as u32]
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
    /// Per rank: the winners, scanned in place into birth indices.
    scan: super::prefix_scan::PrefixScan,
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
        self.scan.prepare(device);
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
        self.scan.buffer(device, ranks(cells) as usize)?;
        self.cells = cells;
        self.capacity = capacity;
        Ok(())
    }

    /// The birth list in the engine's order and its count buffer: word 0 the
    /// true count, word 1 births whose recomputed claim failed (always 0).
    pub(crate) fn births(&self) -> (&GpuBuffer, &GpuBuffer) {
        let b = self.buffers.as_ref().expect("sheeting reserved");
        (&b[7], &b[8])
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn winners(&self) -> &GpuBuffer {
        self.scan.buffer_ref()
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

    /// Proof only: each particle's cell and half-cell into the birth list,
    /// two records a particle (`capacity` must cover `2 * count`).
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_index_probe(&self, device: &GpuDevice, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>) {
        let probe = device.create_compute_pipeline(SHADER, "index_probe", "gpu_flip.sheeting.probe");
        self.dispatch_with(enc, inputs, &self.buffers.as_ref().expect("sheeting reserved")[9], Some((&probe, inputs.count)));
    }

    /// Gated by the FLIP clock plan: in an inactive slot every pass returns
    /// before writing.
    pub(crate) fn encode_gated(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>, plan: &GpuBuffer) {
        self.dispatch_with(enc, inputs, plan, None);
    }

    fn dispatch_with(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>, plan: &GpuBuffer, probe: Option<(&GpuComputePipeline, u32)>) {
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
            inv_h_bits: words(1.0 / dx),
            inv_sub_bits: words(1.0 / (0.5 * dx)),
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
            buffer(15, self.scan.buffer_ref()),
        ];
        let cells = n[0] * n[1] * n[2];
        let groups = |work: u32| [work.div_ceil(256).max(1), 1, 1];
        if let Some((pipeline, work)) = probe {
            enc.dispatch_compute(pipeline, &bindings, groups(work), "gpu_flip.sheeting.probe");
            return;
        }
        let ranks = ranks(n) as u32;
        let work = [cells, inputs.count, cells, cells, cells, 8 * cells, ranks, ranks];
        let labels = [
            "gpu_flip.sheeting.clear",
            "gpu_flip.sheeting.detect",
            "gpu_flip.sheeting.feather",
            "gpu_flip.sheeting.feather_border",
            "gpu_flip.sheeting.select",
            "gpu_flip.sheeting.candidates",
            "gpu_flip.sheeting.resolve",
            "gpu_flip.sheeting.place",
        ];
        for (i, ((pipeline, work), label)) in self.pipelines.iter().zip(work).zip(labels).enumerate() {
            enc.dispatch_compute(pipeline, &bindings, groups(work), label);
            enc.compute_memory_barrier_buffers();
            if i == 6 {
                self.scan.encode_labelled_gated(
                    enc,
                    ranks as usize,
                    super::prefix_scan::ScanLabels { blocks: "gpu_flip.sheeting.scan.blocks", add: "gpu_flip.sheeting.scan.add" },
                    plan,
                );
                enc.compute_memory_barrier_buffers();
            }
        }
    }
}
