//! GPU cut maps for exact triangle fragmentation.
//!
//! The two nodes in this file differ only in how they enumerate candidate
//! regions. Both use the same deterministic count/scan/emit shader passes and
//! write a `Vec4Vertex` map: xyz are reference-triangle barycentrics and w is
//! the source triangle index. Unused records are zero with w = -1.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuEvent};

use manifold_node_engine::content_revision::ContentVersion;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::{MeshVertex, Vec4Vertex};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

/// The vertices a cut may add past its source, a memory reserve: each cut object lays this many map records and mesh vertices at build (96 bytes each, about 19 MB), so Bands and Cells move live.
pub const CUT_MAP_EXTRA_RECORDS: u32 = 196_608;
/// What a cut past [`CUT_MAP_EXTRA_RECORDS`] says.
const OVER_RESERVE: &str = "the cut adds more than its memory reserve of 196,608 vertices; fewer Bands or bigger cells fit";
const MESH_VERTEX_SIZE: u64 = std::mem::size_of::<MeshVertex>() as u64;
const VEC4_VERTEX_SIZE: u64 = std::mem::size_of::<Vec4Vertex>() as u64;
const SHADER: &str = include_str!("shaders/mesh_cut_map.wgsl");

#[cfg(test)]
mod oracle;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CutMapUniforms {
    mode: u32,
    bands: u32,
    triangle_count: u32,
    candidate_count: u32,
    output_capacity: u32,
    _pad0: u32,
    cell_size: f32,
    scale: f32,
    source_offset: [f32; 3],
    _pad1: f32,
    direction: [f32; 3],
    _pad2: f32,
}

/// Fenced CPU readback slot. GPU scratch is queue-owned and shared by all
/// dispatches; only these snapshots are ringed for asynchronous polling.
pub struct CutMapReadback {
    pub buffer: GpuBuffer,
    pub event: Option<GpuEvent>,
    pub signal: u64,
}

pub struct CutMapState {
    pub count_pipeline: Option<GpuComputePipeline>,
    pub scan_pipeline: Option<GpuComputePipeline>,
    pub emit_pipeline: Option<GpuComputePipeline>,
    pub clear_pipeline: Option<GpuComputePipeline>,
    status_clear_pipeline: Option<GpuComputePipeline>,
    counts: Option<GpuBuffer>,
    prefix: Option<GpuBuffer>,
    status: Option<GpuBuffer>,
    candidate_capacity: u32,
    pub readbacks: Vec<CutMapReadback>,
    pub last_key: Option<u64>,
    status_needs_snapshot: bool,
    next_readback: usize,
    /// How many leading map records are live, written by the scan pass
    /// (binding 6) and published as the map's live extent.
    live_count: Option<GpuBuffer>,
}

impl CutMapState {
    pub fn new() -> Self {
        Self {
            count_pipeline: None,
            scan_pipeline: None,
            emit_pipeline: None,
            clear_pipeline: None,
            status_clear_pipeline: None,
            counts: None,
            prefix: None,
            status: None,
            candidate_capacity: 0,
            readbacks: Vec::new(),
            last_key: None,
            status_needs_snapshot: false,
            next_readback: 0,
            live_count: None,
        }
    }
}

impl Default for CutMapState {
    fn default() -> Self {
        Self::new()
    }
}

fn map_capacity(reference_capacity: u32) -> Option<u32> {
    (reference_capacity / 3)
        .checked_mul(3)
        .and_then(|base| base.checked_add(CUT_MAP_EXTRA_RECORDS))
}

/// Bytes reserved by the count/prefix scratch for a map allocation. The map
/// capacity includes the additive fragment reserve; source records therefore
/// determine the candidate count, followed by one tail word per workgroup.
pub(crate) fn scratch_bytes(map_records: u64) -> Option<u64> {
    let source_records = map_records.checked_sub(u64::from(CUT_MAP_EXTRA_RECORDS))?;
    let candidates = source_records / 3;
    let blocks = candidates.checked_add(255)?.checked_div(256)?;
    let words = candidates.checked_add(blocks)?;
    words.checked_mul(4)?.checked_mul(2)?.checked_add(16 + 48)
}

inventory::submit! {
    manifold_node_engine::exec::resource_allocation::ArrayScratch {
        type_id: "node.cut_mesh_bands",
        port: "map",
        bytes: scratch_bytes,
    }
}
inventory::submit! {
    manifold_node_engine::exec::resource_allocation::ArrayScratch {
        type_id: "node.cut_mesh_cells",
        port: "map",
        bytes: scratch_bytes,
    }
}

fn scratch_words(candidate_count: u32) -> Option<u64> {
    let blocks = u64::from(candidate_count)
        .checked_add(255)?
        .checked_div(256)?;
    u64::from(candidate_count).checked_add(blocks)
}

fn hash_key(values: &[u64], content: Option<ContentVersion>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = ahash::AHasher::default();
    content.hash(&mut hash);
    values.hash(&mut hash);
    hash.finish()
}

