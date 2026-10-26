use crate::freeze::classify::FusionKind;
use crate::parameters::{ParamDef, ParamType};
use crate::ports::{NodeInput, NodeOutput, PortType};

use super::standalone::{StandaloneKernelSpec, generate_standalone};
use super::types::{TexDim, VOLUME_WORKGROUP_3D, dim_forms};

/// Pins the `dim_forms` D3 workgroup string to [`VOLUME_WORKGROUP_3D`], the
/// constant every volume primitive's `run()` sizes its dispatch grid with.
/// If either side changes without the other, generated kernels and host
/// dispatches silently disagree and only a fraction of the volume computes.
#[test]
fn volume_workgroup_constant_matches_emitted_kernel() {
    let n = VOLUME_WORKGROUP_3D;
    assert_eq!(
        dim_forms(TexDim::D3).workgroup,
        format!("{n}, {n}, {n}"),
        "dim_forms D3 workgroup drifted from VOLUME_WORKGROUP_3D"
    );
}

/// CINEMATIC_POST P0 (D7, standalone layer): the TEXTURE codegen path now
/// accepts `derived_uniforms` exactly like the buffer path
/// (`generate_standalone_buffer`, see its own field-emission block) — a
/// scalar defaults to `f32`, a `"name:vec3"` entry expands to three
/// consecutive f32 fields, and both are appended to the Params struct AFTER
/// the user's scalar params (so a future Camera-consuming texture atom like
/// `coc_from_depth` (P1, not this phase) can declare `derived_uniforms:
/// ["cam_pos:vec3"]` the same way `scatter_particles_camera` /
/// `flatten_to_camera_plane` already do on the buffer path). This is a
/// synthetic 0-texture-input (Source) atom — no registered primitive
/// exercises this path yet — proving the mechanism, not a new node.
#[test]
fn generate_standalone_ext_threads_derived_uniforms_after_params() {
    use crate::parameters::ParamValue;
    use crate::ports::PortKind;
    use std::borrow::Cow;

    let outputs = [NodeOutput {
        name: Cow::Borrowed("out"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    }];
    let params = [ParamDef {
        name: Cow::Borrowed("gain"),
        label: "Gain",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 4.0)),
        enum_values: &[],
    }];
    // Body signature order matches the wrapper's arg-building order for a
    // 0-texture-input Source atom: uv, dims, <scalar params...>, <derived
    // fields...>. `foo` (bare name) → f32; `cam_pos:vec3` → vec3<f32>.
    let body = "fn body(uv: vec2<f32>, dims: vec2<f32>, gain: f32, foo: f32, cam_pos: vec3<f32>) -> vec4<f32> {\n    return vec4<f32>(gain + foo + cam_pos.x, cam_pos.y, cam_pos.z, 1.0);\n}";

    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: FusionKind::Source,
        body,
        inputs: &[],
        params: &params,
        input_access: &[],
        derived_uniforms: &["foo", "cam_pos:vec3"],
        outputs: &outputs,
        stencil_fetch: false,
        includes: &[],
    })
    .expect("synthetic source atom with derived uniforms generates");

    assert!(
        naga::front::wgsl::parse_str(&generated).is_ok(),
        "generated kernel must parse through naga:\n{generated}"
    );

    // Exact Params struct: gain (the user param) first, then the derived
    // fields in declaration order (foo scalar, then cam_pos's 3 f32 words),
    // then padding to the next 16-byte (4-word) multiple. 1 + 1 + 3 = 5
    // words → 3 padding words, so this also proves the word-count/padding
    // arithmetic accounts for derived fields, not just user params.
    let expected_params_struct = "struct Params {\n    \
        gain: f32,\n    \
        foo: f32,\n    \
        cam_pos_x: f32,\n    \
        cam_pos_y: f32,\n    \
        cam_pos_z: f32,\n    \
        _pad0: u32,\n    \
        _pad1: u32,\n    \
        _pad2: u32,\n\
        }\n";
    assert!(
        generated.contains(expected_params_struct),
        "Params struct must place derived fields after params, then pad 5 words to 8:\n{generated}"
    );

    // The body call threads the derived fields as its trailing args, in the
    // same order, with the vec3 reassembled from its three packed words.
    assert!(
        generated.contains(
            "let result = body(uv, vec2<f32>(dims), params.gain, params.foo, \
             vec3<f32>(params.cam_pos_x, params.cam_pos_y, params.cam_pos_z));"
        ),
        "body call must pass derived fields after params, vec3 reassembled:\n{generated}"
    );
}

