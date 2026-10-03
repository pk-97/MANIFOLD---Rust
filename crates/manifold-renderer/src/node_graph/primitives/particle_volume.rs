//! Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.particle_volume` — the liquid level set: one value per lattice node,
//! the distance to the nearest anisotropic kernel in the node's bins
//! (GPU_FLUID_SURFACE_DESIGN.md D8, D15, D18, P6e). A per-element gather on
//! the codegen path. The optional interior field follows Ferstl et al.,
//! "Narrow Band FLIP for Liquid Simulations", CGF 35(2), 2016, Eq. 4.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::liquid_bricks;
use super::sort_particles_into_cells::{bin_param, float_param, read_searched_bins};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{CellRange, FluidBlob};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then derived `brick_pass`
/// and `interior_len`, then `dispatch_count` and 16-byte alignment padding.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VolumeUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    resolution_scale: i32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    band_extra: f32,
    brick_pass: u32,
    interior_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

/// Level-set nodes per axis: `(n − 1)·m + 1` over the solid lattice's box.
pub(crate) fn refined_nodes(solid_nodes: [f32; 3], scale: u32) -> [u32; 3] {
    solid_nodes.map(|n| (n.max(2.0) as u32 - 1) * scale + 1)
}

/// Resolution Scale: level-set nodes per solid-lattice cell.
pub(crate) fn volume_scale(params: &ParamValues) -> u32 {
    match params.get("resolution_scale") {
        Some(ParamValue::Float(v)) => v.round().clamp(1.0, 8.0) as u32,
        _ => 2,
    }
}

crate::primitive! {
    name: ParticleVolume,
    type_id: "node.particle_volume",
    purpose: "The liquid's level set on a lattice: at each node, the distance to the nearest kernel ellipsoid, a·(|G·(x − c)| − 1) with a the kernel's longest axis (exact for spheres), negative inside and initialized to three times the largest kernel radius; each kernel visits the inclusive FLIP grid support box from floor((centre - 1.5r)/spacing) through floor((centre + 1.5r)/spacing)+1; positive band_extra expands support and the exterior distance by that distance plus one lattice-cell diagonal so an offset crossing retains its interpolation support. The lattice is the solid lattice (nodes_x/y/z over the center/size box) refined resolution_scale times per cell. Nodes inside a solid are never inside the liquid; borders follow the same solid-SDF rule as the production FLIP mesher (the preview-only +0.001 closure is not applied). The largest kernel radius and support come from node.blob_bounds on the same blobs.",
    inputs: {
        blobs: Array(FluidBlob) required,
        cell_ranges: Array(CellRange) required,
        solid: Array(f32) required,
        bricks: Array(u32) optional,
        interior: Array(f32) optional,
        bounds: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        band_extra: ScalarF32 optional,
        bins_x: ScalarF32 optional, bins_y: ScalarF32 optional, bins_z: ScalarF32 optional,
    },
    outputs: {
        levelset: Array(f32),
        volume_nodes_x: ScalarF32, volume_nodes_y: ScalarF32, volume_nodes_z: ScalarF32,
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
        ParamDef {
            name: Cow::Borrowed("resolution_scale"),
            label: "Resolution Scale",
            ty: ParamType::Int,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 4.0)),
            enum_values: &[],
        },
        bin_param!("bins_x", "Bins X"),
        bin_param!("bins_y", "Bins Y"),
        bin_param!("bins_z", "Bins Z"),
        float_param!("band_extra", "Extra Distance Band", 0.0, 0.0, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire blobs from node.shape_particle_blobs and bounds from a node.blob_bounds fed the same blobs, cell_ranges and bins_x/y/z from the same node.sort_particles_into_cells (the bins are the sort's, never worked out again on the GPU; cell_ranges must hold one range per bin or nothing runs, a named error; all three unwired takes the sort's CPU rule on the shared box, checked the same way), and the producer's solid lattice (solid_b, grid_nodes_x/y/z, grid_bounds through node.transform_components). Optionally wire the narrow-band solver's cell-centred interior distance, with exactly one value per authored physical cell: the solid lattice has three padding nodes on every side, so the extent is (solid_nodes - 7)^3; an unwired input preserves the dense particle path. resolution_scale sets mesh detail (2–4 per simulation cell) and the allocation (solid capacity × scale³); it is not a live wire. The blobs' particle_scale sets how far the surface sits from the particles. volume_nodes_x/y/z carry the refined lattice to node.count_surface_triangles and node.volume_surface_mesh.",
    examples: [],
    picker: { label: "Particle Volume", category: Atom },
    summary: "Turns liquid particles into a distance field on a grid, the step before the surface mesh is drawn.",
    category: Particles3D,
    role: Filter,
    aliases: ["level set", "signed distance", "liquid field", "scalar field"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/particle_volume_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    derived_uniforms: ["brick_pass:u32", "interior_len:u32"],
    wgsl_includes: [liquid_bricks::COMMON],
    buffer_index: "liquid_brick_index",
}

// Per-frame recompute for a fused region's derived block. An unwired
// interior is the baseline path; wired extents are checked by the liquid planner.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.particle_volume",
        array_ports: &["interior"],
        recompute: |ctx| {
            Some(vec![0.0, (ctx.array_len)("interior").unwrap_or(0) as f32])
        },
    }
}

