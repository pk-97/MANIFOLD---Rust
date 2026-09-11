use crate::node_graph::effect_node::{EffectNodeContext, NodeRequires};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;
use manifold_gpu::GpuBinding;
pub const WGSL: &str = include_str!("shaders/water_particle_bins.wgsl");
pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
    let _ = device.create_compute_pipeline(WGSL, "cs_main", "node.water_particle_bins");
}
crate::primitive! {
name:WaterParticleBins,
type_id:"node.water_particle_bins",
purpose:"Builds a fixed 32x32x32 linked cell binning of water particles.",
inputs:{
    particles:Array(WaterParticle) required},
outputs:{
    heads:Array(u32),
    next:Array(u32)},
params:[],
depth_rule:Terminal,
composition_notes:"Fixed four metre domain; invalid particles are skipped.",
examples:[],
picker:{
    label:"Water Particle Bins",
    category:Atom},
summary:"Bins water particles for bounded neighbour lookup.",
category:Particles3D,
role:Filter,
aliases:["water bins"],
boundary_reason:Blocked,
}
impl Primitive for WaterParticleBins {
    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: false,
            gpu_encoder: true,
        }
    }
    fn array_output_capacity(
        &self,
        p: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        c: &[(&str, u32)],
    ) -> Option<u32> {
        match p {
            "heads" => Some(32768),
            "next" => c.iter().find(|x| x.0 == "particles").map(|x| x.1),
            _ => None,
        }
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(p) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(h) = ctx.outputs.array("heads") else {
            return;
        };
        let Some(n) = ctx.outputs.array("next") else {
            return;
        };
        let count = (p.size / 96) as u32;
        if h.size < 32768 * 4 || n.size < u64::from(count) * 4 {
            ctx.error("node.water_particle_bins: insufficient bin capacity");
            return;
        }
        let g = ctx.gpu_encoder();
        g.native_enc.clear_buffer(h);
        g.native_enc.clear_buffer(n);
        if count == 0 {
            return;
        }
        let pipe = self.pipeline.get_or_insert_with(|| {
            g.device
                .create_compute_pipeline(WGSL, "cs_main", "node.water_particle_bins")
        });
        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct U {
            count: u32,
            _p: [u32; 3],
        }
        let u = U { count, _p: [0; 3] };
        g.native_enc.dispatch_compute(
            pipe,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: p,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: h,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: n,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.water_particle_bins",
        );
    }
}