fn ensure_pipeline<'a>(
    pipeline: &'a mut Option<GpuComputePipeline>,
    device: &manifold_gpu::GpuDevice,
    entry: &str,
    label: &str,
) -> &'a GpuComputePipeline {
    if pipeline.is_none() {
        *pipeline = Some(device.create_compute_pipeline(SHADER, entry, label));
    }
    pipeline.as_ref().expect("cut-map pipeline just created")
}

fn clear_map(
    gpu: &mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>,
    pipeline: &GpuComputePipeline,
    uniforms: &CutMapUniforms,
    map: &GpuBuffer,
    live_count: &GpuBuffer,
) {
    let capacity = uniforms.output_capacity;
    if capacity == 0 {
        return;
    }
    gpu.native_enc.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(uniforms),
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: map,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: live_count,
                offset: 0,
            },
        ],
        [capacity.div_ceil(256), 1, 1],
        "node.cut_mesh_map.clear",
    );
}

fn clear_invalid_output(
    ctx: &mut EffectNodeContext<'_, '_>,
    state: &mut CutMapState,
    map: &GpuBuffer,
    live_count: &GpuBuffer,
    output_capacity: u32,
) {
    if output_capacity == 0 {
        return;
    }
    let uniforms = CutMapUniforms {
        mode: 0,
        bands: 1,
        triangle_count: 0,
        candidate_count: 0,
        output_capacity,
        _pad0: 0,
        cell_size: 1.0,
        scale: 1.0,
        source_offset: [0.0; 3],
        _pad1: 0.0,
        direction: [0.0, 1.0, 0.0],
        _pad2: 0.0,
    };
    let gpu = ctx.gpu_encoder();
    let pipeline = ensure_pipeline(
        &mut state.clear_pipeline,
        gpu.device,
        "clear_main",
        "node.cut_mesh_map.clear_invalid",
    );
    clear_map(gpu, pipeline, &uniforms, map, live_count);
}

fn report_completed_status(ctx: &mut EffectNodeContext<'_, '_>, status: &GpuBuffer) {
    let Some(ptr) = status.mapped_ptr() else {
        return;
    };
    let words = unsafe { std::slice::from_raw_parts(ptr as *const u32, 4) };
    if words[3] != 0 {
        let reason = match words[3] {
            1 => OVER_RESERVE,
            2 => "nonfinite geometry or intersection",
            3 => "source triangle index is not exactly representable as f32",
            _ => "unsupported numeric condition",
        };
        ctx.error(format!(
            "node.cut_mesh_map: previous GPU map invalid ({reason})"
        ));
    }
}

fn poll_completed_status(ctx: &mut EffectNodeContext<'_, '_>, state: &mut CutMapState) {
    for slot in &mut state.readbacks {
        let signal = slot.signal;
        if signal == 0 {
            continue;
        }
        if slot
            .event
            .as_ref()
            .is_some_and(|event| event.is_done(signal))
        {
            report_completed_status(ctx, &slot.buffer);
            slot.signal = 0;
        }
    }
}

fn ensure_readbacks(state: &mut CutMapState, device: &manifold_gpu::GpuDevice) {
    if state.readbacks.is_empty() {
        state.readbacks = (0..3)
            .map(|_| CutMapReadback {
                buffer: device.create_buffer_shared(16),
                event: None,
                signal: 0,
            })
            .collect();
    }
}