impl Primitive for ParticleVolume {
    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        if port != "levelset" {
            return None;
        }
        let solid = inputs
            .iter()
            .find(|(name, _)| *name == "solid")
            .map(|&(_, n)| n)?;
        Some(solid.saturating_mul(volume_scale(params).pow(3)))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes =
            ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let scale = volume_scale(ctx.params);
        let lattice_valid = nodes.iter().all(|&n| n >= 2.0);
        let refined = if lattice_valid {
            refined_nodes(nodes, scale)
        } else {
            [0; 3]
        };
        for (port, value) in ["volume_nodes_x", "volume_nodes_y", "volume_nodes_z"]
            .into_iter()
            .zip(refined)
        {
            ctx.outputs
                .set_scalar(port, ParamValue::Float(value as f32));
        }
        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] =
            ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let uniforms = VolumeUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            cell_size: ctx.scalar_or_param("cell_size", 0.0625),
            resolution_scale: scale as i32,
            bins_x: 0,
            bins_y: 0,
            bins_z: 0,
            band_extra: ctx.scalar_or_param("band_extra", 0.0),
            brick_pass: 0,
            interior_len: 0,
            dispatch_count: 0,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        if !lattice_valid {
            ctx.error(format!(
                "Particle Volume: a {}×{}×{} solid lattice has fewer than 2 nodes on an axis. Wire nodes_x/y/z from the same producer as solid.",
                nodes[0], nodes[1], nodes[2]
            ));
            return;
        }
        let (Some(blobs), Some(ranges), Some(solid), Some(bounds), Some(levelset)) = (
            ctx.inputs.array("blobs"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("bounds"),
            ctx.outputs.array("levelset"),
        ) else {
            return;
        };
        let total = refined.iter().map(|&n| u64::from(n)).product::<u64>();
        let capacity = levelset.size / 4;
        let solid_total = nodes.iter().map(|&n| n as u64).product::<u64>();
        if total > capacity || solid_total > solid.size / 4 {
            ctx.error(format!(
                "Particle Volume: a {}×{}×{} lattice needs {total} nodes; storage holds {capacity}. Wire nodes_x/y/z from the same producer as solid.",
                refined[0], refined[1], refined[2]
            ));
            return;
        }
        let interior_input = ctx.inputs.array("interior");
        if interior_input.is_some_and(|buffer| buffer.size == 0) {
            ctx.error("Particle Volume: interior is wired but has zero extent; omit the wire for the baseline path or provide one f32 per solid cell.");
            return;
        }
        let interior_wired = interior_input;
        if interior_wired.is_some() && nodes.iter().any(|&n| n < 8.0) {
            ctx.error(format!(
                "Particle Volume: interior needs at least 8 solid nodes on every axis (three padding nodes per side); got {}×{}×{}.",
                nodes[0], nodes[1], nodes[2]
            ));
            return;
        }
        let interior_total = nodes
            .map(|n| (n as u64).saturating_sub(7))
            .into_iter()
            .product::<u64>();
        let (interior, interior_len) = if let Some(buffer) = interior_wired {
            let actual = buffer.size / 4;
            if !buffer.size.is_multiple_of(4) || actual != interior_total {
                ctx.error(format!(
                    "Particle Volume: interior has {actual} f32 values; expected {interior_total} cell-centred values for the {}×{}×{} physical cells inside the padded solid lattice.",
                    nodes[0] - 7.0, nodes[1] - 7.0, nodes[2] - 7.0
                ));
                return;
            }
            (buffer, actual as u32)
        } else {
            // Keep the binding ABI total while making the optional input inert.
            (solid, 0)
        };
        let bins = match read_searched_bins(ctx, ranges.size, "Particle Volume") {
            Ok(Some(bins)) => bins,
            Ok(None) => return,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let [bins_x, bins_y, bins_z] = bins.map(|n| n as i32);
        let uniforms = VolumeUniforms {
            bins_x,
            bins_y,
            bins_z,
            interior_len,
            brick_pass: 0,
            dispatch_count: total as u32,
            ..uniforms
        };
        if bounds.size != 8 {
            ctx.error("Particle Volume: bounds must contain the two words from Blob Bounds");
            return;
        }
        let bricks = ctx.inputs.array("bricks");
        if bricks.is_some_and(|b| !liquid_bricks::valid_schedule(b, refined)) {
            ctx.error("Liquid lattice: brick schedule does not match the lattice dimensions");
            return;
        }
        let gpu = ctx.gpu_encoder();
        for pass in 0..if bricks.is_some() { 2 } else { 1 } {
            let uniforms = VolumeUniforms {
                brick_pass: if bricks.is_some() { 2 - pass } else { 0 },
                ..uniforms
            };
            liquid_bricks::dispatch(
                gpu.native_enc,
                pipeline,
                &[
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
                        binding: 2,
                        buffer: ranges,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: solid,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: bricks.unwrap_or(solid),
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 5,
                        buffer: interior,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 6,
                        buffer: bounds,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 7,
                        buffer: levelset,
                        offset: 0,
                    },
                ],
                bricks,
                uniforms.brick_pass,
                total as u32,
                "node.particle_volume",
            );
        }
    }
}

