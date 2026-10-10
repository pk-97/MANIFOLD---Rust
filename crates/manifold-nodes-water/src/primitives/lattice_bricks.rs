//! `node.lattice_bricks` — the conservative occupied-brick list for the GPU
//! liquid surface.  The list is the only sparse-domain contract: its first
//! eight words are a dispatch header, followed by the dense brick mask and an
//! ascending compact list of active brick ids.
//!
//! The support test follows the FLIP Fluids mesher's positive distance band
//! and halo rule.  The implementation is an independent port of that design
//! (Museth 2013 and the FLIP Fluids `particlemesher.cpp` reference), not a
//! copied native implementation.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_water_liquid::primitives::prefix_scan::{PrefixScan, ScanLabels};
use manifold_water_liquid::float_param;
use manifold_water_liquid::primitives::sort_particles_into_cells::{bin_param, read_searched_bins};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_water_liquid::fluid_particles::{CellRange, FluidBlob};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

const SHADER: &str = include_str!("shaders/lattice_bricks.wgsl");
const HEADER_WORDS: u64 = 8;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BrickUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    cell_size: f32,
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    resolution_scale: u32,
    bins_x: u32,
    bins_y: u32,
    bins_z: u32,
    bricks_x: u32,
    bricks_y: u32,
    bricks_z: u32,
    brick_count: u32,
    band_extra: f32,
    blob_count: u32,
}

manifold_core::testkit_visible! {
/// Dimensions and storage size of one brick layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BrickLayout {
    pub nodes: [u32; 3],
    pub bricks: [u32; 3],
    pub count: u32,
    pub words: u32,
}
}

/// Public extra-field wrapper for the shared private PrefixScan state. The
/// macro exposes extra-field types through the generated node, so the wrapper
/// keeps the scan implementation private while satisfying that interface.
#[derive(Default)]
pub struct BrickScan {
    inner: PrefixScan,
}

impl BrickScan {
    fn prepare(&mut self, device: &manifold_gpu::GpuDevice) {
        self.inner.prepare(device);
    }

    fn buffer(&mut self, device: &manifold_gpu::GpuDevice, n: usize) -> Result<GpuBuffer, String> {
        self.inner.buffer(device, n).cloned()
    }

    fn encode(&self, encoder: &mut manifold_gpu::GpuEncoder, n: usize) {
        self.inner.encode_labelled(
            encoder,
            n,
            ScanLabels {
                blocks: "node.lattice_bricks.scan.blocks",
                add: "node.lattice_bricks.scan.add",
            },
        );
    }
}

/// Refined lattice dimensions shared with `node.particle_volume`.
pub(crate) fn refined_nodes(solid_nodes: [u32; 3], resolution_scale: u32) -> Option<[u32; 3]> {
    let scale = resolution_scale.clamp(1, 8);
    let mut nodes = [0; 3];
    for (axis, node) in solid_nodes.into_iter().enumerate() {
        nodes[axis] = node
            .max(2)
            .checked_sub(1)?
            .checked_mul(scale)?
            .checked_add(1)?;
    }
    Some(nodes)
}

manifold_core::testkit_visible! {
/// Compute the layout with checked integer arithmetic.  A missing layout is a
/// named capacity error at the node boundary rather than a wrapped dispatch.
pub(crate) fn brick_layout(solid_nodes: [u32; 3], resolution_scale: u32) -> Option<BrickLayout> {
    let nodes = refined_nodes(solid_nodes, resolution_scale)?;
    // Every dense sample index in the generated consumers is a u32.
    nodes[0].checked_mul(nodes[1])?.checked_mul(nodes[2])?;
    let bricks = nodes.map(|n| n.div_ceil(8));
    let count = bricks[0].checked_mul(bricks[1])?.checked_mul(bricks[2])?;
    let words = HEADER_WORDS
        .checked_add(u64::from(count).checked_mul(2)?)?
        .try_into()
        .ok()?;
    Some(BrickLayout {
        nodes,
        bricks,
        count,
        words,
    })
}
}

