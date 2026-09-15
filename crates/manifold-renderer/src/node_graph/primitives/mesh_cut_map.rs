//! GPU cut maps for exact triangle fragmentation.
//!
//! The two nodes in this file differ only in how they enumerate candidate
//! regions. Both use the same deterministic count/scan/emit shader passes and
//! write a `Vec4Vertex` map: xyz are reference-triangle barycentrics and w is
//! the source triangle index. Unused records are zero with w = -1.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuEvent};

use crate::generators::mesh_common::{MeshVertex, Vec4Vertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const CUT_MAP_EXTRA_RECORDS: u32 = 196_608;
const MESH_VERTEX_SIZE: u64 = std::mem::size_of::<MeshVertex>() as u64;
const VEC4_VERTEX_SIZE: u64 = std::mem::size_of::<Vec4Vertex>() as u64;
const SHADER: &str = include_str!("shaders/mesh_cut_map.wgsl");

#[cfg(test)]
#[path = "../mesh_cut.rs"]
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

fn scratch_words(candidate_count: u32) -> Option<u64> {
    let blocks = u64::from(candidate_count)
        .checked_add(255)?
        .checked_div(256)?;
    u64::from(candidate_count).checked_add(blocks)
}

fn hash_key(values: &[u64]) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    for value in values {
        hash ^= *value;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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
    gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
    pipeline: &GpuComputePipeline,
    uniforms: &CutMapUniforms,
    map: &GpuBuffer,
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
        ],
        [capacity.div_ceil(256), 1, 1],
        "node.cut_mesh_map.clear",
    );
}

fn clear_invalid_output(
    ctx: &mut EffectNodeContext<'_, '_>,
    state: &mut CutMapState,
    map: &GpuBuffer,
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
    clear_map(gpu, pipeline, &uniforms, map);
}

fn report_completed_status(ctx: &mut EffectNodeContext<'_, '_>, status: &GpuBuffer) {
    let Some(ptr) = status.mapped_ptr() else {
        return;
    };
    let words = unsafe { std::slice::from_raw_parts(ptr as *const u32, 4) };
    if words[3] != 0 {
        let reason = match words[3] {
            1 => "output capacity exceeded",
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
    gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
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
    readback.signal = event.current_value();
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
    let Some(reference) = ctx.inputs.array("reference") else {
        ctx.error("node.cut_mesh_map: missing required `reference` input");
        return;
    };
    if ctx
        .inputs
        .slot("reference")
        .is_some_and(|slot| !ctx.inputs.slot_content_ready(slot))
    {
        ctx.mark_outputs_pending();
        return;
    }
    let Some(map) = ctx.outputs.array("map") else {
        ctx.error("node.cut_mesh_map: missing required `map` output");
        return;
    };
    let reference_capacity = reference.size / MESH_VERTEX_SIZE;
    let output_capacity = map.size / VEC4_VERTEX_SIZE;
    if reference_capacity > u32::MAX as u64 || output_capacity > u32::MAX as u64 {
        ctx.mark_outputs_pending();
        ctx.error("node.cut_mesh_map: input or output capacity exceeds GPU addressable range");
        return;
    }
    let reference_capacity = reference_capacity as u32;
    let output_capacity = output_capacity as u32;
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
        clear_invalid_output(ctx, state, map, output_capacity);
        ctx.mark_outputs_pending();
        ctx.error("node.cut_mesh_map: output capacity arithmetic overflow");
        return;
    };
    if output_capacity < capacity_contract {
        clear_invalid_output(ctx, state, map, output_capacity);
        ctx.mark_outputs_pending();
        ctx.error(format!("node.cut_mesh_map: output capacity {output_capacity} is below required reserve {capacity_contract}"));
        return;
    }
    let candidate_count = triangle_count;
    let reference_generation = ctx.inputs.slot_generation("reference").unwrap_or(0);
    let key = hash_key(&[
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
        reference_generation,
        reference.identity_key() as u64,
        map.identity_key() as u64,
        output_capacity as u64,
        ctx.rebuild_epoch,
    ]);
    if controls_valid && state.last_key == Some(key) {
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
    clear_map(gpu, clear_pipeline, &uniforms, map);
    if !controls_valid {
        ctx.error("node.cut_mesh_map: nonfinite cutter control");
        return;
    }
    if candidate_count == 0 {
        state.last_key = Some(key);
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

crate::primitive! {
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
    composition_notes: "Bands are clamped to 1..64 and direction is normalized with a +Y fallback. Only internal slab planes cut geometry; the first and last bands are unbounded. Every map record is xyz barycentrics plus w = exact source triangle index, with unused records set to zero/-1. All numeric controls are port-shadowed.",
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
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        cut_map_capacity(port_name, input_capacities)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        run_cut_map(ctx, 0, &mut self.state, 8.0, 0.2);
    }
}

crate::primitive! {
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
    composition_notes: "Cells use normalized source position `(position + source_offset) / abs(scale).max(1e-6)` and the exact shared quantization `floor(p / abs(cell_size).max(1e-6) + 0.5)`. Every map record is xyz barycentrics plus w = exact source triangle index, with unused records set to zero/-1. All numeric controls are port-shadowed.",
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
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        cut_map_capacity(port_name, input_capacities)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        run_cut_map(ctx, 1, &mut self.state, 8.0, 0.15);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

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

    #[test]
    fn wrappers_declare_reference_and_map_abi() {
        let mesh = crate::node_graph::ports::ArrayType::of_known::<MeshVertex>();
        let map = crate::node_graph::ports::ArrayType::of_known::<Vec4Vertex>();
        assert_eq!(CutMeshBands::TYPE_ID, "node.cut_mesh_bands");
        assert_eq!(CutMeshCells::TYPE_ID, "node.cut_mesh_cells");
        assert_eq!(
            CutMeshBands::INPUTS[0].ty,
            crate::node_graph::ports::PortType::Array(mesh)
        );
        assert_eq!(
            CutMeshBands::OUTPUTS[0].ty,
            crate::node_graph::ports::PortType::Array(map)
        );
        assert_eq!(
            CutMeshCells::INPUTS[0].ty,
            crate::node_graph::ports::PortType::Array(mesh)
        );
        assert_eq!(
            CutMeshCells::OUTPUTS[0].ty,
            crate::node_graph::ports::PortType::Array(map)
        );
    }

    #[test]
    fn map_shader_contains_all_deterministic_passes() {
        assert!(SHADER.contains("fn clear_main"));
        assert!(SHADER.contains("fn count_main"));
        assert!(SHADER.contains("fn scan_main"));
        assert!(SHADER.contains("fn emit_main"));
        assert!(SHADER.contains("source triangle index"));
    }

    #[cfg(feature = "gpu-proofs")]
    fn dispatch_shader(
        vertices: &[MeshVertex],
        mode: u32,
        bands: u32,
        output_capacity: u32,
    ) -> (Vec<Vec4Vertex>, [u32; 4]) {
        let device = crate::test_device();
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
            },
            MeshVertex {
                position: [1.0, 0.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
            },
            MeshVertex {
                position: [0.0, 1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
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
            },
            MeshVertex {
                position: [1.0, 0.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
            },
            MeshVertex {
                position: [0.0, 1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
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
            },
            MeshVertex {
                position: [1.0, 1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
            },
            MeshVertex {
                position: [-1.0, 1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0; 3],
                _pad1: 0.0,
                uv: [0.0; 2],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
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
