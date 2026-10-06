//! `node.cut_out_box` — hides the part of a mesh inside a world box by
//! zeroing its vertex alpha, for a material in alpha Mask mode
//! (docs/OCEAN_SURFACE_DESIGN.md D7).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::{active_elements, standalone_pipeline};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CutUniforms {
    pub center_x: f32,
    pub center_y: f32,
    pub center_z: f32,
    pub size_x: f32,
    pub size_y: f32,
    pub size_z: f32,
    pub feather: f32,
    pub dispatch_count: u32,
}

#[cfg(test)]
impl CutUniforms {
    fn center(&self) -> [f32; 3] {
        [self.center_x, self.center_y, self.center_z]
    }

    fn size(&self) -> [f32; 3] {
        [self.size_x, self.size_y, self.size_z]
    }
}

crate::primitive! {
    name: CutOutBox,
    type_id: "node.cut_out_box",
    purpose: "Hide the part of a mesh inside a world-space box: vertex alpha is multiplied by 0 inside the box and ramps back to 1 over Feather metres outside it. Pair with a material in alpha Mask mode. The cut edge is as fine as the mesh.",
    inputs: {
        mesh: Array(MeshVertex) required,
        center_x: ScalarF32 optional,
        center_y: ScalarF32 optional,
        center_z: ScalarF32 optional,
        size_x: ScalarF32 optional,
        size_y: ScalarF32 optional,
        size_z: ScalarF32 optional,
        feather: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef { name: Cow::Borrowed("center_x"), label: "Center X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-10000.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("center_y"), label: "Center Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-10000.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("center_z"), label: "Center Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-10000.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("size_x"), label: "Size X", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("size_y"), label: "Size Y", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("size_z"), label: "Size Z", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 10000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("feather"), label: "Feather", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 100.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Set the material's alpha mode to Mask. To cut a liquid tank out of an ocean, use the tank's box with a generous Size Y so waves can't reach over it.",
    examples: ["OceanCliff"],
    picker: { label: "Cut Out Box", category: Atom },
    summary: "Hides the part of a mesh inside a box, for example to cut a hole in an ocean where a splash tank sits.",
    category: Geometry3D,
    role: Filter,
    aliases: ["cut out", "hole", "box mask", "clip box", "hide inside"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/cut_out_box_body.wgsl"),
}

impl Primitive for CutOutBox {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(p, _)| *p == "mesh").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let uniforms = CutUniforms {
            center_x: ctx.scalar_or_param("center_x", 0.0),
            center_y: ctx.scalar_or_param("center_y", 0.0),
            center_z: ctx.scalar_or_param("center_z", 0.0),
            size_x: ctx.scalar_or_param("size_x", 1.0),
            size_y: ctx.scalar_or_param("size_y", 1.0),
            size_z: ctx.scalar_or_param("size_z", 1.0),
            feather: ctx.scalar_or_param("feather", 0.0).max(0.0),
            dispatch_count: 0,
        };
        let (Some(mesh), Some(out)) = (ctx.inputs.array("mesh"), ctx.outputs.array("out")) else {
            return;
        };
        let count = active_elements::<MeshVertex>(mesh.size.min(out.size), u32::MAX);
        if count == 0 {
            return;
        }
        let uniforms = CutUniforms { dispatch_count: count, ..uniforms };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: mesh, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.cut_out_box",
        );
    }
}