#[cfg(test)]
mod cpu_tests {
    fn index(p: [usize; 3], n: [usize; 3]) -> usize {
        p[0] + n[0] * (p[1] + n[1] * p[2])
    }

    pub(super) fn trilinear_interior(
        interior: &[f32],
        p: [f32; 3],
        lattice_min: [f32; 3],
        size: [f32; 3],
        solid_nodes: [usize; 3],
    ) -> f32 {
        let cells = solid_nodes.map(|n| n - 7);
        assert_eq!(interior.len(), cells.iter().product::<usize>());
        let spacing: [f32; 3] =
            std::array::from_fn(|axis| size[axis] / (solid_nodes[axis] - 1) as f32);
        let top = cells.map(|n| n - 1);
        let mut g = [0.0; 3];
        for axis in 0..3 {
            let physical_min = lattice_min[axis] + 3.0 * spacing[axis];
            g[axis] = ((p[axis] - physical_min) / spacing[axis] - 0.5).clamp(0.0, top[axis] as f32);
        }
        let base = g.map(|v| v.floor() as usize);
        let f: [f32; 3] = std::array::from_fn(|axis| g[axis] - base[axis] as f32);
        let mut value = 0.0;
        for corner in 0..8 {
            let offset = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let at = std::array::from_fn(|axis| (base[axis] + offset[axis]).min(top[axis]));
            let weight = (0..3)
                .map(|axis| {
                    if offset[axis] == 1 {
                        f[axis]
                    } else {
                        1.0 - f[axis]
                    }
                })
                .product::<f32>();
            value += weight * interior[index(at, cells)];
        }
        value
    }