fn param_u32(params: &ParamValues, name: &str, default: u32) -> u32 {
    match params.get(name) {
        Some(ParamValue::Float(value)) if value.is_finite() => value.round().max(0.0) as u32,
        _ => default,
    }
}

fn resolution_from_params(params: &ParamValues) -> u32 {
    param_u32(params, "resolution_scale", 2).clamp(1, 8)
}

#[cfg(any(test, feature = "testkit"))]
const HALO_NODES: u32 = 5;

#[cfg(any(test, feature = "testkit"))]
fn expanded_bounds(
    brick: [u32; 3],
    layout: BrickLayout,
    lattice_min: [f32; 3],
    size: [f32; 3],
) -> ([f32; 3], [f32; 3]) {
    let mut lo = [0.0; 3];
    let mut hi = [0.0; 3];
    for axis in 0..3 {
        let first = brick[axis] * 8;
        let last = ((brick[axis] + 1) * 8 - 1).min(layout.nodes[axis] - 1);
        lo[axis] = lattice_min[axis]
            + first.saturating_sub(HALO_NODES) as f32 * size[axis]
                / (layout.nodes[axis] - 1) as f32;
        hi[axis] = lattice_min[axis]
            + (last + HALO_NODES).min(layout.nodes[axis] - 1) as f32 * size[axis]
                / (layout.nodes[axis] - 1) as f32;
    }
    (lo, hi)
}

#[cfg(any(test, feature = "testkit"))]
fn intersects_support(center: [f32; 3], support: [f32; 3], lo: [f32; 3], hi: [f32; 3]) -> bool {
    (0..3).all(|axis| {
        let distance = (lo[axis] - center[axis]).max(center[axis] - hi[axis]).max(0.0);
        distance <= support[axis]
    })
}

/// CPU reference for the GPU mark pass.  This deliberately uses the blob's
/// bounding support, not its anisotropic matrix, so every possible level-set
/// crossing is retained even when a shape is stretched.
#[cfg(any(test, feature = "testkit"))]
pub fn conservative_brick_mask(
    blobs: &[FluidBlob],
    center: [f32; 3],
    size: [f32; 3],
    solid_nodes: [u32; 3],
    resolution_scale: u32,
    _cell_size: f32,
) -> Option<Vec<u32>> {
    let layout = brick_layout(solid_nodes, resolution_scale)?;
    let lattice_min = [
        center[0] - size[0] * 0.5,
        center[1] - size[1] * 0.5,
        center[2] - size[2] * 0.5,
    ];
    let h: [f32; 3] = std::array::from_fn(|a| size[a] / (layout.nodes[a] - 1) as f32);
    let mut mask = vec![0u32; layout.count as usize];
    for id in 0..layout.count {
        let brick = [
            id % layout.bricks[0],
            (id / layout.bricks[0]) % layout.bricks[1],
            id / (layout.bricks[0] * layout.bricks[1]),
        ];
        if brick
            .iter()
            .enumerate()
            .any(|(axis, &v)| v == 0 || v + 1 == layout.bricks[axis])
        {
            mask[id as usize] = 1;
            continue;
        }
        let (lo, hi) = expanded_bounds(brick, layout, lattice_min, size);
        if blobs.iter().any(|blob| {
            let reach = blob.center_radius[3];
            reach > 0.0
                && intersects_support(
                    [
                        blob.center_radius[0],
                        blob.center_radius[1],
                        blob.center_radius[2],
                    ],
                    h.map(|spacing| 1.5 * reach + spacing),
                    lo,
                    hi,
                )
        }) {
            mask[id as usize] = 1;
        }
    }
    Some(mask)
}