fn snapshot_status(
    gpu: &mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>,
    readbacks: &mut [CutMapReadback],
    next_readback: &mut usize,
    status_clear_pipeline: &mut Option<GpuComputePipeline>,
    status: &GpuBuffer,
    bindings: &[GpuBinding<'_>],
) -> bool {
    let Some(index) = (0..readbacks.len())
        .map(|step| (*next_readback + step) % readbacks.len())
        .find(|&index| readbacks[index].signal == 0)
    else {
        return false;
    };
    *next_readback = (index + 1) % readbacks.len();
    let readback = &mut readbacks[index];
    gpu.native_enc
        .copy_buffer_to_buffer(status, &readback.buffer, 16);
    let status_clear = ensure_pipeline(
        status_clear_pipeline,
        gpu.device,
        "clear_status_main",
        "node.cut_mesh_map.clear_status",
    );
    gpu.native_enc.dispatch_compute(
        status_clear,
        bindings,
        [1, 1, 1],
        "node.cut_mesh_map.clear_status",
    );
    let event = readback
        .event
        .get_or_insert_with(|| gpu.device.create_event());
    gpu.native_enc.signal_event(event);
    readback.signal = event.encoded_value();
    true
}

fn run_cut_map(
    ctx: &mut EffectNodeContext<'_, '_>,
    mode: u32,
    state: &mut CutMapState,
    bands_default: f32,
    cell_size_default: f32,
) {
    poll_completed_status(ctx, state);
    if ctx.inputs.slot("reference").is_none() {
        ctx.mark_outputs_pending();
        ctx.error("node.cut_mesh_map: missing required `reference` input");
        return;
    }
    // A wired mesh with no buffer yet (or no GPU) waits, as the cut's
    // remaps and fragments do.
    let (Some(reference), Some(map)) = (ctx.inputs.array("reference"), ctx.outputs.array("map"))
    else {
        ctx.mark_outputs_pending();
        return;
    };
    if state.live_count.is_none() {
        let buffer = ctx.gpu_encoder().device.create_buffer_shared(4);
        buffer.zero_fill();
        state.live_count = Some(buffer);
    }
    let live_count = state
        .live_count
        .clone()
        .expect("cut-map live count allocated");
    let live_count = &live_count;
    let reference_capacity = reference.size / MESH_VERTEX_SIZE;
    let output_capacity = map.size / VEC4_VERTEX_SIZE;
    if reference_capacity > u32::MAX as u64 || output_capacity > u32::MAX as u64 {
        ctx.mark_outputs_pending();
        ctx.error("node.cut_mesh_map: input or output capacity exceeds GPU addressable range");
        return;
    }
    let reference_capacity = reference_capacity as u32;
    let output_capacity = output_capacity as u32;
    // Published on every run, stasis included: downstream steps stop at the
    // live records, and the extent is the only record of where they end.
    ctx.outputs.set_live_extent(
        "map",
        manifold_node_engine::scene::live_extent::LiveExtent {
            counts: live_count.clone(),
            offset: 0,
            per_item: 1,
            bound: output_capacity,
        },
    );
    let triangle_count = reference_capacity / 3;
    let bands_value = if mode == 0 {
        ctx.scalar_or_param("bands", bands_default)
    } else {
        1.0
    };
    let bands_valid = bands_value.is_finite();
    let bands = bands_value.round().clamp(1.0, 64.0) as u32;
    let cell_size = ctx.scalar_or_param("cell_size", cell_size_default);
    let scale = ctx.scalar_or_param("scale", 1.0);
    let source_offset = [
        ctx.scalar_or_param("source_offset_x", 0.0),
        ctx.scalar_or_param("source_offset_y", 0.0),
        ctx.scalar_or_param("source_offset_z", 0.0),
    ];
    let direction = [
        ctx.scalar_or_param("direction_x", 0.0),
        ctx.scalar_or_param("direction_y", 1.0),
        ctx.scalar_or_param("direction_z", 0.0),
    ];
    let scalar_values = [
        cell_size,
        scale,
        source_offset[0],
        source_offset[1],
        source_offset[2],
        direction[0],
        direction[1],
        direction[2],
    ];
    let controls_valid = bands_valid && scalar_values.iter().all(|value| value.is_finite());
    let Some(capacity_contract) = map_capacity(reference_capacity) else {
        clear_invalid_output(ctx, state, map, live_count, output_capacity);
        ctx.mark_outputs_pending();
        ctx.error("node.cut_mesh_map: output capacity arithmetic overflow");
        return;
    };
    if output_capacity < capacity_contract {
        clear_invalid_output(ctx, state, map, live_count, output_capacity);
        ctx.mark_outputs_pending();
        ctx.error(format!("node.cut_mesh_map: output capacity {output_capacity} is below required reserve {capacity_contract}"));
        return;
    }
    let candidate_count = triangle_count;
    let reference_wired = ctx.inputs.slot("reference").is_some();
    let reference_content = ctx.inputs.content_version("reference");
    let key = hash_key(
        &[
            mode as u64,
            bands as u64,
            cell_size.to_bits() as u64,
            scale.to_bits() as u64,
            source_offset[0].to_bits() as u64,
            source_offset[1].to_bits() as u64,
            source_offset[2].to_bits() as u64,
            direction[0].to_bits() as u64,
            direction[1].to_bits() as u64,
            direction[2].to_bits() as u64,
            output_capacity as u64,
            ctx.rebuild_epoch,
        ],
        reference_content,
    );
    let content_known = !reference_wired || reference_content.is_some();
    let content_unchanged = controls_valid && content_known && state.last_key == Some(key);
    if content_unchanged && ctx.outputs_retained() {
        if state.status_needs_snapshot && state.status.is_some() {
            let gpu = ctx.gpu_encoder();
            ensure_readbacks(state, gpu.device);
            let status = state.status.as_ref().expect("cut-map status allocated");
            let status_bindings = [GpuBinding::Buffer {
                binding: 4,
                buffer: status,
                offset: 0,
            }];
            state.status_needs_snapshot = !snapshot_status(
                gpu,
                &mut state.readbacks,
                &mut state.next_readback,
                &mut state.status_clear_pipeline,
                status,
                &status_bindings,
            );
        }
        ctx.mark_outputs_unchanged();
        return;
    }
    state.last_key = None;
    let uniforms = CutMapUniforms {
        mode,
        bands,
        triangle_count,
        candidate_count,
        output_capacity,
        _pad0: 0,
        cell_size,
        scale,
        source_offset,
        _pad1: 0.0,
        direction,
        _pad2: 0.0,
    };
    let gpu = ctx.gpu_encoder();
    let clear_pipeline = ensure_pipeline(
        &mut state.clear_pipeline,
        gpu.device,
        "clear_main",
        "node.cut_mesh_map.clear",
    );
    clear_map(gpu, clear_pipeline, &uniforms, map, live_count);
    if !controls_valid {
        ctx.error("node.cut_mesh_map: nonfinite cutter control");
        return;
    }
    if candidate_count == 0 {
        state.last_key = Some(key);
        if content_unchanged {
            ctx.mark_output_content_unchanged();
        }
        return;
    }
    let Some(scratch_words) = scratch_words(candidate_count) else {
        ctx.error("node.cut_mesh_map: scratch allocation arithmetic overflow");
        return;
    };
    let Some(scratch_bytes) = scratch_words.checked_mul(4) else {
        ctx.error("node.cut_mesh_map: scratch allocation arithmetic overflow");
        return;
    };
    if state.candidate_capacity < candidate_count {
        let bytes = scratch_bytes.max(4);
        state.counts = Some(gpu.device.create_buffer_shared(bytes));
        state.prefix = Some(gpu.device.create_buffer_shared(bytes));
        state.candidate_capacity = candidate_count;
    }
    if state.status.is_none() {
        let status = gpu.device.create_buffer_shared(16);
        status.zero_fill();
        state.status = Some(status);
    }
    ensure_readbacks(state, gpu.device);
    let counts = state.counts.as_ref().expect("cut-map counts allocated");
    let prefix = state.prefix.as_ref().expect("cut-map prefix allocated");
    let status = state.status.as_ref().expect("cut-map status allocated");
    let bindings = [
        GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(&uniforms),
        },
        GpuBinding::Buffer {
            binding: 1,
            buffer: reference,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 2,
            buffer: counts,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 3,
            buffer: prefix,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 4,
            buffer: status,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 5,
            buffer: map,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 6,
            buffer: live_count,
            offset: 0,
        },
    ];
    let count_pipeline = ensure_pipeline(
        &mut state.count_pipeline,
        gpu.device,
        "count_main",
        "node.cut_mesh_map.count",
    );
    let scan_pipeline = ensure_pipeline(
        &mut state.scan_pipeline,
        gpu.device,
        "scan_main",
        "node.cut_mesh_map.scan",
    );
    let emit_pipeline = ensure_pipeline(
        &mut state.emit_pipeline,
        gpu.device,
        "emit_main",
        "node.cut_mesh_map.emit",
    );
    gpu.native_enc.dispatch_compute(
        count_pipeline,
        &bindings,
        [candidate_count.div_ceil(256), 1, 1],
        "node.cut_mesh_map.count",
    );
    gpu.native_enc.compute_memory_barrier_buffers();
    gpu.native_enc.dispatch_compute(
        scan_pipeline,
        &bindings,
        [1, 1, 1],
        "node.cut_mesh_map.scan",
    );
    gpu.native_enc.compute_memory_barrier_buffers();
    gpu.native_enc.dispatch_compute(
        emit_pipeline,
        &bindings,
        [candidate_count.div_ceil(256), 1, 1],
        "node.cut_mesh_map.emit",
    );
    state.status_needs_snapshot = !snapshot_status(
        gpu,
        &mut state.readbacks,
        &mut state.next_readback,
        &mut state.status_clear_pipeline,
        status,
        &bindings,
    );
    state.last_key = Some(key);
    if content_unchanged {
        ctx.mark_output_content_unchanged();
    }
}

