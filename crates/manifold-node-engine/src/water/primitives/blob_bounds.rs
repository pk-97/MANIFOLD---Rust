//! Exact bounds for indexed blob gathers. Shared by the field and its sparse
//! schedule; spatial bins never impose a radius or quality limit.
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::water::fluid_particles::FluidBlob;
use manifold_node_engine::primitive::Primitive;

const SHADER: &str = include_str!("shaders/blob_bounds.wgsl");
const THREADS: u32 = 256;
const MAX_GROUPS: u32 = 256;
const PARTIAL_BYTES: u64 = MAX_GROUPS as u64 * 2 * size_of::<f32>() as u64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BoundsParams {
    count: u32,
    groups: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: BlobBounds,
    type_id: "node.blob_bounds",
    purpose: "Reduce surface blobs to two exact conservative bounds: the largest kernel axis, and the largest 1.5-axis support plus centre displacement from the sorted particle. A barriered maximum reduction; no size cap or atomic operations.",
    inputs: { blobs: Array(FluidBlob) required, },
    outputs: { bounds: Array(f32), },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Wire the same shaped blobs into this node, particle_volume and lattice_bricks. Both consumers require these two words to search all bins that may contain an influencing blob, independently of bin width; one reduction serves every lattice sample. Graphs saved before this node get it at load.",
    examples: [],
    picker: { label: "Blob Bounds", category: Atom },
    summary: "Measures kernel reach for exact particle surface searches.",
    category: Particles3D,
    role: Filter,
    aliases: ["kernel bounds", "surface support"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        reduction: Option<GpuComputePipeline> = None,
        partial_reduction: Option<GpuComputePipeline> = None,
        finish_reduction: Option<GpuComputePipeline> = None,
        partials: Option<GpuBuffer> = None,
    },
}

impl Primitive for BlobBounds {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.reduction.get_or_insert_with(|| device.create_compute_pipeline(SHADER, "main", "node.blob_bounds"));
        self.partial_reduction.get_or_insert_with(|| device.create_compute_pipeline(SHADER, "partial_main", "node.blob_bounds.partial"));
        self.finish_reduction.get_or_insert_with(|| device.create_compute_pipeline(SHADER, "finish_main", "node.blob_bounds.finish"));
    }
    fn array_output_capacity(&self, port: &str, _: &ParamValues, _: &[(&str, u32)]) -> Option<u32> {
        (port == "bounds").then_some(2)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(blobs), Some(bounds)) = (ctx.inputs.array("blobs"), ctx.outputs.array("bounds")) else { return };
        let count = (blobs.size / size_of::<FluidBlob>() as u64) as u32;
        let groups = count.div_ceil(THREADS).min(MAX_GROUPS);
        let params = BoundsParams { count, groups, _pad1: 0, _pad2: 0 };
        let gpu = ctx.gpu_encoder();
        if count <= THREADS {
            gpu.native_enc.dispatch_compute(self.reduction.as_ref().expect("installed blob bounds"), &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                GpuBinding::Buffer { binding: 1, buffer: blobs, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: bounds, offset: 0 },
            ], [1, 1, 1], "node.blob_bounds");
        } else {
            let partials = self.partials.get_or_insert_with(|| gpu.device.create_buffer(PARTIAL_BYTES));
            gpu.native_enc.dispatch_compute(self.partial_reduction.as_ref().expect("installed partial blob bounds"), &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                GpuBinding::Buffer { binding: 1, buffer: blobs, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: partials, offset: 0 },
            ], [groups, 1, 1], "node.blob_bounds.partial");
            gpu.native_enc.compute_memory_barrier_buffers();
            gpu.native_enc.dispatch_compute(self.finish_reduction.as_ref().expect("installed final blob bounds"), &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                GpuBinding::Buffer { binding: 2, buffer: bounds, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: partials, offset: 0 },
            ], [1, 1, 1], "node.blob_bounds.finish");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn liquid_blob_bounds_shader_validates() {
        let module = naga::front::wgsl::parse_str(super::SHADER).expect("blob bounds WGSL");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).expect("blob bounds validates");
        for entry in ["main", "partial_main", "finish_main"] {
            assert!(module.entry_points.iter().any(|point| point.name == entry), "missing {entry}");
        }
        assert_eq!(size_of::<super::BoundsParams>(), 16);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_node_engine::testkit::liquid_surface::{Harness, read};

    fn reference(blobs: &[FluidBlob]) -> [u32; 2] {
        let mut bounds = [0.0f32; 2];
        for blob in blobs {
            let r = blob.center_radius[3];
            assert!(r.is_finite() && blob.shape_off[3].is_finite());
            if r > 0.0 {
                let support = 1.5 * r + blob.shape_off[3];
                assert!(support.is_finite() && support >= 0.0);
                bounds[0] = bounds[0].max(r);
                bounds[1] = bounds[1].max(support);
            }
        }
        bounds.map(f32::to_bits)
    }

    #[test]
    fn liquid_blob_bounds_hierarchical_matches_cpu_across_counts_and_reuse() {
        let mut harness = Harness::new();
        let mut node = BlobBounds::new();
        node.prepare_pipelines(&harness.device);
        let (bounds_slot, bounds_buffer) = harness.array::<f32>(&[], 2);
        let mut scratch: Option<GpuBuffer> = None;
        // The last two inputs shrink the allocation and number of partials
        // after the capped 256-group path, while reusing the same primitive.
        for count in [1, 255, 256, 257, 1025, 65_537, 257, 1] {
            let mut blobs = vec![FluidBlob::default(); count];
            for (i, blob) in blobs.iter_mut().enumerate() {
                blob.center_radius[3] = if i % 11 == 0 { -1.0 } else { (i % 7 + 1) as f32 * 0.125 };
                blob.shape_off[3] = if i % 11 == 0 { 1024.0 } else { (i % 5) as f32 * 0.25 };
            }
            // Radius maximum in the final record; support maximum elsewhere.
            // Binary fractions keep the CPU and shader arithmetic exact.
            blobs[count - 1].center_radius[3] = 8.0;
            blobs[count - 1].shape_off[3] = 0.0;
            if count > 1 {
                blobs[count / 2].center_radius[3] = 2.0;
                blobs[count / 2].shape_off[3] = 32.0;
            }
            let (blob_slot, blob_buffer) = harness.array(&blobs, count);
            for population in 0..3 {
                if population == 1 {
                    for blob in &mut blobs {
                        blob.center_radius[3] = 0.25;
                        blob.shape_off[3] = 0.125;
                    }
                } else if population == 2 {
                    for (i, blob) in blobs.iter_mut().enumerate() {
                        blob.center_radius[3] = if i % 2 == 0 { 0.0 } else { -1.0 };
                        blob.shape_off[3] = 1024.0;
                    }
                }
                // SAFETY: Harness waits after each run, and this buffer holds count records.
                unsafe { blob_buffer.write(0, bytemuck::cast_slice(&blobs)); }
                let (_, errors) = harness.run(&mut node, &[("blobs", blob_slot)], &[("bounds", bounds_slot)], &ParamValues::default());
                assert!(errors.is_empty(), "count {count}, population {population}: {errors:?}");
                assert_eq!(read::<u32>(&bounds_buffer, 2), reference(&blobs), "count {count}, population {population}");
                if let Some(partials) = &node.partials {
                    assert_eq!(partials.size, PARTIAL_BYTES);
                    if let Some(scratch) = &scratch {
                        assert!(partials.ptr_eq(scratch), "resized populations reuse the fixed scratch buffer");
                    } else {
                        scratch = Some(partials.clone());
                    }
                } else {
                    assert!(count <= THREADS as usize, "large input allocates scratch once");
                }
            }
        }
    }
}

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};