/// Compact an inclusive mask into the same ascending layout emitted by the
/// GPU.  The zeroed tail models retired bricks after a frame shrinks.
#[cfg(any(test, feature = "testkit"))]
pub fn compact_brick_words(mask: &[u32], bricks: [u32; 3]) -> Vec<u32> {
    let mut ids = Vec::with_capacity(mask.iter().filter(|&&v| v != 0).count());
    for (id, &active) in mask.iter().enumerate() {
        if active != 0 {
            ids.push(id as u32);
        }
    }
    let count = ids.len() as u32;
    let n = mask.len();
    let mut words = vec![0u32; HEADER_WORDS as usize + 2 * n];
    words[0] = count;
    words[1] = count * 2;
    words[2] = 1;
    words[3] = 1;
    words[4..7].copy_from_slice(&bricks);
    words[8..8 + n].copy_from_slice(mask);
    words[8 + n..8 + n + ids.len()].copy_from_slice(&ids);
    words
}

manifold_node_engine::primitive! {
    name: LatticeBricks,
    type_id: "node.lattice_bricks",
    purpose: "Build the conservative occupied-brick list for a liquid level-set lattice. The output is eight header words, a dense 0/1 brick mask, and an ascending compact brick-id list; the mark includes blob support, the positive exterior band, all smoothing and gradient halos, and every domain border so sparse consumers retain the dense field support.",
    inputs: {
        blobs: Array(FluidBlob) required,
        cell_ranges: Array(CellRange) required,
        solid: Array(f32) required,
        bounds: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        band_extra: ScalarF32 optional,
        bins_x: ScalarF32 optional, bins_y: ScalarF32 optional, bins_z: ScalarF32 optional,
    },
    outputs: {
        bricks: Array(u32),
    },
    params: [
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("nodes_x", "Solid Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
        ParamDef { name: Cow::Borrowed("resolution_scale"), label: "Resolution Scale", ty: ParamType::Int, default: ParamValue::Float(2.0), range: Some((1.0, 8.0)), enum_values: &[] },
        bin_param!("bins_x", "Bins X"),
        bin_param!("bins_y", "Bins Y"),
        bin_param!("bins_z", "Bins Z"),
        float_param!("band_extra", "Extra Distance Band", 0.0, 0.0, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire blobs and cell_ranges from the particle surface's sort and shape nodes, bounds from the node.blob_bounds that feeds node.particle_volume, solid from the same lattice capacity as node.particle_volume, and center/size/nodes/cell_size/bins from those same producers. Feed the output to every sparse liquid-surface stage. The header's indirect grid is [2*active_count,1,1], the dense mask clears retired bricks each frame, and the boundary bricks stay active even for an empty frame. This is a barriered producer with a PrefixScan, so it is a graph fusion boundary.",
    examples: [],
    picker: { label: "Lattice Bricks", category: Atom },
    summary: "Finds the lattice bricks that can affect the liquid surface.",
    category: Particles3D,
    role: Filter,
    aliases: ["occupied bricks", "sparse liquid lattice", "surface brick list"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        scatter: Option<GpuComputePipeline> = None,
        mark: Option<GpuComputePipeline> = None,
        compact: Option<GpuComputePipeline> = None,
        scan: BrickScan = BrickScan::default(),
        bricks: Option<GpuBuffer> = None,
        hits: Option<GpuBuffer> = None,
    },
}

impl Primitive for LatticeBricks {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "bricks"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "bricks").then_some(self.bricks.as_ref()).flatten()
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "bricks").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        {
            let gpu = ctx.gpu_encoder();
            if self.scatter.is_none() {
                self.scatter = Some(gpu.device.create_compute_pipeline(
                    SHADER,
                    "scatter_bricks",
                    "node.lattice_bricks",
                ));
            }
            if self.mark.is_none() {
                self.mark = Some(gpu.device.create_compute_pipeline(
                    SHADER,
                    "mark_bricks",
                    "node.lattice_bricks",
                ));
            }
            if self.compact.is_none() {
                self.compact = Some(gpu.device.create_compute_pipeline(
                    SHADER,
                    "compact_bricks",
                    "node.lattice_bricks",
                ));
            }
            self.scan.prepare(gpu.device);
        }
        let (Some(blobs), Some(ranges), Some(solid), Some(bounds)) = (
            ctx.inputs.array("blobs"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("bounds"),
        ) else {
            return;
        };
        let solid_nodes = ["nodes_x", "nodes_y", "nodes_z"]
            .map(|name| ctx.scalar_or_param(name, 2.0).round().max(2.0) as u32);
        let scale = resolution_from_params(ctx.params);
        let Some(layout) = brick_layout(solid_nodes, scale) else {
            ctx.error("Lattice Bricks: lattice dimensions overflow brick storage");
            return;
        };
        let expected_words = u64::from(layout.words);
        let solid_nodes_total = u64::from(solid_nodes[0])
            .saturating_mul(u64::from(solid_nodes[1]))
            .saturating_mul(u64::from(solid_nodes[2]));
        if solid_nodes_total > solid.size / 4 {
            ctx.error("Lattice Bricks: solid capacity is smaller than nodes_x*nodes_y*nodes_z");
            return;
        }
        let Some(bins) = (match read_searched_bins(ctx, ranges.size, "Lattice Bricks") {
            Ok(value) => value,
            Err(error) => {
                ctx.error(error);
                return;
            }
        }) else {
            return;
        };

        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] =
            ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        if bounds.size != 8 {
            ctx.error("Lattice Bricks: bounds must contain the two words from Blob Bounds");
            return;
        }
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let allocation_error = {
            let gpu = ctx.gpu_encoder();
            let bytes = expected_words * 4;
            if self
                .bricks
                .as_ref()
                .is_none_or(|buffer| buffer.size != bytes)
            {
                match gpu.device.try_create_buffer_shared(bytes.max(16)) {
                    Ok(buffer) => {
                        self.bricks = Some(buffer);
                        None
                    }
                    Err(error) => Some(error),
                }
            } else {
                None
            }
        };
        if let Some(error) = allocation_error {
            ctx.error(format!("Lattice Bricks: output allocation failed: {error}"));
            return;
        }
        let Some(bricks) = self.bricks.as_ref().cloned() else {
            return;
        };
        let hits_bytes = (u64::from(layout.count) * 4).max(16);
        if self.hits.as_ref().is_none_or(|buffer| buffer.size != hits_bytes) {
            let gpu = ctx.gpu_encoder();
            match gpu.device.try_create_buffer_shared(hits_bytes) {
                // The mark pass zeroes every word it reads, so only a new
                // allocation needs clearing before the first scatter.
                Ok(buffer) => {
                    gpu.native_enc.clear_buffer(&buffer);
                    self.hits = Some(buffer);
                }
                Err(error) => {
                    ctx.error(format!("Lattice Bricks: hit storage allocation failed: {error}"));
                    return;
                }
            }
        }
        let Some(hits) = self.hits.as_ref().cloned() else {
            return;
        };
        let blob_count = u32::try_from(blobs.size / std::mem::size_of::<FluidBlob>() as u64).unwrap_or(u32::MAX);
        let (scan_buffer, scan_error) = {
            let gpu = ctx.gpu_encoder();
            match self.scan.buffer(gpu.device, layout.count as usize) {
                Ok(buffer) => (Some(buffer), None),
                Err(error) => (None, Some(error)),
            }
        };
        if let Some(error) = scan_error {
            ctx.error(format!(
                "Lattice Bricks: prefix storage allocation failed: {error}"
            ));
            return;
        }
        let scan_buffer = scan_buffer.expect("scan buffer is present without an allocation error");
        let uniforms = BrickUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            cell_size,
            nodes_x: layout.nodes[0],
            nodes_y: layout.nodes[1],
            nodes_z: layout.nodes[2],
            resolution_scale: scale,
            bins_x: bins[0],
            bins_y: bins[1],
            bins_z: bins[2],
            bricks_x: layout.bricks[0],
            bricks_y: layout.bricks[1],
            bricks_z: layout.bricks[2],
            brick_count: layout.count,
            band_extra: ctx.scalar_or_param("band_extra", 0.0),
            blob_count,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: blobs,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &scan_buffer,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &bricks,
                offset: 0,
            },
            GpuBinding::Buffer { binding: 6, buffer: &hits, offset: 0 },
        ];
        let groups = [layout.count.div_ceil(256), 1, 1];
        let gpu = ctx.gpu_encoder();
        if blob_count > 0 {
            gpu.native_enc.dispatch_compute(
                self.scatter.as_ref().expect("scatter pipeline prepared"),
                &bindings,
                [blob_count.div_ceil(256), 1, 1],
                "node.lattice_bricks.scatter",
            );
            gpu.native_enc.compute_memory_barrier_buffers();
        }
        gpu.native_enc.dispatch_compute(
            self.mark.as_ref().expect("mark pipeline prepared"),
            &bindings,
            groups,
            "node.lattice_bricks.mark",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        self.scan.encode(gpu.native_enc, layout.count as usize);
        gpu.native_enc.compute_memory_barrier_buffers();
        gpu.native_enc.dispatch_compute(
            self.compact.as_ref().expect("compact pipeline prepared"),
            &bindings,
            groups,
            "node.lattice_bricks.compact",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(x: f32, y: f32, z: f32, reach: f32) -> FluidBlob {
        FluidBlob {
            center_radius: [x, y, z, reach],
            ..FluidBlob::default()
        }
    }

    #[test]
    fn brick_layout_uses_ceil_eight_and_exact_two_section_storage() {
        let layout = brick_layout([17, 9, 2], 2).expect("small layout");
        assert_eq!(layout.nodes, [33, 17, 3]);
        assert_eq!(layout.bricks, [5, 3, 1]);
        assert_eq!(layout.count, 15);
        assert_eq!(layout.words, 38);
    }

    #[test]
    fn conservative_mask_contains_brute_force_support_on_noncubic_grid() {
        let nodes = [41, 49, 57];
        let center = [0.0; 3];
        let size = [8.0, 9.0, 10.0];
        let cell_size = 0.25;
        let blobs = [
            blob(-2.2, -1.4, -2.0, 0.20),
            blob(1.8, 0.7, 2.1, 0.55),
            blob(0.5, -2.0, 1.0, 0.35),
        ];
        let mask = conservative_brick_mask(&blobs, center, size, nodes, 1, cell_size).unwrap();
        let layout = brick_layout(nodes, 1).unwrap();
        assert_eq!(layout.bricks, [6, 7, 8]);
        assert!(
            mask.contains(&0),
            "small supports leave interior bricks inactive"
        );
        for (id, &value) in mask.iter().enumerate() {
            let brick = [
                id as u32 % layout.bricks[0],
                (id as u32 / layout.bricks[0]) % layout.bricks[1],
                id as u32 / (layout.bricks[0] * layout.bricks[1]),
            ];
            if brick
                .iter()
                .enumerate()
                .any(|(axis, &v)| v == 0 || v + 1 == layout.bricks[axis])
            {
                assert_eq!(value, 1, "domain border brick {brick:?}");
            }
        }

        // Brute force every lattice sample against every spherical support.
        // Any sample that can differ from the canonical exterior must land in
        // an active brick; the AABB/halo test may retain additional bricks.
        let min = [-4.0, -4.5, -5.0];
        for z in 0..nodes[2] {
            for y in 0..nodes[1] {
                for x in 0..nodes[0] {
                    let at = [x, y, z];
                    if blobs.iter().any(|item| {
                        (0..3).all(|a| {
                            let h = size[a] / (nodes[a] - 1) as f32;
                            let lo = ((item.center_radius[a] - 1.5*item.center_radius[3] - min[a])/h).floor();
                            let hi = ((item.center_radius[a] + 1.5*item.center_radius[3] - min[a])/h).floor()+1.0;
                            at[a] as f32 >= lo && at[a] as f32 <= hi
                        })
                    }) {
                        // Three smoothing taps on each axis, one gradient
                        // neighbour, and the other corner of an owned cell.
                        for hz in z.saturating_sub(5)..=(z + 5).min(nodes[2] - 1) {
                            for hy in y.saturating_sub(5)..=(y + 5).min(nodes[1] - 1) {
                                for hx in x.saturating_sub(5)..=(x + 5).min(nodes[0] - 1) {
                                    let b = [hx / 8, hy / 8, hz / 8];
                                    let id =
                                        b[0] + layout.bricks[0] * (b[1] + layout.bricks[1] * b[2]);
                                    assert_eq!(
                                        mask[id as usize], 1,
                                        "sample {at:?} lost halo [{hx},{hy},{hz}]"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        let moved = conservative_brick_mask(
            &[blob(-0.2, 2.6, 2.8, 0.15)],
            center,
            size,
            nodes,
            1,
            cell_size,
        )
        .unwrap();
        assert_ne!(
            mask, moved,
            "changing support position and radius changes interior occupancy"
        );
    }

    #[test]
    fn candidate_bins_cover_volume_home_plus_one_for_every_five_node_halo() {
        let nodes = [41, 49, 57];
        let layout = brick_layout(nodes, 1).unwrap();
        let min = [-4.0_f32, -4.5, -5.0];
        let size = [8.0_f32, 9.0, 10.0];
        let cell = 0.25_f32;
        let bins = [32_u32, 36, 40];
        for id in 0..layout.count {
            let brick = [
                id % layout.bricks[0],
                (id / layout.bricks[0]) % layout.bricks[1],
                id / (layout.bricks[0] * layout.bricks[1]),
            ];
            if brick
                .iter()
                .enumerate()
                .any(|(axis, &v)| v == 0 || v + 1 == layout.bricks[axis])
            {
                continue;
            }
            let (lo, hi) = expanded_bounds(brick, layout, min, size);
            for axis in 0..3 {
                let first = ((lo[axis] - min[axis]) / cell).floor() as i32 - 1;
                let last = ((hi[axis] - min[axis]) / cell).floor() as i32 + 1;
                let first = first.clamp(0, bins[axis] as i32 - 1);
                let last = last.clamp(0, bins[axis] as i32 - 1);
                let node_first = brick[axis] * 8;
                let node_last = ((brick[axis] + 1) * 8 - 1).min(nodes[axis] - 1);
                for node in node_first.saturating_sub(5)..=(node_last + 5).min(nodes[axis] - 1) {
                    let point = min[axis] + node as f32 * size[axis] / (nodes[axis] - 1) as f32;
                    let home = (((point - min[axis]) / cell).floor() as i32)
                        .clamp(0, bins[axis] as i32 - 1);
                    // The volume skips bins outside the domain after adding
                    // each offset to its clamped home bin.
                    for candidate in (home - 1..=home + 1)
                        .filter(|&b| b >= 0 && b < bins[axis] as i32)
                    {
                        assert!(first <= candidate && candidate <= last,
                            "brick {brick:?} axis {axis} node {node}: [{first},{last}] misses bin {candidate}");
                    }
                }
            }
        }
    }

    /// The shader's distance band: the extra band plus one lattice diagonal
    /// when the band is on.
    fn band(layout: BrickLayout, size: [f32; 3], band_extra: f32) -> f32 {
        let h: [f32; 3] = std::array::from_fn(|a| size[a] / (layout.nodes[a] - 1) as f32);
        band_extra + if band_extra > 0.0 { h.iter().map(|v| v * v).sum::<f32>().sqrt() } else { 0.0 }
    }

    /// `scatter_bricks`' candidate brick range per axis, inclusive; an empty
    /// range (first past last) for the blobs it skips.
    fn scatter_candidates(blob: &FluidBlob, layout: BrickLayout, center: [f32; 3], size: [f32; 3], band_extra: f32) -> [(u32, u32); 3] {
        const ROUNDING: f32 = 1.907_348_6e-6;
        const LIMIT: f32 = 1.0e18;
        const MIN_SPACING: f32 = 1.0e-15;
        let reach = blob.center_radius[3];
        if reach.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) || blob.center_radius[..3].iter().any(|v| !v.is_finite()) {
            return [(1, 0); 3];
        }
        let full = std::array::from_fn(|a| (0, layout.bricks[a] - 1));
        let lattice_min: [f32; 3] = std::array::from_fn(|a| center[a] - 0.5 * size[a]);
        let gaps: [f32; 3] = std::array::from_fn(|a| (layout.nodes[a] - 1) as f32);
        let bounded = reach < LIMIT && band_extra.abs() < LIMIT
            && (0..3).all(|a| blob.center_radius[a].abs() < LIMIT && center[a].abs() < LIMIT
                && size[a] < LIMIT && size[a] > MIN_SPACING * gaps[a]);
        if !bounded {
            return full;
        }
        let extra = band(layout, size, band_extra);
        let axes: [Option<(f32, f32)>; 3] = std::array::from_fn(|a| {
            let h = size[a] / gaps[a];
            let support = 1.5 * reach + extra + h;
            let centre = blob.center_radius[a];
            let magnitude = lattice_min[a].abs().max((lattice_min[a] + size[a]).abs()).max(centre.abs().max(support));
            if (magnitude / h).partial_cmp(&1.0e30) != Some(std::cmp::Ordering::Less) {
                return None;
            }
            let slack = (magnitude * ROUNDING / h).ceil() + 2.0;
            let from_min = centre - lattice_min[a];
            let lo_node = ((from_min - support) / h).floor() - slack - (5 + 7) as f32;
            let hi_node = ((from_min + support) / h).floor() + slack + 5.0;
            Some((lo_node, hi_node))
        });
        if axes.iter().any(Option::is_none) {
            return full;
        }
        std::array::from_fn(|a| {
            let last = (layout.bricks[a] - 1) as f32;
            let (lo_node, hi_node) = axes[a].expect("checked above");
            ((lo_node / 8.0).floor().clamp(0.0, last) as u32, (hi_node / 8.0).floor().clamp(0.0, last) as u32)
        })
    }

    /// Asserts every interior brick the hit test accepts for each blob lies in
    /// the range that blob scatters to; returns how many were accepted.
    fn assert_scatter_covers(layout: BrickLayout, center: [f32; 3], size: [f32; 3], band_extra: f32, blobs: &[FluidBlob]) -> usize {
        let lattice_min: [f32; 3] = std::array::from_fn(|a| center[a] - size[a] * 0.5);
        let extra = band(layout, size, band_extra);
        let h: [f32; 3] = std::array::from_fn(|a| size[a] / (layout.nodes[a] - 1) as f32);
        let mut accepted = 0;
        for item in blobs {
            let p = [item.center_radius[0], item.center_radius[1], item.center_radius[2]];
            let reach = item.center_radius[3];
            let range = scatter_candidates(item, layout, center, size, band_extra);
            for id in 0..layout.count {
                let brick = [id % layout.bricks[0], (id / layout.bricks[0]) % layout.bricks[1], id / (layout.bricks[0] * layout.bricks[1])];
                if brick.iter().enumerate().any(|(axis, &v)| v == 0 || v + 1 == layout.bricks[axis]) {
                    continue;
                }
                let (lo, hi) = expanded_bounds(brick, layout, lattice_min, size);
                if intersects_support(p, h.map(|spacing| 1.5 * reach + extra + spacing), lo, hi) {
                    accepted += 1;
                    assert!((0..3).all(|a| range[a].0 <= brick[a] && brick[a] <= range[a].1),
                        "lattice {center:?}+{size:?}: blob {p:?} reach {reach} reaches brick {brick:?} outside its scatter range {range:?}");
                }
            }
        }
        accepted
    }

    /// Every interior brick the gather's hit test accepts lies in the range
    /// the blob scatters to, so the scatter's mask is the gather's. Covers
    /// lattices far from the origin with cells near f32 resolution, where the
    /// hit test's own rounding is many nodes wide, and huge finite blobs.
    #[test]
    fn scatter_candidates_cover_every_brick_the_hit_test_accepts() {
        let mut state = 0x2545_f491_u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32
        };
        let lattices = [
            ([67, 67, 67], 1, 0.0, [0.25, -0.5, 0.125], [4.0, 2.25, 3.0]),
            ([67, 67, 67], 1, 0.09, [0.25, -0.5, 0.125], [4.0, 2.25, 3.0]),
            ([67, 35, 51], 2, 0.05, [0.25, -0.5, 0.125], [4.0, 2.25, 3.0]),
            ([20, 20, 20], 3, 0.0, [0.25, -0.5, 0.125], [4.0, 2.25, 3.0]),
            ([257, 25, 25], 1, 0.0, [1000.0, 1000.0, 1000.0], [0.001, 0.001, 0.001]),
            ([67, 41, 33], 1, 0.0, [-3000.0, 512.0, 20000.0], [0.05, 0.02, 0.4]),
            ([33, 33, 33], 2, 0.03, [77.0, -1e5, 3.0], [0.5, 0.25, 1.0]),
        ];
        for (solid_nodes, scale, band_extra, center, size) in lattices {
            let layout = brick_layout(solid_nodes, scale).unwrap();
            let h: [f32; 3] = std::array::from_fn(|a| size[a] / (layout.nodes[a] - 1) as f32);
            let blobs: Vec<FluidBlob> = (0..400).map(|_| {
                // Inside the lattice and up to a cell past it on every side;
                // reaches from a hundredth of a node up to a few nodes.
                let p: [f32; 3] = std::array::from_fn(|a| center[a] - 0.5 * size[a] - h[a] + next() * (size[a] + 2.0 * h[a]));
                blob(p[0], p[1], p[2], h[0] * (0.01 + 2.5 * next()))
            }).collect();
            let accepted = assert_scatter_covers(layout, center, size, band_extra, &blobs);
            assert!(accepted > 0, "lattice {center:?}+{size:?} must exercise interior bricks");
        }

        // Review counterexamples: a tiny blob on a lattice whose node spacing
        // is below the f32 resolution of its coordinates; a finite blob near
        // f32 max whose support covers the whole lattice; a lattice and blob
        // whose candidate bounds overflow to NaN; and a lattice so large the
        // hit test's own multiply overflows to infinity.
        let rounding = brick_layout([257, 25, 25], 1).unwrap();
        assert!(assert_scatter_covers(rounding, [1000.0; 3], [0.001; 3], 0.0, &[blob(1000.0, 1000.0, 1000.0, 1e-7)]) > 0);
        let huge = brick_layout([25, 25, 25], 1).unwrap();
        assert!(assert_scatter_covers(huge, [0.0; 3], [4.0; 3], 0.0, &[blob(3.1e38, 0.0, 0.0, 2.1e38)]) > 0);
        assert!(assert_scatter_covers(huge, [3e38; 3], [4.0; 3], 0.0, &[blob(-3e38, -3e38, -3e38, 3e38)]) > 0);
        assert!(assert_scatter_covers(huge, [0.0; 3], [1e38; 3], 0.0, &[blob(2e38, 2e38, 2e38, 1.0)]) > 0);
        // A huge negative band shrinks the support below zero: nothing to reach.
        assert_eq!(assert_scatter_covers(huge, [0.0; 3], [4.0; 3], -3e38, &[blob(0.0, 0.0, 0.0, 1.0)]), 0);
        // Normal inputs whose node spacing divides down to a subnormal.
        let u = f32::from_bits(1);
        let tiny = 16_809_984.0 * u;
        let subnormal = brick_layout([65537, 25, 25], 1).unwrap();
        assert!(assert_scatter_covers(subnormal, [tiny / 2.0; 3], [tiny; 3], 0.0, &[blob(tiny, tiny / 2.0, tiny / 2.0, 8_388_608.0 * u)]) > 0);
    }

    #[test]
    fn compact_list_is_ascending_unique_and_clears_retired_tail() {
        let words = compact_brick_words(&[0, 1, 1, 0, 1], [5, 1, 1]);
        assert_eq!(&words[0..8], &[3, 6, 1, 1, 5, 1, 1, 0]);
        assert_eq!(&words[8..13], &[0, 1, 1, 0, 1]);
        assert_eq!(&words[13..18], &[1, 2, 4, 0, 0]);
    }

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("lattice brick shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("lattice brick shader validates");
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
