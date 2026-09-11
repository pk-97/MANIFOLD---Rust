//! Composable fractional MAC projection operations. Stationary solids only.
//! q=dt*p/rho. Matrix rows are scaled by h²; relaxation and gradient share
//! the same ghost-fluid coefficient, including the distance-ratio cap 25.
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacPressureRow {
    pub mac_lower_diag: [f32; 4],
    pub mac_upper_rhs: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacCountUniforms {
    pub dispatch_count: u32,
    pub pad: [u32; 3],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacGravityUniforms {
    pub step_dt: f32,
    pub dispatch_count: u32,
    pub pad: [u32; 2],
}

pub fn mac_apply_gravity_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacApplyGravity>()
        .expect("mac_apply_gravity codegen")
}
crate::primitive! {
name:MacApplyGravity,type_id:"node.mac_apply_gravity",purpose:"Apply gravity to valid MAC faces and enforce stationary blocked face normal velocity.",
inputs:{grid: Channels["mac_velocity": Vec4F, "mac_valid": Vec4F] required,
geometry: Channels["mac_open": Vec4F] required,
step_dt: ScalarF32 optional,},outputs:{out:Channels["mac_velocity": Vec4F, "mac_valid": Vec4F],},params:[ParamDef{name:Cow::Borrowed("step_dt"),label:"Substep dt",ty:ParamType::Float,default:ParamValue::Float(1.0/120.0),range:Some((0.00001,0.02)),enum_values:&[]},],
depth_rule:Terminal,composition_notes:"Apply gravity to valid MAC faces and enforce stationary blocked face normal velocity. Compose rows, zero, alternating relaxation, residual validation and gradient as separate graph nodes. The stationary box geometry and liquid SDF are explicit input wires.",
examples:[],picker:{label:"MacApplyGravity",category:Atom},summary:"Apply gravity to valid MAC faces and enforce stationary blocked face normal velocity.",category:Particles3D,role:Filter,aliases:[],
fusion_kind:Source,wgsl_body:include_str!("shaders/mac_apply_gravity_body.wgsl"),
input_access:[BufferGather,BufferGather],
wgsl_includes:[include_str!("shaders/water_common.wgsl"),include_str!("shaders/mac_index.wgsl")],
extra_fields:{source:String=mac_apply_gravity_source(),},
}
impl Primitive for MacApplyGravity {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then_some(274625)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(grid) = ctx.inputs.array("grid") else {
            return;
        };
        let Some(geometry) = ctx.inputs.array("geometry") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        if grid.size < 274625 * 32 || geometry.size < 274625 * 16 {
            ctx.error("MAC projection: incomplete input lattice");
            return;
        }
        let step_dt = ctx.scalar_or_param("step_dt", 1.0 / 120.0);
        if out.size < 274625 * 32 {
            ctx.error("node.mac_apply_gravity: insufficient output capacity");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(&self.source, "cs_main", "node.mac_apply_gravity")
        });
        let u = MacGravityUniforms {
            step_dt,
            dispatch_count: 274625,
            pad: [0; 2],
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: grid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: geometry,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [274625u32.div_ceil(256), 1, 1],
            "node.mac_apply_gravity",
        );
    }
}

pub fn mac_pressure_rows_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacPressureRows>()
        .expect("mac_pressure_rows codegen")
}
crate::primitive! {
name:MacPressureRows,type_id:"node.mac_pressure_rows",purpose:"Assemble fractional stationary-solid ghost-fluid pressure rows scaled by h squared.",
inputs:{phi: Array(f32) required,
geometry: Channels["mac_open": Vec4F] required,
grid: Channels["mac_velocity": Vec4F, "mac_valid": Vec4F] required,},outputs:{out:Channels["mac_lower_diag": Vec4F, "mac_upper_rhs": Vec4F],},params:[],
depth_rule:Terminal,composition_notes:"Assemble fractional stationary-solid ghost-fluid pressure rows scaled by h squared. Compose rows, zero, alternating relaxation, residual validation and gradient as separate graph nodes. The stationary box geometry and liquid SDF are explicit input wires.",
examples:[],picker:{label:"MacPressureRows",category:Atom},summary:"Assemble fractional stationary-solid ghost-fluid pressure rows scaled by h squared.",category:Particles3D,role:Filter,aliases:[],
fusion_kind:Source,wgsl_body:include_str!("shaders/mac_pressure_rows_body.wgsl"),
input_access:[BufferGather,BufferGather,BufferGather],
wgsl_includes:[include_str!("shaders/water_common.wgsl"),include_str!("shaders/mac_index.wgsl")],
extra_fields:{source:String=mac_pressure_rows_source(),},
}
impl Primitive for MacPressureRows {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then_some(262144)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(phi) = ctx.inputs.array("phi") else {
            return;
        };
        let Some(geometry) = ctx.inputs.array("geometry") else {
            return;
        };
        let Some(grid) = ctx.inputs.array("grid") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        if phi.size < 262144 * 4 || geometry.size < 274625 * 16 || grid.size < 274625 * 32 {
            ctx.error("MAC projection: incomplete input lattice");
            return;
        }
        if out.size < 262144 * 32 {
            ctx.error("node.mac_pressure_rows: insufficient output capacity");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(&self.source, "cs_main", "node.mac_pressure_rows")
        });
        let u = MacCountUniforms {
            dispatch_count: 262144,
            pad: [0; 3],
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: phi,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: geometry,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: grid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: out,
                    offset: 0,
                },
            ],
            [262144u32.div_ceil(256), 1, 1],
            "node.mac_pressure_rows",
        );
    }
}