fn cut_map_capacity(port_name: &str, input_capacities: &[(&str, u32)]) -> Option<u32> {
    if port_name != "map" {
        return None;
    }
    let reference = input_capacities
        .iter()
        .find(|(name, _)| *name == "reference")
        .map(|(_, capacity)| *capacity)?;
    map_capacity(reference)
}

manifold_node_engine::primitive! {
    name: CutMeshBands,
    type_id: "node.cut_mesh_bands",
    purpose: "Clip each reference triangle into exact directional bands and emit barycentric provenance records for downstream live mesh fragmentation.",
    inputs: {
        reference: Array(MeshVertex) required,
        bands: ScalarF32 optional,
        direction_x: ScalarF32 optional,
        direction_y: ScalarF32 optional,
        direction_z: ScalarF32 optional,
        scale: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
    },
    outputs: { map: Array(Vec4Vertex), },
    params: [
        ParamDef { name: Cow::Borrowed("bands"), label: "Bands", ty: ParamType::Int, default: ParamValue::Float(8.0), range: Some((1.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_x"), label: "Direction X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_y"), label: "Direction Y", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_z"), label: "Direction Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scale"), label: "Scene Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Bands are clamped to 1..64 and direction is normalized with a +Y fallback. Only internal slab planes cut geometry; the first and last bands are unbounded. Every map record is xyz barycentrics plus w = exact source triangle index, with unused records set to zero/-1. Valid records are compacted at the front; the map publishes a live extent counting them (0 when the map is invalid), so every step downstream stops at the live records and the reserve tail stays zero. All numeric controls are port-shadowed.",
    examples: [], picker: { label: "Cut Mesh Bands", category: Atom },
    summary: "Cuts mesh triangles into directional bands while retaining source-triangle provenance.",
    category: Geometry3D, role: Source,
    aliases: ["mesh bands", "band cuts", "fragment bands"],
    boundary_reason: BarrieredReduction,
    extra_fields: { state: CutMapState = CutMapState::new() },
}

impl Primitive for CutMeshBands {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        cut_map_capacity(port_name, input_capacities)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        run_cut_map(ctx, 0, &mut self.state, 8.0, 0.2);
    }
}

manifold_node_engine::primitive! {
    name: CutMeshCells,
    type_id: "node.cut_mesh_cells",
    purpose: "Clip each reference triangle into exact grid cells and emit barycentric provenance records for downstream live mesh fragmentation.",
    inputs: {
        reference: Array(MeshVertex) required,
        cell_size: ScalarF32 optional,
        scale: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
    },
    outputs: { map: Array(Vec4Vertex), },
    params: [
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.15), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scale"), label: "Scene Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Cells use normalized source position `(position + source_offset) / abs(scale).max(1e-6)` and the exact shared quantization `floor(p / abs(cell_size).max(1e-6) + 0.5)`. Every map record is xyz barycentrics plus w = exact source triangle index, with unused records set to zero/-1. Valid records are compacted at the front; the map publishes a live extent counting them (0 when the map is invalid), so every step downstream stops at the live records and the reserve tail stays zero. All numeric controls are port-shadowed.",
    examples: [], picker: { label: "Cut Mesh Cells", category: Atom },
    summary: "Cuts mesh triangles into grid cells while retaining source-triangle provenance.",
    category: Geometry3D, role: Source,
    aliases: ["mesh cells", "cell cuts", "grid fragments"],
    boundary_reason: BarrieredReduction,
    extra_fields: { state: CutMapState = CutMapState::new() },
}

impl Primitive for CutMeshCells {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        cut_map_capacity(port_name, input_capacities)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        run_cut_map(ctx, 1, &mut self.state, 8.0, 0.15);
    }
}

