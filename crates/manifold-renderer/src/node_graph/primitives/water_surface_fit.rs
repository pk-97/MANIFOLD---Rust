use crate::node_graph::effect_node::{EffectNodeContext, NodeRequires};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;
pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<WaterSurfaceFit>()
        .expect("water surface fit codegen")
}
pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
    let _ = device.create_compute_pipeline(
        &shader_source(),
        crate::node_graph::freeze::codegen::ENTRY,
        "node.water_surface_fit",
    );
}
crate::primitive! {
name:WaterSurfaceFit,
type_id:"node.water_surface_fit",
purpose:"Fits bounded neighbour covariance ellipsoids to water particles.",
inputs:{
    particles:Array(WaterParticle) required,
    heads:Array(u32) required,
    next:Array(u32) required,
    radius:ScalarF32 optional,
    center_blend:ScalarF32 optional},
outputs:{
    shapes:Channels["surface_center_radius":Vec4F,
        "surface_axis_x":Vec4F,
        "surface_axis_y":Vec4F,
        "surface_axis_z":Vec4F]},
params:[ParamDef{
        name:Cow::Borrowed("radius"),
        label:"Radius",
        ty:ParamType::Float,
        default:ParamValue::Float(0.046875),
        range:Some((0.001,
        1.0)),
        enum_values:&[]},
    ParamDef{
        name:Cow::Borrowed("center_blend"),
        label:"Center Blend",
        ty:ParamType::Float,
        default:ParamValue::Float(0.5),
        range:Some((0.0,
        1.0)),
        enum_values:&[]}],
depth_rule:Terminal,
composition_notes:"Bounded 27-cell covariance fit; isolated particles remain spheres.",
examples:[],
picker:{
    label:"Water Surface Fit",
    category:Atom},
summary:"Fits an ellipsoid to local water particle neighbours.",
category:Particles3D,
role:Filter,
aliases:["water shape"],
fusion_kind:Pointwise,
wgsl_body:include_str!("shaders/water_surface_fit_body.wgsl"),
input_access:[BufferGather,
    BufferGather,
    BufferGather],
}
impl Primitive for WaterSurfaceFit {
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
        (p == "shapes")
            .then(|| c.iter().find(|x| x.0 == "particles").map(|x| x.1))
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(p), Some(h), Some(n), Some(o)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("heads"),
            ctx.inputs.array("next"),
            ctx.outputs.array("shapes"),
        ) else {
            return;
        };
        let count = (p.size / 96) as u32;
        if count == 0 {
            ctx.gpu_encoder().native_enc.clear_buffer(o);
            return;
        }
        if h.size < 32768 * 4 || n.size < p.size / 96 * 4 || o.size < u64::from(count) * 64 {
            ctx.gpu_encoder().native_enc.clear_buffer(o);
            ctx.error("node.water_surface_fit: invalid bin or shape capacity");
            return;
        }
        let r = ctx.scalar_or_param("radius", 0.046875);
        let b = ctx.scalar_or_param("center_blend", 0.5);
        if !r.is_finite() || r <= 0.0 || !b.is_finite() {
            ctx.gpu_encoder().native_enc.clear_buffer(o);
            ctx.error("node.water_surface_fit: invalid parameters");
            return;
        }
        let g = ctx.gpu_encoder();
        let pipe = self.pipeline.get_or_insert_with(|| {
            g.device.create_compute_pipeline(
                &shader_source(),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.water_surface_fit",
            )
        });
        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct U {
            radius: f32,
            blend: f32,
            count: u32,
            _p: u32,
        }
        let u = U {
            radius: r,
            blend: b.clamp(0., 1.),
            count,
            _p: 0,
        };
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
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: o,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.water_surface_fit",
        );
    }
}
