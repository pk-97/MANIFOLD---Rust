//! Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
//! `node.particle_volume` — the liquid level set: one value per lattice node,
//! the distance to the nearest anisotropic kernel in the node's bins
//! (GPU_FLUID_SURFACE_DESIGN.md D8, D15, D18, P6e). A per-element gather on
//! the codegen path. The optional interior field follows Ferstl et al.,
//! "Narrow Band FLIP for Liquid Simulations", CGF 35(2), 2016, Eq. 4.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::liquid_bricks;
use crate::float_param;
use super::sort_particles_into_cells::{bin_param, read_searched_bins};
use crate::primitives::standalone_pipeline::standalone_pipeline;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::water::fluid_particles::{CellRange, FluidBlob};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;

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
    crate::freeze::derived_uniform_registry::DerivedUniformRecompute {
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
        let (interior, interior_len) = if let Some(buffer) = interior_wired {
            let actual = buffer.size / 4;
            if !buffer.size.is_multiple_of(4) || crate::water::liquid::lattice::interior_cells(nodes.map(|n| n as u32), actual).is_none() {
                ctx.error(format!(
                    "Particle Volume: interior has {actual} f32 values; expected the exact physical cell count for native (nodes minus 4) or solver (nodes minus 7) padding at {}×{}×{} nodes.",
                    nodes[0], nodes[1], nodes[2]
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
    use crate::testkit::particle_volume::*;






    #[test]
    fn deep_pool_interior_reaches_below_particle_band() {
        let interior = vec![-3.0; 2 * 2 * 2];
        let phi = union(0.25, Some(&interior), [1.0; 3], [0.0; 3], [2.0; 3], [9; 3]);
        assert_eq!(phi, -2.75);
    }

    #[test]
    fn native_mesh_interior_samples_simulation_cell_centres() {
        let layout = crate::scene::fluid_domain::domain_layout(None, 2.0, 8).unwrap();
        let mesh = crate::water::liquid::lattice::LiquidLattice::from_layout(&layout).surface();
        let field: Vec<f32> = (0..8u32.pow(3)).map(|i| (i % 8) as f32 + 0.5).collect();
        for i in 0..8 {
            let p = [layout.min[0] + (i as f32 + 0.5) * 0.25, 1.0, 0.0];
            let value = trilinear_interior(&field, p, mesh.min(), mesh.bounds().scale, mesh.nodes().map(|n| n as usize));
            assert_eq!(value, i as f32 + 0.5);
        }
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
            crate::freeze::codegen::standalone_for_spec::<super::ParticleVolume>()
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
        let frame = crate::exec::effect_node::FrameTime {
            beats: manifold_core::Beats(0.0),
            seconds: manifold_core::Seconds(0.0),
            delta: manifold_core::Seconds(0.0),
            frame_count: 0,
        };
        let array_len = |port: &str| (port == "interior").then_some(7);
        let ctx = crate::freeze::derived_uniform_registry::DerivedUniformContext {
            frame: &frame,
            camera: None,
            array_len: &array_len,
        };
        assert_eq!(
            crate::freeze::derived_uniform_registry::recompute(
                "node.particle_volume",
                &ctx,
            ),
            Some(vec![0.0, 7.0])
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