/// Give every liquid field consumer saved before `node.blob_bounds` existed
/// its bounds, the way the shipped Liquid Surface group wires them: one
/// bounds node per blob source, feeding every consumer of that source.
/// Runs once at graph installation; a graph that already wires `bounds` is
/// untouched.
pub fn wire_blob_bounds(def: &mut EffectGraphDef) -> bool {
    const CONSUMERS: [&str; 2] = ["node.particle_volume", "node.lattice_bricks"];
    let mut next_id = def.nodes.iter().map(|n| n.id).max().map_or(0, |id| id + 1);
    let consumers: Vec<u32> =
        def.nodes.iter().filter(|n| CONSUMERS.contains(&n.type_id.as_str())).map(|n| n.id).collect();
    let mut changed = false;
    for consumer in consumers {
        if def.wires.iter().any(|w| w.to_node == consumer && w.to_port == "bounds") {
            continue;
        }
        let Some((from_node, from_port)) =
            def.wires.iter().find(|w| w.to_node == consumer && w.to_port == "blobs").map(|w| (w.from_node, w.from_port.clone()))
        else {
            continue;
        };
        let measuring = |w: &EffectGraphWire| {
            w.from_node == from_node
                && w.from_port == from_port
                && w.to_port == "blobs"
                && def.nodes.iter().any(|n| n.id == w.to_node && n.type_id == "node.blob_bounds")
        };
        let bounds = match def.wires.iter().find(|w| measuring(w)).map(|w| w.to_node) {
            Some(existing) => existing,
            None => {
                let id = next_id;
                next_id += 1;
                def.nodes.push(EffectGraphNode {
                    id,
                    node_id: manifold_core::NodeId::default(),
                    type_id: "node.blob_bounds".to_string(),
                    handle: None,
                    params: Default::default(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: Default::default(),
                    output_canvas_scales: Default::default(),
                    group: None,
                });
                def.wires.push(EffectGraphWire { from_node, from_port, to_node: id, to_port: "blobs".into() });
                id
            }
        };
        def.wires.push(EffectGraphWire { from_node: bounds, from_port: "bounds".into(), to_node: consumer, to_port: "bounds".into() });
        changed = true;
    }
    changed
}


inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_blob_bounds",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 400,
        apply: wire_blob_bounds,
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
