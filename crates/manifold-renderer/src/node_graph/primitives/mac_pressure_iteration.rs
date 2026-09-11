//! A single red/black pressure sweep and a separate residual acceptance atom.
//! The graph owns iteration count and ordering. A failed residual sets the
//! sticky pressure fault before WaterCommit can accept candidate particles.
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;
pub const WGSL: &str = concat!(
    include_str!("shaders/water_common.wgsl"),
    "\n",
    include_str!("shaders/mac_index.wgsl"),
    "\n",
    include_str!("shaders/mac_pressure_iteration.wgsl")
);
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacIterationUniforms {
    pub parity: u32,
    pub omega: f32,
    pub absolute_tolerance: f32,
    pub relative_tolerance: f32,
}
pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
    let _ = device.create_compute_pipeline(WGSL, "cs_relax", "node.mac_pressure_relax");
    let _ = device.create_compute_pipeline(WGSL, "cs_validate", "node.mac_pressure_validate");
}

crate::primitive! {name:MacPressureRelax,type_id:"node.mac_pressure_relax",purpose:"One atomic checkerboard SOR sweep on fractional pressure rows; compose alternating parity nodes.",
inputs:{rows:Channels["mac_lower_diag":Vec4F,"mac_upper_rhs":Vec4F] required,pressure:Array(u32) required,status:Array(u32) required,parity:ScalarF32 optional,omega:ScalarF32 optional,},
outputs:{out:Array(u32),},params:[ParamDef{name:Cow::Borrowed("parity"),label:"parity",ty:ParamType::Int,default:ParamValue::Float(0.0),range:Some((0.0,1.0)),enum_values:&[]},ParamDef{name:Cow::Borrowed("omega"),label:"omega",ty:ParamType::Float,default:ParamValue::Float(1.7),range:Some((0.01,1.99)),enum_values:&[]}],depth_rule:Terminal,
composition_notes:"One atomic checkerboard SOR sweep on fractional pressure rows; compose alternating parity nodes. Pressure stores f32 q as u32 bits. Stationary fractional solids, q=dt*p/rho. A status wire orders acceptance after the solve.",examples:[],picker:{label:"MacPressureRelax",category:Atom},summary:"One atomic checkerboard SOR sweep on fractional pressure rows; compose alternating parity nodes.",category:Particles3D,role:Filter,aliases:[],boundary_reason:Blocked,
}
impl Primitive for MacPressureRelax {
    fn array_output_capacity(
        &self,
        p: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        c: &[(&str, u32)],
    ) -> Option<u32> {
        if p != "out" {
            return None;
        }
        c.iter().find(|(p, _)| *p == "pressure").map(|(_, n)| *n)
    }
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("pressure", "out")]
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(rows) = ctx.inputs.array("rows") else {
            return;
        };
        let Some(pressure) = ctx.inputs.array("pressure") else {
            return;
        };
        let Some(status) = ctx.inputs.array("status") else {
            return;
        };
        if rows.size < 262144 * 32 || pressure.size < 262144 * 4 || status.size < 4 {
            ctx.error("node.mac_pressure_relax: insufficient pressure lattice capacity");
            return;
        }
        let u = MacIterationUniforms {
            parity: ctx.scalar_or_param("parity", 0.0) as u32,
            omega: ctx.scalar_or_param("omega", 1.7),
            absolute_tolerance: 0.0,
            relative_tolerance: 0.0,
        };
        if u.parity > 1 || !u.omega.is_finite() || u.omega <= 0.0 || u.omega >= 2.0 {
            ctx.error("node.mac_pressure_relax: invalid parity or omega");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(WGSL, "cs_relax", "node.mac_pressure_relax")
        });
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: rows,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: pressure,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: status,
                    offset: 0,
                },
            ],
            [262144u32.div_ceil(256), 1, 1],
            "node.mac_pressure_relax",
        );
    }
}

crate::primitive! {name:MacPressureValidate,type_id:"node.mac_pressure_validate",purpose:"Validate pressure residual against absolute divergence and relative RHS tolerances; reject unconverged projection.",
inputs:{rows:Channels["mac_lower_diag":Vec4F,"mac_upper_rhs":Vec4F] required,pressure:Array(u32) required,status:Array(u32) required,absolute_tolerance:ScalarF32 optional,relative_tolerance:ScalarF32 optional,},
outputs:{status_out:Array(u32),},params:[ParamDef{name:Cow::Borrowed("absolute_tolerance"),label:"absolute_tolerance",ty:ParamType::Float,default:ParamValue::Float(0.001),range:Some((1e-06,0.1)),enum_values:&[]},ParamDef{name:Cow::Borrowed("relative_tolerance"),label:"relative_tolerance",ty:ParamType::Float,default:ParamValue::Float(0.0001),range:Some((1e-06,0.01)),enum_values:&[]}],depth_rule:Terminal,
composition_notes:"Validate pressure residual against absolute divergence and relative RHS tolerances; reject unconverged projection. Pressure stores f32 q as u32 bits. Stationary fractional solids, q=dt*p/rho. A status wire orders acceptance after the solve.",examples:[],picker:{label:"MacPressureValidate",category:Atom},summary:"Validate pressure residual against absolute divergence and relative RHS tolerances; reject unconverged projection.",category:Particles3D,role:Filter,aliases:[],boundary_reason:Blocked,
}
impl Primitive for MacPressureValidate {
    fn array_output_capacity(
        &self,
        p: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        c: &[(&str, u32)],
    ) -> Option<u32> {
        if p != "status_out" {
            return None;
        }
        c.iter().find(|(p, _)| *p == "status").map(|(_, n)| *n)
    }
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("status", "status_out")]
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(rows) = ctx.inputs.array("rows") else {
            return;
        };
        let Some(pressure) = ctx.inputs.array("pressure") else {
            return;
        };
        let Some(status) = ctx.inputs.array("status") else {
            return;
        };
        if rows.size < 262144 * 32 || pressure.size < 262144 * 4 || status.size < 4 {
            ctx.error("node.mac_pressure_validate: insufficient pressure lattice capacity");
            return;
        }
        let u = MacIterationUniforms {
            parity: 0,
            omega: 0.0,
            absolute_tolerance: ctx.scalar_or_param("absolute_tolerance", 0.001),
            relative_tolerance: ctx.scalar_or_param("relative_tolerance", 0.0001),
        };
        if !u.absolute_tolerance.is_finite()
            || u.absolute_tolerance <= 0.0
            || !u.relative_tolerance.is_finite()
            || u.relative_tolerance < 0.0
        {
            ctx.error("node.mac_pressure_validate: invalid residual tolerance");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(WGSL, "cs_validate", "node.mac_pressure_validate")
        });
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: rows,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: pressure,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: status,
                    offset: 0,
                },
            ],
            [262144u32.div_ceil(256), 1, 1],
            "node.mac_pressure_validate",
        );
    }
}