    pub(super) fn union(
        particle_phi: f32,
        interior: Option<&[f32]>,
        p: [f32; 3],
        lattice_min: [f32; 3],
        size: [f32; 3],
        solid_nodes: [usize; 3],
    ) -> f32 {
        let Some(interior) = interior else {
            return particle_phi;
        };
        assert!(!interior.is_empty(), "wired interior must cover its physical cells");
        let spacing: [f32; 3] =
            std::array::from_fn(|axis| size[axis] / (solid_nodes[axis] - 1) as f32);
        let h = spacing.into_iter().fold(f32::INFINITY, f32::min);
        particle_phi.min(trilinear_interior(interior, p, lattice_min, size, solid_nodes) + h)
    }

    #[test]
    fn deep_pool_interior_reaches_below_particle_band() {
        let interior = vec![-3.0; 2 * 2 * 2];
        let phi = union(0.25, Some(&interior), [1.0; 3], [0.0; 3], [2.0; 3], [9; 3]);
        assert_eq!(phi, -2.75);
    }

    #[test]
    fn shallow_surface_prefers_particle_distance() {
        let interior = vec![-0.1; 2 * 2 * 2];
        let phi = union(-0.2, Some(&interior), [1.0; 3], [0.0; 3], [2.0; 3], [9; 3]);
        assert_eq!(phi, -0.2);
    }

    #[test]
    fn unwired_interior_is_bit_identity() {
        let p = 0.375;
        assert_eq!(union(p, None, [0.0; 3], [0.0; 3], [1.0; 3], [2; 3]), p);
    }