/// CPU reference of the WGSL body, for the proofs.
#[cfg(test)]
pub(crate) fn reference(v: &MeshVertex, u: &CutUniforms) -> MeshVertex {
    let d = (0..3)
        .map(|a| (v.position[a] - u.center()[a]).abs() - 0.5 * u.size()[a])
        .fold(f32::NEG_INFINITY, f32::max);
    let keep = if u.feather > 0.0 { (d / u.feather).clamp(0.0, 1.0) } else { f32::from(u8::from(d >= 0.0)) };
    let mut out = *v;
    out.color[3] *= keep;
    out
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::CapacityExpr;
    use crate::node_graph::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::ocean_displace::{self, OceanDisplace};

    fn cut() -> CutUniforms {
        CutUniforms { center_x: 40.0, center_y: 0.0, center_z: -60.0, size_x: 120.0, size_y: 20.0, size_z: 80.0, feather: 6.0, dispatch_count: 0 }
    }

    fn upload(device: &manifold_gpu::GpuDevice, bytes: &[u8]) -> manifold_gpu::GpuBuffer {
        let b = device.create_buffer_shared(bytes.len() as u64);
        unsafe { b.write(0, bytes) };
        b
    }

    fn read(buf: &manifold_gpu::GpuBuffer, n: usize) -> Vec<MeshVertex> {
        let ptr = buf.mapped_ptr().expect("shared buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, n) }.to_vec()
    }

    /// The generated kernel matches the CPU reference vertex for vertex.
    #[test]
    fn cut_out_box_matches_cpu() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<CutOutBox>().expect("cut_out_box codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, ENTRY, "cut-out-box-test");
        let (vertices, _, _) = ocean_displace::gpu_tests::fixture();
        let u = CutUniforms { dispatch_count: vertices.len() as u32, ..cut() };
        let mesh = upload(&device, bytemuck::cast_slice(&vertices));
        let out = device.create_buffer_shared(std::mem::size_of_val(vertices.as_slice()) as u64);
        let mut enc = device.create_encoder("cut-out-box-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&u) },
                GpuBinding::Buffer { binding: 1, buffer: &mesh, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &out, offset: 0 },
            ],
            [(vertices.len() as u32).div_ceil(256), 1, 1],
            "cut-out-box-test",
        );
        enc.commit_and_wait_completed();
        let got = read(&out, vertices.len());
        let (mut hidden, mut kept) = (0, 0);
        for (i, (v, g)) in vertices.iter().zip(&got).enumerate() {
            let want = reference(v, &u);
            assert!((g.color[3] - want.color[3]).abs() <= 1e-6, "vertex {i}: alpha {} vs {}", g.color[3], want.color[3]);
            assert_eq!(g.position, v.position);
            hidden += usize::from(want.color[3] == 0.0);
            kept += usize::from(want.color[3] == 1.0);
        }
        assert!(hidden > 0 && kept > 0, "fixture must exercise inside and outside ({hidden}, {kept})");
    }

    /// The scalar fields of the generated `struct Params`, in order.
    fn params_fields(wgsl: &str) -> Vec<String> {
        let start = wgsl.find("struct Params {").expect("fused kernel has a Params struct");
        let body = &wgsl[start + "struct Params {".len()..];
        let body = &body[..body.find('}').expect("Params closes")];
        body.split(',')
            .filter_map(|f| f.trim().split(':').next())
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect()
    }

    /// Displace then cut, fused into one kernel, equals the two atoms run one
    /// after the other (docs/OCEAN_SURFACE_DESIGN.md section 4, invariant 5).
    #[test]
    fn ocean_displace_then_cut_fused_matches_unfused() {
        let device = crate::test_device();
        let (vertices, du, fields) = ocean_displace::gpu_tests::fixture();
        let cu = CutUniforms { dispatch_count: vertices.len() as u32, ..cut() };
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![
                RegionNode {
                    node_id: id(0),
                    fusion_kind: OceanDisplace::FUSION_KIND,
                    body: OceanDisplace::WGSL_BODY.unwrap(),
                    params: OceanDisplace::PARAMS,
                    inputs: vec![InputSource::External(0), InputSource::External(1), InputSource::External(2), InputSource::External(3)],
                    input_access: OceanDisplace::INPUT_ACCESS.to_vec(),
                    node_inputs: OceanDisplace::INPUTS,
                    node_outputs: OceanDisplace::OUTPUTS,
                    node_includes: OceanDisplace::WGSL_INCLUDES,
                    derived_uniforms: OceanDisplace::DERIVED_UNIFORMS,
                    type_id: OceanDisplace::TYPE_ID.to_string(),
                    derived_camera_ext: Some(0),
                    output_storage: "rgba16float",
                    stencil_fetch: false,
                    quantize_f16: false,
                },
                RegionNode {
                    node_id: id(1),
                    fusion_kind: CutOutBox::FUSION_KIND,
                    body: CutOutBox::WGSL_BODY.unwrap(),
                    params: CutOutBox::PARAMS,
                    inputs: vec![InputSource::Node(id(0))],
                    input_access: CutOutBox::INPUT_ACCESS.to_vec(),
                    node_inputs: CutOutBox::INPUTS,
                    node_outputs: CutOutBox::OUTPUTS,
                    node_includes: CutOutBox::WGSL_INCLUDES,
                    derived_uniforms: CutOutBox::DERIVED_UNIFORMS,
                    type_id: CutOutBox::TYPE_ID.to_string(),
                    derived_camera_ext: None,
                    output_storage: "rgba16float",
                    stencil_fetch: false,
                    quantize_f16: false,
                },
            ],
            num_external_inputs: 4,
            outputs: vec![(id(1), "out".into())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 1,
            output_capacity: Some(CapacityExpr::Slot(0)),
        };
        let generated = generate_fused(&region).expect("displace+cut fused codegen");
        let wgsl = generated.wgsl;
        assert!(wgsl.contains("fn n0_body") && wgsl.contains("fn n1_body"), "both bodies fused:\n{wgsl}");

        // Fill the fused Params by field name: n<k>_<param or derived name>.
        let mut values: Vec<(String, u32)> = Vec::new();
        let f = |v: f32| v.to_bits();
        let push = |values: &mut Vec<(String, u32)>, name: &str, bits: u32| values.push((name.to_string(), bits));
        push(&mut values, "n0_choppiness", f(du.choppiness));
        push(&mut values, "n0_foam_threshold", f(du.foam_threshold));
        push(&mut values, "n0_foam_width", f(du.foam_width));
        for (c, cs) in (0..ocean_displace::CASCADES).map(|c| (c, du.cascade(c))) {
            push(&mut values, &format!("n0_size_{c}"), cs.size as u32);
            push(&mut values, &format!("n0_tile_size_{c}"), f(cs.tile_size));
            push(&mut values, &format!("n0_fade_start_{c}"), f(cs.fade_start));
            push(&mut values, &format!("n0_fade_end_{c}"), f(cs.fade_end));
        }
        push(&mut values, "n0_cam_x", f(du.cam_x));
        push(&mut values, "n0_cam_z", f(du.cam_z));
        for (a, axis) in ["x", "y", "z"].iter().enumerate() {
            push(&mut values, &format!("n1_center_{axis}"), f(cu.center()[a]));
            push(&mut values, &format!("n1_size_{axis}"), f(cu.size()[a]));
        }
        push(&mut values, "n1_feather", f(cu.feather));
        let fields_in_order = params_fields(&wgsl);
        let mut words: Vec<u32> = fields_in_order
            .iter()
            .map(|name| {
                values
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|&(_, b)| b)
                    .or_else(|| name.contains("count").then_some(vertices.len() as u32))
                    .or_else(|| name.starts_with("_pad").then_some(0))
                    .unwrap_or_else(|| panic!("unknown fused Params field {name}:\n{wgsl}"))
            })
            .collect();
        while !words.len().is_multiple_of(4) {
            words.push(0);
        }

        // The fused kernel's bindings, by name, as it declares them.
        let binding_of = |name: &str| -> u32 {
            let line = wgsl
                .lines()
                .find(|l| l.contains("@binding(") && l.contains(&format!(" {name}:")))
                .unwrap_or_else(|| panic!("no binding for {name}:\n{wgsl}"));
            let at = line.find("@binding(").unwrap() + "@binding(".len();
            line[at..].split(')').next().unwrap().trim().parse().unwrap()
        };
        let mesh = upload(&device, bytemuck::cast_slice(&vertices));
        let fb: Vec<_> = fields.iter().map(|v| upload(&device, bytemuck::cast_slice(v))).collect();
        let n = vertices.len();
        let bytes = std::mem::size_of_val(vertices.as_slice()) as u64;
        let groups = [(n as u32).div_ceil(256), 1, 1];
        let fused_out = device.create_buffer_shared(bytes);
        let fused_pipeline = device.create_compute_pipeline(&wgsl, ENTRY, "ocean-fused-test");
        // Unfused: the two standalone kernels, one after the other.
        let displace_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<OceanDisplace>().expect("displace codegen");
        let cut_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<CutOutBox>().expect("cut codegen");
        let displace_pipeline = device.create_compute_pipeline(&displace_wgsl, ENTRY, "ocean-unfused-displace");
        let cut_pipeline = device.create_compute_pipeline(&cut_wgsl, ENTRY, "ocean-unfused-cut");
        let mid = device.create_buffer_shared(bytes);
        let unfused_out = device.create_buffer_shared(bytes);
        let mut enc = device.create_encoder("ocean-fused-test");
        enc.dispatch_compute(
            &fused_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&words) },
                GpuBinding::Buffer { binding: binding_of("src_0"), buffer: &mesh, offset: 0 },
                GpuBinding::Buffer { binding: binding_of("src_1"), buffer: &fb[0], offset: 0 },
                GpuBinding::Buffer { binding: binding_of("src_2"), buffer: &fb[1], offset: 0 },
                GpuBinding::Buffer { binding: binding_of("src_3"), buffer: &fb[2], offset: 0 },
                GpuBinding::Buffer { binding: binding_of("dst"), buffer: &fused_out, offset: 0 },
            ],
            groups,
            "ocean-fused-test",
        );
        enc.dispatch_compute(
            &displace_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&du) },
                GpuBinding::Buffer { binding: 1, buffer: &mesh, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &fb[0], offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &fb[1], offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &fb[2], offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: &mid, offset: 0 },
            ],
            groups,
            "ocean-unfused-displace",
        );
        enc.dispatch_compute(
            &cut_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&cu) },
                GpuBinding::Buffer { binding: 1, buffer: &mid, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &unfused_out, offset: 0 },
            ],
            groups,
            "ocean-unfused-cut",
        );
        enc.commit_and_wait_completed();
        let (fused, unfused) = (read(&fused_out, n), read(&unfused_out, n));
        let (mut moved, mut hidden) = (0, 0);
        for (i, (a, b)) in fused.iter().zip(&unfused).enumerate() {
            assert_eq!(bytemuck::bytes_of(a), bytemuck::bytes_of(b), "vertex {i}: fused {:?} {:?} vs unfused {:?} {:?}", a.position, a.color, b.position, b.color);
            moved += usize::from(a.position != vertices[i].position);
            hidden += usize::from(a.color[3] == 0.0);
        }
        assert!(moved > 0 && hidden > 0, "the fixture must move and cut vertices ({moved}, {hidden})");
    }
}