inventory::submit! {
    manifold_node_engine::scene::live_extent::LiveExtentOutput {
        type_id: "node.cut_mesh_bands",
        port: "map",
    }
}
inventory::submit! {
    manifold_node_engine::scene::live_extent::LiveExtentOutput {
        type_id: "node.cut_mesh_cells",
        port: "map",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_capacity_reserves_additive_fragment_space() {
        assert_eq!(map_capacity(2), Some(CUT_MAP_EXTRA_RECORDS));
        assert_eq!(map_capacity(12), Some(196_620));
        assert_eq!(map_capacity(u32::MAX), None);
    }

    #[test]
    fn scratch_bytes_accounts_for_blocks_status_and_readbacks() {
        assert_eq!(scratch_bytes(u64::from(CUT_MAP_EXTRA_RECORDS)), Some(64));
        // One source triangle: two four-byte arrays, one block tail word in
        // each, sixteen status bytes, and three sixteen-byte snapshots.
        assert_eq!(
            scratch_bytes(u64::from(CUT_MAP_EXTRA_RECORDS) + 3),
            Some(80)
        );
        assert_eq!(scratch_bytes(u64::from(CUT_MAP_EXTRA_RECORDS) - 1), None);
    }

    #[cfg(feature = "gpu-proofs")]
    fn dispatch_shader(
        vertices: &[MeshVertex],
        mode: u32,
        bands: u32,
        output_capacity: u32,
    ) -> (Vec<Vec4Vertex>, [u32; 4]) {
        let device = manifold_gpu::testkit::test_device();
        let reference = device.create_buffer_shared(std::mem::size_of_val(vertices) as u64);
        unsafe {
            reference.write(0, bytemuck::cast_slice(vertices));
        }
        let triangle_count = (vertices.len() / 3) as u32;
        let candidate_count = triangle_count;
        let block_count = candidate_count.div_ceil(256);
        let scratch_words = u64::from(candidate_count + block_count);
        let counts = device.create_buffer_shared(scratch_words.max(1) * 4);
        let prefix = device.create_buffer_shared(scratch_words.max(1) * 4);
        let status = device.create_buffer_shared(16);
        status.zero_fill();
        let map = device.create_buffer_shared(u64::from(output_capacity.max(1)) * VEC4_VERTEX_SIZE);
        let uniforms = CutMapUniforms {
            mode,
            bands,
            triangle_count,
            candidate_count,
            output_capacity,
            _pad0: 0,
            cell_size: 1.0,
            scale: 1.0,
            source_offset: [0.0; 3],
            _pad1: 0.0,
            direction: [0.0, 1.0, 0.0],
            _pad2: 0.0,
        };
        let pipeline = |entry, label| device.create_compute_pipeline(SHADER, entry, label);
        let clear = pipeline("clear_main", "cut-map-test-clear");
        let count = pipeline("count_main", "cut-map-test-count");
        let scan = pipeline("scan_main", "cut-map-test-scan");
        let emit = pipeline("emit_main", "cut-map-test-emit");
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &reference,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &counts,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &prefix,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &status,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: &map,
                offset: 0,
            },
        ];
        let mut encoder = device.create_encoder("cut-map-test");
        encoder.dispatch_compute(
            &clear,
            &bindings,
            [output_capacity.div_ceil(256), 1, 1],
            "cut-map-test-clear",
        );
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(
            &count,
            &bindings,
            [block_count.max(1), 1, 1],
            "cut-map-test-count",
        );
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(&scan, &bindings, [1, 1, 1], "cut-map-test-scan");
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(
            &emit,
            &bindings,
            [block_count.max(1), 1, 1],
            "cut-map-test-emit",
        );
        encoder.commit_and_wait_completed();
        let map_ptr = map.mapped_ptr().expect("shared map");
        let output = unsafe {
            std::slice::from_raw_parts(
                map_ptr as *const Vec4Vertex,
                output_capacity.max(1) as usize,
            )
        }
        .to_vec();
        let status_ptr = status.mapped_ptr().expect("shared status");
        let status = unsafe { *(status_ptr as *const [u32; 4]) };
        (output, status)
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_identity_emits_finite_winding_preserving_barycentrics() {
        let vertices = [
            MeshVertex {
                position: [0.0, 0.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
            MeshVertex {
                position: [1.0, 0.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
            MeshVertex {
                position: [0.0, 1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
        ];
        let (output, status) = dispatch_shader(&vertices, 0, 1, 3);
        assert_eq!(status[1], 0);
        assert_eq!(output[0].position, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(output[1].position, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(output[2].position, [0.0, 0.0, 1.0, 0.0]);
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_overflow_invalidates_every_record() {
        let vertices = [
            MeshVertex {
                position: [0.0, 0.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
            MeshVertex {
                position: [1.0, 0.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
            MeshVertex {
                position: [0.0, 1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
        ];
        let (output, status) = dispatch_shader(&vertices, 0, 1, 2);
        assert_eq!(status[1], 1);
        assert!(output.iter().all(|vertex| vertex.position[3] == -1.0));
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_band_crossing_has_two_finite_fragments() {
        let vertices = [
            MeshVertex {
                position: [0.0, -1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
            MeshVertex {
                position: [1.0, 1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
            MeshVertex {
                position: [-1.0, 1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            },
        ];
        let (output, status) = dispatch_shader(&vertices, 0, 2, 12);
        assert_eq!(status[1], 0);
        let mut expected = Vec::new();
        for plane in [[0.0, -1.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]] {
            oracle::clip_triangle_to_planes(
                vertices.map(|vertex| vertex.position),
                &[plane],
                &mut expected,
            )
            .unwrap();
        }
        assert_eq!(
            status[0], 9,
            "one triangle plus a triangulated quadrilateral"
        );
        for (actual, expected) in output.iter().zip(expected.iter().flatten()) {
            assert_eq!(actual.position[3], 0.0);
            for (actual, expected) in actual.position[..3].iter().zip(expected.barycentric) {
                assert!((actual - expected).abs() < 1e-6);
            }
        }
        assert!(output[9..].iter().all(|vertex| vertex.position[3] == -1.0));
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_cell_crossing_matches_independent_clipping_oracle() {
        let vertices =
            [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]].map(|position| MeshVertex {
                position,
                ..bytemuck::Zeroable::zeroed()
            });
        let mut expected = Vec::new();
        for x in 0..=1 {
            for y in 0..=1 {
                oracle::clip_triangle_to_planes(
                    vertices.map(|vertex| vertex.position),
                    &[
                        [1.0, 0.0, 0.0, 0.5 - x as f32],
                        [-1.0, 0.0, 0.0, x as f32 + 0.5],
                        [0.0, 1.0, 0.0, 0.5 - y as f32],
                        [0.0, -1.0, 0.0, y as f32 + 0.5],
                        [0.0, 0.0, 1.0, 0.5],
                        [0.0, 0.0, -1.0, 0.5],
                    ],
                    &mut expected,
                )
                .unwrap();
            }
        }
        let (actual, status) = dispatch_shader(&vertices, 1, 1, 24);
        assert_eq!(status[1], 0);
        assert_eq!(status[0] as usize, expected.len() * 3);
        for (actual, expected) in actual.iter().zip(expected.iter().flatten()) {
            assert_eq!(actual.position[3], 0.0);
            for (actual, expected) in actual.position[..3].iter().zip(expected.barycentric) {
                assert!((actual - expected).abs() < 1e-6);
            }
        }
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_prefix_handles_multiple_workgroups() {
        let vertex = |position| MeshVertex {
            position,
            ..bytemuck::Zeroable::zeroed()
        };
        let mut vertices = Vec::with_capacity(257 * 3);
        for _ in 0..257 {
            vertices.extend([
                vertex([0.0, 0.0, 0.0]),
                vertex([1.0, 0.0, 0.0]),
                vertex([0.0, 1.0, 0.0]),
            ]);
        }
        let (output, status) = dispatch_shader(&vertices, 0, 1, 257 * 3);
        assert_eq!(status[1], 0);
        assert_eq!(status[0], 257 * 3);
        for (triangle, records) in output.chunks_exact(3).enumerate() {
            for record in records {
                assert_eq!(record.position[3], triangle as f32);
            }
            assert_eq!(records[0].position[..3], [1.0, 0.0, 0.0]);
            assert_eq!(records[1].position[..3], [0.0, 1.0, 0.0]);
            assert_eq!(records[2].position[..3], [0.0, 0.0, 1.0]);
        }
    }

    /// Walking only the bands a triangle reaches keeps every fragment: at 64
    /// bands, each triangle's fragments still cover its whole area, and none
    /// falls outside its band.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_bands_cover_each_triangle_whole() {
        let hash = |index: u32| {
            let mut value = index.wrapping_mul(0x9e37_79b9) ^ 0x85eb_ca6b;
            value ^= value >> 16;
            value = value.wrapping_mul(0x7feb_352d);
            value ^= value >> 15;
            value as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let triangles = 2048u32;
        let mut vertices = Vec::with_capacity(triangles as usize * 3);
        for triangle in 0..triangles {
            // From slivers inside one band to triangles across all of them.
            let size = 0.002 + 2.0 * (triangle as f32 / triangles as f32).powi(3);
            let centre = [
                hash(triangle * 7),
                hash(triangle * 7 + 1),
                hash(triangle * 7 + 2),
            ];
            for corner in 0..3 {
                let offset = |axis: u32| hash(triangle * 7 + 3 + corner * 3 + axis) * size;
                let position = [
                    centre[0] + offset(0),
                    centre[1] + offset(1),
                    centre[2] + offset(2),
                ];
                vertices.push(MeshVertex {
                    position,
                    ..bytemuck::Zeroable::zeroed()
                });
            }
        }
        let bands = 64;
        let capacity = triangles * 3 + CUT_MAP_EXTRA_RECORDS;
        let (output, status) = dispatch_shader(&vertices, 0, bands, capacity);
        assert_eq!(status[1], 0, "no error");
        let full = 3f32.sqrt() / 2.0;
        let mut covered = vec![0.0f32; triangles as usize];
        for record in output[..status[0] as usize].chunks_exact(3) {
            let triangle = record[0].position[3] as usize;
            let [a, b, c] = [0, 1, 2].map(|k| glam::Vec3::from_slice(&record[k].position[..3]));
            covered[triangle] += (b - a).cross(c - a).length() / 2.0;
            // Projected along y, the fragment stays inside one band.
            let corners =
                [0, 1, 2].map(|k| glam::Vec3::from_slice(&vertices[triangle * 3 + k].position));
            let ys = [a, b, c]
                .map(|bary| (corners[0] * bary.x + corners[1] * bary.y + corners[2] * bary.z).y);
            let (low, high) = (
                ys.iter().copied().fold(f32::INFINITY, f32::min),
                ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            );
            let band_of = |y: f32| {
                ((0.5 + 0.5 * y) * bands as f32)
                    .floor()
                    .clamp(0.0, bands as f32 - 1.0)
            };
            if high - low > 2e-4 {
                assert_eq!(
                    band_of(low + 1e-4),
                    band_of(high - 1e-4),
                    "triangle {triangle}: a fragment spans two bands"
                );
            }
        }
        for (triangle, area) in covered.iter().enumerate() {
            assert!(
                (area - full).abs() < 1e-3,
                "triangle {triangle}: fragments cover {area} of {full}"
            );
        }
    }

    /// A photoscan-sized cut: every triangle keeps its place in the map and
    /// the scan's GPU time is printed (median of nine), so a drag of Bands
    /// or Direction on a big scan can be timed against its frame.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_scan_orders_a_photoscan_and_reports_its_time() {
        let device = manifold_gpu::testkit::test_device();
        for triangles in [131_072u32, 1_048_576] {
            let vertex = |position| MeshVertex {
                position,
                ..bytemuck::Zeroable::zeroed()
            };
            let mut vertices = Vec::with_capacity(triangles as usize * 3);
            for _ in 0..triangles {
                vertices.extend([
                    vertex([0.0, 0.0, 0.0]),
                    vertex([1.0, 0.0, 0.0]),
                    vertex([0.0, 1.0, 0.0]),
                ]);
            }
            let reference =
                device.create_buffer_shared(std::mem::size_of_val(vertices.as_slice()) as u64);
            unsafe { reference.write(0, bytemuck::cast_slice(&vertices)) };
            let blocks = triangles.div_ceil(256);
            let words = u64::from(triangles + blocks);
            let counts = device.create_buffer_shared(words * 4);
            let prefix = device.create_buffer_shared(words * 4);
            let status = device.create_buffer_shared(16);
            status.zero_fill();
            let live = device.create_buffer_shared(4);
            let output_capacity = triangles * 3;
            let map = device.create_buffer_shared(u64::from(output_capacity) * VEC4_VERTEX_SIZE);
            let uniforms = CutMapUniforms {
                mode: 0,
                bands: 1,
                triangle_count: triangles,
                candidate_count: triangles,
                output_capacity,
                _pad0: 0,
                cell_size: 1.0,
                scale: 1.0,
                source_offset: [0.0; 3],
                _pad1: 0.0,
                direction: [0.0, 1.0, 0.0],
                _pad2: 0.0,
            };
            let pipeline = |entry| device.create_compute_pipeline(SHADER, entry, entry);
            let (count, scan, emit) = (
                pipeline("count_main"),
                pipeline("scan_main"),
                pipeline("emit_main"),
            );
            let bindings = [
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &reference,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &counts,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &prefix,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &status,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: &map,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: &live,
                    offset: 0,
                },
            ];
            // The scan overwrites the block tails it reads, so each timed
            // scan follows a fresh count.
            let mut times = Vec::new();
            let mut count_times = Vec::new();
            for _ in 0..9 {
                let mut encoder = device.create_encoder("cut-map-scan-count");
                encoder.dispatch_compute(&count, &bindings, [blocks, 1, 1], "count");
                count_times.push(encoder.commit_and_wait_profiled(&device).total_ms);
                let mut encoder = device.create_encoder("cut-map-scan");
                encoder.dispatch_compute(&scan, &bindings, [1, 1, 1], "scan");
                times.push(encoder.commit_and_wait_profiled(&device).total_ms);
            }
            let mut encoder = device.create_encoder("cut-map-scan-emit");
            encoder.dispatch_compute(&emit, &bindings, [blocks, 1, 1], "emit");
            let emit_ms = encoder.commit_and_wait_profiled(&device).total_ms;
            times.sort_by(f64::total_cmp);
            count_times.sort_by(f64::total_cmp);
            println!(
                "CUT_MAP_SCAN triangles={triangles} blocks={blocks} scan_ms={:.4} count_ms={:.4} emit_ms={emit_ms:.4}",
                times[4], count_times[4]
            );
            let status =
                unsafe { *(status.mapped_ptr().expect("shared status") as *const [u32; 4]) };
            assert_eq!(status[1], 0, "no error");
            assert_eq!(status[0], output_capacity, "every triangle is counted");
            let live = unsafe { *(live.mapped_ptr().expect("shared live count") as *const u32) };
            assert_eq!(live, output_capacity);
            let records = unsafe {
                std::slice::from_raw_parts(
                    map.mapped_ptr().expect("shared map") as *const Vec4Vertex,
                    output_capacity as usize,
                )
            };
            for (triangle, record) in records.chunks_exact(3).enumerate() {
                assert!(
                    record.iter().all(|r| r.position[3] == triangle as f32),
                    "triangle {triangle} sits at its own place"
                );
            }
        }
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_prefix_rejects_single_block_aggregate_overflow() {
        let vertex = |position| MeshVertex {
            position,
            ..bytemuck::Zeroable::zeroed()
        };
        let vertices = vec![
            vertex([0.0, 0.0, 0.0]),
            vertex([1.0, 0.0, 0.0]),
            vertex([0.0, 1.0, 0.0]),
            vertex([2.0, 0.0, 0.0]),
            vertex([3.0, 0.0, 0.0]),
            vertex([2.0, 1.0, 0.0]),
        ];
        let (output, status) = dispatch_shader(&vertices, 0, 1, 4);
        assert_eq!(status[1], 1);
        assert!(output.iter().all(|vertex| vertex.position[3] == -1.0));
    }
}

#[cfg(test)]
mod scratch_source_tests {
    #[test]
    fn array_scratch_registration_preserves_checked_sizing() {
        for type_id in ["node.cut_mesh_bands", "node.cut_mesh_cells"] {
            let scratch =
                manifold_node_engine::exec::resource_allocation::array_scratch(type_id).unwrap();
            assert_eq!(scratch.port, "map");
            let bytes = scratch.bytes;
            for records in [0, 196_608, 196_611, u64::MAX] {
                assert_eq!(bytes(records), super::scratch_bytes(records));
            }
        }
    }
}