/// a `ParamType::Color` param (the shading-family
/// atoms' `color`/`color_a`/`color_x_low`/... tint) now lays out on the
/// standalone codegen path exactly like Vec3 does — four consecutive f32
/// fields (`<name>_x/_y/_z/_w`), reassembled at the body call as a
/// `vec4<f32>`. `ParamType::Vec4` shares the same branch (color.rs's
/// channel_mixer `row0..row3`). This proves the mechanism generically; the
/// real atoms (blinn_specular, fresnel_rim, matcap_two_tone, color.rs) each
/// carry their own gpu_tests parity proof against the hand shader.
#[test]
fn generate_standalone_ext_expands_color_param_to_vec4() {
    use crate::parameters::ParamValue;
    use crate::ports::PortKind;
    use std::borrow::Cow;

    let inputs = [NodeInput {
        name: Cow::Borrowed("in"),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    }];
    let outputs = [NodeOutput {
        name: Cow::Borrowed("out"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    }];
    let params = [
        ParamDef {
            name: Cow::Borrowed("intensity"),
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("tint"),
            label: "Tint",
            ty: ParamType::Color,
            default: ParamValue::Color([1.0, 1.0, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
    ];
    let body = "fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, intensity: f32, tint: vec4<f32>) -> vec4<f32> {\n    return c * intensity * tint;\n}";

    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: FusionKind::Pointwise,
        body,
        inputs: &inputs,
        params: &params,
        input_access: &[],
        derived_uniforms: &[],
        outputs: &outputs,
        stencil_fetch: false,
        includes: &[],
    })
    .expect("color-param atom must generate");

    assert!(
        naga::front::wgsl::parse_str(&generated).is_ok(),
        "generated kernel must parse through naga:\n{generated}"
    );

    // intensity (1 word) + tint (4 words) = 5 → pad to 8.
    let expected_params_struct = "struct Params {\n    \
        intensity: f32,\n    \
        tint_x: f32,\n    \
        tint_y: f32,\n    \
        tint_z: f32,\n    \
        tint_w: f32,\n    \
        _pad0: u32,\n    \
        _pad1: u32,\n    \
        _pad2: u32,\n\
        }\n";
    assert!(
        generated.contains(expected_params_struct),
        "Color param must expand to four consecutive f32 fields, padded to 8 words:\n{generated}"
    );

    assert!(
        generated.contains(
            "let result = body(c_in, uv, vec2<f32>(dims), params.intensity, \
             vec4<f32>(params.tint_x, params.tint_y, params.tint_z, params.tint_w));"
        ),
        "body call must reassemble the Color param as vec4<f32>:\n{generated}"
    );
}

/// Parses and validates generated WGSL the way the GPU compiler will.
fn assert_valid_wgsl(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl)
        .unwrap_or_else(|e| panic!("{}\n{wgsl}", e.emit_to_string(wgsl)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{e:?}\n{wgsl}"));
}

/// A `CoincidentTexel` input is loaded at the texel under the fragment's spot in
/// THAT texture's own grid, so a velocity or depth buffer smaller than the
/// output (temporal upscale renders them at 2/3) is read where the fragment is.
/// The sampler-read input beside it is untouched, and the helper is defined once.
#[test]
fn coincident_texel_input_reads_its_own_grid() {
    use crate::freeze::classify::InputAccess;
    use crate::ports::PortKind;
    use std::borrow::Cow;

    let input = |name| NodeInput {
        name: Cow::Borrowed(name),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    };
    let inputs = [input("in"), input("velocity")];
    let outputs = [NodeOutput {
        name: Cow::Borrowed("out"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    }];
    let body = "fn body(c_in: vec4<f32>, c_velocity: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {\n    return c_in + c_velocity;\n}";

    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: FusionKind::MultiInputCoincident,
        body,
        inputs: &inputs,
        params: &[],
        input_access: &[InputAccess::Coincident, InputAccess::CoincidentTexel],
        derived_uniforms: &[],
        outputs: &outputs,
        stencil_fetch: false,
        includes: &[],
    })
    .expect("a texel-read atom generates");
    assert_valid_wgsl(&generated);

    assert_eq!(
        generated.matches("fn own_texel(").count(),
        1,
        "the helper is defined once:\n{generated}"
    );
    assert!(
        generated.contains(
            "let c_velocity = textureLoad(tex_velocity, \
             own_texel(uv, textureDimensions(tex_velocity)), 0);"
        ),
        "the texel input loads at its own grid's texel:\n{generated}"
    );
    assert!(
        generated.contains("let c_in = textureSampleLevel(tex_in, samp, uv, 0.0);"),
        "the sampler-read input is unchanged:\n{generated}"
    );
    assert!(
        !generated.contains("let coord"),
        "no input reads at the fragment's pixel index:\n{generated}"
    );
}

/// A volume atom's texel input maps by the same rule in three dimensions.
#[test]
fn coincident_texel_volume_input_reads_its_own_grid() {
    use crate::freeze::classify::InputAccess;
    use crate::ports::PortKind;
    use std::borrow::Cow;

    let inputs = [NodeInput {
        name: Cow::Borrowed("gradient"),
        ty: PortType::Texture3D,
        kind: PortKind::Input,
        required: true,
    }];
    let outputs = [NodeOutput {
        name: Cow::Borrowed("force"),
        ty: PortType::Texture3D,
        kind: PortKind::Output,
        required: false,
    }];
    let body = "fn body(c_gradient: vec4<f32>, uv: vec3<f32>, dims: vec3<f32>) -> vec4<f32> {\n    return c_gradient;\n}";

    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: FusionKind::Pointwise,
        body,
        inputs: &inputs,
        params: &[],
        input_access: &[InputAccess::CoincidentTexel],
        derived_uniforms: &[],
        outputs: &outputs,
        stencil_fetch: false,
        includes: &[],
    })
    .expect("a texel-read volume atom generates");
    assert_valid_wgsl(&generated);
    assert!(
        generated.contains("fn own_texel(uv: vec3<f32>, tex_dims: vec3<u32>) -> vec3<i32>"),
        "the volume helper is three-dimensional:\n{generated}"
    );
    assert!(
        generated.contains(
            "let c_gradient = textureLoad(tex_gradient, \
             own_texel(uv, textureDimensions(tex_gradient)), 0);"
        ),
        "the volume input loads at its own grid's texel:\n{generated}"
    );
}