    #[test]
    fn rectangular_lattice_uses_cell_center_offset_and_physical_spacing() {
        // Three-by-two-by-one physical cells inside a padded rectangular
        // solid lattice; spacing is size/(solid_nodes-1) on each axis.
        let cells = [3, 2, 1];
        let interior: Vec<f32> = (0..cells.iter().product()).map(|i| i as f32).collect();
        let sample = trilinear_interior(
            &interior,
            [3.0, 3.0, 1.0],
            [0.0; 3],
            [6.0, 4.0, 2.0],
            [10, 9, 8],
        );
        assert_eq!(sample, 4.0);
        let combined = union(
            10.0,
            Some(&interior),
            [3.0, 3.0, 1.0],
            [0.0; 3],
            [6.0, 4.0, 2.0],
            [10, 9, 8],
        );
        assert!((combined - 4.2857144).abs() < 1.0e-6);
    }
    #[test]
    fn particle_volume_interior_codegen_validates() {
        let wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<super::ParticleVolume>()
                .expect("particle_volume buffer codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{error:?}"));
        assert!(wgsl.contains("buf_bricks: array<u32>"), "{wgsl}");
        assert!(wgsl.contains("buf_interior: array<f32>"), "{wgsl}");
        assert!(wgsl.contains("brick_pass: u32"), "{wgsl}");
        assert!(wgsl.contains("interior_len: u32"), "{wgsl}");
        assert!(
            wgsl.contains("params.brick_pass, params.interior_len)"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("@group(0) @binding(4) var<storage, read> buf_bricks"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("@group(0) @binding(5) var<storage, read> buf_interior"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("@group(0) @binding(7) var<storage, read_write> buf_levelset"),
            "{wgsl}"
        );
        let clear_guard = wgsl
            .find("if brick_pass != 2u")
            .expect("brick clear guard");
        let interior_union = wgsl
            .find("phi = min(phi, pv_interior")
            .expect("interior union");
        let solid_constraint = wgsl
            .find("if pv_solid(")
            .expect("solid constraint");
        assert!(clear_guard < interior_union && clear_guard < solid_constraint);
        assert!(!wgsl.contains("brick_pass == 2u { return band; }"));
        assert_eq!(std::mem::size_of::<super::VolumeUniforms>(), 80);
    }

    #[test]
    fn particle_volume_recomputes_derived_uniforms_in_declared_order() {
        let frame = crate::node_graph::effect_node::FrameTime {
            beats: manifold_core::Beats(0.0),
            seconds: manifold_core::Seconds(0.0),
            delta: manifold_core::Seconds(0.0),
            frame_count: 0,
        };
        let array_len = |port: &str| (port == "interior").then_some(7);
        let ctx = crate::node_graph::freeze::derived_uniform_registry::DerivedUniformContext {
            frame: &frame,
            camera: None,
            array_len: &array_len,
        };
        assert_eq!(
            crate::node_graph::freeze::derived_uniform_registry::recompute(
                "node.particle_volume",
                &ctx,
            ),
            Some(vec![0.0, 7.0])
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::super::liquid_surface_tests::{Harness, Lattice, read};
    use super::*;
    use crate::node_graph::fluid_particles::{CellRange, FluidBlob, bin_counts};

    fn expected(
        lattice: &Lattice,
        solid_nodes: [usize; 3],
        interior: Option<&[f32]>,
        blob: Option<FluidBlob>,
    ) -> Vec<f32> {
        let levels = solid_nodes;
        let total = levels.iter().product();
        let band = blob.map_or(0.0, |b| 3.0 * b.center_radius[3]);
        let min = lattice.min();
        (0..total)
            .map(|idx| {
                let ijk = [
                    idx % levels[0],
                    (idx / levels[0]) % levels[1],
                    idx / (levels[0] * levels[1]),
                ];
                let p = std::array::from_fn(|axis| {
                    min[axis] + ijk[axis] as f32 * lattice.size[axis] / (levels[axis] - 1) as f32
                });
                let particle_phi = blob.map_or(band, |blob| {
                    let support = 1.5 * blob.center_radius[3];
                    if (0..3).any(|axis| {
                        let h = lattice.size[axis] / (levels[axis] - 1) as f32;
                        let lo = ((blob.center_radius[axis] - support - min[axis]) / h).floor() as i32;
                        let hi = ((blob.center_radius[axis] + support - min[axis]) / h).floor() as i32 + 1;
                        (ijk[axis] as i32) < lo || ijk[axis] as i32 > hi
                    }) { return band; }
                    let d: [f32; 3] = std::array::from_fn(|axis| p[axis] - blob.center_radius[axis]);
                    let v = [
                        blob.shape_diag[0] * d[0]
                            + blob.shape_off[0] * d[1]
                            + blob.shape_off[1] * d[2],
                        blob.shape_off[0] * d[0]
                            + blob.shape_diag[1] * d[1]
                            + blob.shape_off[2] * d[2],
                        blob.shape_off[1] * d[0]
                            + blob.shape_off[2] * d[1]
                            + blob.shape_diag[2] * d[2],
                    ];
                    let reach = blob.center_radius[3];
                    let distance = v.iter().map(|value| value * value).sum::<f32>().sqrt();
                    band.min(reach * (distance - 1.0))
                });
                super::cpu_tests::union(particle_phi, interior, p, min, lattice.size, levels)
            })
            .collect()
    }

    fn run_volume(
        lattice: &Lattice,
        solid_nodes: [usize; 3],
        interior: Option<&[f32]>,
        blob: Option<FluidBlob>,
    ) -> Vec<f32> {
        let mut harness = Harness::new();
        let blobs = blob.into_iter().collect::<Vec<_>>();
        let range_count = bin_counts(lattice.size, lattice.cell)
            .iter()
            .product::<u32>() as usize;
        let ranges = vec![
            CellRange {
                start: 0,
                count: blobs.len() as u32
            };
            range_count
        ];
        let (blobs_slot, _) = harness.array(&blobs, blobs.len().max(1));
        let (ranges_slot, _) = harness.array(&ranges, ranges.len());
        let solid = vec![1.0_f32; solid_nodes.iter().product()];
        let (solid_slot, _) = harness.array(&solid, solid.len());
        let interior_slot = interior.map(|values| harness.array(values, values.len().max(1)).0);
        // What node.blob_bounds reduces these blobs to.
        let bounds = blob.map_or([0.0; 2], |b| {
            [b.center_radius[3], 1.5 * b.center_radius[3] + b.shape_off[3]]
        });
        let (bounds_slot, _) = harness.array(&bounds, bounds.len());
        let levels = solid_nodes.map(|n| n as u32);
        let total = levels.iter().product::<u32>() as usize;
        let (levelset_slot, levelset_buf) = harness.array::<f32>(&[], total);
        let mut inputs = vec![
            ("blobs", blobs_slot),
            ("cell_ranges", ranges_slot),
            ("solid", solid_slot),
            ("bounds", bounds_slot),
        ];
        if let Some(slot) = interior_slot {
            inputs.push(("interior", slot));
        }
        let (_, errors) = harness.run(
            &mut ParticleVolume::new(),
            &inputs,
            &[("levelset", levelset_slot)],
            &lattice.params(&[
                ("nodes_x", levels[0] as f32),
                ("nodes_y", levels[1] as f32),
                ("nodes_z", levels[2] as f32),
                ("resolution_scale", 1.0),
            ]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        read(&levelset_buf, total)
    }

    fn assert_matches(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (idx, (&got, &want)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (got - want).abs() <= 2e-5,
                "node {idx}: got {got}, expected {want}"
            );
        }
    }

    #[test]
    fn gpu_flip_narrow_band_mesher_values() {
        let deep = Lattice {
            center: [0.0; 3],
            size: [2.0; 3],
            cell: 2.0,
        };
        let deep_interior = vec![-3.0; 2 * 2 * 2];
        let deep_actual = run_volume(&deep, [9, 9, 9], Some(&deep_interior), None);
        assert_matches(
            &deep_actual,
            &expected(&deep, [9, 9, 9], Some(&deep_interior), None),
        );

        let surface_blob = FluidBlob {
            center_radius: [0.0, 0.0, 0.0, 0.5],
            shape_diag: [2.0, 2.0, 2.0, 0.0],
            shape_off: [0.0; 4],
        };
        let shallow = vec![-0.1; 2 * 2 * 2];
        let surface = run_volume(&deep, [9, 9, 9], Some(&shallow), Some(surface_blob));
        assert_matches(
            &surface,
            &expected(&deep, [9, 9, 9], Some(&shallow), Some(surface_blob)),
        );

        let off = run_volume(&deep, [9, 9, 9], None, Some(surface_blob));
        assert_matches(&off, &expected(&deep, [9, 9, 9], None, Some(surface_blob)));

        let rectangular = Lattice {
            center: [0.0; 3],
            size: [6.0, 4.0, 4.0],
            cell: 2.0,
        };
        let rectangular_nodes = [10, 9, 8];
        let physical_cells = rectangular_nodes.map(|n| n - 7);
        let rectangular_interior: Vec<f32> = (0..physical_cells.iter().product::<usize>())
            .map(|value| -6.0 + value as f32)
            .collect();
        let rectangular_actual =
            run_volume(&rectangular, rectangular_nodes, Some(&rectangular_interior), None);
        assert_matches(
            &rectangular_actual,
            &expected(&rectangular, rectangular_nodes, Some(&rectangular_interior), None),
        );
    }
}
