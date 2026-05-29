//! `node.voronoi_cell_id` — Voronoi partition that emits the F1-winning
//! cell's integer coordinate (RG = cell_id.xy, B=0, A=1). Aspect-corrected
//! so cells are square in pixels (X scale = `cell_count` × aspect). hash2
//! jitter.
//!
//! Pure generator. The cell-id field that lets a graph re-hash each cell
//! per beat (feed RG + a beat seed into `node.hash_field_by_seed`) for
//! per-cell content shuffle / visibility — the foundation of Voronoi
//! Prism and any other beat-reseeded cellular composite. Distinct from
//! `node.voronoi_2d` (which emits F1/F2/edge/static-cell-hash on its own
//! wang-hash partition); this exposes the raw cell coordinate of the
//! legacy prism's hash2 partition.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VoronoiCellIdUniforms {
    cell_count: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: VoronoiCellId,
    type_id: "node.voronoi_cell_id",
    purpose: "Pure generator. Voronoi partition emitting the F1-winning cell's integer coordinate (RG = cell_id.xy, B=0, A=1). Aspect-corrected (cells square in pixels, X scale = cell_count × aspect), hash2 jitter. Feed RG + a beat (or any) seed into node.hash_field_by_seed to get per-cell randoms that re-roll each beat — the foundation of Voronoi Prism and beat-reseeded cellular composites. Distinct from node.voronoi_2d (F1/F2/edge/static hash on a wang-hash partition); this exposes the raw cell coordinate.",
    inputs: {
        cell_count: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "cell_count",
            label: "Cell Count",
            ty: ParamType::Int,
            default: ParamValue::Float(16.0),
            range: Some((4.0, 64.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "cell_id components are small integers (0 .. ~cell_count·aspect) stored exactly in fp16. Read them with node.hash_field_by_seed (textureLoad, no interpolation) so the per-cell value stays exact at cell boundaries. cell_count port-shadows the param. Aspect comes from the output dimensions — no aspect param.",
    examples: ["preset.effect.voronoi_prism"],
    picker: { label: "Voronoi Cell ID", category: Atom },
}

impl Primitive for VoronoiCellId {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cell_count = ctx.scalar_or_param("cell_count", 16.0);

        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/voronoi_cell_id.wgsl"),
                "cs_main",
                "node.voronoi_cell_id",
            )
        });

        let uniforms = VoronoiCellIdUniforms {
            cell_count,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.voronoi_cell_id",
        );
    }
}