/// A kernel with no texel input carries no helper: the idle path costs nothing
/// and its WGSL (the pipeline-cache key) is unchanged.
#[test]
fn kernel_without_texel_input_has_no_texel_helper() {
    use crate::ports::PortKind;
    use std::borrow::Cow;

    let inputs = [NodeInput {
        name: Cow::Borrowed("in"),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    }];
    let outputs = [NodeOutput {
        name: Cow::Borrowed("out"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    }];
    let body = "fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {\n    return c;\n}";
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: FusionKind::Pointwise,
        body,
        inputs: &inputs,
        params: &[],
        input_access: &[],
        derived_uniforms: &[],
        outputs: &outputs,
        stencil_fetch: false,
        includes: &[],
    })
    .expect("a sampler-read atom generates");
    assert_valid_wgsl(&generated);
    assert!(
        !generated.contains("own_texel"),
        "a sampler-read kernel has no texel helper:\n{generated}"
    );
}

/// The fused kernel reads a `CoincidentTexel` external with the same rule as the
/// standalone wrapper; a plain external still loads at the kernel's own coord.
#[test]
fn fused_texel_external_reads_its_own_grid() {
    use super::{FusionRegion, InputSource, RegionNode, generate_fused};
    use crate::exec::effect_node::NodeInstanceId;
    use crate::freeze::classify::InputAccess;

    let mix = "fn body(a: vec4<f32>, b: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {\n    return mix(a, b, 0.5);\n}\n";
    let gain = "fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {\n    return c * 2.0;\n}\n";
    let id = NodeInstanceId;
    let node = |i, kind, body, inputs, input_access| RegionNode {
        node_id: id(i),
        fusion_kind: kind,
        body,
        params: &[],
        inputs,
        input_access,
        node_inputs: &[],
        node_outputs: &[],
        node_includes: &[],
        derived_uniforms: &[],
        type_id: String::new(),
        derived_camera_ext: None,
        output_storage: "rgba16float",
        stencil_fetch: false,
        quantize_f16: false,
    };
    let region = FusionRegion {
        nodes: vec![
            node(
                0,
                FusionKind::MultiInputCoincident,
                mix,
                vec![InputSource::External(0), InputSource::External(1)],
                vec![InputAccess::Coincident, InputAccess::CoincidentTexel],
            ),
            node(
                1,
                FusionKind::Pointwise,
                gain,
                vec![InputSource::Node(id(0))],
                vec![],
            ),
        ],
        num_external_inputs: 2,
        outputs: vec![(id(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        live_anchor: None,
        output_capacity: None,
    };
    let g = generate_fused(&region).expect("a region with a texel external fuses");
    assert_valid_wgsl(&g.wgsl);
    assert_eq!(
        g.wgsl.matches("fn own_texel(").count(),
        1,
        "the helper is defined once:\n{}",
        g.wgsl
    );
    assert!(
        g.wgsl.contains(
            "let ext_1 = textureLoad(src_1, own_texel(uv, textureDimensions(src_1)), 0);"
        ),
        "the texel external loads at its own grid's texel:\n{}",
        g.wgsl
    );
    assert!(
        g.wgsl.contains("let ext_0 = textureLoad(src_0, coord, 0);"),
        "a plain external still loads at the kernel's coord:\n{}",
        g.wgsl
    );
}