pub fn mac_pressure_zero_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacPressureZero>()
        .expect("mac_pressure_zero codegen")
}
crate::primitive! {
name:MacPressureZero,type_id:"node.mac_pressure_zero",purpose:"Initialize pressure solution bits to zero for a fresh fractional projection.",
inputs:{rows: Channels["mac_lower_diag": Vec4F, "mac_upper_rhs": Vec4F] required,},outputs:{out:Array(u32),},params:[],
depth_rule:Terminal,composition_notes:"Initialize pressure solution bits to zero for a fresh fractional projection. Compose rows, zero, alternating relaxation, residual validation and gradient as separate graph nodes. The stationary box geometry and liquid SDF are explicit input wires.",
examples:[],picker:{label:"MacPressureZero",category:Atom},summary:"Initialize pressure solution bits to zero for a fresh fractional projection.",category:Particles3D,role:Filter,aliases:[],
fusion_kind:Source,wgsl_body:include_str!("shaders/mac_pressure_zero_body.wgsl"),
input_access:[BufferGather],
wgsl_includes:[include_str!("shaders/water_common.wgsl"),include_str!("shaders/mac_index.wgsl")],
extra_fields:{source:String=mac_pressure_zero_source(),},
}
impl Primitive for MacPressureZero {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then_some(262144)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(rows) = ctx.inputs.array("rows") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        if rows.size < 262144 * 32 {
            ctx.error("MAC projection: incomplete input lattice");
            return;
        }
        if out.size < 262144 * 4 {
            ctx.error("node.mac_pressure_zero: insufficient output capacity");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(&self.source, "cs_main", "node.mac_pressure_zero")
        });
        let u = MacCountUniforms {
            dispatch_count: 262144,
            pad: [0; 3],
        };
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
                    buffer: out,
                    offset: 0,
                },
            ],
            [262144u32.div_ceil(256), 1, 1],
            "node.mac_pressure_zero",
        );
    }
}

pub fn mac_pressure_gradient_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacPressureGradient>()
        .expect("mac_pressure_gradient codegen")
}
crate::primitive! {
name:MacPressureGradient,type_id:"node.mac_pressure_gradient",purpose:"Subtract the ghost-fluid pressure gradient using exactly the assembled interface coefficients.",
inputs:{grid: Channels["mac_velocity": Vec4F, "mac_valid": Vec4F] required,
geometry: Channels["mac_open": Vec4F] required,
phi: Array(f32) required,
pressure: Array(u32) required,},outputs:{out:Channels["mac_velocity": Vec4F, "mac_valid": Vec4F],},params:[],
depth_rule:Terminal,composition_notes:"Subtract the ghost-fluid pressure gradient using exactly the assembled interface coefficients. Compose rows, zero, alternating relaxation, residual validation and gradient as separate graph nodes. The stationary box geometry and liquid SDF are explicit input wires.",
examples:[],picker:{label:"MacPressureGradient",category:Atom},summary:"Subtract the ghost-fluid pressure gradient using exactly the assembled interface coefficients.",category:Particles3D,role:Filter,aliases:[],
fusion_kind:Source,wgsl_body:include_str!("shaders/mac_pressure_gradient_body.wgsl"),
input_access:[BufferGather,BufferGather,BufferGather,BufferGather],
wgsl_includes:[include_str!("shaders/water_common.wgsl"),include_str!("shaders/mac_index.wgsl")],
extra_fields:{source:String=mac_pressure_gradient_source(),},
}
impl Primitive for MacPressureGradient {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then_some(274625)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(grid) = ctx.inputs.array("grid") else {
            return;
        };
        let Some(geometry) = ctx.inputs.array("geometry") else {
            return;
        };
        let Some(phi) = ctx.inputs.array("phi") else {
            return;
        };
        let Some(pressure) = ctx.inputs.array("pressure") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        if grid.size < 274625 * 32
            || geometry.size < 274625 * 16
            || phi.size < 262144 * 4
            || pressure.size < 262144 * 4
        {
            ctx.error("MAC projection: incomplete input lattice");
            return;
        }
        if out.size < 274625 * 32 {
            ctx.error("node.mac_pressure_gradient: insufficient output capacity");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &self.source,
                "cs_main",
                "node.mac_pressure_gradient",
            )
        });
        let u = MacCountUniforms {
            dispatch_count: 274625,
            pad: [0; 3],
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: grid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: geometry,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: phi,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: pressure,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: out,
                    offset: 0,
                },
            ],
            [274625u32.div_ceil(256), 1, 1],
            "node.mac_pressure_gradient",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_projection_generated_shaders_validate() {
        for source in [
            mac_apply_gravity_source(),
            mac_pressure_rows_source(),
            mac_pressure_zero_source(),
            mac_pressure_gradient_source(),
        ] {
            let m = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&m)
            .unwrap();
        }
    }
}
